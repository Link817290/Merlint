use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8019;
const DEFAULT_TARGET: &str = "https://api.anthropic.com";

pub async fn run(port: Option<u16>, foreground: bool) -> anyhow::Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let target = DEFAULT_TARGET.to_string();
    let trace_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".merlint");
    std::fs::create_dir_all(&trace_dir)?;
    let output = trace_dir.join("traces.json");

    if foreground {
        // Run in foreground (with logs)
        eprintln!("merlint proxy starting on port {} -> {}", port, target);
        eprintln!("Set ANTHROPIC_BASE_URL=http://127.0.0.1:{}", port);
        eprintln!("Press Ctrl+C to stop\n");

        super::proxy::run(port, target, None, output, false, true).await
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
                eprintln!("  Set this in your shell:");
                eprintln!("    export ANTHROPIC_BASE_URL=http://127.0.0.1:{}", port);
                eprintln!();
                eprintln!("  Commands:");
                eprintln!("    merlint dashboard    # live monitoring");
                eprintln!("    merlint down         # stop proxy");
                eprintln!("    merlint latest       # analyze session");
            }
            Err(e) => {
                anyhow::bail!("Failed to start proxy: {}", e);
            }
        }

        Ok(())
    }
}

pub fn run_down(port: Option<u16>) -> anyhow::Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let pid_file = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".merlint")
        .join("proxy.pid");

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
            eprintln!("merlint proxy stopped (PID {})", pid);
        }
    } else {
        // Try connecting to check if it's actually running
        eprintln!("No PID file found. Proxy may not be running on port {}.", port);
    }

    Ok(())
}
