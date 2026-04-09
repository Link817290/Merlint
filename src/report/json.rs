use serde::Serialize;

use crate::analyzer::cache::CacheAnalysis;
use crate::analyzer::efficiency::EfficiencyAnalysis;
use crate::analyzer::token::SessionTokenSummary;
use crate::models::trace::TraceSession;

#[derive(Serialize)]
pub struct JsonReport {
    pub session_id: String,
    pub num_calls: usize,
    pub tokens: TokenReport,
    pub cache: CacheReport,
    pub structure: StructureReport,
    pub tools: ToolReport,
    pub efficiency: EfficiencyReport,
    pub per_call: Vec<crate::analyzer::token::CallTokens>,
}

#[derive(Serialize)]
pub struct TokenReport {
    pub total: u64,
    pub prompt: u64,
    pub completion: u64,
    pub avg_per_call: f64,
    pub total_latency_ms: u64,
}

#[derive(Serialize)]
pub struct CacheReport {
    pub data_available: bool,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub hit_ratio: Option<f64>,
    pub prefix_stability: f64,
    pub theoretical_hit_ratio: f64,
    pub issues_count: usize,
}

#[derive(Serialize)]
pub struct StructureReport {
    pub avg_messages_per_call: f64,
    pub avg_system_messages_per_call: f64,
    pub avg_tools_defined_per_call: f64,
}

#[derive(Serialize)]
pub struct ToolReport {
    pub defined: usize,
    pub used: usize,
    pub unused: usize,
    pub unused_names: Vec<String>,
    pub used_names: Vec<String>,
}

#[derive(Serialize)]
pub struct EfficiencyReport {
    pub tool_calls: usize,
    pub retries: usize,
    pub loop_patterns: usize,
    pub redundant_reads: usize,
}

pub fn generate_json(
    session: &TraceSession,
    tokens: &SessionTokenSummary,
    cache: &CacheAnalysis,
    efficiency: &EfficiencyAnalysis,
) -> String {
    let hit_ratio = if tokens.cache_data_available && tokens.total_prompt_tokens > 0 {
        Some(tokens.total_cache_read_tokens as f64 / tokens.total_prompt_tokens as f64)
    } else {
        None
    };

    let report = JsonReport {
        session_id: session.id.clone(),
        num_calls: tokens.num_calls,
        tokens: TokenReport {
            total: tokens.total_tokens,
            prompt: tokens.total_prompt_tokens,
            completion: tokens.total_completion_tokens,
            avg_per_call: if tokens.num_calls > 0 { tokens.total_tokens as f64 / tokens.num_calls as f64 } else { 0.0 },
            total_latency_ms: efficiency.total_latency_ms,
        },
        cache: CacheReport {
            data_available: tokens.cache_data_available,
            cache_read_tokens: tokens.total_cache_read_tokens,
            cache_creation_tokens: tokens.total_cache_creation_tokens,
            hit_ratio,
            prefix_stability: cache.prefix_stability_ratio,
            theoretical_hit_ratio: cache.theoretical_cache_hit_ratio,
            issues_count: cache.issues.len(),
        },
        structure: StructureReport {
            avg_messages_per_call: tokens.avg_messages_per_call,
            avg_system_messages_per_call: tokens.avg_system_messages_per_call,
            avg_tools_defined_per_call: tokens.avg_tools_defined_per_call,
        },
        tools: ToolReport {
            defined: tokens.tools_defined,
            used: tokens.tools_used,
            unused: tokens.tools_defined.saturating_sub(tokens.tools_used),
            unused_names: tokens.tool_names_unused.clone(),
            used_names: tokens.tool_names_used.clone(),
        },
        efficiency: EfficiencyReport {
            tool_calls: efficiency.tool_call_count,
            retries: efficiency.retry_count,
            loop_patterns: efficiency.loop_patterns.len(),
            redundant_reads: efficiency.redundant_reads.len(),
        },
        per_call: tokens.per_call.clone(),
    };

    serde_json::to_string_pretty(&report).unwrap_or_default()
}
