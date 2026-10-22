//! Tasks: a Todoist-compatible task engine on the gym/body command protocol
//! (`{op_id, expected_revision, timezone, command}` in, `{ok, replayed,
//! code?, error?, view}` out; op_id receipts, atomic 0600 files).
//!
//! - [`models`]: the stored state (tasks, projects, sections, labels,
//!   filters, completed history, the Todoist outbox) and the wire types.
//! - [`nlp`]: quick add and natural-language dates (it/en/da), recurrence.
//! - [`views`]: inbox, today (with overdue), upcoming, project, label,
//!   filter, search and completed views, sidebar counts, the daily goal.
//! - [`sync`]: what the server's Todoist worker calls: the queued commands
//!   to push, and the merge of `/api/v1/sync` answers (temp ids, conflicts:
//!   Todoist wins, with a visible note).
//!
//! Every local edit is applied at once (optimistic) and queued as a Todoist
//! command; the server pushes the queue and pulls changes. Without a
//! Todoist account the engine still works locally and the queue waits.

pub mod filter;
pub mod models;
pub mod nlp;
pub mod signals;
pub mod sync;
pub mod views;
pub use models::*;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_INPUT: usize = 256 * 1024;
pub const MAX_CONTENT: usize = 500;
pub const MAX_DESCRIPTION: usize = 16_000;
pub const MAX_LABELS: usize = 50;
pub const MAX_TASKS: usize = 50_000;
pub const MAX_RECEIPTS: usize = 100_000;
pub const MAX_OUTBOX: usize = 5_000;
pub const MAX_COMPLETED: usize = 2_000;
pub const MAX_TRASH: usize = 500;
pub const MAX_CONFLICTS: usize = 100;
/// A soft delete waits this long before it is sent to Todoist.
pub const UNDO_MS: u64 = 20_000;
pub const DAY_MS: u64 = 86_400_000;
const MAX_TIME: u64 = 253_402_300_799_000;
type Result<T> = std::result::Result<T, String>;

pub struct Engine {
    state: State,
    path: Option<PathBuf>,
    _lock: Option<File>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(MAX_TIME as u128) as u64
}

pub fn ms_to_utc(ms: u64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ms as i64).unwrap_or_default()
}

pub fn today_in(tz: Tz, now: u64) -> NaiveDate {
    ms_to_utc(now).with_timezone(&tz).date_naive()
}

/// A due date as a local date (and time) in `tz`.
pub fn due_local(due: &Due, tz: Tz) -> Option<(NaiveDate, Option<NaiveTime>)> {
    let d = due.date.trim();
    if d.len() == 10 {
        return NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .ok()
            .map(|x| (x, None));
    }
    if let Some(utc) = d.strip_suffix('Z') {
        let t = NaiveDateTime::parse_from_str(utc, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        let local = Utc.from_utc_datetime(&t).with_timezone(&tz);
        return Some((local.date_naive(), Some(local.time())));
    }
    let t = NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M"))
        .ok()?;
    Some((t.date(), Some(t.time())))
}

/// A new date for an existing due, keeping its time (floating or fixed).
pub fn moved_due_date(due: &Due, date: NaiveDate, tz: Tz) -> String {
    let d = due.date.trim();
    if d.len() == 10 {
        return date.format("%Y-%m-%d").to_string();
    }
    if d.ends_with('Z') {
        let zone: Tz = due
            .timezone
            .as_deref()
            .and_then(|z| z.parse().ok())
            .unwrap_or(tz);
        let time = due_local(due, zone)
            .and_then(|(_, t)| t)
            .unwrap_or_default();
        let local = zone
            .from_local_datetime(&date.and_time(time))
            .earliest()
            .map(|x| x.with_timezone(&Utc));
        return match local {
            Some(u) => u.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
            None => date.format("%Y-%m-%d").to_string(),
        };
    }
    let time = due_local(due, tz).and_then(|(_, t)| t).unwrap_or_default();
    date.and_time(time)
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

impl Engine {
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn in_memory() -> Self {
        Self {
            state: State::initial(),
            path: None,
            _lock: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "This tasks data file is already open in another process".to_string())?;
        let state = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            serde_json::from_slice::<State>(&bytes)
                .map_err(|e| format!("Saved data could not be read: {e}"))?
        } else {
            State::initial()
        };
        validate(&state)?;
        Ok(Self {
            state,
            path: Some(path),
            _lock: Some(lock),
        })
    }

    pub fn handle(&mut self, json: &str) -> Response {
        self.handle_at(json, now_ms())
    }

    pub fn handle_at(&mut self, json: &str, now: u64) -> Response {
        match self.process(json, now) {
            Ok(r) => r,
            Err((code, error)) => Response {
                ok: false,
                replayed: false,
                error: Some(error),
                code: Some(code.into()),
                ..Default::default()
            },
        }
    }

    fn process(
        &mut self,
        json: &str,
        now: u64,
    ) -> std::result::Result<Response, (&'static str, String)> {
        let invalid = |e: String| ("invalid_command", e);
        if json.len() > MAX_INPUT || now > MAX_TIME {
            return Err(invalid(
                "Request or timestamp exceeds supported bounds".into(),
            ));
        }
        let req: Request = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
        let tz: Tz = req
            .timezone
            .parse()
            .map_err(|_| invalid("Unknown IANA timezone".into()))?;
        let mut out = Response {
            ok: true,
            ..Default::default()
        };
        let mut view_query = None;
        match &req.command {
            Command::Snapshot { view } => view_query = view.clone(),
            Command::ParseQuickAdd { text, lang } => {
                out.parsed = Some(self.parse_quick(text, lang.as_deref(), tz, now));
            }
            _ => {
                let op = req
                    .op_id
                    .clone()
                    .filter(|id| !id.trim().is_empty() && id.len() <= 128)
                    .ok_or_else(|| {
                        invalid("A nonempty op_id (at most 128 bytes) is required".into())
                    })?;
                let fingerprint = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&req.command).map_err(|e| invalid(e.to_string()))?
                    )
                );
                if let Some(previous) = self.state.receipts.get(&op) {
                    if previous != &fingerprint {
                        return Err((
                            "op_id_reused",
                            "This operation ID was already used for a different command".into(),
                        ));
                    }
                    out.replayed = true;
                } else {
                    if req
                        .expected_revision
                        .is_some_and(|r| r != self.state.revision)
                    {
                        return Err((
                            "conflict",
                            "State changed. Refresh and retry the edit.".into(),
                        ));
                    }
                    // Rejection never mutates live state: only a fully
                    // validated, durably written candidate becomes the state.
                    let mut candidate = self.state.clone();
                    let outcome =
                        apply(&mut candidate, req.command.clone(), now, tz).map_err(invalid)?;
                    candidate.revision = self
                        .state
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| invalid("Revision limit reached".into()))?;
                    candidate.receipts.insert(op, fingerprint);
                    validate(&candidate).map_err(invalid)?;
                    self.persist(&candidate).map_err(|e| ("storage_error", e))?;
                    self.state = candidate;
                    out.task_id = outcome.task_id;
                    out.parsed = outcome.parsed;
                    out.queued = outcome.queued;
                }
            }
        }
        out.view = Some(views::view(
            &self.state,
            view_query.as_ref().unwrap_or(&ViewQuery::default()),
            tz,
            now,
        ));
        Ok(out)
    }

    fn parse_quick(&self, text: &str, lang: Option<&str>, tz: Tz, now: u64) -> Parsed {
        let ctx = nlp::Ctx {
            today: today_in(tz, now),
            next_week: self.state.settings.next_week,
        };
        let names: Vec<String> = self
            .state
            .projects
            .values()
            .map(|p| p.name.clone())
            .collect();
        nlp::quick_add(text, lang.unwrap_or(&self.state.settings.lang), ctx, &names)
    }

    /// Swap in `candidate` (validated, persisted). The revision moves only
    /// when something a client sees changed.
    pub(crate) fn commit(&mut self, mut candidate: State) -> Result<bool> {
        let changed = !sync::same_content(&self.state, &candidate);
        if changed {
            candidate.revision = self
                .state
                .revision
                .checked_add(1)
                .ok_or("Revision limit reached")?;
        } else {
            candidate.revision = self.state.revision;
        }
        validate(&candidate)?;
        if candidate != self.state {
            self.persist(&candidate)?;
            self.state = candidate;
        }
        Ok(changed)
    }

    fn persist(&self, state: &State) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
            serde_json::to_writer(&mut file, state).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            if let Some(parent) = path.parent() {
                if let Ok(dir) = File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

// -- commands ------------------------------------------------------------------------

#[derive(Default)]
struct Outcome {
    task_id: Option<String>,
    parsed: Option<Parsed>,
    queued: bool,
}

fn text(field: &str, v: &str, max: usize) -> Result<String> {
    let t = v.trim();
    if t.is_empty() {
        return Err(format!("{field} is required"));
    }
    if t.chars().count() > max {
        return Err(format!("{field} is limited to {max} characters"));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err(format!("{field}: no control characters"));
    }
    Ok(t.to_owned())
}

fn description(v: &str) -> Result<String> {
    if v.chars().count() > MAX_DESCRIPTION {
        return Err(format!(
            "description is limited to {MAX_DESCRIPTION} characters"
        ));
    }
    Ok(v.trim().to_owned())
}

fn labels_field(v: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in v {
        let l = text("label", l.trim_start_matches('@'), 60)?;
        if l.contains(char::is_whitespace) {
            return Err("labels cannot contain spaces".into());
        }
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&l)) {
            out.push(l);
        }
    }
    if out.len() > MAX_LABELS {
        return Err(format!("at most {MAX_LABELS} labels"));
    }
    Ok(out)
}

fn priority_field(p: u8) -> Result<u8> {
    if !(1..=4).contains(&p) {
        return Err("priority must be 1 (urgent) to 4".into());
    }
    Ok(5 - p)
}

fn ctx_for(s: &State, tz: Tz, now: u64) -> nlp::Ctx {
    nlp::Ctx {
        today: today_in(tz, now),
        next_week: s.settings.next_week,
    }
}

/// A due from a phrase (kept verbatim for Todoist) or an explicit date.
fn due_from(
    s: &State,
    string: Option<&str>,
    date: Option<&str>,
    lang: Option<&str>,
    tz: Tz,
    now: u64,
) -> Result<Option<(Due, Value)>> {
    match (string.map(str::trim).filter(|x| !x.is_empty()), date) {
        (Some(_), Some(_)) => Err("give due_string or due_date, not both".into()),
        (Some(phrase), None) => {
            if phrase.chars().count() > 200 {
                return Err("due_string is limited to 200 characters".into());
            }
            let m = nlp::parse_due(phrase, ctx_for(s, tz, now));
            let lang = lang
                .map(str::to_owned)
                .or_else(|| m.as_ref().and_then(|m| m.lang).map(str::to_owned))
                .unwrap_or_else(|| s.settings.lang.clone());
            let date = match &m {
                Some(m) => match m.time {
                    Some(t) => m
                        .date
                        .and_time(t)
                        .format("%Y-%m-%dT%H:%M:%S%.6f")
                        .to_string(),
                    None => m.date.format("%Y-%m-%d").to_string(),
                },
                // Todoist knows more phrases than this engine: keep it, the
                // sync fills the date in.
                None => String::new(),
            };
            let due = Due {
                date,
                string: phrase.to_owned(),
                lang: lang.clone(),
                is_recurring: m.as_ref().is_some_and(|m| m.recurrence.is_some()),
                timezone: None,
            };
            Ok(Some((due, json!({"string": phrase, "lang": lang}))))
        }
        (None, Some(d)) => {
            let d = d.trim();
            let date = if let Ok(x) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                x.format("%Y-%m-%d").to_string()
            } else if let Ok(x) = NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M")
                .or_else(|_| NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S"))
            {
                x.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
            } else {
                return Err("due_date must be YYYY-MM-DD or YYYY-MM-DDTHH:MM".into());
            };
            let due = Due {
                date: date.clone(),
                string: d.to_owned(),
                lang: s.settings.lang.clone(),
                is_recurring: false,
                timezone: None,
            };
            Ok(Some((due, json!({"date": date}))))
        }
        (None, None) => Ok(None),
    }
}

fn queue<'a>(
    s: &'a mut State,
    kind: &str,
    args: Value,
    task_id: Option<&str>,
    temp_id: Option<String>,
    now: u64,
    not_before: u64,
) -> &'a mut Op {
    let base = task_id
        .and_then(|id| s.tasks.get(id))
        .and_then(|t| t.updated_at.clone());
    s.outbox.push(Op {
        uuid: Uuid::new_v4().to_string(),
        kind: kind.into(),
        args,
        temp_id,
        task_id: task_id.map(str::to_owned),
        base_updated_at: base,
        created_ms: now,
        not_before_ms: not_before,
        attempts: 0,
        last_error: None,
        expect_recurring: false,
    });
    s.outbox.last_mut().expect("just pushed")
}

fn open_task<'a>(s: &'a State, id: &str) -> Result<&'a Task> {
    match s.tasks.get(id) {
        Some(t) if t.deleted_ms.is_none() && !t.checked => Ok(t),
        _ => Err("Task not found".into()),
    }
}

/// `id` and every task below it.
pub fn descendants(s: &State, id: &str) -> Vec<String> {
    let mut out = vec![id.to_owned()];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].clone();
        out.extend(
            s.tasks
                .values()
                .filter(|t| t.parent_id.as_deref() == Some(parent.as_str()))
                .map(|t| t.id.clone()),
        );
        i += 1;
    }
    out
}

fn project_exists(s: &State, id: &str) -> bool {
    s.projects.contains_key(id) || (id.is_empty() && s.inbox_id().is_none())
}

struct NewTask {
    content: String,
    description: String,
    project_id: Option<String>,
    section_id: Option<String>,
    parent_id: Option<String>,
    due: Option<(Due, Value)>,
    priority: Option<u8>,
    labels: Vec<String>,
    deadline: Option<String>,
}

fn add_task(s: &mut State, n: NewTask, now: u64) -> Result<String> {
    let mut project_id = n.project_id.unwrap_or_default();
    let mut section_id = n.section_id;
    if let Some(parent) = &n.parent_id {
        let p = open_task(s, parent)?;
        project_id = p.project_id.clone();
        section_id = p.section_id.clone();
    }
    if !project_id.is_empty() && !project_exists(s, &project_id) {
        return Err("Project not found".into());
    }
    if let Some(sec) = &section_id {
        match s.sections.get(sec) {
            Some(x) if project_id.is_empty() || x.project_id == project_id => {
                project_id = x.project_id.clone();
            }
            _ => return Err("Section not found in that project".into()),
        }
    }
    let local_project = if project_id.is_empty() {
        s.inbox_id().map(str::to_owned).unwrap_or_default()
    } else {
        project_id.clone()
    };
    let id = format!("{TEMP_PREFIX}{}", Uuid::new_v4());
    let child_order = s
        .tasks
        .values()
        .filter(|t| t.project_id == local_project && t.parent_id == n.parent_id)
        .map(|t| t.child_order)
        .max()
        .unwrap_or(0)
        + 1;
    let mut args = Map::new();
    args.insert("content".into(), json!(n.content));
    if !n.description.is_empty() {
        args.insert("description".into(), json!(n.description));
    }
    if !project_id.is_empty() && n.parent_id.is_none() {
        args.insert("project_id".into(), json!(project_id));
    }
    if let Some(sec) = &section_id {
        if n.parent_id.is_none() {
            args.insert("section_id".into(), json!(sec));
        }
    }
    if let Some(p) = &n.parent_id {
        args.insert("parent_id".into(), json!(p));
    }
    if let Some((_, wire)) = &n.due {
        args.insert("due".into(), wire.clone());
    }
    if let Some(p) = n.priority {
        args.insert("priority".into(), json!(p));
    }
    if !n.labels.is_empty() {
        args.insert("labels".into(), json!(n.labels));
    }
    if let Some(d) = &n.deadline {
        args.insert("deadline".into(), json!({"date": d}));
    }
    s.tasks.insert(
        id.clone(),
        Task {
            id: id.clone(),
            project_id: local_project,
            section_id,
            parent_id: n.parent_id,
            content: n.content,
            description: n.description,
            priority: n.priority.unwrap_or(1),
            due: n.due.map(|(d, _)| d),
            deadline: n.deadline,
            labels: n.labels,
            child_order,
            day_order: -1,
            added_at: Some(ms_to_utc(now).to_rfc3339()),
            ..Task::default()
        },
    );
    queue(
        s,
        "item_add",
        Value::Object(args),
        None,
        Some(id.clone()),
        now,
        0,
    )
    .task_id = Some(id.clone());
    Ok(id)
}

pub(crate) fn push_conflict(
    s: &mut State,
    task_id: Option<&str>,
    content: &str,
    kind: &str,
    message: &str,
    now: u64,
) {
    s.conflicts.push(Conflict {
        id: format!("c_{}", Uuid::new_v4().simple()),
        task_id: task_id.map(str::to_owned),
        content: content.to_owned(),
        kind: kind.into(),
        message: message.into(),
        at_ms: now,
    });
    if s.conflicts.len() > MAX_CONFLICTS {
        let drop = s.conflicts.len() - MAX_CONFLICTS;
        s.conflicts.drain(..drop);
    }
}

fn record_completion(s: &mut State, c: Completed) {
    s.completed.push(c);
    if s.completed.len() > MAX_COMPLETED {
        let drop = s.completed.len() - MAX_COMPLETED;
        s.completed.drain(..drop);
    }
}

fn reschedule(s: &mut State, id: &str, date: NaiveDate, tz: Tz, now: u64) -> Result<()> {
    let t = open_task(s, id)?.clone();
    let mut due = t.due.clone().unwrap_or(Due {
        date: String::new(),
        string: String::new(),
        lang: s.settings.lang.clone(),
        is_recurring: false,
        timezone: None,
    });
    let new_date = if due.date.is_empty() {
        date.format("%Y-%m-%d").to_string()
    } else {
        moved_due_date(&due, date, tz)
    };
    let wire = if due.is_recurring {
        // Keep the recurrence: Todoist takes the new current occurrence
        // with the same recurring string.
        json!({"date": new_date, "string": due.string, "lang": due.lang, "is_recurring": true})
    } else {
        due.string = date.format("%Y-%m-%d").to_string();
        json!({"date": new_date})
    };
    due.date = new_date;
    let recurring = due.is_recurring;
    if let Some(x) = s.tasks.get_mut(id) {
        x.due = Some(due);
        x.provisional = false;
    }
    queue(
        s,
        "item_update",
        json!({"id": id, "due": wire}),
        Some(id),
        None,
        now,
        0,
    )
    .expect_recurring = recurring;
    Ok(())
}

fn date_or_phrase(s: &State, v: &str, tz: Tz, now: u64) -> Result<NaiveDate> {
    let v = v.trim();
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        return Ok(d);
    }
    match nlp::parse_due(v, ctx_for(s, tz, now)) {
        Some(m) if m.recurrence.is_none() => Ok(m.date),
        _ => Err("date must be YYYY-MM-DD or a phrase such as tomorrow / next week".into()),
    }
}

fn apply(s: &mut State, command: Command, now: u64, tz: Tz) -> Result<Outcome> {
    let mut out = Outcome {
        queued: true,
        ..Outcome::default()
    };
    if s.outbox.len() >= MAX_OUTBOX {
        return Err("Too many edits are waiting for Todoist; reconnect Todoist in Links".into());
    }
    match command {
        Command::Snapshot { .. } | Command::ParseQuickAdd { .. } => {
            return Err("Queries cannot mutate state".into())
        }
        Command::Add {
            content,
            description: desc,
            project_id,
            section_id,
            parent_id,
            due_string,
            due_date,
            lang,
            priority,
            labels,
        } => {
            let due = due_from(
                s,
                due_string.as_deref(),
                due_date.as_deref(),
                lang.as_deref(),
                tz,
                now,
            )?;
            let n = NewTask {
                content: text("content", &content, MAX_CONTENT)?,
                description: description(desc.as_deref().unwrap_or(""))?,
                project_id,
                section_id,
                parent_id,
                due,
                priority: priority.map(priority_field).transpose()?,
                labels: labels_field(labels.unwrap_or_default())?,
                deadline: None,
            };
            out.task_id = Some(add_task(s, n, now)?);
        }
        Command::QuickAdd {
            text: line,
            lang,
            project_id,
            section_id,
            default_due,
        } => {
            if line.chars().count() > MAX_CONTENT + 300 {
                return Err("quick add text is too long".into());
            }
            let ctx = ctx_for(s, tz, now);
            let names: Vec<String> = s.projects.values().map(|p| p.name.clone()).collect();
            let mut p = nlp::quick_add(
                &line,
                lang.as_deref().unwrap_or(&s.settings.lang),
                ctx,
                &names,
            );
            let mut content = p.content.clone();
            let mut project = project_id;
            let mut section = section_id;
            if let Some(name) = &p.project {
                match s
                    .projects
                    .values()
                    .find(|x| x.name.eq_ignore_ascii_case(name))
                {
                    Some(x) => {
                        if project.as_deref() != Some(x.id.as_str()) {
                            section = None;
                        }
                        project = Some(x.id.clone());
                    }
                    // Like Todoist: an unknown #name stays in the text.
                    None => content = format!("{content} #{name}"),
                }
            }
            if let Some(name) = &p.section {
                let pid = project.clone().or_else(|| s.inbox_id().map(str::to_owned));
                match s.sections.values().find(|x| {
                    x.name.eq_ignore_ascii_case(name) && Some(&x.project_id) == pid.as_ref()
                }) {
                    Some(x) => section = Some(x.id.clone()),
                    None => content = format!("{content} /{name}"),
                }
            }
            let due = match (&p.due_string, default_due.as_deref()) {
                (Some(phrase), _) => due_from(s, Some(phrase), None, Some(&p.lang), tz, now)?,
                (None, Some(d)) if !d.trim().is_empty() => {
                    due_from(s, Some(d), None, None, tz, now)?
                }
                _ => None,
            };
            let n = NewTask {
                content: text("content", &content, MAX_CONTENT)?,
                description: description(p.description.as_deref().unwrap_or(""))?,
                project_id: project,
                section_id: section,
                parent_id: None,
                due,
                priority: p.priority.map(priority_field).transpose()?,
                labels: labels_field(p.labels.clone())?,
                deadline: nlp::deadline_of(&line, ctx),
            };
            out.task_id = Some(add_task(s, n, now)?);
            p.content = content;
            out.parsed = Some(p);
        }
        Command::Complete { task_id } => {
            let t = open_task(s, &task_id)?.clone();
            let today = today_in(tz, now);
            if t.recurring() {
                let due = t.due.clone().expect("recurring has a due");
                let next = nlp::recurrence_of(&due.string, today)
                    .zip(due_local(&due, tz))
                    .map(|(r, (d, _))| r.next_after(d, today));
                if let Some(x) = s.tasks.get_mut(&task_id) {
                    if let (Some(next), Some(d)) = (next, x.due.as_mut()) {
                        d.date = moved_due_date(&due, next, tz);
                    }
                    x.provisional = true;
                }
                record_completion(
                    s,
                    Completed {
                        task_id: task_id.clone(),
                        content: t.content.clone(),
                        project_id: t.project_id.clone(),
                        completed_ms: now,
                        recurring: true,
                        source: "local".into(),
                        snapshot: None,
                        children: vec![],
                        previous_due: Some(due),
                    },
                );
                queue(
                    s,
                    "item_close",
                    json!({"id": task_id}),
                    Some(&task_id),
                    None,
                    now,
                    0,
                )
                .expect_recurring = true;
            } else {
                let ids = descendants(s, &task_id);
                let mut children = Vec::new();
                for id in ids.iter().skip(1) {
                    if let Some(c) = s.tasks.remove(id) {
                        children.push(c);
                    }
                }
                let mut snap = s.tasks.remove(&task_id).expect("open task");
                snap.checked = true;
                snap.completed_at = Some(ms_to_utc(now).to_rfc3339());
                record_completion(
                    s,
                    Completed {
                        task_id: task_id.clone(),
                        content: t.content.clone(),
                        project_id: t.project_id.clone(),
                        completed_ms: now,
                        recurring: false,
                        source: "local".into(),
                        snapshot: Some(snap),
                        children,
                        previous_due: None,
                    },
                );
                let base = t.updated_at.clone();
                let op = queue(s, "item_close", json!({"id": task_id}), None, None, now, 0);
                op.task_id = Some(task_id.clone());
                op.base_updated_at = base;
            }
            out.task_id = Some(task_id);
        }
        Command::Uncomplete { task_id } => {
            let pos = s
                .completed
                .iter()
                .rposition(|c| c.task_id == task_id)
                .ok_or("No completion of that task to undo")?;
            let c = s.completed.remove(pos);
            if let Some(prev) = c.previous_due.clone() {
                let t = s.tasks.get_mut(&task_id).ok_or("Task not found")?;
                t.due = Some(prev.clone());
                t.provisional = false;
                queue(
                    s,
                    "item_update_date_complete",
                    json!({"id": task_id, "due": {"date": prev.date, "string": prev.string, "lang": prev.lang, "is_recurring": true}, "is_forward": 0}),
                    Some(&task_id),
                    None,
                    now,
                    0,
                )
                .expect_recurring = true;
            } else {
                let mut t = c.snapshot.clone().unwrap_or_else(|| Task {
                    id: task_id.clone(),
                    project_id: c.project_id.clone(),
                    content: c.content.clone(),
                    priority: 1,
                    ..Task::default()
                });
                t.checked = false;
                t.completed_at = None;
                s.tasks.insert(task_id.clone(), t);
                for mut ch in c.children {
                    ch.checked = false;
                    s.tasks.insert(ch.id.clone(), ch);
                }
                queue(
                    s,
                    "item_uncomplete",
                    json!({"id": task_id}),
                    Some(&task_id),
                    None,
                    now,
                    0,
                );
            }
            out.task_id = Some(task_id);
        }
        Command::Update {
            task_id,
            content,
            description: desc,
            priority,
            labels,
            due_string,
            due_date,
            lang,
            clear_due,
        } => {
            let t = open_task(s, &task_id)?.clone();
            let mut args = Map::new();
            let mut nt = t.clone();
            if let Some(c) = content {
                nt.content = text("content", &c, MAX_CONTENT)?;
                args.insert("content".into(), json!(nt.content));
            }
            if let Some(d) = desc {
                nt.description = description(&d)?;
                args.insert("description".into(), json!(nt.description));
            }
            if let Some(p) = priority {
                nt.priority = priority_field(p)?;
                args.insert("priority".into(), json!(nt.priority));
            }
            if let Some(l) = labels {
                nt.labels = labels_field(l)?;
                args.insert("labels".into(), json!(nt.labels));
            }
            let mut expect_recurring = false;
            if clear_due {
                if due_string.is_some() || due_date.is_some() {
                    return Err("clear_due cannot be combined with a new due".into());
                }
                nt.due = None;
                args.insert("due".into(), Value::Null);
            } else if t.recurring() && due_string.is_none() {
                if let Some(d) = due_date.as_deref() {
                    // A picked date on a recurring task moves this occurrence.
                    let date = date_or_phrase(s, d.get(..10).unwrap_or(d), tz, now)?;
                    if !args.is_empty() {
                        s.tasks.insert(task_id.clone(), nt.clone());
                        queue(
                            s,
                            "item_update",
                            {
                                let mut a = args.clone();
                                a.insert("id".into(), json!(task_id));
                                Value::Object(a)
                            },
                            Some(&task_id),
                            None,
                            now,
                            0,
                        );
                    }
                    reschedule(s, &task_id, date, tz, now)?;
                    out.task_id = Some(task_id);
                    return Ok(out);
                }
            } else if let Some((due, wire)) = due_from(
                s,
                due_string.as_deref(),
                due_date.as_deref(),
                lang.as_deref(),
                tz,
                now,
            )? {
                expect_recurring = due.is_recurring;
                nt.due = Some(due);
                nt.provisional = false;
                args.insert("due".into(), wire);
            }
            if args.is_empty() {
                return Err("Nothing to change".into());
            }
            args.insert("id".into(), json!(task_id));
            s.tasks.insert(task_id.clone(), nt);
            queue(
                s,
                "item_update",
                Value::Object(args),
                Some(&task_id),
                None,
                now,
                0,
            )
            .expect_recurring = expect_recurring;
            out.task_id = Some(task_id);
        }
        Command::Reschedule { task_id, date } => {
            let d = date_or_phrase(s, &date, tz, now)?;
            reschedule(s, &task_id, d, tz, now)?;
            out.task_id = Some(task_id);
        }
        Command::RescheduleOverdue { date } => {
            let today = today_in(tz, now);
            let d = match date {
                Some(x) => date_or_phrase(s, &x, tz, now)?,
                None => today,
            };
            let overdue: Vec<String> = views::open_tasks(s)
                .filter(|t| {
                    t.due
                        .as_ref()
                        .and_then(|x| due_local(x, tz))
                        .is_some_and(|(x, _)| x < today)
                })
                .map(|t| t.id.clone())
                .collect();
            if overdue.is_empty() {
                out.queued = false;
            }
            for id in overdue {
                reschedule(s, &id, d, tz, now)?;
            }
        }
        Command::Delete { task_id } => {
            open_task(s, &task_id)?;
            if let Some(t) = s.tasks.get_mut(&task_id) {
                t.deleted_ms = Some(now);
            }
            queue(
                s,
                "item_delete",
                json!({"id": task_id}),
                Some(&task_id),
                None,
                now,
                now + UNDO_MS,
            );
            out.task_id = Some(task_id);
        }
        Command::UndoDelete { task_id } => {
            let pending = s
                .outbox
                .iter()
                .position(|o| o.kind == "item_delete" && o.task_id.as_deref() == Some(&task_id));
            let live = s
                .tasks
                .get(&task_id)
                .is_some_and(|t| t.deleted_ms.is_some());
            match (pending, live) {
                (Some(pos), true) => {
                    s.outbox.remove(pos);
                    if let Some(t) = s.tasks.get_mut(&task_id) {
                        t.deleted_ms = None;
                    }
                    out.queued = false;
                }
                _ => {
                    // Already deleted on Todoist (or on its way): restore as a
                    // new task with the same fields.
                    let t = if live {
                        s.tasks.remove(&task_id)
                    } else {
                        let pos = s
                            .trash
                            .iter()
                            .rposition(|t| t.id == task_id)
                            .ok_or("Nothing to restore")?;
                        Some(s.trash.remove(pos))
                    }
                    .ok_or("Nothing to restore")?;
                    let due = t.due.as_ref().map(|d| {
                        let wire = if d.is_recurring {
                            json!({"date": d.date, "string": d.string, "lang": d.lang, "is_recurring": true})
                        } else {
                            json!({"date": d.date})
                        };
                        (d.clone(), wire)
                    });
                    let n = NewTask {
                        content: t.content,
                        description: t.description,
                        project_id: Some(t.project_id).filter(|p| s.projects.contains_key(p)),
                        section_id: t.section_id.filter(|x| s.sections.contains_key(x)),
                        parent_id: t.parent_id.filter(|p| s.tasks.contains_key(p)),
                        due,
                        priority: Some(t.priority),
                        labels: t.labels,
                        deadline: t.deadline,
                    };
                    out.task_id = Some(add_task(s, n, now)?);
                }
            }
        }
        Command::Move {
            task_id,
            project_id,
            section_id,
            parent_id,
        } => {
            open_task(s, &task_id)?;
            let n = [&project_id, &section_id, &parent_id]
                .iter()
                .filter(|x| x.is_some())
                .count();
            if n != 1 {
                return Err("give exactly one of project_id, section_id, parent_id".into());
            }
            let (project, section, parent, arg) = if let Some(p) = project_id {
                if !s.projects.contains_key(&p) {
                    return Err("Project not found".into());
                }
                (
                    p.clone(),
                    None,
                    None,
                    json!({"id": task_id, "project_id": p}),
                )
            } else if let Some(sec) = section_id {
                let x = s.sections.get(&sec).ok_or("Section not found")?;
                (
                    x.project_id.clone(),
                    Some(sec.clone()),
                    None,
                    json!({"id": task_id, "section_id": sec}),
                )
            } else {
                let p = parent_id.expect("one is set");
                if descendants(s, &task_id).contains(&p) {
                    return Err("A task cannot move under itself".into());
                }
                let x = open_task(s, &p)?;
                (
                    x.project_id.clone(),
                    x.section_id.clone(),
                    Some(p.clone()),
                    json!({"id": task_id, "parent_id": p}),
                )
            };
            for id in descendants(s, &task_id) {
                if let Some(t) = s.tasks.get_mut(&id) {
                    t.project_id = project.clone();
                    t.section_id = section.clone();
                    if id == task_id {
                        t.parent_id = parent.clone();
                    }
                }
            }
            queue(s, "item_move", arg, Some(&task_id), None, now, 0);
            out.task_id = Some(task_id);
        }
        Command::SetDailyGoal { goal } => {
            if !(1..=100).contains(&goal) {
                return Err("daily goal must be 1 to 100 tasks".into());
            }
            s.settings.daily_goal = goal;
            queue(
                s,
                "update_goals",
                json!({"daily_goal": goal}),
                None,
                None,
                now,
                0,
            );
        }
        Command::DismissConflict { conflict_id } => {
            let before = s.conflicts.len();
            s.conflicts.retain(|c| c.id != conflict_id);
            if s.conflicts.len() == before {
                return Err("Conflict not found".into());
            }
            out.queued = false;
        }
    }
    Ok(out)
}

pub fn validate(s: &State) -> Result<()> {
    if s.schema_version != 1 {
        return Err("Unsupported tasks data schema version".into());
    }
    if s.tasks.len() > MAX_TASKS || s.receipts.len() > MAX_RECEIPTS {
        return Err("Data capacity reached".into());
    }
    if s.outbox.len() > MAX_OUTBOX + 200 {
        return Err("Too many edits are waiting for Todoist".into());
    }
    let mut uuids = HashSet::new();
    for o in &s.outbox {
        if !uuids.insert(o.uuid.as_str()) {
            return Err("Duplicate queued command".into());
        }
    }
    for (k, t) in &s.tasks {
        if k != &t.id || t.id.is_empty() || t.id.len() > 128 {
            return Err("Corrupt task id".into());
        }
        if !(1..=4).contains(&t.priority) {
            return Err("Corrupt task priority".into());
        }
        if t.parent_id.as_deref() == Some(t.id.as_str()) {
            return Err("A task cannot be its own parent".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_local_forms() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let d = |s: &str| Due {
            date: s.into(),
            ..Due::default()
        };
        assert_eq!(
            due_local(&d("2026-10-22"), tz),
            Some((NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(), None))
        );
        let (day, t) = due_local(&d("2026-10-22T23:30:00.000000Z"), tz).unwrap();
        assert_eq!(day, NaiveDate::from_ymd_opt(2026, 10, 23).unwrap());
        assert_eq!(t.unwrap().format("%H:%M").to_string(), "01:30");
        let (_, t) = due_local(&d("2026-10-22T09:00:00"), tz).unwrap();
        assert_eq!(t.unwrap().format("%H:%M").to_string(), "09:00");
        let new = moved_due_date(
            &d("2026-10-22T09:00:00.000000"),
            NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
            tz,
        );
        assert_eq!(new, "2026-10-25T09:00:00.000000");
        let fixed = Due {
            date: "2026-10-22T07:00:00.000000Z".into(),
            timezone: Some("Europe/Rome".into()),
            ..Due::default()
        };
        // 09:00 in Rome stays 09:00 after the DST change on 25 October
        let moved = moved_due_date(&fixed, NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(), tz);
        assert_eq!(moved, "2026-10-26T08:00:00.000000Z");
    }
}
