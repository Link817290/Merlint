mod analyzer;
mod banner;
mod models;
mod parser;
mod proxy;
mod report;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::Mutex;

use models::trace::TraceSession;
use parser::discover::AgentKind;

const DEFAULT_TRACE_FILE: &str = "/tmp/agentbench-traces.json";

#[derive(Parser)]
#[command(name = "agentbench")]
#[command(about = "Agent execution efficiency analyzer — real token data, cache hits, and performance benchmarking")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a transparent proxy to intercept LLM API calls
    Proxy {
        #[arg(short, long, default_value = "8080")]
        port: u16,
        #[arg(short, long, default_value = "https://api.openai.com")]
        target: String,
        #[arg(short = 'k', long)]
        api_key: Option<String>,
        #[arg(short, long, default_value = DEFAULT_TRACE_FILE)]
        output: PathBuf,
        #[arg(short, long)]
        daemon: bool,
    },

    /// Scan this machine for agent sessions (Claude Code, Codex)
    Scan,

    /// Analyze a trace/session file (supports agentbench JSON, Claude Code, Codex)
    Analyze {
        /// Path to trace or session file
        #[arg(default_value = DEFAULT_TRACE_FILE)]
        trace_file: PathBuf,
        /// Source format (auto-detected if not specified)
        #[arg(short = 's', long)]
        source: Option<SourceFormat>,
        #[arg(short, long, default_value = "terminal")]
        format: OutputFormat,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Query live trace data (outputs JSON to stdout, for agent integration)
    Query {
        #[arg(short, long, default_value = DEFAULT_TRACE_FILE)]
        trace_file: PathBuf,
        #[arg(short = 's', long)]
        source: Option<SourceFormat>,
        #[arg(short, long, default_value = "all")]
        metric: QueryMetric,
    },

    /// Analyze the latest session from a local agent
    Latest {
        /// Which agent to analyze
        #[arg(short, long)]
        agent: Option<AgentFilter>,
        #[arg(short, long, default_value = "terminal")]
        format: OutputFormat,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum OutputFormat {
    Terminal,
    Json,
    Both,
}

#[derive(Clone, clap::ValueEnum)]
enum SourceFormat {
    /// agentbench native trace format
    Agentbench,
    /// Claude Code session (JSONL or JSON)
    ClaudeCode,
    /// Codex CLI session
    Codex,
}

#[derive(Clone, clap::ValueEnum)]
enum AgentFilter {
    ClaudeCode,
    Codex,
}

#[derive(Clone, clap::ValueEnum)]
enum QueryMetric {
    All,
    Tokens,
    Cache,
    Tools,
    Efficiency,
    Calls,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            banner::print_banner();
            return Ok(());
        }
    };

    match command {
        Commands::Proxy {
            port, target, api_key, output, daemon,
        } => {
            if !daemon {
                tracing_subscriber::fmt().with_target(false).with_level(true).init();
            }

            let session = Arc::new(Mutex::new(TraceSession::new()));
            let session_for_shutdown = session.clone();
            let output_path = output.clone();
            let is_daemon = daemon;

            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                if !is_daemon { eprintln!("\n\nShutting down proxy..."); }

                let session = session_for_shutdown.lock().await;
                if let Ok(json) = serde_json::to_string_pretty(&*session) {
                    let _ = std::fs::write(&output_path, &json);
                    if !is_daemon { eprintln!("Traces saved to {}", output_path.display()); }
                }
                if !is_daemon && !session.entries.is_empty() {
                    run_and_print_report(&session);
                }
                std::process::exit(0);
            });

            let config = proxy::server::ProxyConfig {
                listen_port: port, target_url: target, api_key,
                live_trace_file: Some(output),
            };
            proxy::server::run_proxy(config, session).await?;
        }

        Commands::Scan => {
            let sources = parser::discover::discover_agents();
            if sources.is_empty() {
                println!("No agent sessions found on this machine.");
                println!();
                println!("Looked for:");
                println!("  Claude Code: ~/.claude/projects/*/sessions/");
                println!("  Codex:       ~/.local/share/codex-cli/ or ~/.codex/");
                return Ok(());
            }

            println!("Found {} agent source(s):\n", sources.len());
            for src in &sources {
                let kind_str = match src.kind {
                    AgentKind::ClaudeCode => "Claude Code",
                    AgentKind::Codex => "Codex CLI",
                };
                let sessions = parser::discover::list_sessions(&src.session_dir);
                println!("  {} [{}]", kind_str, src.name);
                println!("    Dir:      {}", src.session_dir.display());
                println!("    Sessions: {}", sessions.len());
                if let Some(latest) = sessions.first() {
                    println!("    Latest:   {}", latest.file_name().unwrap_or_default().to_string_lossy());
                }
                println!();
            }
            println!("Use `agentbench latest` to analyze the most recent session.");
            println!("Use `agentbench analyze <file> -s claude-code` to analyze a specific file.");
        }

        Commands::Latest { agent, format } => {
            let sources = parser::discover::discover_agents();
            let filtered: Vec<_> = sources
                .into_iter()
                .filter(|s| match &agent {
                    Some(AgentFilter::ClaudeCode) => s.kind == AgentKind::ClaudeCode,
                    Some(AgentFilter::Codex) => s.kind == AgentKind::Codex,
                    None => true,
                })
                .collect();

            if filtered.is_empty() {
                eprintln!("No agent sessions found. Run `agentbench scan` to check.");
                return Ok(());
            }

            // Find the latest session across all matching sources
            let mut best: Option<(PathBuf, AgentKind, std::time::SystemTime)> = None;
            for src in &filtered {
                let sessions = parser::discover::list_sessions(&src.session_dir);
                if let Some(latest) = sessions.first() {
                    let mtime = latest.metadata().ok().and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    if best.as_ref().map(|(_, _, t)| mtime > *t).unwrap_or(true) {
                        best = Some((latest.clone(), src.kind, mtime));
                    }
                }
            }

            let (path, kind, _) = match best {
                Some(b) => b,
                None => {
                    eprintln!("No session files found.");
                    return Ok(());
                }
            };

            eprintln!("Analyzing: {}", path.display());
            let session = load_from_source(&path, kind)?;

            if session.entries.is_empty() {
                eprintln!("No API calls found in this session.");
                return Ok(());
            }

            output_report(&session, &format, &None);
        }

        Commands::Analyze {
            trace_file, source, format, output,
        } => {
            let session = smart_load(&trace_file, source.as_ref())?;
            if session.entries.is_empty() {
                eprintln!("No entries found.");
                return Ok(());
            }
            output_report(&session, &format, &output);
        }

        Commands::Query {
            trace_file, source, metric,
        } => {
            let session = match smart_load(&trace_file, source.as_ref()) {
                Ok(s) => s,
                Err(_) => {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "error": "no trace data",
                        "hint": "run `agentbench scan` or `agentbench proxy --daemon`"
                    }))?);
                    return Ok(());
                }
            };

            if session.entries.is_empty() {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"error": "no entries", "num_calls": 0}))?);
                return Ok(());
            }

            let ts = analyzer::token::summarize_session_tokens(&session);
            let ca = analyzer::cache::analyze_cache(&session);
            let ea = analyzer::efficiency::analyze_efficiency(&session);

            let out = match metric {
                QueryMetric::All => report::json::generate_json(&session, &ts, &ca, &ea),
                QueryMetric::Tokens => serde_json::to_string_pretty(&serde_json::json!({
                    "total_tokens": ts.total_tokens, "prompt_tokens": ts.total_prompt_tokens,
                    "completion_tokens": ts.total_completion_tokens, "num_calls": ts.num_calls,
                    "avg_per_call": if ts.num_calls > 0 { ts.total_tokens as f64 / ts.num_calls as f64 } else { 0.0 },
                }))?,
                QueryMetric::Cache => {
                    let hr = if ts.cache_data_available && ts.total_prompt_tokens > 0 {
                        Some(ts.total_cache_read_tokens as f64 / ts.total_prompt_tokens as f64)
                    } else { None };
                    serde_json::to_string_pretty(&serde_json::json!({
                        "data_available": ts.cache_data_available, "cache_read_tokens": ts.total_cache_read_tokens,
                        "cache_creation_tokens": ts.total_cache_creation_tokens, "hit_ratio": hr,
                        "prefix_stability": ca.prefix_stability_ratio,
                        "theoretical_hit_ratio": ca.theoretical_cache_hit_ratio,
                        "issues": ca.issues.len(),
                    }))?
                }
                QueryMetric::Tools => serde_json::to_string_pretty(&serde_json::json!({
                    "defined": ts.tools_defined, "used": ts.tools_used,
                    "unused": ts.tools_defined.saturating_sub(ts.tools_used),
                    "unused_names": ts.tool_names_unused, "used_names": ts.tool_names_used,
                    "avg_defined_per_call": ts.avg_tools_defined_per_call,
                }))?,
                QueryMetric::Efficiency => serde_json::to_string_pretty(&serde_json::json!({
                    "total_calls": ea.total_calls, "tool_calls": ea.tool_call_count,
                    "retries": ea.retry_count, "loop_patterns": ea.loop_patterns.len(),
                    "redundant_reads": ea.redundant_reads.len(),
                    "tokens_per_call_avg": ea.tokens_per_call_avg,
                }))?,
                QueryMetric::Calls => serde_json::to_string_pretty(&ts.per_call)?,
            };
            println!("{}", out);
        }
    }

    Ok(())
}

fn run_and_print_report(session: &TraceSession) {
    let ts = analyzer::token::summarize_session_tokens(session);
    let ca = analyzer::cache::analyze_cache(session);
    let ea = analyzer::efficiency::analyze_efficiency(session);
    report::terminal::print_report(session, &ts, &ca, &ea);
}

fn output_report(session: &TraceSession, format: &OutputFormat, output: &Option<PathBuf>) {
    let ts = analyzer::token::summarize_session_tokens(session);
    let ca = analyzer::cache::analyze_cache(session);
    let ea = analyzer::efficiency::analyze_efficiency(session);

    match format {
        OutputFormat::Terminal => report::terminal::print_report(session, &ts, &ca, &ea),
        OutputFormat::Json => println!("{}", report::json::generate_json(session, &ts, &ca, &ea)),
        OutputFormat::Both => {
            report::terminal::print_report(session, &ts, &ca, &ea);
            let json = report::json::generate_json(session, &ts, &ca, &ea);
            if let Some(ref out) = output {
                let _ = std::fs::write(out, &json);
                eprintln!("JSON report saved to {}", out.display());
            } else {
                println!("{}", json);
            }
        }
    }

    if let Some(ref out) = output {
        if !matches!(format, OutputFormat::Both) {
            let json = report::json::generate_json(session, &ts, &ca, &ea);
            let _ = std::fs::write(out, &json);
            eprintln!("JSON report saved to {}", out.display());
        }
    }
}

/// Load a session file with format auto-detection
fn smart_load(path: &PathBuf, source: Option<&SourceFormat>) -> anyhow::Result<TraceSession> {
    if let Some(fmt) = source {
        return match fmt {
            SourceFormat::Agentbench => load_native(path),
            SourceFormat::ClaudeCode => parser::claude_code::parse_session(path),
            SourceFormat::Codex => parser::codex::parse_session(path),
        };
    }

    // Auto-detect: try native first, then Claude Code, then Codex
    if let Ok(s) = load_native(path) {
        return Ok(s);
    }
    if let Ok(s) = parser::claude_code::parse_session(path) {
        if !s.entries.is_empty() {
            return Ok(s);
        }
    }
    if let Ok(s) = parser::codex::parse_session(path) {
        if !s.entries.is_empty() {
            return Ok(s);
        }
    }

    anyhow::bail!("Could not parse {} as any known format", path.display())
}

fn load_native(path: &PathBuf) -> anyhow::Result<TraceSession> {
    let content = std::fs::read_to_string(path)?;
    let session: TraceSession = serde_json::from_str(&content)?;
    Ok(session)
}

fn load_from_source(path: &PathBuf, kind: AgentKind) -> anyhow::Result<TraceSession> {
    match kind {
        AgentKind::ClaudeCode => parser::claude_code::parse_session(path),
        AgentKind::Codex => parser::codex::parse_session(path),
    }
}
