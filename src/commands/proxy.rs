use std::path::PathBuf;

use merlint::history;
use merlint::proxy;

use super::helpers::run_and_print_report;

pub async fn run(
    port: u16,
    target: String,
    api_key: Option<String>,
    output: PathBuf,
    daemon: bool,
    optimize: bool,
) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .with_writer(std::io::stderr)
        .init();

    // Create multi-session store
    let store = proxy::session_store::new_session_store(optimize);

    // Load historical tool data into the store
    if optimize {
        if let Ok(db) = history::db::HistoryDb::open() {
            if let (Ok(freq), Ok(total)) = (db.tool_frequency(), db.session_count()) {
                if total >= 3 {
                    let freq_data: Vec<(String, i64)> = freq
                        .iter()
                        .map(|f| (f.tool_name.clone(), f.session_count))
                        .collect();
                    let mut s = store.lock().await;
                    s.set_history(freq_data, total);
                    tracing::info!("Loaded tool history from {} sessions", total);
                }
            }
        }
    }

    let store_for_shutdown = store.clone();
    let output_path = output.clone();
    let is_daemon = daemon;

    // Use the output path's parent as the trace directory
    let trace_dir = output.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&trace_dir)?;

    // Initialize persistent spend log
    let spend_log = match proxy::spend_log::new_spend_log() {
        Ok(sl) => {
            tracing::info!("Spend tracking initialized (~/.merlint/spend.db)");
            Some(sl)
        }
        Err(e) => {
            tracing::warn!("Failed to initialize spend log: {} (continuing without)", e);
            None
        }
    };

    let config = proxy::server::ProxyConfig {
        listen_port: port,
        target_url: target,
        api_key,
        live_trace_dir: Some(trace_dir),
        optimize,
    };

    tokio::select! {
        result = proxy::server::run_proxy(config, store.clone(), spend_log) => {
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            if !is_daemon { eprintln!("\n\nShutting down proxy..."); }

            let store_guard = store_for_shutdown.lock().await;
            let sessions = store_guard.all_slots();
            let session_count = sessions.len();

            for slot in &sessions {
                let (key, session, tx_opt) = (slot.key, slot.session, slot.transformer);
                // Save trace file per session
                let file_name = if key == "default" {
                    output_path.clone()
                } else {
                    let dir = output_path.parent().unwrap_or_else(|| std::path::Path::new("."));
                    dir.join(format!("session-{}.json", sanitize_key(key)))
                };

                if let Ok(json) = serde_json::to_string_pretty(session) {
                    let _ = std::fs::write(&file_name, &json);
                    if !is_daemon {
                        eprintln!("Traces saved: {} ({} entries) -> {}",
                            key, session.entries.len(), file_name.display());
                    }
                }

                // Print report for non-empty sessions
                if !is_daemon && !session.entries.is_empty() {
                    if session_count > 1 {
                        eprintln!("\n--- Session: {} ---", key);
                    }
                    run_and_print_report(session);
                }

                // Persist tool usage to history DB
                if let Some(tx) = tx_opt {
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

            if !is_daemon && session_count > 1 {
                eprintln!("\n{} sessions tracked in total.", session_count);
            }
        }
    }

    Ok(())
}

fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
