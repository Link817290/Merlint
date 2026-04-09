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

use crate::models::api::{ChatRequest, ChatResponse};
use crate::models::trace::{Provider, TraceEntry, TraceSession};

pub struct ProxyConfig {
    pub listen_port: u16,
    pub target_url: String,
    pub api_key: Option<String>,
    /// When set, append each trace entry to this file as it arrives
    pub live_trace_file: Option<PathBuf>,
}

pub async fn run_proxy(config: ProxyConfig, session: Arc<Mutex<TraceSession>>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{}", config.listen_port);
    let listener = TcpListener::bind(&addr).await?;
    info!("agentbench proxy listening on {}", addr);
    info!("forwarding to {}", config.target_url);

    let config = Arc::new(config);

    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let config = config.clone();
        let session = session.clone();

        tokio::spawn(async move {
            let config = config.clone();
            let session = session.clone();
            let service = service_fn(move |req| {
                handle_request(req, config.clone(), session.clone())
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
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    let body_bytes = req.collect().await?.to_bytes();

    let provider = detect_provider(&path, &headers);
    let is_chat = path.contains("/chat/completions") || path.contains("/messages");

    let target_url = format!("{}{}", config.target_url.trim_end_matches('/'), path);

    let client = reqwest::Client::new();
    let mut forward_req = client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST),
        &target_url,
    );

    for (key, value) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str == "host" || key_str == "content-length" || key_str == "transfer-encoding" {
            continue;
        }
        if let Ok(v) = value.to_str() {
            forward_req = forward_req.header(key.as_str(), v);
        }
    }

    if let Some(ref api_key) = config.api_key {
        forward_req = forward_req.header("Authorization", format!("Bearer {}", api_key));
    }

    forward_req = forward_req.body(body_bytes.to_vec());

    let start = Instant::now();
    let response = match forward_req.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("upstream error: {}", e);
            let err_body = serde_json::json!({
                "error": { "message": format!("agentbench proxy error: {}", e) }
            });
            return Ok(Response::builder()
                .status(502)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(serde_json::to_vec(&err_body).unwrap())))
                .unwrap());
        }
    };
    let latency_ms = start.elapsed().as_millis() as u64;

    let status = response.status();
    let resp_headers = response.headers().clone();
    let resp_bytes = response.bytes().await.unwrap_or_default();

    // Record trace if this is a chat completion
    if is_chat && status.is_success() {
        if let (Ok(chat_req), Ok(chat_resp)) = (
            serde_json::from_slice::<ChatRequest>(&body_bytes),
            serde_json::from_slice::<ChatResponse>(&resp_bytes),
        ) {
            let entry = TraceEntry::new(provider, chat_req, chat_resp, latency_ms);
            let entry_id = entry.id.clone();
            let tokens = entry.total_tokens().unwrap_or(0);

            // Live write to file if configured
            if let Some(ref path) = config.live_trace_file {
                let mut sess = session.lock().await;
                sess.add_entry(entry);
                // Overwrite with full session each time (atomic for query reads)
                if let Ok(json) = serde_json::to_string(&*sess) {
                    let _ = std::fs::write(path, &json);
                }
            } else {
                session.lock().await.add_entry(entry);
            }

            info!(
                "[trace {}] {} tokens, {}ms",
                &entry_id[..8],
                tokens,
                latency_ms
            );
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

    Ok(resp_builder
        .body(Full::new(resp_bytes))
        .unwrap())
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
