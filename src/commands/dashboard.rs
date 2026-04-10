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
}

#[derive(Debug, Clone)]
struct ProxyStatus {
    session_count: usize,
    total_requests: u64,
    uptime_secs: i64,
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
    request_count: u64,
    total_tokens: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    tokens_saved: i64,
    tools_tracked: u64,
    total_latency_ms: u64,
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
    };

    loop {
        match fetch_status(&client, port).await {
            Ok(status) => {
                state.status = Some(status);
                state.error = None;
            }
            Err(e) => {
                state.error = Some(e.to_string());
            }
        }

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

    let sessions = body["sessions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|s| SessionInfo {
                    key: s["key"].as_str().unwrap_or("?").to_string(),
                    request_count: s["request_count"].as_u64().unwrap_or(0),
                    total_tokens: s["total_tokens"].as_u64().unwrap_or(0),
                    prompt_tokens: s["prompt_tokens"].as_u64().unwrap_or(0),
                    completion_tokens: s["completion_tokens"].as_u64().unwrap_or(0),
                    cache_read_tokens: s["cache_read_tokens"].as_u64().unwrap_or(0),
                    tokens_saved: s["tokens_saved"].as_i64().unwrap_or(0),
                    tools_tracked: s["tools_tracked"].as_u64().unwrap_or(0),
                    total_latency_ms: s["total_latency_ms"].as_u64().unwrap_or(0),
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
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Header
    render_header(f, chunks[0], state);

    // Body
    match (&state.status, &state.error) {
        (_, Some(err)) => render_offline(f, chunks[1], state.port, err),
        (Some(status), None) => render_body(f, chunks[1], status),
        (None, None) => {
            let body = Paragraph::new("  Connecting...")
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(body, chunks[1]);
        }
    }

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" quit  │  "),
        Span::styled("refreshes every 1s", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, chunks[2]);
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

    let header_line = Line::from(vec![
        Span::styled("  :", Style::default().fg(Color::White)),
        Span::styled(format!("{}", state.port), Style::default().fg(Color::Yellow)),
        Span::raw("  │  "),
        Span::styled(status_text, Style::default().fg(status_color).bold()),
        Span::raw("  │  "),
        Span::styled(format!("{} sessions", session_count), Style::default().fg(Color::White)),
        Span::raw("  │  "),
        Span::styled(format!("{} reqs", total_req), Style::default().fg(Color::White)),
        Span::raw("  │  "),
        Span::styled(uptime, Style::default().fg(Color::DarkGray)),
    ]);

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
                .title(" Sessions ")
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
    let count = status.sessions.len().min(4);
    let constraints: Vec<Constraint> = (0..count)
        .map(|_| Constraint::Min(6))
        .collect();

    let session_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, session) in status.sessions.iter().take(4).enumerate() {
        render_session_card(f, session_chunks[i], session);
    }
}

fn render_session_card(f: &mut Frame, area: Rect, session: &SessionInfo) {
    let key_display = if session.key.len() > 20 {
        format!("{}...", &session.key[..20])
    } else {
        session.key.clone()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(format!(" {} ", key_display))
        .title_style(Style::default().fg(Color::Cyan).bold());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    let avg_latency = if session.request_count > 0 {
        session.total_latency_ms / session.request_count
    } else {
        0
    };

    let stats = vec![
        Line::from(vec![
            Span::styled("  Reqs:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", session.request_count),
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
                format_tokens(session.total_tokens),
                Style::default().fg(Color::White).bold(),
            ),
            Span::styled(
                format!(
                    "  ({}p / {}c)",
                    format_tokens(session.prompt_tokens),
                    format_tokens(session.completion_tokens)
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Saved:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("~{}", format_tokens(session.tokens_saved.max(0) as u64)),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                format!("    {} tools", session.tools_tracked),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];
    let stats_widget = Paragraph::new(stats);
    f.render_widget(stats_widget, cols[0]);

    // Right: cache gauge
    let cache_pct = if session.prompt_tokens > 0 {
        (session.cache_read_tokens as f64 / session.prompt_tokens as f64 * 100.0) as u16
    } else {
        0
    };

    let right_lines = vec![
        Line::from(vec![
            Span::styled("  Cache: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}%", cache_pct),
                Style::default()
                    .fg(if cache_pct >= 60 {
                        Color::Green
                    } else if cache_pct >= 30 {
                        Color::Yellow
                    } else {
                        Color::Red
                    })
                    .bold(),
            ),
        ]),
        Line::from(Span::styled(
            format!("  {}", make_bar(cache_pct, 20)),
            Style::default().fg(if cache_pct >= 60 {
                Color::Green
            } else if cache_pct >= 30 {
                Color::Yellow
            } else {
                Color::Red
            }),
        )),
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

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

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
