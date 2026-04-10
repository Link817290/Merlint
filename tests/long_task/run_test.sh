#!/bin/bash
# Long-task integration test: simulates a real coding agent session through merlint proxy.
#
# Architecture:
#   [test script] --> [merlint proxy :8019] --> [mock anthropic :9999]
#
# Scenario: Agent builds a REST API project over 30 requests,
# including tool calls, file reads (with duplicates), streaming responses.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MERLINT_BIN="${SCRIPT_DIR}/../../target/debug/merlint"
MOCK_PORT=9999
PROXY_PORT=8019
TRACE_DIR="/tmp/merlint-long-test"
SPEND_DB="$HOME/.merlint/spend.db"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

cleanup() {
    echo -e "\n${CYAN}Cleaning up...${NC}"
    kill $MOCK_PID 2>/dev/null || true
    kill $PROXY_PID 2>/dev/null || true
    wait $MOCK_PID 2>/dev/null || true
    wait $PROXY_PID 2>/dev/null || true
}
trap cleanup EXIT

echo -e "${BOLD}${CYAN}═══════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}${CYAN}  merlint long-task integration test${NC}"
echo -e "${BOLD}${CYAN}  Simulating: Agent builds a REST API (30 requests)${NC}"
echo -e "${BOLD}${CYAN}═══════════════════════════════════════════════════════${NC}"
echo ""

# --- Step 0: Build merlint ---
echo -e "${YELLOW}[0/5] Building merlint...${NC}"
cd "$SCRIPT_DIR/../.."
cargo build --quiet 2>&1
echo -e "${GREEN}  ✓ Build OK${NC}"

# --- Step 1: Start mock Anthropic server ---
echo -e "${YELLOW}[1/5] Starting mock Anthropic API on :${MOCK_PORT}...${NC}"
python3 "$SCRIPT_DIR/mock_anthropic.py" $MOCK_PORT &
MOCK_PID=$!
sleep 1

# Verify mock is running
if ! kill -0 $MOCK_PID 2>/dev/null; then
    echo -e "${RED}  ✗ Mock server failed to start${NC}"
    exit 1
fi
echo -e "${GREEN}  ✓ Mock server running (PID $MOCK_PID)${NC}"

# --- Step 2: Start merlint proxy ---
echo -e "${YELLOW}[2/5] Starting merlint proxy on :${PROXY_PORT}...${NC}"
rm -rf "$TRACE_DIR"
mkdir -p "$TRACE_DIR"

# Remove old spend data for clean test
rm -f "$SPEND_DB"

$MERLINT_BIN proxy \
    --port $PROXY_PORT \
    --target "http://127.0.0.1:${MOCK_PORT}" \
    --output "${TRACE_DIR}/traces.json" \
    --optimize \
    &
PROXY_PID=$!
sleep 2

if ! kill -0 $PROXY_PID 2>/dev/null; then
    echo -e "${RED}  ✗ Proxy failed to start${NC}"
    exit 1
fi
echo -e "${GREEN}  ✓ Proxy running (PID $PROXY_PID)${NC}"

# --- Step 3: System prompt (used for session key derivation) ---
SYSTEM_PROMPT='You are a coding assistant. Primary working directory: /workspace/project. Help the user build a REST API in Rust.'

# Tool definitions (simulating Claude Code tools)
TOOLS='[
  {"name":"Bash","description":"Execute shell commands","input_schema":{"type":"object","properties":{"command":{"type":"string"}}}},
  {"name":"Read","description":"Read a file","input_schema":{"type":"object","properties":{"file_path":{"type":"string"}}}},
  {"name":"Write","description":"Write a file","input_schema":{"type":"object","properties":{"file_path":{"type":"string"},"content":{"type":"string"}}}},
  {"name":"Edit","description":"Edit a file","input_schema":{"type":"object","properties":{"file_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}}}},
  {"name":"Glob","description":"Find files by pattern","input_schema":{"type":"object","properties":{"pattern":{"type":"string"}}}},
  {"name":"Grep","description":"Search file content","input_schema":{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}}}},
  {"name":"TodoWrite","description":"Write todo list","input_schema":{"type":"object","properties":{"todos":{"type":"array"}}}},
  {"name":"Agent","description":"Spawn sub-agent","input_schema":{"type":"object","properties":{"prompt":{"type":"string"}}}},
  {"name":"WebSearch","description":"Search the web","input_schema":{"type":"object","properties":{"query":{"type":"string"}}}},
  {"name":"WebFetch","description":"Fetch a URL","input_schema":{"type":"object","properties":{"url":{"type":"string"}}}}
]'

PROXY_URL="http://127.0.0.1:${PROXY_PORT}"

# --- Step 4: Send requests ---
echo -e "${YELLOW}[3/5] Running 30-request coding session...${NC}"
echo ""

MESSAGES='[]'
PASS=0
FAIL=0

send_request() {
    local req_num=$1
    local stream=${2:-false}
    local desc=$3

    local body
    body=$(jq -n \
        --arg model "claude-sonnet-4-20250514" \
        --argjson tools "$TOOLS" \
        --argjson msgs "$MESSAGES" \
        --argjson stream "$stream" \
        --arg system "$SYSTEM_PROMPT" \
        '{model: $model, tools: $tools, messages: $msgs, stream: $stream, system: $system, max_tokens: 4096}')

    local resp
    if [ "$stream" = "true" ]; then
        resp=$(curl -s "$PROXY_URL/v1/messages" \
            -H "Content-Type: application/json" \
            -H "anthropic-version: 2023-06-01" \
            -H "x-api-key: test-key" \
            -d "$body" 2>/dev/null)
    else
        resp=$(curl -s "$PROXY_URL/v1/messages" \
            -H "Content-Type: application/json" \
            -H "anthropic-version: 2023-06-01" \
            -H "x-api-key: test-key" \
            -d "$body" 2>/dev/null)
    fi

    # Check response is valid
    local resp_type
    if [ "$stream" = "true" ]; then
        # For SSE, check if we got event stream data
        if echo "$resp" | grep -q "message_start\|tool_use\|end_turn\|text"; then
            resp_type="stream_ok"
        else
            resp_type="error"
        fi
    else
        resp_type=$(echo "$resp" | jq -r '.type // .error.type // "error"' 2>/dev/null)
    fi

    if [ "$resp_type" = "message" ] || [ "$resp_type" = "stream_ok" ]; then
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}✓${NC} #${req_num}: ${desc}"
    else
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}✗${NC} #${req_num}: ${desc} (got: ${resp_type})"
        echo "    Response: $(echo "$resp" | head -c 200)"
    fi

    # Add messages for conversation continuity
    # Add user message
    if [ $req_num -eq 1 ]; then
        MESSAGES=$(echo "$MESSAGES" | jq '. + [{"role":"user","content":"I want to build a REST API in Rust using axum. Start by exploring the project structure."}]')
    elif [ $req_num -le 5 ]; then
        # Simulate tool results being added
        MESSAGES=$(echo "$MESSAGES" | jq --arg n "$req_num" '. + [
            {"role":"assistant","content":[{"type":"tool_use","id":"tu_\($n)","name":"Read","input":{"file_path":"/workspace/project/src/main.rs"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_\($n)","content":"use axum::Router;\n\nfn main() {\n    // routes here\n    println!(\"Hello\");\n}\n"}]}
        ]')
    elif [ $req_num -le 10 ]; then
        MESSAGES=$(echo "$MESSAGES" | jq --arg n "$req_num" '. + [
            {"role":"assistant","content":[{"type":"tool_use","id":"tu_\($n)","name":"Write","input":{"file_path":"/workspace/project/src/routes.rs","content":"pub mod users;"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_\($n)","content":"File written successfully."}]}
        ]')
    else
        MESSAGES=$(echo "$MESSAGES" | jq --arg n "$req_num" '. + [
            {"role":"assistant","content":[{"type":"text","text":"Working on the project..."}]},
            {"role":"user","content":"Continue with the implementation."}
        ]')
    fi
}

# Phase 1: Project exploration (requests 1-5)
echo -e "  ${CYAN}--- Phase 1: Project Exploration ---${NC}"
send_request 1 false "Explore project structure (Bash + Glob)"
send_request 2 false "Read main.rs"
send_request 3 false "Read Cargo.toml"
send_request 4 false "Read lib.rs"
send_request 5 false "Re-read main.rs (duplicate read)"

# Phase 2: Code generation (requests 6-10)
echo -e "  ${CYAN}--- Phase 2: Code Generation ---${NC}"
send_request 6 false "Create routes/mod.rs"
send_request 7 false "Create routes/users.rs"
send_request 8 false "Create routes/health.rs"
send_request 9 false "Edit main.rs (add async)"
send_request 10 false "Run cargo test"

# Phase 3: Iteration (requests 11-15, with streaming)
echo -e "  ${CYAN}--- Phase 3: Iteration + Streaming ---${NC}"
send_request 11 true "Streaming: analyze test results"
send_request 12 false "Re-read users.rs before edit"
send_request 13 false "Edit users.rs (add validation)"
send_request 14 false "Create db.rs module"
send_request 15 false "Re-read Cargo.toml"

# Phase 4: Dependencies and testing (requests 16-20)
echo -e "  ${CYAN}--- Phase 4: Dependencies + Testing ---${NC}"
send_request 16 false "Edit Cargo.toml (add sqlx)"
send_request 17 false "Create integration tests"
send_request 18 false "Re-read main.rs (3rd time!)"
send_request 19 false "Edit main.rs (add routes)"
send_request 20 true "Streaming: progress summary"

# Phase 5: Error handling and middleware (requests 21-25)
echo -e "  ${CYAN}--- Phase 5: Error Handling + Middleware ---${NC}"
send_request 21 false "Create error.rs"
send_request 22 false "Create middleware.rs"
send_request 23 false "Run cargo build"
send_request 24 true "Streaming: diagnose build error"
send_request 25 false "Fix middleware.rs"

# Phase 6: Final integration (requests 26-30)
echo -e "  ${CYAN}--- Phase 6: Final Integration ---${NC}"
send_request 26 false "Run cargo build (retry)"
send_request 27 false "Read lib.rs to update"
send_request 28 false "Edit lib.rs (add modules)"
send_request 29 false "Run cargo test (final)"
send_request 30 true "Streaming: final summary"

echo ""
echo -e "${BOLD}═══════════════════════════════════════════════════════${NC}"
echo -e "  Requests: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC} / 30 total"
echo -e "${BOLD}═══════════════════════════════════════════════════════${NC}"

# --- Step 5: Check merlint results ---
echo ""
echo -e "${YELLOW}[4/5] Checking proxy status...${NC}"
sleep 1

STATUS=$(curl -s "$PROXY_URL/merlint/status" 2>/dev/null)
if [ -n "$STATUS" ]; then
    echo -e "${GREEN}  ✓ Status endpoint OK${NC}"

    SESSION_COUNT=$(echo "$STATUS" | jq '.session_count')
    TOTAL_REQS=$(echo "$STATUS" | jq '.total_requests')
    echo "    Sessions: $SESSION_COUNT"
    echo "    Total requests: $TOTAL_REQS"

    # Per-session details
    echo "$STATUS" | jq -r '.sessions[] | "    Session \(.key): \(.request_count) reqs, \(.total_tokens) tokens, saved \(.tokens_saved) tokens, cache \(.api_cache_hit_rate)%, pruning_suspended=\(.pruning_suspended)"'
else
    echo -e "${RED}  ✗ Status endpoint failed${NC}"
fi

# Check spend tracking
echo ""
echo -e "${YELLOW}[5/5] Checking spend tracking...${NC}"

SPEND=$(curl -s "$PROXY_URL/merlint/spend" 2>/dev/null)
if [ -n "$SPEND" ] && ! echo "$SPEND" | jq -e '.error' > /dev/null 2>&1; then
    echo -e "${GREEN}  ✓ Spend tracking OK${NC}"
    echo "    Total cost: \$$(echo "$SPEND" | jq -r '.total.cost_usd')"
    echo "    Total saved: \$$(echo "$SPEND" | jq -r '.total.saved_usd')"
    echo "    Total tokens: $(echo "$SPEND" | jq -r '.total.tokens')"
    echo "    Requests logged: $(echo "$SPEND" | jq -r '.total.requests')"
else
    echo -e "${YELLOW}  ! Spend data empty or not available (OK for first run)${NC}"
fi

# Run merlint spend CLI
echo ""
echo -e "${CYAN}Running: merlint spend --days 1${NC}"
$MERLINT_BIN spend --days 1 2>/dev/null || echo "  (no spend data yet)"

echo ""
echo -e "${CYAN}Running: merlint spend --insights${NC}"
$MERLINT_BIN spend --insights 2>/dev/null || echo "  (no insight data yet)"

# Check trace files
echo ""
echo -e "${CYAN}Trace files:${NC}"
ls -la "$TRACE_DIR"/*.json 2>/dev/null | while read line; do
    echo "  $line"
done

echo ""
if [ $FAIL -eq 0 ]; then
    echo -e "${BOLD}${GREEN}═══════════════════════════════════════════${NC}"
    echo -e "${BOLD}${GREEN}  ALL 30 REQUESTS PASSED ✓${NC}"
    echo -e "${BOLD}${GREEN}═══════════════════════════════════════════${NC}"
else
    echo -e "${BOLD}${RED}═══════════════════════════════════════════${NC}"
    echo -e "${BOLD}${RED}  ${FAIL} REQUESTS FAILED ✗${NC}"
    echo -e "${BOLD}${RED}═══════════════════════════════════════════${NC}"
    exit 1
fi
