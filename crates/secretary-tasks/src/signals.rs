//! Task signals for Lucina's prompts and the proactive engine
//! (`GET /v1/tasks/signals`, and `tasks` in `/v1/insights/summary`):
//! today's load, overdue tasks with how long they have slipped, and the
//! recurring routines with their completion streaks.

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::models::*;
use crate::views::{completed_today, open_tasks};
use crate::{due_local, ms_to_utc, today_in};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRef {
    pub id: String,
    pub content: String,
    pub project: String,
    /// User-facing 1 (urgent) … 4.
    pub priority: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    pub recurring: bool,
    /// Days past the due date (0 for today).
    pub days_overdue: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Routine {
    pub id: String,
    pub content: String,
    /// Todoist's recurrence text ("every day").
    pub recurrence: String,
    pub due_date: String,
    pub days_overdue: i64,
    /// Consecutive days with a completion, ending today or yesterday.
    pub streak_days: u32,
    /// Completions in the last 7 days.
    pub done_last_7: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_completed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signals {
    pub today: String,
    pub timezone: String,
    /// Open tasks due today (not overdue).
    pub today_open: u32,
    pub overdue_open: u32,
    pub completed_today: u32,
    pub daily_goal: u32,
    pub goal_reached: bool,
    /// Estimated minutes of today's and overdue open tasks (Todoist
    /// durations, or "15 min" in the title).
    pub load_minutes: u32,
    /// Today's and overdue tasks without an estimate.
    pub unestimated: u32,
    pub today_tasks: Vec<TaskRef>,
    /// Oldest first.
    pub overdue: Vec<TaskRef>,
    pub routines: Vec<Routine>,
    pub sync_status: SyncStatus,
    pub pending_edits: u32,
    pub conflicts: u32,
}

/// Minutes written in a title: "15 min shadowing", "20min speaking",
/// "1h reading", "mezz'ora", "30 minuti", "45 minutter".
pub fn minutes_in(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if lower.contains("mezz'ora") || lower.contains("mezzora") || lower.contains("half hour") {
        return Some(30);
    }
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let n: u32 = chars[start..i].iter().collect::<String>().parse().ok()?;
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let unit: String = chars[j..]
                .iter()
                .take_while(|c| c.is_alphabetic())
                .collect();
            let mins = match unit.as_str() {
                "m" | "min" | "mins" | "minute" | "minutes" | "minuto" | "minuti" | "minut"
                | "minutter" => Some(n),
                "h" | "hr" | "hrs" | "hour" | "hours" | "ora" | "ore" | "time" | "timer" => {
                    Some(n * 60)
                }
                _ => None,
            };
            if let Some(m) = mins.filter(|m| (1..=24 * 60).contains(m)) {
                return Some(m);
            }
        }
        i += 1;
    }
    None
}

fn minutes_of(t: &Task) -> Option<u32> {
    match &t.duration {
        Some(d) if d.unit == "day" => Some(d.amount.saturating_mul(480)),
        Some(d) => Some(d.amount),
        None => minutes_in(&t.content),
    }
}

pub fn signals(s: &State, tz: Tz, now: u64) -> Signals {
    let today = today_in(tz, now);
    let mut today_tasks = Vec::new();
    let mut overdue = Vec::new();
    let mut routines = Vec::new();
    let (mut load, mut unestimated) = (0u32, 0u32);
    let project = |id: &str| {
        s.projects
            .get(id)
            .map_or_else(|| "Inbox".to_owned(), |p| p.name.clone())
    };
    for t in open_tasks(s) {
        let Some((date, time)) = t.due.as_ref().and_then(|d| due_local(d, tz)) else {
            continue;
        };
        let days_overdue = (today - date).num_days();
        let r = TaskRef {
            id: t.id.clone(),
            content: t.content.clone(),
            project: project(&t.project_id),
            priority: t.p(),
            due_date: Some(date.format("%Y-%m-%d").to_string()),
            time: time.map(|x| x.format("%H:%M").to_string()),
            recurring: t.recurring(),
            days_overdue: days_overdue.max(0),
            minutes: minutes_of(t),
        };
        if date <= today {
            match r.minutes {
                Some(m) => load = load.saturating_add(m),
                None => unestimated += 1,
            }
        }
        if t.recurring() {
            let days: BTreeSet<NaiveDate> = s
                .completed
                .iter()
                .filter(|c| c.task_id == t.id)
                .map(|c| ms_to_utc(c.completed_ms).with_timezone(&tz).date_naive())
                .collect();
            let mut day = if days.contains(&today) {
                today
            } else {
                today - Duration::days(1)
            };
            let mut streak = 0;
            while days.contains(&day) {
                streak += 1;
                day -= Duration::days(1);
            }
            routines.push(Routine {
                id: t.id.clone(),
                content: t.content.clone(),
                recurrence: t.due.as_ref().map(|d| d.string.clone()).unwrap_or_default(),
                due_date: date.format("%Y-%m-%d").to_string(),
                days_overdue: days_overdue.max(0),
                streak_days: streak,
                done_last_7: days
                    .iter()
                    .filter(|d| **d > today - Duration::days(7))
                    .count() as u32,
                last_completed_ms: s
                    .completed
                    .iter()
                    .filter(|c| c.task_id == t.id)
                    .map(|c| c.completed_ms)
                    .max(),
            });
        }
        if date < today {
            overdue.push(r);
        } else if date == today {
            today_tasks.push(r);
        }
    }
    overdue.sort_by(|a, b| b.days_overdue.cmp(&a.days_overdue).then(a.id.cmp(&b.id)));
    today_tasks.sort_by(|a, b| {
        (a.time.is_none(), &a.time, a.priority, &a.id).cmp(&(
            b.time.is_none(),
            &b.time,
            b.priority,
            &b.id,
        ))
    });
    routines.sort_by(|a, b| {
        b.days_overdue
            .cmp(&a.days_overdue)
            .then(a.content.cmp(&b.content))
    });
    let done = completed_today(s, tz, now);
    Signals {
        today: today.format("%Y-%m-%d").to_string(),
        timezone: tz.name().to_owned(),
        today_open: today_tasks.len() as u32,
        overdue_open: overdue.len() as u32,
        completed_today: done,
        daily_goal: s.settings.daily_goal,
        goal_reached: s.settings.daily_goal > 0 && done >= s.settings.daily_goal,
        load_minutes: load,
        unestimated,
        today_tasks,
        overdue,
        routines,
        sync_status: s.sync.status.clone(),
        pending_edits: s.outbox.len() as u32,
        conflicts: s.conflicts.len() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_from_titles() {
        assert_eq!(minutes_in("15 min shadowing"), Some(15));
        assert_eq!(minutes_in("10 min anki/memrise"), Some(10));
        assert_eq!(minutes_in("20min speaking"), Some(20));
        assert_eq!(minutes_in("1h reading"), Some(60));
        assert_eq!(minutes_in("30 minuti di ripasso"), Some(30));
        assert_eq!(minutes_in("Brain Training"), None);
        assert_eq!(minutes_in("buy 2 apples"), None);
    }
}
