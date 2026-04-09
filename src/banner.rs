use colored::*;

pub fn print_banner() {
    let ver = env!("CARGO_PKG_VERSION");

    let b = "║".bright_cyan();

    println!();
    println!("  {}", "╔════════════════════════════════════════════════════╗".bright_cyan());
    println!("  {}                                                    {}", b, b);
    println!("  {}       {}                                            {}", b, "/\\".bright_magenta().bold(), b);
    println!("  {}      {}                                           {}", b, "/  \\".bright_magenta().bold(), b);
    println!("  {}     {}    {} {}", b, "/____\\".bright_magenta(), "_ __ ___   ___ _ __| (_)_ __| |_".bright_green(), b);
    println!("  {}     {}    {} {}", b, "(O  O)".bright_yellow().bold(), "| '_ ` _ \\/ _ | '__| | | '_ | __|".bright_green(), b);
    println!("  {}      {}     {} {}", b, "<>".bright_yellow(), "| | | | | |  _/|  | | | | | | |_".bright_green(), b);
    println!("  {}     {}    {} {}", b, "/|  |\\".white(), "|_| |_| |_|\\___|_| |_|_|_| |_|\\__|".bright_green(), b);
    println!("  {}    {}                                          {}", b, format!("{}{}",  "*---+".bright_yellow().bold(), "~".bright_magenta().bold()), b);
    println!("  {}                                                    {}", b, b);
    let tag = format!("  Agent Token Optimizer                    v{}", ver);
    println!("  {} {} {}", b, tag.bright_black(), b);
    println!("  {}", "╚════════════════════════════════════════════════════╝".bright_cyan());
    println!();
    println!("  {}  {}", ">>".bright_magenta(), "Diagnose. Optimize. Monitor. Repeat.".white().bold());
    println!();
    println!("  {}", "COMMANDS".bright_yellow().bold());
    println!("    {}        {}", "scan".bright_cyan(), "Find agent sessions on this machine".dimmed());
    println!("    {}      {}", "latest".bright_cyan(), "Analyze the most recent session".dimmed());
    println!("    {}     {}", "analyze".bright_cyan(), "Deep-dive into a trace file".dimmed());
    println!("    {}    {}", "optimize".bright_cyan(), "Generate & apply optimizations".dimmed());
    println!("    {}     {}", "monitor".bright_cyan(), "Watch sessions, auto-optimize".dimmed());
    println!("    {}       {}", "query".bright_cyan(), "JSON output (for agent integration)".dimmed());
    println!("    {}       {}", "proxy".bright_cyan(), "Intercept LLM API calls live".dimmed());
    println!();
    println!("  {} {}", "Run".dimmed(), "merlint <command> --help".bright_white());
    println!();
}
