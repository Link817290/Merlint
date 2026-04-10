use std::path::PathBuf;

use merlint::analyzer;
use merlint::report;

use super::helpers::{smart_load, QueryMetric, SourceFormat};

pub fn run(
    trace_file: PathBuf,
    source: Option<SourceFormat>,
    metric: QueryMetric,
) -> anyhow::Result<()> {
    let session = match smart_load(&trace_file, source.as_ref()) {
        Ok(s) => s,
        Err(_) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "error": "no trace data",
                    "hint": "run `merlint scan` or `merlint proxy --daemon`"
                }))?
            );
            return Ok(());
        }
    };

    if session.entries.is_empty() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"error": "no entries", "num_calls": 0}))?
        );
        return Ok(());
    }

    let ts = analyzer::token::summarize_session_tokens(&session);
    let ca = analyzer::cache::analyze_cache(&session);
    let ea = analyzer::efficiency::analyze_efficiency(&session);

    let out = match metric {
        QueryMetric::All => report::json::generate_json(&session, &ts, &ca, &ea),
        QueryMetric::Tokens => serde_json::to_string_pretty(&serde_json::json!({
            "total_tokens": ts.total_tokens, "prompt_tokens": ts.total_prompt_tokens,
            "completion_tokens": ts.total_completion_tokens, "num_calls": ts.num_calls,
            "avg_per_call": if ts.num_calls > 0 { ts.total_tokens as f64 / ts.num_calls as f64 } else { 0.0 },
        }))?,
        QueryMetric::Cache => {
            let hr = if ts.cache_data_available && ts.total_prompt_tokens > 0 {
                Some(ts.total_cache_read_tokens as f64 / ts.total_prompt_tokens as f64)
            } else {
                None
            };
            serde_json::to_string_pretty(&serde_json::json!({
                "data_available": ts.cache_data_available, "cache_read_tokens": ts.total_cache_read_tokens,
                "cache_creation_tokens": ts.total_cache_creation_tokens, "hit_ratio": hr,
                "prefix_stability": ca.prefix_stability_ratio,
                "theoretical_hit_ratio": ca.theoretical_cache_hit_ratio,
                "issues": ca.issues.len(),
            }))?
        }
        QueryMetric::Tools => serde_json::to_string_pretty(&serde_json::json!({
            "defined": ts.tools_defined, "used": ts.tools_used,
            "unused": ts.tools_defined.saturating_sub(ts.tools_used),
            "unused_names": ts.tool_names_unused, "used_names": ts.tool_names_used,
            "avg_defined_per_call": ts.avg_tools_defined_per_call,
        }))?,
        QueryMetric::Efficiency => serde_json::to_string_pretty(&serde_json::json!({
            "total_calls": ea.total_calls, "tool_calls": ea.tool_call_count,
            "retries": ea.retry_count, "loop_patterns": ea.loop_patterns.len(),
            "redundant_reads": ea.redundant_reads.len(),
            "tokens_per_call_avg": ea.tokens_per_call_avg,
        }))?,
        QueryMetric::Calls => serde_json::to_string_pretty(&ts.per_call)?,
    };
    println!("{}", out);

    Ok(())
}
