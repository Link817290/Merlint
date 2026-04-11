use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::*,
};

const DEFAULT_PORT: u16 = 8019;

struct DashboardState {
    status: Option<ProxyStatus>,
    error: Option<String>,
    port: u16,
    tick: u64,
    prev_requests: u64,
    sparkline: Vec<u64>,
}

#[derive(Debug, Clone)]
struct ProxyStatus {
    session_count: usize,
    total_requests: u64,
    uptime_secs: i64,
    today_cost_usd: f64,
    today_saved_usd: f64,
    sessions: Vec<SessionInfo>,
    activity: Vec<ActivityItem>,
    events: Vec<EventItem>,
}

#[derive(Debug, Clone)]
struct EventItem {
    time: String,
    kind: String,
    message: String,
}

#[derive(Debug, Clone)]
struct SessionInfo {
    key: String,
    project: String,
    request_count: u64,
    total_tokens: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    tokens_saved: i64,
    tools_tracked: u64,
    total_latency_ms: u64,
    pruning_suspended: bool,
}


#[derive(Debug, Clone)]
struct ActivityItem {
    time: String,
    session: String,
    method: String,
    path: String,
    status: u16,
    latency_ms: u64,
    tokens_saved: Option<i64>,
}

pub async fn run(port: Option<u16>) -> anyhow::Result<()> {
    let port = port.unwrap_or(DEFAULT_PORT);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, port).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    port: u16,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let mut state = DashboardState {
        status: None,
        error: None,
        port,
        tick: 0,
        prev_requests: 0,
        sparkline: vec![0; 30],
    };

    loop {
        match fetch_status(&client, port).await {
            Ok(status) => {
                // Track request delta for sparkline
                let new_reqs = status.total_requests;
                let delta = new_reqs.saturating_sub(state.prev_requests);
                state.prev_requests = new_reqs;
                state.sparkline.push(delta);
                if state.sparkline.len() > 30 {
                    state.sparkline.remove(0);
                }
                state.status = Some(status);
                state.error = None;
            }
            Err(e) => {
                state.sparkline.push(0);
                if state.sparkline.len() > 30 { state.sparkline.remove(0); }
                state.error = Some(e.to_string());
            }
        }
        state.tick += 1;

        terminal.draw(|f| render(f, &state))?;

        if event::poll(Duration::from_secs(1))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        _ => {}
                    }
                }
            }
        }
    }
}

async fn fetch_status(client: &reqwest::Client, port: u16) -> anyhow::Result<ProxyStatus> {
    let url = format!("http://127.0.0.1:{}/merlint/status", port);
    let resp = client.get(&url).send().await?;
    let body: serde_json::Value = resp.json().await?;

    let session_count = body["session_count"].as_u64().unwrap_or(0) as usize;
    let total_requests = body["total_requests"].as_u64().unwrap_or(0);
    let uptime_secs = body["uptime_secs"].as_i64().unwrap_or(0);
    let today_cost_usd = body["today_cost_usd"].as_f64().unwrap_or(0.0);
    let today_saved_usd = body["today_saved_usd"].as_f64().unwrap_or(0.0);

    let sessions = body["sessions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|s| SessionInfo {
                    key: s["key"].as_str().unwrap_or("?").to_string(),
                    project: s["project"].as_str().unwrap_or("unknown").to_string(),
                    request_count: s["request_count"].as_u64().unwrap_or(0),
                    total_tokens: s["total_tokens"].as_u64().unwrap_or(0),
                    prompt_tokens: s["prompt_tokens"].as_u64().unwrap_or(0),
                    completion_tokens: s["completion_tokens"].as_u64().unwrap_or(0),
                    cache_read_tokens: s["cache_read_tokens"].as_u64().unwrap_or(0),
                    tokens_saved: s["tokens_saved"].as_i64().unwrap_or(0),
                    tools_tracked: s["tools_tracked"].as_u64().unwrap_or(0),
                    total_latency_ms: s["total_latency_ms"].as_u64().unwrap_or(0),
                    pruning_suspended: s["pruning_suspended"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();

    let activity = body["activity"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| ActivityItem {
                    time: a["time"].as_str().unwrap_or("").to_string(),
                    session: a["session"].as_str().unwrap_or("").to_string(),
                    method: a["method"].as_str().unwrap_or("").to_string(),
                    path: a["path"].as_str().unwrap_or("").to_string(),
                    status: a["status"].as_u64().unwrap_or(0) as u16,
                    latency_ms: a["latency_ms"].as_u64().unwrap_or(0),
                    tokens_saved: a["tokens_saved"].as_i64(),
                })
                .collect()
        })
        .unwrap_or_default();

    let events = body["events"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|e| EventItem {
                    time: e["time"].as_str().unwrap_or("").to_string(),
                    kind: e["kind"].as_str().unwrap_or("").to_string(),
                    message: e["message"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ProxyStatus {
        session_count,
        total_requests,
        uptime_secs,
        today_cost_usd,
        today_saved_usd,
        sessions,
        activity,
        events,
    })
}

fn render(f: &mut Frame, state: &DashboardState) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Length(1),  // sparkline
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Header
    render_header(f, chunks[0], state);

    // Sparkline bar
    render_sparkline(f, chunks[1], state);

    // Body
    match (&state.status, &state.error) {
        (_, Some(err)) => render_offline(f, chunks[2], state.port, err),
        (Some(status), None) => render_body(f, chunks[2], status),
        (None, None) => {
            let body = Paragraph::new("  Connecting...")
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(body, chunks[2]);
        }
    }

    // Footer with animated dots
    let dots = match state.tick % 4 { 0 => "   ", 1 => ".  ", 2 => ".. ", _ => "..." };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" quit  │  "),
        Span::styled(format!("refreshing{}", dots), Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, chunks[3]);
}

fn render_sparkline(f: &mut Frame, area: Rect, state: &DashboardState) {
    let spark_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max_val = state.sparkline.iter().copied().max().unwrap_or(1).max(1);
    let width = area.width as usize;

    let mut spans = vec![Span::styled("  ", Style::default())];

    // Fit sparkline to available width
    let data = if state.sparkline.len() > width.saturating_sub(4) {
        &state.sparkline[state.sparkline.len() - (width.saturating_sub(4))..]
    } else {
        &state.sparkline
    };

    for (i, &val) in data.iter().enumerate() {
        let level = if val == 0 { 0 } else { ((val as f64 / max_val as f64) * 7.0) as usize };
        let ch = spark_chars[level.min(7)];
        // Color gradient: dim for old, bright for recent
        let age = data.len().saturating_sub(i + 1);
        let color = if val == 0 {
            Color::DarkGray
        } else if age < 3 {
            Color::Green
        } else if age < 10 {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }

    // Label
    spans.push(Span::styled(" reqs/s", Style::default().fg(Color::DarkGray)));

    let sparkline_widget = Paragraph::new(Line::from(spans));
    f.render_widget(sparkline_widget, area);
}

fn render_header(f: &mut Frame, area: Rect, state: &DashboardState) {
    let header = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" merlint dashboard ")
        .title_alignment(Alignment::Center);

    let (status_text, status_color) = if state.error.is_some() {
        ("● OFFLINE", Color::Red)
    } else {
        ("● RUNNING", Color::Green)
    };

    let uptime = state.status.as_ref().map(|s| format_uptime(s.uptime_secs)).unwrap_or_default();
    let total_req = state.status.as_ref().map(|s| s.total_requests).unwrap_or(0);
    let session_count = state.status.as_ref().map(|s| s.session_count).unwrap_or(0);
    let today_cost = state.status.as_ref().map(|s| s.today_cost_usd).unwrap_or(0.0);
    let today_saved = state.status.as_ref().map(|s| s.today_saved_usd).unwrap_or(0.0);

    let mut spans = vec![
        Span::styled("  :", Style::default().fg(Color::White)),
        Span::styled(format!("{}", state.port), Style::default().fg(Color::Yellow)),
        Span::raw("  │  "),
        Span::styled(status_text, Style::default().fg(status_color).bold()),
        Span::raw("  │  "),
        Span::styled(format!("{} projects", session_count), Style::default().fg(Color::White)),
        Span::raw("  │  "),
        Span::styled(format!("{} reqs", total_req), Style::default().fg(Color::White)),
    ];

    if today_cost > 0.0 {
        spans.push(Span::raw("  │  "));
        // Pulse the $ sign on odd ticks when there's activity
        let cost_style = if state.tick % 2 == 0 && state.sparkline.last().copied().unwrap_or(0) > 0 {
            Style::default().fg(Color::White).bold()
        } else {
            Style::default().fg(Color::Yellow)
        };
        spans.push(Span::styled(format!("${:.2}", today_cost), cost_style));
        if today_saved > 0.01 {
            spans.push(Span::styled(format!(" (-${:.2})", today_saved), Style::default().fg(Color::Green)));
        }
    }

    spans.push(Span::raw("  │  "));
    spans.push(Span::styled(uptime, Style::default().fg(Color::DarkGray)));

    let header_line = Line::from(spans);

    let header_widget = Paragraph::new(header_line)
        .block(header)
        .alignment(Alignment::Center);
    f.render_widget(header_widget, area);
}

fn render_offline(f: &mut Frame, area: Rect, port: u16, err: &str) {
    let msg = if err.contains("Connection refused") || err.contains("connect") {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  merlint proxy is not running",
                Style::default().fg(Color::Red).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Troubleshooting:",
                Style::default().fg(Color::White).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  1. Start the proxy:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "     merlint up",
                Style::default().fg(Color::Cyan).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  2. Set the env var so Claude Code routes through it:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                format!("     export ANTHROPIC_BASE_URL=http://127.0.0.1:{}", port),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  3. Then restart Claude Code in that terminal.",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Error: {}", err),
                Style::default().fg(Color::Red),
            )),
        ]
    };
    let body = Paragraph::new(msg).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(body, area);
}

fn render_body(f: &mut Frame, area: Rect, status: &ProxyStatus) {
    let has_activity = !status.activity.is_empty();
    let has_events = !status.events.is_empty();
    let has_sessions = !status.sessions.is_empty();

    // Layout: sessions | activity + events side by side
    let constraints = if has_sessions && (has_activity || has_events) {
        vec![Constraint::Percentage(45), Constraint::Percentage(55)]
    } else if has_activity || has_events {
        vec![Constraint::Length(4), Constraint::Min(0)]
    } else {
        vec![Constraint::Min(0)]
    };

    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Sessions area
    if !has_sessions {
        let msg = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Proxy is running. {} total requests received.", status.total_requests),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  No chat sessions yet — waiting for API requests...",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let body = Paragraph::new(msg).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Projects ")
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(body, body_chunks[0]);
    } else {
        render_sessions(f, body_chunks[0], status);
    }

    // Bottom area: activity log + events side by side
    if body_chunks.len() > 1 {
        if has_activity && has_events {
            let bottom = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(body_chunks[1]);
            render_activity_log(f, bottom[0], status);
            render_event_log(f, bottom[1], status);
        } else if has_activity {
            render_activity_log(f, body_chunks[1], status);
        } else if has_events {
            render_event_log(f, body_chunks[1], status);
        }
    }
}

fn render_sessions(f: &mut Frame, area: Rect, status: &ProxyStatus) {
    // Filter out empty sessions, group by project
    let active: Vec<&SessionInfo> = status.sessions.iter()
        .filter(|s| s.request_count > 0)
        .collect();

    if active.is_empty() {
        let msg = Paragraph::new("  No active projects yet...")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    // Group by project path
    let mut groups: Vec<(String, Vec<&SessionInfo>)> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in &active {
        let proj = if s.project != "unknown" { &s.project } else { &s.key };
        if let Some(&idx) = seen.get(proj) {
            groups[idx].1.push(s);
        } else {
            seen.insert(proj.clone(), groups.len());
            groups.push((proj.clone(), vec![s]));
        }
    }

    let max_cards = (area.height as usize / 5).max(1);
    let count = groups.len().min(max_cards);

    let constraints: Vec<Constraint> = (0..count)
        .map(|_| Constraint::Min(5))
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, (proj_path, sessions)) in groups.iter().take(count).enumerate() {
        render_project_card(f, chunks[i], proj_path, sessions);
    }
}

fn render_project_card(f: &mut Frame, area: Rect, proj_path: &str, sessions: &[&SessionInfo]) {
    // Extract short project name from path
    let proj_name = proj_path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(proj_path);
    let title_display = if proj_name.len() > 24 {
        format!("{}...", &proj_name[..21])
    } else {
        proj_name.to_string()
    };
    let conv_label = if sessions.len() > 1 {
        format!(" ({} convs)", sessions.len())
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(format!(" {}{} ", title_display, conv_label))
        .title_style(Style::default().fg(Color::Cyan).bold());

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Aggregate across all sessions in this project
    let total_reqs: u64 = sessions.iter().map(|s| s.request_count).sum();
    let total_tokens: u64 = sessions.iter().map(|s| s.total_tokens).sum();
    let total_prompt: u64 = sessions.iter().map(|s| s.prompt_tokens).sum();
    let total_completion: u64 = sessions.iter().map(|s| s.completion_tokens).sum();
    let total_cache_read: u64 = sessions.iter().map(|s| s.cache_read_tokens).sum();
    let total_saved: i64 = sessions.iter().map(|s| s.tokens_saved).sum();
    let total_tools: u64 = sessions.iter().map(|s| s.tools_tracked).max().unwrap_or(0);
    let total_latency: u64 = sessions.iter().map(|s| s.total_latency_ms).sum();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    let avg_latency = if total_reqs > 0 { total_latency / total_reqs } else { 0 };

    let stats = vec![
        Line::from(vec![
            Span::styled("  Reqs:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", total_reqs),
                Style::default().fg(Color::White).bold(),
            ),
            Span::styled(
                format!("    avg {}ms", avg_latency),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Tokens: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_tokens(total_tokens),
                Style::default().fg(Color::White).bold(),
            ),
            Span::styled(
                format!(
                    "  ({}p / {}c)",
                    format_tokens(total_prompt),
                    format_tokens(total_completion)
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Pruned: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("~{}", format_tokens(total_saved.max(0) as u64)),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                format!("    {} tools", total_tools),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    let stats_widget = Paragraph::new(stats);
    f.render_widget(stats_widget, cols[0]);

    // Right: API cache gauge (Anthropic prompt caching, NOT merlint pruning)
    let api_cache_pct = if total_prompt > 0 {
        (total_cache_read as f64 / total_prompt as f64 * 100.0) as u16
    } else {
        0
    };

    let cache_color = if api_cache_pct >= 60 { Color::Green } else if api_cache_pct >= 30 { Color::Yellow } else { Color::Red };

    let any_paused = sessions.iter().any(|s| s.pruning_suspended);
    let prune_status = if any_paused {
        Span::styled(" [paused]", Style::default().fg(Color::Yellow))
    } else {
        Span::styled("", Style::default())
    };

    let right_lines = vec![
        Line::from(vec![
            Span::styled("  API$: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}%", api_cache_pct), Style::default().fg(cache_color).bold()),
            Span::styled(format!(" ({})", format_tokens(total_cache_read)), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(Span::styled(
            format!("  {}", make_bar(api_cache_pct, 20)),
            Style::default().fg(cache_color),
        )),
        Line::from(vec![
            Span::styled("  Prune: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} tools", total_tools), Style::default().fg(Color::White)),
            prune_status,
        ]),
    ];
    let right_widget = Paragraph::new(right_lines);
    f.render_widget(right_widget, cols[1]);
}

fn render_activity_log(f: &mut Frame, area: Rect, status: &ProxyStatus) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Recent Activity ")
        .title_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let max_lines = inner.height as usize;
    let items: Vec<Line> = status.activity.iter().take(max_lines).map(|a| {
        let status_color = if a.status < 300 { Color::Green } else { Color::Red };
        let saved_text = a.tokens_saved
            .map(|s| format!(" saved ~{}", format_tokens(s.max(0) as u64)))
            .unwrap_or_default();

        // Shorten the path for display
        let path_short = if a.path.len() > 30 {
            format!("...{}", &a.path[a.path.len()-27..])
        } else {
            a.path.clone()
        };

        let session_short = if a.session.len() > 12 {
            format!("{}…", &a.session[..11])
        } else {
            a.session.clone()
        };

        Line::from(vec![
            Span::styled(format!("  {} ", a.time), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:3} ", a.status), Style::default().fg(status_color)),
            Span::styled(format!("{:>4}ms ", a.latency_ms), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[{}] ", session_short), Style::default().fg(Color::Blue)),
            Span::styled(format!("{} {}", a.method, path_short), Style::default().fg(Color::White)),
            Span::styled(saved_text, Style::default().fg(Color::Green)),
        ])
    }).collect();

    let log_widget = Paragraph::new(items);
    f.render_widget(log_widget, inner);
}

fn render_event_log(f: &mut Frame, area: Rect, status: &ProxyStatus) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Events ")
        .title_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let max_lines = inner.height as usize;
    let items: Vec<Line> = status.events.iter().take(max_lines).map(|e| {
        let (icon, color) = match e.kind.as_str() {
            "session" => ("+", Color::Green),
            "optimize" => ("~", Color::Cyan),
            _ => ("i", Color::DarkGray),
        };

        Line::from(vec![
            Span::styled(format!("  {} ", e.time), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("[{}] ", icon), Style::default().fg(color).bold()),
            Span::styled(&e.message, Style::default().fg(Color::White)),
        ])
    }).collect();

    let log_widget = Paragraph::new(items);
    f.render_widget(log_widget, inner);
}

use merlint::util::format::format_tokens_u64 as format_tokens;

fn format_uptime(secs: i64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn make_bar(pct: u16, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    let empty = width - filled;
    format!(
        "[{}{}]",
        "█".repeat(filled),
        "░".repeat(empty)
    )
}

