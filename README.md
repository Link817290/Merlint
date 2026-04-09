```
        /\
       /  \        merlint
      /____\       Agent Token Optimizer
      (O  O)
       <>
      /|  |\
     *---+~
```

# merlint

**Diagnose. Optimize. Monitor. Repeat.**

merlint 是一个 LLM Agent 效率优化工具，帮你减少 token 浪费、提高缓存命中率、自动生成优化配置。

支持 Claude Code、Codex、OpenAI、Anthropic 等主流 Agent 和 API。

---

## 安装

**Windows（PowerShell 一键安装）：**

```powershell
irm https://raw.githubusercontent.com/Link817290/Merlint/main/install.ps1 | iex
```

**macOS / Linux（Bash 一键安装）：**

```bash
curl -fsSL https://raw.githubusercontent.com/Link817290/Merlint/main/install.sh | bash
```

**直接下载二进制：**

前往 [Releases](https://github.com/Link817290/Merlint/releases) 页面下载对应平台的可执行文件。

**从源码安装（需要 Rust）：**

```bash
cargo install --git https://github.com/Link817290/Merlint.git
```

---

## 快速开始

```bash
# 扫描本地 Agent 会话
merlint scan

# 分析最近一次会话
merlint latest

# 自动优化（生成 CLAUDE.md + 工具白名单）
merlint optimize

# 持续监控，自动优化
merlint monitor
```

---

## 功能

### 1. 诊断（Diagnose）

分析 Agent 会话的 token 使用情况，找出浪费点：

- **Token 统计** — 每次 API 调用的 prompt/completion/total token 用量
- **缓存分析** — prompt 前缀稳定性、缓存命中率、理论最优缓存率
- **效率检测** — 循环调用、重复读取文件、无效重试
- **工具利用率** — 定义了多少工具、实际用了几个、哪些从没用过

```bash
merlint analyze --source session.jsonl --format claude-code
```

### 2. 优化（Optimize）

根据诊断结果自动生成优化方案：

- **裁剪工具** — 移除未使用的工具定义，每个节省约 200 token/次
- **优化 Prompt** — 重构 system prompt 结构，提高缓存命中
- **生成配置** — 自动生成 `CLAUDE.md` 和 `.merlint-tools.json`
- **减少冗余** — 识别重复文件读取和无效重试模式

```bash
# 自动优化（默认）
merlint optimize --source session.jsonl

# 仅查看建议，不写入文件
merlint optimize --source session.jsonl --dry-run
```

### 3. 监控（Monitor）

后台持续监控，发现新会话自动分析和优化：

```bash
# 每 30 秒检查一次，自动优化
merlint monitor

# 自定义间隔
merlint monitor --interval 60

# 仅监控不自动优化
merlint monitor --no-auto-optimize
```

### 4. 代理（Proxy）

透明 HTTP 代理，拦截 LLM API 调用实时记录：

```bash
merlint proxy --port 8080 --target https://api.openai.com
```

自动识别 OpenAI 和 Anthropic API，记录每次请求的 token 消耗。

---

## 支持的格式

| 格式 | 说明 | 自动检测 |
|------|------|----------|
| `merlint` | 原生 JSON 格式 | ✓ |
| `claude-code` | Claude Code JSONL 会话 | ✓ |
| `codex` | Codex CLI JSON 会话 | ✓ |

merlint 会自动检测文件格式，通常不需要手动指定 `--format`。

---

## 报告示例

```
═══════════════════════════════════════════
          Merlint Report
═══════════════════════════════════════════

▸ Overview
  API Calls:    12
  Total Tokens: 156.2K
  ├─ Prompt:     142.8K
  └─ Completion: 13.4K

▸ Cache (API data)
  Cache Read:     98.2K (68.8% of prompt)
  Hit Rate:       [█████████████░░░░░░░] 69%

▸ Tool Efficiency
  Defined: 23    Used: 8    Unused: 15
  ⚠ 65% of defined tools never used

▸ Efficiency
  ⚠ 'src/main.rs' read 4 times
  ✗ Loop: 'Bash' called 5 times consecutively
```

---

## 命令一览

| 命令 | 说明 |
|------|------|
| `merlint scan` | 扫描本地 Agent 会话文件 |
| `merlint latest` | 分析最近一次会话 |
| `merlint analyze` | 分析指定会话文件 |
| `merlint optimize` | 生成优化方案并应用 |
| `merlint monitor` | 持续监控 + 自动优化 |
| `merlint query` | 查询特定指标 |
| `merlint proxy` | 启动透明代理 |

---

## License

MIT
