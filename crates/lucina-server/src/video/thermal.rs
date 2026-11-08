//! nezopt runs hot when several sessions build at once. The owner's rule: background CPU
//! work runs in a capped scope and pauses while Tctl ≥ 90 °C. [`command`] wraps a program
//! in a transient `systemd-run --user --scope` with a CPU quota and a memory cap (which also
//! keeps ffmpeg out of lucina-rs.service's 768M), and [`wait_cool`] waits for the CPU to
//! cool down before a heavy step.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ThermalConfig {
    /// `LUCINA_VIDEO_MAX_TCTL` (°C).
    pub max_c: f64,
    /// Wait at most this long, then go on at the lowest priority.
    pub max_wait: Duration,
    pub poll: Duration,
    /// `systemd-run` scope caps; `LUCINA_VIDEO_SCOPE=false` runs programs directly (tests).
    pub scope: bool,
    pub cpu_quota: String,
    pub memory_max: String,
}

impl ThermalConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        Self {
            max_c: get("LUCINA_VIDEO_MAX_TCTL")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(90.0),
            max_wait: Duration::from_secs(
                get("LUCINA_VIDEO_COOL_WAIT_SEC")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(20 * 60),
            ),
            poll: Duration::from_secs(15),
            scope: get("LUCINA_VIDEO_SCOPE")
                .is_none_or(|v| !matches!(v.trim(), "0" | "false" | "off")),
            cpu_quota: get("LUCINA_VIDEO_CPU_QUOTA").unwrap_or_else(|| "400%".into()),
            memory_max: get("LUCINA_VIDEO_MEMORY_MAX").unwrap_or_else(|| "6G".into()),
        }
    }
}

/// The AMD package temperature (`k10temp` Tctl), if this host has one.
pub fn tctl() -> Option<f64> {
    let dirs = std::fs::read_dir("/sys/class/hwmon").ok()?;
    for d in dirs.flatten() {
        let p = d.path();
        if std::fs::read_to_string(p.join("name")).ok()?.trim() != "k10temp" {
            continue;
        }
        for i in 1..=8 {
            let label =
                std::fs::read_to_string(p.join(format!("temp{i}_label"))).unwrap_or_default();
            if label.trim() == "Tctl" {
                let milli: f64 = std::fs::read_to_string(p.join(format!("temp{i}_input")))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
                return Some(milli / 1000.0);
            }
        }
    }
    None
}

/// Waits while Tctl ≥ the limit, at most `max_wait`. Returns how long it waited.
pub async fn wait_cool(cfg: &ThermalConfig) -> Duration {
    let start = std::time::Instant::now();
    let mut logged = false;
    while let Some(t) = tctl() {
        if t < cfg.max_c {
            break;
        }
        if start.elapsed() >= cfg.max_wait {
            tracing::warn!(tctl = t, waited = ?start.elapsed(), "still hot: going on at the lowest priority");
            break;
        }
        if !logged {
            tracing::info!(
                tctl = t,
                limit = cfg.max_c,
                "nezopt is hot: waiting before the next step"
            );
            logged = true;
        }
        tokio::time::sleep(cfg.poll).await;
    }
    start.elapsed()
}

/// `program` in a capped transient scope (nice 15), or directly when scopes are off.
pub fn command(cfg: &ThermalConfig, program: &Path) -> Command {
    let mut cmd = if cfg.scope {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--scope", "--quiet", "--collect", "--nice=15"])
            .arg(format!("-pCPUQuota={}", cfg.cpu_quota))
            .arg(format!("-pMemoryMax={}", cfg.memory_max))
            .arg("--")
            .arg(program);
        c
    } else {
        Command::new(program)
    };
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    cmd
}

/// Runs `cmd` to the end (at most `timeout`) and returns its stdout; a non-zero exit is an
/// error carrying the end of stderr.
pub async fn output(mut cmd: Command, timeout: Duration) -> anyhow::Result<String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| anyhow!("spawn: {e}"))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| anyhow!("timed out after {timeout:?}"))??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        bail!(
            "exit {:?}: {}",
            out.status.code(),
            tail.chars().take(600).collect::<String>()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_reports_failures_with_stderr() {
        let cfg = ThermalConfig::from_lookup(&|k: &str| {
            (k == "LUCINA_VIDEO_SCOPE").then(|| "false".into())
        });
        let mut ok = command(&cfg, Path::new("/bin/sh"));
        ok.args(["-c", "echo hi"]);
        assert_eq!(
            output(ok, Duration::from_secs(5)).await.unwrap().trim(),
            "hi"
        );
        let mut bad = command(&cfg, Path::new("/bin/sh"));
        bad.args(["-c", "echo boom >&2; exit 3"]);
        let e = output(bad, Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("boom") && e.contains("3"), "{e}");
    }
}
