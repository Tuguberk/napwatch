use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub charging: bool,
    pub on_ac: bool,
    pub time_remaining: Option<String>,
}

pub fn get_battery_info() -> Result<BatteryInfo> {
    let out = Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .context("running pmset -g batt")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let on_ac = text.contains("AC Power");

    let line = text
        .lines()
        .nth(1)
        .context("pmset -g batt: missing battery detail line")?;

    let mut segments = line.split(';');
    let first = segments.next().unwrap_or("");
    let percentage = first
        .split_whitespace()
        .find_map(|tok| tok.strip_suffix('%').and_then(|n| n.parse::<u8>().ok()))
        .context("pmset -g batt: could not parse percentage")?;

    let status = segments.next().unwrap_or("").trim();
    let charging = matches!(status, "charging" | "charged" | "finishing charge");

    let time_remaining = segments.next().and_then(|s| {
        let time_part = s.split(" remaining").next().unwrap_or(s).trim();
        if time_part.is_empty() || time_part.contains("no estimate") {
            None
        } else {
            Some(time_part.to_string())
        }
    });

    Ok(BatteryInfo {
        percentage,
        charging,
        on_ac,
        time_remaining,
    })
}

#[derive(Debug, Clone, Default)]
pub struct WakeStats {
    pub sleep_count: u32,
    pub dark_wake_count: u32,
    pub user_wake_count: u32,
}

pub fn get_wake_stats() -> Result<WakeStats> {
    let out = Command::new("pmset")
        .args(["-g", "stats"])
        .output()
        .context("running pmset -g stats")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let field = |label: &str| -> u32 {
        text.lines()
            .find(|l| l.starts_with(label))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };

    Ok(WakeStats {
        sleep_count: field("Sleep Count"),
        dark_wake_count: field("Dark Wake Count"),
        user_wake_count: field("User Wake Count"),
    })
}

#[derive(Debug, Clone, Default)]
pub struct PowerSettings {
    pub powernap: bool,
    pub womp: bool,
    pub lowpowermode: bool,
    pub tcpkeepalive: bool,
    pub standby: bool,
    pub disksleep: u32,
    pub displaysleep: u32,
}

pub fn get_power_settings() -> Result<PowerSettings> {
    let out = Command::new("pmset")
        .arg("-g")
        .output()
        .context("running pmset -g")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut map: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        if let (Some(key), Some(rest)) = (parts.next(), parts.next()) {
            let value = rest.trim().split_whitespace().next().unwrap_or("").to_string();
            map.insert(key.to_string(), value);
        }
    }

    let get_bool = |k: &str| map.get(k).map(|v| v == "1").unwrap_or(false);
    let get_u32 = |k: &str| map.get(k).and_then(|v| v.parse().ok()).unwrap_or(0);

    Ok(PowerSettings {
        powernap: get_bool("powernap"),
        womp: get_bool("womp"),
        lowpowermode: get_bool("lowpowermode"),
        tcpkeepalive: get_bool("tcpkeepalive"),
        standby: get_bool("standby"),
        disksleep: get_u32("disksleep"),
        displaysleep: get_u32("displaysleep"),
    })
}

#[derive(Debug, Clone)]
pub struct ProcessPower {
    pub pid: u32,
    pub command: String,
    pub power: f32,
}

pub fn get_top_processes(n: usize) -> Result<Vec<ProcessPower>> {
    // `-l 1` would take a single instantaneous sample, but power is a rate
    // (energy impact over time) and reads as 0.0 for everyone without a
    // prior sample to diff against. `-l 2` takes two 1s-apart samples; we
    // keep only the second block, which has real deltas. Over-fetch rows
    // since top's internal ranking during that second sample can be
    // slightly stale — we re-sort ourselves below.
    let fetch = (n * 2).max(20);
    let out = Command::new("top")
        .args([
            "-l",
            "2",
            "-n",
            &fetch.to_string(),
            "-stats",
            "pid,command,power",
            "-o",
            "power",
        ])
        .output()
        .context("running top")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut rows = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("PID") && trimmed.contains("COMMAND") {
            // Reset on every header seen so only the last (second) sample's
            // rows survive the loop.
            rows.clear();
            in_table = true;
            continue;
        }
        if !in_table || trimmed.is_empty() {
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let pid = match fields.next().and_then(|s| s.parse::<u32>().ok()) {
            Some(p) => p,
            None => continue,
        };
        let rest: Vec<&str> = fields.collect();
        let Some((power_str, command_parts)) = rest.split_last() else {
            continue;
        };
        let power = power_str.parse::<f32>().unwrap_or(0.0);
        let command = command_parts.join(" ");
        rows.push(ProcessPower { pid, command, power });
    }

    rows.sort_by(|a, b| b.power.partial_cmp(&a.power).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(n);
    Ok(rows)
}

#[derive(Debug, Clone, Default)]
pub struct ProcessDetail {
    pub pid: u32,
    pub ppid: u32,
    pub parent_command: Option<String>,
    pub user: String,
    pub cpu_percent: f32,
    pub mem_percent: f32,
    pub elapsed: String,
    pub full_command: String,
    pub bundle_name: Option<String>,
    pub bundle_id: Option<String>,
    pub bundle_version: Option<String>,
    pub launchd_label: Option<String>,
}

pub fn get_process_detail(pid: u32) -> Result<ProcessDetail> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,user=,%cpu=,%mem=,etime=,comm="])
        .output()
        .context("running ps")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("process {pid} is no longer running");
    }

    let mut fields = trimmed.split_whitespace();
    let ppid: u32 = fields.next().context("ps: missing ppid")?.parse().unwrap_or(0);
    let user = fields.next().context("ps: missing user")?.to_string();
    let cpu_percent: f32 = fields.next().context("ps: missing %cpu")?.parse().unwrap_or(0.0);
    let mem_percent: f32 = fields.next().context("ps: missing %mem")?.parse().unwrap_or(0.0);
    let elapsed = fields.next().context("ps: missing etime")?.to_string();
    let full_command: String = fields.collect::<Vec<_>>().join(" ");

    let parent_command = Command::new("ps")
        .args(["-p", &ppid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let bundle_path = full_command.find(".app/").map(|idx| full_command[..idx + 4].to_string());
    let (bundle_name, bundle_id, bundle_version) = match &bundle_path {
        Some(path) => (
            extract_plist_value(path, "CFBundleName"),
            extract_plist_value(path, "CFBundleIdentifier"),
            extract_plist_value(path, "CFBundleShortVersionString"),
        ),
        None => (None, None, None),
    };

    Ok(ProcessDetail {
        pid,
        ppid,
        parent_command,
        user,
        cpu_percent,
        mem_percent,
        elapsed,
        full_command,
        bundle_name,
        bundle_id,
        bundle_version,
        launchd_label: find_launchd_label(pid),
    })
}

fn extract_plist_value(bundle_path: &str, key: &str) -> Option<String> {
    let info_plist = format!("{bundle_path}/Contents/Info.plist");
    let out = Command::new("plutil")
        .args(["-extract", key, "raw", "-o", "-", &info_plist])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn find_launchd_label(pid: u32) -> Option<String> {
    let out = Command::new("launchctl").arg("list").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let pid_str = pid.to_string();
    text.lines().find_map(|line| {
        let mut cols = line.split('\t');
        let col_pid = cols.next()?;
        if col_pid == pid_str {
            cols.next()?; // status
            cols.next().map(|s| s.to_string())
        } else {
            None
        }
    })
}

pub fn get_nice(pid: u32) -> Result<i32> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "nice="])
        .output()
        .context("running ps")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("process {pid} is no longer running");
    }
    trimmed.parse().context("parsing nice value")
}

#[derive(Debug, Clone, Copy)]
pub struct InstantPower {
    /// Positive while charging, negative while discharging.
    pub watts: f64,
    /// Same sign convention as `watts`, derived from amperage / rated capacity.
    pub percent_per_hour: f64,
}

pub fn get_instant_power() -> Result<InstantPower> {
    let out = Command::new("ioreg")
        .args(["-rn", "AppleSmartBattery"])
        .output()
        .context("running ioreg")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let amperage_ma = extract_ioreg_signed(&text, "InstantAmperage")
        .or_else(|| extract_ioreg_signed(&text, "Amperage"))
        .context("ioreg: missing InstantAmperage/Amperage")?;
    let voltage_mv = extract_ioreg_signed(&text, "Voltage").context("ioreg: missing Voltage")?;
    let max_capacity_mah =
        extract_ioreg_signed(&text, "AppleRawMaxCapacity").context("ioreg: missing AppleRawMaxCapacity")?;

    let watts = (voltage_mv as f64 / 1000.0) * (amperage_ma as f64 / 1000.0);
    let percent_per_hour = if max_capacity_mah != 0 {
        (amperage_ma as f64 / max_capacity_mah as f64) * 100.0
    } else {
        0.0
    };

    Ok(InstantPower { watts, percent_per_hour })
}

/// ioreg prints these as plain decimal, but negative values (e.g. discharge
/// current) come through two's-complement-wrapped as a huge unsigned number
/// since the underlying field is a signed 64-bit int. Parsing as u64 first
/// and reinterpreting the bits via `as i64` recovers the real signed value.
fn extract_ioreg_signed(text: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\" = ");
    let line = text.lines().find(|l| l.trim_start().starts_with(&needle))?;
    let value_str = line.trim_start().strip_prefix(&needle)?.trim();
    value_str.parse::<u64>().ok().map(|v| v as i64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeEventKind {
    Sleep,
    DarkWake,
    Wake,
}

#[derive(Debug, Clone)]
pub struct WakeEvent {
    /// "YYYY-MM-DD HH:MM:SS", kept as a plain string — lexicographically
    /// sortable/comparable as-is, so no date parsing dependency is needed.
    pub timestamp: String,
    pub kind: WakeEventKind,
    pub reason: String,
}

/// Full sleep/wake history from `pmset -g log` — the command itself always
/// dumps everything (no since/tail flag), so this is a comparatively
/// expensive call (~1s+ once the log has days of history); callers should
/// poll it on a much slower cadence than the rest of the data sources.
pub fn get_wake_events() -> Result<Vec<WakeEvent>> {
    let out = Command::new("pmset")
        .args(["-g", "log"])
        .output()
        .context("running pmset -g log")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut events = Vec::new();
    for line in text.lines() {
        let Some((header, message)) = line.split_once('\t') else {
            continue;
        };
        let mut parts = header.splitn(4, ' ');
        let (Some(date), Some(time), Some(_tz), Some(type_field)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let kind = match type_field.trim() {
            "Sleep" => WakeEventKind::Sleep,
            "DarkWake" => WakeEventKind::DarkWake,
            "Wake" => WakeEventKind::Wake,
            _ => continue,
        };
        events.push(WakeEvent {
            timestamp: format!("{date} {time}"),
            kind,
            reason: extract_wake_reason(message),
        });
    }
    Ok(events)
}

fn extract_wake_reason(message: &str) -> String {
    if let Some(idx) = message.find("due to ") {
        let after = &message[idx + "due to ".len()..];
        // Quoted reasons (e.g. 'Sleep Service Back to Sleep':TCPKeepAlive=...)
        // carry trailing metadata after the closing quote — keep only the
        // quoted text itself rather than cutting at " Using" for these.
        if let Some(rest) = after.strip_prefix('\'') {
            if let Some(end) = rest.find('\'') {
                return rest[..end].trim().to_string();
            }
        }
        let end = after.find(" Using").unwrap_or(after.len());
        let reason = after[..end].trim();
        if !reason.is_empty() {
            return reason.to_string();
        }
    }
    message.trim().to_string()
}
