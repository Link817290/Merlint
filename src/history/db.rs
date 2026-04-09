use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::analyzer::cache::CacheAnalysis;
use crate::analyzer::efficiency::EfficiencyAnalysis;
use crate::analyzer::token::SessionTokenSummary;

/// Persistent history database stored in ~/.merlint/history.db
pub struct HistoryDb {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: i64,
    pub session_id: String,
    pub source_file: String,
    pub agent_kind: String,
    pub analyzed_at: String,
    // Token stats
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub num_calls: i64,
    // Efficiency stats
    pub tool_call_count: i64,
    pub retry_count: i64,
    pub loop_pattern_count: i64,
    pub redundant_read_count: i64,
    pub tokens_per_call_avg: f64,
    // Cache stats
    pub prefix_stability: f64,
    pub cache_hit_ratio: f64,
    pub cache_issues: i64,
    // Tool stats
    pub tools_defined: i64,
    pub tools_used: i64,
    pub tools_unused: i64,
}

impl HistoryDb {
    pub fn open() -> Result<Self> {
        let db_path = Self::db_path()?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".merlint").join("history.db"))
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id          TEXT NOT NULL,
                source_file         TEXT NOT NULL,
                agent_kind          TEXT NOT NULL,
                analyzed_at         TEXT NOT NULL DEFAULT (datetime('now')),
                -- token stats
                total_tokens        INTEGER NOT NULL DEFAULT 0,
                prompt_tokens       INTEGER NOT NULL DEFAULT 0,
                completion_tokens   INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                num_calls           INTEGER NOT NULL DEFAULT 0,
                -- efficiency stats
                tool_call_count     INTEGER NOT NULL DEFAULT 0,
                retry_count         INTEGER NOT NULL DEFAULT 0,
                loop_pattern_count  INTEGER NOT NULL DEFAULT 0,
                redundant_read_count INTEGER NOT NULL DEFAULT 0,
                tokens_per_call_avg REAL NOT NULL DEFAULT 0.0,
                -- cache stats
                prefix_stability    REAL NOT NULL DEFAULT 0.0,
                cache_hit_ratio     REAL NOT NULL DEFAULT 0.0,
                cache_issues        INTEGER NOT NULL DEFAULT 0,
                -- tool stats
                tools_defined       INTEGER NOT NULL DEFAULT 0,
                tools_used          INTEGER NOT NULL DEFAULT 0,
                tools_unused        INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_analyzed_at ON sessions(analyzed_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_agent_kind ON sessions(agent_kind);

            CREATE TABLE IF NOT EXISTS tool_usage (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id      TEXT NOT NULL,
                tool_name       TEXT NOT NULL,
                call_count      INTEGER NOT NULL DEFAULT 0,
                recorded_at     TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_tool_usage_tool_name ON tool_usage(tool_name);
            CREATE INDEX IF NOT EXISTS idx_tool_usage_session ON tool_usage(session_id);",
        )?;
        Ok(())
    }

    /// Store analysis results for a session
    pub fn store_session(
        &self,
        session_id: &str,
        source_file: &str,
        agent_kind: &str,
        ts: &SessionTokenSummary,
        ea: &EfficiencyAnalysis,
        ca: &CacheAnalysis,
    ) -> Result<i64> {
        let cache_hit = ca.actual_cache_hit_ratio
            .unwrap_or(ca.theoretical_cache_hit_ratio);

        self.conn.execute(
            "INSERT INTO sessions (
                session_id, source_file, agent_kind,
                total_tokens, prompt_tokens, completion_tokens,
                cache_read_tokens, cache_creation_tokens, num_calls,
                tool_call_count, retry_count, loop_pattern_count,
                redundant_read_count, tokens_per_call_avg,
                prefix_stability, cache_hit_ratio, cache_issues,
                tools_defined, tools_used, tools_unused
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                session_id,
                source_file,
                agent_kind,
                ts.total_tokens as i64,
                ts.total_prompt_tokens as i64,
                ts.total_completion_tokens as i64,
                ts.total_cache_read_tokens as i64,
                ts.total_cache_creation_tokens as i64,
                ts.num_calls as i64,
                ea.tool_call_count as i64,
                ea.retry_count as i64,
                ea.loop_patterns.len() as i64,
                ea.redundant_reads.len() as i64,
                ea.tokens_per_call_avg,
                ca.prefix_stability_ratio,
                cache_hit,
                ca.issues.len() as i64,
                ts.tools_defined as i64,
                ts.tools_used as i64,
                ts.tool_names_unused.len() as i64,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get all sessions, most recent first
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sessions ORDER BY analyzed_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                source_file: row.get(2)?,
                agent_kind: row.get(3)?,
                analyzed_at: row.get(4)?,
                total_tokens: row.get(5)?,
                prompt_tokens: row.get(6)?,
                completion_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_creation_tokens: row.get(9)?,
                num_calls: row.get(10)?,
                tool_call_count: row.get(11)?,
                retry_count: row.get(12)?,
                loop_pattern_count: row.get(13)?,
                redundant_read_count: row.get(14)?,
                tokens_per_call_avg: row.get(15)?,
                prefix_stability: row.get(16)?,
                cache_hit_ratio: row.get(17)?,
                cache_issues: row.get(18)?,
                tools_defined: row.get(19)?,
                tools_used: row.get(20)?,
                tools_unused: row.get(21)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get sessions within a date range
    pub fn sessions_between(&self, from: &str, to: &str) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sessions WHERE analyzed_at BETWEEN ?1 AND ?2 ORDER BY analyzed_at DESC",
        )?;
        let rows = stmt.query_map(params![from, to], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                source_file: row.get(2)?,
                agent_kind: row.get(3)?,
                analyzed_at: row.get(4)?,
                total_tokens: row.get(5)?,
                prompt_tokens: row.get(6)?,
                completion_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_creation_tokens: row.get(9)?,
                num_calls: row.get(10)?,
                tool_call_count: row.get(11)?,
                retry_count: row.get(12)?,
                loop_pattern_count: row.get(13)?,
                redundant_read_count: row.get(14)?,
                tokens_per_call_avg: row.get(15)?,
                prefix_stability: row.get(16)?,
                cache_hit_ratio: row.get(17)?,
                cache_issues: row.get(18)?,
                tools_defined: row.get(19)?,
                tools_used: row.get(20)?,
                tools_unused: row.get(21)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get aggregate stats
    pub fn aggregate_stats(&self) -> Result<AggregateStats> {
        let row = self.conn.query_row(
            "SELECT
                COUNT(*) as session_count,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                COALESCE(AVG(total_tokens), 0) as avg_tokens_per_session,
                COALESCE(AVG(tokens_per_call_avg), 0) as avg_tokens_per_call,
                COALESCE(AVG(cache_hit_ratio), 0) as avg_cache_hit,
                COALESCE(AVG(prefix_stability), 0) as avg_prefix_stability,
                COALESCE(SUM(retry_count), 0) as total_retries,
                COALESCE(SUM(loop_pattern_count), 0) as total_loops,
                COALESCE(SUM(redundant_read_count), 0) as total_redundant_reads,
                COALESCE(AVG(tools_unused), 0) as avg_unused_tools
            FROM sessions",
            [],
            |row| {
                Ok(AggregateStats {
                    session_count: row.get(0)?,
                    total_tokens: row.get(1)?,
                    avg_tokens_per_session: row.get(2)?,
                    avg_tokens_per_call: row.get(3)?,
                    avg_cache_hit: row.get(4)?,
                    avg_prefix_stability: row.get(5)?,
                    total_retries: row.get(6)?,
                    total_loops: row.get(7)?,
                    total_redundant_reads: row.get(8)?,
                    avg_unused_tools: row.get(9)?,
                })
            },
        )?;
        Ok(row)
    }

    /// Store per-tool usage data for a session
    pub fn store_tool_usage(
        &self,
        session_id: &str,
        tool_counts: &[(String, usize)],
    ) -> Result<()> {
        for (tool_name, count) in tool_counts {
            self.conn.execute(
                "INSERT INTO tool_usage (session_id, tool_name, call_count) VALUES (?1, ?2, ?3)",
                params![session_id, tool_name, *count as i64],
            )?;
        }
        Ok(())
    }

    /// Get tool frequency across all sessions (tool_name -> total_calls)
    pub fn tool_frequency(&self) -> Result<Vec<ToolFrequency>> {
        let mut stmt = self.conn.prepare(
            "SELECT tool_name, SUM(call_count) as total_calls, COUNT(DISTINCT session_id) as session_count
             FROM tool_usage
             GROUP BY tool_name
             ORDER BY total_calls DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ToolFrequency {
                tool_name: row.get(0)?,
                total_calls: row.get(1)?,
                session_count: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get tools that have never been used across all recorded sessions
    pub fn never_used_tools(&self, all_defined: &[String]) -> Result<Vec<String>> {
        let used: std::collections::HashSet<String> = self.tool_frequency()?
            .into_iter()
            .map(|f| f.tool_name)
            .collect();
        Ok(all_defined.iter()
            .filter(|t| !used.contains(t.as_str()))
            .cloned()
            .collect())
    }

    /// Get tools used in fewer than `threshold` fraction of sessions
    pub fn low_frequency_tools(&self, threshold: f64) -> Result<Vec<ToolFrequency>> {
        let total_sessions = self.session_count()?;
        if total_sessions == 0 {
            return Ok(vec![]);
        }
        let freq = self.tool_frequency()?;
        Ok(freq.into_iter()
            .filter(|f| (f.session_count as f64 / total_sessions as f64) < threshold)
            .collect())
    }

    /// Count total sessions stored
    pub fn session_count(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }
}

#[derive(Debug, Clone)]
pub struct ToolFrequency {
    pub tool_name: String,
    pub total_calls: i64,
    pub session_count: i64,
}

#[derive(Debug)]
pub struct AggregateStats {
    pub session_count: i64,
    pub total_tokens: i64,
    pub avg_tokens_per_session: f64,
    pub avg_tokens_per_call: f64,
    pub avg_cache_hit: f64,
    pub avg_prefix_stability: f64,
    pub total_retries: i64,
    pub total_loops: i64,
    pub total_redundant_reads: i64,
    pub avg_unused_tools: f64,
}
