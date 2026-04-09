use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use colored::Colorize;
use tokio::sync::Mutex;

use merlint::analyzer;
use merlint::banner;
use merlint::deep;
use merlint::history;
use merlint::models;
use merlint::optimizer;
use merlint::parser;
use merlint::profile;
use merlint::proxy;
use merlint::report;

use models::trace::TraceSession;
use parser::discover::AgentKind;

const DEFAULT_TRACE_FILE: &str = "/tmp/merlint-traces.json";

#[derive(Parser)]
#[command(name = "merlint")]
#[command(about = "Agent token optimizer — diagnose, optimize, and monitor LLM agent efficiency")]
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
        /// Enable real-time request optimization (prune unused tools, merge system messages)
        #[arg(long)]
        optimize: bool,
    },

    /// Scan this machine for agent sessions (Claude Code, Codex)
    Scan {
        /// Use AI for deep analysis (requires ANTHROPIC_API_KEY or OPENAI_API_KEY)
        #[arg(long)]
        deep: bool,
    },

    /// Analyze a trace/session file (supports merlint JSON, Claude Code, Codex)
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
        /// Use AI for deep analysis
        #[arg(long)]
        deep: bool,
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
        /// Use AI for deep analysis
        #[arg(long)]
        deep: bool,
    },

    /// Generate optimization plan from a trace/session
    Optimize {
        /// Path to trace or session file
        #[arg(default_value = DEFAULT_TRACE_FILE)]
        trace_file: PathBuf,
        #[arg(short = 's', long)]
        source: Option<SourceFormat>,
        /// Auto-apply optimizations (default: true)
        #[arg(long, default_value = "true")]
        auto: bool,
        /// Target directory for generated config files
        #[arg(short, long, default_value = ".")]
        target: PathBuf,
        /// Dry run — show plan without writing files
        #[arg(long)]
        dry_run: bool,
        /// Output plan as JSON instead of terminal
        #[arg(long)]
        json: bool,
    },

    /// Monitor agent sessions continuously, auto-optimize on new data
    Monitor {
        /// Which agent to monitor
        #[arg(short, long)]
        agent: Option<AgentFilter>,
        /// Check interval in seconds
        #[arg(short, long, default_value = "30")]
        interval: u64,
        /// Auto-apply optimizations when issues found
        #[arg(long, default_value = "true")]
        auto_optimize: bool,
        /// Target directory for generated config files
        #[arg(short, long, default_value = ".")]
        target: PathBuf,
    },

    /// Show usage report with trends over time
    Report {
        /// Time period: "week" or "month"
        #[arg(short, long, default_value = "week")]
        period: ReportPeriod,
        /// Number of periods to show
        #[arg(short, long, default_value = "4")]
        count: usize,
    },

    /// Show your agent usage profile and habits
    Profile {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Save report to file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Run as background daemon: periodic scan + summarize + update pruning config
    Daemon {
        /// Check interval in seconds
        #[arg(short, long, default_value = "3600")]
        interval: u64,
        /// Target directory for generated config files
        #[arg(short, long, default_value = ".")]
        target: PathBuf,
        /// Also generate a visual report each cycle
        #[arg(long)]
        report: bool,
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
    /// merlint native trace format
    Merlint,
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

#[derive(Clone, clap::ValueEnum)]
enum ReportPeriod {
    Week,
    Month,
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
            port, target, api_key, output, daemon, optimize,
        } => {
            if !daemon {
                tracing_subscriber::fmt().with_target(false).with_level(true).init();
            }

            let session = Arc::new(Mutex::new(TraceSession::new()));
            let session_for_shutdown = session.clone();
            let output_path = output.clone();
            let is_daemon = daemon;

            let transformer = if optimize {
                let tx = proxy::transformer::new_shared_transformer();
                // Load historical tool frequency to bootstrap pruning
                if let Ok(db) = history::db::HistoryDb::open() {
                    if let (Ok(freq), Ok(total)) = (db.tool_frequency(), db.session_count()) {
                        if total >= 3 {
                            let freq_data: Vec<(String, i64)> = freq.iter()
                                .map(|f| (f.tool_name.clone(), f.session_count))
                                .collect();
                            let mut t = tx.try_lock().expect("transformer lock");
                            t.load_history(&freq_data, total);
                            if !is_daemon {
                                eprintln!("Loaded tool history from {} sessions", total);
                            }
                        }
                    }
                }
                Some(tx)
            } else {
                None
            };

            let transformer_for_shutdown = transformer.clone();

            let config = proxy::server::ProxyConfig {
                listen_port: port, target_url: target, api_key,
                live_trace_file: Some(output),
                optimize,
            };

            // Run proxy until Ctrl+C, then clean up gracefully
            tokio::select! {
                result = proxy::server::run_proxy(config, session.clone(), transformer.clone()) => {
                    result?;
                }
                _ = tokio::signal::ctrl_c() => {
                    if !is_daemon { eprintln!("\n\nShutting down proxy..."); }

                    let session = session_for_shutdown.lock().await;
                    if let Ok(json) = serde_json::to_string_pretty(&*session) {
                        let _ = std::fs::write(&output_path, &json);
                        if !is_daemon { eprintln!("Traces saved to {}", output_path.display()); }
                    }
                    if !is_daemon && !session.entries.is_empty() {
                        run_and_print_report(&session);
                    }

                    // Persist tool usage to SQLite
                    if let Some(ref tx) = transformer_for_shutdown {
                        let t = tx.lock().await;
                        let usage = t.tool_usage_snapshot();
                        if !usage.is_empty() {
                            if let Ok(db) = history::db::HistoryDb::open() {
                                let sid = &session.id;
                                if let Err(e) = db.store_tool_usage(sid, &usage) {
                                    eprintln!("Failed to persist tool usage: {}", e);
                                } else if !is_daemon {
                                    eprintln!("Tool usage saved ({} tools tracked)", usage.len());
                                }
                            }
                        }
                    }
                }
            }
        }

        Commands::Scan { deep } => {
            let sources = parser::discover::discover_agents();
            if sources.is_empty() {
                println!("No agent sessions found on this machine.");
                println!();
                println!("Looked for:");
                println!("  Claude Code: ~/.claude/projects/*/sessions/");
                println!("  Codex:       ~/.local/share/codex-cli/ or ~/.codex/");
                return Ok(());
            }

            // Collect all sessions with index
            let mut all_sessions: Vec<(PathBuf, AgentKind, String, String)> = Vec::new();
            for src in &sources {
                let kind_str = match src.kind {
                    AgentKind::ClaudeCode => "Claude Code",
                    AgentKind::Codex => "Codex CLI",
                };
                let sessions = parser::discover::list_sessions(&src.session_dir);
                for s in sessions {
                    let size = s.metadata().ok().map(|m| m.len()).unwrap_or(0);
                    let mtime = s.metadata().ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                        .map(|d| {
                            let dt = chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                                .unwrap_or_default();
                            dt.format("%Y-%m-%d %H:%M").to_string()
                        })
                        .unwrap_or_else(|| "unknown".into());
                    let size_str = if size >= 1_000_000 {
                        format!("{:.1}MB", size as f64 / 1_000_000.0)
                    } else if size >= 1_000 {
                        format!("{:.0}KB", size as f64 / 1_000.0)
                    } else {
                        format!("{}B", size)
                    };
                    let name = s.file_name().unwrap_or_default().to_string_lossy().to_string();
                    let label = format!("[{}] {} ({}, {})", kind_str, name, size_str, mtime);
                    all_sessions.push((s, src.kind, src.name.clone(), label));
                }
            }

            if all_sessions.is_empty() {
                println!("No session files found.");
                return Ok(());
            }

            println!("Found {} session(s):\n", all_sessions.len());
            for (i, (_, _, project, label)) in all_sessions.iter().enumerate() {
                println!("  {:>3})  {} / {}", i + 1, project, label);
            }
            println!();
            println!("  {:>3})  Exit", 0);
            println!();

            // Interactive selection
            eprint!("Select session to analyze [1-{}]: ", all_sessions.len());
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let choice: usize = input.trim().parse().unwrap_or(0);

            if choice == 0 || choice > all_sessions.len() {
                return Ok(());
            }

            let (path, kind, _, _) = &all_sessions[choice - 1];
            eprintln!("\nAnalyzing: {}\n", path.display());

            let session = load_from_source(path, *kind)?;
            if session.entries.is_empty() {
                eprintln!("No API calls found in this session.");
                return Ok(());
            }

            let (ts, ca, ea) = analyze_session(&session);
            report::terminal::print_report(&session, &ts, &ca, &ea);

            // Store in history
            store_to_history(
                &session.id,
                &path.display().to_string(),
                agent_kind_str(*kind),
                &ts, &ea, &ca,
            );

            // Deep analysis
            if deep {
                eprintln!("{}", "Running deep analysis...".magenta());
                match deep::analyze::deep_analyze(&session, &ts, &ca, &ea).await {
                    Ok(result) => deep::analyze::print_deep_result(&result),
                    Err(e) => eprintln!("  Deep analysis failed: {}", e),
                }
            }

            // Ask if user wants to optimize
            eprintln!();
            eprint!("Generate optimization plan? [Y/n]: ");
            let mut opt_input = String::new();
            std::io::stdin().read_line(&mut opt_input)?;
            let opt_choice = opt_input.trim().to_lowercase();

            if opt_choice.is_empty() || opt_choice == "y" || opt_choice == "yes" {
                let plan = build_optimization_plan(&session);
                if plan.is_empty() {
                    eprintln!("No optimizations needed — looking good!");
                } else {
                    optimizer::applier::print_plan(&plan);
                    eprintln!();
                    eprint!("Apply optimizations to current directory? [Y/n]: ");
                    let mut apply_input = String::new();
                    std::io::stdin().read_line(&mut apply_input)?;
                    let apply_choice = apply_input.trim().to_lowercase();
                    if apply_choice.is_empty() || apply_choice == "y" || apply_choice == "yes" {
                        let target = PathBuf::from(".");
                        let results = optimizer::applier::apply_plan(&plan, &target, false);
                        optimizer::applier::print_apply_results(&results);
                    }
                }
            }
        }

        Commands::Latest { agent, format, deep } => {
            let (path, kind) = match find_latest_session(&agent) {
                Some(found) => found,
                None => {
                    eprintln!("No agent sessions found. Run `merlint scan` to check.");
                    return Ok(());
                }
            };

            eprintln!("Analyzing: {}", path.display());
            let session = load_from_source(&path, kind)?;

            if session.entries.is_empty() {
                eprintln!("No API calls found in this session.");
                return Ok(());
            }

            let (ts, ca, ea) = analyze_session(&session);
            output_report(&session, &format, &None, &ts, &ca, &ea);

            // Store in history
            store_to_history(
                &session.id,
                &path.display().to_string(),
                agent_kind_str(kind),
                &ts, &ea, &ca,
            );

            if deep {
                eprintln!("{}", "Running deep analysis...".magenta());
                match deep::analyze::deep_analyze(&session, &ts, &ca, &ea).await {
                    Ok(result) => deep::analyze::print_deep_result(&result),
                    Err(e) => eprintln!("  Deep analysis failed: {}", e),
                }
            }
        }

        Commands::Analyze {
            trace_file, source, format, output, deep,
        } => {
            let session = smart_load(&trace_file, source.as_ref())?;
            if session.entries.is_empty() {
                eprintln!("No entries found.");
                return Ok(());
            }

            let (ts, ca, ea) = analyze_session(&session);
            output_report(&session, &format, &output, &ts, &ca, &ea);

            // Store in history
            store_to_history(
                &session.id,
                &trace_file.display().to_string(),
                "manual",
                &ts, &ea, &ca,
            );

            if deep {
                eprintln!("{}", "Running deep analysis...".magenta());
                match deep::analyze::deep_analyze(&session, &ts, &ca, &ea).await {
                    Ok(result) => deep::analyze::print_deep_result(&result),
                    Err(e) => eprintln!("  Deep analysis failed: {}", e),
                }
            }
        }

        Commands::Optimize {
            trace_file, source, auto, target, dry_run, json,
        } => {
            let session = smart_load(&trace_file, source.as_ref())?;
            if session.entries.is_empty() {
                eprintln!("No entries found.");
                return Ok(());
            }

            let plan = build_optimization_plan(&session);

            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
                return Ok(());
            }

            optimizer::applier::print_plan(&plan);

            if plan.is_empty() {
                return Ok(());
            }

            if auto && !dry_run {
                eprintln!("Auto-applying optimizations to {}...", target.display());
                let results = optimizer::applier::apply_plan(&plan, &target, false);
                optimizer::applier::print_apply_results(&results);
            } else if dry_run {
                let results = optimizer::applier::apply_plan(&plan, &target, true);
                optimizer::applier::print_apply_results(&results);
                eprintln!("Dry run — no files written. Remove --dry-run to apply.");
            } else {
                eprintln!("Run with --auto to apply, or --dry-run to preview.");
            }
        }

        Commands::Monitor {
            agent, interval, auto_optimize, target,
        } => {
            tracing_subscriber::fmt().with_target(false).with_level(true).init();
            eprintln!("Monitoring agent sessions (interval: {}s, auto-optimize: {})", interval, auto_optimize);
            eprintln!("Press Ctrl+C to stop.\n");

            let mut last_seen: Option<(PathBuf, std::time::SystemTime)> = None;

            loop {
                if let Some((path, kind)) = find_latest_session(&agent) {
                    let mtime = path.metadata().ok().and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                    let is_new = last_seen.as_ref()
                        .map(|(p, t)| p != &path || *t != mtime)
                        .unwrap_or(true);

                    if is_new {
                        eprintln!("[{}] New/updated session: {}", chrono::Local::now().format("%H:%M:%S"), path.display());
                        last_seen = Some((path.clone(), mtime));

                        if let Ok(session) = load_from_source(&path, kind) {
                            if !session.entries.is_empty() {
                                let (ts, ca, ea) = analyze_session(&session);

                                store_to_history(
                                    &session.id,
                                    &path.display().to_string(),
                                    agent_kind_str(kind),
                                    &ts, &ea, &ca,
                                );

                                let plan = build_optimization_plan(&session);
                                if !plan.is_empty() {
                                    optimizer::applier::print_plan(&plan);
                                    if auto_optimize {
                                        let results = optimizer::applier::apply_plan(&plan, &target, false);
                                        optimizer::applier::print_apply_results(&results);
                                    }
                                } else {
                                    eprintln!("  No optimizations needed.");
                                }
                            }
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
            }
        }

        Commands::Query {
            trace_file, source, metric,
        } => {
            let session = match smart_load(&trace_file, source.as_ref()) {
                Ok(s) => s,
                Err(_) => {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "error": "no trace data",
                        "hint": "run `merlint scan` or `merlint proxy --daemon`"
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

        Commands::Report { period, count } => {
            let db = match history::db::HistoryDb::open() {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("Failed to open history database: {}", e);
                    eprintln!("Run `merlint scan` first to analyze some sessions.");
                    return Ok(());
                }
            };

            let total = db.session_count()?;
            if total == 0 {
                eprintln!("No sessions in history yet.");
                eprintln!("Run `merlint scan` to analyze sessions and build history.");
                return Ok(());
            }

            println!();
            println!("{}", "  ========================================".cyan());
            println!("{}", "    merlint — Usage Report".cyan().bold());
            println!("{}", "  ========================================".cyan());
            println!();

            let now = chrono::Utc::now();
            let period_days = match period {
                ReportPeriod::Week => 7,
                ReportPeriod::Month => 30,
            };
            let period_name = match period {
                ReportPeriod::Week => "Week",
                ReportPeriod::Month => "Month",
            };

            for i in 0..count {
                let end = now - chrono::Duration::days((i * period_days) as i64);
                let start = end - chrono::Duration::days(period_days as i64);

                let from = start.format("%Y-%m-%d").to_string();
                let to = end.format("%Y-%m-%d").to_string();

                let sessions = db.sessions_between(&from, &to)?;

                if sessions.is_empty() {
                    println!("  {} {} ({} ~ {}): no data", period_name, i + 1, from, to);
                    continue;
                }

                let total_tokens: i64 = sessions.iter().map(|s| s.total_tokens).sum();
                let avg_tokens = total_tokens as f64 / sessions.len() as f64;
                let avg_cache: f64 = sessions.iter().map(|s| s.cache_hit_ratio).sum::<f64>()
                    / sessions.len() as f64;
                let total_retries: i64 = sessions.iter().map(|s| s.retry_count).sum();

                let label = if i == 0 {
                    format!("{} (current)", period_name)
                } else {
                    format!("{} -{}", period_name, i)
                };

                println!("  {} ({} ~ {})", label.bold(), from, to);
                println!("    Sessions: {}  |  Total tokens: {}  |  Avg/session: {:.0}",
                    sessions.len(),
                    format_tokens(total_tokens),
                    avg_tokens,
                );
                println!("    Cache hit: {:.0}%  |  Retries: {}",
                    avg_cache * 100.0,
                    total_retries,
                );
                println!();
            }

            // Show overall trend if enough data
            let all = db.list_sessions(100)?;
            if all.len() >= 4 {
                let half = all.len() / 2;
                let newer_avg: f64 = all[..half].iter().map(|s| s.total_tokens as f64).sum::<f64>() / half as f64;
                let older_avg: f64 = all[half..].iter().map(|s| s.total_tokens as f64).sum::<f64>() / (all.len() - half) as f64;

                if older_avg > 0.0 {
                    let change = (newer_avg - older_avg) / older_avg * 100.0;
                    let trend = if change < -10.0 {
                        format!("{:.0}% decrease", change.abs()).green().to_string()
                    } else if change > 10.0 {
                        format!("{:.0}% increase", change).red().to_string()
                    } else {
                        "stable".white().to_string()
                    };
                    println!("  Overall trend: {}", trend);
                    println!();
                }
            }
        }

        Commands::Profile { json, output } => {
            let db = match history::db::HistoryDb::open() {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("Failed to open history database: {}", e);
                    eprintln!("Run `merlint scan` first to analyze some sessions.");
                    return Ok(());
                }
            };

            let total = db.session_count()?;
            if total == 0 {
                eprintln!("No sessions in history yet.");
                eprintln!("Run `merlint scan` to analyze sessions and build history.");
                return Ok(());
            }

            match profile::engine::build_profile(&db) {
                Ok(p) => {
                    if json || output.is_some() {
                        let report = profile::engine::profile_to_json(&p);
                        if let Ok(json_str) = serde_json::to_string_pretty(&report) {
                            if let Some(ref out) = output {
                                let _ = std::fs::write(out, &json_str);
                                eprintln!("Profile report saved to {}", out.display());
                            }
                            if json {
                                println!("{}", json_str);
                            }
                        }
                    }
                    if !json {
                        profile::engine::print_profile(&p);
                    }
                }
                Err(e) => eprintln!("Failed to build profile: {}", e),
            }
        }

        Commands::Daemon { interval, target, report } => {
            tracing_subscriber::fmt().with_target(false).with_level(true).init();
            eprintln!("merlint daemon started (interval: {}s)", interval);
            eprintln!("Press Ctrl+C to stop.\n");

            let mut last_session_count: i64 = 0;
            let mut analyzed_files: std::collections::HashSet<String> = std::collections::HashSet::new();

            loop {
                // 1. Scan for new sessions and analyze them
                let sources = parser::discover::discover_agents();
                let mut new_sessions = 0;

                for src in &sources {
                    let sessions = parser::discover::list_sessions(&src.session_dir);
                    for s in &sessions {
                        // Skip files already analyzed this daemon lifetime
                        let file_key = format!("{}:{}", s.display(),
                            s.metadata().ok().and_then(|m| m.len().into()).unwrap_or(0u64));
                        if analyzed_files.contains(&file_key) {
                            continue;
                        }
                        if let Ok(session) = load_from_source(s, src.kind) {
                            if !session.entries.is_empty() {
                                let (ts, ca, ea) = analyze_session(&session);
                                store_to_history(
                                    &session.id,
                                    &s.display().to_string(),
                                    agent_kind_str(src.kind),
                                    &ts, &ea, &ca,
                                );
                                new_sessions += 1;
                                analyzed_files.insert(file_key);
                            }
                        }
                    }
                }

                // 2. Generate pruning config based on accumulated data
                if let Ok(db) = history::db::HistoryDb::open() {
                    let current_count = db.session_count().unwrap_or(0);
                    if current_count > last_session_count {
                        last_session_count = current_count;
                        eprintln!("[{}] {} sessions in history ({} new this cycle)",
                            chrono::Local::now().format("%H:%M:%S"),
                            current_count, new_sessions);

                        // Build profile and generate recommendations
                        if let Ok(profile) = profile::engine::build_profile(&db) {
                            if !profile.recommendations.is_empty() {
                                eprintln!("  Recommendations:");
                                for rec in &profile.recommendations {
                                    eprintln!("    -> {}", rec);
                                }
                            }
                        }

                        // Generate pruning config if enough data
                        if current_count >= 3 {
                            if let Ok(freq) = db.tool_frequency() {
                                let all_tools: Vec<String> = freq.iter()
                                    .map(|f| f.tool_name.clone())
                                    .collect();
                                if let Ok(rec) = profile::engine::recommend_pruning(&db, &all_tools) {
                                    if !rec.prune.is_empty() {
                                        let config = serde_json::json!({
                                            "description": "Auto-generated by merlint daemon",
                                            "confidence": rec.confidence,
                                            "data_sessions": rec.data_sessions,
                                            "allowed_tools": rec.keep,
                                            "pruned_tools": rec.prune,
                                            "updated_at": chrono::Local::now().to_rfc3339(),
                                        });
                                        let config_path = target.join(".merlint-tools.json");
                                        if let Ok(json) = serde_json::to_string_pretty(&config) {
                                            let _ = std::fs::write(&config_path, &json);
                                            eprintln!("  Updated {} (pruned {} tools, confidence {:.0}%)",
                                                config_path.display(), rec.prune.len(), rec.confidence * 100.0);
                                        }
                                    }
                                }
                            }
                        }

                        // Generate visual report if requested
                        if report {
                            if let Ok(profile) = profile::engine::build_profile(&db) {
                                let report_path = target.join("merlint-report.json");
                                let report_data = serde_json::json!({
                                    "generated_at": chrono::Local::now().to_rfc3339(),
                                    "sessions_analyzed": profile.stats.session_count,
                                    "total_tokens": profile.stats.total_tokens,
                                    "avg_tokens_per_session": profile.stats.avg_tokens_per_session,
                                    "avg_cache_hit": profile.stats.avg_cache_hit,
                                    "habits": profile.habits.iter().map(|h| {
                                        serde_json::json!({
                                            "name": h.name,
                                            "description": h.description,
                                            "severity": format!("{:?}", h.severity),
                                        })
                                    }).collect::<Vec<_>>(),
                                    "trends": profile.trends.iter().map(|t| {
                                        serde_json::json!({
                                            "metric": t.metric,
                                            "direction": format!("{:?}", t.direction),
                                            "description": t.description,
                                        })
                                    }).collect::<Vec<_>>(),
                                    "tool_frequency": profile.tool_freq.iter().map(|f| {
                                        serde_json::json!({
                                            "tool": f.tool_name,
                                            "total_calls": f.total_calls,
                                            "session_count": f.session_count,
                                        })
                                    }).collect::<Vec<_>>(),
                                    "recommendations": profile.recommendations,
                                });
                                if let Ok(json) = serde_json::to_string_pretty(&report_data) {
                                    let _ = std::fs::write(&report_path, &json);
                                    eprintln!("  Report saved to {}", report_path.display());
                                }
                            }
                        }
                    } else {
                        eprintln!("[{}] No new sessions.",
                            chrono::Local::now().format("%H:%M:%S"));
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
            }
        }
    }

    Ok(())
}

fn analyze_session(session: &TraceSession) -> (
    analyzer::token::SessionTokenSummary,
    analyzer::cache::CacheAnalysis,
    analyzer::efficiency::EfficiencyAnalysis,
) {
    let ts = analyzer::token::summarize_session_tokens(session);
    let ca = analyzer::cache::analyze_cache(session);
    let ea = analyzer::efficiency::analyze_efficiency(session);
    (ts, ca, ea)
}

fn run_and_print_report(session: &TraceSession) {
    let (ts, ca, ea) = analyze_session(session);
    report::terminal::print_report(session, &ts, &ca, &ea);
}

fn output_report(
    session: &TraceSession,
    format: &OutputFormat,
    output: &Option<PathBuf>,
    ts: &analyzer::token::SessionTokenSummary,
    ca: &analyzer::cache::CacheAnalysis,
    ea: &analyzer::efficiency::EfficiencyAnalysis,
) {
    match format {
        OutputFormat::Terminal => report::terminal::print_report(session, ts, ca, ea),
        OutputFormat::Json => println!("{}", report::json::generate_json(session, ts, ca, ea)),
        OutputFormat::Both => {
            report::terminal::print_report(session, ts, ca, ea);
            let json = report::json::generate_json(session, ts, ca, ea);
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
            let json = report::json::generate_json(session, ts, ca, ea);
            let _ = std::fs::write(out, &json);
            eprintln!("JSON report saved to {}", out.display());
        }
    }
}

/// Store analysis results to history database (non-fatal on error)
fn store_to_history(
    session_id: &str,
    source_file: &str,
    agent_kind: &str,
    ts: &analyzer::token::SessionTokenSummary,
    ea: &analyzer::efficiency::EfficiencyAnalysis,
    ca: &analyzer::cache::CacheAnalysis,
) {
    match history::db::HistoryDb::open() {
        Ok(db) => {
            if let Err(e) = db.store_session(session_id, source_file, agent_kind, ts, ea, ca) {
                eprintln!("  (history: failed to store: {})", e);
            }
            // Also store per-tool usage
            if !ts.tool_call_counts.is_empty() {
                if let Err(e) = db.store_tool_usage(session_id, &ts.tool_call_counts) {
                    eprintln!("  (history: failed to store tool usage: {})", e);
                }
            }
        }
        Err(e) => {
            eprintln!("  (history: failed to open db: {})", e);
        }
    }
}

/// Load a session file with format auto-detection
fn smart_load(path: &PathBuf, source: Option<&SourceFormat>) -> anyhow::Result<TraceSession> {
    if let Some(fmt) = source {
        return match fmt {
            SourceFormat::Merlint => load_native(path),
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

fn build_optimization_plan(session: &TraceSession) -> optimizer::plan::OptimizationPlan {
    let ts = analyzer::token::summarize_session_tokens(session);
    let ca = analyzer::cache::analyze_cache(session);
    let ea = analyzer::efficiency::analyze_efficiency(session);

    let mut plan = optimizer::plan::OptimizationPlan::new();

    for item in optimizer::tools::optimize_tools(&ts) {
        plan.add(item);
    }
    for item in optimizer::prompt::optimize_prompt(session, &ts, &ca) {
        plan.add(item);
    }
    for item in optimizer::config::optimize_efficiency(&ts, &ea) {
        plan.add(item);
    }

    // Calculate estimated cache improvement
    if !plan.items.is_empty() {
        let current_hit = if ts.cache_data_available && ts.total_prompt_tokens > 0 {
            ts.total_cache_read_tokens as f64 / ts.total_prompt_tokens as f64 * 100.0
        } else {
            ca.theoretical_cache_hit_ratio * 100.0
        };
        let has_cache_fixes = plan.items.iter().any(|i| matches!(i.category, optimizer::plan::OptCategory::Cache));
        if has_cache_fixes {
            plan.estimated_cache_improvement_pct = (80.0 - current_hit).max(0.0);
        }
    }

    plan.sort_by_impact();
    plan
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

fn load_from_source(path: &PathBuf, kind: AgentKind) -> anyhow::Result<TraceSession> {
    match kind {
        AgentKind::ClaudeCode => parser::claude_code::parse_session(path),
        AgentKind::Codex => parser::codex::parse_session(path),
    }
}

/// Find the latest session file across discovered agents, optionally filtered by agent type.
fn find_latest_session(agent: &Option<AgentFilter>) -> Option<(PathBuf, AgentKind)> {
    let sources = parser::discover::discover_agents();
    let filtered: Vec<_> = sources
        .into_iter()
        .filter(|s| match agent {
            Some(AgentFilter::ClaudeCode) => s.kind == AgentKind::ClaudeCode,
            Some(AgentFilter::Codex) => s.kind == AgentKind::Codex,
            None => true,
        })
        .collect();

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

    best.map(|(path, kind, _)| (path, kind))
}

fn agent_kind_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude_code",
        AgentKind::Codex => "codex",
    }
}
