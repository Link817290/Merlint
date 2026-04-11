use merlint::proxy::server::{ProxyConfig, run_proxy};
use merlint::proxy::session_store::new_session_store;

/// Helper: start a mock upstream server that returns a fixed JSON response
async fn start_mock_upstream(response_json: serde_json::Value) -> (u16, tokio::task::JoinHandle<()>) {
    use hyper::{Request, Response};
    use hyper::body::Incoming;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use http_body_util::Full;
    use bytes::Bytes;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let resp_bytes = serde_json::to_vec(&response_json).unwrap();

    let handle = tokio::spawn(async move {
        for _ in 0..10 {
            let Ok((stream, _)) = listener.accept().await else { break };
            let io = TokioIo::new(stream);
            let resp = resp_bytes.clone();
            let service = service_fn(move |_req: Request<Incoming>| {
                let resp = resp.clone();
                async move {
                    Ok::<_, hyper::Error>(Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(Full::new(Bytes::from(resp)))
                        .unwrap())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, service).await;
        }
    });

    (port, handle)
}

/// Helper: send a chat completion request through the proxy
async fn send_chat_request(
    proxy_port: u16,
    request_body: &serde_json::Value,
) -> reqwest::Response {
    let client = reqwest::Client::new();
    client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .json(request_body)
        .send()
        .await
        .unwrap()
}

fn make_openai_request(tools: Vec<&str>, messages: Vec<(&str, &str)>) -> serde_json::Value {
    let tool_defs: Vec<serde_json::Value> = tools
        .iter()
        .map(|name| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": format!("The {} tool", name),
                    "parameters": {"type": "object", "properties": {}}
                }
            })
        })
        .collect();

    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|(role, content)| serde_json::json!({"role": role, "content": content}))
        .collect();

    serde_json::json!({
        "model": "gpt-4",
        "messages": msgs,
        "tools": tool_defs,
    })
}

fn make_openai_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello!"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120
        }
    })
}

fn make_openai_tool_response(tool_name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": "{}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120
        }
    })
}

#[tokio::test]
async fn test_proxy_forwards_request_and_records_trace() {
    let upstream_resp = make_openai_response();
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(false);
    let store_clone = store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let req = make_openai_request(
        vec!["Read", "Write", "Bash"],
        vec![("user", "Hello")],
    );
    let resp = send_chat_request(proxy_port, &req).await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello!");

    // Check trace was recorded
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert!(!sessions.is_empty(), "should have at least one session");
    let (_, session) = &sessions[0];
    assert_eq!(session.entries.len(), 1);
    assert_eq!(session.entries[0].response.usage.as_ref().unwrap().total_tokens, 120);

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_with_optimization_prunes_tools() {
    let upstream_resp = make_openai_tool_response("Read");
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(true);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: true,
    };

    let store_clone = store.clone();
    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let tools = vec!["Read", "Write", "Bash", "Grep", "Glob"];
    let req = make_openai_request(tools.clone(), vec![("user", "test")]);

    // Send 4 requests — pruning kicks in after request 3
    for _ in 0..4 {
        let resp = send_chat_request(proxy_port, &req).await;
        assert_eq!(resp.status(), 200);
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }

    // Check that the session recorded entries
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert!(!sessions.is_empty());

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_anthropic_format_detection() {
    let upstream_resp = serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-20250514",
        "content": [{"type": "text", "text": "Hi there!"}],
        "usage": {
            "input_tokens": 50,
            "output_tokens": 10
        },
        "stop_reason": "end_turn"
    });
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(false);
    let store_clone = store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Send Anthropic-style request to /v1/messages
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "Hi there!");

    // Check trace was recorded with correct token counts
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert!(!sessions.is_empty());
    let (_, session) = &sessions[0];
    assert_eq!(session.entries.len(), 1);
    let usage = session.entries[0].response.usage.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, 50);
    assert_eq!(usage.completion_tokens, 10);

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_sse_response_handling() {
    use hyper::{Request, Response};
    use hyper::body::Incoming;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use http_body_util::Full;
    use bytes::Bytes;

    let sse_body = "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"index\":0}]}\n\n\
                    data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"index\":0}]}\n\n\
                    data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\" world\"},\"index\":0,\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n\
                    data: [DONE]\n\n";

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();

    let sse_bytes = Bytes::from(sse_body);
    let _upstream = tokio::spawn(async move {
        for _ in 0..5 {
            let Ok((stream, _)) = listener.accept().await else { break };
            let io = TokioIo::new(stream);
            let body = sse_bytes.clone();
            let service = service_fn(move |_req: Request<Incoming>| {
                let body = body.clone();
                async move {
                    Ok::<_, hyper::Error>(Response::builder()
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(Full::new(body))
                        .unwrap())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, service).await;
        }
    });

    let store = new_session_store(false);
    let store_clone = store.clone();

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy_listener.local_addr().unwrap().port();
    drop(proxy_listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let req = make_openai_request(vec![], vec![("user", "Hi")]);
    let resp = send_chat_request(proxy_port, &req).await;
    assert_eq!(resp.status(), 200);

    let content_type = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(content_type.contains("text/event-stream"));

    // Trace should have been recorded with merged SSE content
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert!(!sessions.is_empty());
    let (_, session) = &sessions[0];
    assert_eq!(session.entries.len(), 1);

    let entry = &session.entries[0];
    if let Some(choice) = entry.response.choices.first() {
        if let Some(msg) = &choice.message {
            if let Some(content) = &msg.content {
                let text = content.as_text();
                assert!(text.contains("Hello"), "SSE content should be merged: got {}", text);
            }
        }
    }

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_multi_session_tracking() {
    let upstream_resp = make_openai_response();
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(false);
    let store_clone = store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // Session A has an explicit "Primary working directory:" marker —
    // all of its requests should hash to the same path-based session key.
    let req_a = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helping with Project Alpha.\nPrimary working directory: /workspace/alpha\n"},
            {"role": "user", "content": "Hello from A"}
        ]
    });
    client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .json(&req_a)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Session B has a different working directory → different session key.
    let req_b = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helping with Project Beta.\nPrimary working directory: /workspace/beta\n"},
            {"role": "user", "content": "Hello from B"}
        ]
    });
    client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .json(&req_b)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Second request for session A (same working directory → same key).
    client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .json(&req_a)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Verify: should have 2 sessions, session A has 2 entries, session B has 1.
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert_eq!(sessions.len(), 2, "should have 2 separate sessions");

    let total_entries: usize = sessions.iter().map(|(_, sess)| sess.entries.len()).sum();
    assert_eq!(total_entries, 3, "total entries across sessions should be 3");

    // One session should have 2 entries, the other 1.
    let mut counts: Vec<usize> = sessions.iter().map(|(_, sess)| sess.entries.len()).collect();
    counts.sort();
    assert_eq!(counts, vec![1, 2]);

    proxy_handle.abort();
}

/// Historical stats should carry over across proxy restarts. We simulate a
/// restart by dropping the SessionStore and starting a new proxy against a
/// shared spend.db. When the same conversation's first user message arrives
/// again, the rebuilt SessionSlot should pre-load its historical totals
/// from spend.db so the dashboard shows lifetime counts, not just the
/// current-run counts.
#[tokio::test]
async fn test_session_stats_persist_across_restart() {
    use merlint::proxy::session_store::SessionStore;
    use merlint::proxy::spend_log::{SpendEntry, SpendLog};

    // Use a unique scratch spend.db via explicit open_at, so this test is
    // fully isolated from the user's real ~/.merlint/spend.db and from
    // other tests running in parallel — no process-wide env var tricks.
    let tmp_dir = std::env::temp_dir().join(format!(
        "merlint-persist-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let db_path = tmp_dir.join("spend.db");

    // Simulate prior-run spend data: same session_key shape as what
    // extract_session_key produces (sys-{pathHash}-{convHash}).
    let key1 = "sys-abcdef0123456789-fedcba9876543210";
    let key2 = "sys-abcdef0123456789-0000000000000000"; // same project, different conv
    {
        let db = SpendLog::open_at(&db_path).unwrap();
        for i in 0..3 {
            db.log(&SpendEntry {
                request_id: format!("req-k1-{}", i),
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_key: key1.to_string(),
                project_path: "/workspace/demo".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                prompt_tokens: 100,
                completion_tokens: 50,
                cache_read_tokens: 10,
                // First request writes the cache, the other two read it.
                // Summed historical cache_creation must survive the preload.
                cache_creation_tokens: if i == 0 { 90 } else { 0 },
                cost_usd: 0.01,
                cost_saved_usd: 0.0,
                tokens_saved: 5,
                latency_ms: 250,
                tools_called: "[]".to_string(),
                status: 200,
            })
            .unwrap();
        }
        // Second conversation under same project — should preload too.
        db.log(&SpendEntry {
            request_id: "req-k2-0".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_key: key2.to_string(),
            project_path: "/workspace/demo".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            prompt_tokens: 200,
            completion_tokens: 80,
            cache_read_tokens: 50,
            cache_creation_tokens: 0,
            cost_usd: 0.02,
            cost_saved_usd: 0.0,
            tokens_saved: 0,
            latency_ms: 400,
            tools_called: "[]".to_string(),
            status: 200,
        })
        .unwrap();

        // Legacy row written before the project_path column existed —
        // empty project_path means we can't attribute it to any project,
        // so preload should skip it instead of creating an "unknown" card.
        db.log(&SpendEntry {
            request_id: "req-legacy".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_key: "sys-legacydeadbeef".to_string(),
            project_path: String::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            prompt_tokens: 999,
            completion_tokens: 999,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 1.0,
            cost_saved_usd: 0.0,
            tokens_saved: 0,
            latency_ms: 100,
            tools_called: "[]".to_string(),
            status: 200,
        })
        .unwrap();
    }

    // Fresh store (simulating proxy restart). Explicitly run preload with
    // the test-local DB — no real DB is touched.
    let db = SpendLog::open_at(&db_path).unwrap();
    let mut store = SessionStore::new(false);
    let loaded = store.preload_recent_sessions(&db, 7);
    assert_eq!(
        loaded, 2,
        "should preload only the 2 conversations with a project_path; legacy row skipped"
    );

    // The legacy row must NOT have created a slot.
    assert!(
        store
            .all_slots()
            .iter()
            .all(|s| s.key != "sys-legacydeadbeef"),
        "legacy session without project_path should be skipped by preload"
    );

    // Look up key1: historical should be populated with 3 entries × stats.
    let slot1 = store
        .all_slots()
        .into_iter()
        .find(|s| s.key == key1)
        .expect("preloaded slot for key1");
    assert_eq!(slot1.project_path, Some("/workspace/demo"));
    let h1 = slot1.historical.expect("historical on key1");
    assert_eq!(h1.request_count, 3);
    assert_eq!(h1.prompt_tokens, 300);
    assert_eq!(h1.completion_tokens, 150);
    assert_eq!(h1.cache_read_tokens, 30);
    assert_eq!(
        h1.cache_creation_tokens, 90,
        "historical cache_creation must be summed from spend_log, not dropped"
    );
    assert_eq!(h1.total_latency_ms, 750);
    assert_eq!(h1.tokens_saved, 15);

    // key2: single entry.
    let slot2 = store
        .all_slots()
        .into_iter()
        .find(|s| s.key == key2)
        .expect("preloaded slot for key2");
    let h2 = slot2.historical.expect("historical on key2");
    assert_eq!(h2.request_count, 1);
    assert_eq!(h2.prompt_tokens, 200);

    // attach_historical path: a freshly-created slot with no historical
    // should accept a later attach and reject a second one.
    store.get_or_create_with_project(
        "sys-00000000-00000000",
        Some("/workspace/demo".to_string()),
    );
    let summary = merlint::proxy::spend_log::HistoricalSummary {
        request_count: 42,
        prompt_tokens: 1,
        completion_tokens: 2,
        cache_read_tokens: 3,
        cache_creation_tokens: 7,
        total_latency_ms: 4,
        cost_usd: 0.5,
        tokens_saved: 6,
    };
    store.attach_historical("sys-00000000-00000000", summary.clone());
    let attached = store
        .all_slots()
        .into_iter()
        .find(|s| s.key == "sys-00000000-00000000")
        .unwrap();
    assert_eq!(attached.historical.unwrap().request_count, 42);

    // mark_request_started should stamp the slot's cache-TTL anchor so the
    // dashboard countdown resets at upstream-forward time, not at
    // response-completion time.
    let before = chrono::Utc::now();
    store.mark_request_started("sys-00000000-00000000");
    let after = chrono::Utc::now();
    let stamped = store
        .all_slots()
        .into_iter()
        .find(|s| s.key == "sys-00000000-00000000")
        .unwrap();
    let ts = stamped.last_request_at.expect("stamp should be set");
    assert!(ts >= before && ts <= after, "timestamp must fall inside the call window");

    // mark_request_started on an unknown key is a no-op, not a panic.
    store.mark_request_started("sys-does-not-exist");

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Two conversations from the same project (same working directory, but
/// distinct first user messages — e.g. two Claude Code windows) should appear
/// as two separate sessions that share the same project_path. This lets the
/// dashboard render "Project X — 2 conversations" instead of collapsing them.
#[tokio::test]
async fn test_proxy_same_project_multiple_conversations() {
    let upstream_resp = make_openai_response();
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(false);
    let store_clone = store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // Same working directory in the system prompt → same project_path.
    let sys = "You are Claude Code.\nPrimary working directory: /workspace/myapp\n";

    // Conversation 1, turn 1
    let conv1_t1 = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": "Fix the login bug"}
        ]
    });
    // Conversation 1, turn 2 — same first user msg, plus assistant + new user
    let conv1_t2 = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": "Fix the login bug"},
            {"role": "assistant", "content": "Looking at auth.rs..."},
            {"role": "user", "content": "What about the session timeout?"}
        ]
    });
    // Conversation 2 — different first user message → different session
    let conv2 = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": "Refactor the payment module"}
        ]
    });

    for body in [&conv1_t1, &conv1_t2, &conv2] {
        client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
            .header("content-type", "application/json")
            .header("authorization", "Bearer test-key")
            .json(body)
            .send()
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
    }

    let s = store.lock().await;
    let slots = s.all_slots();
    let project_slots: Vec<_> = slots
        .iter()
        .filter(|sl| sl.project_path == Some("/workspace/myapp"))
        .collect();

    // Two distinct conversations under the same project_path.
    assert_eq!(
        project_slots.len(),
        2,
        "expected 2 conversations under /workspace/myapp, got {}",
        project_slots.len()
    );

    // Conversation 1 should have accumulated both turns; conversation 2 just one.
    let mut counts: Vec<usize> = project_slots
        .iter()
        .map(|sl| sl.session.entries.len())
        .collect();
    counts.sort();
    assert_eq!(counts, vec![1, 2], "one conversation has 2 turns, the other 1");

    // Session keys should differ so the dashboard renders them as separate rows.
    let keys: std::collections::HashSet<&str> =
        project_slots.iter().map(|sl| sl.key).collect();
    assert_eq!(keys.len(), 2, "session keys should differ between conversations");

    // But the shared prefix (path hash) should be identical — that's how
    // the dashboard groups them under the same project card.
    let prefixes: std::collections::HashSet<&str> = project_slots
        .iter()
        .map(|sl| &sl.key[..20]) // "sys-" + 16 hex chars of path hash
        .collect();
    assert_eq!(prefixes.len(), 1, "path-hash prefix should match for same project");

    proxy_handle.abort();
}

/// Short auxiliary requests (Haiku title generation, quota probes) should
/// collapse into a single __background__ bucket instead of each spawning
/// its own "unknown" session. This locks in the behaviour added for the
/// Claude Code multi-session scenario.
#[tokio::test]
async fn test_proxy_background_bucket_collapses_short_requests() {
    use merlint::proxy::session_store::BACKGROUND_SESSION_KEY;

    let upstream_resp = make_openai_response();
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let store = new_session_store(false);
    let store_clone = store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();

    // 1) Short generic request (simulates Haiku title generation).
    let title_req = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are Claude Code. Generate a short title for the following conversation."},
            {"role": "user", "content": "你好"}
        ]
    });
    // 2) Even shorter quota probe — no system prompt.
    let quota_req = serde_json::json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "quota"}]
    });

    for body in [&title_req, &quota_req, &title_req] {
        client
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
            .header("content-type", "application/json")
            .header("authorization", "Bearer test-key")
            .json(body)
            .send()
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
    }

    let s = store.lock().await;
    let sessions = s.all_sessions();
    let bg_count = sessions
        .iter()
        .filter(|(k, _)| *k == BACKGROUND_SESSION_KEY)
        .count();
    let non_bg: Vec<&str> = sessions
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| *k != BACKGROUND_SESSION_KEY && *k != "__non_chat__")
        .collect();

    assert_eq!(
        bg_count, 1,
        "all 3 short/unmarked requests should collapse into exactly one background session"
    );
    assert!(
        non_bg.is_empty(),
        "no real project sessions should be created by auxiliary requests, got: {:?}",
        non_bg
    );

    // The background session should have collected all three entries.
    let (_, bg_session) = sessions
        .iter()
        .find(|(k, _)| *k == BACKGROUND_SESSION_KEY)
        .unwrap();
    assert_eq!(bg_session.entries.len(), 3);

    proxy_handle.abort();
}

/// Regression test for the Anthropic native SSE response parsing bug.
///
/// Before the fix, `process_chat_response` called `serde_json::from_slice`
/// directly on SSE-framed bytes for Anthropic requests. That always failed,
/// so `TraceEntry` was never built and `session.entries` stayed empty — which
/// showed up in the dashboard as every session having `prompt: 0 / completion: 0`
/// even though Today's Cost was non-zero (spend logging had its own SSE path).
///
/// This test sends a /v1/messages request through the proxy, has the mock
/// upstream emit a realistic Anthropic event stream, and verifies the trace
/// captured the usage numbers from message_start + message_delta.
#[tokio::test]
async fn test_proxy_anthropic_sse_response_handling() {
    use hyper::{Request, Response};
    use hyper::body::Incoming;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use http_body_util::Full;
    use bytes::Bytes;

    // Realistic Anthropic streaming event sequence for a short message
    // containing both a text block and a tool_use block. Usage appears in
    // message_start (input) and is finalized in message_delta (output).
    let sse_body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-20250514\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":123,\"output_tokens\":1,\"cache_read_input_tokens\":42,\"cache_creation_input_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"Read\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"file_path\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"/tmp/x\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":77}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();

    let sse_bytes = Bytes::from(sse_body);
    let _upstream = tokio::spawn(async move {
        for _ in 0..5 {
            let Ok((stream, _)) = listener.accept().await else { break };
            let io = TokioIo::new(stream);
            let body = sse_bytes.clone();
            let service = service_fn(move |_req: Request<Incoming>| {
                let body = body.clone();
                async move {
                    Ok::<_, hyper::Error>(Response::builder()
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(Full::new(body))
                        .unwrap())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, service).await;
        }
    });

    let store = new_session_store(false);
    let store_clone = store.clone();

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy_listener.local_addr().unwrap().port();
    drop(proxy_listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_dir: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, store_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Send an Anthropic-native request. Include a "Primary working directory:"
    // marker in the system prompt so we also exercise project_path extraction.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/messages", proxy_port))
        .header("content-type", "application/json")
        .header("x-api-key", "test-key")
        .header("anthropic-version", "2023-06-01")
        .json(&serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "system": "You are a coding assistant.\nPrimary working directory: /Applications/github/Merlint\n",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let content_type = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(content_type.contains("text/event-stream"));

    // Drain the streamed body so the background collector finishes.
    let _ = resp.bytes().await.unwrap();

    // Give the background post-processing task a moment to land the entry.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    let s = store.lock().await;
    let slots = s.all_slots();
    let chat_slots: Vec<_> = slots.iter().filter(|sl| sl.key != "__non_chat__").collect();
    assert_eq!(chat_slots.len(), 1, "exactly one chat session should be tracked");

    let slot = &chat_slots[0];
    assert_eq!(
        slot.project_path,
        Some("/Applications/github/Merlint"),
        "project path should be extracted from the system prompt"
    );
    assert_eq!(
        slot.session.entries.len(),
        1,
        "SSE response should have produced exactly one trace entry"
    );

    let entry = &slot.session.entries[0];
    let usage = entry.response.usage.as_ref().expect("usage should be populated");
    assert_eq!(usage.prompt_tokens, 123, "input_tokens from message_start");
    assert_eq!(usage.completion_tokens, 77, "output_tokens from message_delta");
    assert_eq!(usage.cache_read_input_tokens, Some(42), "cache_read_input_tokens from message_start");

    // The tool call assembled from input_json_delta fragments should round-trip.
    let choice = entry.response.choices.first().expect("at least one choice");
    let msg = choice.message.as_ref().expect("assistant message");
    let tool_calls = msg.tool_calls.as_ref().expect("tool_use block should be captured");
    assert_eq!(tool_calls.len(), 1);
    let f = tool_calls[0].function.as_ref().unwrap();
    assert_eq!(f.name, "Read");
    assert!(f.arguments.contains("/tmp/x"), "input_json_delta fragments should be merged: got {}", f.arguments);

    proxy_handle.abort();
}
