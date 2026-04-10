#!/usr/bin/env python3
"""
merlint A/B Test — True controlled experiment.

Sends TWO identical requests to the Anthropic API:
  A) With all 22 Claude Code tool definitions (baseline)
  B) With only 8 commonly-used tools (optimized / pruned)

Same model, same messages, same system prompt.
Only difference: number of tool definitions.
Different tool sets → different cache keys → no cache advantage.

Compares the real `usage` data returned by the API.
"""

import json
import os
import sys
import time

try:
    import anthropic
except ImportError:
    print("Installing anthropic SDK...")
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "anthropic", "-q"])
    import anthropic


# ---- Tool definitions (realistic Claude Code schemas) ----

def make_tool(name, desc, params):
    """Create an Anthropic-format tool definition."""
    return {
        "name": name,
        "description": desc,
        "input_schema": {
            "type": "object",
            "properties": params,
            "required": list(params.keys())[:1] if params else [],
        }
    }


# All 22 Claude Code tools with realistic descriptions and parameters
ALL_TOOLS = [
    make_tool("Read", "Read a file from the local filesystem. Returns the file content with line numbers.",
              {"file_path": {"type": "string", "description": "The absolute path to the file to read"},
               "offset": {"type": "number", "description": "Line number to start reading from"},
               "limit": {"type": "number", "description": "Number of lines to read"}}),
    make_tool("Write", "Write content to a file on the local filesystem. Creates or overwrites the file.",
              {"file_path": {"type": "string", "description": "The absolute path to write to"},
               "content": {"type": "string", "description": "The content to write"}}),
    make_tool("Edit", "Edit a file by replacing a specific string with another string.",
              {"file_path": {"type": "string", "description": "The absolute path to the file"},
               "old_string": {"type": "string", "description": "The text to find and replace"},
               "new_string": {"type": "string", "description": "The replacement text"}}),
    make_tool("Bash", "Execute a bash command in the shell and return its output.",
              {"command": {"type": "string", "description": "The bash command to execute"},
               "timeout": {"type": "number", "description": "Timeout in milliseconds"},
               "description": {"type": "string", "description": "Description of what the command does"}}),
    make_tool("Glob", "Find files matching a glob pattern in the filesystem.",
              {"pattern": {"type": "string", "description": "The glob pattern to match"},
               "path": {"type": "string", "description": "The directory to search in"}}),
    make_tool("Grep", "Search file contents using a regular expression pattern.",
              {"pattern": {"type": "string", "description": "The regex pattern to search for"},
               "path": {"type": "string", "description": "The directory to search in"},
               "include": {"type": "string", "description": "File pattern to include"}}),
    make_tool("Agent", "Launch a sub-agent to handle a specific task independently.",
              {"prompt": {"type": "string", "description": "The task description for the sub-agent"},
               "subagent_type": {"type": "string", "description": "Type of agent: Explore or default"}}),
    make_tool("AskUserQuestion", "Ask the user a question and wait for their response.",
              {"question": {"type": "string", "description": "The question to ask the user"}}),
    make_tool("TodoWrite", "Create or update a todo list to track tasks and progress.",
              {"todos": {"type": "string", "description": "JSON array of todo items with status"}}),
    make_tool("WebFetch", "Fetch the content of a URL and return it as text.",
              {"url": {"type": "string", "description": "The URL to fetch"},
               "prompt": {"type": "string", "description": "Instructions for processing the content"}}),
    make_tool("WebSearch", "Search the web using a search engine query.",
              {"query": {"type": "string", "description": "The search query"},
               "num_results": {"type": "number", "description": "Number of results to return"}}),
    make_tool("NotebookEdit", "Edit a Jupyter notebook cell by index.",
              {"notebook_path": {"type": "string", "description": "Path to the notebook file"},
               "cell_index": {"type": "number", "description": "Index of the cell to edit"},
               "new_source": {"type": "string", "description": "New source code for the cell"}}),
    make_tool("Skill", "Execute a predefined skill by name.",
              {"skill_name": {"type": "string", "description": "Name of the skill to execute"},
               "arguments": {"type": "string", "description": "Arguments to pass to the skill"}}),
    make_tool("ExitPlanMode", "Exit planning mode and begin executing the plan.",
              {}),
    make_tool("EnterPlanMode", "Enter planning mode to think through a problem before acting.",
              {}),
    make_tool("ExitWorktree", "Exit the current git worktree and return to the main workspace.",
              {}),
    make_tool("EnterWorktree", "Enter a git worktree for isolated work.",
              {"branch": {"type": "string", "description": "Branch name for the worktree"}}),
    make_tool("CronCreate", "Create a scheduled cron job.",
              {"schedule": {"type": "string", "description": "Cron schedule expression"},
               "command": {"type": "string", "description": "Command to execute"}}),
    make_tool("CronDelete", "Delete an existing cron job by ID.",
              {"cron_id": {"type": "string", "description": "The ID of the cron job to delete"}}),
    make_tool("CronList", "List all active cron jobs.",
              {}),
    make_tool("TaskOutput", "Read the output of a background task.",
              {"task_id": {"type": "string", "description": "The ID of the background task"}}),
    make_tool("TaskStop", "Stop a running background task.",
              {"task_id": {"type": "string", "description": "The ID of the task to stop"}}),
]

# Tools that are commonly used (what optimizer would keep)
USED_TOOL_NAMES = {"Read", "Write", "Edit", "Bash", "Glob", "Grep", "Agent", "TodoWrite"}
USED_TOOLS = [t for t in ALL_TOOLS if t["name"] in USED_TOOL_NAMES]

# Same conversation for both requests
SYSTEM_PROMPT = (
    "You are a helpful coding assistant. Follow user instructions carefully. "
    "Use the available tools to accomplish tasks. Read files before editing them."
)

MESSAGES = [
    {"role": "user", "content": "Read the file /workspace/merlint/src/main.rs and tell me what it does. Keep your answer brief."},
]


def run_request(client, tools, label):
    """Send a request and return usage data."""
    print(f"\n  Sending request: {label}")
    print(f"  Tools defined: {len(tools)} ({', '.join(t['name'] for t in tools)})")

    start = time.time()
    response = client.messages.create(
        model="claude-sonnet-4-20250514",  # Use Sonnet to keep costs low
        max_tokens=300,
        system=SYSTEM_PROMPT,
        messages=MESSAGES,
        tools=tools,
    )
    elapsed = time.time() - start

    usage = response.usage
    result = {
        "label": label,
        "tools_count": len(tools),
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_creation": getattr(usage, "cache_creation_input_tokens", 0) or 0,
        "cache_read": getattr(usage, "cache_read_input_tokens", 0) or 0,
        "elapsed_sec": round(elapsed, 2),
        "stop_reason": response.stop_reason,
    }

    total_prompt = result["input_tokens"] + result["cache_creation"] + result["cache_read"]
    result["total_prompt"] = total_prompt
    result["total_tokens"] = total_prompt + result["output_tokens"]

    print(f"  Input tokens:         {result['input_tokens']:,}")
    print(f"  Cache creation:       {result['cache_creation']:,}")
    print(f"  Cache read:           {result['cache_read']:,}")
    print(f"  Total prompt tokens:  {total_prompt:,}")
    print(f"  Output tokens:        {result['output_tokens']:,}")
    print(f"  Time: {elapsed:.1f}s")

    return result


def main():
    client = anthropic.Anthropic()

    print("=" * 60)
    print("  merlint A/B Test — True Controlled Experiment")
    print("=" * 60)
    print()
    print("  Model: claude-sonnet-4-20250514")
    print(f"  Group A: {len(ALL_TOOLS)} tools (baseline)")
    print(f"  Group B: {len(USED_TOOLS)} tools (pruned)")
    print(f"  Same system prompt, same user message")
    print(f"  Different tool sets = different cache keys")

    # Run A first
    result_a = run_request(client, ALL_TOOLS, f"Baseline ({len(ALL_TOOLS)} tools)")

    # Wait a moment to avoid rate limits
    print("\n  Waiting 3s between requests...")
    time.sleep(3)

    # Run B
    result_b = run_request(client, USED_TOOLS, f"Optimized ({len(USED_TOOLS)} tools)")

    # Compare
    print()
    print("=" * 60)
    print("  RESULTS")
    print("=" * 60)

    prompt_diff = result_a["total_prompt"] - result_b["total_prompt"]
    total_diff = result_a["total_tokens"] - result_b["total_tokens"]

    print(f"\n  {'Metric':<25} {'Baseline':>10} {'Optimized':>10} {'Diff':>10}")
    print(f"  {'-'*25} {'-'*10} {'-'*10} {'-'*10}")
    print(f"  {'Tools defined':<25} {result_a['tools_count']:>10} {result_b['tools_count']:>10} {result_a['tools_count'] - result_b['tools_count']:>10}")
    print(f"  {'Input tokens':<25} {result_a['input_tokens']:>10,} {result_b['input_tokens']:>10,} {result_a['input_tokens'] - result_b['input_tokens']:>10,}")
    print(f"  {'Cache creation':<25} {result_a['cache_creation']:>10,} {result_b['cache_creation']:>10,} {result_a['cache_creation'] - result_b['cache_creation']:>10,}")
    print(f"  {'Cache read':<25} {result_a['cache_read']:>10,} {result_b['cache_read']:>10,} {result_a['cache_read'] - result_b['cache_read']:>10,}")
    print(f"  {'Total prompt':<25} {result_a['total_prompt']:>10,} {result_b['total_prompt']:>10,} {prompt_diff:>10,}")
    print(f"  {'Output tokens':<25} {result_a['output_tokens']:>10,} {result_b['output_tokens']:>10,} {result_a['output_tokens'] - result_b['output_tokens']:>10,}")
    print(f"  {'Total tokens':<25} {result_a['total_tokens']:>10,} {result_b['total_tokens']:>10,} {total_diff:>10,}")

    if result_a["total_prompt"] > 0:
        pct = prompt_diff / result_a["total_prompt"] * 100
        print(f"\n  Prompt token reduction: {prompt_diff:,} tokens ({pct:.1f}%)")

    # Cost calculation (Sonnet pricing)
    input_price = 3.0 / 1_000_000
    cache_create_price = 3.75 / 1_000_000
    cache_read_price = 0.30 / 1_000_000
    output_price = 15.0 / 1_000_000

    cost_a = (result_a["input_tokens"] * input_price +
              result_a["cache_creation"] * cache_create_price +
              result_a["cache_read"] * cache_read_price +
              result_a["output_tokens"] * output_price)

    cost_b = (result_b["input_tokens"] * input_price +
              result_b["cache_creation"] * cache_create_price +
              result_b["cache_read"] * cache_read_price +
              result_b["output_tokens"] * output_price)

    print(f"\n  Baseline cost:   ${cost_a:.6f}")
    print(f"  Optimized cost:  ${cost_b:.6f}")
    print(f"  Saved:           ${cost_a - cost_b:.6f} ({(cost_a - cost_b)/cost_a*100:.1f}%)" if cost_a > 0 else "")

    print(f"\n  Tools removed: {len(ALL_TOOLS) - len(USED_TOOLS)}")
    removed = [t["name"] for t in ALL_TOOLS if t["name"] not in USED_TOOL_NAMES]
    print(f"  Removed: {', '.join(removed)}")

    # Extrapolate to a full session
    print()
    print("=" * 60)
    print("  EXTRAPOLATION (if applied to a 100-call session)")
    print("=" * 60)
    print(f"  Tokens saved per call:  {prompt_diff:,}")
    print(f"  Over 100 calls:         {prompt_diff * 100:,} tokens")
    print(f"  Cost saved (Sonnet):    ${(cost_a - cost_b) * 100:.4f}")

    # At Opus pricing
    opus_input = 15.0 / 1_000_000
    opus_cache_create = 18.75 / 1_000_000
    opus_cache_read = 1.50 / 1_000_000
    opus_output = 75.0 / 1_000_000

    opus_a = (result_a["input_tokens"] * opus_input +
              result_a["cache_creation"] * opus_cache_create +
              result_a["cache_read"] * opus_cache_read +
              result_a["output_tokens"] * opus_output)
    opus_b = (result_b["input_tokens"] * opus_input +
              result_b["cache_creation"] * opus_cache_create +
              result_b["cache_read"] * opus_cache_read +
              result_b["output_tokens"] * opus_output)

    print(f"  Cost saved (Opus):      ${(opus_a - opus_b) * 100:.4f}")

    # Save results
    out = {
        "baseline": result_a,
        "optimized": result_b,
        "diff": {
            "tools_removed": len(ALL_TOOLS) - len(USED_TOOLS),
            "prompt_tokens_saved": prompt_diff,
            "prompt_reduction_pct": prompt_diff / result_a["total_prompt"] * 100 if result_a["total_prompt"] > 0 else 0,
            "cost_saved_sonnet": cost_a - cost_b,
            "cost_saved_opus_per_100_calls": (opus_a - opus_b) * 100,
        },
        "methodology": "True A/B test: same request sent twice with different tool sets. Different cache keys, no cache advantage.",
    }

    out_path = os.path.join(os.path.dirname(__file__), "ab_test_results.json")
    with open(out_path, "w") as f:
        json.dump(out, f, indent=2)
    print(f"\n  Results saved to {out_path}")


if __name__ == "__main__":
    main()
