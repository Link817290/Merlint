use std::path::PathBuf;

use merlint::history;
use merlint::profile as profile_mod;

pub fn run(json: bool, output: Option<PathBuf>) -> anyhow::Result<()> {
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

    match profile_mod::engine::build_profile(&db) {
        Ok(p) => {
            if json || output.is_some() {
                let report = profile_mod::engine::profile_to_json(&p);
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
                profile_mod::engine::print_profile(&p);
            }
        }
        Err(e) => eprintln!("Failed to build profile: {}", e),
    }

    Ok(())
}
