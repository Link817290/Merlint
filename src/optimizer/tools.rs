use crate::analyzer::token::SessionTokenSummary;
use crate::optimizer::plan::*;

/// Analyze tool usage and generate pruning recommendations
pub fn optimize_tools(tokens: &SessionTokenSummary) -> Vec<OptimizationItem> {
    let mut items = Vec::new();

    let unused_count = tokens.tools_defined.saturating_sub(tokens.tools_used);
    if unused_count == 0 {
        return items;
    }

    // Estimate ~200 tokens per tool definition (schema + description)
    let estimated_savings = (unused_count as i64) * 200;

    let severity = if unused_count > 5 {
        OptSeverity::High
    } else if unused_count > 2 {
        OptSeverity::Medium
    } else {
        OptSeverity::Low
    };

    let pct = if tokens.tools_defined > 0 {
        unused_count as f64 / tokens.tools_defined as f64 * 100.0
    } else {
        0.0
    };

    items.push(OptimizationItem {
        id: "tools-prune-unused".into(),
        category: OptCategory::Tools,
        severity,
        title: format!("Remove {} unused tools ({:.0}% of defined)", unused_count, pct),
        description: format!(
            "Defined {} tools but only {} were ever called. Each unused tool wastes ~200 tokens per API call in the tools schema.",
            tokens.tools_defined, tokens.tools_used
        ),
        estimated_savings,
        action: OptAction::PruneTools {
            remove: tokens.tool_names_unused.clone(),
            keep: tokens.tool_names_used.clone(),
        },
    });

    // Check for high tool count even if all used
    if tokens.tools_defined > 20 {
        items.push(OptimizationItem {
            id: "tools-high-count".into(),
            category: OptCategory::Tools,
            severity: OptSeverity::Medium,
            title: format!("{} tools defined — consider lazy loading", tokens.tools_defined),
            description: "Large tool schemas consume significant prompt tokens. Consider defining tools dynamically based on context rather than all upfront.".into(),
            estimated_savings: 0,
            action: OptAction::GenerateConfig {
                file_path: "CLAUDE.md".into(),
                content: "# Tool Optimization\nConsider using dynamic tool registration to only load tools relevant to the current task context.\n".into(),
            },
        });
    }

    items
}
