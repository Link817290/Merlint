use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8019;
const DEFAULT_TARGET: &str = "https://api.anthropic.com";
const SHELL_HOOK: &str = "# merlint: auto-configure proxy\n[ -f \"$HOME/.merlint/env\" ] && . \"$HOME/.merlint/env\"";
const PS_HOOK: &str = "# merlint: auto-configure proxy\r\n$merlintEnv = Join-Path $HOME '.merlint' 'env.ps1'\r\nif (Test-Path $merlintEnv) { . $merlintEnv }";

fn merlint_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".merlint")
}

/// Write the env files so new shells pick up ANTHROPIC_BASE_URL.
fn write_env_file(port: u16) -> anyhow::Result<()> {
    let dir = merlint_dir();
    // POSIX shell (bash/zsh)
    std::fs::write(
        dir.join("env"),
        format!("export ANTHROPIC_BASE_URL=http://127.0.0.1:{}\n", port),
    )?;
    // PowerShell
    std::fs::write(
        dir.join("env.ps1"),
        format!("$env:ANTHROPIC_BASE_URL = 'http://127.0.0.1:{}'\r\n", port),
    )?;
    Ok(())
}

/// Clear the env files so new shells don't route through the proxy.
fn clear_env_file() {
    let dir = merlint_dir();
    let _ = std::fs::write(dir.join("env"), "# merlint proxy not running\n");
    let _ = std::fs::write(dir.join("env.ps1"), "# merlint proxy not running\r\n");
}

/// Check if shell profile already has the merlint hook.
fn has_shell_hook(profile: &PathBuf) -> bool {
    if let Ok(content) = std::fs::read_to_string(profile) {
        content.contains(".merlint")
    } else {
        false
    }
}

/// Get the PowerShell profile path.
fn powershell_profile() -> Option<PathBuf> {
    // Windows: Documents\PowerShell\Microsoft.PowerShell_profile.ps1
    // or Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1
    let home = dirs::home_dir()?;

    // Try pwsh (PowerShell 7+) first, then Windows PowerShell 5
    let candidates = [
        home.join("Documents").join("PowerShell").join("Microsoft.PowerShell_profile.ps1"),
        home.join("Documents").join("WindowsPowerShell").join("Microsoft.PowerShell_profile.ps1"),
    ];

    // Return existing profile, or first path for creation
    for p in &candidates {
        if p.exists() {
            return Some(p.clone());
        }
    }

    // On Windows, return first candidate so we can create it
    if cfg!(windows) {
        Some(candidates[0].clone())
    } else {
        None
    }
}

pub async fn run(port: Option<u16>, foreground: bool) -> anyhow::Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let target = DEFAULT_TARGET.to_string();
    let trace_dir = merlint_dir();
    std::fs::create_dir_all(&trace_dir)?;
    let output = trace_dir.join("traces.json");

    // Write env file for shell integration
    write_env_file(port)?;
    // Auto-install shell hook if not already present
    auto_install_hook();

    if foreground {
        // Run in foreground (with logs)
        eprintln!("merlint proxy starting on port {} -> {}", port, target);
        eprintln!("Press Ctrl+C to stop\n");
        print_current_terminal_hint();

        let result = super::proxy::run(port, target, None, output, false, true).await;
        clear_env_file();
        result
    } else {
        // Daemon mode — fork to background
        use std::process::Command;

        // Check if already running
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()?;
        let url = format!("http://127.0.0.1:{}/merlint/status", port);
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                eprintln!("merlint proxy is already running on port {}", port);
                eprintln!("Use 'merlint dashboard' to view status");
                return Ok(());
            }
        }

        // Find our own binary
        let exe = std::env::current_exe()?;

        let child = Command::new(&exe)
            .args([
                "proxy",
                "--port", &port.to_string(),
                "--target", DEFAULT_TARGET,
                "--optimize",
                "--daemon",
                "--output", output.to_str().unwrap_or("/tmp/merlint-traces.json"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(trace_dir.join("proxy.log"))
                .map(std::process::Stdio::from)
                .unwrap_or(std::process::Stdio::null()))
            .spawn();

        match child {
            Ok(child) => {
                let pid = child.id();
                // Save PID
                let pid_file = trace_dir.join("proxy.pid");
                let _ = std::fs::write(&pid_file, pid.to_string());

                eprintln!("merlint proxy started (PID {}, port {})", pid, port);
                eprintln!();
                print_current_terminal_hint();
                eprintln!();
                eprintln!("  Commands:");
                eprintln!("    merlint dashboard    # live monitoring");
                eprintln!("    merlint down         # stop proxy");
                eprintln!("    merlint latest       # analyze session");
            }
            Err(e) => {
                clear_env_file();
                anyhow::bail!("Failed to start proxy: {}", e);
            }
        }

        Ok(())
    }
}

/// Automatically install shell hooks if not already present.
/// Called by `merlint up` so the user never needs to run `setup-shell` manually.
fn auto_install_hook() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };

    // POSIX shells
    for (profile, name) in [
        (home.join(".zshrc"), "zsh"),
        (home.join(".bashrc"), "bash"),
    ] {
        if !profile.exists() || has_shell_hook(&profile) {
            continue;
        }
        if let Ok(mut content) = std::fs::read_to_string(&profile) {
            content.push_str("\n\n");
            content.push_str(SHELL_HOOK);
            content.push('\n');
            if std::fs::write(&profile, content).is_ok() {
                eprintln!("  Auto-configured {} for merlint proxy", name);
            }
        }
    }

    // PowerShell
    if let Some(ps_profile) = powershell_profile() {
        if !has_shell_hook(&ps_profile) {
            if let Some(parent) = ps_profile.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut content = if ps_profile.exists() {
                std::fs::read_to_string(&ps_profile).unwrap_or_default()
            } else {
                String::new()
            };
            content.push_str("\r\n\r\n");
            content.push_str(PS_HOOK);
            content.push_str("\r\n");
            if std::fs::write(&ps_profile, content).is_ok() {
                eprintln!("  Auto-configured PowerShell for merlint proxy");
            }
        }
    }
}

fn print_current_terminal_hint() {
    eprintln!("  New terminals will auto-route through merlint.");
    if cfg!(windows) {
        eprintln!("  For THIS terminal, run:");
        eprintln!("    . $HOME\\.merlint\\env.ps1");
    } else {
        eprintln!("  For THIS terminal, run:");
        eprintln!("    source ~/.merlint/env");
    }
}

pub fn run_down(port: Option<u16>) -> anyhow::Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let pid_file = merlint_dir().join("proxy.pid");

    if pid_file.exists() {
        let pid_str = std::fs::read_to_string(&pid_file)?;
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            #[cfg(unix)]
            {
                use std::process::Command;
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
            #[cfg(windows)]
            {
                use std::process::Command;
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .status();
            }
            let _ = std::fs::remove_file(&pid_file);
            // Clear env files so new shells go direct
            clear_env_file();
            eprintln!("merlint proxy stopped (PID {})", pid);
            eprintln!("New terminals will connect directly to Anthropic API.");
        }
    } else {
        eprintln!("No PID file found. Proxy may not be running on port {}.", port);
    }

    Ok(())
}

/// Install the shell hook into shell profiles.
pub fn run_setup_shell() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let merlint_dir = home.join(".merlint");
    std::fs::create_dir_all(&merlint_dir)?;

    // Create env files (placeholder if proxy not running)
    let env_file = merlint_dir.join("env");
    if !env_file.exists() {
        std::fs::write(&env_file, "# merlint proxy not running\n")?;
    }
    let env_ps1 = merlint_dir.join("env.ps1");
    if !env_ps1.exists() {
        std::fs::write(&env_ps1, "# merlint proxy not running\r\n")?;
    }

    let mut installed = Vec::new();

    // POSIX shells (bash/zsh)
    let posix_profiles = vec![
        (home.join(".zshrc"), "zsh"),
        (home.join(".bashrc"), "bash"),
    ];

    for (profile, shell_name) in &posix_profiles {
        if !profile.exists() {
            continue;
        }
        if has_shell_hook(profile) {
            eprintln!("  {} already configured", shell_name);
            continue;
        }
        let mut content = std::fs::read_to_string(profile)?;
        content.push_str("\n\n");
        content.push_str(SHELL_HOOK);
        content.push('\n');
        std::fs::write(profile, content)?;
        installed.push(shell_name.to_string());
    }

    // PowerShell
    if let Some(ps_profile) = powershell_profile() {
        if has_shell_hook(&ps_profile) {
            eprintln!("  PowerShell already configured");
        } else {
            // Create parent directory if needed
            if let Some(parent) = ps_profile.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut content = if ps_profile.exists() {
                std::fs::read_to_string(&ps_profile)?
            } else {
                String::new()
            };
            content.push_str("\r\n\r\n");
            content.push_str(PS_HOOK);
            content.push_str("\r\n");
            std::fs::write(&ps_profile, content)?;
            installed.push("PowerShell".to_string());
        }
    }

    if installed.is_empty() {
        eprintln!("Shell hooks already installed (or no shell profile found).");
    } else {
        for name in &installed {
            eprintln!("  Added merlint hook to {}", name);
        }
        eprintln!();
        eprintln!("How it works:");
        eprintln!("  - 'merlint up'   -> new terminals auto-route through proxy");
        eprintln!("  - 'merlint down' -> new terminals connect directly to API");
        eprintln!();
        if cfg!(windows) {
            eprintln!("Restart your terminal or run: . $HOME\\.merlint\\env.ps1");
        } else {
            eprintln!("Restart your terminal or run: source ~/.merlint/env");
        }
    }

    Ok(())
}
