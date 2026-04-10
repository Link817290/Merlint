use std::path::PathBuf;

use colored::Colorize;

use merlint::deep;
use merlint::optimizer;
use merlint::parser;
use merlint::parser::discover::AgentKind;
use merlint::report;

use super::helpers::{agent_kind_str, analyze_session, build_optimization_plan, load_from_source, store_to_history};

pub async fn run(deep: bool) -> anyhow::Result<()> {
    let sources = parser::discover::discover_agents();
    if sources.is_empty() {
        println!("No agent sessions found on this machine.");
        println!();
        println!("Looked for:");
        println!("  Claude Code: ~/.claude/projects/*/sessions/");
        println!("  Codex:       ~/.local/share/codex-cli/ or ~/.codex/");
        return Ok(());
    }

    let mut all_sessions: Vec<(PathBuf, AgentKind, String, String)> = Vec::new();
    for src in &sources {
        let kind_str = match src.kind {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex CLI",
        };
        let sessions = parser::discover::list_sessions(&src.session_dir);
        for s in sessions {
            let size = s.metadata().ok().map(|m| m.len()).unwrap_or(0);
            let mtime = s
                .metadata()
                .ok()
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
            let name = s
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
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

    store_to_history(
        &session.id,
        &path.display().to_string(),
        agent_kind_str(*kind),
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

    Ok(())
}
