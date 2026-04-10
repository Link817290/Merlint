use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::trace::TraceSession;
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

/// Manages multiple concurrent sessions, each with its own trace and transformer.
pub struct SessionStore {
    sessions: HashMap<String, SessionSlot>,
    /// Whether optimization is enabled
    optimize: bool,
    /// Shared history data for initializing new transformers
    history_data: Option<(Vec<(String, i64)>, i64)>,
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
}

impl SessionStore {
    pub fn new(optimize: bool) -> Self {
        Self {
            sessions: HashMap::new(),
            optimize,
            history_data: None,
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
        let is_new = !self.sessions.contains_key(key);
        if is_new {
            let mut session = TraceSession::new();
            session.meta_session_key = Some(key.to_string());

            let transformer = if self.optimize {
                let tx = new_shared_transformer();
                // Initialize with history if available
                if let Some((ref freq, total)) = self.history_data {
                    if let Ok(mut t) = tx.try_lock() {
                        t.load_history(freq, total);
                    }
                }
                Some(tx)
            } else {
                None
            };

            self.sessions.insert(key.to_string(), SessionSlot {
                session,
                transformer,
            });
        }
        (self.sessions.get_mut(key).unwrap(), is_new)
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
    pub fn all_slots(&self) -> Vec<(&str, &TraceSession, Option<&SharedTransformer>)> {
        self.sessions
            .iter()
            .map(|(k, s)| (k.as_str(), &s.session, s.transformer.as_ref()))
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
fn system_prompt_hash(body: &serde_json::Value) -> Option<u64> {
    let text = extract_system_text(body)?;
    if text.is_empty() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    Some(hasher.finish())
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
