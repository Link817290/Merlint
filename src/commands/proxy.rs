use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use merlint::history;
use merlint::models::trace::TraceSession;
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
    if !daemon {
        tracing_subscriber::fmt()
            .with_target(false)
            .with_level(true)
            .init();
    }

    let session = Arc::new(Mutex::new(TraceSession::new()));
    let session_for_shutdown = session.clone();
    let output_path = output.clone();
    let is_daemon = daemon;

    let transformer = if optimize {
        let tx = proxy::transformer::new_shared_transformer();
        if let Ok(db) = history::db::HistoryDb::open() {
            if let (Ok(freq), Ok(total)) = (db.tool_frequency(), db.session_count()) {
                if total >= 3 {
                    let freq_data: Vec<(String, i64)> = freq
                        .iter()
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
        listen_port: port,
        target_url: target,
        api_key,
        live_trace_file: Some(output),
        optimize,
    };

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

    Ok(())
}
