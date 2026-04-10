use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::models::api::{
    ChatRequest, ChatResponse, Choice, FunctionCall, FunctionDef, Message, MessageContent, Tool,
    ToolCall, Usage,
};
use crate::models::trace::{Provider, TraceEntry, TraceSession};
use super::transformer::{is_file_write_tool, SharedTransformer};

pub struct ProxyConfig {
    pub listen_port: u16,
    pub target_url: String,
    pub api_key: Option<String>,
    /// When set, append each trace entry to this file as it arrives
    pub live_trace_file: Option<PathBuf>,
    /// Enable real-time request optimization
    pub optimize: bool,
}

pub async fn run_proxy(
    config: ProxyConfig,
    session: Arc<Mutex<TraceSession>>,
    transformer: Option<SharedTransformer>,
) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", config.listen_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("merlint proxy listening on {}", addr);
    info!("forwarding to {}", config.target_url);
    if config.optimize {
        info!("real-time optimization ENABLED");
    }

    let config = Arc::new(config);
    let client = Arc::new(reqwest::Client::new());

    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let config = config.clone();
        let session = session.clone();
        let transformer = transformer.clone();
        let client = client.clone();

        tokio::spawn(async move {
            let config = config.clone();
            let session = session.clone();
            let transformer = transformer.clone();
            let client = client.clone();
            let service = service_fn(move |req| {
                handle_request(req, config.clone(), session.clone(), transformer.clone(), client.clone())
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                if !e.to_string().contains("connection closed") {
                    error!("connection error from {}: {}", peer, e);
                }
            }
        });
    }
}

async fn handle_request(
    req: Request<Incoming>,
    config: Arc<ProxyConfig>,
    session: Arc<Mutex<TraceSession>>,
    transformer: Option<SharedTransformer>,
    client: Arc<reqwest::Client>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    let body_bytes = req.collect().await?.to_bytes();

    let provider = detect_provider(&path, &headers);
    let is_chat = path.contains("/chat/completions") || path.contains("/messages")
        || path.contains("/completions");

    let target_url = format!("{}{}", config.target_url.trim_end_matches('/'), path);

    // Optionally transform the request body
    let is_anthropic_native = provider == Provider::Anthropic;
    let (final_body, transform_stats) = if is_chat && config.optimize {
        if let Some(ref tx) = transformer {
            if is_anthropic_native {
                // Anthropic-native format: use raw JSON transformation
                // This only modifies messages, leaving tools and other fields intact
                if let Ok(mut raw_body) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    let mut t = tx.lock().await;
                    let result = t.transform_raw(&mut raw_body);
                    let stats = if result.estimated_tokens_saved > 0 {
                        Some((result.tools_pruned, result.messages_optimized, result.estimated_tokens_saved))
                    } else {
                        None
                    };
                    match serde_json::to_vec(&raw_body) {
                        Ok(new_body) => (Bytes::from(new_body), stats),
                        Err(_) => (body_bytes.clone(), None),
                    }
                } else {
                    (body_bytes.clone(), None)
                }
            } else {
                // OpenAI-compatible format: use typed transformation
                if let Ok(chat_req) = serde_json::from_slice::<ChatRequest>(&body_bytes) {
                    let mut t = tx.lock().await;
                    let result = t.transform(chat_req);

                    let stats = if result.estimated_tokens_saved > 0 {
                        Some((result.tools_pruned, result.messages_merged, result.estimated_tokens_saved))
                    } else {
                        None
                    };

                    match serde_json::to_vec(&result.request) {
                        Ok(new_body) => (Bytes::from(new_body), stats),
                        Err(_) => (body_bytes.clone(), None),
                    }
                } else {
                    (body_bytes.clone(), None)
                }
            }
        } else {
            (body_bytes.clone(), None)
        }
    } else {
        (body_bytes.clone(), None)
    };

    let mut forward_req = client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST),
        &target_url,
    );

    for (key, value) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str == "host" || key_str == "content-length" || key_str == "transfer-encoding"
            || key_str == "accept-encoding"
        {
            continue;
        }
        if let Ok(v) = value.to_str() {
            forward_req = forward_req.header(key.as_str(), v);
        }
    }

    if let Some(ref api_key) = config.api_key {
        forward_req = forward_req.header("Authorization", format!("Bearer {}", api_key));
    }

    forward_req = forward_req.body(final_body.to_vec());

    let start = Instant::now();
    let response = match forward_req.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("upstream error: {}", e);
            let err_body = serde_json::json!({
                "error": { "message": format!("merlint proxy error: {}", e) }
            });
            return Ok(Response::builder()
                .status(502)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(serde_json::to_vec(&err_body).unwrap())))
                .unwrap());
        }
    };

    let status = response.status();
    let resp_headers = response.headers().clone();
    let is_sse = resp_headers.get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    // Buffer the full response for trace recording.
    // For SSE streams, this collects all chunks then forwards the complete buffer.
    // The parse_response() function already handles merging SSE chunks for traces.
    let resp_bytes = response.bytes().await.unwrap_or_default();
    let latency_ms = start.elapsed().as_millis() as u64;

    // Record trace if this is a chat completion
    if is_chat && status.is_success() {
        // Record tool usage from response (works for both formats)
        if let Some(ref tx) = transformer {
            if is_anthropic_native {
                // Anthropic response: tool_use blocks in content array
                if let Ok(resp_val) = serde_json::from_slice::<serde_json::Value>(&resp_bytes) {
                    let mut used_tools = Vec::new();
                    let mut write_paths = Vec::new();
                    if let Some(content) = resp_val.get("content").and_then(|v| v.as_array()) {
                        for block in content {
                            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                                    used_tools.push(name.to_string());
                                    if is_file_write_tool(name) {
                                        if let Some(p) = extract_write_path_from_value(block.get("input")) {
                                            write_paths.push(p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !used_tools.is_empty() {
                        let mut t = tx.lock().await;
                        t.record_tool_usage(&used_tools);
                        for p in &write_paths {
                            t.invalidate_file(p);
                        }
                    }
                }
            } else {
                // OpenAI response: tool_calls in choices[].message
                if let Ok(chat_resp) = parse_response(&resp_bytes) {
                    let mut used_tools = Vec::new();
                    let mut write_paths = Vec::new();
                    for choice in &chat_resp.choices {
                        if let Some(ref msg) = choice.message {
                            if let Some(ref calls) = msg.tool_calls {
                                for call in calls {
                                    if let Some(ref f) = call.function {
                                        used_tools.push(f.name.clone());
                                        if is_file_write_tool(&f.name) {
                                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&f.arguments) {
                                                if let Some(p) = extract_write_path_from_value(Some(&v)) {
                                                    write_paths.push(p);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !used_tools.is_empty() {
                        let mut t = tx.lock().await;
                        t.record_tool_usage(&used_tools);
                        for p in &write_paths {
                            t.invalidate_file(p);
                        }
                    }
                }
            }
        }

        // Build trace entry — convert Anthropic format to internal types if needed
        let (chat_req_opt, chat_resp_opt) = if is_anthropic_native {
            let req = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                .ok()
                .and_then(|v| anthropic_request_to_chat_request(&v));
            let resp = serde_json::from_slice::<serde_json::Value>(&resp_bytes)
                .ok()
                .and_then(|v| anthropic_response_to_chat_response(&v));
            (req, resp)
        } else {
            (
                serde_json::from_slice::<ChatRequest>(&body_bytes).ok(),
                parse_response(&resp_bytes).ok(),
            )
        };

        if let (Some(chat_req), Some(chat_resp)) = (chat_req_opt, chat_resp_opt) {
            let entry = TraceEntry::new(provider, chat_req, chat_resp, latency_ms);
            let entry_id = entry.id.clone();
            let tokens = entry.total_tokens().unwrap_or(0);

            // Live write to file if configured
            if let Some(ref path) = config.live_trace_file {
                let mut sess = session.lock().await;
                sess.add_entry(entry);
                if let Ok(json) = serde_json::to_string(&*sess) {
                    let _ = std::fs::write(path, &json);
                }
            } else {
                session.lock().await.add_entry(entry);
            }

            // Log with optimization info
            if let Some((pruned, merged, saved)) = transform_stats {
                info!(
                    "[trace {}] {} tokens, {}ms | optimized: -{} tools, -{} msgs, ~{} tokens saved",
                    &entry_id[..8], tokens, latency_ms, pruned, merged, saved
                );
            } else {
                info!(
                    "[trace {}] {} tokens, {}ms",
                    &entry_id[..8], tokens, latency_ms
                );
            }
        } else {
            // Still log optimization stats even if trace entry couldn't be built
            if let Some((pruned, merged, saved)) = transform_stats {
                info!(
                    "[proxy] {}ms | optimized: -{} tools, -{} msgs, ~{} tokens saved",
                    latency_ms, pruned, merged, saved
                );
            }
        }
    }

    let mut resp_builder = Response::builder().status(status.as_u16());
    for (key, value) in resp_headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str == "transfer-encoding" || key_str == "content-length" {
            continue;
        }
        resp_builder = resp_builder.header(key.as_str(), value);
    }

    // Preserve SSE content-type so the client can parse SSE frames
    if is_sse {
        resp_builder = resp_builder.header("content-type", "text/event-stream");
    }

    Ok(resp_builder
        .body(Full::new(resp_bytes))
        .unwrap())
}

/// Parse response bytes as ChatResponse, handling both regular JSON and SSE streaming formats.
fn parse_response(bytes: &[u8]) -> Result<ChatResponse, String> {
    // Try direct JSON parse first
    if let Ok(resp) = serde_json::from_slice::<ChatResponse>(bytes) {
        return Ok(resp);
    }

    // Try parsing as a generic JSON value to build a ChatResponse
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return chatresponse_from_value(&val);
    }

    // Try SSE format: each line is "data: {...}" — collect and merge chunks
    let text = String::from_utf8_lossy(bytes);
    let mut chunks: Vec<serde_json::Value> = Vec::new();
    for line in text.lines() {
        let data = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"));
        if let Some(data) = data {
            let data = data.trim();
            if data == "[DONE]" || data.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                chunks.push(v);
            }
        }
    }

    if chunks.is_empty() {
        return Err("no parseable JSON found in response".into());
    }

    // Merge SSE chunks into a single ChatResponse
    merge_sse_chunks(&chunks)
}

fn chatresponse_from_value(val: &serde_json::Value) -> Result<ChatResponse, String> {
    // Build a ChatResponse from arbitrary JSON, being lenient about field names
    let id = val.get("id").and_then(|v| v.as_str()).map(String::from);
    let model = val.get("model").and_then(|v| v.as_str()).map(String::from);

    // Try to find choices
    let choices = if let Some(arr) = val.get("choices").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|c| serde_json::from_value::<Choice>(c.clone()).ok()).collect()
    } else {
        // Some APIs put the message at top level (no choices array)
        // Try to construct a synthetic choice from top-level content/role
        let mut synthetic = Vec::new();
        if let Some(content) = val.get("content").or(val.get("text")).or(val.get("result")) {
            let msg = Message {
                role: "assistant".into(),
                content: Some(MessageContent::Text(
                    content.as_str().unwrap_or("").to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            };
            synthetic.push(Choice {
                index: Some(0),
                message: Some(msg),
                finish_reason: val.get("finish_reason").and_then(|v| v.as_str()).map(String::from),
            });
        }
        synthetic
    };

    // Try to find usage
    let usage = val.get("usage").and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok());

    if choices.is_empty() && usage.is_none() {
        return Err("could not extract choices or usage from response JSON".to_string());
    }

    Ok(ChatResponse {
        id,
        model,
        choices,
        usage,
        extra: serde_json::Map::new(),
    })
}

fn merge_sse_chunks(chunks: &[serde_json::Value]) -> Result<ChatResponse, String> {
    if chunks.is_empty() {
        return Err("no SSE chunks".into());
    }

    // Use the last chunk for metadata (often contains usage)
    let last = &chunks[chunks.len() - 1];
    let first = &chunks[0];

    let id = first.get("id").and_then(|v| v.as_str()).map(String::from);
    let model = first.get("model").and_then(|v| v.as_str()).map(String::from);

    // Concatenate all delta content
    let mut full_content = String::new();
    let mut tool_calls_map: std::collections::BTreeMap<u32, (String, String, String)> = std::collections::BTreeMap::new();
    let mut finish_reason = None;

    for chunk in chunks {
        if let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    // Text content
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        full_content.push_str(content);
                    }
                    // Tool calls in delta
                    if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let entry = tool_calls_map.entry(idx).or_insert_with(|| {
                                let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let name = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                                (id, name, String::new())
                            });
                            if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
                                entry.2.push_str(args);
                            }
                        }
                    }
                }
                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }
        }
    }

    let tool_calls = if tool_calls_map.is_empty() {
        None
    } else {
        Some(tool_calls_map.into_values().map(|(id, name, args)| {
            ToolCall {
                id: Some(id),
                call_type: Some("function".into()),
                function: Some(FunctionCall { name, arguments: args }),
            }
        }).collect())
    };

    let msg = Message {
        role: "assistant".into(),
        content: if full_content.is_empty() { None } else { Some(MessageContent::Text(full_content)) },
        tool_calls,
        tool_call_id: None,
        name: None,
    };

    let usage = last.get("usage").and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok());

    Ok(ChatResponse {
        id,
        model,
        choices: vec![Choice {
            index: Some(0),
            message: Some(msg),
            finish_reason,
        }],
        usage,
        extra: serde_json::Map::new(),
    })
}

fn detect_provider(path: &str, headers: &hyper::HeaderMap) -> Provider {
    if path.contains("/messages") {
        return Provider::Anthropic;
    }
    if headers.contains_key("x-api-key") || headers.contains_key("anthropic-version") {
        return Provider::Anthropic;
    }
    if path.contains("/chat/completions") {
        return Provider::OpenAI;
    }
    Provider::Unknown
}

/// Extract file path from a tool input/arguments JSON value
fn extract_write_path_from_value(val: Option<&serde_json::Value>) -> Option<String> {
    let v = val?;
    for key in &["filePath", "file_path", "path", "filename", "file"] {
        if let Some(p) = v.get(key).and_then(|v| v.as_str()) {
            return Some(p.to_string());
        }
    }
    None
}

/// Convert Anthropic-native request JSON to internal ChatRequest for trace storage
fn anthropic_request_to_chat_request(val: &serde_json::Value) -> Option<ChatRequest> {
    let model = val.get("model").and_then(|v| v.as_str()).map(String::from);

    let mut messages = Vec::new();

    // System prompt (top-level field in Anthropic format)
    if let Some(sys) = val.get("system") {
        let sys_text = match sys {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => {
                arr.iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                            b.get("text").and_then(|v| v.as_str())
                        } else {
                            b.as_str()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        };
        if !sys_text.is_empty() {
            messages.push(Message {
                role: "system".into(),
                content: Some(MessageContent::Text(sys_text)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // Regular messages
    if let Some(msgs) = val.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string();

            let content_text = match msg.get("content") {
                Some(serde_json::Value::String(s)) => Some(MessageContent::Text(s.clone())),
                Some(serde_json::Value::Array(arr)) => {
                    let text: String = arr.iter()
                        .filter_map(|b| {
                            if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                                b.get("text").and_then(|v| v.as_str()).map(String::from)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if text.is_empty() { None } else { Some(MessageContent::Text(text)) }
                }
                _ => None,
            };

            // Extract tool_use blocks as tool_calls (from assistant messages)
            let tool_calls = if let Some(serde_json::Value::Array(arr)) = msg.get("content") {
                let calls: Vec<ToolCall> = arr.iter()
                    .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                    .filter_map(|b| {
                        let id = b.get("id").and_then(|v| v.as_str()).map(String::from);
                        let name = b.get("name").and_then(|v| v.as_str())?;
                        let args = b.get("input")
                            .map(|v| serde_json::to_string(v).unwrap_or_default())
                            .unwrap_or_default();
                        Some(ToolCall {
                            id,
                            call_type: Some("function".into()),
                            function: Some(FunctionCall { name: name.to_string(), arguments: args }),
                        })
                    })
                    .collect();
                if calls.is_empty() { None } else { Some(calls) }
            } else {
                None
            };

            // Extract tool_call_id from tool_result blocks
            let tool_call_id = if let Some(serde_json::Value::Array(arr)) = msg.get("content") {
                arr.iter()
                    .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_result"))
                    .and_then(|b| b.get("tool_use_id").and_then(|v| v.as_str()).map(String::from))
            } else {
                None
            };

            messages.push(Message {
                role,
                content: content_text,
                tool_calls,
                tool_call_id,
                name: None,
            });
        }
    }

    // Convert tools (Anthropic: top-level name/description/input_schema)
    let tools: Vec<Tool> = val.get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().filter_map(|t| {
                let name = t.get("name").and_then(|v| v.as_str())?;
                Some(Tool {
                    tool_type: Some("function".into()),
                    function: Some(FunctionDef {
                        name: name.to_string(),
                        description: t.get("description").and_then(|v| v.as_str()).map(String::from),
                        parameters: t.get("input_schema").cloned(),
                    }),
                    extra: serde_json::Map::new(),
                })
            }).collect()
        })
        .unwrap_or_default();

    Some(ChatRequest {
        model,
        messages,
        tools,
        extra: serde_json::Map::new(),
    })
}

/// Convert Anthropic-native response JSON to internal ChatResponse for trace storage
fn anthropic_response_to_chat_response(val: &serde_json::Value) -> Option<ChatResponse> {
    let id = val.get("id").and_then(|v| v.as_str()).map(String::from);
    let model = val.get("model").and_then(|v| v.as_str()).map(String::from);

    let mut text_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = val.get("content").and_then(|v| v.as_array()) {
        for block in content {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text_content.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let tc_id = block.get("id").and_then(|v| v.as_str()).map(String::from);
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args = block.get("input")
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .unwrap_or_default();
                    tool_calls.push(ToolCall {
                        id: tc_id,
                        call_type: Some("function".into()),
                        function: Some(FunctionCall { name, arguments: args }),
                    });
                }
                _ => {}
            }
        }
    }

    let msg = Message {
        role: "assistant".into(),
        content: if text_content.is_empty() { None } else { Some(MessageContent::Text(text_content)) },
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id: None,
        name: None,
    };

    // Map Anthropic usage fields to internal format
    let usage = val.get("usage").map(|u| {
        let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        Usage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: input + output,
            cache_creation_input_tokens: u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()),
            cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
        }
    });

    let finish_reason = val.get("stop_reason").and_then(|v| v.as_str()).map(String::from);

    Some(ChatResponse {
        id,
        model,
        choices: vec![Choice {
            index: Some(0),
            message: Some(msg),
            finish_reason,
        }],
        usage,
        extra: serde_json::Map::new(),
    })
}
