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
}
