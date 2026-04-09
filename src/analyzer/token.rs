use std::collections::{HashMap, HashSet};

use crate::models::trace::TraceSession;

/// Real token usage from API — no estimation
#[derive(Debug, Serialize)]
pub struct SessionTokenSummary {
    /// Total tokens across all calls (from API usage)
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,

    /// Cache data (Anthropic only, 0 if unavailable)
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub cache_data_available: bool,

    /// Structural analysis (exact counts, not token estimates)
    pub num_calls: usize,
    pub avg_messages_per_call: f64,
    pub avg_system_messages_per_call: f64,
    pub avg_tools_defined_per_call: f64,

    /// Tool usage analysis
    pub tools_defined: usize,
    pub tools_used: usize,
    pub tool_names_defined: Vec<String>,
    pub tool_names_used: Vec<String>,
    pub tool_names_unused: Vec<String>,

    /// Per-tool call counts (tool_name -> number of times called)
    pub tool_call_counts: Vec<(String, usize)>,

    /// Per-call breakdown (real API data)
    pub per_call: Vec<CallTokens>,
}

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CallTokens {
    pub call_index: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cache_read: Option<u64>,
    pub cache_creation: Option<u64>,
    pub num_messages: usize,
    pub num_tools_defined: usize,
    pub num_tool_calls_made: usize,
    pub latency_ms: u64,
}

pub fn summarize_session_tokens(session: &TraceSession) -> SessionTokenSummary {
    let entries = &session.entries;
    let num_calls = entries.len();

    let mut total_prompt = 0u64;
    let mut total_completion = 0u64;
    let mut total_tokens = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_creation = 0u64;
    let mut cache_data_available = false;

    let mut total_messages = 0usize;
    let mut total_system_messages = 0usize;
    let mut total_tools_defined = 0usize;

    let mut all_defined: HashSet<String> = HashSet::new();
    let mut all_used: HashSet<String> = HashSet::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();

    let mut per_call = Vec::with_capacity(num_calls);

    for (idx, entry) in entries.iter().enumerate() {
        let prompt = entry.prompt_tokens().unwrap_or(0);
        let completion = entry.completion_tokens().unwrap_or(0);
        let total = entry.total_tokens().unwrap_or(0);
        let cache_read = entry.cache_read_tokens();
        let cache_creation = entry.cache_creation_tokens();

        total_prompt += prompt;
        total_completion += completion;
        total_tokens += total;

        if let Some(cr) = cache_read {
            total_cache_read += cr;
            cache_data_available = true;
        }
        if let Some(cc) = cache_creation {
            total_cache_creation += cc;
            cache_data_available = true;
        }

        // Structural counts
        let msg_count = entry.request.messages.len();
        let sys_count = entry.request.messages.iter().filter(|m| m.role == "system").count();
        let tool_def_count = entry.request.tools.len();

        total_messages += msg_count;
        total_system_messages += sys_count;
        total_tools_defined += tool_def_count;

        // Track tool definitions
        for tool in &entry.request.tools {
            if let Some(ref f) = tool.function {
                all_defined.insert(f.name.clone());
            }
        }

        // Track tool usage from response
        let mut tool_calls_made = 0;
        for choice in &entry.response.choices {
            if let Some(ref msg) = choice.message {
                if let Some(ref calls) = msg.tool_calls {
                    for call in calls {
                        if let Some(ref f) = call.function {
                            all_used.insert(f.name.clone());
                            *tool_counts.entry(f.name.clone()).or_insert(0) += 1;
                            tool_calls_made += 1;
                        }
                    }
                }
            }
        }

        per_call.push(CallTokens {
            call_index: idx,
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cache_read,
            cache_creation,
            num_messages: msg_count,
            num_tools_defined: tool_def_count,
            num_tool_calls_made: tool_calls_made,
            latency_ms: entry.latency_ms,
        });
    }

    let mut tool_names_defined: Vec<String> = all_defined.iter().cloned().collect();
    tool_names_defined.sort();
    let mut tool_names_used: Vec<String> = all_used.iter().cloned().collect();
    tool_names_used.sort();
    let mut tool_call_counts: Vec<(String, usize)> = tool_counts.into_iter().collect();
    tool_call_counts.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by count descending

    let tool_names_unused: Vec<String> = all_defined
        .difference(&all_used)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .collect();

    SessionTokenSummary {
        total_prompt_tokens: total_prompt,
        total_completion_tokens: total_completion,
        total_tokens,
        total_cache_read_tokens: total_cache_read,
        total_cache_creation_tokens: total_cache_creation,
        cache_data_available,
        num_calls,
        avg_messages_per_call: if num_calls > 0 { total_messages as f64 / num_calls as f64 } else { 0.0 },
        avg_system_messages_per_call: if num_calls > 0 { total_system_messages as f64 / num_calls as f64 } else { 0.0 },
        avg_tools_defined_per_call: if num_calls > 0 { total_tools_defined as f64 / num_calls as f64 } else { 0.0 },
        tools_defined: all_defined.len(),
        tools_used: all_used.len(),
        tool_names_defined,
        tool_names_used,
        tool_names_unused,
        tool_call_counts,
        per_call,
    }
}
