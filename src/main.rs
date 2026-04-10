use std::path::PathBuf;

use clap::{Parser, Subcommand};

use merlint::banner;

mod commands;

use commands::helpers::{AgentFilter, OutputFormat, QueryMetric, ReportPeriod, SourceFormat};

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

    /// Quick start: launch proxy with defaults (port 8019, Anthropic, optimize)
    Up {
        /// Port to listen on
        #[arg(short, long)]
        port: Option<u16>,
        /// Run in foreground instead of background
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the proxy started by 'merlint up'
    Down {
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Install shell hook so ANTHROPIC_BASE_URL auto-configures with merlint up/down
    SetupShell,

    /// Live terminal dashboard showing proxy status and session stats
    Dashboard {
        /// Proxy port to connect to
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Show spend tracking: cost, savings, and usage breakdown
    Spend {
        /// Number of days to show (default: 7)
        #[arg(short, long, default_value = "7")]
        days: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show waste pattern insights
        #[arg(long)]
        insights: bool,
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
            port,
            target,
            api_key,
            output,
            daemon,
            optimize,
        } => commands::proxy::run(port, target, api_key, output, daemon, optimize).await,

        Commands::Scan { deep } => commands::scan::run(deep).await,

        Commands::Latest {
            agent,
            format,
            deep,
        } => commands::analyze::run_latest(agent, format, deep).await,

        Commands::Analyze {
            trace_file,
            source,
            format,
            output,
            deep,
        } => commands::analyze::run_analyze(trace_file, source, format, output, deep).await,

        Commands::Optimize {
            trace_file,
            source,
            auto,
            target,
            dry_run,
            json,
        } => commands::analyze::run_optimize(trace_file, source, auto, target, dry_run, json),

        Commands::Query {
            trace_file,
            source,
            metric,
        } => commands::query::run(trace_file, source, metric),

        Commands::Monitor {
            agent,
            interval,
            auto_optimize,
            target,
        } => commands::monitor::run(agent, interval, auto_optimize, target).await,

        Commands::Report { period, count } => commands::report::run(period, count),

        Commands::Profile { json, output } => commands::profile::run(json, output),

        Commands::Up { port, foreground } => commands::up::run(port, foreground).await,

        Commands::Down { port } => commands::up::run_down(port),

        Commands::SetupShell => commands::up::run_setup_shell(),

        Commands::Dashboard { port } => commands::dashboard::run(port).await,

        Commands::Spend { days, json, insights } => commands::spend::run(days, json, insights),

        Commands::Daemon {
            interval,
            target,
            report,
        } => commands::daemon::run(interval, target, report).await,
    }
}
