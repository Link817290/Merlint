use colored::Colorize;

use merlint::history;

use super::helpers::{format_tokens, ReportPeriod};

pub fn run(period: ReportPeriod, count: usize) -> anyhow::Result<()> {
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
        let avg_cache: f64 =
            sessions.iter().map(|s| s.cache_hit_ratio).sum::<f64>() / sessions.len() as f64;
        let total_retries: i64 = sessions.iter().map(|s| s.retry_count).sum();

        let label = if i == 0 {
            format!("{} (current)", period_name)
        } else {
            format!("{} -{}", period_name, i)
        };

        println!("  {} ({} ~ {})", label.bold(), from, to);
        println!(
            "    Sessions: {}  |  Total tokens: {}  |  Avg/session: {:.0}",
            sessions.len(),
            format_tokens(total_tokens),
            avg_tokens,
        );
        println!(
            "    Cache hit: {:.0}%  |  Retries: {}",
            avg_cache * 100.0,
            total_retries,
        );
        println!();
    }

    let all = db.list_sessions(100)?;
    if all.len() >= 4 {
        let half = all.len() / 2;
        let newer_avg: f64 =
            all[..half].iter().map(|s| s.total_tokens as f64).sum::<f64>() / half as f64;
        let older_avg: f64 = all[half..]
            .iter()
            .map(|s| s.total_tokens as f64)
            .sum::<f64>()
            / (all.len() - half) as f64;

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

    Ok(())
}
