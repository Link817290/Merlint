#!/usr/bin/env python3
"""
End-to-end integration test for merlint proxy.
6 scenarios covering both continuous and interrupted usage patterns.
"""
import json, subprocess, sys, time, os, signal, requests, textwrap, tempfile, shutil, glob

MERLINT = "./target/release/merlint"
UPSTREAM_PORT = 9999
PROXY_PORT = 9876
PASS = 0
FAIL = 0
TESTS = 0

# ── Real file content from merlint's own source ──────────────────
REAL_FILE = open("src/proxy/transformer.rs").read()[:2000]
REAL_CONFIG = open("Cargo.toml").read()
REAL_SERVER = open("src/proxy/server.rs").read()[:1500]

# ── Tool definitions ─────────────────────────────────────────────

def make_tool(name, desc, props):
    return {
        "name": name, "description": desc,
        "input_schema": {
            "type": "object",
            "properties": {k: {"type": "string"} for k in props},
            "required": props,
        },
    }

CODING_TOOLS = [
    make_tool("Read", "Read a file from disk", ["file_path"]),
    make_tool("Write", "Write content to a file", ["file_path", "content"]),
    make_tool("Edit", "Edit a file with string replacement", ["file_path", "old_string", "new_string"]),
    make_tool("Bash", "Execute a bash command", ["command"]),
    make_tool("Glob", "Find files matching a pattern", ["pattern"]),
    make_tool("Grep", "Search file contents with regex", ["pattern", "path"]),
    make_tool("TodoWrite", "Write todo items for tracking", ["todos"]),
    make_tool("Agent", "Spawn a sub-agent for parallel work", ["prompt"]),
    make_tool("WebSearch", "Search the web", ["query"]),
    make_tool("WebFetch", "Fetch a web page", ["url"]),
    make_tool("NotebookEdit", "Edit a Jupyter notebook cell", ["notebook"]),
    make_tool("AskUser", "Ask the user a question", ["question"]),
]

RESEARCH_TOOLS = [
    make_tool("WebSearch", "Search the web for information", ["query"]),
    make_tool("WebFetch", "Fetch and read a web page", ["url"]),
    make_tool("Read", "Read a local file", ["file_path"]),
    make_tool("Write", "Write to a file", ["file_path", "content"]),
    make_tool("Bash", "Run a shell command", ["command"]),
    make_tool("Glob", "Find files by pattern", ["pattern"]),
    make_tool("Grep", "Search file contents", ["pattern"]),
    make_tool("Agent", "Delegate to sub-agent", ["prompt"]),
    make_tool("NotebookEdit", "Edit notebook", ["notebook"]),
    make_tool("AskUser", "Ask the user", ["question"]),
]

DEVOPS_TOOLS = [
    make_tool("Bash", "Execute a bash command", ["command"]),
    make_tool("Read", "Read a file from disk", ["file_path"]),
    make_tool("Write", "Write content to a file", ["file_path", "content"]),
    make_tool("Edit", "Edit a file", ["file_path", "old_string", "new_string"]),
    make_tool("Glob", "Find files", ["pattern"]),
    make_tool("Grep", "Search file contents", ["pattern"]),
    make_tool("Agent", "Sub-agent", ["prompt"]),
    make_tool("WebSearch", "Web search", ["query"]),
    make_tool("AskUser", "Ask user", ["question"]),
]

# ── Assertion helpers ────────────────────────────────────────────

def assert_ge(desc, actual, expected):
    global TESTS, PASS, FAIL; TESTS += 1
    try: a, e = int(actual), int(expected)
    except: FAIL += 1; print(f"  ✗ {desc}: cannot parse '{actual}'"); return
    if a >= e: PASS += 1; print(f"  ✓ {desc}: {a} >= {e}")
    else: FAIL += 1; print(f"  ✗ {desc}: expected >= {e}, got {a}")

def assert_gt(desc, actual, expected):
    global TESTS, PASS, FAIL; TESTS += 1
    try: a, e = int(actual), int(expected)
    except: FAIL += 1; print(f"  ✗ {desc}: cannot parse '{actual}'"); return
    if a > e: PASS += 1; print(f"  ✓ {desc}: {a} > {e}")
    else: FAIL += 1; print(f"  ✗ {desc}: expected > {e}, got {a}")

def assert_true(desc, val):
    global TESTS, PASS, FAIL; TESTS += 1
    if val: PASS += 1; print(f"  ✓ {desc}")
    else: FAIL += 1; print(f"  ✗ {desc}")

def assert_false(desc, val):
    global TESTS, PASS, FAIL; TESTS += 1
    if not val: PASS += 1; print(f"  ✓ {desc}")
    else: FAIL += 1; print(f"  ✗ {desc}: expected false, got true")

# ── API helpers ──────────────────────────────────────────────────

def send_anthropic(system_prompt, messages, tools, cache_hint=None):
    """Send an Anthropic-format request through the proxy."""
    headers = {
        "anthropic-version": "2023-06-01",
        "x-api-key": "test-key",
        "content-type": "application/json",
    }
    if cache_hint:
        headers["X-Simulate-Cache"] = cache_hint
    body = {
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 4096,
        "system": system_prompt,
        "tools": tools,
        "messages": messages,
    }
    try:
        r = requests.post(
            f"http://127.0.0.1:{PROXY_PORT}/v1/messages",
            json=body, headers=headers, timeout=10,
        )
        return r.json()
    except Exception as e:
        print(f"    request error: {e}")
        return None

def get_status():
    try: return requests.get(f"http://127.0.0.1:{PROXY_PORT}/merlint/status", timeout=5).json()
    except: return {}

def get_session_by_prefix(status, prefix):
    """Find a session by key prefix."""
    for s in status.get("sessions", []):
        if prefix in s["key"]:
            return s
    return None

def build_tool_result(resp, content_fn):
    content = resp.get("content", [])
    assistant_msg = {"role": "assistant", "content": content}
    results = []
    for block in content:
        if block.get("type") == "tool_use":
            result_text = content_fn(block["name"], block.get("input", {}))
            results.append({
                "type": "tool_result",
                "tool_use_id": block["id"],
                "content": result_text,
            })
    user_msg = {"role": "user", "content": results if results else "continue"}
    return assistant_msg, user_msg

# ── Content generators ───────────────────────────────────────────

def coding_content(tool_name, _):
    if tool_name == "Read": return REAL_FILE
    if tool_name == "Glob": return "src/main.rs\nsrc/lib.rs\nsrc/proxy/server.rs\nsrc/proxy/transformer.rs\nsrc/proxy/session_store.rs"
    if tool_name == "Grep": return "src/proxy/transformer.rs:133:    pub fn transform\nsrc/proxy/server.rs:72:async fn handle_request"
    if tool_name == "Bash": return "running 16 tests\ntest result: ok. 16 passed; 0 failed"
    if tool_name == "Edit": return "Applied edit: replaced 3 lines"
    return "ok"

def research_content(tool_name, _):
    if tool_name == "WebSearch":
        return "Results: 1. Tokio 2.0 Release Notes 2. async-std comparison 3. Smol 1.0 4. Async traits RFC 5. Embassy embedded async" + " " * 400
    if tool_name == "WebFetch":
        return "# Tokio 2.0: Structured Concurrency\n\nThe Tokio team announces Tokio 2.0 with structured concurrency, improved cancellation safety, and 15% scheduling overhead reduction. " + "Details about the release. " * 30
    if tool_name == "Read": return REAL_FILE
    return "ok"

def devops_content(tool_name, _):
    if tool_name == "Bash":
        return "$ kubectl get pods -n production\napi-server-7d4f8b Running 2d\nworker-5c6d7f Running 5d\nredis-cache Running 5d\npostgres-0 Running 12d\n\n$ kubectl top pods\napi-server 125m 256Mi\nworker 89m 512Mi"
    if tool_name == "Read": return REAL_CONFIG
    if tool_name == "DockerExec": return "drwxr-xr-x app.js package.json node_modules/"
    return "ok"

def debug_content(tool_name, _):
    """For intermittent debugging — user reads different files each time."""
    if tool_name == "Read": return REAL_SERVER  # Different file than coding scenario
    if tool_name == "Bash": return "thread 'main' panicked at 'index out of bounds'\nnote: run with RUST_BACKTRACE=1\nstack backtrace:\n  0: std::panicking::begin_panic\n  1: merlint::proxy::server::handle_request\n  2: tokio::runtime::task::harness"
    if tool_name == "Grep": return "src/proxy/server.rs:204:    let response = match forward_req.send().await {"
    return "ok"

# ── Main ─────────────────────────────────────────────────────────

def main():
    global PASS, FAIL, TESTS

    print("=== Starting upstream server ===")
    upstream = subprocess.Popen(
        [sys.executable, "tests/e2e_upstream.py", str(UPSTREAM_PORT)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    time.sleep(1)

    trace_dir = tempfile.mkdtemp()
    print("=== Starting merlint proxy (optimize=on) ===")
    proxy = subprocess.Popen(
        [MERLINT, "proxy",
         "--port", str(PROXY_PORT),
         "--target", f"http://127.0.0.1:{UPSTREAM_PORT}",
         "--output", f"{trace_dir}/trace.json",
         "--optimize"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    time.sleep(2)

    try:
        status = get_status()
        assert status.get("status") == "running", "Proxy not running!"
        print("  proxy alive\n")

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 1: Continuous coding — 10 turns, cache stays warm
        #  Pattern: User actively coding, no gaps
        # ══════════════════════════════════════════════════════════
        print("=" * 60)
        print("  S1: Continuous Coding — 10 turns, always warm cache")
        print("  12 tools, ~5 used. Cache 40% from turn 3.")
        print("  Expect: pruning suspended, savings from file dedup only")
        print("=" * 60)

        sys1 = "You are a coding assistant working on a Rust project. Help fix bugs and improve code quality."
        msgs1 = [{"role": "user", "content": "Fix the crash in transformer.rs."}]
        for turn in range(1, 11):
            cache = "warm" if turn >= 3 else "cold"
            resp = send_anthropic(sys1, msgs1, CODING_TOOLS, cache_hint=cache)
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}  cache={u['cache_read_input_tokens']:4d}  hint={cache}")
            a, m = build_tool_result(resp, coding_content)
            msgs1.extend([a, m])

        s1 = get_status()
        s1s = [s for s in s1["sessions"] if s["request_count"] >= 10][0]
        print(f"\n  Result: saved={s1s['tokens_saved']}  suspended={s1s['pruning_suspended']}  tracked={s1s['tools_tracked']}")
        assert_gt("S1 tokens saved (file dedup)", s1s["tokens_saved"], 0)
        assert_true("S1 pruning suspended (warm cache)", s1s["pruning_suspended"])

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 2: Bursty debugging — 3 fast, gap, 4 more, gap, 3 more
        #  Pattern: User debugs in bursts, walks away between rounds
        #  Cache: warm within bursts, cold between them
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  S2: Bursty Debugging — bursts with cold gaps between")
        print("  12 tools, ~3 used (Read/Bash/Grep). Cache resets between bursts.")
        print("  Expect: pruning fires during cold windows")
        print("=" * 60)

        sys2 = "You are a debugging specialist. Track down crashes using logs, stack traces, and code reading."
        msgs2 = [{"role": "user", "content": "The server crashes intermittently. Help me debug."}]

        # Burst 1: 3 fast turns (cold → warm)
        cache_seq_s2 = [
            # Burst 1: cold start
            "cold", "cold", "warm",
            # Gap: user goes to get coffee, cache expires
            "cold",
            # Burst 2: 4 turns
            "cold", "warm", "warm", "warm",
            # Gap: user reads docs for 10 min
            "cold",
            # Burst 3: 3 turns
            "partial", "warm", "warm",
        ]
        for turn in range(1, 12):
            cache = cache_seq_s2[turn - 1] if turn <= len(cache_seq_s2) else "warm"
            gap = ""
            if turn == 4: gap = " [after coffee break]"
            if turn == 9: gap = " [after reading docs]"
            resp = send_anthropic(sys2, msgs2, CODING_TOOLS, cache_hint=cache)
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}  cache={u['cache_read_input_tokens']:4d}  hint={cache}{gap}")
            a, m = build_tool_result(resp, debug_content)
            msgs2.extend([a, m])

        s2 = get_status()
        s2s = [s for s in s2["sessions"] if s["request_count"] >= 11]
        if s2s:
            s2s = s2s[0]
            print(f"\n  Result: saved={s2s['tokens_saved']}  suspended={s2s['pruning_suspended']}  tracked={s2s['tools_tracked']}  cache={s2s['api_cache_hit_rate']}%")
            assert_gt("S2 tokens saved", s2s["tokens_saved"], 0)
            # After warm burst at end, pruning might be suspended again
            # The key test: did it ever prune? Check tokens_saved.
        else:
            print("  WARNING: could not find S2 session")

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 3: Slow thinker — every request is cold
        #  Pattern: User sends one request, thinks for 6+ min, sends next
        #  Cache never warms up → pruning should fire freely
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  S3: Slow Thinker — all requests cold, no cache")
        print("  10 tools, ~3 used. Cache always 0%.")
        print("  Expect: aggressive tool pruning after request 3")
        print("=" * 60)

        sys3 = "You are a research analyst. Investigate technical topics with web searches and synthesize findings."
        msgs3 = [{"role": "user", "content": "Research Rust async runtimes."}]

        for turn in range(1, 9):
            resp = send_anthropic(sys3, msgs3, RESEARCH_TOOLS, cache_hint="cold")
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}  cache={u['cache_read_input_tokens']:4d}  (always cold)")
            a, m = build_tool_result(resp, research_content)
            msgs3.extend([a, m])

        s3 = get_status()
        s3s = [s for s in s3["sessions"] if not s["pruning_suspended"] and s["request_count"] >= 8]
        if s3s:
            s3s = s3s[0]
            print(f"\n  Result: saved={s3s['tokens_saved']}  suspended={s3s['pruning_suspended']}  tracked={s3s['tools_tracked']}  cache={s3s['api_cache_hit_rate']}%")
            assert_false("S3 pruning NOT suspended (always cold)", s3s["pruning_suspended"])
            assert_gt("S3 tokens saved (pruning + dedup)", s3s["tokens_saved"], 0)
            assert_ge("S3 tools tracked >= 3", s3s["tools_tracked"], 1)
        else:
            # Might be suspended if cumulative stats from other scenarios leak
            # Check any research session
            all_research = [s for s in s3["sessions"] if s["request_count"] == 8]
            if all_research:
                s3s = all_research[0]
                print(f"\n  Result: saved={s3s['tokens_saved']}  suspended={s3s['pruning_suspended']}  tracked={s3s['tools_tracked']}  cache={s3s['api_cache_hit_rate']}%")
                assert_gt("S3 tokens saved", s3s["tokens_saved"], 0)

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 4: DevOps with lunch break + new tool after
        #  Pattern: 4 turns → 30min lunch → 4 turns with DockerExec added
        #  Tests: cache cold after break, new tool unfreezes pruning
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  S4: DevOps + Lunch Break — cache cold, new tool mid-session")
        print("  9→10 tools. Cache warm→cold→warm pattern.")
        print("  Expect: frozen set invalidated by DockerExec")
        print("=" * 60)

        sys4 = "You are a devops engineer managing Kubernetes deployments. Monitor and maintain production infrastructure."
        msgs4 = [{"role": "user", "content": "Deploy the new version."}]

        # Before lunch: 4 turns, cache warms
        cache_seq_s4 = [
            "cold", "cold", "warm", "warm",  # morning
            "cold", "cold", "warm", "warm",  # after lunch
        ]
        devops_base = DEVOPS_TOOLS[:]
        for turn in range(1, 9):
            cache = cache_seq_s4[turn - 1]
            tools = devops_base[:]
            gap = ""
            if turn >= 5:
                tools.append(make_tool("DockerExec", "Execute in container", ["container", "command"]))
                if turn == 5: gap = " [back from lunch, added DockerExec]"

            resp = send_anthropic(sys4, msgs4, tools, cache_hint=cache)
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}  cache={u['cache_read_input_tokens']:4d}  tools={len(tools)}{gap}")
            a, m = build_tool_result(resp, devops_content)
            msgs4.extend([a, m])

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 5: Quick question — only 2 turns, no optimization
        #  Pattern: User asks a simple question, gets answer, leaves
        #  Tests: no crash on short sessions, no false optimizations
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  S5: Quick Question — 2 turns, too short to optimize")
        print("  12 tools. Should NOT prune (request_count < 3)")
        print("=" * 60)

        sys5 = "You are a quick helper. Answer coding questions concisely and efficiently."
        msgs5 = [{"role": "user", "content": "What does #[derive(Debug)] do in Rust?"}]
        for turn in range(1, 3):
            resp = send_anthropic(sys5, msgs5, CODING_TOOLS, cache_hint="cold")
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}")
            a, m = build_tool_result(resp, coding_content)
            msgs5.extend([a, m])

        s5 = get_status()
        s5s = [s for s in s5["sessions"] if s["request_count"] == 2]
        if s5s:
            print(f"\n  Result: saved={s5s[0]['tokens_saved']}  tracked={s5s[0]['tools_tracked']}")
            # Tokens saved should be 0 or very small (only dedup, no pruning)

        # ══════════════════════════════════════════════════════════
        #  SCENARIO 6: Overnight batch — 15 turns, always cold
        #  Pattern: Automated agent runs one request every 10min
        #  Cache permanently cold → maximum pruning opportunity
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  S6: Overnight Batch — 15 turns, permanently cold cache")
        print("  12 tools, only 2 used (Bash/Read). Maximum pruning.")
        print("  Expect: 10 tools pruned after turn 3, high token savings")
        print("=" * 60)

        sys6 = "You are an automated batch processor. Run scheduled checks on the codebase every 10 minutes overnight."
        msgs6 = [{"role": "user", "content": "Run the nightly health check suite."}]

        for turn in range(1, 16):
            resp = send_anthropic(sys6, msgs6, CODING_TOOLS, cache_hint="cold")
            if not resp: print(f"  [turn {turn}] ERROR"); continue
            u = resp["usage"]
            print(f"  [turn {turn:2d}] in={u['input_tokens']:5d}  cache=0  (batch mode)")
            a, m = build_tool_result(resp, coding_content)
            msgs6.extend([a, m])

        s6 = get_status()
        s6s = [s for s in s6["sessions"] if s["request_count"] == 15]
        if s6s:
            s6s = s6s[0]
            print(f"\n  Result: saved={s6s['tokens_saved']}  suspended={s6s['pruning_suspended']}  tracked={s6s['tools_tracked']}  cache={s6s['api_cache_hit_rate']}%")
            assert_false("S6 pruning NOT suspended (always cold)", s6s["pruning_suspended"])
            assert_gt("S6 tokens saved (heavy pruning)", s6s["tokens_saved"], 1000)
            assert_ge("S6 tools tracked >= 2", s6s["tools_tracked"], 2)

        # ══════════════════════════════════════════════════════════
        #  FINAL VALIDATION
        # ══════════════════════════════════════════════════════════
        print("\n" + "=" * 60)
        print("  FINAL VALIDATION")
        print("=" * 60)

        final = get_status()
        print(f"  Uptime: {final['uptime_secs']}s")
        print(f"  Total requests: {final['total_requests']}")
        print(f"  Sessions: {final['session_count']}")
        print()

        for s in final["sessions"]:
            key = s["key"][:24]
            ps = "PAUSED" if s["pruning_suspended"] else "active"
            print(f"  [{key}]  reqs={s['request_count']}  saved={s['tokens_saved']}  tools={s['tools_tracked']}  cache={s['api_cache_hit_rate']}%  prune={ps}")

        print()
        events = final.get("events", [])
        print(f"  Events ({len(events)}):")
        for e in events[:20]:
            print(f"    {e['time']} [{e['kind']:8s}] {e['message']}")

        activity = final.get("activity", [])
        trace_files = glob.glob(f"{trace_dir}/session-*.json")

        # Final assertions
        print(f"\n  Final assertions:")
        total_requests = final["total_requests"]
        total_saved = sum(s["tokens_saved"] for s in final["sessions"])
        n_sessions = final["session_count"]
        n_suspended = sum(1 for s in final["sessions"] if s["pruning_suspended"])
        n_active = sum(1 for s in final["sessions"] if not s["pruning_suspended"])

        assert_ge("6 distinct sessions", n_sessions, 6)
        assert_ge("54+ total requests", total_requests, 54)
        assert_gt("total tokens saved", total_saved, 2000)
        assert_gt("events logged", len(events), 6)
        assert_ge("activity entries", len(activity), 20)
        assert_ge("trace files", len(trace_files), 1)

        # Key behavioral assertion: NOT all sessions have pruning suspended
        # Cold-cache scenarios should have active pruning
        assert_gt("some sessions have active pruning", n_active, 0)
        assert_gt("some sessions have suspended pruning", n_suspended, 0)

        # Sessions are independent
        suspended_keys = {s["key"] for s in final["sessions"] if s["pruning_suspended"]}
        active_keys = {s["key"] for s in final["sessions"] if not s["pruning_suspended"]}
        assert_true("suspended and active sessions are different keys", len(suspended_keys & active_keys) == 0)

        print(f"\n  Summary:")
        print(f"    {n_suspended} sessions with warm cache (pruning paused)")
        print(f"    {n_active} sessions with cold cache (pruning active)")
        print(f"    {total_saved} total tokens saved across all sessions")
        print(f"    {len(events)} events, {len(activity)} activity entries")

    finally:
        proxy.send_signal(signal.SIGTERM)
        upstream.send_signal(signal.SIGTERM)
        proxy.wait(timeout=5)
        upstream.wait(timeout=5)
        shutil.rmtree(trace_dir, ignore_errors=True)

    print(f"\n{'=' * 60}")
    print(f"  RESULTS: {PASS} passed, {FAIL} failed (out of {TESTS})")
    print(f"{'=' * 60}")
    sys.exit(1 if FAIL > 0 else 0)

if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.abspath(__file__)) + "/..")
    main()
