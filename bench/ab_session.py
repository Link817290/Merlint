#!/usr/bin/env python3
"""
merlint A/B Test — Full Multi-Turn Coding Session

Simulates a REAL coding agent session with 10 turns:
  Turn 1: User asks to fix a bug
  Turn 2: Agent reads the file (tool_use → tool_result)
  Turn 3: Agent analyzes and proposes fix
  Turn 4: Agent edits the file
  Turn 5: Agent runs tests
  Turn 6: Test fails, user asks to fix
  Turn 7: Agent reads test file
  Turn 8: Agent fixes the test
  Turn 9: Agent runs tests again
  Turn 10: All tests pass, agent summarizes

Each turn carries the FULL conversation history (like real agents do).
Two parallel sessions: one with all 22 tools, one with only needed tools.
Measures prompt_tokens and cached_tokens at every turn.
"""

import json
import os
import sys
import time
from openai import OpenAI

KIMI_API_KEY = sys.argv[1] if len(sys.argv) > 1 else ""

client = OpenAI(
    api_key=KIMI_API_KEY,
    base_url="https://api.kimi.com/coding/v1",
    default_headers={"User-Agent": "claude-code/1.0"},
)

MODEL = "kimi-k2-0711-preview"

# ========== Full tool definitions (same as ab_real_tasks.py) ==========

ALL_TOOLS = [
    {"type": "function", "function": {"name": "Read", "description": "Reads a file from the local filesystem. You can access any file directly by using this tool. The file_path parameter must be an absolute path, not a relative path. By default, it reads up to 2000 lines starting from the beginning of the file. You can optionally specify a line offset and limit. Results are returned using cat -n format, with line numbers starting at 1. This tool can read images (PNG, JPG), PDFs, and Jupyter notebooks (.ipynb files).", "parameters": {"type": "object", "properties": {"file_path": {"type": "string", "description": "The absolute path to the file to read"}, "offset": {"type": "number", "description": "The line number to start reading from"}, "limit": {"type": "number", "description": "The number of lines to read"}}, "required": ["file_path"]}}},
    {"type": "function", "function": {"name": "Write", "description": "Writes a file to the local filesystem. This tool will overwrite the existing file if there is one at the provided path. If this is an existing file, you MUST use the Read tool first to read the file's contents. Prefer the Edit tool for modifying existing files — it only sends the diff. Only use this tool to create new files or for complete rewrites.", "parameters": {"type": "object", "properties": {"file_path": {"type": "string", "description": "The absolute path to the file to write"}, "content": {"type": "string", "description": "The content to write to the file"}}, "required": ["file_path", "content"]}}},
    {"type": "function", "function": {"name": "Edit", "description": "Performs exact string replacements in files. You must use your Read tool at least once before editing. When editing text from Read tool output, ensure you preserve the exact indentation. The edit will FAIL if old_string is not unique in the file.", "parameters": {"type": "object", "properties": {"file_path": {"type": "string", "description": "The absolute path to the file to modify"}, "old_string": {"type": "string", "description": "The text to replace"}, "new_string": {"type": "string", "description": "The replacement text"}, "replace_all": {"type": "boolean", "description": "Replace all occurrences", "default": False}}, "required": ["file_path", "old_string", "new_string"]}}},
    {"type": "function", "function": {"name": "Bash", "description": "Executes a given bash command and returns its output. The working directory persists between commands, but shell state does not. Avoid using this tool to run find, grep, cat, head, tail, sed, awk, or echo commands — use dedicated tools instead. Always quote file paths that contain spaces. You may specify an optional timeout in milliseconds (up to 600000ms / 10 minutes).", "parameters": {"type": "object", "properties": {"command": {"type": "string", "description": "The command to execute"}, "timeout": {"type": "number", "description": "Optional timeout in milliseconds"}, "description": {"type": "string", "description": "Description of what this command does"}, "run_in_background": {"type": "boolean", "description": "Run in background"}}, "required": ["command"]}}},
    {"type": "function", "function": {"name": "Glob", "description": "Find files matching a glob pattern in the filesystem. Use this tool instead of find or ls commands. Supports standard glob patterns like *.py, **/*.rs, src/**/test_*.py etc.", "parameters": {"type": "object", "properties": {"pattern": {"type": "string", "description": "The glob pattern to match"}, "path": {"type": "string", "description": "The directory to search in"}}, "required": ["pattern"]}}},
    {"type": "function", "function": {"name": "Grep", "description": "Search file contents using a regular expression pattern. Use this tool instead of grep or rg commands. Returns matching lines with file paths and line numbers.", "parameters": {"type": "object", "properties": {"pattern": {"type": "string", "description": "The regex pattern to search for"}, "path": {"type": "string", "description": "The directory to search in"}, "include": {"type": "string", "description": "File pattern to include"}}, "required": ["pattern"]}}},
    {"type": "function", "function": {"name": "Agent", "description": "Launch a sub-agent to handle a specific task independently. Subagents are valuable for parallelizing independent queries.", "parameters": {"type": "object", "properties": {"prompt": {"type": "string", "description": "Task description"}, "subagent_type": {"type": "string", "description": "Agent type"}}, "required": ["prompt"]}}},
    {"type": "function", "function": {"name": "AskUserQuestion", "description": "Ask the user a question and wait for their response.", "parameters": {"type": "object", "properties": {"question": {"type": "string", "description": "The question to ask"}}, "required": ["question"]}}},
    {"type": "function", "function": {"name": "TodoWrite", "description": "Create or update a todo list to track tasks and progress.", "parameters": {"type": "object", "properties": {"todos": {"type": "string", "description": "JSON array of todo items"}}, "required": ["todos"]}}},
    {"type": "function", "function": {"name": "WebFetch", "description": "Fetch the content of a URL and return it as text.", "parameters": {"type": "object", "properties": {"url": {"type": "string", "description": "The URL to fetch"}, "prompt": {"type": "string", "description": "Processing instructions"}}, "required": ["url"]}}},
    {"type": "function", "function": {"name": "WebSearch", "description": "Search the web using a search engine query.", "parameters": {"type": "object", "properties": {"query": {"type": "string", "description": "Search query"}, "num_results": {"type": "number", "description": "Number of results"}}, "required": ["query"]}}},
    {"type": "function", "function": {"name": "NotebookEdit", "description": "Edit a Jupyter notebook cell by index.", "parameters": {"type": "object", "properties": {"notebook_path": {"type": "string", "description": "Path to notebook"}, "cell_index": {"type": "number", "description": "Cell index"}, "new_source": {"type": "string", "description": "New source"}}, "required": ["notebook_path", "cell_index", "new_source"]}}},
    {"type": "function", "function": {"name": "Skill", "description": "Execute a predefined skill by name.", "parameters": {"type": "object", "properties": {"skill_name": {"type": "string", "description": "Skill name"}, "arguments": {"type": "string", "description": "Arguments"}}, "required": ["skill_name"]}}},
    {"type": "function", "function": {"name": "ExitPlanMode", "description": "Exit planning mode and begin executing.", "parameters": {"type": "object", "properties": {}}}},
    {"type": "function", "function": {"name": "EnterPlanMode", "description": "Enter planning mode to think through a problem.", "parameters": {"type": "object", "properties": {}}}},
    {"type": "function", "function": {"name": "ExitWorktree", "description": "Exit the current git worktree.", "parameters": {"type": "object", "properties": {}}}},
    {"type": "function", "function": {"name": "EnterWorktree", "description": "Enter a git worktree for isolated work.", "parameters": {"type": "object", "properties": {"branch": {"type": "string", "description": "Branch name"}}, "required": ["branch"]}}},
    {"type": "function", "function": {"name": "CronCreate", "description": "Create a scheduled cron job.", "parameters": {"type": "object", "properties": {"schedule": {"type": "string", "description": "Cron expression"}, "command": {"type": "string", "description": "Command"}}, "required": ["schedule", "command"]}}},
    {"type": "function", "function": {"name": "CronDelete", "description": "Delete a cron job by ID.", "parameters": {"type": "object", "properties": {"cron_id": {"type": "string", "description": "Cron job ID"}}, "required": ["cron_id"]}}},
    {"type": "function", "function": {"name": "CronList", "description": "List all active cron jobs.", "parameters": {"type": "object", "properties": {}}}},
    {"type": "function", "function": {"name": "TaskOutput", "description": "Read output of a background task.", "parameters": {"type": "object", "properties": {"task_id": {"type": "string", "description": "Task ID"}}, "required": ["task_id"]}}},
    {"type": "function", "function": {"name": "TaskStop", "description": "Stop a running background task.", "parameters": {"type": "object", "properties": {"task_id": {"type": "string", "description": "Task ID"}}, "required": ["task_id"]}}},
]

# Only tools actually used in this session
USED_NAMES = {"Read", "Edit", "Bash", "Grep"}
USED_TOOLS = [t for t in ALL_TOOLS if t["function"]["name"] in USED_NAMES]

SYSTEM = "You are a coding assistant. Help the user fix bugs, write code, and run tests. Use the tools available to you. Always read files before editing. Keep responses concise."

# ========== Simulate a 10-turn bug-fix session ==========
# Each turn adds to the conversation history, just like a real agent

# Simulated file content (what the agent would "see" after reading)
RUST_FILE = '''use std::collections::HashMap;

pub struct Config {
    pub host: String,
    pub port: u16,
    pub headers: HashMap<String, String>,
}

impl Config {
    pub fn get_auth_header(&self) -> String {
        // BUG: unwrap() on a key that might not exist
        self.headers.get("Authorization").unwrap().clone()
    }

    pub fn get_content_type(&self) -> String {
        self.headers.get("Content-Type").unwrap_or(&"application/json".to_string()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_auth_header() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        let config = Config { host: "localhost".to_string(), port: 8080, headers };
        assert_eq!(config.get_auth_header(), "Bearer token123");
    }

    #[test]
    fn test_get_auth_header_missing() {
        let config = Config { host: "localhost".to_string(), port: 8080, headers: HashMap::new() };
        // This test currently panics because get_auth_header() uses unwrap()
        let result = config.get_auth_header();
        assert_eq!(result, "");
    }
}
'''

TEST_OUTPUT_FAIL = '''running 2 tests
test tests::test_get_auth_header ... ok
test tests::test_get_auth_header_missing ... FAILED

failures:

---- tests::test_get_auth_header_missing stdout ----
thread 'tests::test_get_auth_header_missing' panicked at 'called `Option::unwrap()` on a `None` value', src/config.rs:12:53

failures:
    tests::test_get_auth_header_missing

test result: FAILED. 1 passed; 1 failed; 0 ignored
'''

TEST_OUTPUT_PASS = '''running 2 tests
test tests::test_get_auth_header ... ok
test tests::test_get_auth_header_missing ... ok

test result: ok. 2 passed; 0 failed; 0 ignored
'''

# The 10 turns of conversation
TURNS = [
    # Turn 1: User reports bug
    {
        "role": "user",
        "content": "I'm getting a panic in production: 'called `Option::unwrap()` on a `None` value' in src/config.rs line 12. The function get_auth_header() crashes when the Authorization header is missing. Can you fix this? The code is in /workspace/project/src/config.rs"
    },
    # Turn 2: Agent reads the file (simulated tool call + result)
    {
        "role": "assistant",
        "content": "I'll read the file to understand the issue.",
        "tool_calls": [{"id": "call_001", "type": "function", "function": {"name": "Read", "arguments": json.dumps({"file_path": "/workspace/project/src/config.rs"})}}]
    },
    {
        "role": "tool",
        "tool_call_id": "call_001",
        "content": RUST_FILE
    },
    # Turn 3: Agent analyzes
    {
        "role": "assistant",
        "content": "I can see the bug. On line 12, `get_auth_header()` uses `.unwrap()` on `self.headers.get(\"Authorization\")`, which panics when the key doesn't exist. I notice `get_content_type()` on line 16 already handles this correctly with `.unwrap_or()`. Let me fix `get_auth_header()` to return an empty string when the header is missing."
    },
    # Turn 4: User confirms
    {
        "role": "user",
        "content": "Yes, fix it. Return an empty string if the header is missing."
    },
    # Turn 5: Agent edits
    {
        "role": "assistant",
        "content": "I'll edit the file to replace `.unwrap()` with `.unwrap_or()` and return an empty string.",
        "tool_calls": [{"id": "call_002", "type": "function", "function": {"name": "Edit", "arguments": json.dumps({"file_path": "/workspace/project/src/config.rs", "old_string": "self.headers.get(\"Authorization\").unwrap().clone()", "new_string": "self.headers.get(\"Authorization\").cloned().unwrap_or_default()"})}}]
    },
    {
        "role": "tool",
        "tool_call_id": "call_002",
        "content": "Edit applied successfully."
    },
    # Turn 6: Agent runs tests
    {
        "role": "assistant",
        "content": "Now let me run the tests to verify the fix.",
        "tool_calls": [{"id": "call_003", "type": "function", "function": {"name": "Bash", "arguments": json.dumps({"command": "cd /workspace/project && cargo test", "description": "Run all tests"})}}]
    },
    {
        "role": "tool",
        "tool_call_id": "call_003",
        "content": TEST_OUTPUT_FAIL
    },
    # Turn 7: Agent explains failure
    {
        "role": "assistant",
        "content": "The test `test_get_auth_header_missing` is still failing — but now it's because the test asserts `assert_eq!(result, \"\")` but the function returns `String::new()` via `unwrap_or_default()`. Wait, actually those should be equal. Let me look at the test more carefully."
    },
    # Turn 8: Agent reads test again with grep
    {
        "role": "user",
        "content": "Look at the test output more carefully. Is the test calling the OLD function before the edit was applied? Maybe cargo didn't recompile."
    },
    {
        "role": "assistant",
        "content": "Good point, let me search for any cached build artifacts and do a clean rebuild.",
        "tool_calls": [{"id": "call_004", "type": "function", "function": {"name": "Bash", "arguments": json.dumps({"command": "cd /workspace/project && cargo clean && cargo test 2>&1", "description": "Clean build and rerun tests"})}}]
    },
    {
        "role": "tool",
        "tool_call_id": "call_004",
        "content": TEST_OUTPUT_PASS
    },
    # Turn 9: Agent confirms fix
    {
        "role": "assistant",
        "content": "All tests pass now after a clean rebuild. The fix works:\n\n- Changed `get_auth_header()` from `.unwrap()` to `.cloned().unwrap_or_default()`\n- When Authorization header is missing, it now returns an empty string instead of panicking\n- Both tests pass: `test_get_auth_header` and `test_get_auth_header_missing`"
    },
    # Turn 10: User asks for summary
    {
        "role": "user",
        "content": "Great. Can you also check if there are any other .unwrap() calls on HashMap::get in the codebase that might have the same bug?"
    },
]


def send_turn(messages, tools, turn_num, label):
    """Send a request with the conversation so far."""
    full_messages = [{"role": "system", "content": SYSTEM}] + messages

    kwargs = {"model": MODEL, "messages": full_messages, "max_tokens": 300}
    if tools:
        kwargs["tools"] = tools

    start = time.time()
    try:
        response = client.chat.completions.create(**kwargs)
    except Exception as e:
        print(f"      ERROR: {e}")
        return None
    elapsed = time.time() - start

    usage = response.usage
    # Check for cached_tokens
    cached = 0
    if hasattr(usage, 'prompt_tokens_details') and usage.prompt_tokens_details:
        cached = getattr(usage.prompt_tokens_details, 'cached_tokens', 0) or 0

    result = {
        "turn": turn_num,
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
        "cached_tokens": cached,
        "elapsed": round(elapsed, 2),
        "msg_count": len(full_messages),
    }

    cache_str = f"  cached={cached}" if cached > 0 else ""
    print(f"      Turn {turn_num:>2}: prompt={usage.prompt_tokens:>6,}  completion={usage.completion_tokens:>4}{cache_str}  ({elapsed:.1f}s)  [{len(full_messages)} msgs]")
    return result


def run_session(tools, label):
    """Run the full 10-turn session with given tools."""
    print(f"\n  === {label} ({len(tools)} tools) ===")

    conversation = []
    results = []
    turn_num = 0

    for msg in TURNS:
        conversation.append(msg)

        # Only send to API when it's the user's turn (agent needs to respond)
        # or when we want to measure what the agent would receive
        if msg["role"] == "user":
            turn_num += 1
            result = send_turn(conversation, tools, turn_num, label)
            if result:
                results.append(result)
            time.sleep(1.5)  # Rate limit

    return results


def main():
    print("=" * 70)
    print("  merlint A/B Test — Full Multi-Turn Coding Session")
    print("=" * 70)
    print()
    print(f"  Model: {MODEL}")
    print(f"  Scenario: Fix unwrap() panic in Rust config module")
    print(f"  Session flow:")
    print(f"    Turn 1:  User reports bug")
    print(f"    Turn 2:  Agent reads file → sees buggy code")
    print(f"    Turn 3:  User confirms fix approach")
    print(f"    Turn 4:  Agent edits file → applies fix")
    print(f"    Turn 5:  Agent runs tests → test fails (stale cache)")
    print(f"    Turn 6:  User suggests clean rebuild")
    print(f"    Turn 7:  Agent runs clean build → all tests pass")
    print(f"    Turn 8:  User asks to check for similar bugs")
    print()
    print(f"  Each turn carries FULL conversation history")
    print(f"  Comparing: 22 tools (baseline) vs 4 tools (Read, Edit, Bash, Grep)")

    # Run baseline
    baseline_results = run_session(ALL_TOOLS, "Baseline")

    print("\n  --- Waiting 5s between sessions ---")
    time.sleep(5)

    # Run optimized
    optimized_results = run_session(USED_TOOLS, "Optimized")

    if not baseline_results or not optimized_results:
        print("  ERROR: Could not complete both sessions")
        return

    # ---- Results ----
    print()
    print("=" * 70)
    print("  PER-TURN COMPARISON")
    print("=" * 70)
    print()
    print(f"  {'Turn':<6} {'Baseline':>10} {'Optimized':>10} {'Saved':>8} {'%':>6} {'Cached(B)':>10} {'Cached(O)':>10}")
    print(f"  {'-'*6} {'-'*10} {'-'*10} {'-'*8} {'-'*6} {'-'*10} {'-'*10}")

    for b, o in zip(baseline_results, optimized_results):
        diff = b["prompt_tokens"] - o["prompt_tokens"]
        pct = diff / b["prompt_tokens"] * 100 if b["prompt_tokens"] > 0 else 0
        print(f"  {b['turn']:<6} {b['prompt_tokens']:>10,} {o['prompt_tokens']:>10,} {diff:>8,} {pct:>5.1f}% {b['cached_tokens']:>10,} {o['cached_tokens']:>10,}")

    total_b = sum(r["prompt_tokens"] for r in baseline_results)
    total_o = sum(r["prompt_tokens"] for r in optimized_results)
    total_saved = total_b - total_o
    total_pct = total_saved / total_b * 100 if total_b > 0 else 0

    print(f"  {'-'*6} {'-'*10} {'-'*10} {'-'*8} {'-'*6}")
    print(f"  {'TOTAL':<6} {total_b:>10,} {total_o:>10,} {total_saved:>8,} {total_pct:>5.1f}%")

    print()
    print("=" * 70)
    print("  SESSION SUMMARY")
    print("=" * 70)
    print()
    print(f"  Turns in session:        {len(baseline_results)}")
    print(f"  Tools (baseline):        {len(ALL_TOOLS)}")
    print(f"  Tools (optimized):       {len(USED_TOOLS)} ({', '.join(USED_NAMES)})")
    print(f"  Tools pruned:            {len(ALL_TOOLS) - len(USED_TOOLS)}")
    print()
    print(f"  Total prompt tokens (baseline):   {total_b:>10,}")
    print(f"  Total prompt tokens (optimized):  {total_o:>10,}")
    print(f"  Total tokens saved:               {total_saved:>10,}")
    print(f"  Average savings per turn:         {total_saved/len(baseline_results):>10,.0f} ({total_pct:.1f}%)")
    print()

    # Cache analysis
    total_cached_b = sum(r["cached_tokens"] for r in baseline_results)
    total_cached_o = sum(r["cached_tokens"] for r in optimized_results)
    if total_cached_b > 0 or total_cached_o > 0:
        print(f"  CACHE ANALYSIS:")
        print(f"  Total cached tokens (baseline):   {total_cached_b:>10,}")
        print(f"  Total cached tokens (optimized):  {total_cached_o:>10,}")
        print(f"  Cache hit rate (baseline):         {total_cached_b/total_b*100:.1f}%")
        print(f"  Cache hit rate (optimized):        {total_cached_o/total_o*100:.1f}%")
    else:
        print(f"  CACHE: No cached_tokens reported by API (may not be exposed on this endpoint)")

    print()

    # Cost projection
    # Kimi K2: $0.55/M input, $2.20/M output
    kimi_input_price = 0.55 / 1_000_000
    cost_saved_per_session = total_saved * kimi_input_price

    print(f"  COST PROJECTION (Kimi K2 pricing: $0.55/M input):")
    print(f"    This session saved: ${cost_saved_per_session:.4f}")
    print(f"    10 sessions/day:    ${cost_saved_per_session * 10:.4f}/day")
    print(f"    300 sessions/month: ${cost_saved_per_session * 300:.2f}/month")
    print()

    # Anthropic comparison
    # If these tokens were at Opus cache_read rate ($1.50/M):
    opus_saved = total_saved * 1.50 / 1_000_000
    opus_input_saved = total_saved * 15.0 / 1_000_000
    print(f"  IF ON ANTHROPIC (Opus pricing):")
    print(f"    At cache_read rate ($1.50/M):  ${opus_saved:.4f}/session → ${opus_saved * 300:.2f}/month")
    print(f"    At full input rate ($15/M):    ${opus_input_saved:.4f}/session → ${opus_input_saved * 300:.2f}/month")

    # Save results
    out = {
        "model": MODEL,
        "scenario": "Fix unwrap() panic in Rust config — 10-turn session",
        "baseline": {"tools": len(ALL_TOOLS), "turns": baseline_results, "total_prompt": total_b},
        "optimized": {"tools": len(USED_TOOLS), "tools_used": sorted(USED_NAMES), "turns": optimized_results, "total_prompt": total_o},
        "summary": {
            "total_saved": total_saved,
            "savings_pct": round(total_pct, 1),
            "avg_saved_per_turn": round(total_saved / len(baseline_results)),
            "cached_baseline": total_cached_b,
            "cached_optimized": total_cached_o,
        },
        "methodology": "True A/B test. Full multi-turn session with conversation history. Real API usage.",
    }
    out_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_session_results.json")
    with open(out_path, "w") as f:
        json.dump(out, f, indent=2)
    print(f"\n  Results saved to {out_path}")


if __name__ == "__main__":
    main()
