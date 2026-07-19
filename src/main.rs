mod actions;
mod app;
mod power;
mod ui;

use anyhow::Result;
use app::App;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--once") {
        println!("battery:  {:#?}", power::get_battery_info());
        println!("wake:     {:#?}", power::get_wake_stats());
        println!("settings: {:#?}", power::get_power_settings());
        println!("top procs:{:#?}", power::get_top_processes(5));
        return Ok(());
    }
    if let Some(pid_arg) = std::env::args().skip_while(|a| a != "--detail").nth(1) {
        let pid: u32 = pid_arg.parse().expect("pid must be a number");
        println!("{:#?}", power::get_process_detail(pid));
        return Ok(());
    }
    if std::env::args().any(|a| a == "--wake-log") {
        match power::get_wake_events() {
            Ok(events) => {
                println!("parsed {} events; last 10:", events.len());
                for e in events.iter().rev().take(10).rev() {
                    println!("{:?}", e);
                }
            }
            Err(e) => println!("error: {e}"),
        }
        return Ok(());
    }

    println!("napwatch: requesting sudo so Power Nap / Wake-on-LAN can be toggled from the UI...");
    let sudo_ok = actions::ensure_sudo();
    if sudo_ok {
        actions::spawn_sudo_refresher();
    } else {
        println!("Continuing in read-only mode (no sudo session).");
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, sudo_ok);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, sudo_ok: bool) -> Result<()> {
    let mut app = App::new(sudo_ok);
    let rx = app::spawn_poller(Duration::from_secs(2));

    loop {
        while let Ok(snap) = rx.try_recv() {
            app.apply(snap);
        }
        app.tick_status();

        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    // Pending kill confirmation takes priority over every other key.
                    KeyCode::Char('y') | KeyCode::Char('Y') if app.pending_kill.is_some() => {
                        handle_confirm_kill(&mut app);
                    }
                    _ if app.pending_kill.is_some() => {
                        app.pending_kill = None;
                    }
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') if app.show_help => {
                        app.show_help = false;
                    }
                    _ if app.show_help => {}
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter | KeyCode::Char('i')
                        if app.show_detail =>
                    {
                        app.show_detail = false;
                    }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                    KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                    KeyCode::Enter | KeyCode::Char('i') => handle_show_detail(&mut app),
                    KeyCode::Char('?') => app.show_help = true,
                    KeyCode::Char('K') => app.request_kill(),
                    KeyCode::Char('+') | KeyCode::Char('=') => handle_renice(&mut app, 1),
                    KeyCode::Char('-') => handle_renice(&mut app, -1),
                    KeyCode::Char('p') => handle_toggle(&mut app, "powernap"),
                    KeyCode::Char('w') => handle_toggle(&mut app, "womp"),
                    KeyCode::Char('l') => handle_toggle(&mut app, "lowpowermode"),
                    KeyCode::Char('s') => handle_toggle(&mut app, "standby"),
                    KeyCode::Char('t') => handle_toggle(&mut app, "tcpkeepalive"),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn handle_show_detail(app: &mut App) {
    let Some(pid) = app.selected_pid else {
        app.set_status("no process selected");
        return;
    };
    match power::get_process_detail(pid) {
        Ok(detail) => {
            app.detail = Some(detail);
            app.show_detail = true;
        }
        Err(e) => app.set_status(format!("error: {e}")),
    }
}

fn handle_toggle(app: &mut App, key: &str) {
    if !app.sudo_ok {
        app.set_status("sudo unavailable — restart napwatch to enable toggles");
        return;
    }
    let current = match (&app.settings, key) {
        (Some(s), "powernap") => s.powernap,
        (Some(s), "womp") => s.womp,
        (Some(s), "lowpowermode") => s.lowpowermode,
        (Some(s), "standby") => s.standby,
        (Some(s), "tcpkeepalive") => s.tcpkeepalive,
        _ => {
            app.set_status("settings not loaded yet");
            return;
        }
    };
    match actions::toggle_bool_setting(key, current) {
        Ok(()) => {
            app.apply_toggle(key, !current);
            app.set_status(format!("{key} -> {}", if current { "off" } else { "on" }));
        }
        Err(e) => app.set_status(format!("error: {e}")),
    }
}

fn handle_confirm_kill(app: &mut App) {
    let Some((pid, name)) = app.pending_kill.take() else {
        return;
    };
    match actions::kill_process(pid, app.sudo_ok) {
        Ok(()) => app.set_status(format!("sent SIGTERM to {name} ({pid})")),
        Err(e) => app.set_status(format!("error: {e}")),
    }
}

fn handle_renice(app: &mut App, delta: i32) {
    let Some(pid) = app.selected_pid else {
        app.set_status("no process selected");
        return;
    };
    match actions::renice_process(pid, delta, app.sudo_ok) {
        Ok(new_value) => app.set_status(format!("renice pid {pid} -> {new_value}")),
        Err(e) => app.set_status(format!("error: {e}")),
    }
}
