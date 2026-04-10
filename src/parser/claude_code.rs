use std::path::Path;

use serde::Deserialize;

use crate::models::api::*;
use crate::models::trace::*;

/// Parse a Claude Code session file (JSONL format)
/// Each line is a message event with type, role, content, etc.
pub fn parse_session(path: &Path) -> anyhow::Result<TraceSession> {
    let content = std::fs::read_to_string(path)?;
    let mut session = TraceSession::new();

    // Claude Code sessions can be JSONL (one JSON object per line)
    // or a single JSON array
    let events: Vec<CCEvent> = if content.trim_start().starts_with('[') {
        serde_json::from_str(&content)?
    } else {
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<CCEvent>(l).ok())
            .collect()
    };

    // Group events into request-response pairs
    // Look for assistant messages that contain usage data
    let mut current_messages: Vec<Message> = Vec::new();
    let mut current_tools: Vec<Tool> = Vec::new();

    for event in &events {
        match event.event_type.as_deref().or(event.role.as_deref()) {
            Some("system") => {
                let msg = Message {
                    role: "system".into(),
                    content: event.content_as_message_content(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                };
                current_messages.push(msg);
            }
            Some("human") | Some("user") => {
                let msg = Message {
                    role: "user".into(),
                    content: event.content_as_message_content(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                };
                current_messages.push(msg);
            }
            Some("assistant") => {
                // Check if this has usage data — means it's a complete response
                let msg = Message {
                    role: "assistant".into(),
                    content: event.content_as_message_content(),
                    tool_calls: event.extract_tool_calls(),
                    tool_call_id: None,
                    name: None,
                };
                current_messages.push(msg.clone());

                if let Some(ref usage) = event.usage {
                    // This is a response — create a trace entry
                    let request = ChatRequest {
                        model: event.model.clone(),
                        messages: current_messages.clone(),
                        tools: current_tools.clone(),
                        extra: Default::default(),
                    };

                    let response = ChatResponse {
                        id: event.id.clone(),
                        model: event.model.clone(),
                        choices: vec![Choice {
                            index: Some(0),
                            message: Some(msg),
                            finish_reason: event.stop_reason.clone(),
                        }],
                        usage: Some(Usage {
                            prompt_tokens: usage.input_tokens.unwrap_or(0),
                            completion_tokens: usage.output_tokens.unwrap_or(0),
                            total_tokens: usage.input_tokens.unwrap_or(0)
                                + usage.output_tokens.unwrap_or(0),
                            cache_read_input_tokens: usage.cache_read_input_tokens,
                            cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        }),
                        extra: Default::default(),
                    };

                    let entry = TraceEntry::new(
                        Provider::Anthropic,
                        request,
                        response,
                        event.duration_ms.unwrap_or(0),
                    );
                    session.add_entry(entry);
                }
            }
            Some("tool_result") | Some("tool") => {
                let msg = Message {
                    role: "tool".into(),
                    content: event.content_as_message_content(),
                    tool_calls: None,
                    tool_call_id: event.tool_use_id.clone(),
                    name: event.tool_name.clone(),
                };
                current_messages.push(msg);
            }
            _ => {}
        }

        // Collect tool definitions if present
        if let Some(ref tools) = event.tools {
            current_tools = tools
                .iter()
                .map(|t| Tool {
                    tool_type: Some("function".into()),
                    function: Some(FunctionDef {
                        name: t.name.clone().unwrap_or_default(),
                        description: t.description.clone(),
                        parameters: t.input_schema.clone(),
                    }),
                    extra: Default::default(),
                })
                .collect();
        }
    }

    Ok(session)
}

// ── Claude Code event structures ──

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CCEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    role: Option<String>,
    id: Option<String>,
    model: Option<String>,

    // Content can be string or structured
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<serde_json::Value>,

    // Usage
    usage: Option<CCUsage>,
    duration_ms: Option<u64>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,

    // Tool related
    tool_use_id: Option<String>,
    tool_name: Option<String>,
    stop_reason: Option<String>,

    // Tool definitions
    tools: Option<Vec<CCTool>>,
}

#[derive(Debug, Deserialize)]
struct CCUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CCTool {
    name: Option<String>,
    description: Option<String>,
    input_schema: Option<serde_json::Value>,
}

impl CCEvent {
    fn content_as_message_content(&self) -> Option<MessageContent> {
        // Try content field first
        if let Some(ref c) = self.content {
            if let Some(s) = c.as_str() {
                return Some(MessageContent::Text(s.to_string()));
            }
            if c.is_array() {
                // Array of content blocks — extract text
                if let Some(arr) = c.as_array() {
                    let text: String = arr
                        .iter()
                        .filter_map(|block| {
                            if block.get("type")?.as_str()? == "text" {
                                block.get("text")?.as_str().map(String::from)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        return Some(MessageContent::Text(text));
                    }
                }
            }
        }
        // Try message field
        if let Some(ref m) = self.message {
            if let Some(s) = m.as_str() {
                return Some(MessageContent::Text(s.to_string()));
            }
        }
        None
    }

    fn extract_tool_calls(&self) -> Option<Vec<ToolCall>> {
        let content = self.content.as_ref()?;
        let arr = content.as_array()?;
        let calls: Vec<ToolCall> = arr
            .iter()
            .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .filter_map(|block| {
                let name = block.get("name")?.as_str()?.to_string();
                let id = block.get("id")?.as_str()?.to_string();
                let args = block
                    .get("input")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                Some(ToolCall {
                    id: Some(id),
                    call_type: Some("function".into()),
                    function: Some(FunctionCall {
                        name,
                        arguments: args,
                    }),
                })
            })
            .collect();
        if calls.is_empty() {
            None
        } else {
            Some(calls)
        }
    }
}
