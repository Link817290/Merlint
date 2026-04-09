use colored::*;

use crate::analyzer::cache::CacheAnalysis;
use crate::analyzer::efficiency::EfficiencyAnalysis;
use crate::analyzer::token::SessionTokenSummary;
use crate::models::trace::TraceSession;

pub fn print_report(
    session: &TraceSession,
    tokens: &SessionTokenSummary,
    cache: &CacheAnalysis,
    efficiency: &EfficiencyAnalysis,
) {
    println!();
    println!("{}", "═══════════════════════════════════════════".bright_cyan());
    println!("{}", "         AgentBench Report".bright_cyan().bold());
    println!("{}", "═══════════════════════════════════════════".bright_cyan());
    println!();

    // ── Overview ──
    println!("{}", "▸ Overview".white().bold());
    println!("  Session:      {}", &session.id[..8.min(session.id.len())]);
    println!("  API Calls:    {}", tokens.num_calls);
    println!("  Total Tokens: {} (from API usage)", fmt(tokens.total_tokens));
    println!("  ├─ Prompt:     {}", fmt(tokens.total_prompt_tokens));
    println!("  └─ Completion: {}", fmt(tokens.total_completion_tokens));
    println!("  Total Time:   {:.1}s", efficiency.total_latency_ms as f64 / 1000.0);
    if tokens.num_calls > 0 {
        println!("  Avg Tok/Call: {:.0}", tokens.total_tokens as f64 / tokens.num_calls as f64);
    }
    println!();

    // ── Cache (real API data) ──
    println!("{}", "▸ Cache (API data)".white().bold());
    if tokens.cache_data_available {
        let total_input = tokens.total_prompt_tokens;
        let cached = tokens.total_cache_read_tokens;
        let created = tokens.total_cache_creation_tokens;
        let hit_pct = if total_input > 0 { cached as f64 / total_input as f64 * 100.0 } else { 0.0 };

        println!("  Cache Read:     {} ({:.1}% of prompt)", fmt(cached), hit_pct);
        println!("  Cache Created:  {}", fmt(created));

        let bar_len = (hit_pct / 5.0) as usize;
        let bar: String = "█".repeat(bar_len.min(20));
        let pad: String = "░".repeat(20 - bar_len.min(20));
        let color = if hit_pct > 60.0 { bar.green() } else if hit_pct > 30.0 { bar.yellow() } else { bar.red() };
        println!("  Hit Rate:       [{}{}] {:.0}%", color, pad.bright_black(), hit_pct);
    } else {
        println!("  {}", "No cache data from API (OpenAI doesn't report cache tokens)".dimmed());
    }

    // Structural cache analysis
    println!("  Prefix Stab.:   {:.0}%", cache.prefix_stability_ratio * 100.0);
    println!("  Theoretical:    {:.0}%", cache.theoretical_cache_hit_ratio * 100.0);
    for issue in &cache.issues {
        let icon = match issue.severity {
            crate::analyzer::cache::Severity::Critical => "✗".red(),
            crate::analyzer::cache::Severity::Warning => "⚠".yellow(),
        };
        println!("  {} {}", icon, issue.description);
        println!("    → {}", issue.suggestion.dimmed());
    }
    println!();

    // ── Structure Analysis ──
    println!("{}", "▸ Structure (exact counts)".white().bold());
    println!("  Avg Messages/Call:  {:.1}", tokens.avg_messages_per_call);
    println!("  Avg System Msgs:    {:.1}", tokens.avg_system_messages_per_call);
    println!("  Avg Tools Defined:  {:.0}", tokens.avg_tools_defined_per_call);
    println!();

    // ── Tool Efficiency ──
    println!("{}", "▸ Tool Efficiency".white().bold());
    println!(
        "  Defined: {}    Used: {}    Unused: {}",
        tokens.tools_defined, tokens.tools_used,
        tokens.tools_defined.saturating_sub(tokens.tools_used)
    );
    if tokens.tools_used < tokens.tools_defined {
        let unused = tokens.tools_defined - tokens.tools_used;
        let pct = unused as f64 / tokens.tools_defined as f64 * 100.0;
        println!(
            "  {} {:.0}% of defined tools never used — each wastes prompt tokens every call",
            "⚠".yellow(),
            pct
        );
        if !tokens.tool_names_unused.is_empty() && tokens.tool_names_unused.len() <= 15 {
            println!("    Unused: {}", tokens.tool_names_unused.join(", "));
        } else if !tokens.tool_names_unused.is_empty() {
            println!("    {} unused tools (too many to list)", tokens.tool_names_unused.len());
        }
    } else if tokens.tools_defined > 0 {
        println!("  {} All defined tools were used", "✓".green());
    }
    println!();

    // ── Efficiency Issues ──
    println!("{}", "▸ Efficiency".white().bold());
    println!("  Tool Calls: {}  (unique tools: {})", efficiency.tool_call_count, efficiency.unique_tool_calls);
    if efficiency.retry_count > 0 {
        println!(
            "  {} {} retries (same tool+args repeated consecutively)",
            "⚠".yellow(),
            efficiency.retry_count
        );
    }
    for lp in &efficiency.loop_patterns {
        println!("  {} Loop: {}", "✗".red(), lp.description);
    }
    for rr in &efficiency.redundant_reads {
        println!(
            "  {} '{}' read {} times (calls: {:?})",
            "⚠".yellow(), rr.resource, rr.read_count, rr.call_indices
        );
    }
    if efficiency.loop_patterns.is_empty()
        && efficiency.redundant_reads.is_empty()
        && efficiency.retry_count == 0
    {
        println!("  {} No efficiency issues detected", "✓".green());
    }

    // ── Per-call breakdown ──
    if tokens.per_call.len() > 1 {
        println!();
        println!("{}", "▸ Per-Call Breakdown".white().bold());
        println!("  {:>4}  {:>8}  {:>8}  {:>8}  {:>6}  {:>5}  {:>5}",
            "#", "prompt", "complet.", "total", "cache%", "msgs", "tools");
        for c in &tokens.per_call {
            let cache_pct = match c.cache_read {
                Some(cr) if c.prompt_tokens > 0 => format!("{:.0}%", cr as f64 / c.prompt_tokens as f64 * 100.0),
                _ => "-".to_string(),
            };
            println!("  {:>4}  {:>8}  {:>8}  {:>8}  {:>6}  {:>5}  {:>5}",
                c.call_index,
                fmt(c.prompt_tokens),
                fmt(c.completion_tokens),
                fmt(c.total_tokens),
                cache_pct,
                c.num_messages,
                c.num_tools_defined,
            );
        }
    }

    println!();
    println!("{}", "═══════════════════════════════════════════".bright_cyan());
    println!();
}

fn fmt(n: u64) -> String {
    if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1_000_000.0) }
    else if n >= 1_000 { format!("{:.1}K", n as f64 / 1_000.0) }
    else { n.to_string() }
}
