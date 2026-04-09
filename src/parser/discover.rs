use std::path::PathBuf;

/// Known agent frameworks and their session log locations
#[derive(Debug, Clone)]
pub struct AgentSource {
    pub name: String,
    pub kind: AgentKind,
    pub session_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

/// Auto-discover installed agent frameworks on this machine
pub fn discover_agents() -> Vec<AgentSource> {
    let mut sources = Vec::new();
    let home = match dirs_home() {
        Some(h) => h,
        None => return sources,
    };

    // ── Claude Code ──
    let claude_dir = home.join(".claude");
    if claude_dir.is_dir() {
        // Find all project session dirs
        let projects_dir = claude_dir.join("projects");
        if projects_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&projects_dir) {
                for entry in entries.flatten() {
                    let sessions_dir = entry.path().join("sessions");
                    if sessions_dir.is_dir() {
                        sources.push(AgentSource {
                            name: format!("claude-code/{}", entry.file_name().to_string_lossy()),
                            kind: AgentKind::ClaudeCode,
                            session_dir: sessions_dir,
                        });
                    }
                }
            }
        }
    }

    // ── Codex CLI ──
    // macOS: ~/Library/Application Support/codex-cli/
    // Linux: ~/.local/share/codex-cli/ or ~/.codex/
    let codex_paths = vec![
        home.join("Library/Application Support/codex-cli"),
        home.join(".local/share/codex-cli"),
        home.join(".codex"),
    ];
    for p in codex_paths {
        if p.is_dir() {
            sources.push(AgentSource {
                name: "codex".into(),
                kind: AgentKind::Codex,
                session_dir: p,
            });
            break;
        }
    }

    sources
}

/// Find the most recent session file in a directory
pub fn find_latest_session(dir: &PathBuf, extension: &str) -> Option<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == extension)
                .unwrap_or(false)
        })
        .collect();

    files.sort_by_key(|e| {
        e.metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });

    files.last().map(|e| e.path())
}

/// List all session files sorted by modification time (newest first)
pub fn list_sessions(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_file()
                && p.extension()
                    .map(|ext| ext == "json" || ext == "jsonl")
                    .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    files.sort_by(|a, b| {
        let ma = a.metadata().ok().and_then(|m| m.modified().ok());
        let mb = b.metadata().ok().and_then(|m| m.modified().ok());
        mb.cmp(&ma) // newest first
    });

    files
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
}
