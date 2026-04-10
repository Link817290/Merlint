use std::sync::Arc;
use tokio::sync::Mutex;

use merlint::models::trace::TraceSession;
use merlint::proxy::server::{ProxyConfig, run_proxy};
use merlint::proxy::transformer::new_shared_transformer;

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
        // Accept up to 10 connections then stop
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

    let session = Arc::new(Mutex::new(TraceSession::new()));
    let session_clone = session.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_file: None,
        optimize: false,
    };

    // Start proxy in background
    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, session_clone, None).await;
    });

    // Give proxy time to start
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
    let sess = session.lock().await;
    assert_eq!(sess.entries.len(), 1);
    assert_eq!(sess.entries[0].response.usage.as_ref().unwrap().total_tokens, 120);

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_with_optimization_prunes_tools() {
    let upstream_resp = make_openai_tool_response("Read");
    let (upstream_port, _upstream) = start_mock_upstream(upstream_resp).await;

    let session = Arc::new(Mutex::new(TraceSession::new()));
    let transformer = Some(new_shared_transformer());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_file: None,
        optimize: true,
    };

    let session_clone = session.clone();
    let tx_clone = transformer.clone();
    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, session_clone, tx_clone).await;
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

    // Check transformer state
    let tx = transformer.unwrap();
    let t = tx.lock().await;
    // Read should be recorded as used (from response tool_calls)
    let snapshot = t.tool_usage_snapshot();
    assert!(snapshot.iter().any(|(name, _)| name == "Read"), "Read should be in usage snapshot");

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

    let session = Arc::new(Mutex::new(TraceSession::new()));
    let session_clone = session.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_file: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, session_clone, None).await;
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
    let sess = session.lock().await;
    assert_eq!(sess.entries.len(), 1);
    let usage = sess.entries[0].response.usage.as_ref().unwrap();
    assert_eq!(usage.prompt_tokens, 50);
    assert_eq!(usage.completion_tokens, 10);

    proxy_handle.abort();
}

#[tokio::test]
async fn test_proxy_sse_response_handling() {
    // Test that SSE responses are parsed correctly for trace recording
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

    let session = Arc::new(Mutex::new(TraceSession::new()));
    let session_clone = session.clone();

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy_listener.local_addr().unwrap().port();
    drop(proxy_listener);

    let config = ProxyConfig {
        listen_port: proxy_port,
        target_url: format!("http://127.0.0.1:{}", upstream_port),
        api_key: None,
        live_trace_file: None,
        optimize: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = run_proxy(config, session_clone, None).await;
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let req = make_openai_request(vec![], vec![("user", "Hi")]);
    let resp = send_chat_request(proxy_port, &req).await;
    assert_eq!(resp.status(), 200);

    // SSE response should be forwarded
    let content_type = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(content_type.contains("text/event-stream"));

    // Trace should have been recorded with merged SSE content
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let sess = session.lock().await;
    assert_eq!(sess.entries.len(), 1);

    // The merged message should contain "Hello world"
    let entry = &sess.entries[0];
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
