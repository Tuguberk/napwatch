use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Prompts for the sudo password on the current TTY (must be called before
/// entering raw mode / the alternate screen). Returns whether a usable
/// sudo session was established.
pub fn ensure_sudo() -> bool {
    Command::new("sudo")
        .arg("-v")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Keeps the cached sudo ticket alive so toggles never need to re-prompt
/// mid-UI. Runs non-interactively; if the ticket has already expired this
/// simply fails silently and the next toggle attempt reports the error.
pub fn spawn_sudo_refresher() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(60));
        let _ = Command::new("sudo")
            .args(["-n", "-v"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

fn set_pmset(key: &str, value: &str) -> Result<()> {
    let status = Command::new("sudo")
        .args(["-n", "pmset", "-a", key, value])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawning sudo pmset")?;
    if !status.success() {
        bail!("pmset -a {key} {value} failed (sudo session may have expired)");
    }
    Ok(())
}

pub fn toggle_bool_setting(key: &str, current: bool) -> Result<()> {
    set_pmset(key, if current { "0" } else { "1" })
}

fn run_maybe_sudo(program: &str, args: &[String], use_sudo: bool) -> Result<()> {
    let status = if use_sudo {
        let mut full_args = vec!["-n".to_string(), program.to_string()];
        full_args.extend(args.iter().cloned());
        Command::new("sudo")
            .args(&full_args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    }
    .with_context(|| format!("spawning {program}"))?;

    if !status.success() {
        bail!("{program} failed (permission denied, or the process is gone)");
    }
    Ok(())
}

/// Sends SIGTERM. Uses the cached sudo ticket when available so it also
/// works on processes owned by other users (mostly root system daemons);
/// otherwise only works on processes the current user owns.
pub fn kill_process(pid: u32, use_sudo: bool) -> Result<()> {
    run_maybe_sudo("kill", &["-TERM".to_string(), pid.to_string()], use_sudo)
}

/// Adjusts scheduling priority by a relative delta (clamped to the valid
/// -20..=19 nice range) and returns the resulting value.
pub fn renice_process(pid: u32, delta: i32, use_sudo: bool) -> Result<i32> {
    let current = crate::power::get_nice(pid)?;
    let new_value = (current + delta).clamp(-20, 19);
    run_maybe_sudo(
        "renice",
        &[new_value.to_string(), "-p".to_string(), pid.to_string()],
        use_sudo,
    )?;
    Ok(new_value)
}
