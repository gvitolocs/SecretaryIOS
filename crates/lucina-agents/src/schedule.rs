//! Schedules and heartbeats (Paseo `protocol/schedule/types.js` `StoredScheduleSchema`
//! and `protocol/schedule/cron-expression.js`).
//!
//! A heartbeat is a schedule whose target is an existing agent (`target.type:
//! "agent"`): each fire is a system-wrapped prompt to that agent. Any other schedule
//! starts a new agent per run. Lucina adds `labels` and `createdBy` (the caller's
//! origin: `local`, `device:<id>`, `mcp:<token id>` or `agent:<agent id>`). The owner
//! may change every schedule; an MCP token or an agent only the ones it created.
//!
//! The next-run computation needs time zones and lives in `lucina-agentd`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Cadence {
    Every {
        every_ms: u64,
    },
    Cron {
        expression: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    #[default]
    Local,
    Worktree,
}

/// What a new-agent schedule starts each time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentConfig {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    pub cwd: String,
    /// Lucina: run in this workspace (its cwd); `isolation: worktree` branches off it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<Isolation>,
    /// Archive the run's workspace when it ends (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_on_finish: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Target {
    /// A heartbeat: the prompt goes to this agent.
    Agent { agent_id: String },
    NewAgent { config: NewAgentConfig },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleStatus {
    Active,
    Paused,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    pub id: String,
    pub scheduled_for: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: RunStatus,
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub name: Option<String>,
    pub prompt: String,
    pub cadence: Cadence,
    pub target: Target,
    pub status: ScheduleStatus,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub paused_at: Option<String>,
    pub expires_at: Option<String>,
    pub max_runs: Option<u32>,
    pub runs: Vec<ScheduleRun>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The origin that created it (module docs).
    pub created_by: String,
}

/// Runs kept per schedule (Paseo keeps every run; a 5-minute heartbeat would grow
/// without end).
pub const MAX_RUNS_KEPT: usize = 200;

impl Schedule {
    /// Paseo `countCompletedRuns` (Lucina counts dropped history too).
    pub fn completed_runs(&self) -> u64 {
        let kept = self.runs.iter().filter(|r| r.status != RunStatus::Running).count() as u64;
        kept + self.labels.get(DROPPED_RUNS_LABEL).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0)
    }

    pub fn is_heartbeat(&self) -> bool {
        matches!(self.target, Target::Agent { .. })
    }
}

/// How many finished runs were dropped from `runs` (for `maxRuns`).
pub const DROPPED_RUNS_LABEL: &str = "lucina.dropped-runs";

// -- cron -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronField {
    allowed: Vec<bool>,
}

impl CronField {
    pub fn matches(&self, value: u32) -> bool {
        self.allowed.get(value as usize).copied().unwrap_or(false)
    }
}

/// A parsed five-field cron expression (minute hour day-of-month month day-of-week).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    pub minute: CronField,
    pub hour: CronField,
    pub day_of_month: CronField,
    pub month: CronField,
    pub day_of_week: CronField,
}

struct Bounds {
    min: u32,
    max: u32,
    name: &'static str,
}

const BOUNDS: [Bounds; 5] = [
    Bounds { min: 0, max: 59, name: "minute" },
    Bounds { min: 0, max: 23, name: "hour" },
    Bounds { min: 1, max: 31, name: "day-of-month" },
    Bounds { min: 1, max: 12, name: "month" },
    Bounds { min: 0, max: 6, name: "day-of-week" },
];

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// JavaScript `Number.parseInt(s, 10)` on a string of digits (saturating).
fn int(s: &str) -> u32 {
    s.parse::<u64>().map(|v| v.min(u32::MAX as u64) as u32).unwrap_or(u32::MAX)
}

fn parse_field(source: &str, b: &Bounds) -> Result<CronField, String> {
    let trimmed = source.trim();
    let invalid = |what: &str| format!("Invalid cron {} {what}", b.name);
    if trimmed.is_empty() {
        return Err(invalid("field"));
    }
    let mut allowed = vec![false; b.max as usize + 1];
    for raw in trimmed.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            return Err(invalid("field"));
        }
        let pieces: Vec<&str> = part.split('/').collect();
        if pieces.len() > 2 {
            return Err(invalid("step"));
        }
        let base = pieces[0];
        let step = match pieces.get(1).map(|s| s.trim()) {
            None => 1,
            // parseInt reads leading digits; String(step) must equal the source.
            Some(s) if digits(s) && !(s.len() > 1 && s.starts_with('0')) && int(s) > 0 => int(s),
            Some(_) => return Err(invalid("step")),
        };
        let mut range = |start: u32, end: u32| {
            let mut v = start;
            while v <= end {
                allowed[v as usize] = true;
                v = match v.checked_add(step) {
                    Some(n) => n,
                    None => break,
                };
            }
        };
        if base == "*" {
            range(b.min, b.max);
            continue;
        }
        if let Some((s, e)) = base.split_once('-') {
            if digits(s) && digits(e) {
                let (start, end) = (int(s), int(e));
                if start > end || start < b.min || end > b.max {
                    return Err(invalid("range"));
                }
                range(start, end);
                continue;
            }
        }
        if !digits(base) {
            return Err(invalid("value"));
        }
        let v = int(base);
        if v < b.min || v > b.max {
            return Err(invalid("value"));
        }
        allowed[v as usize] = true;
    }
    Ok(CronField { allowed })
}

/// Paseo `parseCronExpression` (same error texts).
pub fn parse_cron(expression: &str) -> Result<CronExpression, String> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err("Cron expressions must have 5 fields".into());
    }
    Ok(CronExpression {
        minute: parse_field(parts[0], &BOUNDS[0])?,
        hour: parse_field(parts[1], &BOUNDS[1])?,
        day_of_month: parse_field(parts[2], &BOUNDS[2])?,
        month: parse_field(parts[3], &BOUNDS[3])?,
        day_of_week: parse_field(parts[4], &BOUNDS[4])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_fields() {
        let c = parse_cron("*/15 7-9 * * 1-5").unwrap();
        assert!(c.minute.matches(45) && !c.minute.matches(10));
        assert!(c.hour.matches(9) && !c.hour.matches(10));
        assert!(c.day_of_week.matches(5) && !c.day_of_week.matches(0));
        assert_eq!(parse_cron("* * * *").unwrap_err(), "Cron expressions must have 5 fields");
        assert_eq!(parse_cron("60 * * * *").unwrap_err(), "Invalid cron minute value");
        assert_eq!(parse_cron("*/0 * * * *").unwrap_err(), "Invalid cron minute step");
        assert_eq!(parse_cron("* 9-7 * * *").unwrap_err(), "Invalid cron hour range");
        assert_eq!(parse_cron("* * * * 7").unwrap_err(), "Invalid cron day-of-week value");
    }

    #[test]
    fn wire_shape() {
        let s: Schedule = serde_json::from_value(serde_json::json!({
            "id": "ab12cd34", "name": null, "prompt": "p",
            "cadence": {"type": "cron", "expression": "0 7 * * *", "timezone": "Europe/Rome"},
            "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/w"}},
            "status": "active", "createdAt": "t", "updatedAt": "t", "nextRunAt": null, "lastRunAt": null,
            "pausedAt": null, "expiresAt": null, "maxRuns": null, "runs": [], "createdBy": "local"
        }))
        .unwrap();
        assert!(!s.is_heartbeat());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["target"]["type"], "new-agent");
        assert_eq!(v["cadence"]["timezone"], "Europe/Rome");
    }
}
