//! `/status [full]` (Node `src/status-report.js`) with the Rust runtime's
//! facts: no provider blocks exist here (the planner has no wait-out), so
//! that line always reads "none blocked".
use std::sync::OnceLock;

use lucina_memory::MemoryMode;
use regex::Regex;

use crate::AppState;

/// Node `STATUS_COMMAND`.
pub fn is_status_command(text: &str) -> Option<bool> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^/status(?:@\w+)?(?:\s+(full|--full))?\s*$").expect("status regex")
    });
    re.captures(text).map(|c| c.get(1).is_some())
}

/// Node `formatDuration`.
pub fn format_duration(ms: u64) -> String {
    let total_minutes = ((ms as f64) / 60_000.0).round() as u64;
    let days = total_minutes / 1440;
    let hours = (total_minutes % 1440) / 60;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

/// Whether the reminder store can be written (Node `checkReminderStoreWritable`).
fn store_writable(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let target = if path.exists() {
        path.to_owned()
    } else {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."))
            .to_owned()
    };
    let c = std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: a valid NUL-terminated path; access(2) only reads it.
    if unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub struct Facts<'a> {
    pub st: &'a AppState,
    pub bot_username: &'a str,
    pub bot_key: &'a str,
    pub lease_held: bool,
    pub decline: bool,
}

pub fn build(f: &Facts<'_>, full: bool) -> String {
    let st = f.st;
    let cfg = &st.cfg;
    let mut store: Result<(), String> = Ok(());
    let mut pending = Vec::new();
    if cfg.reminders_enabled {
        store = store_writable(st.reminders.path());
        match st.reminders.list(false) {
            Ok(all) => pending = all.into_iter().filter(|r| r.status == "pending").collect(),
            Err(e) => store = Err(e.to_string().chars().take(160).collect()),
        }
    }
    let mut problems = Vec::new();
    if store.is_err() {
        problems.push("reminder store not writable");
    }
    let headline = format!(
        "{} status: {}",
        cfg.persona,
        if problems.is_empty() {
            "ok".to_owned()
        } else {
            format!("degraded ({})", problems.join(", "))
        }
    );
    let memory = match st.memory.mode() {
        MemoryMode::Off => "none".to_owned(),
        m => format!("honcho ({})", m.as_str()),
    };
    let reminders = if cfg.reminders_enabled {
        format!(
            " · reminders: {} pending{}",
            pending.len(),
            if store.is_ok() {
                ""
            } else {
                " (store NOT writable)"
            }
        )
    } else {
        String::new()
    };
    let mut lines = vec![
        headline,
        format!(
            "• up {} · runtime lucina-rs{}",
            format_duration(st.started.elapsed().as_millis() as u64),
            if f.bot_username.is_empty() {
                String::new()
            } else {
                format!(" · @{}", f.bot_username)
            }
        ),
        format!("• memory: {memory}{reminders}"),
        format!(
            "• telegram lock: {}",
            if f.lease_held {
                format!("bot {} held", f.bot_key)
            } else {
                "none held".into()
            }
        ),
        "• providers: none blocked".into(),
    ];
    if full {
        let next_due = pending.iter().filter_map(|r| r.due()).min();
        let scheduler = st
            .scheduler_health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .to_owned();
        lines.push(format!(
            "• host {hostname} · lucina-rs {} · pid {}",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ));
        lines.push(format!(
            "• reminder store: {}{}",
            if cfg.reminders_enabled {
                st.reminders.path().display().to_string()
            } else {
                "(disabled)".into()
            },
            match &store {
                Ok(()) => String::new(),
                Err(e) => format!(" — {e}"),
            }
        ));
        lines.push(match next_due {
            Some(d) => format!("• next reminder: {} UTC", d.format("%Y-%m-%d %H:%M")),
            None => "• next reminder: none".into(),
        });
        lines.push(format!(
            "• scheduler: {}",
            if scheduler.enabled {
                format!(
                    "on (last tick {})",
                    scheduler.last_tick_at.as_deref().unwrap_or("never")
                )
            } else {
                "off".into()
            }
        ));
        lines.push(format!(
            "• unauthorized DMs: {}",
            if f.decline { "decline" } else { "ignore" }
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_command_and_durations() {
        assert_eq!(is_status_command("/status"), Some(false));
        assert_eq!(is_status_command("/STATUS@Lucina00_bot full"), Some(true));
        assert_eq!(is_status_command("/status --full "), Some(true));
        assert_eq!(is_status_command("/status now"), None);
        assert_eq!(is_status_command("status"), None);
        assert_eq!(format_duration(59_000), "1m");
        assert_eq!(format_duration(3_600_000 + 5 * 60_000), "1h05m");
        assert_eq!(format_duration(26 * 3_600_000), "1d2h");
    }
}
