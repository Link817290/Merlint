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

**macOS / Linux（一键安装 + 自动代理）：**

```bash
curl -fsSL https://raw.githubusercontent.com/Link817290/Merlint/main/install.sh | bash
```

安装完成后，merlint 代理会在每次打开终端时自动启动，Claude Code 的请求自动经过优化。**无需任何额外配置。**

**Windows（PowerShell 一键安装）：**

```powershell
irm https://raw.githubusercontent.com/Link817290/Merlint/main/install.ps1 | iex
```

**从源码安装（需要 Rust）：**

```bash
cargo install --git https://github.com/Link817290/Merlint.git
```

**直接下载二进制：**

前往 [Releases](https://github.com/Link817290/Merlint/releases) 页面下载对应平台的可执行文件。

---

## 工作原理

```
Claude Code (窗口1)  ──┐
Claude Code (窗口2)  ──┼──→  merlint proxy :8019  ──→  api.anthropic.com
Claude Code (窗口3)  ──┘
                              │
                              ├─ 实时优化请求（裁剪工具、合并消息、去重）
                              ├─ 按项目自动分离会话
                              └─ 记录 token 用量 & 生成报告
```

安装后 merlint 自动设置 `ANTHROPIC_BASE_URL`，所有 Claude Code 请求透明经过代理。每个项目窗口独立追踪，互不干扰。

---

## 快速开始

安装完即可使用，代理自动运行。以下命令可随时查看状态：

```bash
# 查看代理状态
merlint-status

# 分析最近一次会话
merlint latest

# 扫描本地所有 Agent 会话
merlint scan

# 自动优化（生成 CLAUDE.md + 工具白名单）
merlint optimize

# 持续监控，自动优化
merlint monitor
```

控制代理：

```bash
merlint-stop     # 停止代理
merlint-start    # 重启代理
```

---

## 功能

### 1. 实时代理优化（Proxy）

透明 HTTP 代理，拦截 LLM API 调用并实时优化：

- **工具裁剪** — 自动移除未使用的工具定义，每个节省约 200 token/次
- **系统消息合并** — 合并重复的 system prompt 片段
- **文件读取缓存** — 去重连续相同的文件读取结果
- **多会话追踪** — 自动识别不同 Claude Code 窗口，按项目独立统计

```bash
# 自定义启动（通常不需要，安装时已自动配置）
merlint proxy --target https://api.anthropic.com --optimize --port 8019
```

支持 OpenAI 和 Anthropic 两种 API 格式，自动检测。

### 2. 诊断（Diagnose）

分析 Agent 会话的 token 使用情况，找出浪费点：

- **Token 统计** — 每次 API 调用的 prompt/completion/total token 用量
- **缓存分析** — prompt 前缀稳定性、缓存命中率、理论最优缓存率
- **效率检测** — 循环调用、重复读取文件、无效重试
- **工具利用率** — 定义了多少工具、实际用了几个、哪些从没用过

```bash
merlint analyze --source session.jsonl --format claude-code
```

### 3. 优化（Optimize）

根据诊断结果自动生成优化方案：

- **裁剪工具** — 移除未使用的工具定义
- **优化 Prompt** — 重构 system prompt 结构，提高缓存命中
- **生成配置** — 自动生成 `CLAUDE.md` 和 `.merlint-tools.json`
- **减少冗余** — 识别重复文件读取和无效重试模式

```bash
merlint optimize --source session.jsonl
```

### 4. 监控（Monitor）

后台持续监控，发现新会话自动分析和优化：

```bash
# 每 30 秒检查一次，自动优化
merlint monitor

# 自定义间隔
merlint monitor --interval 60
```

---

## 多会话追踪

merlint 自动识别不同的 Claude Code 窗口/项目：

- 通过 system prompt 哈希区分不同项目
- 每个项目独立的 token 统计和优化器状态
- 也支持显式 `X-Merlint-Session` 请求头

无需任何配置，开多个窗口自动分离。

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
| `merlint-status` | 查看代理运行状态 |
| `merlint-start` | 启动代理 |
| `merlint-stop` | 停止代理 |

---

## License

MIT
