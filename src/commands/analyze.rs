use std::path::PathBuf;

use colored::Colorize;

use merlint::deep;

use super::helpers::{
    agent_kind_str, analyze_session, find_latest_session, load_from_source, output_report,
    smart_load, store_to_history, AgentFilter, OutputFormat, SourceFormat,
};

pub async fn run_latest(
    agent: Option<AgentFilter>,
    format: OutputFormat,
    deep: bool,
) -> anyhow::Result<()> {
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

    store_to_history(
        &session.id,
        &path.display().to_string(),
        agent_kind_str(kind),
        &ts,
        &ea,
        &ca,
    );

    if deep {
        eprintln!("{}", "Running deep analysis...".magenta());
        match deep::analyze::deep_analyze(&session, &ts, &ca, &ea).await {
            Ok(result) => deep::analyze::print_deep_result(&result),
            Err(e) => eprintln!("  Deep analysis failed: {}", e),
        }
    }

    Ok(())
}

pub async fn run_analyze(
    trace_file: PathBuf,
    source: Option<SourceFormat>,
    format: OutputFormat,
    output: Option<PathBuf>,
    deep: bool,
) -> anyhow::Result<()> {
    let session = smart_load(&trace_file, source.as_ref())?;
    if session.entries.is_empty() {
        eprintln!("No entries found.");
        return Ok(());
    }

    let (ts, ca, ea) = analyze_session(&session);
    output_report(&session, &format, &output, &ts, &ca, &ea);

    store_to_history(
        &session.id,
        &trace_file.display().to_string(),
        "manual",
        &ts,
        &ea,
        &ca,
    );

    if deep {
        eprintln!("{}", "Running deep analysis...".magenta());
        match deep::analyze::deep_analyze(&session, &ts, &ca, &ea).await {
            Ok(result) => deep::analyze::print_deep_result(&result),
            Err(e) => eprintln!("  Deep analysis failed: {}", e),
        }
    }

    Ok(())
}

pub fn run_optimize(
    trace_file: PathBuf,
    source: Option<SourceFormat>,
    auto: bool,
    target: PathBuf,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    use merlint::optimizer;
    use super::helpers::build_optimization_plan;

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

    Ok(())
}
