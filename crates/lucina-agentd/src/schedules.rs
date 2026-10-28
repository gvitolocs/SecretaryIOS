//! Schedule arithmetic (Paseo `server/schedule/cron.js` and the pure parts of
//! `schedule/service.js`): next-run times in a time zone, claiming a due run,
//! finishing it, completing a schedule, and repairing runs a restart interrupted.
//!
//! Every function takes `now` and changes one [`Schedule`] in place; the scheduler in
//! [`crate::manager`] does the I/O. Times are ISO strings with milliseconds, as
//! JavaScript's `toISOString()`.

use chrono::{DateTime, Datelike, Duration, SecondsFormat, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use lucina_agents::schedule::{parse_cron, Cadence, RunStatus, Schedule, ScheduleRun, ScheduleStatus, Target, DROPPED_RUNS_LABEL, MAX_RUNS_KEPT};

pub fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

/// Paseo `assertValidTimeZone` (Intl accepts names in any case).
pub fn time_zone(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .ok()
        .or_else(|| chrono_tz::TZ_VARIANTS.iter().copied().find(|t| t.name().eq_ignore_ascii_case(name)))
        .ok_or_else(|| format!("Invalid cron time zone: {name}"))
}

/// Paseo `validateScheduleCadence`.
pub fn validate(cadence: &Cadence) -> Result<(), String> {
    match cadence {
        Cadence::Every { every_ms } if *every_ms == 0 => Err("everyMs must be a positive integer".into()),
        Cadence::Every { .. } => Ok(()),
        Cadence::Cron { expression, timezone } => {
            parse_cron(expression)?;
            if let Some(tz) = timezone {
                time_zone(tz)?;
            }
            Ok(())
        }
    }
}

/// Paseo `computeNextRunAt`: the first whole minute after `after` that matches, read
/// in the cadence's time zone (UTC without one), within a year.
pub fn next_run_at(cadence: &Cadence, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let (expression, timezone) = match cadence {
        Cadence::Every { every_ms } => return Ok(after + Duration::milliseconds(*every_ms as i64)),
        Cadence::Cron { expression, timezone } => (expression, timezone),
    };
    let cron = parse_cron(expression)?;
    let tz = timezone.as_deref().map(time_zone).transpose()?;
    let minute_start = Utc
        .with_ymd_and_hms(after.year(), after.month(), after.day(), after.hour(), after.minute(), 0)
        .single()
        .ok_or("bad time")?;
    let mut cursor = minute_start + Duration::minutes(1);
    for _ in 0..366 * 24 * 60 {
        let (minute, hour, day, month, weekday) = match tz {
            None => (cursor.minute(), cursor.hour(), cursor.day(), cursor.month(), cursor.weekday().num_days_from_sunday()),
            Some(tz) => {
                let l = cursor.with_timezone(&tz);
                (l.minute(), l.hour(), l.day(), l.month(), l.weekday().num_days_from_sunday())
            }
        };
        if cron.minute.matches(minute)
            && cron.hour.matches(hour)
            && cron.day_of_month.matches(day)
            && cron.month.matches(month)
            && cron.day_of_week.matches(weekday)
        {
            return Ok(cursor);
        }
        cursor += Duration::minutes(1);
    }
    Err(format!("Unable to compute next run time for cron expression: {expression}"))
}

fn next_iso(cadence: &Cadence, after: DateTime<Utc>) -> Option<String> {
    next_run_at(cadence, after).ok().map(iso)
}

/// Paseo `advanceNextRunAtPast`.
fn advance_past(cadence: &Cadence, mut next: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    while next <= now {
        next = next_run_at(cadence, next).ok()?;
    }
    Some(next)
}

/// Paseo `shouldCompleteSchedule`.
pub fn should_complete(s: &Schedule, now: DateTime<Utc>) -> bool {
    if s.expires_at.as_deref().and_then(parse_iso).is_some_and(|e| e <= now) {
        return true;
    }
    s.max_runs.is_some_and(|m| s.completed_runs() >= m as u64)
}

/// Paseo `completeSchedule`.
pub fn complete(s: &mut Schedule, now: DateTime<Utc>) {
    s.status = ScheduleStatus::Completed;
    s.next_run_at = None;
    s.paused_at = None;
    s.updated_at = iso(now);
}

/// The first run time of a new schedule (Paseo `buildScheduleRecord`): now when it
/// runs on create (the default for `every`), else the next match.
pub fn first_run(cadence: &Cadence, run_on_create: Option<bool>, now: DateTime<Utc>) -> Result<String, String> {
    let run_now = run_on_create.unwrap_or(matches!(cadence, Cadence::Every { .. }));
    Ok(iso(if run_now { now } else { next_run_at(cadence, now)? }))
}

pub fn pause(s: &mut Schedule, now: DateTime<Utc>) -> Result<(), String> {
    match s.status {
        ScheduleStatus::Completed => Err(format!("Schedule {} is already completed", s.id)),
        ScheduleStatus::Paused => Ok(()),
        ScheduleStatus::Active => {
            s.status = ScheduleStatus::Paused;
            s.next_run_at = None;
            s.paused_at = Some(iso(now));
            s.updated_at = iso(now);
            Ok(())
        }
    }
}

pub fn resume(s: &mut Schedule, now: DateTime<Utc>) -> Result<(), String> {
    match s.status {
        ScheduleStatus::Completed => Err(format!("Schedule {} is already completed", s.id)),
        ScheduleStatus::Active => Ok(()),
        ScheduleStatus::Paused => {
            s.status = ScheduleStatus::Active;
            s.paused_at = None;
            s.next_run_at = Some(iso(next_run_at(&s.cadence, now)?));
            s.updated_at = iso(now);
            Ok(())
        }
    }
}

/// Paseo `mergeScheduleCadenceTimezone`: a new cron cadence without a zone keeps the old one.
pub fn merge_timezone(current: &Cadence, next: Cadence) -> Cadence {
    match (current, next) {
        (Cadence::Cron { timezone: Some(tz), .. }, Cadence::Cron { expression, timezone: None }) => {
            Cadence::Cron { expression, timezone: Some(tz.clone()) }
        }
        (_, next) => next,
    }
}

fn running_run(id: String, scheduled_for: String, now: DateTime<Utc>) -> ScheduleRun {
    ScheduleRun {
        id,
        scheduled_for,
        started_at: iso(now),
        ended_at: None,
        status: RunStatus::Running,
        agent_id: None,
        workspace_id: None,
        output: None,
        error: None,
    }
}

/// Paseo `claimDueRun`: take the due slot and move the cursor past `now` in the same
/// update. Returns the run id, or `None` when nothing is due.
pub fn claim_due(s: &mut Schedule, run_id: String, now: DateTime<Utc>) -> Option<String> {
    let due = s.next_run_at.as_deref().and_then(parse_iso)?;
    if s.status != ScheduleStatus::Active || should_complete(s, now) || due > now {
        return None;
    }
    let scheduled_for = s.next_run_at.clone()?;
    let is_final = s.max_runs.is_some_and(|m| s.completed_runs() + 1 >= m as u64);
    s.next_run_at = if is_final {
        None
    } else {
        next_run_at(&s.cadence, due).ok().and_then(|n| advance_past(&s.cadence, n, now)).map(iso)
    };
    s.updated_at = iso(now);
    s.runs.push(running_run(run_id.clone(), scheduled_for, now));
    Some(run_id)
}

/// Paseo `runOnce`'s run: recorded, the cadence untouched.
pub fn start_manual(s: &mut Schedule, run_id: String, now: DateTime<Utc>) {
    s.updated_at = iso(now);
    s.runs.push(running_run(run_id, iso(now), now));
}

/// How a run ended.
pub struct RunEnd {
    pub status: RunStatus,
    pub agent_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    /// The target is gone for good (Paseo `ScheduleTargetGoneError`): complete the schedule.
    pub target_gone: bool,
    pub manual: bool,
}

/// Paseo `finishRun`.
pub fn finish_run(s: &mut Schedule, run_id: &str, end: RunEnd, now: DateTime<Utc>) {
    let mut scheduled_for = None;
    for run in s.runs.iter_mut().filter(|r| r.id == run_id) {
        run.status = end.status;
        run.ended_at = Some(iso(now));
        if end.agent_id.is_some() {
            run.agent_id = end.agent_id.clone();
        }
        run.output = end.output.clone();
        run.error = end.error.clone();
        scheduled_for = parse_iso(&run.scheduled_for);
    }
    s.last_run_at = Some(iso(now));
    s.updated_at = iso(now);
    if end.target_gone {
        complete(s, now);
    } else if s.status == ScheduleStatus::Completed || end.manual {
        // Completed meanwhile, or a manual run: the cadence stays as it is.
    } else if should_complete(s, now) {
        complete(s, now);
    } else if s.status == ScheduleStatus::Paused {
        s.next_run_at = None;
    } else {
        let next = s
            .next_run_at
            .as_deref()
            .and_then(parse_iso)
            .or_else(|| scheduled_for.and_then(|f| next_run_at(&s.cadence, f).ok()));
        s.next_run_at = next.and_then(|n| advance_past(&s.cadence, n, now)).map(iso);
    }
    trim_runs(s);
}

/// Keep at most [`MAX_RUNS_KEPT`] runs; the dropped finished ones still count for `maxRuns`.
fn trim_runs(s: &mut Schedule) {
    while s.runs.len() > MAX_RUNS_KEPT {
        let Some(i) = s.runs.iter().position(|r| r.status != RunStatus::Running) else { break };
        s.runs.remove(i);
        let n = s.labels.get(DROPPED_RUNS_LABEL).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0) + 1;
        s.labels.insert(DROPPED_RUNS_LABEL.into(), n.to_string());
    }
}

/// Paseo `recoverInterruptedSchedule`: a run the restart cut off fails, and an overdue
/// cursor moves past `now` (missed fires are skipped, not replayed). Returns the
/// interrupted run's workspace to archive, if its schedule archives on finish.
pub fn recover(s: &mut Schedule, now: DateTime<Utc>) -> (bool, Option<String>) {
    let mut dirty = false;
    let mut archive = None;
    if let Some(run) = s.runs.iter_mut().find(|r| r.status == RunStatus::Running) {
        if let Target::NewAgent { config } = &s.target {
            if config.archive_on_finish.unwrap_or(true) || run.agent_id.is_none() {
                archive = run.workspace_id.clone();
            }
        }
        run.status = RunStatus::Failed;
        run.ended_at = Some(iso(now));
        run.error = Some("Daemon restarted before the scheduled run completed".into());
        if should_complete(s, now) {
            complete(s, now);
        }
        dirty = true;
    }
    if s.status == ScheduleStatus::Active {
        if let Some(due) = s.next_run_at.as_deref().and_then(parse_iso).filter(|d| *d <= now) {
            s.next_run_at = next_run_at(&s.cadence, due).ok().and_then(|n| advance_past(&s.cadence, n, now)).map(iso);
            dirty = true;
        }
    }
    if dirty {
        s.updated_at = iso(now);
    }
    (dirty, archive)
}

/// The next run after a cadence change (Paseo `update`).
pub fn next_after_update(s: &Schedule, now: DateTime<Utc>) -> Option<String> {
    (s.status == ScheduleStatus::Active).then(|| next_iso(&s.cadence, now)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn at(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    fn schedule(cadence: Cadence, max_runs: Option<u32>, now: DateTime<Utc>) -> Schedule {
        Schedule {
            id: "ab12cd34".into(),
            name: None,
            prompt: "p".into(),
            next_run_at: Some(first_run(&cadence, None, now).unwrap()),
            cadence,
            target: Target::Agent { agent_id: "a".into() },
            status: ScheduleStatus::Active,
            created_at: iso(now),
            updated_at: iso(now),
            last_run_at: None,
            paused_at: None,
            expires_at: None,
            max_runs,
            runs: Vec::new(),
            labels: BTreeMap::new(),
            created_by: "local".into(),
        }
    }

    fn end(manual: bool) -> RunEnd {
        RunEnd { status: RunStatus::Succeeded, agent_id: None, output: None, error: None, target_gone: false, manual }
    }

    #[test]
    fn claim_and_finish_follow_the_cadence() {
        let t0 = at("2026-10-28T10:00:30.000Z");
        let cron = Cadence::Cron { expression: "*/5 * * * *".into(), timezone: None };
        let mut s = schedule(cron, Some(2), t0);
        assert_eq!(s.next_run_at.as_deref(), Some("2026-10-28T10:05:00.000Z"));
        assert_eq!(claim_due(&mut s, "r0".into(), t0), None, "not due yet");
        // Late by 12 minutes: one run, and the cursor skips the missed slots.
        let late = at("2026-10-28T10:17:00.000Z");
        assert_eq!(claim_due(&mut s, "r1".into(), late).as_deref(), Some("r1"));
        assert_eq!(s.runs[0].scheduled_for, "2026-10-28T10:05:00.000Z");
        assert_eq!(s.next_run_at.as_deref(), Some("2026-10-28T10:20:00.000Z"));
        assert_eq!(claim_due(&mut s, "r2".into(), late), None, "one run at a time per slot");
        finish_run(&mut s, "r1", end(false), late);
        assert_eq!(s.status, ScheduleStatus::Active);
        // The second run is the last one (maxRuns 2): no successor, then completed.
        let t2 = at("2026-10-28T10:20:01.000Z");
        assert!(claim_due(&mut s, "r2".into(), t2).is_some());
        assert_eq!(s.next_run_at, None);
        finish_run(&mut s, "r2", end(false), t2);
        assert_eq!(s.status, ScheduleStatus::Completed);
    }

    #[test]
    fn every_runs_on_create_and_manual_runs_keep_the_cadence() {
        let t0 = at("2026-10-28T10:00:00.000Z");
        let mut s = schedule(Cadence::Every { every_ms: 60_000 }, None, t0);
        assert_eq!(s.next_run_at.as_deref(), Some("2026-10-28T10:00:00.000Z"));
        start_manual(&mut s, "m".into(), t0);
        finish_run(&mut s, "m", end(true), t0);
        assert_eq!(s.next_run_at.as_deref(), Some("2026-10-28T10:00:00.000Z"));
        assert_eq!(s.last_run_at.as_deref(), Some("2026-10-28T10:00:00.000Z"));
    }

    #[test]
    fn target_gone_completes_and_restart_fails_the_running_run() {
        let t0 = at("2026-10-28T10:00:00.000Z");
        let mut s = schedule(Cadence::Every { every_ms: 60_000 }, None, t0);
        claim_due(&mut s, "r1".into(), t0).unwrap();
        let (dirty, archive) = recover(&mut s, at("2026-10-28T11:00:00.500Z"));
        assert!(dirty && archive.is_none());
        assert_eq!(s.runs[0].status, RunStatus::Failed);
        assert_eq!(s.next_run_at.as_deref(), Some("2026-10-28T11:01:00.000Z"));
        claim_due(&mut s, "r2".into(), at("2026-10-28T11:01:00.000Z")).unwrap();
        finish_run(&mut s, "r2", RunEnd { target_gone: true, status: RunStatus::Failed, ..end(false) }, t0);
        assert_eq!(s.status, ScheduleStatus::Completed);
        assert_eq!(pause(&mut s, t0).unwrap_err(), "Schedule ab12cd34 is already completed");
    }

    #[test]
    fn history_is_capped_but_still_counts() {
        let t0 = at("2026-10-28T10:00:00.000Z");
        let mut s = schedule(Cadence::Every { every_ms: 1000 }, Some(250), t0);
        for i in 0..230 {
            let now = t0 + Duration::seconds(i);
            let id = format!("r{i}");
            claim_due(&mut s, id.clone(), now).unwrap();
            finish_run(&mut s, &id, end(false), now);
        }
        assert_eq!(s.runs.len(), MAX_RUNS_KEPT);
        assert_eq!(s.completed_runs(), 230);
    }

    #[test]
    fn time_zones() {
        assert!(time_zone("europe/rome").is_ok());
        assert_eq!(time_zone("Mars/Base").unwrap_err(), "Invalid cron time zone: Mars/Base");
        let rome = Cadence::Cron { expression: "0 7 * * *".into(), timezone: Some("Europe/Rome".into()) };
        assert_eq!(iso(next_run_at(&rome, at("2026-10-28T10:00:00.000Z")).unwrap()), "2026-10-29T06:00:00.000Z");
    }
}
