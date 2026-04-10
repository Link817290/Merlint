#!/usr/bin/env python3
"""
merlint A/B Test — True controlled experiment using Kimi API.

Sends TWO identical requests:
  A) With all 22 tool definitions (baseline)
  B) With only 8 commonly-used tools (optimized / pruned)

Same model, same messages, same system prompt.
Only difference: number of tool definitions.
Compares real usage.prompt_tokens from API response.
"""

import json
import os
import sys
import time
from openai import OpenAI

KIMI_API_KEY = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("KIMI_API_KEY", "")

client = OpenAI(
    api_key=KIMI_API_KEY,
    base_url="https://api.kimi.com/coding/v1",
    default_headers={"User-Agent": "claude-code/1.0"},
)

# ---- Tool definitions (OpenAI function calling format) ----

def make_tool(name, desc, params):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": params,
                "required": list(params.keys())[:1] if params else [],
            }
        }
    }

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
    make_tool("ExitPlanMode", "Exit planning mode and begin executing the plan.", {}),
    make_tool("EnterPlanMode", "Enter planning mode to think through a problem before acting.", {}),
    make_tool("ExitWorktree", "Exit the current git worktree and return to the main workspace.", {}),
    make_tool("EnterWorktree", "Enter a git worktree for isolated work.",
              {"branch": {"type": "string", "description": "Branch name for the worktree"}}),
    make_tool("CronCreate", "Create a scheduled cron job.",
              {"schedule": {"type": "string", "description": "Cron schedule expression"},
               "command": {"type": "string", "description": "Command to execute"}}),
    make_tool("CronDelete", "Delete an existing cron job by ID.",
              {"cron_id": {"type": "string", "description": "The ID of the cron job to delete"}}),
    make_tool("CronList", "List all active cron jobs.", {}),
    make_tool("TaskOutput", "Read the output of a background task.",
              {"task_id": {"type": "string", "description": "The ID of the background task"}}),
    make_tool("TaskStop", "Stop a running background task.",
              {"task_id": {"type": "string", "description": "The ID of the task to stop"}}),
]

USED_NAMES = {"Read", "Write", "Edit", "Bash", "Glob", "Grep", "Agent", "TodoWrite"}
USED_TOOLS = [t for t in ALL_TOOLS if t["function"]["name"] in USED_NAMES]

SYSTEM_PROMPT = (
    "You are a helpful coding assistant. Follow user instructions carefully. "
    "Use the available tools to accomplish tasks. Read files before editing them."
)

USER_MSG = "Read the file src/main.rs and tell me what it does. Keep your answer to one sentence."

MESSAGES = [
    {"role": "system", "content": SYSTEM_PROMPT},
    {"role": "user", "content": USER_MSG},
]


def run_request(tools, label):
    print(f"\n  [{label}]")
    print(f"  Tools: {len(tools)} ({', '.join(t['function']['name'] for t in tools)})")

    start = time.time()
    try:
        kwargs = {
            "model": "kimi-k2-0711-preview",
            "messages": MESSAGES,
            "max_tokens": 200,
        }
        if tools:
            kwargs["tools"] = tools
        response = client.chat.completions.create(**kwargs)
    except Exception as e:
        print(f"  ERROR: {e}")
        return None
    elapsed = time.time() - start

    usage = response.usage
    result = {
        "label": label,
        "tools_count": len(tools),
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
        "elapsed_sec": round(elapsed, 2),
    }

    print(f"  Prompt tokens:      {usage.prompt_tokens:,}")
    print(f"  Completion tokens:  {usage.completion_tokens:,}")
    print(f"  Total tokens:       {usage.total_tokens:,}")
    print(f"  Time:               {elapsed:.1f}s")

    return result


def main():
    print("=" * 60)
    print("  merlint A/B Test — Kimi (Moonshot) API")
    print("  True Controlled Experiment")
    print("=" * 60)
    print()
    print(f"  Model: kimi-k2-0711-preview")
    print(f"  Group A (Baseline):  {len(ALL_TOOLS)} tools")
    print(f"  Group B (Optimized): {len(USED_TOOLS)} tools")
    print(f"  Same system prompt, same user message")

    # Run baseline
    result_a = run_request(ALL_TOOLS, f"Baseline ({len(ALL_TOOLS)} tools)")
    if not result_a:
        return

    time.sleep(2)

    # Run optimized
    result_b = run_request(USED_TOOLS, f"Optimized ({len(USED_TOOLS)} tools)")
    if not result_b:
        return

    # Also run with NO tools as reference
    time.sleep(2)
    result_none = run_request([], f"No tools (reference)")

    # Compare
    print()
    print("=" * 60)
    print("  RESULTS")
    print("=" * 60)

    prompt_diff = result_a["prompt_tokens"] - result_b["prompt_tokens"]
    pct = prompt_diff / result_a["prompt_tokens"] * 100 if result_a["prompt_tokens"] > 0 else 0

    print(f"\n  {'Metric':<25} {'22 tools':>10} {'8 tools':>10} {'0 tools':>10} {'Diff(A-B)':>10}")
    print(f"  {'-'*25} {'-'*10} {'-'*10} {'-'*10} {'-'*10}")
    print(f"  {'Prompt tokens':<25} {result_a['prompt_tokens']:>10,} {result_b['prompt_tokens']:>10,} {result_none['prompt_tokens'] if result_none else 'N/A':>10} {prompt_diff:>10,}")
    print(f"  {'Completion tokens':<25} {result_a['completion_tokens']:>10,} {result_b['completion_tokens']:>10,} {result_none['completion_tokens'] if result_none else 'N/A':>10}")
    print(f"  {'Total tokens':<25} {result_a['total_tokens']:>10,} {result_b['total_tokens']:>10,} {result_none['total_tokens'] if result_none else 'N/A':>10}")

    print(f"\n  Prompt reduction from pruning 14 tools: {prompt_diff:,} tokens ({pct:.1f}%)")

    if result_none:
        tools_total = result_a["prompt_tokens"] - result_none["prompt_tokens"]
        tools_pruned = result_a["prompt_tokens"] - result_b["prompt_tokens"]
        print(f"  Total tool definition tokens (22 tools): {tools_total:,}")
        print(f"  Pruned tool tokens (14 tools):           {tools_pruned:,}")
        if tools_total > 0:
            print(f"  Pruned / Total tools ratio:              {tools_pruned/tools_total*100:.0f}%")

    # Cost estimate (Kimi pricing: ¥0.012/1K tokens for moonshot-v1-8k)
    kimi_price = 0.012 / 1000  # ¥/token
    cost_diff = prompt_diff * kimi_price

    print(f"\n  Per-call cost savings (Kimi): ¥{cost_diff:.4f}")
    print(f"  Over 100 calls: ¥{cost_diff * 100:.2f}")
    print(f"  Over 1000 calls: ¥{cost_diff * 1000:.2f}")

    # Save
    out = {
        "baseline": result_a,
        "optimized": result_b,
        "no_tools": result_none,
        "diff": {
            "tools_removed": len(ALL_TOOLS) - len(USED_TOOLS),
            "prompt_tokens_saved": prompt_diff,
            "prompt_reduction_pct": pct,
        },
        "methodology": "True A/B test on Kimi API. Same request, different tool counts.",
    }
    out_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_test_results.json")
    with open(out_path, "w") as f:
        json.dump(out, f, indent=2)
    print(f"\n  Results saved to {out_path}")


if __name__ == "__main__":
    main()
