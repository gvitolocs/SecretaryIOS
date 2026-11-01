//! Intent samples for a future local heuristic (owner, 2026-11-01). Each of the owner's own turns
//! that Jev already classified adds one line. This module never calls Jev, so it costs no Jev tokens.
//!
//! `<data>/jev-cal/samples-YYYY-MM.jsonl`, mode 0600: the owner's words, like the labelled set
//! beside them. `LUCINA_JEV_SAMPLES=off` stops it. Nothing reads these files yet.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

/// A sample keeps this much of a message.
const MAX_TEXT: usize = 4000;

fn enabled() -> bool {
    !std::env::var("LUCINA_JEV_SAMPLES")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"))
}

fn line(
    now: DateTime<Utc>,
    text: &str,
    platform: &str,
    verdict: &(Value, f64),
    rule: Option<&str>,
) -> String {
    json!({
        "ts": now.to_rfc3339(),
        "message": text.chars().take(MAX_TEXT).collect::<String>(),
        "intent": verdict.0["intent"],
        "confidence": verdict.1,
        "model": verdict.0["model"],
        "platform": platform,
        "tier0": rule,
    })
    .to_string()
}

fn append(dir: &Path, now: DateTime<Utc>, line: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("samples-{}.jsonl", now.format("%Y-%m")));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// Note a turn Jev answered, in the background. A failure is logged and never touches the turn.
pub fn record(
    data_dir: &Path,
    text: &str,
    platform: &str,
    verdict: &(Value, f64),
    rule: Option<&str>,
) {
    if !enabled() {
        return;
    }
    let now = Utc::now();
    let line = line(now, text, platform, verdict, rule);
    let dir = data_dir.join("jev-cal");
    tokio::task::spawn_blocking(move || {
        if let Err(e) = append(&dir, now, &line) {
            tracing::warn!(error = %e, "jev sample not saved");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn samples_go_to_a_private_monthly_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cal");
        let now = "2026-11-02T08:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let verdict = (json!({"intent": "chat", "model": "jev-1.14.0"}), 0.91);
        for text in ["ciao", "e\ndue"] {
            append(&dir, now, &line(now, text, "web", &verdict, Some("social_post"))).unwrap();
        }
        let path = dir.join("samples-2026-11.jsonl");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let body = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["message"], "e\ndue");
        assert_eq!(rows[1]["intent"], "chat");
        assert_eq!(rows[1]["confidence"], 0.91);
        assert_eq!(rows[1]["model"], "jev-1.14.0");
        assert_eq!(rows[1]["platform"], "web");
        assert_eq!(rows[1]["tier0"], "social_post");
    }

    #[test]
    fn a_long_message_is_capped() {
        let now = Utc::now();
        let row: Value = serde_json::from_str(&line(now, &"é".repeat(MAX_TEXT + 9), "web", &(json!({}), 0.5), None)).unwrap();
        assert_eq!(row["message"].as_str().unwrap().chars().count(), MAX_TEXT);
    }
}
