"""
Mock Anthropic API server for long-task testing.
Simulates realistic responses including tool_use, cache stats, and streaming.
"""
import json
import time
import sys
import http.server
import threading

REQUEST_COUNT = 0
TOOL_USE_HISTORY = []

# Simulate cache: first requests have cache_creation, later ones have cache_read
CACHE_CREATED = False

def make_text_response(model, text, input_tokens, output_tokens, cache_read=0, cache_create=0):
    return {
        "id": f"msg_{int(time.time()*1000)}",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": cache_create,
        }
    }

def make_tool_response(model, tool_name, tool_id, tool_input, input_tokens, output_tokens, cache_read=0, cache_create=0, text_before=""):
    content = []
    if text_before:
        content.append({"type": "text", "text": text_before})
    content.append({
        "type": "tool_use",
        "id": tool_id,
        "name": tool_name,
        "input": tool_input,
    })
    return {
        "id": f"msg_{int(time.time()*1000)}",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": cache_create,
        }
    }

def make_multi_tool_response(model, tools, input_tokens, output_tokens, cache_read=0, text_before=""):
    content = []
    if text_before:
        content.append({"type": "text", "text": text_before})
    for t in tools:
        content.append({
            "type": "tool_use",
            "id": t["id"],
            "name": t["name"],
            "input": t["input"],
        })
    return {
        "id": f"msg_{int(time.time()*1000)}",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": 0,
        }
    }

def make_sse_response(model, text, input_tokens, output_tokens, cache_read=0):
    """Generate SSE streaming response chunks."""
    chunks = []
    # Start event
    chunks.append(f"event: message_start\ndata: {json.dumps({'type':'message_start','message':{'id':f'msg_{int(time.time()*1000)}','type':'message','role':'assistant','model':model,'content':[],'usage':{'input_tokens':input_tokens,'output_tokens':0,'cache_read_input_tokens':cache_read,'cache_creation_input_tokens':0}}})}\n\n")

    # Content block start
    chunks.append(f"event: content_block_start\ndata: {json.dumps({'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}})}\n\n")

    # Stream text in small chunks
    words = text.split(' ')
    for i, word in enumerate(words):
        w = word + (' ' if i < len(words)-1 else '')
        chunks.append(f"event: content_block_delta\ndata: {json.dumps({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':w}})}\n\n")

    # Content block stop
    chunks.append(f"event: content_block_stop\ndata: {json.dumps({'type':'content_block_stop','index':0})}\n\n")

    # Message delta with usage
    chunks.append(f"event: message_delta\ndata: {json.dumps({'type':'message_delta','delta':{'stop_reason':'end_turn'},'usage':{'output_tokens':output_tokens}})}\n\n")

    # Message stop
    chunks.append(f"event: message_stop\ndata: {json.dumps({'type':'message_stop'})}\n\n")

    return chunks


class MockAnthropicHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass  # Suppress default logging

    def do_POST(self):
        global REQUEST_COUNT, CACHE_CREATED

        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)
        req = json.loads(body) if body else {}

        REQUEST_COUNT += 1
        n = REQUEST_COUNT
        model = req.get("model", "claude-sonnet-4-20250514")
        is_stream = req.get("stream", False)

        # Count tools in request
        tools = req.get("tools", [])
        tool_names = [t.get("name", "") for t in tools]

        # Count messages
        messages = req.get("messages", [])
        msg_count = len(messages)

        # Simulate progressive cache behavior
        base_input = 5000 + msg_count * 800  # grows with conversation
        if n <= 2:
            cache_read = 0
            cache_create = 3000
        elif n <= 5:
            cache_read = int(base_input * 0.2)
            cache_create = 500
        else:
            cache_read = int(base_input * 0.6)
            cache_create = 0

        # Print request stats to stderr for test script to parse
        print(f"[mock] req#{n}: {len(tools)} tools, {msg_count} msgs, stream={is_stream}", file=sys.stderr, flush=True)

        # Generate response based on request number (simulating a real coding session)
        response = self.generate_scenario_response(n, model, base_input, cache_read, cache_create, is_stream)

        if is_stream and isinstance(response, list):
            # SSE streaming response
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            for chunk in response:
                self.wfile.write(chunk.encode())
                self.wfile.flush()
                time.sleep(0.02)  # Simulate streaming delay
        else:
            # Regular JSON response
            resp_bytes = json.dumps(response).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp_bytes)))
            self.end_headers()
            self.wfile.write(resp_bytes)

    def generate_scenario_response(self, n, model, base_input, cache_read, cache_create, is_stream):
        """Simulate a real coding session: building a REST API project."""

        if n == 1:
            # Agent explores the project structure
            return make_multi_tool_response(model, [
                {"id": "tu_1", "name": "Bash", "input": {"command": "find . -type f -name '*.rs' | head -20"}},
                {"id": "tu_2", "name": "Glob", "input": {"pattern": "**/*.toml"}},
            ], base_input, 150, cache_read, "Let me explore the project structure first.")

        elif n == 2:
            return make_tool_response(model, "Read", "tu_3",
                {"file_path": "/workspace/project/src/main.rs"},
                base_input, 80, cache_read, cache_create)

        elif n == 3:
            return make_tool_response(model, "Read", "tu_4",
                {"file_path": "/workspace/project/Cargo.toml"},
                base_input, 60, cache_read, cache_create)

        elif n == 4:
            return make_tool_response(model, "Read", "tu_5",
                {"file_path": "/workspace/project/src/lib.rs"},
                base_input, 90, cache_read, cache_create)

        elif n == 5:
            # Agent reads main.rs again (duplicate read!)
            return make_tool_response(model, "Read", "tu_6",
                {"file_path": "/workspace/project/src/main.rs"},
                base_input, 50, cache_read, cache_create,
                "Let me check main.rs again for the handler setup.")

        elif n == 6:
            # Start writing code
            return make_tool_response(model, "Write", "tu_7",
                {"file_path": "/workspace/project/src/routes/mod.rs",
                 "content": "pub mod users;\npub mod health;\n"},
                base_input, 200, cache_read, 0,
                "I'll create the routes module structure.")

        elif n == 7:
            return make_tool_response(model, "Write", "tu_8",
                {"file_path": "/workspace/project/src/routes/users.rs",
                 "content": """use axum::{Json, extract::Path};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub email: String,
}

pub async fn list_users() -> Json<Vec<User>> {
    Json(vec![
        User { id: 1, name: "Alice".into(), email: "alice@example.com".into() },
    ])
}

pub async fn get_user(Path(id): Path<u64>) -> Json<User> {
    Json(User { id, name: "Alice".into(), email: "alice@example.com".into() })
}

pub async fn create_user(Json(user): Json<User>) -> Json<User> {
    Json(user)
}
"""},
                base_input, 350, cache_read, 0,
                "Now I'll implement the users route handlers.")

        elif n == 8:
            return make_tool_response(model, "Write", "tu_9",
                {"file_path": "/workspace/project/src/routes/health.rs",
                 "content": 'use axum::Json;\n\npub async fn health() -> Json<serde_json::Value> {\n    Json(serde_json::json!({"status": "ok"}))\n}\n'},
                base_input, 120, cache_read, 0)

        elif n == 9:
            # Edit existing file
            return make_tool_response(model, "Edit", "tu_10",
                {"file_path": "/workspace/project/src/main.rs",
                 "old_string": "fn main() {",
                 "new_string": "#[tokio::main]\nasync fn main() {"},
                base_input, 180, cache_read, 0,
                "Now let me update main.rs to use the new routes.")

        elif n == 10:
            # Run tests
            return make_tool_response(model, "Bash", "tu_11",
                {"command": "cargo test 2>&1"},
                base_input, 100, cache_read, 0,
                "Let me run the tests to verify everything compiles.")

        elif n == 11:
            # Text response analyzing test output
            if is_stream:
                return make_sse_response(model,
                    "The tests are passing. Let me now add error handling and validation to the user endpoints. I'll also add proper database integration using sqlx.",
                    base_input, 80, cache_read)
            return make_text_response(model,
                "The tests are passing. Let me now add error handling and validation.",
                base_input, 80, cache_read, 0)

        elif n == 12:
            # Read a file again to check before editing
            return make_tool_response(model, "Read", "tu_12",
                {"file_path": "/workspace/project/src/routes/users.rs"},
                base_input, 60, cache_read, 0,
                "Let me review the users module before adding validation.")

        elif n == 13:
            return make_tool_response(model, "Edit", "tu_13",
                {"file_path": "/workspace/project/src/routes/users.rs",
                 "old_string": "pub async fn create_user",
                 "new_string": "pub async fn create_user_validated"},
                base_input, 250, cache_read, 0)

        elif n == 14:
            # Write new file: database module
            return make_tool_response(model, "Write", "tu_14",
                {"file_path": "/workspace/project/src/db.rs",
                 "content": """use sqlx::PgPool;

pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn new(url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPool::connect(url).await?;
        Ok(Self { pool })
    }

    pub async fn get_user(&self, id: i64) -> Result<Option<crate::routes::users::User>, sqlx::Error> {
        todo!()
    }
}
"""},
                base_input, 280, cache_read, 0,
                "I'll add a database module for PostgreSQL integration.")

        elif n == 15:
            return make_tool_response(model, "Read", "tu_15",
                {"file_path": "/workspace/project/Cargo.toml"},
                base_input, 50, cache_read, 0,
                "Let me check Cargo.toml to add sqlx dependency.")

        elif n == 16:
            return make_tool_response(model, "Edit", "tu_16",
                {"file_path": "/workspace/project/Cargo.toml",
                 "old_string": "[dependencies]",
                 "new_string": '[dependencies]\nsqlx = { version = "0.7", features = ["runtime-tokio", "postgres"] }'},
                base_input, 100, cache_read, 0)

        elif n == 17:
            # Write test file
            return make_tool_response(model, "Write", "tu_17",
                {"file_path": "/workspace/project/tests/integration.rs",
                 "content": """#[tokio::test]
async fn test_health_endpoint() {
    // TODO: setup test server
    assert!(true);
}

#[tokio::test]
async fn test_create_user() {
    assert!(true);
}

#[tokio::test]
async fn test_list_users() {
    assert!(true);
}
"""},
                base_input, 200, cache_read, 0)

        elif n == 18:
            # Read main.rs again (3rd time - waste pattern)
            return make_tool_response(model, "Read", "tu_18",
                {"file_path": "/workspace/project/src/main.rs"},
                base_input, 40, cache_read, 0,
                "Let me check main.rs one more time for the router setup.")

        elif n == 19:
            return make_tool_response(model, "Edit", "tu_19",
                {"file_path": "/workspace/project/src/main.rs",
                 "old_string": "// routes here",
                 "new_string": "let app = Router::new()\n    .route(\"/health\", get(routes::health::health))\n    .route(\"/users\", get(routes::users::list_users).post(routes::users::create_user));"},
                base_input, 300, cache_read, 0)

        elif n == 20:
            # Streaming response for longer explanation
            if is_stream:
                return make_sse_response(model,
                    "Great progress! The REST API now has health check and user CRUD endpoints. Let me add middleware for logging and error handling. I'll also set up proper error types with thiserror.",
                    base_input, 120, cache_read)
            return make_text_response(model,
                "Great progress! The REST API now has health check and user CRUD endpoints. Let me add middleware.",
                base_input, 120, cache_read, 0)

        elif n == 21:
            return make_tool_response(model, "Write", "tu_21",
                {"file_path": "/workspace/project/src/error.rs",
                 "content": """use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

pub enum AppError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, msg).into_response()
    }
}
"""},
                base_input, 280, cache_read, 0)

        elif n == 22:
            return make_tool_response(model, "Write", "tu_22",
                {"file_path": "/workspace/project/src/middleware.rs",
                 "content": """use axum::middleware::Next;
use axum::http::Request;
use axum::response::Response;
use std::time::Instant;

pub async fn logging<B>(req: Request<B>, next: Next<B>) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();
    tracing::info!("{} {} - {:?} - {}ms", method, uri, response.status(), elapsed.as_millis());
    response
}
"""},
                base_input, 250, cache_read, 0)

        elif n == 23:
            # Run cargo build
            return make_tool_response(model, "Bash", "tu_23",
                {"command": "cargo build 2>&1"},
                base_input, 80, cache_read, 0)

        elif n == 24:
            # Fix compilation error
            if is_stream:
                return make_sse_response(model,
                    "There's a compilation error in the middleware - the Next type signature changed in axum 0.7. Let me fix that.",
                    base_input, 60, cache_read)
            return make_text_response(model,
                "There's a compilation error. Let me fix the middleware.",
                base_input, 60, cache_read, 0)

        elif n == 25:
            return make_tool_response(model, "Edit", "tu_25",
                {"file_path": "/workspace/project/src/middleware.rs",
                 "old_string": "pub async fn logging<B>(req: Request<B>, next: Next<B>)",
                 "new_string": "pub async fn logging(req: Request<axum::body::Body>, next: Next)"},
                base_input, 120, cache_read, 0)

        elif n == 26:
            return make_tool_response(model, "Bash", "tu_26",
                {"command": "cargo build 2>&1"},
                base_input, 60, cache_read, 0)

        elif n == 27:
            # Read lib.rs to update module declarations
            return make_tool_response(model, "Read", "tu_27",
                {"file_path": "/workspace/project/src/lib.rs"},
                base_input, 40, cache_read, 0)

        elif n == 28:
            return make_tool_response(model, "Edit", "tu_28",
                {"file_path": "/workspace/project/src/lib.rs",
                 "old_string": "pub mod routes;",
                 "new_string": "pub mod routes;\npub mod db;\npub mod error;\npub mod middleware;"},
                base_input, 80, cache_read, 0)

        elif n == 29:
            return make_tool_response(model, "Bash", "tu_29",
                {"command": "cargo test 2>&1"},
                base_input, 100, cache_read, 0)

        elif n == 30:
            # Final streaming summary
            if is_stream:
                return make_sse_response(model,
                    "All tests pass! Here's a summary of what I've built:\n\n1. REST API with health check and user CRUD endpoints\n2. PostgreSQL database integration with sqlx\n3. Custom error types with proper HTTP status mapping\n4. Request logging middleware\n5. Integration test scaffolding\n\nThe project structure is clean and follows Rust best practices. You can start the server with `cargo run` and test with curl.",
                    base_input, 200, cache_read)
            return make_text_response(model,
                "All tests pass! The REST API is complete with health, users, db, error handling, and middleware.",
                base_input, 200, cache_read, 0)

        else:
            # Extra requests: just return text
            return make_text_response(model, f"Response for request #{n}.", base_input, 50, cache_read, 0)


def run_server(port):
    server = http.server.HTTPServer(('127.0.0.1', port), MockAnthropicHandler)
    print(f"[mock] Anthropic mock server on port {port}", file=sys.stderr, flush=True)
    server.serve_forever()

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9999
    run_server(port)
