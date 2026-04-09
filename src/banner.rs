use colored::*;

pub fn print_banner() {
    let ver = env!("CARGO_PKG_VERSION");

    let top    = "  ╔════════════════════════════════════════════════════════════╗";
    let bottom = "  ╚════════════════════════════════════════════════════════════╝";
    let blank  = "  ║                                                            ║";

    println!();
    println!("{}", top.bright_cyan());
    println!("{}", blank.bright_cyan());

    let lines = [
        "      ▄▀█ █▀▀ █▀▀ █▄ █ ▀█▀ █▄▄ █▀▀ █▄ █ █▀▀ █ █      ",
        "      █▀█ █▄█ ██▄ █ ▀█  █  █▄█ ██▄ █ ▀█ █▄▄ █▀█      ",
    ];

    for line in &lines {
        println!("  {} {} {}", "║".bright_cyan(), line.bright_green().bold(), "║".bright_cyan());
    }

    println!("{}", blank.bright_cyan());
    let tag = format!("     Agent Execution Efficiency Analyzer          v{}     ", ver);
    println!("  {} {} {}", "║".bright_cyan(), tag.bright_black(), "║".bright_cyan());
    println!("{}", bottom.bright_cyan());
    println!();

    println!("  {}  {}", ">>".bright_green(), "Real token data. Zero estimation. Actionable insights.".white().bold());
    println!();
    println!("  {}", "COMMANDS".bright_yellow().bold());
    println!("    {}      {}", "scan".bright_cyan(), "Find agent sessions on this machine".dimmed());
    println!("    {}    {}", "latest".bright_cyan(), "Analyze the most recent session".dimmed());
    println!("    {}   {}", "analyze".bright_cyan(), "Deep-dive into a trace file".dimmed());
    println!("    {}     {}", "query".bright_cyan(), "JSON output (for agent integration)".dimmed());
    println!("    {}     {}", "proxy".bright_cyan(), "Intercept LLM API calls in real time".dimmed());
    println!();
    println!("  {} {}", "Run".dimmed(), "agentbench <command> --help".bright_white());
    println!();
}
