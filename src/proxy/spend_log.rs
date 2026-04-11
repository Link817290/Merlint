use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{params, Connection};
use tokio::sync::Mutex;

/// Persistent per-request spend log stored in ~/.merlint/spend.db
pub struct SpendLog {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct SpendEntry {
    pub request_id: String,
    pub timestamp: String,
    pub session_key: String,
    /// Absolute working directory for the project this request belongs to,
    /// when extract_project_path succeeded. Empty for legacy rows or for
    /// background/unidentified requests.
    pub project_path: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: f64,
    pub cost_saved_usd: f64,
    pub tokens_saved: i64,
    pub latency_ms: u64,
    pub tools_called: String, // JSON array
    pub status: u16,
}

/// Cumulative stats for a single session_key, aggregated across every entry
/// in the spend_log table. Loaded once when a SessionSlot is created so the
/// dashboard can show lifetime totals without losing the per-run live counts.
#[derive(Debug, Clone, Default)]
pub struct HistoricalSummary {
    pub request_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    /// Sum of `cache_creation_input_tokens` across historical entries. Needed
    /// so the dashboard can compute a truthful cache hit rate after a restart
    /// (total_prompt = fresh + cache_read + cache_creation). Without this, any
    /// historical cache write is silently dropped and the displayed hit rate
    /// over-reports by the fraction of writes to the total input.
    pub cache_creation_tokens: u64,
    pub total_latency_ms: u64,
    pub cost_usd: f64,
    pub tokens_saved: i64,
}

/// One row from `recent_sessions`: everything needed to build an inactive
/// SessionSlot at proxy startup.
#[derive(Debug, Clone)]
pub struct RecentSession {
    pub session_key: String,
    pub project_path: String,
    pub summary: HistoricalSummary,
    pub last_activity: String,
}

impl SpendLog {
    pub fn open() -> Result<Self> {
        Self::open_at(&Self::db_path()?)
    }

    /// Open a SpendLog at an explicit filesystem path. Used by tests to
    /// isolate spend data from the user's real ~/.merlint/spend.db without
    /// having to mutate process-wide env vars.
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".merlint").join("spend.db"))
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS spend_log (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                request_id           TEXT NOT NULL,
                timestamp            TEXT NOT NULL,
                session_key          TEXT NOT NULL,
                project_path         TEXT NOT NULL DEFAULT '',
                model                TEXT NOT NULL DEFAULT '',
                prompt_tokens        INTEGER NOT NULL DEFAULT 0,
                completion_tokens    INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens    INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd             REAL NOT NULL DEFAULT 0.0,
                cost_saved_usd       REAL NOT NULL DEFAULT 0.0,
                tokens_saved         INTEGER NOT NULL DEFAULT 0,
                latency_ms           INTEGER NOT NULL DEFAULT 0,
                tools_called         TEXT NOT NULL DEFAULT '[]',
                status               INTEGER NOT NULL DEFAULT 200
            );

            CREATE INDEX IF NOT EXISTS idx_spend_timestamp ON spend_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_spend_session ON spend_log(session_key);
            CREATE INDEX IF NOT EXISTS idx_spend_model ON spend_log(model);

            CREATE TABLE IF NOT EXISTS daily_spend (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                date                 TEXT NOT NULL,
                session_key          TEXT NOT NULL,
                model                TEXT NOT NULL DEFAULT '',
                total_cost_usd       REAL NOT NULL DEFAULT 0.0,
                total_saved_usd      REAL NOT NULL DEFAULT 0.0,
                total_tokens         INTEGER NOT NULL DEFAULT 0,
                total_tokens_saved   INTEGER NOT NULL DEFAULT 0,
                request_count        INTEGER NOT NULL DEFAULT 0,
                UNIQUE(date, session_key, model)
            );

            CREATE INDEX IF NOT EXISTS idx_daily_date ON daily_spend(date);
            CREATE INDEX IF NOT EXISTS idx_daily_session ON daily_spend(session_key);"
        )?;

        // Add project_path column to databases that pre-date this migration.
        // SQLite errors out if the column already exists, so we first check
        // PRAGMA table_info and only ALTER when absent.
        if !self.column_exists("spend_log", "project_path")? {
            self.conn.execute(
                "ALTER TABLE spend_log ADD COLUMN project_path TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({})", table))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Insert a spend log entry and update the daily aggregation.
    pub fn log(&self, entry: &SpendEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO spend_log (
                request_id, timestamp, session_key, project_path, model,
                prompt_tokens, completion_tokens,
                cache_read_tokens, cache_creation_tokens,
                cost_usd, cost_saved_usd, tokens_saved,
                latency_ms, tools_called, status
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                entry.request_id,
                entry.timestamp,
                entry.session_key,
                entry.project_path,
                entry.model,
                entry.prompt_tokens as i64,
                entry.completion_tokens as i64,
                entry.cache_read_tokens as i64,
                entry.cache_creation_tokens as i64,
                entry.cost_usd,
                entry.cost_saved_usd,
                entry.tokens_saved,
                entry.latency_ms as i64,
                entry.tools_called,
                entry.status as i64,
            ],
        )?;

        // Upsert daily aggregation
        let date = &entry.timestamp[..10]; // "2025-04-10"
        let total_tokens = entry.prompt_tokens + entry.completion_tokens;
        self.conn.execute(
            "INSERT INTO daily_spend (date, session_key, model, total_cost_usd, total_saved_usd, total_tokens, total_tokens_saved, request_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
             ON CONFLICT(date, session_key, model) DO UPDATE SET
                total_cost_usd = total_cost_usd + ?4,
                total_saved_usd = total_saved_usd + ?5,
                total_tokens = total_tokens + ?6,
                total_tokens_saved = total_tokens_saved + ?7,
                request_count = request_count + 1",
            params![
                date,
                entry.session_key,
                entry.model,
                entry.cost_usd,
                entry.cost_saved_usd,
                total_tokens as i64,
                entry.tokens_saved,
            ],
        )?;

        Ok(())
    }

    /// Get total spend summary across all time.
    pub fn total_summary(&self) -> Result<SpendSummary> {
        self.conn.query_row(
            "SELECT
                COUNT(*) as requests,
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(cost_saved_usd), 0.0),
                COALESCE(SUM(prompt_tokens + completion_tokens), 0),
                COALESCE(SUM(tokens_saved), 0),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0)
            FROM spend_log",
            [],
            |row| Ok(SpendSummary {
                request_count: row.get(0)?,
                total_cost_usd: row.get(1)?,
                total_saved_usd: row.get(2)?,
                total_tokens: row.get(3)?,
                total_tokens_saved: row.get(4)?,
                total_fresh_input_tokens: row.get(5)?,
                total_cache_read_tokens: row.get(6)?,
                total_cache_creation_tokens: row.get(7)?,
            }),
        ).map_err(Into::into)
    }

    /// Get spend summary for the last N days.
    pub fn summary_last_days(&self, days: u32) -> Result<SpendSummary> {
        self.conn.query_row(
            "SELECT
                COUNT(*) as requests,
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(cost_saved_usd), 0.0),
                COALESCE(SUM(prompt_tokens + completion_tokens), 0),
                COALESCE(SUM(tokens_saved), 0),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0)
            FROM spend_log
            WHERE timestamp >= datetime('now', ?1)",
            params![format!("-{} days", days)],
            |row| Ok(SpendSummary {
                request_count: row.get(0)?,
                total_cost_usd: row.get(1)?,
                total_saved_usd: row.get(2)?,
                total_tokens: row.get(3)?,
                total_tokens_saved: row.get(4)?,
                total_fresh_input_tokens: row.get(5)?,
                total_cache_read_tokens: row.get(6)?,
                total_cache_creation_tokens: row.get(7)?,
            }),
        ).map_err(Into::into)
    }

    /// Get daily spend breakdown for the last N days.
    pub fn daily_breakdown(&self, days: u32) -> Result<Vec<DailySpendRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, SUM(total_cost_usd), SUM(total_saved_usd),
                    SUM(total_tokens), SUM(total_tokens_saved), SUM(request_count)
             FROM daily_spend
             WHERE date >= date('now', ?1)
             GROUP BY date
             ORDER BY date DESC"
        )?;
        let rows = stmt.query_map(
            params![format!("-{} days", days)],
            |row| Ok(DailySpendRow {
                date: row.get(0)?,
                cost_usd: row.get(1)?,
                saved_usd: row.get(2)?,
                tokens: row.get(3)?,
                tokens_saved: row.get(4)?,
                requests: row.get(5)?,
            }),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get per-session spend breakdown for the last N days.
    pub fn session_breakdown(&self, days: u32) -> Result<Vec<SessionSpendRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_key, SUM(total_cost_usd), SUM(total_saved_usd),
                    SUM(total_tokens), SUM(total_tokens_saved), SUM(request_count)
             FROM daily_spend
             WHERE date >= date('now', ?1)
             GROUP BY session_key
             ORDER BY SUM(total_cost_usd) DESC"
        )?;
        let rows = stmt.query_map(
            params![format!("-{} days", days)],
            |row| Ok(SessionSpendRow {
                session_key: row.get(0)?,
                cost_usd: row.get(1)?,
                saved_usd: row.get(2)?,
                tokens: row.get(3)?,
                tokens_saved: row.get(4)?,
                requests: row.get(5)?,
            }),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List all distinct session_keys with activity in the last `days` days,
    /// along with their project_path and cumulative stats. Used at proxy
    /// startup to pre-populate the dashboard with recent sessions before
    /// any new request arrives.
    pub fn recent_sessions(&self, days: u32) -> Result<Vec<RecentSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                session_key,
                MAX(project_path) as project_path,
                COUNT(*) as req,
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(latency_ms), 0),
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(tokens_saved), 0),
                MAX(timestamp) as last_ts
             FROM spend_log
             WHERE timestamp >= datetime('now', ?1)
             GROUP BY session_key
             ORDER BY last_ts DESC"
        )?;
        let rows = stmt.query_map(
            params![format!("-{} days", days)],
            |row| Ok(RecentSession {
                session_key: row.get(0)?,
                project_path: row.get::<_, String>(1).unwrap_or_default(),
                summary: HistoricalSummary {
                    request_count: row.get::<_, i64>(2)? as u64,
                    prompt_tokens: row.get::<_, i64>(3)? as u64,
                    completion_tokens: row.get::<_, i64>(4)? as u64,
                    cache_read_tokens: row.get::<_, i64>(5)? as u64,
                    cache_creation_tokens: row.get::<_, i64>(6)? as u64,
                    total_latency_ms: row.get::<_, i64>(7)? as u64,
                    cost_usd: row.get(8)?,
                    tokens_saved: row.get(9)?,
                },
                last_activity: row.get(10)?,
            }),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get cumulative historical stats for a single session_key. Returns
    /// zeros if the session has no prior entries. Used on slot creation so
    /// the dashboard can surface carried-over stats after a proxy restart.
    pub fn session_history(&self, session_key: &str) -> Result<HistoricalSummary> {
        self.conn.query_row(
            "SELECT
                COUNT(*) as req,
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(latency_ms), 0),
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(tokens_saved), 0)
             FROM spend_log
             WHERE session_key = ?1",
            params![session_key],
            |row| Ok(HistoricalSummary {
                request_count: row.get::<_, i64>(0)? as u64,
                prompt_tokens: row.get::<_, i64>(1)? as u64,
                completion_tokens: row.get::<_, i64>(2)? as u64,
                cache_read_tokens: row.get::<_, i64>(3)? as u64,
                cache_creation_tokens: row.get::<_, i64>(4)? as u64,
                total_latency_ms: row.get::<_, i64>(5)? as u64,
                cost_usd: row.get(6)?,
                tokens_saved: row.get(7)?,
            }),
        ).map_err(Into::into)
    }

    /// Get tool frequency for a specific session_key (project).
    /// Returns tools sorted by frequency descending.
    pub fn tool_frequency_for_session(&self, session_key: &str) -> Result<Vec<(String, i64)>> {
        // Parse tools_called JSON arrays and aggregate
        let mut stmt = self.conn.prepare(
            "SELECT tools_called FROM spend_log
             WHERE session_key = ?1 AND tools_called != '[]'
             ORDER BY timestamp DESC LIMIT 50"
        )?;
        let rows = stmt.query_map(params![session_key], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })?;

        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut total_requests: i64 = 0;
        for row in rows {
            if let Ok(json) = row {
                if let Ok(tools) = serde_json::from_str::<Vec<String>>(&json) {
                    total_requests += 1;
                    for tool in tools {
                        *counts.entry(tool).or_insert(0) += 1;
                    }
                }
            }
        }

        if total_requests < 3 {
            return Ok(Vec::new()); // Not enough data
        }

        let mut result: Vec<(String, i64)> = counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(result)
    }

    /// Get the number of distinct requests for a session_key.
    pub fn session_request_count(&self, session_key: &str) -> Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM spend_log WHERE session_key = ?1",
            params![session_key],
            |row| row.get(0),
        ).map_err(Into::into)
    }

    /// Get per-model spend breakdown for the last N days.
    pub fn model_breakdown(&self, days: u32) -> Result<Vec<ModelSpendRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT model, SUM(total_cost_usd), SUM(total_saved_usd),
                    SUM(total_tokens), SUM(request_count)
             FROM daily_spend
             WHERE date >= date('now', ?1)
             GROUP BY model
             ORDER BY SUM(total_cost_usd) DESC"
        )?;
        let rows = stmt.query_map(
            params![format!("-{} days", days)],
            |row| Ok(ModelSpendRow {
                model: row.get(0)?,
                cost_usd: row.get(1)?,
                saved_usd: row.get(2)?,
                tokens: row.get(3)?,
                requests: row.get(4)?,
            }),
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[derive(Debug, Clone)]
pub struct SpendSummary {
    pub request_count: i64,
    pub total_cost_usd: f64,
    pub total_saved_usd: f64,
    pub total_tokens: i64,
    pub total_tokens_saved: i64,
    /// Sum of `prompt_tokens` (fresh input) across entries. Used together
    /// with cache_read/write totals to compute a net cache savings figure
    /// that matches the per-session breakdown (including cache-write overhead).
    pub total_fresh_input_tokens: i64,
    /// Sum of `cache_read_tokens` across entries in the window. The dashboard
    /// uses this to compute a truthful "cache savings today" number — the
    /// dollar value Anthropic's prompt cache shaved off the theoretical
    /// no-cache cost, which is the real source of savings for most users.
    pub total_cache_read_tokens: i64,
    /// Sum of `cache_creation_tokens` across entries. Netted against
    /// read savings so the headline savings doesn't over-report on turns
    /// that were mostly building cache rather than reading from it.
    pub total_cache_creation_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct DailySpendRow {
    pub date: String,
    pub cost_usd: f64,
    pub saved_usd: f64,
    pub tokens: i64,
    pub tokens_saved: i64,
    pub requests: i64,
}

#[derive(Debug, Clone)]
pub struct SessionSpendRow {
    pub session_key: String,
    pub cost_usd: f64,
    pub saved_usd: f64,
    pub tokens: i64,
    pub tokens_saved: i64,
    pub requests: i64,
}

#[derive(Debug, Clone)]
pub struct ModelSpendRow {
    pub model: String,
    pub cost_usd: f64,
    pub saved_usd: f64,
    pub tokens: i64,
    pub requests: i64,
}

impl SpendLog {
    /// Detect waste patterns in recent spend data.
    pub fn waste_insights(&self, days: u32) -> Result<Vec<WasteInsight>> {
        let mut insights = Vec::new();

        // 1. Detect sessions with high retry/repeated tool usage
        let mut stmt = self.conn.prepare(
            "SELECT session_key, tools_called, prompt_tokens, completion_tokens, cost_usd
             FROM spend_log
             WHERE timestamp >= datetime('now', ?1) AND tools_called != '[]'
             ORDER BY session_key, timestamp"
        )?;
        let rows = stmt.query_map(
            params![format!("-{} days", days)],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            )),
        )?;

        // Aggregate per session
        let mut session_tools: std::collections::HashMap<String, Vec<(Vec<String>, i64, i64, f64)>> = std::collections::HashMap::new();
        for row in rows {
            if let Ok((session, tools_json, prompt, completion, cost)) = row {
                if let Ok(tools) = serde_json::from_str::<Vec<String>>(&tools_json) {
                    session_tools.entry(session).or_default().push((tools, prompt, completion, cost));
                }
            }
        }

        for (session_key, requests) in &session_tools {
            // Count repeated read operations
            let mut read_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            let mut total_cost = 0.0f64;
            let mut total_prompt_tokens = 0i64;
            let mut bloated_count = 0usize;

            for (tools, prompt, _completion, cost) in requests {
                total_cost += cost;
                total_prompt_tokens += prompt;

                for tool in tools {
                    if tool.contains("Read") || tool.contains("read") || tool == "cat" {
                        *read_counts.entry(tool.clone()).or_insert(0) += 1;
                    }
                }

                // Bloated context: prompt > 100k tokens
                if *prompt > 100_000 {
                    bloated_count += 1;
                }
            }

            // Flag sessions with many repeated reads
            let total_reads: usize = read_counts.values().sum();
            if total_reads > 10 && requests.len() > 5 {
                let read_ratio = total_reads as f64 / requests.len() as f64;
                if read_ratio > 2.0 {
                    insights.push(WasteInsight {
                        kind: WasteKind::RepeatedReads,
                        session_key: session_key.clone(),
                        description: format!(
                            "{} file reads across {} requests (avg {:.1} reads/req). Consider using file caching or reading fewer files.",
                            total_reads, requests.len(), read_ratio
                        ),
                        estimated_waste_usd: total_cost * 0.1, // ~10% waste from re-reads
                    });
                }
            }

            // Flag bloated contexts
            if bloated_count > 3 {
                let avg_prompt = total_prompt_tokens / requests.len() as i64;
                insights.push(WasteInsight {
                    kind: WasteKind::BloatedContext,
                    session_key: session_key.clone(),
                    description: format!(
                        "{} requests with >100k prompt tokens (avg {}k). Consider reducing context or using summarization.",
                        bloated_count, avg_prompt / 1000
                    ),
                    estimated_waste_usd: total_cost * 0.2,
                });
            }
        }

        // 2. Detect expensive models used for simple tasks
        {
            let mut stmt = self.conn.prepare(
                "SELECT model, COUNT(*), AVG(completion_tokens), SUM(cost_usd)
                 FROM spend_log
                 WHERE timestamp >= datetime('now', ?1) AND completion_tokens < 100
                 GROUP BY model
                 HAVING COUNT(*) > 5"
            )?;
            let rows = stmt.query_map(params![format!("-{} days", days)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })?;
            for row in rows {
                if let Ok((model, count, avg_comp, cost)) = row {
                    if model.contains("opus") || model.contains("gpt-4") {
                        insights.push(WasteInsight {
                            kind: WasteKind::ExpensiveModel,
                            session_key: String::new(),
                            description: format!(
                                "{} requests to {} with avg {:.0} output tokens. Consider using a smaller model for short completions.",
                                count, model, avg_comp
                            ),
                            estimated_waste_usd: cost * 0.5,
                        });
                    }
                }
            }
        }

        insights.sort_by(|a, b| b.estimated_waste_usd.partial_cmp(&a.estimated_waste_usd).unwrap_or(std::cmp::Ordering::Equal));
        Ok(insights)
    }

    /// Get total spend for today.
    pub fn today_spend(&self) -> Result<f64> {
        self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend_log
             WHERE timestamp >= date('now')",
            [],
            |row| row.get(0),
        ).map_err(Into::into)
    }

    /// Get total spend for a specific session_key today.
    pub fn session_spend_today(&self, session_key: &str) -> Result<f64> {
        self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend_log
             WHERE session_key = ?1 AND timestamp >= date('now')",
            params![session_key],
            |row| row.get(0),
        ).map_err(Into::into)
    }
}

/// Budget configuration for spend limits.
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Max daily spend in USD (0 = unlimited)
    pub daily_limit_usd: f64,
    /// Max per-session spend in USD (0 = unlimited)
    pub session_limit_usd: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_limit_usd: 0.0,
            session_limit_usd: 0.0,
        }
    }
}

impl BudgetConfig {
    pub fn from_env() -> Self {
        let daily = std::env::var("MERLINT_DAILY_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let session = std::env::var("MERLINT_SESSION_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        Self {
            daily_limit_usd: daily,
            session_limit_usd: session,
        }
    }

    pub fn has_limits(&self) -> bool {
        self.daily_limit_usd > 0.0 || self.session_limit_usd > 0.0
    }
}

/// Check budget limits. Returns Ok(()) if within budget, Err(message) if over.
pub fn check_budget(
    spend_log: &SpendLog,
    budget: &BudgetConfig,
    session_key: &str,
) -> Result<(), String> {
    if budget.daily_limit_usd > 0.0 {
        if let Ok(today) = spend_log.today_spend() {
            if today >= budget.daily_limit_usd {
                return Err(format!(
                    "Daily spend limit reached: ${:.2} / ${:.2}",
                    today, budget.daily_limit_usd
                ));
            }
        }
    }
    if budget.session_limit_usd > 0.0 {
        if let Ok(session) = spend_log.session_spend_today(session_key) {
            if session >= budget.session_limit_usd {
                return Err(format!(
                    "Session spend limit reached: ${:.2} / ${:.2}",
                    session, budget.session_limit_usd
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum WasteKind {
    RepeatedReads,
    BloatedContext,
    ExpensiveModel,
}

#[derive(Debug, Clone)]
pub struct WasteInsight {
    pub kind: WasteKind,
    pub session_key: String,
    pub description: String,
    pub estimated_waste_usd: f64,
}

pub type SharedSpendLog = Arc<Mutex<SpendLog>>;

pub fn new_spend_log() -> Result<SharedSpendLog> {
    Ok(Arc::new(Mutex::new(SpendLog::open()?)))
}
