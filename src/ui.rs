use crate::app::App;
use crate::power::{ProcessDetail, WakeEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Min(6),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .split(area);

    draw_battery(f, chunks[0], app);
    draw_wake_stats(f, chunks[1], app);
    draw_processes(f, chunks[2], app);
    draw_settings(f, chunks[3], app);
    draw_footer(f, chunks[4], app);

    if app.show_detail {
        if let Some(detail) = &app.detail {
            draw_detail_popup(f, area, detail);
        }
    }

    if app.show_help {
        draw_help_popup(f, area);
    }
}

fn draw_battery(f: &mut Frame, area: Rect, app: &App) {
    let (pct, label, color) = match &app.battery {
        Some(b) => {
            let state = if b.on_ac { "AC" } else { "Battery" };
            let charging = if b.charging { "charging" } else { "discharging" };
            let time = b
                .time_remaining
                .clone()
                .map(|t| format!(", {t} remaining"))
                .unwrap_or_default();
            let rate = match app.instant_power {
                Some(p) => format!(", {:+.1}W ({:+.1}%/hr)", p.watts, p.percent_per_hour),
                None => String::new(),
            };
            let label = format!("{}% ({state}, {charging}{time}{rate})", b.percentage);
            let color = if !b.on_ac && b.percentage <= 20 {
                Color::Red
            } else if b.percentage <= 50 {
                Color::Yellow
            } else {
                Color::Green
            };
            (b.percentage as u16, label, color)
        }
        None => (0, "waiting for battery data...".to_string(), Color::Gray),
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Battery"))
        .gauge_style(Style::default().fg(color))
        .percent(pct)
        .label(label);
    f.render_widget(gauge, area);
}

fn draw_wake_stats(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);

    let wake_text = match &app.wake {
        Some(w) => vec![
            Line::from(format!("Sleep count:  {}", w.sleep_count)),
            Line::from(format!("Dark wakes:   {}", w.dark_wake_count)),
            Line::from(format!("User wakes:   {}", w.user_wake_count)),
            Line::from(""),
            Line::from(Span::styled(
                "(lifetime, since last boot)",
                Style::default().fg(Color::DarkGray),
            )),
        ],
        None => vec![Line::from("waiting for wake data...")],
    };
    let wake_panel = Paragraph::new(wake_text)
        .block(Block::default().borders(Borders::ALL).title("Wake Stats"));
    f.render_widget(wake_panel, cols[0]);

    let feed_lines: Vec<Line> = if app.wake_feed.is_empty() {
        vec![Line::from(Span::styled(
            "watching for new sleep/wake events...",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.wake_feed
            .iter()
            .rev()
            .map(|e| {
                let time = e.timestamp.split(' ').nth(1).unwrap_or(&e.timestamp);
                let (kind_text, color) = match e.kind {
                    WakeEventKind::Sleep => ("Sleep   ", Color::Blue),
                    WakeEventKind::DarkWake => ("DarkWake", Color::Yellow),
                    WakeEventKind::Wake => ("Wake    ", Color::Green),
                };
                Line::from(vec![
                    Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{kind_text} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(e.reason.clone()),
                ])
            })
            .collect()
    };
    let feed_panel = Paragraph::new(feed_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Live Sleep/Wake Feed (newest first)"),
    );
    f.render_widget(feed_panel, cols[1]);
}

fn draw_processes(f: &mut Frame, area: Rect, app: &App) {
    let selected = app.selected_index();
    let rows: Vec<Row> = app
        .processes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut style = if p.power >= 10.0 {
                Style::default().fg(Color::Red)
            } else if p.power > 0.0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            if i == selected {
                style = style.bg(Color::DarkGray).add_modifier(Modifier::BOLD);
            }
            Row::new(vec![
                Cell::from(p.pid.to_string()),
                Cell::from(p.command.clone()),
                Cell::from(format!("{:.1}", p.power)),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(20),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["PID", "PROCESS", "POWER"]).style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Top Power-Consuming Processes"),
    );
    f.render_widget(table, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn process_display_name(detail: &ProcessDetail) -> String {
    detail.bundle_name.clone().unwrap_or_else(|| {
        detail
            .full_command
            .rsplit('/')
            .next()
            .unwrap_or(&detail.full_command)
            .to_string()
    })
}

fn draw_detail_popup(f: &mut Frame, area: Rect, detail: &ProcessDetail) {
    let popup_area = centered_rect(70, 60, area);
    f.render_widget(Clear, popup_area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Path: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(detail.full_command.clone()),
        ]),
        Line::from(format!(
            "PID: {}   PPID: {}{}",
            detail.pid,
            detail.ppid,
            detail
                .parent_command
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default()
        )),
        Line::from(format!("User: {}", detail.user)),
        Line::from(format!(
            "CPU: {:.1}%   Memory: {:.1}%   Running for: {}",
            detail.cpu_percent, detail.mem_percent, detail.elapsed
        )),
    ];

    if let Some(label) = &detail.launchd_label {
        lines.push(Line::from(format!("Launchd label: {label}")));
    }

    lines.push(Line::from(""));
    if detail.bundle_name.is_some() || detail.bundle_id.is_some() {
        lines.push(Line::from(Span::styled(
            "App bundle",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        if let Some(name) = &detail.bundle_name {
            lines.push(Line::from(format!("Name: {name}")));
        }
        if let Some(id) = &detail.bundle_id {
            lines.push(Line::from(format!("Identifier: {id}")));
        }
        if let Some(version) = &detail.bundle_version {
            lines.push(Line::from(format!("Version: {version}")));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "Not an app bundle (system/background process)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[Esc/Enter/i] close",
        Style::default().fg(Color::DarkGray),
    )));

    let popup = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Process Detail: {} ", process_display_name(detail)))
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(popup, popup_area);
}

fn help_entry(name: &str, key: Option<char>, on: &str, off: &str) -> Vec<Line<'static>> {
    let title = match key {
        Some(k) => format!("{name} [{k}]"),
        None => name.to_string(),
    };
    vec![
        Line::from(Span::styled(title, Style::default().add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled("  ON:  ", Style::default().fg(Color::Green)),
            Span::raw(on.to_string()),
        ]),
        Line::from(vec![
            Span::styled("  OFF: ", Style::default().fg(Color::Red)),
            Span::raw(off.to_string()),
        ]),
        Line::from(""),
    ]
}

fn draw_help_popup(f: &mut Frame, area: Rect) {
    let popup_area = centered_rect(78, 80, area);
    f.render_widget(Clear, popup_area);

    let mut lines = Vec::new();
    lines.extend(help_entry(
        "Power Nap",
        Some('p'),
        "Wakes periodically from sleep to refresh Mail/iCloud/Photos/Calendar and run Time Machine/software-update checks. Main cause of frequent dark-wake battery drain.",
        "Stays in real deep sleep; those apps just sync the moment you wake it instead.",
    ));
    lines.extend(help_entry(
        "Wake for network access",
        Some('w'),
        "Other devices on the network can wake this Mac remotely (AirPlay, Handoff, screen sharing, Find My).",
        "Only wakes on physical action (lid open, key press). Closes another channel that can pull it out of sleep.",
    ));
    lines.extend(help_entry(
        "Low Power Mode",
        Some('l'),
        "Caps CPU/GPU clock speed and throttles background activity to stretch battery life, at the cost of performance.",
        "Full performance, normal battery use.",
    ));
    lines.extend(help_entry(
        "Standby",
        Some('s'),
        "After several hours asleep on battery, drops into a deeper standby state for extra power savings; wake is a bit slower.",
        "Stays in regular sleep indefinitely — RAM stays powered, wakes faster, but drains faster over long unplugged stretches.",
    ));
    lines.extend(help_entry(
        "TCP Keepalive",
        Some('t'),
        "Keeps existing network connections (SSH, file shares) alive through Power Nap's brief wake windows.",
        "Connections may need to reconnect after a longer sleep; no keepalive network traffic while asleep.",
    ));
    lines.push(Line::from(Span::styled(
        "Disk sleep / Display sleep",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(
        "  Idle minutes before the disk (mostly a no-op on SSDs) or the screen powers down. Read-only here — numeric, not a toggle.",
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[Esc/q/?] close",
        Style::default().fg(Color::DarkGray),
    )));

    let popup = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Settings Help ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(popup, popup_area);
}

fn draw_settings(f: &mut Frame, area: Rect, app: &App) {
    let lines = match &app.settings {
        Some(s) => vec![
            setting_line('p', "Power Nap", s.powernap),
            setting_line('w', "Wake for network access", s.womp),
            setting_line('l', "Low Power Mode", s.lowpowermode),
            setting_line('s', "Standby", s.standby),
            setting_line('t', "TCP Keepalive", s.tcpkeepalive),
            Line::from(format!(
                "Disk sleep: {}m   Display sleep: {}m",
                s.disksleep, s.displaysleep
            )),
        ],
        None => vec![Line::from("waiting for settings data...")],
    };
    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Settings (press key to toggle)"),
    );
    f.render_widget(panel, area);
}

fn setting_line(key: char, label: &str, on: bool) -> Line<'static> {
    let (text, color) = if on {
        ("ON", Color::Green)
    } else {
        ("OFF", Color::Red)
    };
    Line::from(vec![
        Span::raw(format!("[{key}] {label}: ")),
        Span::styled(text, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ])
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let (text, color) = if let Some((pid, name)) = &app.pending_kill {
        (
            format!("Kill {name} (pid {pid})? [y] confirm   [any other key] cancel"),
            Color::Red,
        )
    } else {
        let text = app.status.clone().unwrap_or_else(|| {
            let toggles = if app.sudo_ok {
                "p/w/l/s/t toggle settings"
            } else {
                "(read-only: sudo unavailable, toggles disabled)"
            };
            format!(
                "q quit   \u{2191}\u{2193}/jk select   Enter/i detail   ? help   K kill   +/- renice   {toggles}"
            )
        });
        (text, Color::DarkGray)
    };
    let footer = Paragraph::new(text).style(Style::default().fg(color));
    f.render_widget(footer, area);
}
