use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::api::ChatRequest;

/// (msg_index, block_index, content_hash, content_length)
type FileOccurrence = (usize, Option<usize>, u64, usize);

/// Tracks tool usage across the session and optimizes requests in real-time
pub struct RequestTransformer {
    /// Tools that have actually been used (called) in this session
    tools_used: HashSet<String>,
    /// Per-tool call counts for this session
    tool_call_counts: HashMap<String, usize>,
    /// Tools that have been defined at least once (superset)
    tools_seen: HashSet<String>,
    /// Number of requests processed
    request_count: usize,
    /// Cumulative tokens saved by transformations
    tokens_saved: i64,
    /// File read cache: file_path -> (content_hash, content_length)
    file_cache: HashMap<String, (u64, usize)>,
    /// Stats: number of cache hits this session
    pub file_cache_hits: usize,
    /// Frozen prune set: once we decide which tools to keep, lock it for cache stability
    frozen_tools: Option<HashSet<String>>,
    /// Cache hit tracking: (cache_read_tokens, total_prompt_tokens) from recent responses
    cache_stats: (u64, u64),
    /// Whether tool pruning is suspended due to high cache hit rate
    pruning_suspended: bool,
}

/// Result of transforming a raw JSON request (Anthropic-native format)
pub struct RawTransformResult {
    pub estimated_tokens_saved: i64,
    pub messages_optimized: usize,
    pub tools_pruned: usize,
}

/// Result of transforming a request
pub struct TransformResult {
    pub request: ChatRequest,
    pub tools_pruned: usize,
    pub messages_merged: usize,
    pub estimated_tokens_saved: i64,
}

impl Default for RequestTransformer {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestTransformer {
    pub fn new() -> Self {
        Self {
            tools_used: HashSet::new(),
            tool_call_counts: HashMap::new(),
            tools_seen: HashSet::new(),
            request_count: 0,
            tokens_saved: 0,
            file_cache: HashMap::new(),
            file_cache_hits: 0,
            frozen_tools: None,
            cache_stats: (0, 0),
            pruning_suspended: false,
        }
    }

    /// Record cache statistics from an API response to decide whether
    /// tool pruning is worth the cache invalidation cost.
    ///
    /// Anthropic's prompt cache has a 5-minute TTL. If the cache is warm
    /// (high hit rate), pruning tools would invalidate it — costing far
    /// more tokens than pruning saves.
    ///
    /// Timeline: response stats arrive AFTER the request. So by the time
    /// request #3 runs (first potential prune), we already have stats from
    /// responses #1 and #2. If those show warm cache, we suspend pruning
    /// BEFORE it ever fires.
    pub fn record_cache_stats(&mut self, cache_read_tokens: u64, prompt_tokens: u64) {
        self.cache_stats.0 += cache_read_tokens;
        self.cache_stats.1 += prompt_tokens;

        if self.cache_stats.1 > 0 {
            let hit_rate = self.cache_stats.0 as f64 / self.cache_stats.1 as f64;
            // Suspend if cache hit rate > 30%. This threshold is deliberately
            // low because breaking a warm cache costs ~90% of prompt tokens
            // (full price instead of 10%), while pruning only saves ~4K tokens.
            // Example: 50K prompt, 30% cached = 15K at 0.1x = 1.5K effective.
            //          Breaking cache = 15K at 1.0x = 15K. Net loss = 13.5K.
            //          Pruning saves ~4K. Still a net loss of 9.5K.
            if hit_rate > 0.3 {
                if !self.pruning_suspended {
                    self.pruning_suspended = true;
                    // If we already froze a pruned set, clear it so the next
                    // request sends full tools (restore cache compatibility)
                    self.frozen_tools = None;
                }
            } else if self.pruning_suspended && hit_rate < 0.1 {
                // Only resume pruning if cache hit rate drops very low,
                // indicating the cache is no longer warm (e.g., >5min gap)
                self.pruning_suspended = false;
            }
        }
    }

    /// Check if tool pruning is currently suspended due to warm cache.
    pub fn is_pruning_suspended(&self) -> bool {
        self.pruning_suspended
    }

    /// Get the current cache hit rate (0.0 to 1.0).
    pub fn cache_hit_rate(&self) -> f64 {
        if self.cache_stats.1 > 0 {
            self.cache_stats.0 as f64 / self.cache_stats.1 as f64
        } else {
            0.0
        }
    }

    /// Record which tools were used in a response (call after getting response)
    pub fn record_tool_usage(&mut self, tool_names: &[String]) {
        for name in tool_names {
            self.tools_used.insert(name.clone());
            *self.tool_call_counts.entry(name.clone()).or_insert(0) += 1;
        }
    }

    /// Transform a request to reduce token usage
    pub fn transform(&mut self, mut request: ChatRequest) -> TransformResult {
        self.request_count += 1;
        let mut tools_pruned = 0;
        let mut messages_merged = 0;
        let mut estimated_tokens_saved: i64 = 0;

        // === Optimization 1: Prune unused tools (cache-aware) ===
        // After the first few calls, we know which tools are actually used.
        // Remove tools that were defined but never called.
        //
        // Cache protection:
        // - Once we decide which tools to keep, freeze that decision so the
        //   tools array stays stable → Anthropic's prompt cache stays valid.
        // - If the API response shows high cache hit rate (>50%), suspend
        //   pruning entirely — cache savings > token savings.
        // - Frozen prune set is cleared if a NEW tool appears (agent added
        //   a tool we haven't seen before).
        if self.pruning_suspended {
            // Cache is working well — don't touch tools at all
            for tool in &request.tools {
                if let Some(ref f) = tool.function {
                    self.tools_seen.insert(f.name.clone());
                }
            }
        } else if self.request_count >= 3 && !self.tools_used.is_empty() {
            let original_count = request.tools.len();

            let current_names: HashSet<String> = request.tools.iter()
                .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
                .collect();

            // Check if any new tools appeared that we haven't seen before
            let new_tools: HashSet<String> = current_names.difference(&self.tools_seen).cloned().collect();
            let has_new_tools = !new_tools.is_empty();

            // Update seen set
            for name in &current_names {
                self.tools_seen.insert(name.clone());
            }

            // If we have a frozen tool set AND no new tools appeared, reuse it
            if let Some(ref frozen) = self.frozen_tools {
                if !has_new_tools {
                    request.tools.retain(|tool| {
                        if let Some(ref f) = tool.function {
                            frozen.contains(&f.name)
                        } else {
                            true
                        }
                    });
                    tools_pruned = original_count - request.tools.len();
                    estimated_tokens_saved += tools_pruned as i64 * 200;
                } else {
                    // New tools appeared — recompute and re-freeze
                    self.frozen_tools = None;
                }
            }

            // No frozen set yet (first prune, or invalidated by new tools) — compute it
            if self.frozen_tools.is_none() {
                request.tools.retain(|tool| {
                    if let Some(ref f) = tool.function {
                        self.tools_used.contains(&f.name) || new_tools.contains(&f.name)
                    } else {
                        true
                    }
                });

                // Freeze this decision
                let kept: HashSet<String> = request.tools.iter()
                    .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
                    .collect();
                self.frozen_tools = Some(kept);

                tools_pruned = original_count - request.tools.len();
                estimated_tokens_saved += tools_pruned as i64 * 200;
            }
        } else {
            // Just record tool names for future pruning
            for tool in &request.tools {
                if let Some(ref f) = tool.function {
                    self.tools_seen.insert(f.name.clone());
                }
            }
        }

        // === Optimization 2: Merge consecutive system messages ===
        if request.messages.len() > 1 {
            let sys_count = request.messages.iter()
                .filter(|m| m.role == "system")
                .count();
            if sys_count > 1 {
                let mut merged_content = String::new();
                let mut non_system = Vec::new();
                let mut first_sys_idx = None;

                for (i, msg) in request.messages.iter().enumerate() {
                    if msg.role == "system" {
                        if first_sys_idx.is_none() {
                            first_sys_idx = Some(i);
                        }
                        if let Some(ref content) = msg.content {
                            if !merged_content.is_empty() {
                                merged_content.push_str("\n\n");
                            }
                            merged_content.push_str(&content.as_text());
                        }
                    } else {
                        non_system.push(msg.clone());
                    }
                }

                if sys_count > 1 {
                    let mut new_messages = Vec::with_capacity(non_system.len() + 1);
                    new_messages.push(crate::models::api::Message {
                        role: "system".into(),
                        content: Some(crate::models::api::MessageContent::Text(merged_content)),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                    new_messages.extend(non_system);
                    messages_merged = sys_count - 1;
                    // Rough estimate: ~50 tokens overhead per extra system message
                    estimated_tokens_saved += messages_merged as i64 * 50;
                    request.messages = new_messages;
                }
            }
        }

        // === Optimization 3: Deduplicate consecutive identical tool results ===
        if request.messages.len() > 1 {
            let mut new_messages = Vec::with_capacity(request.messages.len());
            let mut prev_tool_content: Option<String> = None;
            let mut did_dedup = false;

            for msg in &request.messages {
                if msg.role == "tool" {
                    let content_text = msg.content.as_ref()
                        .map(|c| c.as_text())
                        .unwrap_or_default();

                    if let Some(ref prev) = prev_tool_content {
                        if *prev == content_text && content_text.len() > 500 {
                            // Replace duplicate tool result with short note
                            let mut dedup_msg = msg.clone();
                            dedup_msg.content = Some(crate::models::api::MessageContent::Text(
                                "[same content as previous tool result]".into()
                            ));
                            let saved_chars = content_text.len() - 40;
                            // ~4 chars per token
                            estimated_tokens_saved += (saved_chars as i64) / 4;
                            new_messages.push(dedup_msg);
                            did_dedup = true;
                            continue;
                        }
                    }
                    prev_tool_content = Some(content_text);
                } else {
                    prev_tool_content = None;
                }
                new_messages.push(msg.clone());
            }

            if did_dedup {
                request.messages = new_messages;
            }
        }

        // === Optimization 4: Cache redundant file reads ===
        // Track ReadFile results by file path. If the same file is read again
        // with identical content AND the original content is still present
        // elsewhere in the messages array, replace with a short summary.
        //
        // Safety: we ONLY replace a duplicate if another copy of the same
        // content still exists in earlier messages. This prevents data loss
        // when the agent framework truncates old messages.
        {
            // Step 1: Build a map of tool_call_id -> file_path from assistant messages
            let mut call_id_to_path: HashMap<String, String> = HashMap::new();
            for msg in &request.messages {
                if msg.role == "assistant" {
                    if let Some(ref calls) = msg.tool_calls {
                        for call in calls {
                            if let Some(ref f) = call.function {
                                if is_file_read_tool(&f.name) {
                                    if let Some(path) = extract_file_path(&f.arguments) {
                                        if let Some(ref id) = call.id {
                                            call_id_to_path.insert(id.clone(), path);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Step 2: For each tool result, check if it's a redundant file read
            if !call_id_to_path.is_empty() {
                // First pass: collect all tool results with their content hashes,
                // keyed by file path. Track which message indices have which content.
                let mut path_occurrences: HashMap<String, Vec<(usize, u64, usize)>> = HashMap::new();

                for (i, msg) in request.messages.iter().enumerate() {
                    if msg.role == "tool" {
                        if let Some(ref call_id) = msg.tool_call_id {
                            if let Some(path) = call_id_to_path.get(call_id) {
                                let content_text = msg.content.as_ref()
                                    .map(|c| c.as_text())
                                    .unwrap_or_default();
                                if content_text.len() > 200 {
                                    let hash = simple_hash(&content_text);
                                    path_occurrences.entry(path.clone())
                                        .or_default()
                                        .push((i, hash, content_text.len()));
                                }
                            }
                        }
                    }
                }

                // Second pass: for each file path that appears multiple times
                // with the same hash, mark all but the FIRST occurrence for replacement.
                let mut indices_to_replace: HashSet<usize> = HashSet::new();
                let mut replacement_info: HashMap<usize, (String, usize)> = HashMap::new();

                for (path, occurrences) in &path_occurrences {
                    if occurrences.len() < 2 {
                        // Also update the cross-request cache for single occurrences
                        let (_, hash, len) = occurrences[0];
                        self.file_cache.insert(path.clone(), (hash, len));
                        continue;
                    }

                    // Group by hash to find duplicates
                    let first_hash = occurrences[0].1;
                    self.file_cache.insert(path.clone(), (first_hash, occurrences[0].2));

                    for &(idx, hash, len) in &occurrences[1..] {
                        if hash == first_hash {
                            // Same content as first read — safe to replace
                            // because the first copy is still in the messages
                            indices_to_replace.insert(idx);
                            replacement_info.insert(idx, (path.clone(), len));
                        } else {
                            // Content changed — update cache, keep full content
                            self.file_cache.insert(path.clone(), (hash, len));
                        }
                    }
                }

                // Also check cross-request cache for files read only once in this request
                // but previously seen in earlier requests
                for (i, msg) in request.messages.iter().enumerate() {
                    if indices_to_replace.contains(&i) {
                        continue; // Already handled
                    }
                    if msg.role == "tool" {
                        if let Some(ref call_id) = msg.tool_call_id {
                            if let Some(path) = call_id_to_path.get(call_id) {
                                let content_text = msg.content.as_ref()
                                    .map(|c| c.as_text())
                                    .unwrap_or_default();
                                if content_text.len() > 200 {
                                    let hash = simple_hash(&content_text);

                                    // Check: does this path appear in path_occurrences
                                    // with an earlier occurrence in THIS request?
                                    let dominated = path_occurrences.get(path)
                                        .map(|occ| occ.iter().any(|&(idx, h, _)| idx < i && h == hash))
                                        .unwrap_or(false);

                                    if dominated {
                                        // Already handled above
                                        continue;
                                    }

                                    // Cross-request: check if same content was seen
                                    // in a PREVIOUS request. But we can only safely
                                    // replace if there's still a copy in this messages
                                    // array (the earlier read from this request).
                                    // If no earlier copy exists in this request, do NOT
                                    // replace — the old request's messages may have been
                                    // truncated.
                                    //
                                    // Just update the cache for next time.
                                    self.file_cache.insert(path.clone(), (hash, content_text.len()));
                                }
                            }
                        }
                    }
                }

                // Apply replacements
                if !indices_to_replace.is_empty() {
                    let mut new_messages = Vec::with_capacity(request.messages.len());
                    for (i, msg) in request.messages.iter().enumerate() {
                        if let Some((path, len)) = replacement_info.get(&i) {
                            let mut cached_msg = msg.clone();
                            cached_msg.content = Some(crate::models::api::MessageContent::Text(
                                format!(
                                    "[file '{}' was already read earlier in this conversation with identical content ({} chars). Refer to the earlier read for full content.]",
                                    path, len
                                )
                            ));
                            let original_len = msg.content.as_ref()
                                .map(|c| c.as_text().len())
                                .unwrap_or(0);
                            let saved_chars = original_len.saturating_sub(120);
                            estimated_tokens_saved += (saved_chars as i64) / 4;
                            self.file_cache_hits += 1;
                            new_messages.push(cached_msg);
                        } else {
                            new_messages.push(msg.clone());
                        }
                    }
                    request.messages = new_messages;
                }
            }
        }

        self.tokens_saved += estimated_tokens_saved;

        TransformResult {
            request,
            tools_pruned,
            messages_merged,
            estimated_tokens_saved,
        }
    }

    /// Get total tokens saved across all transformations
    pub fn total_tokens_saved(&self) -> i64 {
        self.tokens_saved
    }

    /// Get number of requests processed
    pub fn request_count(&self) -> usize {
        self.request_count
    }

    /// Get tool usage counts for persistence (tool_name -> call_count)
    pub fn tool_usage_snapshot(&self) -> Vec<(String, usize)> {
        self.tool_call_counts.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// Transform a raw JSON request (for Anthropic-native format).
    /// Only modifies the `messages` array — leaves `tools` and all other fields untouched.
    /// This avoids the serialize/deserialize round-trip that breaks non-OpenAI tool schemas.
    pub fn transform_raw(&mut self, body: &mut serde_json::Value) -> RawTransformResult {
        self.request_count += 1;
        let mut estimated_tokens_saved: i64 = 0;
        let mut messages_optimized = 0usize;
        let mut tools_pruned = 0usize;

        // === Optimization 0: Prune unused tools (raw JSON, cache-aware) ===
        // Same cache protection as typed transform:
        // - Freeze prune decisions for cache stability
        // - Suspend pruning when cache hit rate > 30%
        if self.pruning_suspended {
            // Cache is working well — don't touch tools
            if let Some(tools_arr) = body.get("tools").and_then(|v| v.as_array()) {
                for t in tools_arr {
                    if let Some(name) = t.get("name").and_then(|v| v.as_str())
                        .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()))
                    {
                        self.tools_seen.insert(name.to_string());
                    }
                }
            }
        } else if self.request_count >= 3 && !self.tools_used.is_empty() {
            if let Some(tools_arr) = body.get_mut("tools").and_then(|v| v.as_array_mut()) {
                let original_count = tools_arr.len();

                let current_names: HashSet<String> = tools_arr.iter()
                    .filter_map(|t| {
                        t.get("name").and_then(|v| v.as_str())
                            .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()))
                            .map(String::from)
                    })
                    .collect();

                let new_tools: HashSet<String> = current_names.difference(&self.tools_seen).cloned().collect();
                let has_new_tools = !new_tools.is_empty();

                for name in &current_names {
                    self.tools_seen.insert(name.clone());
                }

                // Use frozen set if available and no new tools
                if let Some(ref frozen) = self.frozen_tools {
                    if !has_new_tools {
                        tools_arr.retain(|t| {
                            let name = t.get("name").and_then(|v| v.as_str())
                                .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()));
                            match name {
                                Some(n) => frozen.contains(n),
                                None => true,
                            }
                        });
                        tools_pruned = original_count - tools_arr.len();
                        estimated_tokens_saved += tools_pruned as i64 * 200;
                    } else {
                        self.frozen_tools = None;
                    }
                }

                if self.frozen_tools.is_none() {
                    tools_arr.retain(|t| {
                        let name = t.get("name").and_then(|v| v.as_str())
                            .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()));
                        match name {
                            Some(n) => self.tools_used.contains(n) || new_tools.contains(n),
                            None => true,
                        }
                    });

                    let kept: HashSet<String> = tools_arr.iter()
                        .filter_map(|t| {
                            t.get("name").and_then(|v| v.as_str())
                                .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()))
                                .map(String::from)
                        })
                        .collect();
                    self.frozen_tools = Some(kept);

                    tools_pruned = original_count - tools_arr.len();
                    estimated_tokens_saved += tools_pruned as i64 * 200;
                }
            }
        } else {
            if let Some(tools_arr) = body.get("tools").and_then(|v| v.as_array()) {
                for t in tools_arr {
                    if let Some(name) = t.get("name").and_then(|v| v.as_str())
                        .or_else(|| t.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()))
                    {
                        self.tools_seen.insert(name.to_string());
                    }
                }
            }
        }

        let messages = match body.get_mut("messages").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return RawTransformResult { estimated_tokens_saved, messages_optimized: 0, tools_pruned },
        };

        // === Optimization A: Deduplicate consecutive identical content blocks ===
        // In Anthropic format, tool results are in messages with role "tool" or
        // content blocks of type "tool_result" inside "user" messages.
        if messages.len() > 1 {
            let mut prev_content_hash: Option<(u64, usize)> = None;

            for msg in messages.iter_mut() {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("").to_string();

                // Look for content blocks in user messages (Anthropic puts tool_results here)
                if role == "user" {
                    if let Some(content_arr) = msg.get_mut("content").and_then(|v| v.as_array_mut()) {
                        for block in content_arr.iter_mut() {
                            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if block_type == "tool_result" {
                                // Get the text content of this tool_result
                                let text = Self::extract_tool_result_text(block);
                                if text.len() > 500 {
                                    let hash = simple_hash(&text);
                                    if let Some((prev_hash, prev_len)) = prev_content_hash {
                                        if hash == prev_hash && text.len() == prev_len {
                                            // Replace with short summary
                                            Self::replace_tool_result_content(
                                                block,
                                                "[same content as previous tool result]",
                                            );
                                            let saved = text.len().saturating_sub(40);
                                            estimated_tokens_saved += (saved as i64) / 4;
                                            messages_optimized += 1;
                                        }
                                    }
                                    prev_content_hash = Some((hash, text.len()));
                                }
                            }
                        }
                    }
                } else if role != "tool" {
                    prev_content_hash = None;
                }

                // Also handle OpenAI-style tool messages
                if role == "tool" {
                    let text = msg.get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if text.len() > 500 {
                        let hash = simple_hash(&text);
                        if let Some((prev_hash, prev_len)) = prev_content_hash {
                            if hash == prev_hash && text.len() == prev_len {
                                msg["content"] = serde_json::json!("[same content as previous tool result]");
                                let saved = text.len().saturating_sub(40);
                                estimated_tokens_saved += (saved as i64) / 4;
                                messages_optimized += 1;
                            }
                        }
                        prev_content_hash = Some((hash, text.len()));
                    }
                }
            }
        }

        // === Optimization B: File read dedup within the same request ===
        // Scan for tool_use (read file) calls and their corresponding tool_result blocks.
        // If the same file is read multiple times with identical content, replace duplicates.
        {
            // Step 1: Find all read-file tool_use blocks and map tool_use_id → file_path
            let mut call_id_to_path: HashMap<String, String> = HashMap::new();
            for msg in messages.iter() {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role == "assistant" {
                    // Anthropic format: content is array of blocks
                    if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                        for block in blocks {
                            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                if is_file_read_tool(name) {
                                    if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                                        if let Some(input) = block.get("input") {
                                            let path = input.get("file_path")
                                                .or(input.get("filePath"))
                                                .or(input.get("path"))
                                                .and_then(|v| v.as_str());
                                            if let Some(p) = path {
                                                call_id_to_path.insert(id.to_string(), p.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Also handle OpenAI format tool_calls
                    if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            if let Some(func) = tc.get("function") {
                                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                if is_file_read_tool(name) {
                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        let args = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                                        if let Some(p) = extract_file_path(args) {
                                            call_id_to_path.insert(id.to_string(), p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if !call_id_to_path.is_empty() {
                // Step 2: Collect tool_result content by file path
                // path → Vec<(msg_idx, block_idx_or_none, hash, len)>
                let mut path_occurrences: HashMap<String, Vec<FileOccurrence>> = HashMap::new();

                for (mi, msg) in messages.iter().enumerate() {
                    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if role == "user" {
                        if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                            for (bi, block) in blocks.iter().enumerate() {
                                if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                                    let tool_use_id = block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                                    if let Some(path) = call_id_to_path.get(tool_use_id) {
                                        let text = Self::extract_tool_result_text(block);
                                        if text.len() > 200 {
                                            let hash = simple_hash(&text);
                                            path_occurrences.entry(path.clone())
                                                .or_default()
                                                .push((mi, Some(bi), hash, text.len()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Step 3: Mark duplicates for replacement
                let mut replacements: Vec<(usize, usize, String, usize)> = Vec::new();
                for (path, occs) in &path_occurrences {
                    if occs.len() < 2 { continue; }
                    let first_hash = occs[0].2;
                    for &(mi, bi_opt, hash, len) in &occs[1..] {
                        if hash == first_hash {
                            if let Some(bi) = bi_opt {
                                replacements.push((mi, bi, path.clone(), len));
                            }
                        }
                    }
                }

                // Step 4: Apply replacements
                for (mi, bi, path, len) in &replacements {
                    if let Some(blocks) = messages[*mi].get_mut("content").and_then(|v| v.as_array_mut()) {
                        if let Some(block) = blocks.get_mut(*bi) {
                            Self::replace_tool_result_content(
                                block,
                                &format!(
                                    "[file '{}' was already read earlier in this conversation with identical content ({} chars). Refer to the earlier read for full content.]",
                                    path, len
                                ),
                            );
                            let saved = len.saturating_sub(120);
                            estimated_tokens_saved += (saved as i64) / 4;
                            messages_optimized += 1;
                            self.file_cache_hits += 1;
                        }
                    }
                }
            }
        }

        self.tokens_saved += estimated_tokens_saved;
        RawTransformResult { estimated_tokens_saved, messages_optimized, tools_pruned }
    }

    /// Extract text content from an Anthropic tool_result block
    fn extract_tool_result_text(block: &serde_json::Value) -> String {
        // Content can be a string or array of content blocks
        match block.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => {
                arr.iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                            b.get("text").and_then(|v| v.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            }
            _ => String::new(),
        }
    }

    /// Replace the content of an Anthropic tool_result block with a summary
    fn replace_tool_result_content(block: &mut serde_json::Value, replacement: &str) {
        // Set content to a simple string
        block["content"] = serde_json::json!(replacement);
    }

    /// Invalidate the file cache entry for a path (call after WriteFile/StrReplaceFile)
    pub fn invalidate_file(&mut self, path: &str) {
        self.file_cache.remove(path);
    }

    /// Load historical tool frequency to bootstrap pruning from session start
    pub fn load_history(&mut self, freq: &[(String, i64)], total_sessions: i64) {
        if total_sessions < 3 {
            return; // Not enough data
        }
        // Tools used in >20% of sessions are considered "high frequency"
        // Pre-populate tools_used so pruning can start from request #1
        for (name, session_count) in freq {
            let ratio = *session_count as f64 / total_sessions as f64;
            if ratio >= 0.2 {
                self.tools_used.insert(name.clone());
            }
            // All history tools are "seen" — prevents them from being treated
            // as "new tools" on request #1, which would bypass pruning entirely.
            self.tools_seen.insert(name.clone());
        }
        // If we loaded history, allow pruning from request #1 instead of #3
        if !self.tools_used.is_empty() {
            self.request_count = 2; // Next transform will be #3, enabling pruning
        }
    }
}

/// Check if a tool name is a file-reading tool
fn is_file_read_tool(name: &str) -> bool {
    matches!(
        name,
        "ReadFile" | "readFile" | "read_file" | "Read" | "cat" | "view_file"
    )
}

/// Check if a tool name is a file-writing tool
pub fn is_file_write_tool(name: &str) -> bool {
    matches!(
        name,
        "WriteFile" | "writeFile" | "write_file" | "Write"
            | "StrReplaceFile" | "str_replace_file" | "Edit"
            | "CreateFile" | "create_file"
    )
}

/// Extract file path from tool call arguments JSON
fn extract_file_path(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    // Try common field names
    for key in &["filePath", "file_path", "path", "filename", "file"] {
        if let Some(s) = v.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Simple non-crypto hash for content comparison (FNV-1a)
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Thread-safe wrapper
pub type SharedTransformer = Arc<Mutex<RequestTransformer>>;

pub fn new_shared_transformer() -> SharedTransformer {
    Arc::new(Mutex::new(RequestTransformer::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::api::*;

    fn make_tool(name: &str) -> Tool {
        Tool {
            tool_type: Some("function".into()),
            function: Some(FunctionDef {
                name: name.into(),
                description: Some(format!("Does {}", name)),
                parameters: None,
            }),
            extra: serde_json::Map::new(),
        }
    }

    fn make_msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn make_tool_msg(content: &str) -> Message {
        Message {
            role: "tool".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: Some("call_123".into()),
            name: None,
        }
    }

    fn make_request(tools: Vec<Tool>, messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: Some("gpt-4".into()),
            messages,
            tools,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn test_no_pruning_before_call_3() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![make_tool("read"), make_tool("write"), make_tool("delete")],
            vec![make_msg("user", "hello")],
        );

        // Call 1: no pruning
        let r1 = tx.transform(req.clone());
        assert_eq!(r1.request.tools.len(), 3);
        assert_eq!(r1.tools_pruned, 0);

        // Call 2: still no pruning
        let r2 = tx.transform(req.clone());
        assert_eq!(r2.request.tools.len(), 3);
        assert_eq!(r2.tools_pruned, 0);
    }

    #[test]
    fn test_prune_unused_tools_after_call_3() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![make_tool("read"), make_tool("write"), make_tool("delete"), make_tool("list")],
            vec![make_msg("user", "hello")],
        );

        // Calls 1 and 2: observe
        tx.transform(req.clone());
        tx.transform(req.clone());

        // Record that only "read" and "write" were used
        tx.record_tool_usage(&["read".into(), "write".into()]);

        // Call 3: should prune "delete" and "list"
        let r3 = tx.transform(req.clone());
        assert_eq!(r3.tools_pruned, 2);
        assert_eq!(r3.request.tools.len(), 2);

        let remaining: Vec<String> = r3.request.tools.iter()
            .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
            .collect();
        assert!(remaining.contains(&"read".into()));
        assert!(remaining.contains(&"write".into()));
        assert!(!remaining.contains(&"delete".into()));
        assert!(!remaining.contains(&"list".into()));

        // Estimated savings: 2 tools * 200 tokens = 400
        assert_eq!(r3.estimated_tokens_saved, 400);
    }

    #[test]
    fn test_merge_system_messages() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![],
            vec![
                make_msg("system", "You are helpful."),
                make_msg("system", "Always be concise."),
                make_msg("user", "Hi"),
            ],
        );

        let result = tx.transform(req);
        // Should merge 2 system messages into 1
        assert_eq!(result.messages_merged, 1);

        // Result should have 2 messages: 1 system + 1 user
        assert_eq!(result.request.messages.len(), 2);
        assert_eq!(result.request.messages[0].role, "system");
        assert_eq!(result.request.messages[1].role, "user");

        // Merged content
        let sys_content = result.request.messages[0].content.as_ref().unwrap().as_text();
        assert!(sys_content.contains("You are helpful."));
        assert!(sys_content.contains("Always be concise."));
    }

    #[test]
    fn test_no_merge_single_system() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![],
            vec![
                make_msg("system", "You are helpful."),
                make_msg("user", "Hi"),
            ],
        );

        let result = tx.transform(req);
        assert_eq!(result.messages_merged, 0);
        assert_eq!(result.request.messages.len(), 2);
    }

    #[test]
    fn test_dedup_tool_results() {
        let mut tx = RequestTransformer::new();

        // Create a long repeated content (>500 chars)
        let long_content = "x".repeat(1000);

        let req = make_request(
            vec![],
            vec![
                make_msg("user", "read two files"),
                make_tool_msg(&long_content),
                make_tool_msg(&long_content), // duplicate
            ],
        );

        let result = tx.transform(req);
        // The second tool message should be replaced with short text
        let last_msg = &result.request.messages[2];
        let last_content = last_msg.content.as_ref().unwrap().as_text();
        assert_eq!(last_content, "[same content as previous tool result]");
        assert!(result.estimated_tokens_saved > 0);
    }

    #[test]
    fn test_no_dedup_short_tool_results() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![],
            vec![
                make_msg("user", "query"),
                make_tool_msg("short result"),
                make_tool_msg("short result"), // same but short
            ],
        );

        let result = tx.transform(req);
        // Should NOT dedup short results (< 500 chars)
        let last_content = result.request.messages[2].content.as_ref().unwrap().as_text();
        assert_eq!(last_content, "short result");
    }

    #[test]
    fn test_no_dedup_different_results() {
        let mut tx = RequestTransformer::new();

        let long_a = "a".repeat(1000);
        let long_b = "b".repeat(1000);

        let req = make_request(
            vec![],
            vec![
                make_msg("user", "read"),
                make_tool_msg(&long_a),
                make_tool_msg(&long_b), // different
            ],
        );

        let result = tx.transform(req);
        let last_content = result.request.messages[2].content.as_ref().unwrap().as_text();
        assert_eq!(last_content, long_b);
    }

    #[test]
    fn test_file_read_cache_hit() {
        let mut tx = RequestTransformer::new();

        let file_content = "x".repeat(500); // >200 chars

        // Single request that reads the same file twice (common in real sessions
        // where full conversation history includes both reads)
        let req = make_request(
            vec![],
            vec![
                make_msg("user", "read the file"),
                // First read
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("call_1".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(file_content.clone())),
                    tool_calls: None,
                    tool_call_id: Some("call_1".into()),
                    name: None,
                },
                make_msg("assistant", "I see the file. Let me read it again."),
                // Second read of same file
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("call_2".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(file_content.clone())),
                    tool_calls: None,
                    tool_call_id: Some("call_2".into()),
                    name: None,
                },
            ],
        );

        let result = tx.transform(req);

        // First read should be preserved (full content)
        let first_tool = &result.request.messages[2];
        assert_eq!(first_tool.content.as_ref().unwrap().as_text(), file_content);

        // Second read should be replaced (safe because first copy still exists)
        let second_tool = &result.request.messages[5];
        let content2 = second_tool.content.as_ref().unwrap().as_text();
        assert!(content2.contains("already read"), "Expected cache hit, got: {}", content2);
        assert!(content2.contains("/foo/bar.ts"));
        assert_eq!(tx.file_cache_hits, 1);
        assert!(result.estimated_tokens_saved > 0);
    }

    #[test]
    fn test_file_cache_cross_request_no_replace() {
        let mut tx = RequestTransformer::new();
        let file_content = "x".repeat(500);

        // First request: read file
        let req1 = make_request(
            vec![],
            vec![
                make_msg("user", "read"),
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("c1".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(file_content.clone())),
                    tool_calls: None,
                    tool_call_id: Some("c1".into()),
                    name: None,
                },
            ],
        );
        tx.transform(req1);

        // Second request: same file, but old messages might be truncated
        // Only has ONE copy in messages — should NOT replace
        let req2 = make_request(
            vec![],
            vec![
                make_msg("user", "read again"),
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("c2".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(file_content.clone())),
                    tool_calls: None,
                    tool_call_id: Some("c2".into()),
                    name: None,
                },
            ],
        );

        let r2 = tx.transform(req2);
        // Should NOT be replaced — only one copy in this request
        let tool_msg = &r2.request.messages[2];
        assert_eq!(tool_msg.content.as_ref().unwrap().as_text(), file_content);
        assert_eq!(tx.file_cache_hits, 0);
    }

    #[test]
    fn test_file_cache_invalidated_on_change() {
        let mut tx = RequestTransformer::new();

        let content_v1 = "a".repeat(500);
        let content_v2 = "b".repeat(500);

        // First read
        let req1 = make_request(
            vec![],
            vec![
                make_msg("user", "read"),
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("c1".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(content_v1)),
                    tool_calls: None,
                    tool_call_id: Some("c1".into()),
                    name: None,
                },
            ],
        );
        tx.transform(req1);

        // Second read: file content changed
        let req2 = make_request(
            vec![],
            vec![
                make_msg("user", "read again"),
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: Some("c2".into()),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: "ReadFile".into(),
                            arguments: r#"{"filePath":"/foo/bar.ts"}"#.into(),
                        }),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(content_v2.clone())),
                    tool_calls: None,
                    tool_call_id: Some("c2".into()),
                    name: None,
                },
            ],
        );

        let r2 = tx.transform(req2);
        // Content changed, so should NOT be cached
        let tool_msg = &r2.request.messages[2];
        assert_eq!(tool_msg.content.as_ref().unwrap().as_text(), content_v2);
        assert_eq!(tx.file_cache_hits, 0);
    }

    #[test]
    fn test_cumulative_tokens_saved() {
        let mut tx = RequestTransformer::new();

        let req = make_request(
            vec![make_tool("a"), make_tool("b"), make_tool("c")],
            vec![
                make_msg("system", "sys1"),
                make_msg("system", "sys2"),
                make_msg("user", "hi"),
            ],
        );

        tx.transform(req.clone()); // call 1
        tx.transform(req.clone()); // call 2
        tx.record_tool_usage(&["a".into()]);
        tx.transform(req.clone()); // call 3: prune + merge

        assert!(tx.total_tokens_saved() > 0);
        assert_eq!(tx.request_count(), 3);
    }

    #[test]
    fn test_history_pruning_actually_prunes() {
        // Regression: load_history used to leave tools_seen empty,
        // causing ALL tools to be treated as "new" on request #1,
        // which bypassed pruning entirely.
        let mut tx = RequestTransformer::new();

        // Simulate history: "read" and "write" were used in >20% of sessions
        let history = vec![
            ("read".to_string(), 8),
            ("write".to_string(), 6),
            ("delete".to_string(), 1),  // low frequency, should be pruned
            ("list".to_string(), 1),    // low frequency, should be pruned
        ];
        tx.load_history(&history, 10);

        // tools_used should have "read" and "write" (>20%)
        assert!(tx.tools_used.contains("read"));
        assert!(tx.tools_used.contains("write"));
        assert!(!tx.tools_used.contains("delete"));

        // tools_seen should have ALL history tools (prevents "new tool" bypass)
        assert!(tx.tools_seen.contains("read"));
        assert!(tx.tools_seen.contains("delete"));
        assert!(tx.tools_seen.contains("list"));

        // First request (request_count goes to 3 → pruning enabled)
        let req = make_request(
            vec![make_tool("read"), make_tool("write"), make_tool("delete"), make_tool("list")],
            vec![make_msg("user", "hello")],
        );
        let result = tx.transform(req);

        // Should prune "delete" and "list" (not in tools_used, not "new")
        assert_eq!(result.tools_pruned, 2, "History-based pruning should remove 2 tools");
        assert_eq!(result.request.tools.len(), 2);
        let names: Vec<String> = result.request.tools.iter()
            .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
            .collect();
        assert!(names.contains(&"read".into()));
        assert!(names.contains(&"write".into()));
    }

    #[test]
    fn test_history_pruning_preserves_truly_new_tools() {
        // When a tool appears that wasn't in history at all, it should be kept.
        // Tools that ARE in history but below the 20% threshold should be pruned.
        let mut tx = RequestTransformer::new();
        tx.load_history(&[
            ("read".to_string(), 8),
            ("write".to_string(), 6),
            ("delete".to_string(), 1),  // in history, low freq → tools_seen but NOT tools_used
        ], 10);

        // Request includes "brand_new_tool" which is NOT in history at all
        let req = make_request(
            vec![
                make_tool("read"), make_tool("write"),
                make_tool("delete"),        // in history but low freq
                make_tool("brand_new_tool"), // never seen in history
            ],
            vec![make_msg("user", "hello")],
        );
        let result = tx.transform(req);

        // "brand_new_tool" is truly new (not in tools_seen) → should be kept
        // "delete" is in history (in tools_seen) but low freq (not in tools_used) → should be pruned
        let names: Vec<String> = result.request.tools.iter()
            .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
            .collect();
        assert!(names.contains(&"read".into()));
        assert!(names.contains(&"write".into()));
        assert!(names.contains(&"brand_new_tool".into()), "Truly new tools should be preserved");
        assert!(!names.contains(&"delete".into()), "Low-freq history tools should be pruned");
    }

    #[test]
    fn test_cache_aware_pruning_suspension() {
        let mut tx = RequestTransformer::new();
        let tools = vec![make_tool("read"), make_tool("write"), make_tool("unused")];
        let req = make_request(tools.clone(), vec![make_msg("user", "hi")]);

        // Build up to request #3
        tx.transform(req.clone());
        tx.transform(req.clone());
        tx.record_tool_usage(&["read".into(), "write".into()]);

        // Simulate warm cache: 40% hit rate (> 30% threshold)
        tx.record_cache_stats(4000, 10000);
        assert!(tx.is_pruning_suspended());

        // Request #3 with pruning suspended → no pruning
        let r3 = tx.transform(req.clone());
        assert_eq!(r3.tools_pruned, 0, "Pruning should be suspended when cache is warm");
        assert_eq!(r3.request.tools.len(), 3);

        // Cache cools down: hit rate drops to 5% (< 10% resume threshold)
        // Reset stats to simulate cooled cache
        tx.cache_stats = (0, 0);
        tx.record_cache_stats(500, 10000);
        assert!(!tx.is_pruning_suspended(), "Pruning should resume when cache is cold");

        // Next request should prune
        let r4 = tx.transform(req.clone());
        assert_eq!(r4.tools_pruned, 1, "Pruning should resume after cache cools");
    }

    #[test]
    fn test_frozen_tools_stability() {
        // Once frozen, same prune decision should be reused
        let mut tx = RequestTransformer::new();
        let tools = vec![
            make_tool("read"), make_tool("write"),
            make_tool("delete"), make_tool("list"),
        ];
        let req = make_request(tools.clone(), vec![make_msg("user", "hi")]);

        tx.transform(req.clone()); // #1
        tx.transform(req.clone()); // #2
        tx.record_tool_usage(&["read".into(), "write".into()]);

        let r3 = tx.transform(req.clone()); // #3: first prune, freezes
        assert_eq!(r3.tools_pruned, 2);

        let r4 = tx.transform(req.clone()); // #4: uses frozen set
        assert_eq!(r4.tools_pruned, 2);
        assert_eq!(r4.request.tools.len(), 2);

        // New tool appears → invalidate frozen set
        let mut tools_plus = tools.clone();
        tools_plus.push(make_tool("new_tool"));
        let req_new = make_request(tools_plus, vec![make_msg("user", "hi")]);
        let r5 = tx.transform(req_new);

        // new_tool kept (it's new), read/write kept (used), delete/list pruned
        let names: Vec<String> = r5.request.tools.iter()
            .filter_map(|t| t.function.as_ref().map(|f| f.name.clone()))
            .collect();
        assert!(names.contains(&"new_tool".into()));
        assert!(names.contains(&"read".into()));
        assert!(!names.contains(&"delete".into()));
    }

    #[test]
    fn test_raw_transform_tool_dedup() {
        // Regression: OpenAI-style "tool" role dedup in transform_raw
        // used to always clear prev_content_hash before checking it
        let mut tx = RequestTransformer::new();
        let long_content = "x".repeat(1000);
        let mut body = serde_json::json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "do stuff"},
                {"role": "tool", "content": long_content, "tool_call_id": "c1"},
                {"role": "tool", "content": long_content, "tool_call_id": "c2"},
            ]
        });

        let result = tx.transform_raw(&mut body);
        let msgs = body["messages"].as_array().unwrap();
        let second_tool = msgs[2]["content"].as_str().unwrap();
        assert_eq!(
            second_tool, "[same content as previous tool result]",
            "Consecutive identical tool messages should be deduped in raw transform"
        );
        assert!(result.estimated_tokens_saved > 0);
    }
}
