use std::path::PathBuf;

use merlint::optimizer;

use super::helpers::{
    agent_kind_str, analyze_session, build_optimization_plan, find_latest_session, load_from_source,
    store_to_history, AgentFilter,
};

pub async fn run(
    agent: Option<AgentFilter>,
    interval: u64,
    auto_optimize: bool,
    target: PathBuf,
) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();
    eprintln!(
        "Monitoring agent sessions (interval: {}s, auto-optimize: {})",
        interval, auto_optimize
    );
    eprintln!("Press Ctrl+C to stop.\n");

    let mut last_seen: Option<(PathBuf, std::time::SystemTime)> = None;

    loop {
        if let Some((path, kind)) = find_latest_session(&agent) {
            let mtime = path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

            let is_new = last_seen
                .as_ref()
                .map(|(p, t)| p != &path || *t != mtime)
                .unwrap_or(true);

            if is_new {
                eprintln!(
                    "[{}] New/updated session: {}",
                    chrono::Local::now().format("%H:%M:%S"),
                    path.display()
                );
                last_seen = Some((path.clone(), mtime));

                if let Ok(session) = load_from_source(&path, kind) {
                    if !session.entries.is_empty() {
                        let (ts, ca, ea) = analyze_session(&session);

                        store_to_history(
                            &session.id,
                            &path.display().to_string(),
                            agent_kind_str(kind),
                            &ts,
                            &ea,
                            &ca,
                        );

                        let plan = build_optimization_plan(&session);
                        if !plan.is_empty() {
                            optimizer::applier::print_plan(&plan);
                            if auto_optimize {
                                let results =
                                    optimizer::applier::apply_plan(&plan, &target, false);
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
