use colored::Colorize;
use chrono;

use crate::history::db::{AggregateStats, HistoryDb, SessionRecord};

/// User profile summary derived from historical data
#[derive(Debug)]
pub struct UserProfile {
    pub stats: AggregateStats,
    pub habits: Vec<Habit>,
    pub trends: Vec<Trend>,
    pub recommendations: Vec<String>,
    pub tool_freq: Vec<crate::history::db::ToolFrequency>,
}

#[derive(Debug)]
pub struct Habit {
    pub name: String,
    pub description: String,
    pub severity: HabitSeverity,
}

#[derive(Debug)]
pub enum HabitSeverity {
    Good,
    Warning,
    Bad,
}

#[derive(Debug)]
pub struct Trend {
    pub metric: String,
    pub direction: TrendDirection,
    pub description: String,
}

#[derive(Debug)]
pub enum TrendDirection {
    Improving,
    Stable,
    Degrading,
}

/// Build a user profile from history database
pub fn build_profile(db: &HistoryDb) -> anyhow::Result<UserProfile> {
    let stats = db.aggregate_stats()?;
    let recent = db.list_sessions(50)?;

    let habits = detect_habits(&stats, &recent);
    let trends = detect_trends(&recent);
    let recommendations = generate_recommendations(&stats, &habits, &trends);
    let tool_freq = db.tool_frequency().unwrap_or_default();

    Ok(UserProfile {
        stats,
        habits,
        trends,
        recommendations,
        tool_freq,
    })
}

fn detect_habits(stats: &AggregateStats, _recent: &[SessionRecord]) -> Vec<Habit> {
    let mut habits = Vec::new();

    if stats.session_count < 2 {
        return habits;
    }

    // Cache habit
    if stats.avg_cache_hit > 0.7 {
        habits.push(Habit {
            name: "Cache Master".into(),
            description: format!(
                "Average cache hit ratio: {:.0}% — excellent prompt prefix stability",
                stats.avg_cache_hit * 100.0
            ),
            severity: HabitSeverity::Good,
        });
    } else if stats.avg_cache_hit < 0.3 {
        habits.push(Habit {
            name: "Cache Buster".into(),
            description: format!(
                "Average cache hit ratio: {:.0}% — prompt prefixes are unstable, wasting tokens",
                stats.avg_cache_hit * 100.0
            ),
            severity: HabitSeverity::Bad,
        });
    }

    // Tool bloat habit
    if stats.avg_unused_tools > 5.0 {
        habits.push(Habit {
            name: "Tool Hoarder".into(),
            description: format!(
                "Average {:.0} unused tools per session — each tool definition costs tokens",
                stats.avg_unused_tools
            ),
            severity: HabitSeverity::Bad,
        });
    } else if stats.avg_unused_tools < 1.0 {
        habits.push(Habit {
            name: "Tool Minimalist".into(),
            description: "Very few unused tools — clean tool definitions".into(),
            severity: HabitSeverity::Good,
        });
    }

    // Retry habit
    let avg_retries = if stats.session_count > 0 {
        stats.total_retries as f64 / stats.session_count as f64
    } else {
        0.0
    };
    if avg_retries > 3.0 {
        habits.push(Habit {
            name: "Retry Loop".into(),
            description: format!(
                "Average {:.1} retries per session — indicates unclear instructions or tool failures",
                avg_retries
            ),
            severity: HabitSeverity::Bad,
        });
    }

    // Loop pattern habit
    let avg_loops = if stats.session_count > 0 {
        stats.total_loops as f64 / stats.session_count as f64
    } else {
        0.0
    };
    if avg_loops > 2.0 {
        habits.push(Habit {
            name: "Loop Prone".into(),
            description: format!(
                "Average {:.1} loop patterns per session — agent frequently repeats tool calls",
                avg_loops
            ),
            severity: HabitSeverity::Warning,
        });
    }

    // Token efficiency
    if stats.avg_tokens_per_call > 10000.0 {
        habits.push(Habit {
            name: "Heavy Context".into(),
            description: format!(
                "Average {:.0} tokens per call — context windows are very large",
                stats.avg_tokens_per_call
            ),
            severity: HabitSeverity::Warning,
        });
    } else if stats.avg_tokens_per_call < 3000.0 && stats.avg_tokens_per_call > 0.0 {
        habits.push(Habit {
            name: "Lean Context".into(),
            description: format!(
                "Average {:.0} tokens per call — efficient context usage",
                stats.avg_tokens_per_call
            ),
            severity: HabitSeverity::Good,
        });
    }

    // Redundant reads
    let avg_redundant = if stats.session_count > 0 {
        stats.total_redundant_reads as f64 / stats.session_count as f64
    } else {
        0.0
    };
    if avg_redundant > 3.0 {
        habits.push(Habit {
            name: "Re-reader".into(),
            description: format!(
                "Average {:.1} redundant file reads per session — agent re-reads files it already saw",
                avg_redundant
            ),
            severity: HabitSeverity::Warning,
        });
    }

    habits
}

fn detect_trends(recent: &[SessionRecord]) -> Vec<Trend> {
    let mut trends = Vec::new();

    if recent.len() < 4 {
        return trends;
    }

    let half = recent.len() / 2;
    let newer = &recent[..half];
    let older = &recent[half..];

    // Token trend
    let new_avg_tokens = avg_field(newer, |r| r.total_tokens as f64);
    let old_avg_tokens = avg_field(older, |r| r.total_tokens as f64);
    if old_avg_tokens > 0.0 {
        let change = (new_avg_tokens - old_avg_tokens) / old_avg_tokens;
        if change < -0.15 {
            trends.push(Trend {
                metric: "Token Usage".into(),
                direction: TrendDirection::Improving,
                description: format!(
                    "Token usage decreased by {:.0}% ({:.0} -> {:.0} avg per session)",
                    change.abs() * 100.0, old_avg_tokens, new_avg_tokens
                ),
            });
        } else if change > 0.15 {
            trends.push(Trend {
                metric: "Token Usage".into(),
                direction: TrendDirection::Degrading,
                description: format!(
                    "Token usage increased by {:.0}% ({:.0} -> {:.0} avg per session)",
                    change * 100.0, old_avg_tokens, new_avg_tokens
                ),
            });
        } else {
            trends.push(Trend {
                metric: "Token Usage".into(),
                direction: TrendDirection::Stable,
                description: format!("Token usage stable (~{:.0} avg per session)", new_avg_tokens),
            });
        }
    }

    // Cache hit trend
    let new_avg_cache = avg_field(newer, |r| r.cache_hit_ratio);
    let old_avg_cache = avg_field(older, |r| r.cache_hit_ratio);
    let cache_delta = new_avg_cache - old_avg_cache;
    if cache_delta > 0.1 {
        trends.push(Trend {
            metric: "Cache Hit Ratio".into(),
            direction: TrendDirection::Improving,
            description: format!(
                "Cache hit ratio improved: {:.0}% -> {:.0}%",
                old_avg_cache * 100.0, new_avg_cache * 100.0
            ),
        });
    } else if cache_delta < -0.1 {
        trends.push(Trend {
            metric: "Cache Hit Ratio".into(),
            direction: TrendDirection::Degrading,
            description: format!(
                "Cache hit ratio degraded: {:.0}% -> {:.0}%",
                old_avg_cache * 100.0, new_avg_cache * 100.0
            ),
        });
    }

    // Retry trend
    let new_avg_retries = avg_field(newer, |r| r.retry_count as f64);
    let old_avg_retries = avg_field(older, |r| r.retry_count as f64);
    if old_avg_retries > 1.0 && new_avg_retries < old_avg_retries * 0.7 {
        trends.push(Trend {
            metric: "Retries".into(),
            direction: TrendDirection::Improving,
            description: format!(
                "Retries decreased: {:.1} -> {:.1} per session",
                old_avg_retries, new_avg_retries
            ),
        });
    } else if new_avg_retries > old_avg_retries * 1.5 && new_avg_retries > 1.0 {
        trends.push(Trend {
            metric: "Retries".into(),
            direction: TrendDirection::Degrading,
            description: format!(
                "Retries increased: {:.1} -> {:.1} per session",
                old_avg_retries, new_avg_retries
            ),
        });
    }

    trends
}

fn generate_recommendations(
    stats: &AggregateStats,
    habits: &[Habit],
    _trends: &[Trend],
) -> Vec<String> {
    let mut recs = Vec::new();

    for habit in habits {
        match habit.severity {
            HabitSeverity::Bad => match habit.name.as_str() {
                "Cache Buster" => {
                    recs.push("Run `merlint optimize` to generate a CLAUDE.md with stable prompt prefixes".into());
                }
                "Tool Hoarder" => {
                    recs.push("Run `merlint optimize` to generate a .merlint-tools.json allowlist pruning unused tools".into());
                }
                "Retry Loop" => {
                    recs.push("Review your agent instructions for clarity — retries usually mean ambiguous task definitions".into());
                }
                _ => {}
            },
            HabitSeverity::Warning => match habit.name.as_str() {
                "Loop Prone" => {
                    recs.push("Add explicit stop conditions in your agent prompts to prevent tool call loops".into());
                }
                "Heavy Context" => {
                    recs.push("Consider splitting large tasks into smaller subtasks to reduce context window size".into());
                }
                "Re-reader" => {
                    recs.push("Instruct your agent to cache file contents in memory instead of re-reading".into());
                }
                _ => {}
            },
            HabitSeverity::Good => {}
        }
    }

    if stats.session_count >= 10 && recs.is_empty() {
        recs.push("Your agent usage patterns look healthy! Keep it up.".into());
    }

    recs
}

fn avg_field(records: &[SessionRecord], f: impl Fn(&SessionRecord) -> f64) -> f64 {
    if records.is_empty() {
        return 0.0;
    }
    let sum: f64 = records.iter().map(&f).sum();
    sum / records.len() as f64
}

/// Print profile to terminal
pub fn print_profile(profile: &UserProfile) {
    println!();
    println!("{}", "  ========================================".cyan());
    println!("{}", "    merlint — User Profile".cyan().bold());
    println!("{}", "  ========================================".cyan());
    println!();

    let s = &profile.stats;
    println!("  {} sessions analyzed", s.session_count);
    println!("  {} total tokens consumed", format_tokens(s.total_tokens));
    println!("  {:.0} avg tokens/session", s.avg_tokens_per_session);
    println!("  {:.0} avg tokens/call", s.avg_tokens_per_call);
    println!("  {:.0}% avg cache hit ratio", s.avg_cache_hit * 100.0);
    println!();

    if !profile.habits.is_empty() {
        println!("{}", "  Habits:".bold());
        for habit in &profile.habits {
            let icon = match habit.severity {
                HabitSeverity::Good => "+".green(),
                HabitSeverity::Warning => "~".yellow(),
                HabitSeverity::Bad => "!".red(),
            };
            let name = match habit.severity {
                HabitSeverity::Good => habit.name.green(),
                HabitSeverity::Warning => habit.name.yellow(),
                HabitSeverity::Bad => habit.name.red(),
            };
            println!("    [{}] {}: {}", icon, name.bold(), habit.description);
        }
        println!();
    }

    if !profile.trends.is_empty() {
        println!("{}", "  Trends:".bold());
        for trend in &profile.trends {
            let icon = match trend.direction {
                TrendDirection::Improving => "^".green(),
                TrendDirection::Stable => "=".white(),
                TrendDirection::Degrading => "v".red(),
            };
            println!("    [{}] {}: {}", icon, trend.metric.bold(), trend.description);
        }
        println!();
    }

    if !profile.recommendations.is_empty() {
        println!("{}", "  Recommendations:".bold());
        for rec in &profile.recommendations {
            println!("    -> {}", rec);
        }
        println!();
    }

    if profile.habits.is_empty() && profile.trends.is_empty() {
        println!("  Not enough data yet. Analyze more sessions to build your profile.");
        println!("  Run: merlint scan  (to analyze a session and store results)");
        println!();
    }

    // Show tool frequency if available
    if !profile.tool_freq.is_empty() {
        println!("{}", "  Tool Usage Frequency:".bold());
        for tf in &profile.tool_freq {
            let pct = if s.session_count > 0 {
                tf.session_count as f64 / s.session_count as f64 * 100.0
            } else {
                0.0
            };
            let bar_len = (pct / 10.0).round() as usize;
            let bar = "#".repeat(bar_len);
            let pad = ".".repeat(10 - bar_len);
            println!("    {:>16}  [{}{}] {:>3.0}%  ({} calls)",
                tf.tool_name, bar.green(), pad.dimmed(), pct, tf.total_calls);
        }
        println!();
    }
}

/// Convert profile to JSON-serializable format
pub fn profile_to_json(profile: &UserProfile) -> serde_json::Value {
    serde_json::json!({
        "generated_at": chrono::Local::now().to_rfc3339(),
        "sessions_analyzed": profile.stats.session_count,
        "total_tokens_consumed": profile.stats.total_tokens,
        "avg_tokens_per_session": profile.stats.avg_tokens_per_session,
        "avg_tokens_per_call": profile.stats.avg_tokens_per_call,
        "avg_cache_hit_ratio": profile.stats.avg_cache_hit,
        "avg_prefix_stability": profile.stats.avg_prefix_stability,
        "total_retries": profile.stats.total_retries,
        "total_loops": profile.stats.total_loops,
        "total_redundant_reads": profile.stats.total_redundant_reads,
        "avg_unused_tools": profile.stats.avg_unused_tools,
        "habits": profile.habits.iter().map(|h| {
            serde_json::json!({
                "name": h.name,
                "description": h.description,
                "severity": format!("{:?}", h.severity),
            })
        }).collect::<Vec<_>>(),
        "trends": profile.trends.iter().map(|t| {
            serde_json::json!({
                "metric": t.metric,
                "direction": format!("{:?}", t.direction),
                "description": t.description,
            })
        }).collect::<Vec<_>>(),
        "tool_frequency": profile.tool_freq.iter().map(|f| {
            serde_json::json!({
                "tool": f.tool_name,
                "total_calls": f.total_calls,
                "session_count": f.session_count,
                "usage_ratio": if profile.stats.session_count > 0 {
                    f.session_count as f64 / profile.stats.session_count as f64
                } else { 0.0 },
            })
        }).collect::<Vec<_>>(),
        "recommendations": profile.recommendations,
    })
}

/// Generate a pruning recommendation based on accumulated tool usage history.
/// Returns (tools_to_keep, tools_to_prune) given the full set of defined tools.
pub fn recommend_pruning(
    db: &HistoryDb,
    all_defined_tools: &[String],
) -> anyhow::Result<PruningRecommendation> {
    let total_sessions = db.session_count()?;
    if total_sessions < 3 {
        // Not enough data to make recommendations
        return Ok(PruningRecommendation {
            keep: all_defined_tools.to_vec(),
            prune: vec![],
            confidence: 0.0,
            data_sessions: total_sessions,
        });
    }

    let freq = db.tool_frequency()?;
    let freq_map: std::collections::HashMap<String, f64> = freq.iter()
        .map(|f| (f.tool_name.clone(), f.session_count as f64 / total_sessions as f64))
        .collect();

    let mut keep = Vec::new();
    let mut prune = Vec::new();

    // Threshold: if a tool was used in fewer than 10% of sessions, prune it
    let threshold = 0.1;

    for tool in all_defined_tools {
        let usage_ratio = freq_map.get(tool).copied().unwrap_or(0.0);
        if usage_ratio >= threshold {
            keep.push(tool.clone());
        } else {
            prune.push(tool.clone());
        }
    }

    // Confidence is based on how many sessions we've seen
    let confidence = (total_sessions as f64 / 20.0).min(1.0); // Full confidence at 20+ sessions

    Ok(PruningRecommendation {
        keep,
        prune,
        confidence,
        data_sessions: total_sessions,
    })
}

#[derive(Debug)]
pub struct PruningRecommendation {
    pub keep: Vec<String>,
    pub prune: Vec<String>,
    pub confidence: f64,
    pub data_sessions: i64,
}

use crate::util::format::format_tokens;
