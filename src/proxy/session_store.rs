use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::models::trace::TraceSession;
use super::spend_log::{HistoricalSummary, SpendLog};
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
    pub historical: Option<&'a HistoricalSummary>,
    pub last_request_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// Historical totals for this session_key loaded from spend.db at slot
    /// creation time. Lets stats persist across proxy restarts — every time
    /// a conversation resumes, its lifetime stats come with it.
    pub historical: Option<HistoricalSummary>,
    /// Wall-clock moment we last started forwarding a chat request upstream.
    /// Closer to Anthropic's own 5-minute cache TTL reset point than
    /// `session.entries.last().timestamp`, which only fires AFTER the full
    /// response has been collected — for long streaming responses the trace
    /// entry lags the real cache refresh by the full response duration, so
    /// the dashboard countdown would decrement visibly during streaming
    /// instead of snapping back. Using the request-start time fixes that.
    pub last_request_at: Option<chrono::DateTime<chrono::Utc>>,
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
    ///
    /// Two paths create a transformer when `self.optimize` is true:
    /// 1. Brand new slot — build + warm-start the transformer immediately.
    /// 2. Existing slot that still has `transformer: None` — this is the
    ///    "preloaded at startup, now going live" case. Without this lazy
    ///    upgrade, preloaded sessions stay optimizer-less forever because
    ///    preload deliberately inserts slots with no transformer (it would
    ///    be wasteful to warm-start every dormant session at boot).
    pub fn get_or_create_with_project(&mut self, key: &str, project_path: Option<String>) -> (&mut SessionSlot, bool) {
        let is_new = !self.sessions.contains_key(key);

        // Decide up-front whether we'll need to build a transformer. Doing it
        // before we grab a mutable reference to the slot keeps the borrow
        // checker happy — build_transformer needs &self for merged_history().
        let needs_new_transformer = self.optimize
            && (is_new
                || self
                    .sessions
                    .get(key)
                    .map(|s| s.transformer.is_none())
                    .unwrap_or(false));
        let new_transformer = if needs_new_transformer {
            Some(self.build_transformer(key))
        } else {
            None
        };

        if is_new {
            let mut session = TraceSession::new();
            session.meta_session_key = Some(key.to_string());
            self.sessions.insert(key.to_string(), SessionSlot {
                session,
                transformer: new_transformer,
                project_path,
                historical: None,
                last_request_at: None,
            });
        } else {
            let slot = self.sessions.get_mut(key).unwrap();
            if project_path.is_some() && slot.project_path.is_none() {
                slot.project_path = project_path;
            }
            if slot.transformer.is_none() {
                if let Some(tx) = new_transformer {
                    slot.transformer = Some(tx);
                }
            }
        }
        (self.sessions.get_mut(key).unwrap(), is_new)
    }

    /// Build and warm-start a transformer for the given session key. Tries
    /// per-project history from spend.db first; falls back to global merged
    /// history if nothing project-specific is available.
    ///
    /// Opens its own SpendLog handle — this is only called when optimize=on,
    /// which tests leave off, so test runs never touch the real spend.db.
    fn build_transformer(&self, key: &str) -> SharedTransformer {
        let tx = new_shared_transformer();

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

        if !loaded_project {
            if let Some((ref freq, total)) = self.merged_history() {
                if let Ok(mut t) = tx.try_lock() {
                    t.load_history(freq, total);
                }
            }
        }
        tx
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
                historical: s.historical.as_ref(),
                last_request_at: s.last_request_at,
            })
            .collect()
    }

    /// Stamp a session slot with the wall-clock moment we started forwarding
    /// its most recent chat request upstream. Used as the cache-TTL anchor
    /// on the dashboard countdown so the UI resets as soon as the user hits
    /// send, instead of waiting for the full streaming response to finish.
    pub fn mark_request_started(&mut self, key: &str) {
        if let Some(slot) = self.sessions.get_mut(key) {
            slot.last_request_at = Some(chrono::Utc::now());
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Pre-populate the store with sessions previously logged to `db`
    /// within the last `days` days. Only sessions that actually routed
    /// through the proxy (and thus appear in spend_log) will be loaded —
    /// anything that never went through the proxy is intentionally skipped
    /// because we have no data for it and can't track it going forward.
    ///
    /// Rows with no `project_path` are also skipped: those are legacy
    /// entries written before the project_path column was added, so we
    /// can't attribute them to any specific project card. They remain in
    /// spend.db and still count toward total spend / `merlint report`,
    /// but they don't pollute the per-project dashboard view.
    ///
    /// Skips the internal `__background__` / `__non_chat__` buckets so the
    /// dashboard's project view stays clean. Returns the number of slots
    /// that were inserted.
    pub fn preload_recent_sessions(&mut self, db: &SpendLog, days: u32) -> usize {
        let Ok(rows) = db.recent_sessions(days) else { return 0 };
        let mut inserted = 0;
        for row in rows {
            if row.session_key == BACKGROUND_SESSION_KEY || row.session_key == "__non_chat__" {
                continue;
            }
            if row.project_path.is_empty() {
                continue;
            }
            if self.sessions.contains_key(&row.session_key) {
                continue;
            }
            let mut session = TraceSession::new();
            session.meta_session_key = Some(row.session_key.clone());
            self.sessions.insert(
                row.session_key.clone(),
                SessionSlot {
                    session,
                    transformer: None,
                    project_path: Some(row.project_path.clone()),
                    historical: Some(row.summary),
                    last_request_at: None,
                },
            );
            inserted += 1;
        }
        inserted
    }

    /// Attach historical totals to an existing slot. Called from the server
    /// layer after `get_or_create_with_project` when spend_log is configured,
    /// so the dashboard picks up lifetime stats for resumed conversations.
    /// No-op if the slot already has historical data or doesn't exist.
    pub fn attach_historical(&mut self, key: &str, summary: HistoricalSummary) {
        if let Some(slot) = self.sessions.get_mut(key) {
            if slot.historical.is_none() && summary.request_count > 0 {
                slot.historical = Some(summary);
            }
        }
    }
}

pub type SharedSessionStore = Arc<Mutex<SessionStore>>;

pub fn new_session_store(optimize: bool) -> SharedSessionStore {
    Arc::new(Mutex::new(SessionStore::new(optimize)))
}

/// Shared bucket for short/auxiliary requests that don't belong to any
/// identifiable project (e.g. Claude Code's Haiku title-generation calls or
/// quota probes). Hidden from the dashboard project list.
pub const BACKGROUND_SESSION_KEY: &str = "__background__";

/// System prompts shorter than this, without a working-directory marker, are
/// routed to the background bucket. Claude Code main conversations carry
/// ~10–20 KB of system text, auxiliary Haiku requests are well under 1 KB.
const SHORT_SYSTEM_THRESHOLD: usize = 2000;

/// Extract a session key from an API request.
///
/// Priority:
/// 1. `X-Merlint-Session` header (explicit)
/// 2. If the system prompt contains a working-directory marker, hash that
///    path so every request from the same project collapses into one session.
/// 3. If the system prompt is short or missing, route to the shared
///    `__background__` bucket — these are Haiku title generation, quota
///    probes, etc. and shouldn't spawn fake "unknown" projects.
/// 4. Long system prompt without any project marker — fall back to hashing a
///    stable mid-range window so genuinely different agents stay separate.
pub fn extract_session_key(
    headers: &hyper::HeaderMap,
    body: &[u8],
) -> String {
    // 1. Explicit header override
    if let Some(val) = headers.get("x-merlint-session") {
        if let Ok(s) = val.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }

    let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) else {
        return BACKGROUND_SESSION_KEY.to_string();
    };
    let text = extract_system_text(&val).unwrap_or_default();

    // 2. Explicit project marker — compose a stable per-conversation key
    //    from the working directory AND a fingerprint of the conversation
    //    history. Every follow-up turn in the same conversation resends the
    //    same messages[0], so hashing it gives a stable id that's distinct
    //    per Claude Code window / Codex run, while still grouping under the
    //    same project_path in the dashboard.
    if let Some(path) = find_working_dir(&text) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        let path_hash = hasher.finish();

        let conv_hash = conversation_fingerprint(&val);
        return match conv_hash {
            Some(c) => format!("sys-{:016x}-{:016x}", path_hash, c),
            None => format!("sys-{:016x}", path_hash),
        };
    }

    // 3. Short / missing system → background bucket.
    if text.len() < SHORT_SYSTEM_THRESHOLD {
        return BACKGROUND_SESSION_KEY.to_string();
    }

    // 4. Long system prompt, no explicit marker — hash a stable window.
    //    Skip the first 200 chars (generic framework prefix) and the tail
    //    (where skills/channels get appended) so small variations between
    //    turns don't shatter the session.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let start = 200.min(text.len());
    let end = 2200.min(text.len());
    if start >= end {
        text.hash(&mut hasher);
    } else {
        text[start..end].hash(&mut hasher);
    }
    let window_hash = hasher.finish();
    match conversation_fingerprint(&val) {
        Some(c) => format!("sys-{:016x}-{:016x}", window_hash, c),
        None => format!("sys-{:016x}", window_hash),
    }
}

/// Derive a per-conversation fingerprint from the request body. Used to
/// separate multiple concurrent conversations that share the same project.
///
/// Strategy: hash the first user message. Claude Code / Codex / OpenAI
/// clients all resend the full message history on each turn, so the first
/// user message is a stable identifier for the conversation throughout its
/// lifetime. Different chat windows almost always have distinct first
/// messages so collisions are negligible in practice.
fn conversation_fingerprint(body: &serde_json::Value) -> Option<u64> {
    let msgs = body.get("messages")?.as_array()?;
    let first_user = msgs.iter().find(|m| {
        m.get("role").and_then(|v| v.as_str()) == Some("user")
    })?;
    let content = first_user.get("content")?;

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    match content {
        serde_json::Value::String(s) => {
            s.hash(&mut hasher);
            any = !s.is_empty();
        }
        serde_json::Value::Array(arr) => {
            for b in arr {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    t.hash(&mut hasher);
                    any = true;
                } else if let Some(s) = b.as_str() {
                    s.hash(&mut hasher);
                    any = true;
                }
            }
        }
        _ => {}
    }
    if any {
        Some(hasher.finish())
    } else {
        None
    }
}

/// Extract the working-directory path from a system prompt, tolerating
/// multiple marker spellings and variable whitespace/newline placement.
///
/// Accepts: "Primary working directory: /path", "Working directory:\n/path",
/// "cwd: /path", etc. Returns the trimmed path, or None if no marker found.
fn find_working_dir(text: &str) -> Option<&str> {
    // Ordered from most specific to most generic so a "Primary working
    // directory:" match isn't shadowed by the substring "working directory:".
    const MARKERS: &[&str] = &[
        "Primary working directory:",
        "primary working directory:",
        "Working directory:",
        "working directory:",
        "cwd:",
    ];
    for m in MARKERS {
        let Some(pos) = text.find(m) else { continue };
        let rest = &text[pos + m.len()..];
        // Tolerate an optional space or newline after the colon.
        let rest = rest.trim_start_matches([' ', '\t']);
        let end = rest.find('\n').unwrap_or_else(|| rest.len().min(300));
        let path = rest[..end].trim();
        if !path.is_empty() {
            return Some(path);
        }
    }
    None
}

/// Extract the project working directory from the request body, if present.
pub fn extract_project_path(body: &[u8]) -> Option<String> {
    let val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let text = extract_system_text(&val)?;
    find_working_dir(&text).map(String::from)
}

/// Extract system prompt text from either Anthropic or OpenAI format.
fn extract_system_text(body: &serde_json::Value) -> Option<String> {
    // Anthropic format: top-level "system" field. Note we must NOT early-return
    // when this field is null/empty — Claude Code actually sends
    // `"system": null` and puts the real prompt inside `messages[0]` with
    // role="system" (OpenAI convention). We collect from both sources and
    // fall through if the top-level field yields nothing.
    let from_top = body.get("system").and_then(|sys| match sys {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
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
    });
    if from_top.is_some() {
        return from_top;
    }

    // Fallback: look for role="system" entries in the messages array. Content
    // can be a plain string or an array of text blocks (Anthropic-style).
    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        let mut collected: Vec<String> = Vec::new();
        for msg in msgs {
            if msg.get("role").and_then(|v| v.as_str()) != Some("system") {
                continue;
            }
            let content = msg.get("content");
            if let Some(s) = content.and_then(|v| v.as_str()) {
                collected.push(s.to_string());
            } else if let Some(arr) = content.and_then(|v| v.as_array()) {
                for b in arr {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        collected.push(t.to_string());
                    } else if let Some(s) = b.as_str() {
                        collected.push(s.to_string());
                    }
                }
            }
        }
        if !collected.is_empty() {
            return Some(collected.join("\n"));
        }
    }

    None
}
