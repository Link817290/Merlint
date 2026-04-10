#!/usr/bin/env python3
"""
merlint A/B Test — Real Coding Tasks

Runs REAL coding tasks through the API with full-length tool definitions.
Each task is a realistic coding agent scenario.

For each task:
  A) Send with all 22 tools (baseline — what coding agents normally do)
  B) Send with only the tools relevant to that task (optimized — what merlint does)

Reports real usage.prompt_tokens from the API.
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

# ========== FULL tool definitions (realistic Claude Code schemas) ==========

ALL_TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "Read",
            "description": "Reads a file from the local filesystem. You can access any file directly by using this tool. The file_path parameter must be an absolute path, not a relative path. By default, it reads up to 2000 lines starting from the beginning of the file. You can optionally specify a line offset and limit. Results are returned using cat -n format, with line numbers starting at 1. This tool can read images (PNG, JPG), PDFs, and Jupyter notebooks (.ipynb files).",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The absolute path to the file to read"},
                    "offset": {"type": "number", "description": "The line number to start reading from. Only provide if the file is too large to read at once"},
                    "limit": {"type": "number", "description": "The number of lines to read. Only provide if the file is too large to read at once."}
                },
                "required": ["file_path"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Write",
            "description": "Writes a file to the local filesystem. This tool will overwrite the existing file if there is one at the provided path. If this is an existing file, you MUST use the Read tool first to read the file's contents. Prefer the Edit tool for modifying existing files — it only sends the diff. Only use this tool to create new files or for complete rewrites. NEVER create documentation files (*.md) or README files unless explicitly requested.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The absolute path to the file to write (must be absolute, not relative)"},
                    "content": {"type": "string", "description": "The content to write to the file"}
                },
                "required": ["file_path", "content"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Edit",
            "description": "Performs exact string replacements in files. You must use your Read tool at least once before editing. When editing text from Read tool output, ensure you preserve the exact indentation. The edit will FAIL if old_string is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use replace_all to change every instance.",
            "parameters": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "The absolute path to the file to modify"},
                    "old_string": {"type": "string", "description": "The text to replace"},
                    "new_string": {"type": "string", "description": "The text to replace it with (must be different from old_string)"},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences of old_string (default false)", "default": False}
                },
                "required": ["file_path", "old_string", "new_string"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Bash",
            "description": "Executes a given bash command and returns its output. The working directory persists between commands, but shell state does not. Avoid using this tool to run find, grep, cat, head, tail, sed, awk, or echo commands — use dedicated tools instead. Always quote file paths that contain spaces. You may specify an optional timeout in milliseconds (up to 600000ms / 10 minutes). By default, your command will timeout after 120000ms.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command to execute"},
                    "timeout": {"type": "number", "description": "Optional timeout in milliseconds (max 600000)"},
                    "description": {"type": "string", "description": "Clear, concise description of what this command does"},
                    "run_in_background": {"type": "boolean", "description": "Set to true to run this command in the background"}
                },
                "required": ["command"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Glob",
            "description": "Find files matching a glob pattern in the filesystem. Use this tool instead of find or ls commands. Supports standard glob patterns like *.py, **/*.rs, src/**/test_*.py etc.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The glob pattern to match files against"},
                    "path": {"type": "string", "description": "The directory to search in (defaults to current working directory)"}
                },
                "required": ["pattern"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Grep",
            "description": "Search file contents using a regular expression pattern. Use this tool instead of grep or rg commands. Returns matching lines with file paths and line numbers. Supports regex patterns.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "The regex pattern to search for"},
                    "path": {"type": "string", "description": "The directory or file to search in"},
                    "include": {"type": "string", "description": "File pattern to include (e.g. '*.py')"},
                    "context": {"type": "number", "description": "Number of context lines to show around matches"}
                },
                "required": ["pattern"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Agent",
            "description": "Launch a sub-agent to handle a specific task independently. Use specialized agents when the task matches the agent's description. Subagents are valuable for parallelizing independent queries or for protecting the main context window from excessive results.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The task description for the sub-agent"},
                    "subagent_type": {"type": "string", "description": "Type of agent: 'Explore' for codebase exploration, or default for general tasks"}
                },
                "required": ["prompt"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "AskUserQuestion",
            "description": "Ask the user a question and wait for their response. Use this when you need clarification or approval before proceeding with an action.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question to ask the user"}
                },
                "required": ["question"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "TodoWrite",
            "description": "Create or update a todo list to track tasks and progress. Use this for planning your work and helping the user track your progress. Mark each task as completed as soon as you are done with the task.",
            "parameters": {
                "type": "object",
                "properties": {
                    "todos": {"type": "string", "description": "JSON array of todo items, each with 'task' (string) and 'status' ('pending'|'in_progress'|'done')"}
                },
                "required": ["todos"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "WebFetch",
            "description": "Fetch the content of a URL and return it as text. Useful for reading documentation, API references, or web pages.",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The URL to fetch"},
                    "prompt": {"type": "string", "description": "Optional instructions for processing the fetched content"}
                },
                "required": ["url"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "WebSearch",
            "description": "Search the web using a search engine query. Returns search result snippets with URLs. Use this for finding documentation, solutions to errors, or current information.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The search query"},
                    "num_results": {"type": "number", "description": "Number of results to return (default 5)"}
                },
                "required": ["query"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "NotebookEdit",
            "description": "Edit a Jupyter notebook cell by index. Can modify cell source code, change cell type, add new cells, or delete existing cells.",
            "parameters": {
                "type": "object",
                "properties": {
                    "notebook_path": {"type": "string", "description": "Absolute path to the .ipynb notebook file"},
                    "cell_index": {"type": "number", "description": "Index of the cell to edit (0-based)"},
                    "new_source": {"type": "string", "description": "New source code for the cell"},
                    "cell_type": {"type": "string", "description": "Cell type: 'code' or 'markdown'"}
                },
                "required": ["notebook_path", "cell_index", "new_source"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "Skill",
            "description": "Execute a predefined skill by name. Skills are user-defined automation scripts that can perform complex multi-step operations.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {"type": "string", "description": "Name of the skill to execute"},
                    "arguments": {"type": "string", "description": "JSON string of arguments to pass to the skill"}
                },
                "required": ["skill_name"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "ExitPlanMode",
            "description": "Exit planning mode and begin executing the plan. Use this after you have finished thinking through the problem.",
            "parameters": {"type": "object", "properties": {}}
        }
    },
    {
        "type": "function",
        "function": {
            "name": "EnterPlanMode",
            "description": "Enter planning mode to think through a complex problem before acting. In plan mode, you can reason about the approach without executing any tools.",
            "parameters": {"type": "object", "properties": {}}
        }
    },
    {
        "type": "function",
        "function": {
            "name": "ExitWorktree",
            "description": "Exit the current git worktree and return to the main workspace directory.",
            "parameters": {"type": "object", "properties": {}}
        }
    },
    {
        "type": "function",
        "function": {
            "name": "EnterWorktree",
            "description": "Enter a git worktree for isolated work on a separate branch without affecting the main workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "branch": {"type": "string", "description": "Branch name for the worktree"}
                },
                "required": ["branch"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "CronCreate",
            "description": "Create a scheduled cron job that runs a command at specified intervals.",
            "parameters": {
                "type": "object",
                "properties": {
                    "schedule": {"type": "string", "description": "Cron schedule expression (e.g. '*/5 * * * *' for every 5 minutes)"},
                    "command": {"type": "string", "description": "The command to execute on schedule"}
                },
                "required": ["schedule", "command"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "CronDelete",
            "description": "Delete an existing cron job by its ID.",
            "parameters": {
                "type": "object",
                "properties": {
                    "cron_id": {"type": "string", "description": "The ID of the cron job to delete"}
                },
                "required": ["cron_id"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "CronList",
            "description": "List all active cron jobs with their schedules and commands.",
            "parameters": {"type": "object", "properties": {}}
        }
    },
    {
        "type": "function",
        "function": {
            "name": "TaskOutput",
            "description": "Read the output of a background task that was started with run_in_background.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "The ID of the background task"}
                },
                "required": ["task_id"]
            }
        }
    },
    {
        "type": "function",
        "function": {
            "name": "TaskStop",
            "description": "Stop a running background task.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string", "description": "The ID of the task to stop"}
                },
                "required": ["task_id"]
            }
        }
    },
]

# ========== Real coding tasks ==========

SYSTEM_PROMPT = """You are a coding assistant. You help users with software engineering tasks like writing code, debugging, and refactoring. Use the available tools to read files, write code, run commands, and search the codebase. Always read existing files before modifying them."""

TASKS = [
    {
        "name": "Bug fix: fix a null pointer in Rust",
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": "I'm getting a panic in my Rust code: 'called `Option::unwrap()` on a `None` value' at src/proxy/server.rs line 142. The function `handle_request` is trying to unwrap a header value that might not exist. Can you read the file and fix the bug? Use `.unwrap_or_default()` or proper error handling instead of `.unwrap()`."},
        ],
        "tools_needed": ["Read", "Edit", "Bash"],  # Only need to read, edit, and maybe run tests
    },
    {
        "name": "Code review: review a PR diff",
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": "Review these changes I made to the database module. I added a new `store_session` method. Here's the diff:\n\n```\n+    pub fn store_session(&self, session: &SessionData) -> Result<()> {\n+        let conn = self.pool.get()?;\n+        conn.execute(\n+            \"INSERT INTO sessions (id, tokens, cost) VALUES (?1, ?2, ?3)\",\n+            params![session.id, session.tokens, session.cost],\n+        )?;\n+        Ok(())\n+    }\n```\n\nAny issues? Is it safe from SQL injection? Should I add error handling?"},
        ],
        "tools_needed": ["Read", "Grep"],  # Might read related files, search for usage patterns
    },
    {
        "name": "New feature: add CLI argument",
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": "Add a `--json` flag to the `analyze` subcommand in my CLI tool. When passed, it should output the analysis results as JSON instead of the formatted table. The CLI is built with clap 4 in src/main.rs. The analyze command currently calls `print_analysis()` — add a branch that calls `serde_json::to_string_pretty()` on the result when --json is set."},
        ],
        "tools_needed": ["Read", "Edit", "Glob", "Bash"],  # Read files, edit, find related files, test
    },
    {
        "name": "Debug: investigate test failure",
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": "My test `test_transformer_dedup` is failing with:\n\n```\nthread 'proxy::transformer::tests::test_transformer_dedup' panicked at 'assertion failed: `(left == right)`\n  left: `5`,\n  right: `4`'\n```\n\nThe test is in src/proxy/transformer.rs. It tests the dedup_tool_results function. Can you look at the test and the function to figure out why the count is off by one?"},
        ],
        "tools_needed": ["Read", "Grep", "Bash"],  # Read test, search for function, run test
    },
    {
        "name": "Refactor: extract function",
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": "In src/main.rs there's a big match block in the `main()` function that handles each CLI command. The `Scan` arm is 45 lines long. Extract the scan logic into a separate `handle_scan()` function. Keep the same behavior, just move the code. Make sure it compiles."},
        ],
        "tools_needed": ["Read", "Edit", "Bash"],
    },
]


def run_request(tools, messages, label):
    """Send one request and return usage."""
    print(f"    [{label}] {len(tools)} tools...", end=" ", flush=True)
    start = time.time()
    try:
        kwargs = {"model": MODEL, "messages": messages, "max_tokens": 500}
        if tools:
            kwargs["tools"] = tools
        response = client.chat.completions.create(**kwargs)
    except Exception as e:
        print(f"ERROR: {e}")
        return None
    elapsed = time.time() - start

    usage = response.usage
    result = {
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
        "elapsed_sec": round(elapsed, 2),
    }
    print(f"prompt={usage.prompt_tokens:,}  completion={usage.completion_tokens:,}  ({elapsed:.1f}s)")
    return result


def main():
    print("=" * 70)
    print("  merlint A/B Test — Real Coding Tasks")
    print("  True controlled experiment on Kimi K2 API")
    print("=" * 70)
    print()
    print(f"  Model: {MODEL}")
    print(f"  Tasks: {len(TASKS)} real coding scenarios")
    print(f"  For each task: send with ALL 22 tools vs ONLY needed tools")
    print()

    results = []

    for i, task in enumerate(TASKS):
        needed_names = set(task["tools_needed"])
        needed_tools = [t for t in ALL_TOOLS if t["function"]["name"] in needed_names]
        not_needed = len(ALL_TOOLS) - len(needed_tools)

        print(f"  Task {i+1}: {task['name']}")
        print(f"    Tools needed: {', '.join(task['tools_needed'])} ({len(needed_tools)} tools)")
        print(f"    Tools prunable: {not_needed}")

        # A: baseline (all tools)
        result_a = run_request(ALL_TOOLS, task["messages"], "Baseline (22 tools)")
        time.sleep(2)

        # B: optimized (only needed tools)
        result_b = run_request(needed_tools, task["messages"], f"Optimized ({len(needed_tools)} tools)")
        time.sleep(2)

        if result_a and result_b:
            diff = result_a["prompt_tokens"] - result_b["prompt_tokens"]
            pct = diff / result_a["prompt_tokens"] * 100
            print(f"    >> Saved: {diff:,} prompt tokens ({pct:.1f}%)")
            results.append({
                "task": task["name"],
                "tools_needed": task["tools_needed"],
                "tools_pruned": not_needed,
                "baseline": result_a,
                "optimized": result_b,
                "tokens_saved": diff,
                "savings_pct": round(pct, 1),
            })
        print()

    if not results:
        print("  No results collected.")
        return

    # ---- Summary ----
    print("=" * 70)
    print("  SUMMARY — All Tasks")
    print("=" * 70)
    print()
    print(f"  {'Task':<40} {'Baseline':>9} {'Optimized':>9} {'Saved':>8} {'%':>6}")
    print(f"  {'-'*40} {'-'*9} {'-'*9} {'-'*8} {'-'*6}")

    total_baseline = 0
    total_optimized = 0
    for r in results:
        total_baseline += r["baseline"]["prompt_tokens"]
        total_optimized += r["optimized"]["prompt_tokens"]
        print(f"  {r['task']:<40} {r['baseline']['prompt_tokens']:>9,} {r['optimized']['prompt_tokens']:>9,} {r['tokens_saved']:>8,} {r['savings_pct']:>5.1f}%")

    total_saved = total_baseline - total_optimized
    total_pct = total_saved / total_baseline * 100 if total_baseline > 0 else 0
    print(f"  {'-'*40} {'-'*9} {'-'*9} {'-'*8} {'-'*6}")
    print(f"  {'TOTAL':<40} {total_baseline:>9,} {total_optimized:>9,} {total_saved:>8,} {total_pct:>5.1f}%")
    print()

    avg_saved = total_saved / len(results)
    print(f"  Average tokens saved per task: {avg_saved:,.0f}")
    print(f"  Average savings: {total_pct:.1f}%")
    print()

    # Extrapolate
    print("  EXTRAPOLATION:")
    for calls, label in [(100, "100-call session"), (1000, "1000-call session"), (5000, "5000-call session")]:
        opus_save = avg_saved * calls * 1.50 / 1_000_000  # cache_read rate
        sonnet_save = avg_saved * calls * 0.30 / 1_000_000
        print(f"    {label}: ~{avg_saved * calls:,.0f} tokens saved → ${opus_save:.2f} (Opus) / ${sonnet_save:.2f} (Sonnet)")
    print()

    # Save
    out_data = {
        "model": MODEL,
        "tasks": results,
        "summary": {
            "total_baseline": total_baseline,
            "total_optimized": total_optimized,
            "total_saved": total_saved,
            "savings_pct": round(total_pct, 1),
            "avg_saved_per_task": round(avg_saved),
        },
        "methodology": "True A/B test. Same messages, different tool counts. Real API usage data.",
    }
    out_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ab_real_tasks_results.json")
    with open(out_path, "w") as f:
        json.dump(out_data, f, indent=2)
    print(f"  Results saved to {out_path}")


if __name__ == "__main__":
    main()
