use std::collections::HashMap;

use crate::models::trace::TraceSession;

#[derive(Debug)]
pub struct EfficiencyAnalysis {
    pub total_calls: usize,
    pub total_tokens: u64,
    pub total_latency_ms: u64,
    pub tool_call_count: usize,
    pub unique_tool_calls: usize,
    pub retry_count: usize,
    pub loop_patterns: Vec<LoopPattern>,
    pub redundant_reads: Vec<RedundantRead>,
    pub tokens_per_call_avg: f64,
}

/// Detected loop: agent repeatedly calling the same tool with same/similar args
#[derive(Debug)]
pub struct LoopPattern {
    pub tool_name: String,
    pub call_indices: Vec<usize>,
    pub description: String,
}

/// Detected redundant file/resource reads
#[derive(Debug)]
pub struct RedundantRead {
    pub resource: String,
    pub read_count: usize,
    pub call_indices: Vec<usize>,
}

pub fn analyze_efficiency(session: &TraceSession) -> EfficiencyAnalysis {
    let entries = &session.entries;
    let total_calls = entries.len();
    let total_tokens = session.total_tokens();
    let total_latency_ms = session.total_latency_ms();

    // Extract all tool calls in order
    let mut tool_calls: Vec<(usize, String, String)> = Vec::new(); // (call_idx, name, args)
    let mut retry_count = 0;

    for (idx, entry) in entries.iter().enumerate() {
        for choice in &entry.response.choices {
            if let Some(ref msg) = choice.message {
                if let Some(ref calls) = msg.tool_calls {
                    for call in calls {
                        if let Some(ref f) = call.function {
                            tool_calls.push((idx, f.name.clone(), f.arguments.clone()));
                        }
                    }
                }
            }
        }

        // Detect retries: same tool call as previous response
        if idx > 0 {
            let prev_calls = get_tool_calls_from_entry(&entries[idx - 1]);
            let curr_calls = get_tool_calls_from_entry(&entries[idx]);
            for curr in &curr_calls {
                if prev_calls.contains(curr) {
                    retry_count += 1;
                }
            }
        }
    }

    let tool_call_count = tool_calls.len();
    let unique_tools: std::collections::HashSet<&str> =
        tool_calls.iter().map(|(_, n, _)| n.as_str()).collect();
    let unique_tool_calls = unique_tools.len();

    // Detect loop patterns: same tool called 3+ times with similar args
    let loop_patterns = detect_loops(&tool_calls);

    // Detect redundant reads: same resource read multiple times
    let redundant_reads = detect_redundant_reads(&tool_calls);

    let tokens_per_call_avg = if total_calls > 0 {
        total_tokens as f64 / total_calls as f64
    } else {
        0.0
    };

    EfficiencyAnalysis {
        total_calls,
        total_tokens,
        total_latency_ms,
        tool_call_count,
        unique_tool_calls,
        retry_count,
        loop_patterns,
        redundant_reads,
        tokens_per_call_avg,
    }
}

fn get_tool_calls_from_entry(
    entry: &crate::models::trace::TraceEntry,
) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    for choice in &entry.response.choices {
        if let Some(ref msg) = choice.message {
            if let Some(ref tool_calls) = msg.tool_calls {
                for call in tool_calls {
                    if let Some(ref f) = call.function {
                        calls.push((f.name.clone(), f.arguments.clone()));
                    }
                }
            }
        }
    }
    calls
}

fn detect_loops(tool_calls: &[(usize, String, String)]) -> Vec<LoopPattern> {
    let mut patterns = Vec::new();

    // Group consecutive calls by tool name
    let mut groups: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (idx, name, args) in tool_calls {
        groups
            .entry(name.clone())
            .or_default()
            .push((*idx, args.clone()));
    }

    for (name, calls) in &groups {
        if calls.len() < 3 {
            continue;
        }

        // Check for consecutive calls (within a window of 2 apart)
        let mut consecutive_runs: Vec<Vec<usize>> = Vec::new();
        let mut current_run = vec![calls[0].0];

        for i in 1..calls.len() {
            if calls[i].0 - calls[i - 1].0 <= 2 {
                current_run.push(calls[i].0);
            } else {
                if current_run.len() >= 3 {
                    consecutive_runs.push(current_run.clone());
                }
                current_run = vec![calls[i].0];
            }
        }
        if current_run.len() >= 3 {
            consecutive_runs.push(current_run);
        }

        for run in consecutive_runs {
            patterns.push(LoopPattern {
                tool_name: name.clone(),
                call_indices: run.clone(),
                description: format!(
                    "'{}' called {} times in calls #{}-#{}",
                    name,
                    run.len(),
                    run.first().unwrap(),
                    run.last().unwrap()
                ),
            });
        }
    }

    patterns
}

fn detect_redundant_reads(tool_calls: &[(usize, String, String)]) -> Vec<RedundantRead> {
    let read_tools = ["read_file", "Read", "read", "cat", "get_file_contents"];
    let mut reads: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, name, args) in tool_calls {
        if read_tools.iter().any(|r| name.contains(r)) {
            // Try to extract the file path from args
            let resource = extract_resource(args).unwrap_or_else(|| args.clone());
            reads.entry(resource).or_default().push(*idx);
        }
    }

    reads
        .into_iter()
        .filter(|(_, indices)| indices.len() > 1)
        .map(|(resource, indices)| RedundantRead {
            read_count: indices.len(),
            resource,
            call_indices: indices,
        })
        .collect()
}

fn extract_resource(args: &str) -> Option<String> {
    // Try JSON parse to get "path" or "file_path" or "file"
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
        for key in &["path", "file_path", "file", "filename"] {
            if let Some(s) = v.get(key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    None
}
