use std::collections::HashMap;

/// Model pricing in USD per million tokens.
#[derive(Debug, Clone)]
struct ModelPrice {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

/// Cost calculation engine. Loads pricing from embedded JSON.
pub struct CostCalculator {
    models: HashMap<String, ModelPrice>,
    aliases: HashMap<String, String>,
    fallback: ModelPrice,
}

#[derive(Debug, Clone, Default)]
pub struct CostResult {
    /// Total cost of the actual request in USD
    pub cost_usd: f64,
    /// Estimated cost saved by merlint optimization in USD
    pub cost_saved_usd: f64,
}

/// Net effect of Anthropic's prompt cache on a set of requests.
///
/// The counterfactual is: "what if we had sent all these tokens as fresh
/// input at full price?". Compared to that, caching:
///   * discounts `cache_read_tokens` to ~10% of input price (huge win)
///   * charges a ~25% premium on `cache_creation_tokens` (small loss)
///
/// The net of those two is the real value prop — and it's typically huge
/// because writes happen once while reads repeat across every later turn.
#[derive(Debug, Clone, Default)]
pub struct CacheBreakdown {
    /// Net dollars saved by caching vs. the no-cache counterfactual. Can be
    /// negative for one-shot requests where only cache_creation happened,
    /// but quickly dominates once reads start paying back.
    pub savings_usd: f64,
    /// Savings as a fraction of the no-cache input cost, 0..100. "Your prompt
    /// input cost X% less than it would have without caching."
    pub savings_pct: f64,
    /// Hypothetical no-cache input cost for reference / tooltips.
    pub hypothetical_usd: f64,
}

impl CostCalculator {
    pub fn new() -> Self {
        let json_str = include_str!("../../assets/pricing.json");
        Self::from_json(json_str)
    }

    fn from_json(json_str: &str) -> Self {
        let val: serde_json::Value = serde_json::from_str(json_str)
            .expect("pricing.json is invalid");

        let mut models = HashMap::new();
        if let Some(obj) = val.get("models").and_then(|v| v.as_object()) {
            for (name, price) in obj {
                if let Some(mp) = parse_model_price(price) {
                    models.insert(name.clone(), mp);
                }
            }
        }

        let mut aliases = HashMap::new();
        if let Some(obj) = val.get("aliases").and_then(|v| v.as_object()) {
            for (alias, target) in obj {
                if let Some(t) = target.as_str() {
                    aliases.insert(alias.clone(), t.to_string());
                }
            }
        }

        let fallback = val.get("default_fallback")
            .and_then(parse_model_price)
            .unwrap_or(ModelPrice { input: 3.0, output: 15.0, cache_read: Some(0.3), cache_write: None });

        Self { models, aliases, fallback }
    }

    /// Look up pricing for a model name (handles aliases and prefix stripping).
    fn lookup(&self, model: &str) -> &ModelPrice {
        // Direct match
        if let Some(p) = self.models.get(model) {
            return p;
        }
        // Alias match
        if let Some(target) = self.aliases.get(model) {
            if let Some(p) = self.models.get(target) {
                return p;
            }
        }
        // Strip provider prefix (e.g. "anthropic/claude-sonnet-4-6" -> "claude-sonnet-4-6")
        if let Some(pos) = model.find('/') {
            let stripped = &model[pos + 1..];
            if let Some(p) = self.models.get(stripped) {
                return p;
            }
            if let Some(target) = self.aliases.get(stripped) {
                if let Some(p) = self.models.get(target) {
                    return p;
                }
            }
        }
        // Prefix match (e.g. "claude-sonnet-4-6-20250514" matches "claude-sonnet-4-6")
        for (name, price) in &self.models {
            if model.starts_with(name.as_str()) {
                return price;
            }
        }
        &self.fallback
    }

    /// Calculate cost for a completed request.
    pub fn calculate(
        &self,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        tokens_saved: i64,
    ) -> CostResult {
        let price = self.lookup(model);

        // Actual cost: non-cached input + cache_read + cache_write + output
        let non_cached_input = prompt_tokens.saturating_sub(cache_read_tokens + cache_creation_tokens);
        let input_cost = (non_cached_input as f64) * price.input / 1_000_000.0;
        let cache_read_cost = (cache_read_tokens as f64) * price.cache_read.unwrap_or(price.input) / 1_000_000.0;
        let cache_write_cost = (cache_creation_tokens as f64) * price.cache_write.unwrap_or(price.input) / 1_000_000.0;
        let output_cost = (completion_tokens as f64) * price.output / 1_000_000.0;
        let cost_usd = input_cost + cache_read_cost + cache_write_cost + output_cost;

        // Saved cost: tokens_saved were input tokens that would have been sent
        let cost_saved_usd = if tokens_saved > 0 {
            (tokens_saved as f64) * price.input / 1_000_000.0
        } else {
            0.0
        };

        CostResult { cost_usd, cost_saved_usd }
    }

    /// Estimate the dollar value that Anthropic's prompt cache saved for a
    /// given `cache_read` token count. Compared to sending the same tokens at
    /// the full input price, cached tokens bill at `cache_read` rate (typically
    /// 0.1×), so the per-token savings is `input_price - cache_read_price`.
    ///
    /// This is the "invisible" savings that the prompt cache provides —
    /// separate from merlint's own tool-pruning savings which is tracked in
    /// `cost_saved_usd`. Surfacing both lets the dashboard show users where
    /// their real cost reduction comes from (almost always: the cache).
    ///
    /// Note: this is the simple accounting, ignoring cache write overhead.
    /// For the dashboard headline savings that net out cache_creation cost,
    /// use `cache_breakdown()` instead.
    pub fn cache_savings(&self, model: &str, cache_read_tokens: u64) -> f64 {
        if cache_read_tokens == 0 {
            return 0.0;
        }
        let price = self.lookup(model);
        let cache_read_price = price.cache_read.unwrap_or(price.input);
        let per_token_savings = (price.input - cache_read_price).max(0.0);
        (cache_read_tokens as f64) * per_token_savings / 1_000_000.0
    }

    /// Net savings from Anthropic's prompt cache for a set of (aggregated)
    /// request stats: fresh + cache_read + cache_creation tokens under a
    /// single model. Returns absolute dollars and the percentage off the
    /// no-cache input cost so the dashboard can show both.
    pub fn cache_breakdown(
        &self,
        model: &str,
        fresh_input_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    ) -> CacheBreakdown {
        let total_input = fresh_input_tokens + cache_read_tokens + cache_creation_tokens;
        if total_input == 0 {
            return CacheBreakdown::default();
        }
        let price = self.lookup(model);
        let cache_read_price = price.cache_read.unwrap_or(price.input);
        let cache_write_price = price.cache_write.unwrap_or(price.input);

        // No-cache counterfactual: every input token at full input price.
        let hypothetical_usd = (total_input as f64) * price.input / 1_000_000.0;
        // What we actually paid for input (output is unaffected so we leave
        // it out — it keeps the "X% off" figure tight on the input story).
        let actual_input_usd = (fresh_input_tokens as f64) * price.input / 1_000_000.0
            + (cache_read_tokens as f64) * cache_read_price / 1_000_000.0
            + (cache_creation_tokens as f64) * cache_write_price / 1_000_000.0;

        let savings_usd = hypothetical_usd - actual_input_usd;
        let savings_pct = if hypothetical_usd > 0.0 {
            (savings_usd / hypothetical_usd) * 100.0
        } else {
            0.0
        };
        CacheBreakdown {
            savings_usd,
            savings_pct,
            hypothetical_usd,
        }
    }
}

fn parse_model_price(val: &serde_json::Value) -> Option<ModelPrice> {
    let input = val.get("input")?.as_f64()?;
    let output = val.get("output")?.as_f64()?;
    let cache_read = val.get("cache_read").and_then(|v| v.as_f64());
    let cache_write = val.get("cache_write").and_then(|v| v.as_f64());
    Some(ModelPrice { input, output, cache_read, cache_write })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_sonnet_cost() {
        let calc = CostCalculator::new();
        let result = calc.calculate("claude-sonnet-4-6", 10000, 2000, 5000, 0, 3000);
        // input: 5000 non-cached * 3.0/1M = 0.015
        // cache_read: 5000 * 0.3/1M = 0.0015
        // output: 2000 * 15.0/1M = 0.03
        assert!((result.cost_usd - 0.0465).abs() < 0.001);
        // saved: 3000 * 3.0/1M = 0.009
        assert!((result.cost_saved_usd - 0.009).abs() < 0.001);
    }

    #[test]
    fn test_alias_resolution() {
        let calc = CostCalculator::new();
        let result = calc.calculate("claude-3-5-sonnet-latest", 1000000, 0, 0, 0, 0);
        // Should resolve to claude-3-5-sonnet-20241022: 1M * 3.0/1M = 3.0
        assert!((result.cost_usd - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_fallback_pricing() {
        let calc = CostCalculator::new();
        let result = calc.calculate("some-unknown-model-xyz", 1000000, 0, 0, 0, 0);
        // Fallback: 1M * 3.0/1M = 3.0
        assert!((result.cost_usd - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_cache_savings_sonnet() {
        let calc = CostCalculator::new();
        // Sonnet: input $3/M, cache_read $0.3/M → savings $2.7/M
        // 10M cache reads → $27 saved vs sending them at full input price
        let saved = calc.cache_savings("claude-sonnet-4-6", 10_000_000);
        assert!((saved - 27.0).abs() < 0.01, "expected ~$27, got {}", saved);
    }

    #[test]
    fn test_cache_savings_zero_reads() {
        let calc = CostCalculator::new();
        assert_eq!(calc.cache_savings("claude-sonnet-4-6", 0), 0.0);
    }

    #[test]
    fn test_cache_savings_unknown_model_uses_fallback() {
        let calc = CostCalculator::new();
        // Unknown model → fallback pricing (input $3/M, cache_read $0.3/M)
        // 1M cache reads → $2.7 saved
        let saved = calc.cache_savings("totally-fake-model", 1_000_000);
        assert!(saved > 0.0, "fallback should still produce non-zero savings");
    }

    #[test]
    fn test_cache_breakdown_typical_reuse() {
        // Typical Claude Code follow-up turn: small fresh delta, big cache
        // read for the stable system prompt, no new cache write.
        //   fresh:   1000  → $3.0 / M
        //   read:   14000  → $0.3 / M
        //   write:      0
        // No-cache cost: 15000 × $3/M   = $0.045
        // Actual cost:    1000 × $3/M + 14000 × $0.3/M = $0.003 + $0.0042 = $0.0072
        // Savings:        $0.045 - $0.0072 = $0.0378
        // Savings %:      $0.0378 / $0.045 = 84%
        let calc = CostCalculator::new();
        let b = calc.cache_breakdown("claude-sonnet-4-6", 1000, 14000, 0);
        assert!((b.savings_usd - 0.0378).abs() < 1e-4, "got {}", b.savings_usd);
        assert!((b.savings_pct - 84.0).abs() < 0.5, "got {}", b.savings_pct);
    }

    #[test]
    fn test_cache_breakdown_first_turn_has_overhead() {
        // First turn of a fresh conversation: we pay cache_creation premium
        // and haven't read anything yet. Should be slightly NEGATIVE savings.
        //   fresh:   1000
        //   read:       0
        //   write:  14000 → $3.75/M (25% premium over fresh input)
        // No-cache cost: 15000 × $3/M      = $0.045
        // Actual cost:    1000 × $3/M + 14000 × $3.75/M = $0.003 + $0.0525 = $0.0555
        // Savings: $0.045 - $0.0555 = -$0.0105 (net loss of about $0.01)
        let calc = CostCalculator::new();
        let b = calc.cache_breakdown("claude-sonnet-4-6", 1000, 0, 14000);
        assert!(b.savings_usd < 0.0, "first turn should show a small loss");
        assert!(b.savings_pct < 0.0);
    }

    #[test]
    fn test_cache_breakdown_empty_is_zero() {
        let calc = CostCalculator::new();
        let b = calc.cache_breakdown("claude-sonnet-4-6", 0, 0, 0);
        assert_eq!(b.savings_usd, 0.0);
        assert_eq!(b.savings_pct, 0.0);
        assert_eq!(b.hypothetical_usd, 0.0);
    }
}
