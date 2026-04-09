use crate::models::trace::TraceSession;

#[derive(Debug)]
pub struct CacheAnalysis {
    /// How many consecutive requests share the same prefix
    pub prefix_stability_ratio: f64,
    /// Actual cache hit ratio from API usage data (if available)
    pub actual_cache_hit_ratio: Option<f64>,
    /// Theoretical cache hit ratio based on prefix overlap
    pub theoretical_cache_hit_ratio: f64,
    /// Specific issues found
    pub issues: Vec<CacheIssue>,
    /// Per-pair overlap details
    pub pair_overlaps: Vec<PairOverlap>,
}

#[derive(Debug)]
pub struct PairOverlap {
    pub call_index: usize,
    pub shared_prefix_chars: usize,
    pub total_chars_a: usize,
    pub total_chars_b: usize,
    pub overlap_ratio: f64,
}

#[derive(Debug)]
pub struct CacheIssue {
    pub call_index: usize,
    pub severity: Severity,
    pub description: String,
    pub suggestion: String,
}

#[derive(Debug)]
pub enum Severity {
    Warning,
    Critical,
}

pub fn analyze_cache(session: &TraceSession) -> CacheAnalysis {
    let entries = &session.entries;

    if entries.len() < 2 {
        return CacheAnalysis {
            prefix_stability_ratio: 1.0,
            actual_cache_hit_ratio: None,
            theoretical_cache_hit_ratio: 1.0,
            issues: Vec::new(),
            pair_overlaps: Vec::new(),
        };
    }

    let mut pair_overlaps = Vec::new();
    let mut total_overlap_ratio = 0.0;
    let mut issues = Vec::new();

    // Compare consecutive requests
    for i in 1..entries.len() {
        let prev = serialize_messages(&entries[i - 1].request.messages);
        let curr = serialize_messages(&entries[i].request.messages);

        let shared = common_prefix_len(&prev, &curr);
        let max_len = prev.len().max(curr.len());
        let ratio = if max_len > 0 {
            shared as f64 / max_len as f64
        } else {
            1.0
        };

        pair_overlaps.push(PairOverlap {
            call_index: i,
            shared_prefix_chars: shared,
            total_chars_a: prev.len(),
            total_chars_b: curr.len(),
            overlap_ratio: ratio,
        });

        total_overlap_ratio += ratio;

        // Detect issues
        if ratio < 0.3 {
            issues.push(CacheIssue {
                call_index: i,
                severity: Severity::Critical,
                description: format!(
                    "Call #{} shares only {:.0}% prefix with previous call",
                    i,
                    ratio * 100.0
                ),
                suggestion: "Messages may be reordered between calls. Keep system prompt and tool definitions stable at the start.".into(),
            });
        } else if ratio < 0.6 {
            issues.push(CacheIssue {
                call_index: i,
                severity: Severity::Warning,
                description: format!(
                    "Call #{} has {:.0}% prefix overlap (suboptimal)",
                    i,
                    ratio * 100.0
                ),
                suggestion: "Consider moving dynamic content to the end of the message array.".into(),
            });
        }

        // Detect system prompt changes
        let prev_sys = get_system_prompt(&entries[i - 1].request.messages);
        let curr_sys = get_system_prompt(&entries[i].request.messages);
        if prev_sys != curr_sys && !prev_sys.is_empty() {
            issues.push(CacheIssue {
                call_index: i,
                severity: Severity::Critical,
                description: format!("System prompt changed between call #{} and #{}", i - 1, i),
                suggestion: "Changing system prompt between calls destroys cache. Keep it stable.".into(),
            });
        }
    }

    let theoretical_cache_hit_ratio = total_overlap_ratio / (entries.len() - 1) as f64;

    // Calculate actual cache hit ratio from Anthropic usage data
    let actual_cache_hit_ratio = calculate_actual_cache_ratio(session);

    // Check prefix stability (do all requests start the same way?)
    let prefix_stability_ratio = if pair_overlaps.is_empty() {
        1.0
    } else {
        pair_overlaps.iter().filter(|p| p.overlap_ratio > 0.5).count() as f64
            / pair_overlaps.len() as f64
    };

    CacheAnalysis {
        prefix_stability_ratio,
        actual_cache_hit_ratio,
        theoretical_cache_hit_ratio,
        issues,
        pair_overlaps,
    }
}

fn serialize_messages(messages: &[crate::models::api::Message]) -> String {
    messages
        .iter()
        .map(|m| {
            let content = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
            format!("{}:{}", m.role, content)
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .count()
}

fn get_system_prompt(messages: &[crate::models::api::Message]) -> String {
    messages
        .iter()
        .filter(|m| m.role == "system")
        .filter_map(|m| m.content.as_ref())
        .map(|c| c.as_text())
        .collect::<Vec<_>>()
        .join("")
}

fn calculate_actual_cache_ratio(session: &TraceSession) -> Option<f64> {
    let mut total_input = 0u64;
    let mut total_cached = 0u64;
    let mut has_data = false;

    for entry in &session.entries {
        if let Some(ref usage) = entry.response.usage {
            if let Some(cached) = usage.cache_read_input_tokens {
                total_cached += cached;
                total_input += usage.prompt_tokens;
                has_data = true;
            }
        }
    }

    if has_data && total_input > 0 {
        Some(total_cached as f64 / total_input as f64)
    } else {
        None
    }
}
