use crate::analyzer::cache::CacheAnalysis;
use crate::analyzer::token::SessionTokenSummary;
use crate::models::trace::TraceSession;
use crate::optimizer::plan::*;

/// Analyze prompt structure and generate cache optimization recommendations
pub fn optimize_prompt(
    session: &TraceSession,
    tokens: &SessionTokenSummary,
    cache: &CacheAnalysis,
) -> Vec<OptimizationItem> {
    let mut items = Vec::new();

    // ── Check prefix stability ──
    if cache.prefix_stability_ratio < 0.8 && tokens.num_calls > 1 {
        let current_hit = if tokens.cache_data_available && tokens.total_prompt_tokens > 0 {
            tokens.total_cache_read_tokens as f64 / tokens.total_prompt_tokens as f64 * 100.0
        } else {
            cache.theoretical_cache_hit_ratio * 100.0
        };

        let potential_hit = 80.0_f64.min(current_hit + 30.0);
        let improvement = potential_hit - current_hit;

        items.push(OptimizationItem {
            id: "cache-prefix-stability".into(),
            category: OptCategory::Cache,
            severity: if improvement > 20.0 { OptSeverity::High } else { OptSeverity::Medium },
            title: format!(
                "Prompt prefix unstable ({:.0}% stability) — cache hit rate {:.0}% -> {:.0}%",
                cache.prefix_stability_ratio * 100.0, current_hit, potential_hit
            ),
            description: "System prompt or early messages change between calls, invalidating the KV cache. \
                Static content (system prompt, tool definitions, instructions) should always come first, \
                with dynamic content (user messages, tool results) appended at the end.".into(),
            estimated_savings: 0,
            action: OptAction::RestructurePrompt {
                suggestion: build_prompt_restructure_suggestion(session),
            },
        });
    }

    // ── Check system prompt changes ──
    if has_system_prompt_changes(session) {
        items.push(OptimizationItem {
            id: "cache-system-prompt-change".into(),
            category: OptCategory::Cache,
            severity: OptSeverity::High,
            title: "System prompt changes between calls".into(),
            description: "The system prompt was modified mid-session. This completely invalidates \
                the prompt cache. Keep the system prompt identical across all calls in a session. \
                If you need dynamic instructions, append them as user messages instead.".into(),
            estimated_savings: 0,
            action: OptAction::GenerateConfig {
                file_path: "CLAUDE.md".into(),
                content: "# Cache Optimization\n\
                    - NEVER modify the system prompt mid-session\n\
                    - Put all static instructions in the system prompt\n\
                    - Append dynamic context as user messages at the END of the conversation\n\
                    - Keep tool definitions consistent across calls\n".into(),
            },
        });
    }

    // ── Check for large system prompts ──
    let avg_system_msgs = tokens.avg_system_messages_per_call;
    if avg_system_msgs > 2.0 {
        items.push(OptimizationItem {
            id: "prompt-multiple-system".into(),
            category: OptCategory::Prompt,
            severity: OptSeverity::Low,
            title: format!("Avg {:.1} system messages per call", avg_system_msgs),
            description: "Multiple system messages increase prompt size. Consider consolidating \
                into a single system message for better cache performance.".into(),
            estimated_savings: 0,
            action: OptAction::RestructurePrompt {
                suggestion: "Consolidate multiple system messages into one.".into(),
            },
        });
    }

    items
}

fn build_prompt_restructure_suggestion(session: &TraceSession) -> String {
    let mut suggestion = String::new();
    suggestion.push_str("Recommended message order for maximum cache hits:\n");
    suggestion.push_str("  1. System prompt (static, never changes)\n");
    suggestion.push_str("  2. Tool definitions (consistent across calls)\n");
    suggestion.push_str("  3. Historical messages (append-only)\n");
    suggestion.push_str("  4. Current user message (dynamic, at the end)\n");

    if !session.entries.is_empty() {
        let first = &session.entries[0];
        let sys_count = first.request.messages.iter()
            .filter(|m| m.role == "system")
            .count();
        if sys_count > 1 {
            suggestion.push_str(&format!(
                "\nNote: First call has {} system messages. Merge them into one.",
                sys_count
            ));
        }
    }

    suggestion
}

fn has_system_prompt_changes(session: &TraceSession) -> bool {
    if session.entries.len() < 2 {
        return false;
    }

    let get_system = |entry: &crate::models::trace::TraceEntry| -> String {
        entry.request.messages.iter()
            .filter(|m| m.role == "system")
            .map(|m| match &m.content {
                Some(crate::models::api::MessageContent::Text(t)) => t.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("")
    };

    let first_system = get_system(&session.entries[0]);
    session.entries.iter().skip(1).any(|e| get_system(e) != first_system)
}
