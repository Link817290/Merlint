use crate::analyzer::efficiency::EfficiencyAnalysis;
use crate::analyzer::token::SessionTokenSummary;
use crate::optimizer::plan::*;

/// Generate agent config recommendations (CLAUDE.md, .cursorrules, etc.)
pub fn optimize_efficiency(
    tokens: &SessionTokenSummary,
    efficiency: &EfficiencyAnalysis,
) -> Vec<OptimizationItem> {
    let mut items = Vec::new();

    // ── Redundant reads ──
    if !efficiency.redundant_reads.is_empty() {
        let resources: Vec<String> = efficiency.redundant_reads.iter()
            .map(|r| r.resource.clone())
            .collect();
        let total_extra_reads: usize = efficiency.redundant_reads.iter()
            .map(|r| r.read_count.saturating_sub(1))
            .sum();

        // Rough estimate: each redundant read wastes ~500 tokens (file content re-sent)
        let savings = (total_extra_reads as i64) * 500;

        items.push(OptimizationItem {
            id: "efficiency-redundant-reads".into(),
            category: OptCategory::Efficiency,
            severity: if total_extra_reads > 3 { OptSeverity::High } else { OptSeverity::Medium },
            title: format!("{} redundant file reads detected", total_extra_reads),
            description: format!(
                "Files read multiple times in one session: {}. Each re-read sends the full file content again as prompt tokens.",
                resources.join(", ")
            ),
            estimated_savings: savings,
            action: OptAction::AddCacheHints {
                resources: resources.clone(),
                hint: "Add to your agent config: 'Cache file contents in memory after first read. \
                    Do not re-read files unless they may have been modified.'".into(),
            },
        });
    }

    // ── Retry patterns ──
    if efficiency.retry_count > 0 {
        let savings = (efficiency.retry_count as i64) * 300;

        items.push(OptimizationItem {
            id: "efficiency-retries".into(),
            category: OptCategory::Efficiency,
            severity: if efficiency.retry_count > 3 { OptSeverity::High } else { OptSeverity::Medium },
            title: format!("{} identical retries detected", efficiency.retry_count),
            description: "The same tool was called with identical arguments consecutively. \
                This wastes a full round-trip of tokens each time.".into(),
            estimated_savings: savings,
            action: OptAction::ReduceRetries {
                pattern: "identical consecutive calls".into(),
                suggestion: "Add to agent config: 'If a tool call fails, analyze the error before retrying. \
                    Do not repeat the exact same call — adjust parameters or try an alternative approach.'".into(),
            },
        });
    }

    // ── Loop patterns ──
    if !efficiency.loop_patterns.is_empty() {
        for lp in &efficiency.loop_patterns {
            items.push(OptimizationItem {
                id: format!("efficiency-loop-{}", lp.tool_name),
                category: OptCategory::Efficiency,
                severity: OptSeverity::High,
                title: format!("Loop pattern: {}", lp.description),
                description: format!(
                    "Tool '{}' was called 3+ times consecutively, suggesting a loop pattern. \
                    This burns tokens on repetitive context.",
                    lp.tool_name
                ),
                estimated_savings: 1000,
                action: OptAction::ReduceRetries {
                    pattern: lp.description.clone(),
                    suggestion: format!(
                        "Batch operations for '{}' instead of calling one at a time. \
                        Or add a loop-detection guard to your agent config.",
                        lp.tool_name
                    ),
                },
            });
        }
    }

    // ── High token per call ──
    if tokens.num_calls > 0 {
        let avg = tokens.total_tokens as f64 / tokens.num_calls as f64;
        if avg > 10000.0 {
            items.push(OptimizationItem {
                id: "efficiency-high-avg-tokens".into(),
                category: OptCategory::Efficiency,
                severity: OptSeverity::Medium,
                title: format!("High avg tokens per call: {:.0}", avg),
                description: "Average token consumption per call is very high. Consider:\n\
                    - Reducing context window by summarizing old messages\n\
                    - Using shorter system prompts\n\
                    - Trimming tool result content before including in context".into(),
                estimated_savings: 0,
                action: OptAction::GenerateConfig {
                    file_path: "CLAUDE.md".into(),
                    content: "# Token Reduction\n\
                        - Summarize conversation history when context exceeds 8K tokens\n\
                        - Truncate long tool outputs (file reads, search results)\n\
                        - Remove resolved tool results from context\n".into(),
                },
            });
        }
    }

    items
}

/// Generate a combined CLAUDE.md content from all optimization items
pub fn generate_claude_md(plan: &OptimizationPlan) -> String {
    let mut sections: Vec<String> = Vec::new();

    sections.push("# Agent Optimization Guide".into());
    sections.push(format!("# Generated by merlint — {} optimizations applied\n", plan.items.len()));

    // Tools section
    let tool_items: Vec<_> = plan.items.iter()
        .filter(|i| matches!(i.category, OptCategory::Tools))
        .collect();
    if !tool_items.is_empty() {
        sections.push("## Tool Usage".into());
        for item in &tool_items {
            if let OptAction::PruneTools { remove, keep } = &item.action {
                if !remove.is_empty() {
                    sections.push(format!("- Remove unused tools: {}", remove.join(", ")));
                }
                if !keep.is_empty() {
                    sections.push(format!("- Keep active tools: {}", keep.join(", ")));
                }
            }
        }
        sections.push(String::new());
    }

    // Cache section
    let cache_items: Vec<_> = plan.items.iter()
        .filter(|i| matches!(i.category, OptCategory::Cache))
        .collect();
    if !cache_items.is_empty() {
        sections.push("## Cache Optimization".into());
        sections.push("- Keep the system prompt identical across all calls".into());
        sections.push("- Put static content first, dynamic content last".into());
        sections.push("- Do not modify tool definitions mid-session".into());
        sections.push(String::new());
    }

    // Efficiency section
    let eff_items: Vec<_> = plan.items.iter()
        .filter(|i| matches!(i.category, OptCategory::Efficiency))
        .collect();
    if !eff_items.is_empty() {
        sections.push("## Efficiency Rules".into());
        for item in &eff_items {
            match &item.action {
                OptAction::AddCacheHints { resources, hint } => {
                    sections.push(format!("- Cache file reads: {}", resources.join(", ")));
                    sections.push(format!("  {}", hint));
                }
                OptAction::ReduceRetries { suggestion, .. } => {
                    sections.push(format!("- {}", suggestion));
                }
                _ => {}
            }
        }
        sections.push(String::new());
    }

    sections.join("\n")
}

/// Generate a tools allowlist file (JSON)
pub fn generate_tools_allowlist(plan: &OptimizationPlan) -> Option<String> {
    for item in &plan.items {
        if let OptAction::PruneTools { keep, .. } = &item.action {
            if !keep.is_empty() {
                let obj = serde_json::json!({
                    "description": "Optimized tool allowlist generated by merlint",
                    "allowed_tools": keep,
                });
                return serde_json::to_string_pretty(&obj).ok();
            }
        }
    }
    None
}
