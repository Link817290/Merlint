use colored::Colorize;

use merlint::proxy::spend_log::SpendLog;

pub fn run(days: u32, json: bool, insights: bool) -> anyhow::Result<()> {
    let log = SpendLog::open()?;

    if json {
        return run_json(&log, days);
    }

    if insights {
        return run_insights(&log, days);
    }

    let total = log.total_summary()?;
    let period = log.summary_last_days(days)?;
    let daily = log.daily_breakdown(days)?;
    let by_model = log.model_breakdown(days)?;
    let by_session = log.session_breakdown(days)?;

    // Header
    println!();
    println!("  {} {}", "merlint".purple().bold(), "spend report".bold());
    println!("  {}", "─".repeat(50).dimmed());

    // All-time summary
    println!();
    println!("  {} {}", "▸".purple(), "All Time".bold());
    println!("    Requests:     {}", format_num(total.request_count));
    println!("    Total Cost:   {}", format_cost(total.total_cost_usd).yellow());
    println!("    Saved:        {}", format_cost(total.total_saved_usd).green());
    println!("    Tokens:       {}", format_tokens(total.total_tokens));
    println!("    Tokens Saved: {}", format_tokens(total.total_tokens_saved).to_string().green());
    if total.total_cost_usd > 0.0 {
        let pct = (total.total_saved_usd / (total.total_cost_usd + total.total_saved_usd)) * 100.0;
        println!("    Save Rate:    {}", format!("{:.1}%", pct).green().bold());
    }

    // Period summary
    println!();
    println!("  {} {} {}", "▸".purple(), format!("Last {} Days", days).bold(), format!("({} requests)", period.request_count).dimmed());
    println!("    Cost:         {}", format_cost(period.total_cost_usd).yellow());
    println!("    Saved:        {}", format_cost(period.total_saved_usd).green());
    println!("    Tokens:       {}", format_tokens(period.total_tokens));

    // Daily breakdown
    if !daily.is_empty() {
        println!();
        println!("  {} {}", "▸".purple(), "Daily Breakdown".bold());
        println!("    {:<12} {:>10} {:>10} {:>8}", "Date", "Cost", "Saved", "Reqs");
        println!("    {}", "─".repeat(44).dimmed());
        for d in &daily {
            println!("    {:<12} {:>10} {:>10} {:>8}",
                d.date,
                format_cost(d.cost_usd),
                format_cost(d.saved_usd).green(),
                d.requests,
            );
        }
    }

    // By model
    if !by_model.is_empty() {
        println!();
        println!("  {} {}", "▸".purple(), "By Model".bold());
        println!("    {:<35} {:>10} {:>10} {:>8}", "Model", "Cost", "Saved", "Reqs");
        println!("    {}", "─".repeat(67).dimmed());
        for m in &by_model {
            let name = if m.model.len() > 35 { &m.model[..35] } else { &m.model };
            println!("    {:<35} {:>10} {:>10} {:>8}",
                name,
                format_cost(m.cost_usd),
                format_cost(m.saved_usd).green(),
                m.requests,
            );
        }
    }

    // By session
    if !by_session.is_empty() {
        println!();
        println!("  {} {}", "▸".purple(), "By Project/Session".bold());
        println!("    {:<24} {:>10} {:>10} {:>8}", "Session", "Cost", "Saved", "Reqs");
        println!("    {}", "─".repeat(56).dimmed());
        for s in by_session.iter().take(10) {
            let key = if s.session_key.starts_with("sys-") {
                format!("project:{}", &s.session_key[4..12.min(s.session_key.len())])
            } else if s.session_key.len() > 24 {
                format!("{}...", &s.session_key[..21])
            } else {
                s.session_key.clone()
            };
            println!("    {:<24} {:>10} {:>10} {:>8}",
                key,
                format_cost(s.cost_usd),
                format_cost(s.saved_usd).green(),
                s.requests,
            );
        }
    }

    println!();
    Ok(())
}

fn run_insights(log: &SpendLog, days: u32) -> anyhow::Result<()> {
    let insights = log.waste_insights(days)?;

    println!();
    println!("  {} {} (last {} days)", "merlint".purple().bold(), "waste insights".bold(), days);
    println!("  {}", "─".repeat(60).dimmed());

    if insights.is_empty() {
        println!();
        println!("  {} No waste patterns detected. Your usage looks efficient!", "✓".green().bold());
        println!();
        return Ok(());
    }

    let mut total_waste = 0.0;
    for (i, insight) in insights.iter().enumerate() {
        let kind_label = match insight.kind {
            merlint::proxy::spend_log::WasteKind::RepeatedReads => "Repeated Reads",
            merlint::proxy::spend_log::WasteKind::BloatedContext => "Bloated Context",
            merlint::proxy::spend_log::WasteKind::ExpensiveModel => "Expensive Model",
        };
        total_waste += insight.estimated_waste_usd;

        println!();
        println!("  {} {} {}", "▸".yellow(), format!("#{}", i + 1).bold(), kind_label.yellow().bold());
        if !insight.session_key.is_empty() {
            let key = if insight.session_key.starts_with("sys-") {
                format!("project:{}", &insight.session_key[4..12.min(insight.session_key.len())])
            } else {
                insight.session_key.clone()
            };
            println!("    Session: {}", key.dimmed());
        }
        println!("    {}", insight.description);
        println!("    Est. waste: {}", format_cost(insight.estimated_waste_usd).yellow());
    }

    println!();
    println!("  {} Total estimated waste: {}",
        "▸".purple(),
        format_cost(total_waste).yellow().bold(),
    );
    println!();
    Ok(())
}

fn run_json(log: &SpendLog, days: u32) -> anyhow::Result<()> {
    let total = log.total_summary()?;
    let period = log.summary_last_days(days)?;
    let daily = log.daily_breakdown(days)?;
    let by_model = log.model_breakdown(days)?;
    let by_session = log.session_breakdown(days)?;

    let output = serde_json::json!({
        "total": {
            "requests": total.request_count,
            "cost_usd": total.total_cost_usd,
            "saved_usd": total.total_saved_usd,
            "tokens": total.total_tokens,
            "tokens_saved": total.total_tokens_saved,
        },
        "period": {
            "days": days,
            "requests": period.request_count,
            "cost_usd": period.total_cost_usd,
            "saved_usd": period.total_saved_usd,
            "tokens": period.total_tokens,
            "tokens_saved": period.total_tokens_saved,
        },
        "daily": daily.iter().map(|d| serde_json::json!({
            "date": d.date, "cost_usd": d.cost_usd, "saved_usd": d.saved_usd,
            "tokens": d.tokens, "tokens_saved": d.tokens_saved, "requests": d.requests,
        })).collect::<Vec<_>>(),
        "by_model": by_model.iter().map(|m| serde_json::json!({
            "model": m.model, "cost_usd": m.cost_usd, "saved_usd": m.saved_usd,
            "tokens": m.tokens, "requests": m.requests,
        })).collect::<Vec<_>>(),
        "by_session": by_session.iter().map(|s| serde_json::json!({
            "session_key": s.session_key, "cost_usd": s.cost_usd, "saved_usd": s.saved_usd,
            "tokens": s.tokens, "tokens_saved": s.tokens_saved, "requests": s.requests,
        })).collect::<Vec<_>>(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

use merlint::util::format::{format_cost, format_tokens, format_num};
