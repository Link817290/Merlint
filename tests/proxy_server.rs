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

    // Send request with session A
    let req_a = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helping with Project Alpha"},
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

    // Send request with session B (different system prompt)
    let req_b = serde_json::json!({
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are helping with Project Beta"},
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

    // Send another request with session A (same system prompt)
    client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key")
        .json(&req_a)
        .send()
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Verify: should have 2 sessions, session A has 2 entries, session B has 1
    let s = store.lock().await;
    let sessions = s.all_sessions();
    assert_eq!(sessions.len(), 2, "should have 2 separate sessions");

    let total_entries: usize = sessions.iter().map(|(_, sess)| sess.entries.len()).sum();
    assert_eq!(total_entries, 3, "total entries across sessions should be 3");

    // One session should have 2 entries, the other 1
    let mut counts: Vec<usize> = sessions.iter().map(|(_, sess)| sess.entries.len()).collect();
    counts.sort();
    assert_eq!(counts, vec![1, 2]);

    proxy_handle.abort();
}
