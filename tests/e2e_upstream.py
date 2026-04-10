"""
Minimal upstream server that returns realistic Anthropic API responses.
Cache behavior is controlled by the client via X-Simulate-Cache header:
  - "warm" or absent after turn 2: 40% cache hit
  - "cold": 0% cache hit (simulates >5min gap, cache expired)
  - "partial": 15% cache hit (cache partially expired)
"""
import json, sys
from http.server import HTTPServer, BaseHTTPRequestHandler

turn_counter = {"n": 0}

SCENARIO_TOOLS = {
    "coding": [
        ["Read", "Glob"],
        ["Read", "Grep"],
        ["Read"],
        ["Edit"],
        ["Bash"],
        ["Read"],
        ["Edit"],
        ["Bash"],
        ["Read", "Read"],
        ["Edit", "Bash"],
    ],
    "research": [
        ["WebSearch"],
        ["WebFetch"],
        ["WebFetch"],
        ["Read"],
        ["WebSearch"],
        ["WebFetch"],
        ["Read"],
        ["WebFetch"],
    ],
    "devops": [
        ["Bash"],
        ["Read"],
        ["Bash"],
        ["Bash"],
        ["Read"],
        ["Bash"],
        ["DockerExec"],
        ["Bash", "Read"],
    ],
}

def get_scenario(body):
    sys_text = ""
    if "system" in body:
        s = body["system"]
        sys_text = s if isinstance(s, str) else " ".join(
            b.get("text", "") for b in s if isinstance(b, dict)
        )
    for key in SCENARIO_TOOLS:
        if key in sys_text.lower():
            return key
    return "coding"

def make_tool_use_block(name, idx):
    inputs = {
        "Read": {"file_path": "/workspace/src/main.rs"},
        "Glob": {"pattern": "**/*.rs"},
        "Grep": {"pattern": "fn main", "path": "/workspace/src"},
        "Edit": {"file_path": "/workspace/src/main.rs", "old_string": "old", "new_string": "new"},
        "Bash": {"command": "cargo test"},
        "WebSearch": {"query": "rust async patterns"},
        "WebFetch": {"url": "https://docs.rs/tokio/latest"},
        "DockerExec": {"container": "app", "command": "ls /app"},
    }
    return {
        "type": "tool_use",
        "id": f"toolu_{idx:04d}",
        "name": name,
        "input": inputs.get(name, {"arg": "value"}),
    }

class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        body = json.loads(raw) if raw else {}

        scenario = get_scenario(body)
        turn = turn_counter["n"]
        turn_counter["n"] += 1

        tools_seq = SCENARIO_TOOLS.get(scenario, SCENARIO_TOOLS["coding"])
        tool_names = tools_seq[turn % len(tools_seq)]

        msg_count = len(body.get("messages", []))
        prompt_tokens = 1000 + msg_count * 500

        # Cache behavior controlled by client header
        cache_hint = self.headers.get("X-Simulate-Cache", "").lower()
        if cache_hint == "cold":
            cache_read = 0
        elif cache_hint == "partial":
            cache_read = int(prompt_tokens * 0.08)  # 8% — below 10% resume threshold
        elif cache_hint == "warm":
            cache_read = int(prompt_tokens * 0.4)
        else:
            # Default: warm after turn 2
            cache_read = int(prompt_tokens * 0.4) if turn >= 2 else 0

        content = []
        content.append({"type": "text", "text": f"I'll help with that. [turn {turn+1}]"})
        for i, name in enumerate(tool_names):
            content.append(make_tool_use_block(name, turn * 10 + i))

        resp = {
            "id": f"msg_{turn:04d}",
            "type": "message",
            "role": "assistant",
            "model": body.get("model", "claude-sonnet-4-20250514"),
            "content": content,
            "stop_reason": "tool_use" if tool_names else "end_turn",
            "usage": {
                "input_tokens": prompt_tokens,
                "output_tokens": 150 + len(tool_names) * 80,
                "cache_creation_input_tokens": 500 if cache_read == 0 else 0,
                "cache_read_input_tokens": cache_read,
            },
        }

        out = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9999
    srv = HTTPServer(("127.0.0.1", port), Handler)
    print(f"upstream on :{port}", flush=True)
    srv.serve_forever()
