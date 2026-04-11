use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{error, info};

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

fn full_body(bytes: Bytes) -> BoxBody {
    Full::new(bytes).map_err(|_| unreachable!()).boxed()
}

fn full_body_from_vec(v: Vec<u8>) -> BoxBody {
    full_body(Bytes::from(v))
}

/// Build a JSON response, falling back to a 500 error if serialization or builder fails.
fn json_response(status: u16, body: &impl serde::Serialize) -> Response<BoxBody> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_body_from_vec(bytes))
        .unwrap_or_else(|_| {
            Response::new(full_body(Bytes::from_static(b"{\"error\":\"internal\"}")))
        })
}

use crate::models::api::{
    ChatRequest, ChatResponse, Choice, FunctionCall, FunctionDef, Message, MessageContent, Tool,
    ToolCall, Usage,
};
use crate::models::trace::{Provider, TraceEntry};
use super::cost::CostCalculator;
use super::session_store::{extract_session_key, extract_project_path, SharedSessionStore};
use super::spend_log::{BudgetConfig, SharedSpendLog, SpendEntry};
use super::transformer::is_file_write_tool;

pub struct ProxyConfig {
    pub listen_port: u16,
    pub target_url: String,
    pub api_key: Option<String>,
    /// When set, write traces to files under this directory (one per session)
    pub live_trace_dir: Option<PathBuf>,
    /// Enable real-time request optimization
    pub optimize: bool,
}

pub async fn run_proxy(
    config: ProxyConfig,
    store: SharedSessionStore,
    spend_log: Option<SharedSpendLog>,
) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", config.listen_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("merlint proxy listening on {}", addr);
    info!("forwarding to {}", config.target_url);
    if config.optimize {
        info!("real-time optimization ENABLED");
    }
    info!("multi-session tracking ENABLED");
    if spend_log.is_some() {
        info!("spend tracking ENABLED (persistent)");
    }

    // Pre-populate the session store with sessions that had activity in the
    // last 7 days so the dashboard shows historical data immediately, even
    // before any new request arrives. Only sessions that previously routed
    // through the proxy (and thus exist in spend_log) are loaded — anything
    // not in spend_log can't be monitored going forward anyway.
    if let Some(ref sl) = spend_log {
        let log = sl.lock().await;
        let mut s = store.lock().await;
        let loaded = s.preload_recent_sessions(&log, 7);
        if loaded > 0 {
            info!("preloaded {} sessions from spend.db", loaded);
        }
    }

    let budget = BudgetConfig::from_env();
    if budget.has_limits() {
        info!("budget limits: daily=${:.2}, session=${:.2}",
            budget.daily_limit_usd, budget.session_limit_usd);
    }
    let budget = Arc::new(budget);
    let config = Arc::new(config);
    let client = Arc::new(reqwest::Client::new());
    let cost_calc = Arc::new(CostCalculator::new());

    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let config = config.clone();
        let store = store.clone();
        let client = client.clone();
        let spend_log = spend_log.clone();
        let cost_calc = cost_calc.clone();
        let budget = budget.clone();

        tokio::spawn(async move {
            let config = config.clone();
            let store = store.clone();
            let client = client.clone();
            let spend_log = spend_log.clone();
            let cost_calc = cost_calc.clone();
            let budget = budget.clone();
            let service = service_fn(move |req| {
                handle_request(
                    req, config.clone(), store.clone(), client.clone(),
                    spend_log.clone(), cost_calc.clone(), budget.clone(),
                )
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
    store: SharedSessionStore,
    client: Arc<reqwest::Client>,
    spend_log: Option<SharedSpendLog>,
    cost_calc: Arc<CostCalculator>,
    budget: Arc<BudgetConfig>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    // Status endpoint — returns session stats as JSON
    if path == "/merlint/status" {
        return Ok(handle_status(&store, &spend_log, &cost_calc).await);
    }

    // Dashboard endpoint — serves the web UI
    if path == "/merlint/dashboard" || path == "/merlint/dashboard/" {
        return Ok(handle_dashboard());
    }

    // Spend stats API endpoint
    if path == "/merlint/spend" {
        return Ok(handle_spend_api(&spend_log).await);
    }

    let body_bytes = req.collect().await?.to_bytes();

    let provider = detect_provider(&path, &headers);
    let is_chat = (path.contains("/chat/completions") || path.contains("/messages")
        || path.contains("/completions"))
        && !path.contains("/count_tokens")
        && !path.contains("/batches");

    // Extract session key for multi-session routing
    // Non-chat requests don't create sessions — they're only logged as activity
    let session_key = if is_chat {
        extract_session_key(&headers, &body_bytes)
    } else {
        "__non_chat__".to_string()
    };

    // Extract project path for display (only on first request)
    let project_path = if is_chat {
        extract_project_path(&body_bytes)
    } else {
        None
    };

    // Budget enforcement: check spend limits before forwarding
    if is_chat && budget.has_limits() {
        if let Some(ref sl) = spend_log {
            let log = sl.lock().await;
            if let Err(msg) = super::spend_log::check_budget(&log, &budget, &session_key) {
                tracing::warn!("Budget limit hit: {}", msg);
                let err_body = serde_json::json!({
                    "error": {
                        "type": "budget_exceeded",
                        "message": msg,
                    }
                });
                return Ok(json_response(429, &err_body));
            }
        }
    }

    let target_url = format!("{}{}", config.target_url.trim_end_matches('/'), path);

    // Optionally transform the request body
    let is_anthropic_native = provider == Provider::Anthropic;

    // Create (or look up) the session atomically. The "is new?" check and the
    // actual creation happen under the same lock so concurrent requests with
    // the same key can't double-emit "New session" events.
    let mut created_new = false;
    if is_chat {
        let mut store_guard = store.lock().await;
        let (_, is_new) =
            store_guard.get_or_create_with_project(&session_key, project_path.clone());
        created_new = is_new;
        // Don't emit "New session" events for the shared background bucket —
        // every Haiku title/quota request would otherwise spam the event log.
        if is_new && session_key != super::session_store::BACKGROUND_SESSION_KEY {
            let count = store_guard.session_count();
            let project_full = project_path.as_deref().unwrap_or("unknown");
            let project_display = project_full
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or(project_full);
            store_guard.log_event(
                super::session_store::EventKind::NewSession,
                format!("New session: {} ({})", project_display, count),
            );
            drop(store_guard);
            info!(
                "New session detected: [{}] project={} (active sessions: {})",
                session_key, project_full, count
            );
        }
    }

    // If we just created a fresh slot for a live request and spend_log is
    // configured, pull that session's historical totals so cumulative stats
    // survive proxy restarts / resumed conversations. Done outside the
    // earlier critical section to avoid holding the store lock over I/O.
    if created_new && session_key != super::session_store::BACKGROUND_SESSION_KEY {
        if let Some(ref sl) = spend_log {
            let log = sl.lock().await;
            if let Ok(summary) = log.session_history(&session_key) {
                if summary.request_count > 0 {
                    drop(log);
                    let mut store_guard = store.lock().await;
                    store_guard.attach_historical(&session_key, summary);
                }
            }
        }
    }

    let (final_body, transform_stats) = if is_chat && config.optimize {
        // Get (or create) the transformer for this session
        let mut store_guard = store.lock().await;
        let (slot, _) = store_guard.get_or_create_with_project(&session_key, project_path.clone());
        if let Some(ref tx) = slot.transformer {
            let tx = tx.clone();
            drop(store_guard); // release store lock before locking transformer
            if is_anthropic_native {
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
            drop(store_guard);
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

    // Stamp the slot with the request-start time BEFORE we actually send
    // upstream. This is what the dashboard countdown uses as the cache TTL
    // anchor — resetting here means the 5-minute timer restarts the moment
    // a new request goes out, not 5-30 seconds later when the full response
    // has been collected. Only meaningful for chat sessions; non-chat paths
    // don't feed the dashboard cache countdown.
    if is_chat {
        let mut store_guard = store.lock().await;
        store_guard.mark_request_started(&session_key);
    }

    let start = Instant::now();
    let response = match forward_req.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("upstream error: {}", e);
            let err_body = serde_json::json!({
                "error": { "message": format!("merlint proxy error: {}", e) }
            });
            return Ok(json_response(502, &err_body));
        }
    };

    let status = response.status();
    let resp_headers = response.headers().clone();
    let is_sse = resp_headers.get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    // For SSE responses: stream through immediately with background processing
    if is_sse && is_chat {
        let mut resp_builder = Response::builder().status(status.as_u16());
        for (key, value) in resp_headers.iter() {
            let key_str = key.as_str().to_lowercase();
            if key_str == "transfer-encoding" || key_str == "content-length" {
                continue;
            }
            resp_builder = resp_builder.header(key.as_str(), value);
        }
        resp_builder = resp_builder.header("content-type", "text/event-stream");

        // Create a channel for streaming: we tee each chunk to both client and collector
        let (tx_stream, rx_stream) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(32);
        let store_bg = store.clone();
        let session_key_bg = session_key.clone();
        let spend_log_bg = spend_log.clone();
        let cost_calc_bg = cost_calc.clone();
        let transform_stats_bg = transform_stats;
        let is_anthropic_bg = is_anthropic_native;
        let config_bg = config.clone();
        let body_bytes_bg = body_bytes.clone();
        let path_bg = path.clone();
        let provider_bg = provider;
        let status_code = status.as_u16();
        let target_url_bg = target_url.clone();

        // Spawn background task: forward reqwest stream chunks to the channel
        // and collect full response for post-processing
        let mut byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut collected = Vec::new();
            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        collected.extend_from_slice(&chunk);
                        let _ = tx_stream.send(Ok(Frame::data(chunk))).await;
                    }
                    Err(e) => {
                        tracing::warn!("SSE stream error: {}", e);
                        break;
                    }
                }
            }

            let resp_bytes = Bytes::from(collected);
            let latency_ms = start.elapsed().as_millis() as u64;

            if hyper::StatusCode::from_u16(status_code).map(|s| s.is_success()).unwrap_or(false) {
                process_chat_response(
                    &store_bg, &session_key_bg, &resp_bytes, is_anthropic_bg,
                    &spend_log_bg, &cost_calc_bg, transform_stats_bg, latency_ms,
                    &config_bg, &body_bytes_bg, &path_bg, provider_bg, status_code,
                ).await;
            } else {
                log_upstream_error(&target_url_bg, status_code, &resp_bytes);
            }
        });

        // Build streaming response body
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx_stream);
        let stream_body = StreamBody::new(stream);
        let boxed_body: BoxBody = http_body_util::BodyExt::boxed(stream_body);

        return Ok(resp_builder.body(boxed_body).expect("valid response builder"));
    }

    let resp_bytes = response.bytes().await.unwrap_or_default();
    let latency_ms = start.elapsed().as_millis() as u64;

    // Record trace, tool usage, spend for chat completions
    if is_chat && status.is_success() {
        process_chat_response(
            &store, &session_key, &resp_bytes, is_anthropic_native,
            &spend_log, &cost_calc, transform_stats, latency_ms,
            &config, &body_bytes, &path, provider, status.as_u16(),
        ).await;
    } else if is_chat {
        log_upstream_error(&target_url, status.as_u16(), &resp_bytes);
        // Fall through so the error response is still logged as activity below.
        let saved = transform_stats.map(|(_, _, s)| s);
        let mut store_guard = store.lock().await;
        store_guard.log_activity(super::session_store::ActivityEntry {
            timestamp: chrono::Utc::now(),
            session_key: session_key.clone(),
            path: path.clone(),
            method: method.to_string(),
            status: status.as_u16(),
            tokens: None,
            tokens_saved: saved,
            latency_ms,
        });
    } else {
        // Log activity for non-chat requests
        let saved = transform_stats.map(|(_, _, s)| s);
        let mut store_guard = store.lock().await;
        store_guard.log_activity(super::session_store::ActivityEntry {
            timestamp: chrono::Utc::now(),
            session_key: session_key.clone(),
            path: path.clone(),
            method: method.to_string(),
            status: status.as_u16(),
            tokens: None,
            tokens_saved: saved,
            latency_ms,
        });
    }

    let mut resp_builder = Response::builder().status(status.as_u16());
    for (key, value) in resp_headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str == "transfer-encoding" || key_str == "content-length" {
            continue;
        }
        resp_builder = resp_builder.header(key.as_str(), value);
    }

    if is_sse {
        resp_builder = resp_builder.header("content-type", "text/event-stream");
    }

    Ok(resp_builder
        .body(full_body(resp_bytes))
        .expect("valid response builder"))
}

/// Handle the /merlint/dashboard endpoint — serves the web UI with embedded logo.
fn handle_dashboard() -> Response<BoxBody> {
    let html = include_str!("dashboard.html");
    Response::builder()
        .status(200)
        .header("content-type", "text/html; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(full_body(Bytes::from(html)))
        .expect("valid response builder")
}

/// Handle the /merlint/spend endpoint — returns persistent spend stats as JSON.
async fn handle_spend_api(spend_log: &Option<SharedSpendLog>) -> Response<BoxBody> {
    let body = if let Some(ref sl) = spend_log {
        let log = sl.lock().await;
        let total = log.total_summary().ok();
        let today = log.summary_last_days(1).ok();
        let week = log.summary_last_days(7).ok();
        let daily = log.daily_breakdown(30).ok();
        let by_session = log.session_breakdown(30).ok();
        let by_model = log.model_breakdown(30).ok();

        let fmt_summary = |s: &super::spend_log::SpendSummary| serde_json::json!({
            "requests": s.request_count,
            "cost_usd": format!("{:.4}", s.total_cost_usd),
            "saved_usd": format!("{:.4}", s.total_saved_usd),
            "tokens": s.total_tokens,
            "tokens_saved": s.total_tokens_saved,
        });

        serde_json::json!({
            "total": total.as_ref().map(fmt_summary),
            "today": today.as_ref().map(fmt_summary),
            "week": week.as_ref().map(fmt_summary),
            "daily": daily.unwrap_or_default().iter().map(|d| serde_json::json!({
                "date": d.date, "cost_usd": format!("{:.4}", d.cost_usd),
                "saved_usd": format!("{:.4}", d.saved_usd),
                "tokens": d.tokens, "tokens_saved": d.tokens_saved, "requests": d.requests,
            })).collect::<Vec<_>>(),
            "by_session": by_session.unwrap_or_default().iter().map(|s| serde_json::json!({
                "session_key": s.session_key, "cost_usd": format!("{:.4}", s.cost_usd),
                "saved_usd": format!("{:.4}", s.saved_usd),
                "tokens": s.tokens, "tokens_saved": s.tokens_saved, "requests": s.requests,
            })).collect::<Vec<_>>(),
            "by_model": by_model.unwrap_or_default().iter().map(|m| serde_json::json!({
                "model": m.model, "cost_usd": format!("{:.4}", m.cost_usd),
                "saved_usd": format!("{:.4}", m.saved_usd),
                "tokens": m.tokens, "requests": m.requests,
            })).collect::<Vec<_>>(),
        })
    } else {
        serde_json::json!({"error": "spend tracking not enabled"})
    };

    json_response(200, &body)
}

/// Handle the /merlint/status endpoint — returns session stats as JSON.
async fn handle_status(
    store: &SharedSessionStore,
    spend_log: &Option<SharedSpendLog>,
    cost_calc: &CostCalculator,
) -> Response<BoxBody> {
    let s = store.lock().await;
    let mut sessions = Vec::new();

    for slot in s.all_slots() {
        // Hide internal buckets from the per-project breakdown:
        // __non_chat__   — health checks, dashboard fetches, etc.
        // __background__ — Haiku title generation, quota probes, etc.
        if slot.key == "__non_chat__"
            || slot.key == super::session_store::BACKGROUND_SESSION_KEY
        {
            continue;
        }
        let (key, session, tx_opt, project, historical, slot_last_request_at) = (
            slot.key,
            slot.session,
            slot.transformer,
            slot.project_path,
            slot.historical,
            slot.last_request_at,
        );
        // Live values from this proxy run. Note: Anthropic's `input_tokens`
        // is the FRESH (non-cached) portion only — true prompt size is
        // input + cache_read + cache_creation.
        let live_requests: u64 = session.entries.len() as u64;
        let live_fresh_input: u64 = session.entries.iter().filter_map(|e| e.prompt_tokens()).sum();
        let live_completion: u64 = session.entries.iter().filter_map(|e| e.completion_tokens()).sum();
        let live_cache_read: u64 = session.entries.iter().filter_map(|e| e.cache_read_tokens()).sum();
        let live_cache_creation: u64 = session
            .entries
            .iter()
            .filter_map(|e| e.cache_creation_tokens())
            .sum();
        let live_latency: u64 = session.entries.iter().map(|e| e.latency_ms).sum();

        // Merge with stats persisted from prior proxy runs. All four input
        // token buckets (fresh, cache_read, cache_creation) are tracked in
        // HistoricalSummary so the post-restart hit rate matches what the
        // live counter would have shown before the restart.
        let (
            hist_requests,
            hist_fresh_input,
            hist_completion,
            hist_cache_read,
            hist_cache_creation,
            hist_latency,
            hist_saved,
        ) = match historical {
            Some(h) => (
                h.request_count,
                h.prompt_tokens,
                h.completion_tokens,
                h.cache_read_tokens,
                h.cache_creation_tokens,
                h.total_latency_ms,
                h.tokens_saved,
            ),
            None => (0, 0, 0, 0, 0, 0, 0),
        };

        let total_requests = live_requests + hist_requests;
        let total_fresh_input = live_fresh_input + hist_fresh_input;
        let total_completion = live_completion + hist_completion;
        let total_cache_read = live_cache_read + hist_cache_read;
        let total_cache_creation = live_cache_creation + hist_cache_creation;
        let total_latency = live_latency + hist_latency;

        // True prompt = fresh input + cache reads + cache writes.
        let total_prompt = total_fresh_input + total_cache_read + total_cache_creation;
        let total_tokens = total_prompt + total_completion;

        // Cache hit rate as a fraction of the true prompt input. Will be
        // in [0, 100] regardless of how much was cached.
        let api_cache_hit_rate: u64 = if total_prompt > 0 {
            ((total_cache_read as f64 / total_prompt as f64) * 100.0).round() as u64
        } else {
            0
        };

        let mut tokens_saved: i64 = hist_saved;
        let mut tools_tracked: usize = 0;
        let mut pruning_suspended = false;
        if let Some(tx) = tx_opt {
            if let Ok(t) = tx.try_lock() {
                // Transformer tracks run-local savings; add historical on top.
                tokens_saved += t.total_tokens_saved();
                tools_tracked = t.tool_usage_snapshot().len();
                pruning_suspended = t.is_pruning_suspended();
            }
        }

        let last_activity = session.entries.last().map(|e| e.timestamp.to_rfc3339());
        // The dashboard cache countdown prefers `last_request_at` because it
        // fires at the moment we forward upstream — matching Anthropic's own
        // TTL reset point. Falls back to the trace entry timestamp if no
        // live request has been stamped yet this run.
        let last_request_at = slot_last_request_at.map(|t| t.to_rfc3339());

        // Estimated cash value of the cache hits for this session. We don't
        // know the exact model mix per historical row, so we price it against
        // Sonnet 4 as a conservative default — Opus users will see a larger
        // real-world savings and Haiku users a smaller one, but the order of
        // magnitude is right and the display avoids over-promising.
        let cache_savings_usd = cost_calc.cache_savings("claude-sonnet-4-6", total_cache_read);

        sessions.push(serde_json::json!({
            "key": key,
            "project": project.unwrap_or("unknown"),
            "id": session.id,
            "started_at": session.started_at.to_rfc3339(),
            "last_activity": last_activity,
            "last_request_at": last_request_at,
            "request_count": total_requests,
            "live_request_count": live_requests,
            "historical_request_count": hist_requests,
            "total_tokens": total_tokens,
            // prompt_tokens here is the TRUE total (fresh + cached + cache writes)
            // so the dashboard can display sane numbers and compute correct %.
            "prompt_tokens": total_prompt,
            "fresh_input_tokens": total_fresh_input,
            "completion_tokens": total_completion,
            "cache_read_tokens": total_cache_read,
            "cache_creation_tokens": total_cache_creation,
            "total_latency_ms": total_latency,
            "tokens_saved": tokens_saved,
            "cache_savings_usd": cache_savings_usd,
            "tools_tracked": tools_tracked,
            "api_cache_hit_rate": api_cache_hit_rate,
            "pruning_suspended": pruning_suspended,
        }));
    }

    // Sort sessions: most active first
    sessions.sort_by(|a, b| {
        let ra = a["request_count"].as_u64().unwrap_or(0);
        let rb = b["request_count"].as_u64().unwrap_or(0);
        rb.cmp(&ra)
    });

    // Recent activity log
    let activity: Vec<serde_json::Value> = s.activity_log.iter().rev().take(20).map(|a| {
        serde_json::json!({
            "time": a.timestamp.format("%H:%M:%S").to_string(),
            "session": if a.session_key.len() > 16 { &a.session_key[..16] } else { &a.session_key },
            "method": a.method,
            "path": a.path,
            "status": a.status,
            "latency_ms": a.latency_ms,
            "tokens_saved": a.tokens_saved,
        })
    }).collect();

    // Event log
    let events: Vec<serde_json::Value> = s.event_log.iter().rev().take(20).map(|e| {
        let kind_str = match e.kind {
            super::session_store::EventKind::NewSession => "session",
            super::session_store::EventKind::Optimization => "optimize",
            super::session_store::EventKind::Info => "info",
        };
        serde_json::json!({
            "time": e.timestamp.format("%H:%M:%S").to_string(),
            "kind": kind_str,
            "message": e.message,
        })
    }).collect();

    let uptime_secs = (chrono::Utc::now() - s.started_at).num_seconds();

    // Fetch spend summary for today
    let (today_cost, today_saved) = if let Some(ref sl) = spend_log {
        let log = sl.lock().await;
        if let Ok(summary) = log.summary_last_days(1) {
            (summary.total_cost_usd, summary.total_saved_usd)
        } else {
            (0.0, 0.0)
        }
    } else {
        (0.0, 0.0)
    };

    let body = serde_json::json!({
        "status": "running",
        "uptime_secs": uptime_secs,
        "total_requests": s.total_requests,
        "session_count": sessions.len(),
        "today_cost_usd": today_cost,
        "today_saved_usd": today_saved,
        "sessions": sessions,
        "activity": activity,
        "events": events,
    });

    json_response(200, &body)
}

/// Post-process a chat response: record traces, tool usage, cache stats, spend.
async fn process_chat_response(
    store: &SharedSessionStore,
    session_key: &str,
    resp_bytes: &Bytes,
    is_anthropic_native: bool,
    spend_log: &Option<SharedSpendLog>,
    cost_calc: &Arc<CostCalculator>,
    transform_stats: Option<(usize, usize, i64)>,
    latency_ms: u64,
    config: &Arc<ProxyConfig>,
    body_bytes: &Bytes,
    path: &str,
    provider: Provider,
    status_code: u16,
) {
    // Record tool usage and cache stats
    {
        let mut store_guard = store.lock().await;
        let (slot, _) = store_guard.get_or_create(session_key);
        if let Some(ref tx) = slot.transformer {
            let tx = tx.clone();
            drop(store_guard);
            record_tool_usage_from_response(&tx, is_anthropic_native, resp_bytes).await;
            {
                let t = tx.lock().await;
                if t.request_count() >= 5 {
                    let snapshot = t.tool_usage_snapshot();
                    if !snapshot.is_empty() {
                        let mut sg = store.lock().await;
                        sg.contribute_session_tools(session_key, &snapshot);
                    }
                }
            }
            // NB: use anthropic_value_from_bytes here, not from_slice. Anthropic
            // native streaming (Claude Code's default) returns SSE frames that
            // from_slice can't parse at all, so this branch was silently skipped
            // for every streaming request — record_cache_stats never fired, and
            // pruning_suspended stayed at its initial `false`. That let the
            // transformer prune aggressively even when the upstream cache was
            // warm, which breaks the cache prefix and costs net money. Reusing
            // the SSE reassembler means Anthropic streaming now updates the
            // transformer's cache stats the same way a non-streaming response
            // would, and pruning_suspended gets flipped to true whenever the
            // cache hit rate is already healthy.
            if let Some(resp_val) = anthropic_value_from_bytes(resp_bytes) {
                let usage = resp_val.get("usage");
                let cache_read = usage
                    .and_then(|u| u.get("cache_read_input_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let prompt = usage
                    .and_then(|u| u.get("input_tokens").or(u.get("prompt_tokens")))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if prompt > 0 {
                    let mut t = tx.lock().await;
                    t.record_cache_stats(cache_read, prompt);
                }
            }
        }
    }

    // Build trace entry
    let (chat_req_opt, chat_resp_opt) = if is_anthropic_native {
        let req = serde_json::from_slice::<serde_json::Value>(body_bytes)
            .ok()
            .and_then(|v| anthropic_request_to_chat_request(&v));
        let resp = anthropic_value_from_bytes(resp_bytes)
            .and_then(|v| anthropic_response_to_chat_response(&v));
        (req, resp)
    } else {
        (
            serde_json::from_slice::<ChatRequest>(body_bytes).ok(),
            parse_response(resp_bytes).ok(),
        )
    };

    if let (Some(chat_req), Some(chat_resp)) = (chat_req_opt, chat_resp_opt) {
        let entry = TraceEntry::new(provider, chat_req, chat_resp, latency_ms);
        let tokens = entry.total_tokens().unwrap_or(0);

        {
            let mut store_guard = store.lock().await;
            let (slot, _) = store_guard.get_or_create(session_key);
            slot.session.add_entry(entry);

            if let Some(ref dir) = config.live_trace_dir {
                let file_name = format!("session-{}.json", sanitize_key(session_key));
                let path = dir.join(file_name);
                if let Ok(json) = serde_json::to_string(&slot.session) {
                    let _ = std::fs::write(&path, &json);
                }
            }
        }

        let key_display = if session_key.starts_with("sys-") {
            format!("session:{}", &session_key[4..12.min(session_key.len())])
        } else if session_key.len() > 16 {
            format!("{}...", &session_key[..16])
        } else {
            session_key.to_string()
        };
        let req_num = {
            let s = store.lock().await;
            s.get_session(session_key).map(|s| s.entries.len()).unwrap_or(0)
        };
        if let Some((pruned, merged, saved)) = transform_stats {
            let mut parts = Vec::new();
            if pruned > 0 { parts.push(format!("-{} tools", pruned)); }
            if merged > 0 { parts.push(format!("-{} msgs", merged)); }
            parts.push(format!("~{} tokens saved", saved));
            let opt_summary = parts.join(", ");
            info!("[{}] #{} | {} tokens, {}ms | optimized: {}",
                key_display, req_num, tokens, latency_ms, opt_summary);
            {
                let mut s = store.lock().await;
                s.log_event(
                    super::session_store::EventKind::Optimization,
                    format!("[{}] #{}: {}", key_display, req_num, opt_summary),
                );
            }
        } else {
            info!("[{}] #{} | {} tokens, {}ms", key_display, req_num, tokens, latency_ms);
        }
    } else if let Some((pruned, merged, saved)) = transform_stats {
        info!("[{}][proxy] {}ms | optimized: -{} tools, -{} msgs, ~{} tokens saved",
            session_key, latency_ms, pruned, merged, saved);
    }

    // Spend logging
    if let Some(ref sl) = spend_log {
        let tokens_saved = transform_stats.map(|(_, _, s)| s).unwrap_or(0);
        let (model_name, prompt_tok, completion_tok, cache_read_tok, cache_creation_tok, tools_json) =
            extract_usage_for_spend(resp_bytes, is_anthropic_native);
        let cost_result = cost_calc.calculate(
            &model_name, prompt_tok, completion_tok,
            cache_read_tok, cache_creation_tok, tokens_saved,
        );
        // Look up the slot's project_path (set when extract_project_path
        // succeeded during request routing) so recent_sessions can group
        // historical entries by project after a proxy restart.
        let project_path = {
            let s = store.lock().await;
            s.all_slots()
                .iter()
                .find(|sl| sl.key == session_key)
                .and_then(|sl| sl.project_path.map(String::from))
                .unwrap_or_default()
        };
        let entry = SpendEntry {
            request_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_key: session_key.to_string(),
            project_path,
            model: model_name,
            prompt_tokens: prompt_tok,
            completion_tokens: completion_tok,
            cache_read_tokens: cache_read_tok,
            cache_creation_tokens: cache_creation_tok,
            cost_usd: cost_result.cost_usd,
            cost_saved_usd: cost_result.cost_saved_usd,
            tokens_saved,
            latency_ms,
            tools_called: tools_json,
            status: status_code,
        };
        let sl = sl.clone();
        tokio::spawn(async move {
            let log = sl.lock().await;
            if let Err(e) = log.log(&entry) {
                tracing::warn!("spend log write failed: {}", e);
            }
        });
    }

    // Log activity
    {
        let saved = transform_stats.map(|(_, _, s)| s);
        let mut store_guard = store.lock().await;
        store_guard.log_activity(super::session_store::ActivityEntry {
            timestamp: chrono::Utc::now(),
            session_key: session_key.to_string(),
            path: path.to_string(),
            method: "POST".to_string(),
            status: status_code,
            tokens: None,
            tokens_saved: saved,
            latency_ms,
        });
    }
}

/// Record tool usage from the API response into the transformer.
async fn record_tool_usage_from_response(
    tx: &super::transformer::SharedTransformer,
    is_anthropic_native: bool,
    resp_bytes: &[u8],
) {
    if is_anthropic_native {
        if let Ok(resp_val) = serde_json::from_slice::<serde_json::Value>(resp_bytes) {
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
        if let Ok(chat_resp) = parse_response(resp_bytes) {
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

/// Sanitize a session key for use as a filename.
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Emit a warning log when the upstream returns a non-2xx response for a chat
/// request. Includes the target URL, status code, and a trimmed snippet of the
/// error body so users can diagnose routing / auth / endpoint problems.
fn log_upstream_error(target_url: &str, status_code: u16, body: &[u8]) {
    const MAX_SNIPPET: usize = 300;
    let snippet = String::from_utf8_lossy(body);
    let snippet = snippet.trim();
    let snippet = if snippet.chars().count() > MAX_SNIPPET {
        let truncated: String = snippet.chars().take(MAX_SNIPPET).collect();
        format!("{}… ({} bytes total)", truncated, body.len())
    } else {
        snippet.to_string()
    };
    tracing::warn!(
        "upstream chat error: status={} url={} body={}",
        status_code,
        target_url,
        snippet
    );
}

/// Parse response bytes as ChatResponse, handling both regular JSON and SSE streaming formats.
fn parse_response(bytes: &[u8]) -> Result<ChatResponse, String> {
    if let Ok(resp) = serde_json::from_slice::<ChatResponse>(bytes) {
        return Ok(resp);
    }

    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return chatresponse_from_value(&val);
    }

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

    merge_sse_chunks(&chunks)
}

fn chatresponse_from_value(val: &serde_json::Value) -> Result<ChatResponse, String> {
    let id = val.get("id").and_then(|v| v.as_str()).map(String::from);
    let model = val.get("model").and_then(|v| v.as_str()).map(String::from);

    let choices = if let Some(arr) = val.get("choices").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|c| serde_json::from_value::<Choice>(c.clone()).ok()).collect()
    } else {
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

    let last = &chunks[chunks.len() - 1];
    let first = &chunks[0];

    let id = first.get("id").and_then(|v| v.as_str()).map(String::from);
    let model = first.get("model").and_then(|v| v.as_str()).map(String::from);

    let mut full_content = String::new();
    let mut tool_calls_map: std::collections::BTreeMap<u32, (String, String, String)> = std::collections::BTreeMap::new();
    let mut finish_reason = None;

    for chunk in chunks {
        if let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        full_content.push_str(content);
                    }
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

fn extract_write_path_from_value(val: Option<&serde_json::Value>) -> Option<String> {
    let v = val?;
    for key in &["filePath", "file_path", "path", "filename", "file"] {
        if let Some(p) = v.get(key).and_then(|v| v.as_str()) {
            return Some(p.to_string());
        }
    }
    None
}

fn anthropic_request_to_chat_request(val: &serde_json::Value) -> Option<ChatRequest> {
    let model = val.get("model").and_then(|v| v.as_str()).map(String::from);

    let mut messages = Vec::new();

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

/// Extract usage metrics from a response for spend logging.
/// Returns (model, prompt_tokens, completion_tokens, cache_read, cache_creation, tools_json).
fn extract_usage_for_spend(
    resp_bytes: &[u8],
    is_anthropic: bool,
) -> (String, u64, u64, u64, u64, String) {
    let val: serde_json::Value = match serde_json::from_slice(resp_bytes) {
        Ok(v) => v,
        Err(_) => {
            // Try parsing as SSE stream: extract data from event lines
            return extract_usage_from_sse(resp_bytes, is_anthropic);
        }
    };

    let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let usage = val.get("usage");

    let (prompt, completion, cache_read, cache_creation) = if is_anthropic {
        let p = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        let c = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        let cr = usage.and_then(|u| u.get("cache_read_input_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        let cc = usage.and_then(|u| u.get("cache_creation_input_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        (p, c, cr, cc)
    } else {
        let p = usage.and_then(|u| u.get("prompt_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        let c = usage.and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
        let cr = usage.and_then(|u| u.get("prompt_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        (p, c, cr, 0)
    };

    // Extract tool names from response
    let mut tool_names: Vec<String> = Vec::new();
    if is_anthropic {
        if let Some(content) = val.get("content").and_then(|v| v.as_array()) {
            for block in content {
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                        tool_names.push(name.to_string());
                    }
                }
            }
        }
    } else if let Some(choices) = val.get("choices").and_then(|v| v.as_array()) {
        for choice in choices {
            if let Some(tcs) = choice.pointer("/message/tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    if let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) {
                        tool_names.push(name.to_string());
                    }
                }
            }
        }
    }
    let tools_json = serde_json::to_string(&tool_names).unwrap_or_else(|_| "[]".to_string());

    (model, prompt, completion, cache_read, cache_creation, tools_json)
}

/// Extract usage from SSE event stream (for spend tracking of streaming responses).
fn extract_usage_from_sse(
    resp_bytes: &[u8],
    _is_anthropic: bool,
) -> (String, u64, u64, u64, u64, String) {
    let text = String::from_utf8_lossy(resp_bytes);
    let mut model = String::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cache_read: u64 = 0;
    let mut cache_create: u64 = 0;
    let mut tool_names: Vec<String> = Vec::new();

    for line in text.lines() {
        let data = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"));
        if let Some(data) = data {
            let data = data.trim();
            if data == "[DONE]" || data.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                // message_start contains model and initial usage
                if v.get("type").and_then(|t| t.as_str()) == Some("message_start") {
                    if let Some(msg) = v.get("message") {
                        if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                            model = m.to_string();
                        }
                        if let Some(u) = msg.get("usage") {
                            input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            cache_read = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            cache_create = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        }
                    }
                }
                // message_delta contains output usage
                if v.get("type").and_then(|t| t.as_str()) == Some("message_delta") {
                    if let Some(u) = v.get("usage") {
                        output_tokens = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(output_tokens);
                    }
                }
                // content_block_start with tool_use
                if v.get("type").and_then(|t| t.as_str()) == Some("content_block_start") {
                    if let Some(block) = v.get("content_block") {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                                tool_names.push(name.to_string());
                            }
                        }
                    }
                }
                // OpenAI SSE: model in first chunk, usage in last
                if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
                    if model.is_empty() { model = m.to_string(); }
                }
                if let Some(u) = v.get("usage") {
                    if let Some(p) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        input_tokens = p;
                    }
                    if let Some(c) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = c;
                    }
                }
            }
        }
    }

    let tools_json = serde_json::to_string(&tool_names).unwrap_or_else(|_| "[]".to_string());
    (model, input_tokens, output_tokens, cache_read, cache_create, tools_json)
}

/// Parse a (possibly SSE-framed) Anthropic response body into a single JSON value
/// shaped like a non-streaming message response.
///
/// Anthropic native streaming emits `message_start`, `content_block_start`,
/// `content_block_delta` (text_delta / input_json_delta), `content_block_stop`,
/// `message_delta` (with stop_reason + output usage) and `message_stop` events.
/// We walk those events once and reassemble a synthetic message object that
/// `anthropic_response_to_chat_response` and `TraceEntry::new` can consume.
fn anthropic_value_from_bytes(bytes: &[u8]) -> Option<serde_json::Value> {
    // Plain JSON response — use directly.
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
        return Some(v);
    }

    let text = String::from_utf8_lossy(bytes);
    let mut id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cache_read: Option<u64> = None;
    let mut cache_creation: Option<u64> = None;
    let mut stop_reason: Option<String> = None;
    // Content blocks keyed by their index so we can merge deltas in order.
    let mut blocks: std::collections::BTreeMap<u64, serde_json::Value> =
        std::collections::BTreeMap::new();
    // Partial tool_use input_json_delta fragments keyed by block index.
    let mut tool_input_buf: std::collections::BTreeMap<u64, String> =
        std::collections::BTreeMap::new();

    for line in text.lines() {
        let data = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:"));
        let Some(data) = data else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let ev_type = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match ev_type {
            "message_start" => {
                if let Some(msg) = ev.get("message") {
                    if id.is_none() {
                        id = msg.get("id").and_then(|v| v.as_str()).map(String::from);
                    }
                    if model.is_none() {
                        model = msg.get("model").and_then(|v| v.as_str()).map(String::from);
                    }
                    if let Some(u) = msg.get("usage") {
                        if let Some(p) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                            input_tokens = p;
                        }
                        if let Some(o) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                            output_tokens = o;
                        }
                        if let Some(c) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                            cache_read = Some(c);
                        }
                        if let Some(c) = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()) {
                            cache_creation = Some(c);
                        }
                    }
                }
            }
            "content_block_start" => {
                let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(cb) = ev.get("content_block").cloned() {
                    blocks.insert(idx, cb);
                }
            }
            "content_block_delta" => {
                let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(delta) = ev.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = delta.get("text").and_then(|v| v.as_str()) {
                                let block = blocks.entry(idx).or_insert_with(|| {
                                    serde_json::json!({"type": "text", "text": ""})
                                });
                                let existing = block
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                block["text"] = serde_json::Value::String(existing + t);
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|v| v.as_str())
                            {
                                tool_input_buf.entry(idx).or_default().push_str(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(u) = ev.get("usage") {
                    if let Some(o) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        output_tokens = o;
                    }
                }
                if let Some(d) = ev.get("delta") {
                    if let Some(sr) = d.get("stop_reason").and_then(|v| v.as_str()) {
                        stop_reason = Some(sr.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // Attach accumulated tool_use arguments to their blocks.
    for (idx, buf) in tool_input_buf {
        if let Some(block) = blocks.get_mut(&idx) {
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                let parsed: serde_json::Value =
                    serde_json::from_str(&buf).unwrap_or(serde_json::json!({}));
                block["input"] = parsed;
            }
        }
    }

    let content: Vec<serde_json::Value> = blocks.into_values().collect();
    if content.is_empty() && input_tokens == 0 && output_tokens == 0 {
        return None;
    }

    let mut usage = serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
    });
    if let Some(c) = cache_read {
        usage["cache_read_input_tokens"] = serde_json::json!(c);
    }
    if let Some(c) = cache_creation {
        usage["cache_creation_input_tokens"] = serde_json::json!(c);
    }

    let mut obj = serde_json::json!({
        "content": content,
        "usage": usage,
    });
    if let Some(i) = id {
        obj["id"] = serde_json::Value::String(i);
    }
    if let Some(m) = model {
        obj["model"] = serde_json::Value::String(m);
    }
    if let Some(s) = stop_reason {
        obj["stop_reason"] = serde_json::Value::String(s);
    }
    Some(obj)
}

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
