use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::trace::TraceSession;
use super::spend_log::SpendLog;
use super::transformer::{new_shared_transformer, SharedTransformer};

/// A recent activity log entry.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub session_key: String,
    pub path: String,
    pub method: String,
    pub status: u16,
    pub tokens: Option<u64>,
    pub tokens_saved: Option<i64>,
    pub latency_ms: u64,
}

const MAX_ACTIVITY_LOG: usize = 50;
const MAX_EVENT_LOG: usize = 30;

/// An event log entry (new session, optimization events, etc.)
#[derive(Debug, Clone)]
pub struct EventEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub kind: EventKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum EventKind {
    NewSession,
    Optimization,
    Info,
}

/// A read-only view of a session slot, returned by `all_slots()`.
pub struct SessionSnapshot<'a> {
    pub key: &'a str,
    pub session: &'a TraceSession,
    pub transformer: Option<&'a SharedTransformer>,
    pub project_path: Option<&'a str>,
}

/// Manages multiple concurrent sessions, each with its own trace and transformer.
pub struct SessionStore {
    sessions: HashMap<String, SessionSlot>,
    /// Whether optimization is enabled
    optimize: bool,
    /// Shared history data for initializing new transformers
    history_data: Option<(Vec<(String, i64)>, i64)>,
    /// Running tool usage accumulator from current proxy run.
    /// Merged with history_data when creating new sessions.
    runtime_tool_counts: HashMap<String, i64>,
    /// Number of sessions completed during this proxy run
    runtime_session_count: i64,
    /// Sessions that have already contributed to runtime accumulator
    contributed_sessions: HashSet<String>,
    /// Total requests received (including non-chat)
    pub total_requests: u64,
    /// Recent activity log (ring buffer)
    pub activity_log: VecDeque<ActivityEntry>,
    /// Event log (ring buffer) for dashboard display
    pub event_log: VecDeque<EventEntry>,
    /// Timestamp when the store was created
    pub started_at: chrono::DateTime<chrono::Utc>,
}

pub struct SessionSlot {
    pub session: TraceSession,
    pub transformer: Option<SharedTransformer>,
    /// Human-readable project path (e.g. "/workspace/myproject")
    pub project_path: Option<String>,
}

impl SessionStore {
    pub fn new(optimize: bool) -> Self {
        Self {
            sessions: HashMap::new(),
            optimize,
            history_data: None,
            runtime_tool_counts: HashMap::new(),
            runtime_session_count: 0,
            contributed_sessions: HashSet::new(),
            total_requests: 0,
            activity_log: VecDeque::with_capacity(MAX_ACTIVITY_LOG),
            event_log: VecDeque::with_capacity(MAX_EVENT_LOG),
            started_at: chrono::Utc::now(),
        }
    }

    /// Log a request to the activity ring buffer.
    pub fn log_activity(&mut self, entry: ActivityEntry) {
        self.total_requests += 1;
        if self.activity_log.len() >= MAX_ACTIVITY_LOG {
            self.activity_log.pop_front();
        }
        self.activity_log.push_back(entry);
    }

    /// Log an event to the event ring buffer.
    pub fn log_event(&mut self, kind: EventKind, message: String) {
        if self.event_log.len() >= MAX_EVENT_LOG {
            self.event_log.pop_front();
        }
        self.event_log.push_back(EventEntry {
            timestamp: chrono::Utc::now(),
            kind,
            message,
        });
    }

    /// Increment the total request counter (for non-chat requests).
    pub fn inc_requests(&mut self) {
        self.total_requests += 1;
    }

    /// Store tool history so new sessions can be initialized with it.
    pub fn set_history(&mut self, freq_data: Vec<(String, i64)>, total_sessions: i64) {
        self.history_data = Some((freq_data, total_sessions));
    }

    /// Get or create a session slot for the given session key.
    /// Returns (&mut SessionSlot, bool) — the bool is true if a new session was created.
    pub fn get_or_create(&mut self, key: &str) -> (&mut SessionSlot, bool) {
        self.get_or_create_with_project(key, None)
    }

    /// Get or create a session slot, optionally attaching a project path.
    pub fn get_or_create_with_project(&mut self, key: &str, project_path: Option<String>) -> (&mut SessionSlot, bool) {
        let is_new = !self.sessions.contains_key(key);
        if is_new {
            let mut session = TraceSession::new();
            session.meta_session_key = Some(key.to_string());

            let transformer = if self.optimize {
                let tx = new_shared_transformer();

                // Try per-project warm-start first (most specific)
                let mut loaded_project = false;
                if key.starts_with("sys-") {
                    if let Ok(db) = SpendLog::open() {
                        if let Ok(freq) = db.tool_frequency_for_session(key) {
                            if !freq.is_empty() {
                                let total = db.session_request_count(key).unwrap_or(0);
                                if total >= 3 {
                                    if let Ok(mut t) = tx.try_lock() {
                                        t.load_history(&freq, total);
                                        loaded_project = true;
                                    }
                                }
                            }
                        }
                    }
                }

                // Fall back to global history if no project-specific data
                if !loaded_project {
                    let merged = self.merged_history();
                    if let Some((ref freq, total)) = merged {
                        if let Ok(mut t) = tx.try_lock() {
                            t.load_history(freq, total);
                        }
                    }
                }
                Some(tx)
            } else {
                None
            };

            self.sessions.insert(key.to_string(), SessionSlot {
                session,
                transformer,
                project_path,
            });
        }
        (self.sessions.get_mut(key).unwrap(), is_new)
    }

    /// Merge DB history with runtime-accumulated tool usage.
    fn merged_history(&self) -> Option<(Vec<(String, i64)>, i64)> {
        if self.history_data.is_none() && self.runtime_tool_counts.is_empty() {
            return None;
        }

        let mut merged: HashMap<String, i64> = HashMap::new();
        let mut total_sessions: i64 = 0;

        // Start with DB history
        if let Some((ref freq, total)) = self.history_data {
            total_sessions = total;
            for (name, count) in freq {
                merged.insert(name.clone(), *count);
            }
        }

        // Add runtime data
        total_sessions += self.runtime_session_count;
        for (name, count) in &self.runtime_tool_counts {
            *merged.entry(name.clone()).or_insert(0) += count;
        }

        if merged.is_empty() || total_sessions < 3 {
            return None;
        }

        let freq: Vec<(String, i64)> = merged.into_iter().collect();
        Some((freq, total_sessions))
    }

    /// Record tool usage from a session into the runtime accumulator.
    /// Each session only contributes once (idempotent by session key).
    pub fn contribute_session_tools(&mut self, session_key: &str, tools: &[(String, usize)]) {
        if self.contributed_sessions.contains(session_key) {
            return;
        }
        self.contributed_sessions.insert(session_key.to_string());
        self.runtime_session_count += 1;
        for (name, _count) in tools {
            *self.runtime_tool_counts.entry(name.clone()).or_insert(0) += 1;
        }
    }

    pub fn get_session(&self, key: &str) -> Option<&TraceSession> {
        self.sessions.get(key).map(|s| &s.session)
    }

    pub fn get_session_mut(&mut self, key: &str) -> Option<&mut TraceSession> {
        self.sessions.get_mut(key).map(|s| &mut s.session)
    }

    pub fn get_transformer(&self, key: &str) -> Option<&SharedTransformer> {
        self.sessions.get(key).and_then(|s| s.transformer.as_ref())
    }

    /// Return all sessions.
    pub fn all_sessions(&self) -> Vec<(&str, &TraceSession)> {
        self.sessions.iter().map(|(k, s)| (k.as_str(), &s.session)).collect()
    }

    /// Return all sessions with their transformers.
    pub fn all_slots(&self) -> Vec<SessionSnapshot<'_>> {
        self.sessions
            .iter()
            .map(|(k, s)| SessionSnapshot {
                key: k.as_str(),
                session: &s.session,
                transformer: s.transformer.as_ref(),
                project_path: s.project_path.as_deref(),
            })
            .collect()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

pub type SharedSessionStore = Arc<Mutex<SessionStore>>;

pub fn new_session_store(optimize: bool) -> SharedSessionStore {
    Arc::new(Mutex::new(SessionStore::new(optimize)))
}

/// Extract a session key from an API request.
///
/// Priority:
/// 1. `X-Merlint-Session` header (explicit)
/// 2. Hash of the system prompt (implicit — unique per project)
/// 3. "default" fallback
pub fn extract_session_key(
    headers: &hyper::HeaderMap,
    body: &[u8],
) -> String {
    // 1. Explicit header
    if let Some(val) = headers.get("x-merlint-session") {
        if let Ok(s) = val.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }

    // 2. Hash system prompt from request body
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(hash) = system_prompt_hash(&val) {
            return format!("sys-{:016x}", hash);
        }
    }

    // 3. Fallback
    "default".to_string()
}

/// Hash the system prompt content to derive a stable session key.
///
/// Challenge: LLM agent frameworks (Claude Code, etc.) dynamically
/// modify the system prompt by loading skills, channel instructions,
/// and context. We need a hash that's stable across these changes
/// but unique per project/workspace.
///
/// Strategy:
/// 1. Try to extract project-identifying markers (working directory,
///    CLAUDE.md content) — these are stable per-project.
/// 2. Fall back to hashing a mid-range window (chars 200..2200) that
///    captures project context but skips the generic framework prefix
///    and the variable skill/channel suffix.
fn system_prompt_hash(body: &serde_json::Value) -> Option<u64> {
    let text = extract_system_text(body)?;
    if text.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    // Strategy 1: Look for explicit project markers
    // Claude Code includes "Primary working directory: /path/to/project"
    // and CLAUDE.md content. These are stable per-project.
    let mut project_fingerprint = String::new();

    // Extract working directory
    for marker in &["working directory:", "Working directory:", "cwd:"] {
        if let Some(pos) = text.find(marker) {
            let start = pos + marker.len();
            let end = text[start..].find('\n').map(|i| start + i).unwrap_or(text.len().min(start + 200));
            project_fingerprint.push_str(text[start..end].trim());
            break;
        }
    }

    if !project_fingerprint.is_empty() {
        project_fingerprint.hash(&mut hasher);
        return Some(hasher.finish());
    }

    // Strategy 2: No explicit markers found — hash a stable window.
    // Skip first 200 chars (generic framework instructions like
    // "You are Claude Code..."), take up to 2000 chars of the
    // middle section which typically contains project-specific context.
    // Skip the tail where skills/channels are appended.
    let start = 200.min(text.len());
    let end = 2200.min(text.len());
    if start >= end {
        // Very short prompt — just hash it all
        text.hash(&mut hasher);
    } else {
        text[start..end].hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// Extract the project working directory from the request body, if present.
pub fn extract_project_path(body: &[u8]) -> Option<String> {
    let val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let text = extract_system_text(&val)?;
    for marker in &["Primary working directory: ", "working directory: ", "Working directory: ", "cwd: "] {
        if let Some(pos) = text.find(marker) {
            let start = pos + marker.len();
            let end = text[start..].find('\n').map(|i| start + i).unwrap_or(text.len().min(start + 200));
            let path = text[start..end].trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    None
}

/// Extract system prompt text from either Anthropic or OpenAI format.
fn extract_system_text(body: &serde_json::Value) -> Option<String> {
    // Anthropic format: top-level "system" field
    if let Some(sys) = body.get("system") {
        return match sys {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(arr) => {
                let text: String = arr
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                            b.get("text").and_then(|v| v.as_str())
                        } else {
                            b.as_str()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            }
            _ => None,
        };
    }

    // OpenAI format: first message with role "system"
    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            if msg.get("role").and_then(|v| v.as_str()) == Some("system") {
                if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                    return Some(content.to_string());
                }
            }
        }
    }

    None
}
