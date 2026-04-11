//! Native desktop notifications for cache-expiry transitions.
//!
//! The HTML dashboard gained a 5-minute cache countdown earlier, but the
//! countdown only helps a user who's actively looking at the tab. Users
//! who step away for a meeting or coffee still lose the cache without any
//! warning. This module fires a truly OS-level notification — "merlint"
//! as the source app in Notification Center — whenever a tracked session
//! crosses into the final-minute window or expires outright.
//!
//! Implementation: shell out to a platform-specific notification
//! command. No new crates needed, and no dependency on an app bundle /
//! code signature / launcher icon. Failures are best-effort and logged
//! at debug level, never propagated — a broken notification pipe must
//! not disrupt request proxying.
//!
//! Opt-in via the `MERLINT_DESKTOP_NOTIFY` environment variable
//! (default off). Background watcher loop is spawned in `run_proxy`.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Duration;

use super::session_store::{SharedSessionStore, BACKGROUND_SESSION_KEY};

/// The three observable states for a single session's Anthropic prompt
/// cache, from the dashboard / notification loop's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePhase {
    /// More than 60 seconds left on the TTL — cache is healthy.
    Warm,
    /// Between 0 and 60 seconds left — about to expire.
    Expiring,
    /// TTL has elapsed, next request will pay for a full rebuild.
    Cold,
    /// No anchor timestamp at all (preloaded from spend.db, not yet
    /// touched by a live request). We don't know what the real cache
    /// state is, so we stay silent until a real request provides an
    /// anchor.
    Unknown,
}

impl CachePhase {
    fn from_elapsed(elapsed_secs: i64) -> Self {
        if elapsed_secs < 0 {
            CachePhase::Warm // clock skew / future timestamp — treat as fresh
        } else if elapsed_secs < 240 {
            CachePhase::Warm
        } else if elapsed_secs < 300 {
            CachePhase::Expiring
        } else {
            CachePhase::Cold
        }
    }
}

/// Fire-and-forget a desktop notification. Spawns a blocking task so the
/// calling async context never waits on the OS notification command.
pub fn notify(title: String, body: String) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || platform_notify(&title, &body)).await;
        if let Ok(Err(e)) = result {
            tracing::debug!("desktop notification failed: {}", e);
        }
    });
}

#[cfg(target_os = "macos")]
fn platform_notify(title: &str, body: &str) -> std::io::Result<()> {
    // osascript ships with every macOS install and needs no app bundle.
    // The `display notification` verb pops a banner in Notification
    // Center with the invoking process as the source.
    let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(title)
    );
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "osascript exited with {}",
            status
        )))
    }
}

#[cfg(target_os = "linux")]
fn platform_notify(title: &str, body: &str) -> std::io::Result<()> {
    // notify-send is part of libnotify-bin and ships with most desktop
    // distros. Servers without it are expected to opt out of this
    // feature, so fall through as a debug log if the binary is missing.
    let status = std::process::Command::new("notify-send")
        .arg("-a")
        .arg("merlint")
        .arg(title)
        .arg(body)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "notify-send exited with {}",
            status
        )))
    }
}

#[cfg(target_os = "windows")]
fn platform_notify(title: &str, body: &str) -> std::io::Result<()> {
    // PowerShell BalloonTip — available on every Windows install, no
    // BurntToast dependency. Users with stricter PS execution policies
    // may see this fail silently; that's acceptable for a best-effort
    // notification.
    let escape = |s: &str| s.replace('"', "\\\"");
    let script = format!(
        "[reflection.assembly]::loadwithpartialname(\"System.Windows.Forms\") | Out-Null; \
         $n = New-Object System.Windows.Forms.NotifyIcon; \
         $n.Icon = [System.Drawing.SystemIcons]::Information; \
         $n.Visible = $true; \
         $n.ShowBalloonTip(5000, \"{}\", \"{}\", [System.Windows.Forms.ToolTipIcon]::Info);",
        escape(title),
        escape(body)
    );
    let status = std::process::Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(&script)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "powershell exited with {}",
            status
        )))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_notify(_: &str, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::other("platform has no notification command"))
}

/// Is the environment variable opt-in set?
pub fn is_enabled() -> bool {
    matches!(
        std::env::var("MERLINT_DESKTOP_NOTIFY").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Compute a short project display name from an absolute path.
fn short_project_name(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Background loop that polls session state every ~15 seconds and fires
/// a notification whenever a session's cache phase moves from `Warm →
/// Expiring` or from `Warm/Expiring → Cold`. Other transitions
/// (including the back-to-warm case when a user resumes) silently update
/// the phase tracker so the next expiry cycle can fire again.
pub async fn cache_notification_loop(store: SharedSessionStore) {
    let mut phases: HashMap<String, CachePhase> = HashMap::new();

    // 15s cadence — granular enough to catch the 1-minute expiring
    // window once before cold, cheap enough to not matter against a
    // human-scale idle period.
    let mut ticker = tokio::time::interval(Duration::from_secs(15));
    // Skip the immediate first tick so we don't fire on proxy startup
    // before the user has had a chance to do anything.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        let observations: Vec<(String, String, CachePhase)> = {
            let s = store.lock().await;
            s.all_slots()
                .into_iter()
                .filter(|slot| slot.key != "__non_chat__" && slot.key != BACKGROUND_SESSION_KEY)
                .map(|slot| {
                    let anchor: Option<DateTime<Utc>> = slot
                        .last_request_at
                        .or_else(|| slot.session.entries.last().map(|e| e.timestamp));
                    let phase = match anchor {
                        Some(ts) => {
                            let elapsed = (Utc::now() - ts).num_seconds();
                            CachePhase::from_elapsed(elapsed)
                        }
                        None => CachePhase::Unknown,
                    };
                    let project = slot
                        .project_path
                        .map(short_project_name)
                        .unwrap_or_else(|| slot.key.to_string());
                    (slot.key.to_string(), project, phase)
                })
                .collect()
        };

        for (key, project, new_phase) in observations {
            let prev = phases.insert(key.clone(), new_phase);
            let Some(prev) = prev else {
                // First sighting — seed the map without firing so a
                // dashboard that's already cold at proxy start doesn't
                // immediately nag.
                continue;
            };
            if prev == new_phase {
                continue;
            }
            match (prev, new_phase) {
                (CachePhase::Warm, CachePhase::Expiring) => {
                    notify(
                        format!("⏱ {} cache expiring", project),
                        "Less than 1 minute left — send a message to keep it warm"
                            .to_string(),
                    );
                }
                (CachePhase::Warm, CachePhase::Cold)
                | (CachePhase::Expiring, CachePhase::Cold) => {
                    notify(
                        format!("🧊 {} cache expired", project),
                        "Next request will rebuild the cache (~$1.44 on Opus)".to_string(),
                    );
                }
                _ => {
                    // Cold → Warm (resumed) and Expiring → Warm are
                    // silent: the user sent a new message, no news to
                    // tell them. The tracker update re-arms the next
                    // cycle automatically.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_thresholds() {
        assert_eq!(CachePhase::from_elapsed(0), CachePhase::Warm);
        assert_eq!(CachePhase::from_elapsed(239), CachePhase::Warm);
        assert_eq!(CachePhase::from_elapsed(240), CachePhase::Expiring);
        assert_eq!(CachePhase::from_elapsed(299), CachePhase::Expiring);
        assert_eq!(CachePhase::from_elapsed(300), CachePhase::Cold);
        assert_eq!(CachePhase::from_elapsed(10_000), CachePhase::Cold);
        // Clock skew / future timestamp — treat as warm, never fire.
        assert_eq!(CachePhase::from_elapsed(-5), CachePhase::Warm);
    }

    #[test]
    fn short_name_handles_trailing_slash_and_single_segment() {
        assert_eq!(short_project_name("/Applications/github/Merlint"), "Merlint");
        assert_eq!(short_project_name("/Applications/github/Merlint/"), "Merlint");
        assert_eq!(short_project_name("Merlint"), "Merlint");
        assert_eq!(short_project_name(""), "");
    }
}
