use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::api::{ChatRequest, ChatResponse};

/// A single intercepted API call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub latency_ms: u64,
    pub provider: Provider,
    pub request: ChatRequest,
    pub response: ChatResponse,
}

impl TraceEntry {
    pub fn new(
        provider: Provider,
        request: ChatRequest,
        response: ChatResponse,
        latency_ms: u64,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            latency_ms,
            provider,
            request,
            response,
        }
    }

    /// Get real prompt_tokens from API usage (None if unavailable)
    pub fn prompt_tokens(&self) -> Option<u64> {
        self.response.usage.as_ref().map(|u| u.prompt_tokens)
    }

    pub fn completion_tokens(&self) -> Option<u64> {
        self.response.usage.as_ref().map(|u| u.completion_tokens)
    }

    pub fn total_tokens(&self) -> Option<u64> {
        self.response.usage.as_ref().map(|u| u.total_tokens)
    }

    pub fn cache_read_tokens(&self) -> Option<u64> {
        self.response.usage.as_ref().and_then(|u| u.cache_read_input_tokens)
    }

    pub fn cache_creation_tokens(&self) -> Option<u64> {
        self.response.usage.as_ref().and_then(|u| u.cache_creation_input_tokens)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenAI,
    Anthropic,
    Unknown,
}

/// A full trace session containing multiple API calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSession {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub entries: Vec<TraceEntry>,
}

impl TraceSession {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            started_at: Utc::now(),
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, entry: TraceEntry) {
        self.entries.push(entry);
    }

    pub fn total_tokens(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|e| e.total_tokens())
            .sum()
    }

    pub fn total_latency_ms(&self) -> u64 {
        self.entries.iter().map(|e| e.latency_ms).sum()
    }
}

impl Default for TraceSession {
    fn default() -> Self {
        Self::new()
    }
}
