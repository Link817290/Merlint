use colored::Colorize;

use crate::analyzer::cache::CacheAnalysis;
use crate::analyzer::efficiency::EfficiencyAnalysis;
use crate::analyzer::token::SessionTokenSummary;
use crate::models::trace::TraceSession;

/// Run deep analysis using an LLM API (requires ANTHROPIC_API_KEY or OPENAI_API_KEY)
pub async fn deep_analyze(
    _session: &TraceSession,
    ts: &SessionTokenSummary,
    ca: &CacheAnalysis,
    ea: &EfficiencyAnalysis,
) -> anyhow::Result<String> {
    // Build a summary prompt for the LLM
    let summary = build_summary_prompt(ts, ca, ea);

    // Try Anthropic first, then OpenAI
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        return call_anthropic(&key, &summary).await;
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        return call_openai(&key, &summary).await;
    }

    anyhow::bail!(
        "Deep analysis requires an LLM API key.\n\
         Set ANTHROPIC_API_KEY or OPENAI_API_KEY environment variable."
    )
}

fn build_summary_prompt(
    ts: &SessionTokenSummary,
    ca: &CacheAnalysis,
    ea: &EfficiencyAnalysis,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are merlint, an expert at optimizing LLM agent token usage.\n\n");
    prompt.push_str("Analyze this session data and provide specific, actionable optimization advice.\n\n");

    prompt.push_str(&format!("## Session Summary\n"));
    prompt.push_str(&format!("- API Calls: {}\n", ts.num_calls));
    prompt.push_str(&format!("- Total Tokens: {}\n", ts.total_tokens));
    prompt.push_str(&format!("- Prompt Tokens: {}\n", ts.total_prompt_tokens));
    prompt.push_str(&format!("- Completion Tokens: {}\n", ts.total_completion_tokens));
    prompt.push_str(&format!("- Avg Tokens/Call: {:.0}\n", ea.tokens_per_call_avg));
    prompt.push_str(&format!("- Cache Hit Ratio: {:.0}%\n",
        ca.actual_cache_hit_ratio.unwrap_or(ca.theoretical_cache_hit_ratio) * 100.0));
    prompt.push_str(&format!("- Prefix Stability: {:.0}%\n", ca.prefix_stability_ratio * 100.0));
    prompt.push_str(&format!("- Tools Defined: {}, Used: {}, Unused: {}\n",
        ts.tools_defined, ts.tools_used, ts.tool_names_unused.len()));
    prompt.push_str(&format!("- Retries: {}\n", ea.retry_count));
    prompt.push_str(&format!("- Loop Patterns: {}\n", ea.loop_patterns.len()));
    prompt.push_str(&format!("- Redundant Reads: {}\n", ea.redundant_reads.len()));

    if !ts.tool_names_unused.is_empty() {
        prompt.push_str(&format!("\n## Unused Tools\n"));
        for name in &ts.tool_names_unused {
            prompt.push_str(&format!("- {}\n", name));
        }
    }

    if !ea.loop_patterns.is_empty() {
        prompt.push_str(&format!("\n## Loop Patterns Detected\n"));
        for lp in &ea.loop_patterns {
            prompt.push_str(&format!("- {}\n", lp.description));
        }
    }

    if !ea.redundant_reads.is_empty() {
        prompt.push_str(&format!("\n## Redundant Reads\n"));
        for rr in &ea.redundant_reads {
            prompt.push_str(&format!("- {} read {} times\n", rr.resource, rr.read_count));
        }
    }

    if !ca.issues.is_empty() {
        prompt.push_str(&format!("\n## Cache Issues\n"));
        for issue in &ca.issues {
            prompt.push_str(&format!("- {}\n", issue.description));
        }
    }

    // Add per-call token growth pattern
    if ts.per_call.len() > 1 {
        prompt.push_str(&format!("\n## Token Growth Pattern (prompt tokens per call)\n"));
        for ct in &ts.per_call {
            prompt.push_str(&format!("  Call #{}: {} prompt, {} completion\n",
                ct.call_index, ct.prompt_tokens, ct.completion_tokens));
        }
    }

    prompt.push_str("\n## Instructions\n");
    prompt.push_str("Provide:\n");
    prompt.push_str("1. A severity score (1-10, 10 = very wasteful)\n");
    prompt.push_str("2. Top 3 specific optimizations with estimated token savings\n");
    prompt.push_str("3. Recommended CLAUDE.md instructions to fix the issues\n");
    prompt.push_str("4. Any patterns you notice in the token growth\n");
    prompt.push_str("\nBe concise and actionable. No generic advice.");

    prompt
}

async fn call_anthropic(api_key: &str, prompt: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 1500,
        "messages": [{"role": "user", "content": prompt}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Anthropic API error {}: {}", status, text);
    }

    let json: serde_json::Value = resp.json().await?;
    let text = json["content"][0]["text"]
        .as_str()
        .unwrap_or("No response")
        .to_string();

    Ok(text)
}

async fn call_openai(api_key: &str, prompt: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "max_tokens": 1500,
        "messages": [
            {"role": "system", "content": "You are merlint, an LLM agent token optimizer. Be concise and actionable."},
            {"role": "user", "content": prompt}
        ]
    });

    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI API error {}: {}", status, text);
    }

    let json: serde_json::Value = resp.json().await?;
    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No response")
        .to_string();

    Ok(text)
}

/// Print deep analysis results
pub fn print_deep_result(result: &str) {
    println!();
    println!("{}", "  ========================================".magenta());
    println!("{}", "    merlint — Deep Analysis (AI)".magenta().bold());
    println!("{}", "  ========================================".magenta());
    println!();
    for line in result.lines() {
        println!("  {}", line);
    }
    println!();
}
