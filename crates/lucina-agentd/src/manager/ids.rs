//! The id store: every run, change, version and finding has an identifier, and this module is
//! the one place that issues them. The pure rules are in [`lucina_agents::ids`].
//!
//! Rows are kept in `<state dir>/ids/ids.jsonl`, one JSON [`IdRow`] per line, append-only. All
//! rows are also held in memory behind one mutex: issuing an id computes the next number and
//! appends the line while holding that mutex, then `sync_data`. The file is opened in append
//! mode for each write and is never rewritten or truncated. A missing file is an empty store.
//! A line that does not parse makes the store read-only for the life of the process (every
//! write answers `the id store has an unreadable line (<n>); fix it by hand`; reads still work
//! with the lines that parsed).
//!
//! Every appended row carries `prev`: the lowercase hex SHA-256 of the previous line's bytes
//! without its newline, 64 zeros for the first line (the rule of the commit ledger in
//! [`crate::ledger`]). At open the chain is verified: on the first line whose `prev` does not
//! match, the store becomes read-only with `the id store's hash chain is broken at line <n>;
//! fix it by hand`, the listing carries `chainBrokenAt` and every id whose first row is at or
//! after that line carries `afterBreak`.
//!
//! A chain shows a changed, removed or inserted line; it does not show a forged line appended at
//! the end. Until workers run in a sandbox that cannot write this directory, this record is as
//! trustworthy as the sandbox around the worker: workers run as the same Unix user as the
//! daemon.
//!
//! Each row says who declared it (`declaredBy`): the daemon for the rows it writes when it
//! starts and follows a work job, `lead` for rows issued through `POST /ids`, `import` for rows
//! read from a hand-kept ledger.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use lucina_agents::ids::{
    check_new, check_new_area, check_state, current, default_state, format_id, import_plan,
    lead_of, next_number, parse_areas_tsv, parse_id, parse_tsv, states, trace, trailers, IdRow,
    IdView, Kind,
};
use lucina_agents::work::WorkJob;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::work::clean_command;
use super::{Manager, ManagerError};
use crate::pipeline::JobKind;

/// The file in `<state dir>/ids/`.
pub const FILE: &str = "ids.jsonl";
/// `prev` of the first line.
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// How long a `git` call may take.
const GIT_LIMIT: Duration = Duration::from_secs(20);

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn invalid(m: impl Into<String>) -> ManagerError {
    ManagerError::Invalid(m.into())
}

fn nonblank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn unreadable_message(line: usize) -> String {
    format!("the id store has an unreadable line ({line}); fix it by hand")
}

fn broken_message(line: usize) -> String {
    format!("the id store's hash chain is broken at line {line}; fix it by hand")
}

/// Rows and the facts about the file they came from.
#[derive(Default)]
struct State {
    rows: Vec<IdRow>,
    /// The 1-based line of each row in the file.
    line_of: Vec<usize>,
    /// Lines in the file.
    lines: usize,
    /// The hash of the last line ([`ZERO`] for none).
    last_hash: String,
    /// Why nothing may be written.
    read_only: Option<String>,
    chain_broken_at: Option<usize>,
}

impl State {
    fn load(file: &Path) -> State {
        let mut st = State {
            last_hash: ZERO.to_string(),
            ..State::default()
        };
        let bytes = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return st,
            Err(e) => {
                st.read_only = Some(format!("cannot read the id store {}: {e}", file.display()));
                return st;
            }
        };
        let complete = bytes.is_empty() || bytes.ends_with(b"\n");
        let mut parts: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        if complete {
            parts.pop();
        }
        let mut prev = ZERO.to_string();
        let mut unreadable: Option<usize> = None;
        let mut broken: Option<usize> = None;
        for (i, line) in parts.iter().enumerate() {
            let n = i + 1;
            match serde_json::from_slice::<IdRow>(line) {
                Ok(row) => {
                    if row.prev != prev && broken.is_none() {
                        broken = Some(n);
                    }
                    st.rows.push(row);
                    st.line_of.push(n);
                }
                Err(_) => {
                    unreadable.get_or_insert(n);
                }
            }
            prev = sha256_hex(line);
        }
        // A last line without its newline would be glued to the next write.
        if !complete {
            unreadable.get_or_insert(parts.len());
        }
        st.lines = parts.len();
        st.last_hash = prev;
        st.chain_broken_at = broken;
        st.read_only = match (unreadable, broken) {
            (Some(u), Some(b)) if b < u => Some(broken_message(b)),
            (Some(u), _) => Some(unreadable_message(u)),
            (None, Some(b)) => Some(broken_message(b)),
            (None, None) => None,
        };
        st
    }

    fn exists(&self, id: &str) -> bool {
        self.rows.iter().any(|r| r.id == id)
    }

    fn refuse_if_read_only(&self) -> Result<(), ManagerError> {
        match &self.read_only {
            Some(e) => Err(invalid(e.clone())),
            None => Ok(()),
        }
    }

    /// The line of an id's first row.
    fn first_line(&self, id: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.id == id)
            .map(|i| self.line_of[i])
    }

    fn after_break(&self, id: &str) -> bool {
        match (self.chain_broken_at, self.first_line(id)) {
            (Some(b), Some(first)) => first >= b,
            _ => false,
        }
    }

    fn views(&self) -> Vec<IdView> {
        let mut views = current(&self.rows);
        if let Some(b) = self.chain_broken_at {
            let mut first: HashMap<&str, usize> = HashMap::new();
            for (r, n) in self.rows.iter().zip(&self.line_of) {
                first.entry(r.id.as_str()).or_insert(*n);
            }
            for v in &mut views {
                v.after_break = first.get(v.id.as_str()).is_some_and(|n| *n >= b);
            }
        }
        views
    }

    /// Appends the rows (their `prev` is filled in here) as one write.
    fn write(&mut self, dir: &Path, rows: &mut [IdRow]) -> Result<(), ManagerError> {
        self.refuse_if_read_only()?;
        let mut prev = self.last_hash.clone();
        let mut buf = String::new();
        for r in rows.iter_mut() {
            r.prev = prev.clone();
            let line = serde_json::to_string(r)
                .map_err(|e| invalid(format!("cannot write the id store: {e}")))?;
            prev = sha256_hex(line.as_bytes());
            buf.push_str(&line);
            buf.push('\n');
        }
        let io = |e: std::io::Error| invalid(format!("cannot write the id store: {e}"));
        std::fs::create_dir_all(dir).map_err(io)?;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(dir.join(FILE))
            .map_err(io)?;
        f.write_all(buf.as_bytes()).map_err(io)?;
        f.sync_data().map_err(io)?;
        self.last_hash = prev;
        for r in rows.iter() {
            self.lines += 1;
            self.line_of.push(self.lines);
            self.rows.push(r.clone());
        }
        Ok(())
    }
}

/// The stored rows.
pub(super) struct Ids {
    dir: PathBuf,
    state: Mutex<State>,
}

impl Ids {
    /// Opens `<state dir>/ids`, the state directory being the parent of the work directory.
    pub(super) fn open(work_dir: &Path) -> Ids {
        let dir = work_dir.parent().unwrap_or(work_dir).join("ids");
        let state = State::load(&dir.join(FILE));
        if let Some(e) = &state.read_only {
            tracing::error!(file = %dir.join(FILE).display(), "{e}");
        }
        Ids {
            dir,
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Issues the next id of `kind` (for a lead or an area row `prefix` is the new code) and
    /// appends its row. `build` gets the id and returns the row's fields; it runs while the
    /// store is locked, so it must be quick and must not call the store. `owner` is the
    /// owning lead of a new area.
    pub(super) fn issue(
        &self,
        kind: Kind,
        prefix: &str,
        owner: Option<&str>,
        declared_by: &str,
        build: impl FnOnce(&str) -> IdRow,
    ) -> Result<IdRow, ManagerError> {
        let mut st = self.lock();
        st.refuse_if_read_only()?;
        let id = match kind {
            Kind::Lead | Kind::Area => prefix.to_string(),
            _ => format_id(kind, prefix, next_number(&st.rows, kind, prefix)),
        };
        let mut row = build(&id);
        row.id = id;
        row.kind = kind;
        row.utc = now();
        row.declared_by = Some(declared_by.to_string());
        if row.state.is_empty() {
            row.state = default_state(kind).to_string();
        }
        match kind {
            Kind::Area => check_new_area(&st.rows, prefix, owner.unwrap_or(""), &row.title),
            _ => check_new(&st.rows, kind, prefix, &row.title),
        }
        .map_err(invalid)?;
        if !states(kind).contains(&row.state.as_str()) {
            return Err(invalid(format!(
                "state \"{}\" does not belong to a {kind}: use {}",
                row.state,
                states(kind).join(", ")
            )));
        }
        if kind == Kind::Area {
            row.by = owner.unwrap_or_default().to_string();
        }
        for r in &row.refs {
            if !st.exists(r) {
                return Err(invalid(format!("refs: no id {r}")));
            }
        }
        st.write(&self.dir, std::slice::from_mut(&mut row))?;
        Ok(row)
    }

    /// Appends a row with the id of an existing one (a state change). Only the fields the row
    /// sets count: the view takes the latest non-empty value of each.
    pub(super) fn append(
        &self,
        id: &str,
        mut row: IdRow,
        declared_by: &str,
    ) -> Result<IdRow, ManagerError> {
        let mut st = self.lock();
        st.refuse_if_read_only()?;
        row.kind = check_state(&st.rows, id, &row.state).map_err(invalid)?;
        for r in &row.refs {
            if !st.exists(r) {
                return Err(invalid(format!("refs: no id {r}")));
            }
        }
        row.id = id.to_string();
        row.utc = now();
        row.declared_by = Some(declared_by.to_string());
        st.write(&self.dir, std::slice::from_mut(&mut row))?;
        Ok(row)
    }

    pub(super) fn kind_of(&self, id: &str) -> Option<Kind> {
        self.lock().rows.iter().find(|r| r.id == id).map(|r| r.kind)
    }

    pub(super) fn latest_state(&self, id: &str) -> Option<String> {
        self.lock()
            .rows
            .iter()
            .rev()
            .find(|r| r.id == id)
            .map(|r| r.state.clone())
    }

    /// Whether a row of the id already names the lane.
    pub(super) fn has_lane_row(&self, id: &str, lane: &str) -> bool {
        self.lock()
            .rows
            .iter()
            .any(|r| r.id == id && r.lane.as_deref() == Some(lane))
    }

    fn after_break(&self, id: &str) -> bool {
        self.lock().after_break(id)
    }

    /// Reads a hand-kept ledger's rows in. Nothing is written when any row names a lead or an
    /// area that is not registered. Returns how many rows were added and skipped.
    fn import(&self, incoming: Vec<IdRow>) -> Result<(usize, usize), ManagerError> {
        let total = incoming.len();
        let mut st = self.lock();
        st.refuse_if_read_only()?;
        let codes = |kind: Kind| -> HashSet<String> {
            st.rows
                .iter()
                .chain(incoming.iter())
                .filter(|r| r.kind == kind)
                .map(|r| r.id.clone())
                .collect()
        };
        let leads = codes(Kind::Lead);
        let areas = codes(Kind::Area);
        for r in &incoming {
            let prefix = parse_id(&r.id).map(|p| p.prefix).unwrap_or_default();
            match r.kind {
                Kind::Run | Kind::Change | Kind::Version if !leads.contains(&prefix) => {
                    return Err(invalid(format!("lead {prefix} is not registered")));
                }
                Kind::Finding if !areas.contains(&prefix) => {
                    return Err(invalid(format!("area {prefix} is not registered")));
                }
                Kind::Area if !leads.contains(&r.by) => {
                    return Err(invalid(format!("lead {} is not registered", r.by)));
                }
                Kind::Area if leads.contains(&r.id) => {
                    return Err(invalid(format!(
                        "\"{}\" is a lead code: an area code must differ from every lead code",
                        r.id
                    )));
                }
                _ => {}
            }
        }
        let mut plan = import_plan(&st.rows, incoming);
        for r in &mut plan {
            r.declared_by = Some("import".to_string());
        }
        let imported = plan.len();
        if imported > 0 {
            st.write(&self.dir, &mut plan)?;
        }
        Ok((imported, total - imported))
    }
}

/// What `POST /ids` takes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewId {
    pub kind: Kind,
    /// The lead of a run, change or version; the owner of a new area.
    #[serde(default)]
    pub lead: Option<String>,
    /// The area of a finding.
    #[serde(default)]
    pub area: Option<String>,
    /// The new code of a lead or an area.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default, rename = "where")]
    pub where_: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

/// What `POST /ids/{id}/state` takes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateId {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default, rename = "where")]
    pub where_: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

/// What `POST /ids/import` takes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIds {
    #[serde(default)]
    pub tsv: String,
    /// The text is an area file (`code`, `lead`, `covers`).
    #[serde(default)]
    pub areas: bool,
}

/// A `git` call in `dir` with the cleared environment of the lane commands; its output when it
/// exits 0.
async fn git_out(dir: &str, args: &[&str]) -> Option<String> {
    let mut cmd = clean_command("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = tokio::time::timeout(GIT_LIMIT, cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// The exit code of a `git` call in `dir`; `None` when it could not run.
async fn git_code(dir: &str, args: &[&str]) -> Option<i32> {
    let mut cmd = clean_command("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = tokio::time::timeout(GIT_LIMIT, cmd.status())
        .await
        .ok()?
        .ok()?;
    status.code()
}

/// What a work job says about itself in a trace.
fn job_info(j: &WorkJob) -> Value {
    json!({
        "jobId": j.id,
        "lane": j.lane,
        "chain": j.chain,
        "finalLane": j.final_lane,
        "model": j.model,
        "specPath": j.spec_path,
        "workspace": j.workspace,
        "state": j.state,
    })
}

impl Manager {
    fn ids(&self) -> &Ids {
        &self.inner.ids
    }

    /// The state directory (the parent of the work directory).
    fn state_dir(&self) -> PathBuf {
        let work = &self.inner.options.work.dir;
        work.parent().unwrap_or(work).to_path_buf()
    }

    /// Issues an id for a lead (`POST /ids`, `lagent id new`). A run declared by hand must
    /// name who stands behind it.
    pub fn ids_new(&self, req: NewId) -> Result<IdRow, ManagerError> {
        self.ids_issue(req, "lead")
    }

    fn ids_issue(&self, req: NewId, declared_by: &str) -> Result<IdRow, ManagerError> {
        let kind = req.kind;
        let by = req.by.trim().to_string();
        let agent = nonblank(req.agent.clone());
        if declared_by == "lead" && kind == Kind::Run && (by.is_empty() || agent.is_none()) {
            return Err(invalid(
                "a run declared by hand must name who stands behind it: give by and agent (an agent id or a transcript path)",
            ));
        }
        let (prefix, owner) = match kind {
            Kind::Run | Kind::Change | Kind::Version => (
                nonblank(req.lead.clone())
                    .ok_or_else(|| invalid("lead is required for a run, change or version"))?,
                None,
            ),
            Kind::Finding => (
                nonblank(req.area.clone())
                    .ok_or_else(|| invalid("area is required for a finding"))?,
                None,
            ),
            Kind::Lead => (
                nonblank(req.code.clone()).ok_or_else(|| invalid("code is required for a lead"))?,
                None,
            ),
            Kind::Area => (
                nonblank(req.code.clone())
                    .ok_or_else(|| invalid("code is required for an area"))?,
                Some(
                    nonblank(req.lead.clone())
                        .ok_or_else(|| invalid("lead (the owner) is required for an area"))?,
                ),
            ),
        };
        let row = IdRow {
            title: req.title.trim().to_string(),
            by,
            spec: req.spec,
            where_: req.where_,
            base: req.base,
            result: req.result,
            refs: req.refs,
            state: nonblank(req.state).unwrap_or_default(),
            lane: nonblank(req.lane),
            model: nonblank(req.model),
            chain: nonblank(req.chain),
            repo: nonblank(req.repo),
            workspace: nonblank(req.workspace),
            branch: nonblank(req.branch),
            agent,
            ..IdRow::default()
        };
        self.ids()
            .issue(kind, &prefix, owner.as_deref(), declared_by, |_| row)
    }

    /// A state change: a new row with the same id (`POST /ids/{id}/state`).
    pub fn ids_state(&self, id: &str, req: StateId) -> Result<IdRow, ManagerError> {
        let row = IdRow {
            state: req.state.trim().to_string(),
            title: req.title,
            by: req.by,
            spec: req.spec,
            where_: req.where_,
            base: req.base,
            result: req.result,
            refs: req.refs,
            lane: nonblank(req.lane),
            model: nonblank(req.model),
            chain: nonblank(req.chain),
            repo: nonblank(req.repo),
            workspace: nonblank(req.workspace),
            branch: nonblank(req.branch),
            agent: nonblank(req.agent),
            ..IdRow::default()
        };
        self.ids().append(id, row, "lead")
    }

    /// The current views, filtered. `lead` matches the prefix of a run, change or version and
    /// the owning lead of a finding's area; `query` is a case-insensitive substring of the id
    /// and the title.
    pub fn ids_list(
        &self,
        lead: Option<&str>,
        kind: Option<&str>,
        state: Option<&str>,
        query: Option<&str>,
    ) -> Result<Value, ManagerError> {
        let kind = match kind.filter(|k| !k.is_empty()) {
            Some(k) => Some(Kind::parse(k).ok_or_else(|| {
                invalid(format!(
                    "unknown kind \"{k}\": use run, change, version, finding, lead or area"
                ))
            })?),
            None => None,
        };
        let query = query.filter(|q| !q.is_empty()).map(str::to_lowercase);
        let st = self.ids().lock();
        let ids: Vec<IdView> = st
            .views()
            .into_iter()
            .filter(|v| kind.is_none_or(|k| v.kind == k))
            .filter(|v| state.filter(|s| !s.is_empty()).is_none_or(|s| v.state == s))
            .filter(|v| {
                lead.filter(|l| !l.is_empty())
                    .is_none_or(|l| lead_of(&st.rows, &v.id).as_deref() == Some(l))
            })
            .filter(|v| {
                query.as_ref().is_none_or(|q| {
                    v.id.to_lowercase().contains(q) || v.title.to_lowercase().contains(q)
                })
            })
            .collect();
        Ok(json!({
            "ids": ids,
            "chainBrokenAt": st.chain_broken_at,
            "readOnly": st.read_only,
        }))
    }

    /// One id: its view and all its rows, oldest first.
    pub fn ids_get(&self, id: &str) -> Result<Value, ManagerError> {
        let st = self.ids().lock();
        let view = st
            .views()
            .into_iter()
            .find(|v| v.id == id)
            .ok_or_else(|| ManagerError::IdNotFound(format!("no id {id}")))?;
        let rows: Vec<&IdRow> = st.rows.iter().filter(|r| r.id == id).collect();
        Ok(json!({"view": view, "rows": rows, "chainBrokenAt": st.chain_broken_at}))
    }

    /// Reads a hand-kept ledger (or an area file) into the store.
    pub fn ids_import(&self, req: ImportIds) -> Result<Value, ManagerError> {
        let rows = if req.areas {
            parse_areas_tsv(&req.tsv)
        } else {
            parse_tsv(&req.tsv)
        }
        .map_err(invalid)?;
        let (imported, skipped) = self.ids().import(rows)?;
        Ok(json!({"imported": imported, "skipped": skipped}))
    }

    /// Whether `code` is a registered lead.
    pub(super) fn lead_registered(&self, code: &str) -> bool {
        self.ids().kind_of(code) == Some(Kind::Lead)
    }

    /// The change checks of a work start.
    pub(super) fn check_work_ids(&self, lead: &str, change: &str) -> Result<(), ManagerError> {
        if !self.lead_registered(lead) {
            return Err(invalid(format!(
                "lead \"{lead}\" is not registered: register it with `lagent id lead add {lead} <name>`"
            )));
        }
        match self.ids().kind_of(change) {
            None => {
                return Err(invalid(format!(
                    "change \"{change}\" does not exist: open one with `lagent id new change --lead {lead} --title <text>`"
                )))
            }
            Some(Kind::Change) => {}
            Some(other) => {
                return Err(invalid(format!(
                    "\"{change}\" is not a change (it is a {other})"
                )))
            }
        }
        if !change.starts_with(&format!("{lead}-")) {
            let owner = parse_id(change).map(|p| p.prefix).unwrap_or_default();
            return Err(invalid(format!(
                "change {change} belongs to lead {owner}, not {lead}"
            )));
        }
        Ok(())
    }

    /// One trace node: the view, and for a run its work job and commits, for a change the
    /// merge jobs of the pipeline.
    async fn trace_node(&self, mut view: IdView) -> Value {
        view.after_break = self.ids().after_break(&view.id);
        let mut out = serde_json::to_value(&view).unwrap_or(Value::Null);
        match view.kind {
            Kind::Run => {
                let job = self.work_job_of_run(&view.id);
                let commits = self.run_commits(&view, job.as_ref()).await;
                out["job"] = job.as_ref().map(job_info).unwrap_or(Value::Null);
                out["commits"] = json!(commits);
            }
            Kind::Change => out["merges"] = json!(self.change_merges(&view)),
            _ => {}
        }
        out
    }

    /// The commits whose message has a `Run: <id>` line, in the run's workspace (the row's or
    /// the job's) when that is a git directory.
    async fn run_commits(&self, view: &IdView, job: Option<&WorkJob>) -> Vec<Value> {
        let mut dirs: Vec<String> = Vec::new();
        dirs.extend(view.workspace.clone());
        if let Some(j) = job {
            dirs.push(j.workspace.clone());
        }
        let pattern = format!("^Run: {}$", view.id);
        for dir in dirs.into_iter().filter(|d| Path::new(d).is_dir()) {
            let args = [
                "log",
                "--all",
                "-n",
                "50",
                "--format=%H%x09%s",
                "--grep",
                pattern.as_str(),
            ];
            if let Some(out) = git_out(&dir, &args).await {
                return out
                    .lines()
                    .filter_map(|l| l.split_once('\t'))
                    .map(|(sha, subject)| json!({"sha": sha, "subject": subject}))
                    .collect();
            }
        }
        Vec::new()
    }

    /// The merge jobs whose branch or workspace is the change's `where`.
    fn change_merges(&self, view: &IdView) -> Vec<Value> {
        let want = view.where_.trim();
        if want.is_empty() {
            return Vec::new();
        }
        let hit = |c: &str| !c.is_empty() && (want == c || want.ends_with(&format!("/{c}")));
        self.pipeline()
            .jobs(500)
            .into_iter()
            .filter(|j| matches!(j.kind, JobKind::Merge))
            .filter(|j| {
                [
                    j.branch.as_deref(),
                    j.spec.branch.as_deref(),
                    j.workspace_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(hit)
            })
            .map(|j| {
                json!({
                    "jobId": j.job_id,
                    "state": j.state,
                    "branch": j.branch,
                    "landed": j.outcome.as_ref().and_then(|o| o.landed.clone()),
                    "code": j.outcome.as_ref().map(|o| o.code.clone()),
                })
            })
            .collect()
    }

    /// The chain from an id (`lagent trace <id>`): the pure trace plus, for every run, its work
    /// job and commits and, for every change, its merge jobs.
    pub async fn ids_trace(&self, id: &str) -> Result<Value, ManagerError> {
        let (t, broken_at) = {
            let st = self.ids().lock();
            (trace(&st.rows, id), st.chain_broken_at)
        };
        let t = t.ok_or_else(|| ManagerError::IdNotFound(format!("no id {id}")))?;
        let subject = self.trace_node(t.subject).await;
        let mut links = Vec::new();
        for l in t.links {
            links.push(json!({"relation": l.relation, "view": self.trace_node(l.view).await}));
        }
        Ok(json!({"subject": subject, "links": links, "chainBrokenAt": broken_at}))
    }

    /// What a commit says about who made it (`lagent trace --commit`): its trailers, the
    /// attribution ledger's entry, and the chain of its run. A commit without `Lead` and `Run`
    /// answers `missing` and the ledger entry alone.
    pub async fn ids_trace_commit(&self, repo: &str, sha: &str) -> Result<Value, ManagerError> {
        if !Path::new(repo).is_absolute() || !Path::new(repo).is_dir() {
            return Err(invalid(format!(
                "repo \"{repo}\" must be an absolute path to a directory"
            )));
        }
        if sha.is_empty()
            || sha.starts_with('-')
            || sha.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(invalid(format!("\"{sha}\" is not a commit")));
        }
        let spec = format!("{sha}^{{commit}}");
        let full = git_out(repo, &["rev-parse", "--verify", &spec])
            .await
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid(format!("\"{sha}\" is not a commit of {repo}")))?;
        let message = git_out(repo, &["show", "-s", "--format=%B", &full])
            .await
            .unwrap_or_default();
        let time = git_out(repo, &["show", "-s", "--format=%cI", &full])
            .await
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let subject = message.lines().next().unwrap_or_default().to_string();
        let ids = trailers(&message);
        let entry = crate::ledger::Ledger::in_state_dir(&self.state_dir())
            .read_lenient()
            .into_iter()
            .find(|e| e.commit == full);
        let mut missing: Vec<&str> = Vec::new();
        if ids.lead.is_none() {
            missing.push("Lead");
        }
        if ids.run.is_none() {
            missing.push("Run");
        }
        let mut out = json!({
            "sha": full,
            "repo": repo,
            "subject": subject,
            "committerTime": time,
            "trailers": ids,
            "missing": missing,
            "ledger": entry,
            "run": null,
            "runView": null,
            "change": null,
            "descendsFromBase": null,
            "afterFirstRow": null,
            "notes": [],
        });
        let mut notes: Vec<String> = Vec::new();
        if missing.is_empty() {
            let run = ids.run.clone().unwrap_or_default();
            match self.ids_trace(&run).await {
                Ok(t) => {
                    let view = t["subject"].clone();
                    let base = view["base"].as_str().unwrap_or_default().to_string();
                    if !base.is_empty() {
                        out["descendsFromBase"] =
                            match git_code(repo, &["merge-base", "--is-ancestor", &base, &full])
                                .await
                            {
                                Some(0) => json!(true),
                                Some(1) => json!(false),
                                _ => Value::Null,
                            };
                    }
                    let first = chrono::DateTime::parse_from_rfc3339(
                        view["firstUtc"].as_str().unwrap_or_default(),
                    );
                    let committed = chrono::DateTime::parse_from_rfc3339(&time);
                    if let (Ok(first), Ok(committed)) = (first, committed) {
                        out["afterFirstRow"] = json!(committed >= first);
                    }
                    if ids.lead.as_deref() != lead_prefix(&run).as_deref() {
                        notes.push(format!(
                            "the Lead trailer ({}) is not the lead of {run}",
                            ids.lead.clone().unwrap_or_default()
                        ));
                    }
                    out["runView"] = view;
                    out["run"] = t;
                }
                Err(ManagerError::IdNotFound(m)) => {
                    notes.push(format!(
                        "the run in the Run trailer is not in the id store ({m})"
                    ));
                }
                Err(e) => return Err(e),
            }
        } else if let (None, Some(change)) = (&ids.run, &ids.change) {
            if let Ok(t) = self.ids_trace(change).await {
                out["change"] = t;
            }
        }
        out["notes"] = json!(notes);
        Ok(out)
    }
}

/// The lead code of a run id.
fn lead_prefix(id: &str) -> Option<String> {
    parse_id(id).map(|p| p.prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn lead_row(code: &str) -> IdRow {
        IdRow {
            title: format!("{code} chat"),
            ..IdRow::default()
        }
    }

    fn open(d: &Path) -> Ids {
        // The work directory sits beside `ids`.
        Ids::open(&d.join("work"))
    }

    fn run_row(title: &str) -> IdRow {
        IdRow {
            title: title.into(),
            ..IdRow::default()
        }
    }

    fn file(d: &Path) -> PathBuf {
        d.join("ids").join(FILE)
    }

    #[test]
    fn rows_are_chained_and_survive_a_reopen() {
        let d = dir();
        let ids = open(d.path());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        let a = ids
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("one"))
            .unwrap();
        let b = ids
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("two"))
            .unwrap();
        assert_eq!((a.id.as_str(), b.id.as_str()), ("LA-R001", "LA-R002"));
        let text = std::fs::read_to_string(file(d.path())).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let first: IdRow = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.prev, ZERO);
        let second: IdRow = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second.prev, sha256_hex(lines[0].as_bytes()));
        let third: IdRow = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(third.prev, sha256_hex(lines[1].as_bytes()));
        let again = open(d.path());
        assert!(again.lock().read_only.is_none());
        let c = again
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("three"))
            .unwrap();
        assert_eq!(c.id, "LA-R003");
    }

    #[test]
    fn a_changed_line_breaks_the_chain_and_blocks_writes() {
        let d = dir();
        let ids = open(d.path());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        for t in ["one", "two", "three"] {
            ids.issue(Kind::Run, "LA", None, "daemon", |_| run_row(t))
                .unwrap();
        }
        let text = std::fs::read_to_string(file(d.path())).unwrap();
        let edited = text.replacen("\"title\":\"two\"", "\"title\":\"TWO\"", 1);
        assert_ne!(text, edited);
        std::fs::write(file(d.path()), &edited).unwrap();
        let again = open(d.path());
        let st = again.lock();
        // Line 3 was edited, so line 4's prev no longer matches.
        assert_eq!(st.chain_broken_at, Some(4));
        assert_eq!(
            st.read_only.as_deref(),
            Some("the id store's hash chain is broken at line 4; fix it by hand")
        );
        assert_eq!(st.rows.len(), 4, "the lines that parse are still read");
        assert!(st.after_break("LA-R003"));
        assert!(!st.after_break("LA-R002"));
        assert!(!st.after_break("LA"));
        drop(st);
        let err = again
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("x"))
            .unwrap_err();
        assert!(err.to_string().contains("hash chain is broken at line 4"));
        assert_eq!(std::fs::read_to_string(file(d.path())).unwrap(), edited);
    }

    #[test]
    fn an_unreadable_line_makes_the_store_read_only() {
        let d = dir();
        let ids = open(d.path());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        let mut text = std::fs::read_to_string(file(d.path())).unwrap();
        text.push_str("this is not json\n");
        std::fs::write(file(d.path()), &text).unwrap();
        let again = open(d.path());
        assert_eq!(again.lock().rows.len(), 1);
        let err = again
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("x"))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the id store has an unreadable line (2); fix it by hand"
        );
        assert_eq!(std::fs::read_to_string(file(d.path())).unwrap(), text);
        // A last line without its newline is refused too.
        std::fs::write(file(d.path()), text.trim_end()).unwrap();
        assert!(open(d.path()).lock().read_only.is_some());
    }

    #[test]
    fn a_missing_file_is_an_empty_store_and_state_rows_check_their_kind() {
        let d = dir();
        let ids = open(d.path());
        assert!(ids.lock().rows.is_empty());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        ids.issue(Kind::Run, "LA", None, "daemon", |_| run_row("one"))
            .unwrap();
        let ok = IdRow {
            state: "verified".into(),
            ..IdRow::default()
        };
        let row = ids.append("LA-R001", ok, "daemon").unwrap();
        assert_eq!(row.kind, Kind::Run);
        assert_eq!(ids.latest_state("LA-R001").as_deref(), Some("verified"));
        let wrong = IdRow {
            state: "merged".into(),
            ..IdRow::default()
        };
        assert!(ids.append("LA-R001", wrong.clone(), "daemon").is_err());
        assert!(ids.append("LA-R002", wrong, "daemon").is_err());
        // A refs id that does not exist is refused, and nothing is written for it.
        let before = std::fs::read_to_string(file(d.path())).unwrap();
        let dangling = IdRow {
            title: "x".into(),
            refs: vec!["LA-C009".into()],
            ..IdRow::default()
        };
        assert!(ids
            .issue(Kind::Run, "LA", None, "daemon", |_| dangling)
            .is_err());
        assert_eq!(std::fs::read_to_string(file(d.path())).unwrap(), before);
    }
}
