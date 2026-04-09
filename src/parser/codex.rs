use std::path::Path;

use serde::Deserialize;

use crate::models::api::*;
use crate::models::trace::*;

/// Parse a Codex CLI session file
/// Codex stores sessions as JSON with a list of turns
pub fn parse_session(path: &Path) -> anyhow::Result<TraceSession> {
    let content = std::fs::read_to_string(path)?;
    let mut session = TraceSession::new();

    // Try parsing as a Codex session format
    // Codex uses OpenAI responses API format
    if let Ok(codex_session) = serde_json::from_str::<CodexSession>(&content) {
        for (idx, turn) in codex_session.turns.iter().enumerate() {
            let messages = build_messages_up_to(&codex_session, idx);

            let request = ChatRequest {
                model: codex_session.model.clone(),
                messages,
                tools: codex_session
                    .tools
                    .as_ref()
                    .map(|tools| {
                        tools
                            .iter()
                            .map(|t| Tool {
                                tool_type: Some("function".into()),
                                function: Some(FunctionDef {
                                    name: t.name.clone().unwrap_or_default(),
                                    description: t.description.clone(),
                                    parameters: t.parameters.clone(),
                                }),
                                extra: Default::default(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                extra: Default::default(),
            };

            let tool_calls: Option<Vec<ToolCall>> = turn.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c.id.clone(),
                        call_type: Some("function".into()),
                        function: Some(FunctionCall {
                            name: c.name.clone().unwrap_or_default(),
                            arguments: c.arguments.clone().unwrap_or_default(),
                        }),
                    })
                    .collect()
            });

            let response = ChatResponse {
                id: turn.id.clone(),
                model: codex_session.model.clone(),
                choices: vec![Choice {
                    index: Some(0),
                    message: Some(Message {
                        role: "assistant".into(),
                        content: turn.content.as_ref().map(|s| MessageContent::Text(s.clone())),
                        tool_calls,
                        tool_call_id: None,
                        name: None,
                    }),
                    finish_reason: turn.finish_reason.clone(),
                }],
                usage: turn.usage.as_ref().map(|u| Usage {
                    prompt_tokens: u.prompt_tokens.unwrap_or(0),
                    completion_tokens: u.completion_tokens.unwrap_or(0),
                    total_tokens: u.total_tokens.unwrap_or(0),
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                }),
                extra: Default::default(),
            };

            let entry = TraceEntry::new(
                Provider::OpenAI,
                request,
                response,
                turn.duration_ms.unwrap_or(0),
            );
            session.add_entry(entry);
        }
    } else {
        // Fallback: try JSONL format
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(turn) = serde_json::from_str::<CodexTurn>(line) {
                if turn.role.as_deref() == Some("assistant") {
                    if let Some(ref usage) = turn.usage {
                        let request = ChatRequest {
                            model: None,
                            messages: Vec::new(),
                            tools: Vec::new(),
                            extra: Default::default(),
                        };
                        let response = ChatResponse {
                            id: turn.id.clone(),
                            model: None,
                            choices: vec![Choice {
                                index: Some(0),
                                message: Some(Message {
                                    role: "assistant".into(),
                                    content: turn
                                        .content
                                        .as_ref()
                                        .map(|s| MessageContent::Text(s.clone())),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    name: None,
                                }),
                                finish_reason: turn.finish_reason.clone(),
                            }],
                            usage: Some(Usage {
                                prompt_tokens: usage.prompt_tokens.unwrap_or(0),
                                completion_tokens: usage.completion_tokens.unwrap_or(0),
                                total_tokens: usage.total_tokens.unwrap_or(0),
                                cache_read_input_tokens: None,
                                cache_creation_input_tokens: None,
                            }),
                            extra: Default::default(),
                        };
                        session.add_entry(TraceEntry::new(
                            Provider::OpenAI,
                            request,
                            response,
                            turn.duration_ms.unwrap_or(0),
                        ));
                    }
                }
            }
        }
    }

    Ok(session)
}

fn build_messages_up_to(session: &CodexSession, up_to: usize) -> Vec<Message> {
    let mut msgs = Vec::new();
    if let Some(ref system) = session.system_prompt {
        msgs.push(Message {
            role: "system".into(),
            content: Some(MessageContent::Text(system.clone())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    for turn in session.turns.iter().take(up_to) {
        let role = turn.role.clone().unwrap_or_else(|| "user".into());
        msgs.push(Message {
            role,
            content: turn.content.as_ref().map(|s| MessageContent::Text(s.clone())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    msgs
}

// ── Codex session structures ──

#[derive(Debug, Deserialize)]
struct CodexSession {
    model: Option<String>,
    system_prompt: Option<String>,
    #[serde(default)]
    turns: Vec<CodexTurn>,
    tools: Option<Vec<CodexTool>>,
}

#[derive(Debug, Deserialize)]
struct CodexTurn {
    id: Option<String>,
    role: Option<String>,
    content: Option<String>,
    finish_reason: Option<String>,
    duration_ms: Option<u64>,
    usage: Option<CodexUsage>,
    tool_calls: Option<Vec<CodexToolCall>>,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CodexToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexTool {
    name: Option<String>,
    description: Option<String>,
    parameters: Option<serde_json::Value>,
}
