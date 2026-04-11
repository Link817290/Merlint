use std::path::{Path, PathBuf};

use merlint::analyzer;
use merlint::history;
use merlint::models::trace::TraceSession;
use merlint::optimizer;
use merlint::parser;
use merlint::parser::discover::AgentKind;
use merlint::report;

#[derive(Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Terminal,
    Json,
    Both,
}

#[derive(Clone, clap::ValueEnum)]
pub enum SourceFormat {
    Merlint,
    ClaudeCode,
    Codex,
}

#[derive(Clone, clap::ValueEnum)]
pub enum AgentFilter {
    ClaudeCode,
    Codex,
}

#[derive(Clone, clap::ValueEnum)]
pub enum QueryMetric {
    All,
    Tokens,
    Cache,
    Tools,
    Efficiency,
    Calls,
}

#[derive(Clone, clap::ValueEnum)]
pub enum ReportPeriod {
    Week,
    Month,
}

pub fn analyze_session(
    session: &TraceSession,
) -> (
    analyzer::token::SessionTokenSummary,
    analyzer::cache::CacheAnalysis,
    analyzer::efficiency::EfficiencyAnalysis,
) {
    let ts = analyzer::token::summarize_session_tokens(session);
    let ca = analyzer::cache::analyze_cache(session);
    let ea = analyzer::efficiency::analyze_efficiency(session);
    (ts, ca, ea)
}

pub fn run_and_print_report(session: &TraceSession) {
    let (ts, ca, ea) = analyze_session(session);
    report::terminal::print_report(session, &ts, &ca, &ea);
}

pub fn output_report(
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

pub fn store_to_history(
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

pub fn smart_load(path: &Path, source: Option<&SourceFormat>) -> anyhow::Result<TraceSession> {
    if let Some(fmt) = source {
        return match fmt {
            SourceFormat::Merlint => load_native(path),
            SourceFormat::ClaudeCode => parser::claude_code::parse_session(path),
            SourceFormat::Codex => parser::codex::parse_session(path),
        };
    }

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

fn load_native(path: &Path) -> anyhow::Result<TraceSession> {
    let content = std::fs::read_to_string(path)?;
    let session: TraceSession = serde_json::from_str(&content)?;
    Ok(session)
}

pub fn load_from_source(path: &Path, kind: AgentKind) -> anyhow::Result<TraceSession> {
    match kind {
        AgentKind::ClaudeCode => parser::claude_code::parse_session(path),
        AgentKind::Codex => parser::codex::parse_session(path),
    }
}

pub fn build_optimization_plan(session: &TraceSession) -> optimizer::plan::OptimizationPlan {
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

    if !plan.items.is_empty() {
        let current_hit = if ts.cache_data_available && ts.total_prompt_tokens > 0 {
            ts.total_cache_read_tokens as f64 / ts.total_prompt_tokens as f64 * 100.0
        } else {
            ca.theoretical_cache_hit_ratio * 100.0
        };
        let has_cache_fixes = plan
            .items
            .iter()
            .any(|i| matches!(i.category, optimizer::plan::OptCategory::Cache));
        if has_cache_fixes {
            plan.estimated_cache_improvement_pct = (80.0 - current_hit).max(0.0);
        }
    }

    plan.sort_by_impact();
    plan
}

pub fn find_latest_session(agent: &Option<AgentFilter>) -> Option<(PathBuf, AgentKind)> {
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
            let mtime = latest
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if best.as_ref().map(|(_, _, t)| mtime > *t).unwrap_or(true) {
                best = Some((latest.clone(), src.kind, mtime));
            }
        }
    }

    best.map(|(path, kind, _)| (path, kind))
}

pub fn agent_kind_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude_code",
        AgentKind::Codex => "codex",
    }
}

pub use merlint::util::format::format_tokens;
