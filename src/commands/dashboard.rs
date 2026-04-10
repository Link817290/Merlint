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
    sessions: Vec<SessionInfo>,
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
    last_activity: Option<String>,
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
        // Poll status
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

        // Poll for key events (non-blocking, 1s timeout)
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
                    last_activity: s["last_activity"].as_str().map(|s| s.to_string()),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ProxyStatus {
        session_count,
        sessions,
    })
}

fn render(f: &mut Frame, state: &DashboardState) {
    let area = f.area();

    // Main layout: header + body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Header
    let header = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" merlint dashboard ")
        .title_alignment(Alignment::Center);

    let status_text = if state.error.is_some() {
        "● OFFLINE"
    } else {
        "● RUNNING"
    };
    let status_color = if state.error.is_some() {
        Color::Red
    } else {
        Color::Green
    };

    let header_line = Line::from(vec![
        Span::styled("  proxy :", Style::default().fg(Color::White)),
        Span::styled(
            format!("{}", state.port),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  │  "),
        Span::styled(status_text, Style::default().fg(status_color).bold()),
        Span::raw("  │  "),
        Span::styled(
            format!(
                "{} sessions",
                state
                    .status
                    .as_ref()
                    .map(|s| s.session_count)
                    .unwrap_or(0)
            ),
            Style::default().fg(Color::White),
        ),
    ]);

    let header_widget = Paragraph::new(header_line)
        .block(header)
        .alignment(Alignment::Center);
    f.render_widget(header_widget, chunks[0]);

    // Body
    match (&state.status, &state.error) {
        (_, Some(err)) => {
            let msg = if err.contains("Connection refused") || err.contains("connect") {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  merlint proxy is not running",
                        Style::default().fg(Color::Red).bold(),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Start it with:",
                        Style::default().fg(Color::White),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "    merlint up",
                        Style::default().fg(Color::Cyan).bold(),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("  (port {})", state.port),
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
            f.render_widget(body, chunks[1]);
        }
        (Some(status), None) => {
            render_sessions(f, chunks[1], status);
        }
        (None, None) => {
            let body = Paragraph::new("  Connecting...")
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(body, chunks[1]);
        }
    }

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().fg(Color::Yellow).bold()),
        Span::raw(" quit  "),
        Span::styled("refreshes every 1s", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, chunks[2]);
}

fn render_sessions(f: &mut Frame, area: Rect, status: &ProxyStatus) {
    if status.sessions.is_empty() {
        let msg = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No active sessions yet",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Waiting for API requests...",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let body = Paragraph::new(msg).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(body, area);
        return;
    }

    // Split area for each session (up to 6)
    let count = status.sessions.len().min(6);
    let constraints: Vec<Constraint> = (0..count)
        .map(|_| Constraint::Min(7))
        .collect();

    let session_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, session) in status.sessions.iter().take(6).enumerate() {
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

    // Layout: stats on left, gauges on right
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    // Left: stats
    let avg_latency = if session.request_count > 0 {
        session.total_latency_ms / session.request_count
    } else {
        0
    };

    let stats = vec![
        Line::from(vec![
            Span::styled("  Requests: ", Style::default().fg(Color::DarkGray)),
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
            Span::styled("  Tokens:   ", Style::default().fg(Color::DarkGray)),
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
            Span::styled("  Saved:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("~{}", format_tokens(session.tokens_saved.max(0) as u64)),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                format!("    {} tools", session.tools_tracked),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Last:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                session.last_activity.as_deref()
                    .map(|s| s.chars().take(19).collect::<String>())
                    .unwrap_or_else(|| "—".to_string()),
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

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
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
