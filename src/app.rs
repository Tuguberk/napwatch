use crate::power::{
    self, BatteryInfo, InstantPower, PowerSettings, ProcessDetail, ProcessPower, WakeEvent, WakeStats,
};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const STATUS_TTL: Duration = Duration::from_secs(4);
/// `pmset -g log` costs 1s+ to run (no since/tail flag, always dumps the
/// whole history), so it's fetched far less often than everything else.
const WAKE_LOG_EVERY_N_TICKS: u32 = 8;
const WAKE_FEED_CAP: usize = 40;
/// A poller cycle fetches `settings` fast, then spends ~1.5s in `top -l 2`
/// before the whole snapshot is sent — so a snapshot can carry settings that
/// were captured *before* a toggle the user just made, and land *after* it.
/// Applying it would silently flicker the UI back to the pre-toggle value
/// until the next fresh cycle corrects it. This window suppresses incoming
/// settings snapshots for a bit after a local toggle, comfortably longer
/// than that worst-case in-flight latency.
const SETTINGS_OVERRIDE_LOCK: Duration = Duration::from_secs(3);

pub struct Snapshot {
    pub battery: Option<BatteryInfo>,
    pub instant_power: Option<InstantPower>,
    pub wake: Option<WakeStats>,
    pub settings: Option<PowerSettings>,
    pub processes: Vec<ProcessPower>,
    pub new_wake_events: Option<Vec<WakeEvent>>,
}

pub fn spawn_poller(interval: Duration) -> Receiver<Snapshot> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut tick: u32 = 0;
        loop {
            let new_wake_events = if tick % WAKE_LOG_EVERY_N_TICKS == 0 {
                power::get_wake_events().ok()
            } else {
                None
            };
            let snap = Snapshot {
                battery: power::get_battery_info().ok(),
                instant_power: power::get_instant_power().ok(),
                wake: power::get_wake_stats().ok(),
                settings: power::get_power_settings().ok(),
                processes: power::get_top_processes(12).unwrap_or_default(),
                new_wake_events,
            };
            if tx.send(snap).is_err() {
                break;
            }
            tick = tick.wrapping_add(1);
            std::thread::sleep(interval);
        }
    });
    rx
}

pub struct App {
    pub battery: Option<BatteryInfo>,
    pub instant_power: Option<InstantPower>,
    pub wake: Option<WakeStats>,
    pub settings: Option<PowerSettings>,
    pub processes: Vec<ProcessPower>,
    pub selected_pid: Option<u32>,
    pub detail: Option<ProcessDetail>,
    pub show_detail: bool,
    pub show_help: bool,
    pub pending_kill: Option<(u32, String)>,
    pub wake_feed: VecDeque<WakeEvent>,
    last_wake_timestamp: Option<String>,
    pub status: Option<String>,
    status_set_at: Option<Instant>,
    settings_override_until: Option<Instant>,
    pub sudo_ok: bool,
}

impl App {
    pub fn new(sudo_ok: bool) -> Self {
        Self {
            battery: None,
            instant_power: None,
            wake: None,
            settings: None,
            processes: Vec::new(),
            selected_pid: None,
            detail: None,
            show_detail: false,
            show_help: false,
            pending_kill: None,
            wake_feed: VecDeque::new(),
            last_wake_timestamp: None,
            status: None,
            status_set_at: None,
            settings_override_until: None,
            sudo_ok,
        }
    }

    /// Sets a transient status message shown in the footer; it self-clears
    /// after `STATUS_TTL` (via `tick_status`) so it doesn't permanently
    /// paper over the keybinding hints.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_set_at = Some(Instant::now());
    }

    pub fn tick_status(&mut self) {
        if self.status_set_at.is_some_and(|t| t.elapsed() > STATUS_TTL) {
            self.status = None;
            self.status_set_at = None;
        }
    }

    pub fn apply(&mut self, snap: Snapshot) {
        if snap.battery.is_some() {
            self.battery = snap.battery;
        }
        if snap.instant_power.is_some() {
            self.instant_power = snap.instant_power;
        }
        if snap.wake.is_some() {
            self.wake = snap.wake;
        }
        if snap.settings.is_some() {
            let locked = self.settings_override_until.is_some_and(|t| Instant::now() < t);
            if !locked {
                self.settings = snap.settings;
            }
        }
        if !snap.processes.is_empty() {
            if self.selected_pid.is_none() {
                self.selected_pid = Some(snap.processes[0].pid);
            }
            self.processes = snap.processes;
        }
        if let Some(events) = snap.new_wake_events {
            self.merge_wake_events(events);
        }
    }

    // `pmset -g log` always returns full history, so on first observation
    // we seed with just the last few entries (not the whole log) and track
    // the newest timestamp seen; later calls only append events newer than
    // that, giving a feed of things that happen while the app is running.
    fn merge_wake_events(&mut self, events: Vec<WakeEvent>) {
        let Some(newest) = events.last() else {
            return;
        };
        match &self.last_wake_timestamp {
            None => {
                let start = events.len().saturating_sub(5);
                self.wake_feed.extend(events[start..].iter().cloned());
            }
            Some(last_ts) => {
                self.wake_feed
                    .extend(events.iter().filter(|e| e.timestamp > *last_ts).cloned());
            }
        }
        self.last_wake_timestamp = Some(newest.timestamp.clone());
        while self.wake_feed.len() > WAKE_FEED_CAP {
            self.wake_feed.pop_front();
        }
    }

    // Selection tracks a PID rather than a plain index, since the process
    // list re-sorts by power every poll and a fixed index would silently
    // point at a different process after each refresh.
    pub fn selected_index(&self) -> usize {
        match self.selected_pid {
            Some(pid) => self.processes.iter().position(|p| p.pid == pid).unwrap_or(0),
            None => 0,
        }
    }

    pub fn select_next(&mut self) {
        if self.processes.is_empty() {
            return;
        }
        let next = (self.selected_index() + 1).min(self.processes.len() - 1);
        self.selected_pid = Some(self.processes[next].pid);
    }

    pub fn select_prev(&mut self) {
        if self.processes.is_empty() {
            return;
        }
        let prev = self.selected_index().saturating_sub(1);
        self.selected_pid = Some(self.processes[prev].pid);
    }

    /// Reflects a just-succeeded toggle immediately instead of waiting for
    /// the next poll, and locks out incoming settings snapshots briefly so
    /// an in-flight (pre-toggle) one can't clobber it — see
    /// `SETTINGS_OVERRIDE_LOCK`.
    pub fn apply_toggle(&mut self, key: &str, new_value: bool) {
        if let Some(s) = &mut self.settings {
            let field = match key {
                "powernap" => &mut s.powernap,
                "womp" => &mut s.womp,
                "lowpowermode" => &mut s.lowpowermode,
                "standby" => &mut s.standby,
                "tcpkeepalive" => &mut s.tcpkeepalive,
                _ => return,
            };
            *field = new_value;
        }
        self.settings_override_until = Some(Instant::now() + SETTINGS_OVERRIDE_LOCK);
    }

    pub fn request_kill(&mut self) {
        let Some(pid) = self.selected_pid else {
            self.set_status("no process selected");
            return;
        };
        let name = self
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.command.clone())
            .unwrap_or_else(|| pid.to_string());
        self.pending_kill = Some((pid, name));
    }
}
