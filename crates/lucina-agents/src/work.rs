//! Work jobs: one delegated worker run that belongs to a lead. agentd starts a run
//! through the installed `lane` tool and follows it with `lane status`; this module
//! holds the pure rules (states, priorities, merging a status into a job, what counts
//! as an empty result, the per-lead summary). It does no IO and reads no clock.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who runs first when the host is busy: `Owner < High < Normal < Backlog`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Owner,
    High,
    #[default]
    Normal,
    Backlog,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Owner => "owner",
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Backlog => "backlog",
        }
    }
}

impl FromStr for Priority {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "owner" => Ok(Priority::Owner),
            "high" => Ok(Priority::High),
            "normal" => Ok(Priority::Normal),
            "backlog" => Ok(Priority::Backlog),
            other => Err(format!(
                "unknown priority \"{other}\": use owner, high, normal or backlog"
            )),
        }
    }
}

/// `Light`: the worker runs no build on the host (reading, reports, docs, small edits).
/// `Build`: it compiles or tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostClass {
    Light,
    #[default]
    Build,
}

impl CostClass {
    pub fn as_str(self) -> &'static str {
        match self {
            CostClass::Light => "light",
            CostClass::Build => "build",
        }
    }
}

impl FromStr for CostClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "light" => Ok(CostClass::Light),
            "build" => Ok(CostClass::Build),
            other => Err(format!(
                "unknown cost class \"{other}\": use light or build"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    #[default]
    Starting,
    Queued,
    Running,
    Done,
    Verified,
    VerifyFailed,
    Failed,
    Exhausted,
    DirtyAfterQuota,
    EndedDirty,
    EndedJobsQueued,
    /// The attempt ended ok but left nothing: no commit, no change, no expected file.
    EndedEmpty,
    /// `lane` has no record of the run any more.
    Lost,
}

impl WorkState {
    /// The state for a `lane` state string; `None` for one this code does not know.
    pub fn from_lane(state: &str) -> Option<WorkState> {
        Some(match state {
            "prepared" => WorkState::Starting,
            "queued" => WorkState::Queued,
            "running" => WorkState::Running,
            "reported_done" => WorkState::Done,
            "verified" => WorkState::Verified,
            "verify_failed" => WorkState::VerifyFailed,
            "failed" => WorkState::Failed,
            "all_exhausted" => WorkState::Exhausted,
            "dirty_after_quota" => WorkState::DirtyAfterQuota,
            "ended_dirty" => WorkState::EndedDirty,
            "ended_empty" => WorkState::EndedEmpty,
            "ended_with_jobs_queued" => WorkState::EndedJobsQueued,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WorkState::Starting => "starting",
            WorkState::Queued => "queued",
            WorkState::Running => "running",
            WorkState::Done => "done",
            WorkState::Verified => "verified",
            WorkState::VerifyFailed => "verify_failed",
            WorkState::Failed => "failed",
            WorkState::Exhausted => "exhausted",
            WorkState::DirtyAfterQuota => "dirty_after_quota",
            WorkState::EndedDirty => "ended_dirty",
            WorkState::EndedEmpty => "ended_empty",
            WorkState::EndedJobsQueued => "ended_jobs_queued",
            WorkState::Lost => "lost",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            WorkState::Starting | WorkState::Queued | WorkState::Running
        )
    }

    /// The exit code of `lagent work wait` for a final (or, at a timeout, current) state.
    pub fn wait_exit_code(self) -> i32 {
        match self {
            WorkState::Done | WorkState::Verified => 0,
            WorkState::Failed | WorkState::VerifyFailed | WorkState::Lost => 1,
            WorkState::Exhausted => 3,
            WorkState::DirtyAfterQuota => 4,
            WorkState::Starting | WorkState::Queued | WorkState::Running => 5,
            WorkState::EndedJobsQueued => 6,
            WorkState::EndedDirty => 7,
            WorkState::EndedEmpty => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkAttempt {
    #[serde(default)]
    pub lane: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkJob {
    pub id: String,
    pub lead: String,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub cost: CostClass,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub verify: Option<String>,
    /// A path relative to the workspace that the run must produce.
    #[serde(default)]
    pub expect: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub spec_path: String,
    /// RFC 3339.
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub base_head: Option<String>,
    #[serde(default)]
    pub state: WorkState,
    /// The raw `lane` state string.
    #[serde(default)]
    pub lane_state: Option<String>,
    #[serde(default)]
    pub final_lane: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub queue_position: Option<u64>,
    #[serde(default)]
    pub after_head: Option<String>,
    #[serde(default)]
    pub verify_exit: Option<i32>,
    /// The head `lane`'s verify step ran at.
    #[serde(default)]
    pub verify_head: Option<String>,
    #[serde(default)]
    pub dirty_files: u64,
    #[serde(default)]
    pub attempts: Vec<WorkAttempt>,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub empty: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
    /// A run that was started with `lane` directly and is only listed.
    #[serde(default)]
    pub direct: bool,
    /// The `before_head` of the first attempt that has one: where `lane` started the run.
    #[serde(default)]
    pub lane_base_head: Option<String>,
    /// Consecutive `lane status` answers that could not be read.
    #[serde(default)]
    pub status_errors: u32,
    /// The run id (`LA-R023`) agentd issued before it started the run; for a run started
    /// with `lane` directly, the one `lane` recorded.
    #[serde(default)]
    pub run_id: Option<String>,
    /// The change (`LA-C004`) the run works for.
    #[serde(default)]
    pub change: Option<String>,
    /// The chat that started the run.
    #[serde(default)]
    pub session: Option<String>,
    /// `lane` says the process that runs it is gone (`runner_lost`): resume or dismiss it with `lane`.
    #[serde(default)]
    pub runner_lost: bool,
}

/// What [`parse_ls`] read from `lane ls --json`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LsRows {
    pub rows: Vec<Value>,
    /// Non-blank lines that were not one JSON object.
    pub unparsed: usize,
    /// `line <n>: <error>` of the first such line.
    pub first_error: Option<String>,
}

/// `lane ls --json` prints JSON Lines: one object per line. Each non-blank line is read on
/// its own; blank lines are skipped; a line that is not a JSON object is counted, and the
/// error of the first one is kept with its 1-based line number.
pub fn parse_ls(text: &str) -> LsRows {
    let mut out = LsRows::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let error = match serde_json::from_str::<Value>(line) {
            Ok(v @ Value::Object(_)) => {
                out.rows.push(v);
                continue;
            }
            Ok(_) => "not a JSON object".to_string(),
            Err(e) => e.to_string(),
        };
        out.unparsed += 1;
        if out.first_error.is_none() {
            out.first_error = Some(format!("line {}: {error}", i + 1));
        }
    }
    out
}

/// The names of the fields of a `lane status` answer that the reader relies on and that are
/// absent as keys (a key with a null value is present).
pub fn missing_status_fields(status: &Value) -> Vec<&'static str> {
    const TOP: [&str; 12] = [
        "id",
        "state",
        "owner",
        "queue_position",
        "workspace",
        "final_lane",
        "model",
        "attempts",
        "verify",
        "dirty_files",
        "note",
        "updated",
    ];
    let mut out: Vec<&'static str> = TOP
        .into_iter()
        .filter(|k| status.get(*k).is_none())
        .collect();
    if let Some(list) = status.get("attempts").and_then(Value::as_array) {
        const PER: [(&str, &str); 5] = [
            ("lane", "attempts[].lane"),
            ("outcome", "attempts[].outcome"),
            ("reason", "attempts[].reason"),
            ("before_head", "attempts[].before_head"),
            ("after_head", "attempts[].after_head"),
        ];
        for (key, name) in PER {
            if list.iter().any(|a| a.get(key).is_none()) {
                out.push(name);
            }
        }
    }
    if let Some(v @ Value::Object(_)) = status.get("verify") {
        for (key, name) in [("exit_code", "verify.exit_code"), ("head", "verify.head")] {
            if v.get(key).is_none() {
                out.push(name);
            }
        }
    }
    out
}

/// The names of the fields of a `lane ls --json` row that the reader relies on and that are absent.
pub fn missing_ls_fields(row: &Value) -> Vec<&'static str> {
    [
        "id",
        "state",
        "owner",
        "queue_position",
        "category",
        "final_lane",
        "workspace",
        "updated",
    ]
    .into_iter()
    .filter(|k| row.get(*k).is_none())
    .collect()
}

const UNREADABLE_NOTE: &str = "lane status could not be read:";

/// Records a `lane status` answer that could not be read: a note and one more in
/// `status_errors`. The state never changes. Returns true when the job changed.
pub fn status_unreadable(job: &mut WorkJob, error: &str) -> bool {
    let before = job.clone();
    let cut: String = error.chars().take(160).collect();
    job.note = Some(format!("{UNREADABLE_NOTE} {cut}"));
    job.status_errors = job.status_errors.saturating_add(1);
    *job != before
}

fn str_field(v: &Value, key: &str) -> Option<Option<String>> {
    match v.get(key)? {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    }
}

/// Copies what `lane status` says into `job`. Fields the status lacks are left as they
/// are; unknown fields are ignored. `after_head` is the last attempt's non-empty
/// `after_head` (`lane` has no top-level one). Returns true when anything changed.
pub fn apply_status(job: &mut WorkJob, status: &Value) -> bool {
    let before = job.clone();
    // A readable answer ends a run of unreadable ones.
    job.status_errors = 0;
    if job
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with(UNREADABLE_NOTE))
    {
        job.note = None;
    }
    if let Some(raw) = status.get("state").and_then(Value::as_str) {
        job.lane_state = Some(raw.to_string());
        match WorkState::from_lane(raw) {
            Some(s) => {
                job.state = s;
                // An earlier unknown-state note no longer holds.
                if job
                    .note
                    .as_deref()
                    .is_some_and(|n| n.starts_with("unknown lane state:"))
                {
                    job.note = None;
                }
            }
            None => job.note = Some(format!("unknown lane state: {raw}")),
        }
    }
    match status.get("queue_position") {
        Some(Value::Null) => job.queue_position = None,
        Some(v) => {
            if let Some(n) = v.as_u64() {
                job.queue_position = Some(n);
            }
        }
        None => {}
    }
    if let Some(v) = str_field(status, "final_lane") {
        job.final_lane = v;
    }
    if let Some(v) = str_field(status, "model") {
        job.model = v;
    }
    if let Some(list) = status.get("attempts").and_then(Value::as_array) {
        let head = |a: &Value, key: &str| {
            a.get(key)
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .map(str::to_string)
        };
        job.after_head = list.iter().rev().find_map(|a| head(a, "after_head"));
        job.lane_base_head = list.iter().find_map(|a| head(a, "before_head"));
    }
    match status.get("verify") {
        Some(Value::Null) => {
            job.verify_exit = None;
            job.verify_head = None;
        }
        Some(v @ Value::Object(_)) => {
            job.verify_exit = v
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok());
            job.verify_head = v.get("head").and_then(Value::as_str).map(str::to_string);
        }
        _ => {}
    }
    if let Some(files) = status.get("dirty_files").and_then(Value::as_array) {
        job.dirty_files = files.len() as u64;
    }
    if let Some(list) = status.get("attempts").and_then(Value::as_array) {
        job.attempts = list
            .iter()
            .map(|a| WorkAttempt {
                lane: a
                    .get("lane")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                outcome: a
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                reason: a.get("reason").and_then(Value::as_str).map(str::to_string),
            })
            .collect();
    }
    if let Some(note) = status
        .get("note")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    {
        // An unknown state's note (set above) wins over the lane's own note.
        if !job
            .note
            .as_deref()
            .is_some_and(|n| n.starts_with("unknown lane state: "))
        {
            job.note = Some(note.to_string());
        }
    }
    if let Some(lost) = status.get("runner_lost").and_then(Value::as_bool) {
        job.runner_lost = lost;
    }
    // What `lane run` recorded of the ids it was given. A job agentd started keeps its own.
    let text = |key: &str| {
        status
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if job.lead.is_empty() {
        if let Some(lead) = text("lead") {
            job.lead = lead;
        }
    }
    if job.change.is_none() {
        job.change = text("change");
    }
    if let Some(run) = text("run") {
        match &job.run_id {
            None => job.run_id = Some(run),
            Some(own) if *own != run => {
                job.note = Some(format!("lane recorded run {run}, agentd issued {own}"));
            }
            Some(_) => {}
        }
    }
    *job != before
}

/// Whether a finished run produced nothing. With an expected file the file decides.
/// Without one, a run is empty when no file is dirty and no head other than `base_head`
/// shows up in `after_head` or `verify_head` (both absent counts as no new commit).
pub fn is_empty(
    expect_present: Option<bool>,
    base_head: Option<&str>,
    after_head: Option<&str>,
    verify_head: Option<&str>,
    dirty_files: u64,
) -> bool {
    if let Some(present) = expect_present {
        return !present;
    }
    let moved = |h: Option<&str>| h.is_some_and(|h| !h.is_empty() && Some(h) != base_head);
    dirty_files == 0 && !moved(after_head) && !moved(verify_head)
}

/// [`is_empty`] with a third answer: `None` when there is no evidence either way, that is
/// no expected file, nothing dirty and no head at all (a workspace that is not a git
/// repository gives none).
pub fn emptiness(
    expect_present: Option<bool>,
    base_head: Option<&str>,
    after_head: Option<&str>,
    verify_head: Option<&str>,
    dirty_files: u64,
) -> Option<bool> {
    if let Some(present) = expect_present {
        return Some(!present);
    }
    if dirty_files > 0 {
        return Some(false);
    }
    if base_head.is_none() && after_head.is_none() && verify_head.is_none() {
        return None;
    }
    Some(is_empty(
        None,
        base_head,
        after_head,
        verify_head,
        dirty_files,
    ))
}

/// A job for a `lane ls --json` row of a run agentd did not start.
pub fn from_direct(row: &Value) -> Option<WorkJob> {
    let id = row
        .get("id")?
        .as_str()
        .filter(|i| !i.is_empty())?
        .to_string();
    let text = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
    let owner = text("owner").unwrap_or_default();
    let updated = text("updated").unwrap_or_default();
    // A newer `lane` records the lead, run and change it was given.
    let given = |k: &str| text(k).filter(|s| !s.is_empty());
    let mut job = WorkJob {
        id,
        lead: given("lead").unwrap_or_else(|| format!("direct:{owner}")),
        run_id: given("run"),
        change: given("change"),
        runner_lost: row
            .get("runner_lost")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        workspace: text("workspace").unwrap_or_default(),
        lane: text("final_lane"),
        chain: text("category"),
        final_lane: text("final_lane"),
        created_at: updated.clone(),
        queue_position: row.get("queue_position").and_then(Value::as_u64),
        direct: true,
        ..WorkJob::default()
    };
    if let Some(raw) = text("state") {
        match WorkState::from_lane(&raw) {
            Some(s) => job.state = s,
            None => job.note = Some(format!("unknown lane state: {raw}")),
        }
        job.lane_state = Some(raw);
    }
    if job.state.is_terminal() {
        job.ended_at = Some(updated);
    }
    Some(job)
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeadSummary {
    pub lead: String,
    pub running: u64,
    pub queued: u64,
    pub ended: u64,
    pub empty: u64,
    /// Unfinished jobs whose runner `lane` lost.
    #[serde(default)]
    pub lost: u64,
    pub jobs: Vec<WorkJob>,
}

/// One entry per lead. Leads sort by name with the `direct:` ones last; inside a lead
/// the unfinished jobs come first, then by priority, then oldest first.
pub fn summarize(jobs: Vec<WorkJob>) -> Vec<LeadSummary> {
    let mut leads: Vec<LeadSummary> = Vec::new();
    for job in jobs {
        let at = match leads.iter().position(|l| l.lead == job.lead) {
            Some(i) => i,
            None => {
                leads.push(LeadSummary {
                    lead: job.lead.clone(),
                    ..LeadSummary::default()
                });
                leads.len() - 1
            }
        };
        let l = &mut leads[at];
        if job.runner_lost && !job.state.is_terminal() {
            l.lost += 1;
        }
        match job.state {
            WorkState::Running => l.running += 1,
            WorkState::Starting | WorkState::Queued => l.queued += 1,
            _ => {
                l.ended += 1;
                if job.empty == Some(true) {
                    l.empty += 1;
                }
            }
        }
        l.jobs.push(job);
    }
    for l in &mut leads {
        l.jobs.sort_by(|a, b| {
            (a.state.is_terminal(), a.priority, &a.created_at).cmp(&(
                b.state.is_terminal(),
                b.priority,
                &b.created_at,
            ))
        });
    }
    leads.sort_by(|a, b| {
        (a.lead.starts_with("direct:"), &a.lead).cmp(&(b.lead.starts_with("direct:"), &b.lead))
    });
    leads
}

/// Exactly one of a lane and a chain.
pub fn validate_target(lane: Option<&str>, chain: Option<&str>) -> Result<(), String> {
    match (lane.is_some(), chain.is_some()) {
        (true, false) | (false, true) => Ok(()),
        (true, true) => Err("give either a lane or a chain, not both".into()),
        (false, false) => Err("a lane or a chain is required".into()),
    }
}

/// `wk-` and 12 lowercase hex digits from the seed.
pub fn new_id(seed: u128) -> String {
    format!("wk-{:012x}", seed & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [WorkState; 13] = [
        WorkState::Starting,
        WorkState::Queued,
        WorkState::Running,
        WorkState::Done,
        WorkState::Verified,
        WorkState::VerifyFailed,
        WorkState::Failed,
        WorkState::Exhausted,
        WorkState::DirtyAfterQuota,
        WorkState::EndedDirty,
        WorkState::EndedJobsQueued,
        WorkState::EndedEmpty,
        WorkState::Lost,
    ];

    fn job(id: &str, lead: &str, state: WorkState, priority: Priority, created: &str) -> WorkJob {
        WorkJob {
            id: id.into(),
            lead: lead.into(),
            state,
            priority,
            created_at: created.into(),
            ..WorkJob::default()
        }
    }

    #[test]
    fn every_lane_state_maps() {
        let pairs = [
            ("prepared", WorkState::Starting),
            ("queued", WorkState::Queued),
            ("running", WorkState::Running),
            ("reported_done", WorkState::Done),
            ("verified", WorkState::Verified),
            ("verify_failed", WorkState::VerifyFailed),
            ("failed", WorkState::Failed),
            ("all_exhausted", WorkState::Exhausted),
            ("dirty_after_quota", WorkState::DirtyAfterQuota),
            ("ended_dirty", WorkState::EndedDirty),
            ("ended_empty", WorkState::EndedEmpty),
            ("ended_with_jobs_queued", WorkState::EndedJobsQueued),
        ];
        for (raw, want) in pairs {
            assert_eq!(WorkState::from_lane(raw), Some(want), "{raw}");
        }
        assert_eq!(WorkState::from_lane("sleeping"), None);
        assert_eq!(WorkState::from_lane(""), None);
        assert_eq!(WorkState::from_lane("lost"), None);
    }

    #[test]
    fn terminal_states_and_exit_codes() {
        let want = [
            (WorkState::Starting, false, 5),
            (WorkState::Queued, false, 5),
            (WorkState::Running, false, 5),
            (WorkState::Done, true, 0),
            (WorkState::Verified, true, 0),
            (WorkState::VerifyFailed, true, 1),
            (WorkState::Failed, true, 1),
            (WorkState::Exhausted, true, 3),
            (WorkState::DirtyAfterQuota, true, 4),
            (WorkState::EndedDirty, true, 7),
            (WorkState::EndedJobsQueued, true, 6),
            (WorkState::EndedEmpty, true, 8),
            (WorkState::Lost, true, 1),
        ];
        assert_eq!(want.len(), ALL.len());
        for (state, terminal, code) in want {
            assert_eq!(state.is_terminal(), terminal, "{state:?}");
            assert_eq!(state.wait_exit_code(), code, "{state:?}");
        }
    }

    #[test]
    fn state_names_in_json() {
        assert_eq!(
            serde_json::to_value(WorkState::VerifyFailed).unwrap(),
            json!("verify_failed")
        );
        assert_eq!(
            serde_json::to_value(WorkState::EndedJobsQueued).unwrap(),
            json!("ended_jobs_queued")
        );
        for s in ALL {
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_str()));
        }
    }

    #[test]
    fn priority_order_parse_and_default() {
        assert!(Priority::Owner < Priority::High);
        assert!(Priority::High < Priority::Normal);
        assert!(Priority::Normal < Priority::Backlog);
        assert_eq!(Priority::default(), Priority::Normal);
        for p in [
            Priority::Owner,
            Priority::High,
            Priority::Normal,
            Priority::Backlog,
        ] {
            assert_eq!(p.as_str().parse::<Priority>(), Ok(p));
        }
        assert!("urgent".parse::<Priority>().is_err());
        assert!("Owner".parse::<Priority>().is_err());
        assert_eq!(
            serde_json::to_value(Priority::Backlog).unwrap(),
            json!("backlog")
        );
        assert_eq!(CostClass::default(), CostClass::Build);
        assert_eq!("light".parse::<CostClass>(), Ok(CostClass::Light));
        assert!("heavy".parse::<CostClass>().is_err());
    }

    #[test]
    fn empty_rules() {
        // An expected file decides alone.
        assert!(!is_empty(Some(true), Some("a"), None, None, 0));
        assert!(is_empty(Some(false), Some("a"), Some("b"), Some("b"), 5));
        // No commit, nothing dirty.
        assert!(is_empty(None, Some("a"), None, None, 0));
        assert!(is_empty(None, Some("a"), Some("a"), Some("a"), 0));
        assert!(is_empty(None, None, None, None, 0));
        // A new commit through either head.
        assert!(!is_empty(None, Some("a"), Some("b"), None, 0));
        assert!(!is_empty(None, Some("a"), None, Some("c"), 0));
        assert!(!is_empty(None, None, Some("b"), None, 0));
        // Dirty files count.
        assert!(!is_empty(None, Some("a"), None, None, 2));
    }

    #[test]
    fn apply_full_status() {
        let mut j = job("wk-1", "chat", WorkState::Starting, Priority::Normal, "t");
        let status = json!({
            "state": "verified", "owner": "me", "queue_position": null, "category": "deep",
            "workspace": "/w", "final_lane": "qoder", "model": "m1", "session_id": "s",
            "verify": {"exit_code": 0, "head": "def"},
            "dirty_files": ["a", "b"],
            "attempts": [{"lane": "zen", "outcome": "quota", "reason": "limit"}, {"lane": "qoder", "outcome": "ok", "reason": null, "after_head": "abc"}],
            "note": "fine", "updated": "2026-11-17T10:00:00Z", "extra": 1
        });
        assert!(apply_status(&mut j, &status));
        assert_eq!(j.state, WorkState::Verified);
        assert_eq!(j.lane_state.as_deref(), Some("verified"));
        assert_eq!(j.queue_position, None);
        assert_eq!(j.final_lane.as_deref(), Some("qoder"));
        assert_eq!(j.model.as_deref(), Some("m1"));
        assert_eq!(j.after_head.as_deref(), Some("abc"));
        assert_eq!(j.verify_exit, Some(0));
        assert_eq!(j.verify_head.as_deref(), Some("def"));
        assert_eq!(j.dirty_files, 2);
        assert_eq!(j.attempts.len(), 2);
        assert_eq!(
            j.attempts[0],
            WorkAttempt {
                lane: "zen".into(),
                outcome: "quota".into(),
                reason: Some("limit".into())
            }
        );
        assert_eq!(j.attempts[1].reason, None);
        assert_eq!(j.note.as_deref(), Some("fine"));
        // The same status again changes nothing.
        assert!(!apply_status(&mut j, &status));
    }

    #[test]
    fn apply_status_with_missing_fields() {
        let mut j = job("wk-1", "chat", WorkState::Queued, Priority::Normal, "t");
        j.queue_position = Some(3);
        j.model = Some("m".into());
        assert!(!apply_status(&mut j, &json!({})));
        assert_eq!(j.state, WorkState::Queued);
        assert!(apply_status(&mut j, &json!({"state": "running"})));
        assert_eq!(j.state, WorkState::Running);
        assert_eq!(j.queue_position, Some(3));
        assert_eq!(j.model.as_deref(), Some("m"));
        assert!(apply_status(
            &mut j,
            &json!({"state": "running", "queue_position": 1})
        ));
        assert_eq!(j.queue_position, Some(1));
        // Wrongly typed fields are ignored, not an error.
        assert!(!apply_status(
            &mut j,
            &json!({"queue_position": "x", "dirty_files": 3, "attempts": "no"})
        ));
    }

    #[test]
    fn apply_status_with_unknown_state() {
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        assert!(apply_status(
            &mut j,
            &json!({"state": "hibernating", "note": "from lane"})
        ));
        assert_eq!(j.state, WorkState::Running);
        assert_eq!(j.lane_state.as_deref(), Some("hibernating"));
        assert_eq!(j.note.as_deref(), Some("unknown lane state: hibernating"));
    }

    #[test]
    fn an_unknown_state_note_goes_when_the_state_is_known_again() {
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        apply_status(&mut j, &json!({"state": "hibernating"}));
        assert!(apply_status(&mut j, &json!({"state": "running"})));
        assert_eq!(j.note, None);
        apply_status(&mut j, &json!({"state": "hibernating"}));
        assert!(apply_status(
            &mut j,
            &json!({"state": "verified", "note": "from lane"})
        ));
        assert_eq!(j.note.as_deref(), Some("from lane"));
        // Any other note stays.
        j.note = Some("mine".into());
        apply_status(&mut j, &json!({"state": "verified"}));
        assert_eq!(j.note.as_deref(), Some("mine"));
    }

    const LS: &str = include_str!("../tests/fixtures/lane/ls.jsonl");
    const STATUS_RUNNING: &str = include_str!("../tests/fixtures/lane/status-running.json");
    const STATUS_QUEUED: &str = include_str!("../tests/fixtures/lane/status-queued.json");
    const STATUS_DONE: &str = include_str!("../tests/fixtures/lane/status-reported_done.json");
    const STATUS_VERIFIED: &str = include_str!("../tests/fixtures/lane/status-verified.json");
    const STATUS_VERIFY_FAILED: &str =
        include_str!("../tests/fixtures/lane/status-verify_failed.json");
    const STATUS_FAILED: &str = include_str!("../tests/fixtures/lane/status-failed.json");
    const STATUS_EXHAUSTED: &str = include_str!("../tests/fixtures/lane/status-all_exhausted.json");
    const STATUS_DIRTY_QUOTA: &str =
        include_str!("../tests/fixtures/lane/status-dirty_after_quota.json");
    const STATUS_ENDED_DIRTY: &str = include_str!("../tests/fixtures/lane/status-ended_dirty.json");
    const STATUS_JOBS_QUEUED: &str =
        include_str!("../tests/fixtures/lane/status-ended_with_jobs_queued.json");

    #[test]
    fn ls_fixture_gives_ten_rows_and_maps_each_state() {
        let ls = parse_ls(LS);
        assert_eq!(ls.rows.len(), 10);
        assert_eq!(ls.unparsed, 0);
        assert_eq!(ls.first_error, None);
        for row in &ls.rows {
            let raw = row["state"].as_str().unwrap();
            let j = from_direct(row).unwrap();
            assert_eq!(Some(j.state), WorkState::from_lane(raw), "{raw}");
            assert_eq!(j.lane_state.as_deref(), Some(raw));
            assert!(j.direct);
        }
    }

    #[test]
    fn format_contract_holds_for_every_fixture() {
        let statuses = [
            ("status-running.json", STATUS_RUNNING),
            ("status-queued.json", STATUS_QUEUED),
            ("status-reported_done.json", STATUS_DONE),
            ("status-verified.json", STATUS_VERIFIED),
            ("status-verify_failed.json", STATUS_VERIFY_FAILED),
            ("status-failed.json", STATUS_FAILED),
            ("status-all_exhausted.json", STATUS_EXHAUSTED),
            ("status-dirty_after_quota.json", STATUS_DIRTY_QUOTA),
            ("status-ended_dirty.json", STATUS_ENDED_DIRTY),
            ("status-ended_with_jobs_queued.json", STATUS_JOBS_QUEUED),
        ];
        for (name, text) in statuses {
            let v: Value = serde_json::from_str(text).unwrap();
            let missing = missing_status_fields(&v);
            assert!(
                missing.is_empty(),
                "lane output lacks the field(s) {} in {name}: the reader in lucina-agents/src/work.rs must be updated and new samples captured",
                missing.join(", ")
            );
        }
        for (i, row) in parse_ls(LS).rows.iter().enumerate() {
            let missing = missing_ls_fields(row);
            assert!(
                missing.is_empty(),
                "lane output lacks the field(s) {} in ls.jsonl line {}: the reader in lucina-agents/src/work.rs must be updated and new samples captured",
                missing.join(", "),
                i + 1
            );
        }
    }

    #[test]
    fn a_removed_field_is_reported() {
        let mut v: Value = serde_json::from_str(STATUS_VERIFIED).unwrap();
        v.as_object_mut().unwrap().remove("dirty_files");
        assert_eq!(missing_status_fields(&v), ["dirty_files"]);
        let mut v: Value = serde_json::from_str(STATUS_VERIFIED).unwrap();
        v["attempts"][0]
            .as_object_mut()
            .unwrap()
            .remove("after_head");
        v["verify"].as_object_mut().unwrap().remove("head");
        assert_eq!(
            missing_status_fields(&v),
            ["attempts[].after_head", "verify.head"]
        );
        let mut row = parse_ls(LS).rows.remove(0);
        row.as_object_mut().unwrap().remove("category");
        assert_eq!(missing_ls_fields(&row), ["category"]);
    }

    #[test]
    fn ls_of_an_array_is_one_unparsed_line() {
        let ls = parse_ls("[{\"id\":\"a\"},{\"id\":\"b\"}]");
        assert!(ls.rows.is_empty());
        assert_eq!(ls.unparsed, 1);
        assert_eq!(ls.first_error.as_deref(), Some("line 1: not a JSON object"));
    }

    #[test]
    fn ls_with_a_broken_line_keeps_the_good_ones() {
        let ls = parse_ls("{\"id\":\"a\"}\n\n{\"id\":\n   \n{\"id\":\"c\"}\nnope\n");
        assert_eq!(ls.rows.len(), 2);
        assert_eq!(ls.unparsed, 2);
        let first = ls.first_error.unwrap();
        assert!(first.starts_with("line 3: "), "{first}");
    }

    #[test]
    fn ls_of_nothing() {
        for text in ["", "\n", "  \n \n"] {
            assert_eq!(parse_ls(text), LsRows::default());
        }
    }

    #[test]
    fn every_status_fixture_applies() {
        struct Want {
            text: &'static str,
            state: WorkState,
            final_lane: Option<&'static str>,
            model: Option<&'static str>,
            after: Option<&'static str>,
            base: Option<&'static str>,
            verify_exit: Option<i32>,
            verify_head: Option<&'static str>,
            dirty: u64,
            attempts: usize,
        }
        let head_a = "4cc7be8ea2af6e21cab12289bd4859352a73b399";
        let head_v = "4f2d631ea2edaf2305444ae262318217e749af8a";
        let head_x = "8166366d041d94bb3b6b62e5380df62507894e9a";
        let head_d = "f83c86dda64a899bdfe8f1bd6b6124203e1276f4";
        let wants = [
            Want {
                text: STATUS_RUNNING,
                state: WorkState::Running,
                final_lane: None,
                model: None,
                after: None,
                base: None,
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 0,
            },
            Want {
                text: STATUS_QUEUED,
                state: WorkState::Queued,
                final_lane: None,
                model: None,
                after: None,
                base: None,
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 0,
            },
            Want {
                text: STATUS_DONE,
                state: WorkState::Done,
                final_lane: Some("zen-glm-flash"),
                model: Some("glm-5.3-flash-free"),
                after: Some(head_a),
                base: Some(head_a),
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_VERIFIED,
                state: WorkState::Verified,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: None,
                base: None,
                verify_exit: Some(0),
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_VERIFY_FAILED,
                state: WorkState::VerifyFailed,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: Some(head_v),
                base: Some("a65030bfb78e8add4b65b78f94ee18a653e1e378"),
                verify_exit: Some(1),
                verify_head: Some(head_v),
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_FAILED,
                state: WorkState::Failed,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: None,
                base: None,
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_EXHAUSTED,
                state: WorkState::Exhausted,
                final_lane: None,
                model: None,
                after: Some(head_x),
                base: Some(head_x),
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_DIRTY_QUOTA,
                state: WorkState::DirtyAfterQuota,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: Some("675f80f0a0e90a70b4173e67dd74ec4ddf4ac8ee"),
                base: Some("825f1f98e980beba8314cc932a38fa3aa924f53c"),
                verify_exit: None,
                verify_head: None,
                dirty: 3,
                attempts: 1,
            },
            Want {
                text: STATUS_ENDED_DIRTY,
                state: WorkState::EndedDirty,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: Some(head_d),
                base: Some(head_d),
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
            Want {
                text: STATUS_JOBS_QUEUED,
                state: WorkState::EndedJobsQueued,
                final_lane: Some("qoder"),
                model: Some("auto-free"),
                after: None,
                base: None,
                verify_exit: None,
                verify_head: None,
                dirty: 0,
                attempts: 1,
            },
        ];
        for w in wants {
            let status: Value = serde_json::from_str(w.text).unwrap();
            let id = status["id"].as_str().unwrap().to_string();
            let mut j = job(&id, "chat", WorkState::Starting, Priority::Normal, "t");
            apply_status(&mut j, &status);
            assert_eq!(j.state, w.state, "{id}");
            assert_eq!(j.final_lane.as_deref(), w.final_lane, "{id}");
            assert_eq!(j.model.as_deref(), w.model, "{id}");
            assert_eq!(j.after_head.as_deref(), w.after, "{id}");
            assert_eq!(j.lane_base_head.as_deref(), w.base, "{id}");
            assert_eq!(j.verify_exit, w.verify_exit, "{id}");
            assert_eq!(j.verify_head.as_deref(), w.verify_head, "{id}");
            assert_eq!(j.dirty_files, w.dirty, "{id}");
            assert_eq!(j.attempts.len(), w.attempts, "{id}");
            assert_eq!(j.status_errors, 0, "{id}");
        }
    }

    #[test]
    fn the_last_attempt_with_an_after_head_counts() {
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        let status = json!({"state": "failed", "attempts": [
            {"lane": "a", "outcome": "quota", "before_head": "b1", "after_head": "a1"},
            {"lane": "b", "outcome": "failed", "before_head": null, "after_head": null},
            {"lane": "c", "outcome": "failed", "before_head": "", "after_head": ""}
        ], "after_head": "top-level-is-not-read"});
        apply_status(&mut j, &status);
        assert_eq!(j.after_head.as_deref(), Some("a1"));
        assert_eq!(j.lane_base_head.as_deref(), Some("b1"));
        // No attempts: nothing to read.
        apply_status(&mut j, &json!({"attempts": []}));
        assert_eq!(j.after_head, None);
        assert_eq!(j.lane_base_head, None);
    }

    #[test]
    fn empty_for_the_reported_done_fixture() {
        let status: Value = serde_json::from_str(STATUS_DONE).unwrap();
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        apply_status(&mut j, &status);
        let after = j.after_head.clone().unwrap();
        let empty = |base: &str| {
            is_empty(
                None,
                Some(base),
                j.after_head.as_deref(),
                j.verify_head.as_deref(),
                j.dirty_files,
            )
        };
        assert!(empty(&after));
        assert!(!empty("0000000000000000000000000000000000000000"));
    }

    #[test]
    fn an_ended_empty_status_ends_the_job_in_that_state() {
        let mut status: Value = serde_json::from_str(STATUS_DONE).unwrap();
        status["state"] = json!("ended_empty");
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        apply_status(&mut j, &status);
        assert_eq!(j.state, WorkState::EndedEmpty);
        assert_eq!(j.lane_state.as_deref(), Some("ended_empty"));
        assert_eq!(j.note, None);
        assert_eq!(serde_json::to_value(j.state).unwrap(), json!("ended_empty"));
    }

    #[test]
    fn emptiness_branches() {
        // An expected file decides alone.
        assert_eq!(emptiness(Some(true), None, None, None, 0), Some(false));
        assert_eq!(
            emptiness(Some(false), Some("a"), Some("b"), None, 5),
            Some(true)
        );
        // Dirty files: not empty.
        assert_eq!(emptiness(None, None, None, None, 2), Some(false));
        // No evidence at all: unknown.
        assert_eq!(emptiness(None, None, None, None, 0), None);
        // Any head makes it known.
        assert_eq!(emptiness(None, Some("a"), None, None, 0), Some(true));
        assert_eq!(emptiness(None, Some("a"), Some("a"), None, 0), Some(true));
        assert_eq!(emptiness(None, Some("a"), Some("b"), None, 0), Some(false));
        assert_eq!(emptiness(None, None, Some("b"), None, 0), Some(false));
        assert_eq!(emptiness(None, None, None, Some("c"), 0), Some(false));
    }

    #[test]
    fn a_fixture_without_heads_is_unknown_unless_a_file_decides() {
        let status: Value = serde_json::from_str(STATUS_VERIFIED).unwrap();
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        apply_status(&mut j, &status);
        let with = |expect: Option<bool>| {
            emptiness(
                expect,
                j.base_head.as_deref().or(j.lane_base_head.as_deref()),
                j.after_head.as_deref(),
                j.verify_head.as_deref(),
                j.dirty_files,
            )
        };
        assert_eq!(with(None), None);
        assert_eq!(with(Some(true)), Some(false));
        assert_eq!(with(Some(false)), Some(true));
    }

    #[test]
    fn an_unreadable_status_is_noted_and_a_readable_one_clears_it() {
        let mut j = job("wk-1", "chat", WorkState::Running, Priority::Normal, "t");
        assert!(status_unreadable(&mut j, "expected value at line 1"));
        assert_eq!(j.state, WorkState::Running);
        assert_eq!(j.status_errors, 1);
        assert_eq!(
            j.note.as_deref(),
            Some("lane status could not be read: expected value at line 1")
        );
        assert!(status_unreadable(&mut j, &"x".repeat(500)));
        assert_eq!(j.status_errors, 2);
        let note = j.note.clone().unwrap();
        assert_eq!(
            note.chars().count(),
            "lane status could not be read: ".len() + 160
        );
        assert!(apply_status(&mut j, &json!({"state": "running"})));
        assert_eq!(j.status_errors, 0);
        assert_eq!(j.note, None);
        // Lane's own note replaces the unreadable one; another note of ours stays.
        status_unreadable(&mut j, "e");
        apply_status(&mut j, &json!({"state": "running", "note": "from lane"}));
        assert_eq!(j.note.as_deref(), Some("from lane"));
        j.note = Some("mine".into());
        apply_status(&mut j, &json!({"state": "running"}));
        assert_eq!(j.note.as_deref(), Some("mine"));
    }

    #[test]
    fn direct_rows() {
        let row = json!({
            "id": "r1", "state": "running", "owner": "chat-9", "category": "deep", "final_lane": "qoder",
            "queue_position": 2, "workspace": "/w/x", "updated": "2026-11-17T10:00:00Z"
        });
        let j = from_direct(&row).unwrap();
        assert!(j.direct);
        assert_eq!(j.lead, "direct:chat-9");
        assert_eq!(j.id, "r1");
        assert_eq!(j.workspace, "/w/x");
        assert_eq!(j.state, WorkState::Running);
        assert_eq!(j.lane.as_deref(), Some("qoder"));
        assert_eq!(j.chain.as_deref(), Some("deep"));
        assert_eq!(j.priority, Priority::Normal);
        assert_eq!(j.cost, CostClass::Build);
        assert_eq!(j.queue_position, Some(2));
        assert_eq!(j.ended_at, None);
        let done =
            from_direct(&json!({"id": "r2", "state": "verified", "owner": "o", "updated": "u"}))
                .unwrap();
        assert_eq!(done.ended_at.as_deref(), Some("u"));
        assert!(from_direct(&json!({"state": "running"})).is_none());
    }

    #[test]
    fn summary_order_and_counts() {
        let mut empty = job(
            "e",
            "b-lead",
            WorkState::Done,
            Priority::Owner,
            "2026-01-01",
        );
        empty.empty = Some(true);
        let mut direct = job(
            "d",
            "direct:z",
            WorkState::Running,
            Priority::Normal,
            "2026-01-01",
        );
        direct.direct = true;
        let jobs = vec![
            job(
                "1",
                "b-lead",
                WorkState::Verified,
                Priority::Owner,
                "2026-01-01",
            ),
            job(
                "2",
                "b-lead",
                WorkState::Queued,
                Priority::Backlog,
                "2026-01-02",
            ),
            job(
                "3",
                "b-lead",
                WorkState::Running,
                Priority::Normal,
                "2026-01-03",
            ),
            job(
                "4",
                "b-lead",
                WorkState::Running,
                Priority::Normal,
                "2026-01-02",
            ),
            job(
                "5",
                "b-lead",
                WorkState::Starting,
                Priority::High,
                "2026-01-05",
            ),
            empty,
            direct,
            job(
                "6",
                "a-lead",
                WorkState::Failed,
                Priority::Normal,
                "2026-01-01",
            ),
            job(
                "7",
                "direct:a",
                WorkState::Queued,
                Priority::Normal,
                "2026-01-01",
            ),
        ];
        let out = summarize(jobs);
        let leads: Vec<&str> = out.iter().map(|l| l.lead.as_str()).collect();
        assert_eq!(leads, ["a-lead", "b-lead", "direct:a", "direct:z"]);
        let b = &out[1];
        assert_eq!((b.running, b.queued, b.ended, b.empty), (2, 2, 2, 1));
        let ids: Vec<&str> = b.jobs.iter().map(|j| j.id.as_str()).collect();
        // Unfinished first (High, then Normal oldest first, then Backlog), then finished by priority.
        assert_eq!(ids, ["5", "4", "3", "2", "1", "e"]);
        assert_eq!(out[0].ended, 1);
        assert_eq!(out[2].queued, 1);
        assert_eq!(out[3].running, 1);
    }

    #[test]
    fn targets() {
        assert!(validate_target(Some("qoder"), None).is_ok());
        assert!(validate_target(None, Some("deep")).is_ok());
        assert!(validate_target(Some("qoder"), Some("deep")).is_err());
        assert!(validate_target(None, None).is_err());
    }

    #[test]
    fn ids() {
        assert_eq!(new_id(0), "wk-000000000000");
        assert_eq!(new_id(0xABCDEF), "wk-000000abcdef");
        assert_eq!(new_id(u128::MAX), "wk-ffffffffffff");
        assert_eq!(new_id(7).len(), 15);
    }

    const STATUS_QUEUED_LOST: &str = include_str!("../tests/fixtures/lane/status-queued_lost.json");
    const LS_WITH_IDS: &str = include_str!("../tests/fixtures/lane/ls-with-ids.jsonl");

    #[test]
    fn a_queued_run_whose_runner_is_gone_is_marked_lost_and_takes_lanes_ids() {
        let status: Value = serde_json::from_str(STATUS_QUEUED_LOST).unwrap();
        // Older runs lack these fields: the reader does not need them.
        assert!(missing_status_fields(&status).is_empty());
        let mut older = status.clone();
        for k in ["runner_lost", "lead", "run", "change"] {
            older.as_object_mut().unwrap().remove(k);
        }
        assert!(missing_status_fields(&older).is_empty());

        // A job without ids of its own takes lane's.
        let mut j = WorkJob {
            id: "pk-r018-pre-push".into(),
            ..WorkJob::default()
        };
        assert!(apply_status(&mut j, &status));
        assert_eq!(j.state, WorkState::Queued);
        assert!(j.runner_lost);
        assert_eq!(j.lead, "PK");
        assert_eq!(j.run_id.as_deref(), Some("PK-R018"));
        assert_eq!(j.change.as_deref(), Some("PK-C008"));
        assert_eq!(j.note, None);
        // The same status again changes nothing.
        assert!(!apply_status(&mut j, &status));

        // A job agentd started keeps its own ids; a run that differs is noted, the job's wins.
        let mut own = WorkJob {
            id: "la-r001-x".into(),
            lead: "LA".into(),
            run_id: Some("LA-R001".into()),
            change: Some("LA-C001".into()),
            ..WorkJob::default()
        };
        assert!(apply_status(&mut own, &status));
        assert_eq!(own.lead, "LA");
        assert_eq!(own.run_id.as_deref(), Some("LA-R001"));
        assert_eq!(own.change.as_deref(), Some("LA-C001"));
        assert_eq!(
            own.note.as_deref(),
            Some("lane recorded run PK-R018, agentd issued LA-R001")
        );
        assert!(own.runner_lost);
        assert!(!apply_status(&mut own, &status));
        // The same run: no note.
        let mut same = WorkJob {
            id: "pk-r018-pre-push".into(),
            lead: "PK".into(),
            run_id: Some("PK-R018".into()),
            ..WorkJob::default()
        };
        apply_status(&mut same, &status);
        assert_eq!(same.note, None);
        assert_eq!(same.change.as_deref(), Some("PK-C008"));
        // A status that no longer says the runner is lost clears it.
        let mut back = status.clone();
        back["runner_lost"] = json!(false);
        assert!(apply_status(&mut j, &back));
        assert!(!j.runner_lost);
        // A status without the fields leaves a job's ids alone.
        apply_status(&mut j, &older);
        assert_eq!(j.run_id.as_deref(), Some("PK-R018"));
    }

    #[test]
    fn listing_rows_with_ids_give_the_lead_instead_of_direct() {
        let ls = parse_ls(LS_WITH_IDS);
        assert_eq!(ls.rows.len(), 2);
        let a = from_direct(&ls.rows[0]).unwrap();
        assert!(a.direct);
        assert_eq!(a.lead, "PK");
        assert_eq!(a.run_id.as_deref(), Some("PK-R018"));
        assert_eq!(a.change.as_deref(), Some("PK-C008"));
        assert_eq!(a.state, WorkState::Queued);
        let b = from_direct(&ls.rows[1]).unwrap();
        assert_eq!(b.lead, "PK");
        assert_eq!(b.run_id.as_deref(), Some("PK-R021"));
        assert_eq!(b.change.as_deref(), Some("PK-C002"));
        assert_eq!(b.state, WorkState::Running);
        // Rows without the fields behave as before.
        for row in &parse_ls(LS).rows {
            let j = from_direct(row).unwrap();
            assert!(j.lead.starts_with("direct:"), "{}", j.lead);
            assert_eq!((j.run_id, j.change), (None, None));
        }
        let lost =
            from_direct(&json!({"id": "r", "state": "queued", "owner": "o", "runner_lost": true}))
                .unwrap();
        assert!(lost.runner_lost);
        assert_eq!(lost.lead, "direct:o");
    }

    #[test]
    fn lost_runners_are_counted_per_lead() {
        let mut a = job("1", "PK", WorkState::Queued, Priority::Normal, "t");
        a.runner_lost = true;
        let mut b = job("2", "PK", WorkState::Running, Priority::Normal, "t");
        b.runner_lost = true;
        let c = job("3", "PK", WorkState::Running, Priority::Normal, "t");
        // A finished job is not counted even when the flag is still set.
        let mut d = job("4", "PK", WorkState::Failed, Priority::Normal, "t");
        d.runner_lost = true;
        let out = summarize(vec![a, b, c, d]);
        assert_eq!(out[0].lost, 2);
        assert_eq!((out[0].running, out[0].queued, out[0].ended), (2, 1, 1));
    }

    #[test]
    fn stored_jobs_without_the_new_fields_still_load() {
        let old = r#"{"id":"wk-1","lead":"chat","state":"running"}"#;
        let job: WorkJob = serde_json::from_str(old).unwrap();
        assert_eq!(job.run_id, None);
        assert_eq!(job.change, None);
        assert_eq!(job.session, None);
        assert!(!job.runner_lost);
    }
}
