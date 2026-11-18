//! The id store: every run, change, version and finding has an identifier, and this module is
//! the one place that issues them. The pure rules are in [`lucina_agents::ids`].
//!
//! Rows are kept in `<state dir>/ids/ids.jsonl`, one JSON [`IdRow`] per line, append-only. All
//! rows are also held in memory behind one mutex: issuing an id computes the next number and
//! appends the line while holding that mutex, then `sync_data`. The file is opened in append
//! mode for each write and is never rewritten or truncated. A missing file is an empty store.
//! A line that does not parse, and an append that fails partway, make the store read-only for
//! the life of the process (every write answers `the id store has an unreadable line (<n>); fix
//! it by hand`, or the error of the failed append; reads still work with the lines that parsed).
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
//!
//! An import never refuses a file: a line the rules of its kind do not allow is kept as an
//! annotation row, and a line whose lead or area is not registered — the operator
//! registers a lead with `lagent id lead add` before the import — is skipped and listed with the
//! reason. Only a code that is both a lead's and an area's stops the whole request, for there
//! every id of either kind it owns would be ambiguous.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use lucina_agents::ids::{
    check_new, check_new_area, check_state, current, default_state, format_id, import_plan,
    lead_code_clash, lead_of, next_number, parse_areas_tsv, parse_id, parse_tsv, states, trace,
    trailers, IdRow, IdView, Kind, Ledger, LedgerRow, SkippedRow,
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
    /// The test seam, never set outside the unit tests of this module: an append stops after
    /// this many bytes and answers an error, as a disk that fills in the middle of a line.
    fail_write_after: Option<usize>,
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

    /// The append of `bytes` to the open file and its `sync_data`. `fail_after` is the test
    /// seam: the write stops after that many bytes and answers an error.
    fn write_and_sync(
        f: &mut std::fs::File,
        bytes: &[u8],
        fail_after: Option<usize>,
    ) -> std::io::Result<()> {
        match fail_after {
            Some(after) => {
                let mut left = &bytes[..after.min(bytes.len())];
                while !left.is_empty() {
                    let taken = f.write(left)?;
                    left = &left[taken..];
                }
                Err(std::io::Error::other("the disk took only part of the line"))
            }
            None => f.write_all(bytes),
        }?;
        f.sync_data()
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
        if let Err(e) =
            Self::write_and_sync(&mut f, buf.as_bytes(), self.fail_write_after).map_err(io)
        {
            // Bytes of the line may already be on disk. Nothing is remembered and the store
            // stops here for the life of the process, so the same number is never issued twice.
            let message = e.to_string();
            self.read_only = Some(message.clone());
            return Err(invalid(message));
        }
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

    /// The state of the id's latest row that is a row of its own. An annotation keeps a ledger
    /// line, it says nothing about the state, and `current` passes it by too.
    pub(super) fn latest_state(&self, id: &str) -> Option<String> {
        self.lock()
            .rows
            .iter()
            .rev()
            .find(|r| r.id == id && !r.is_annotation())
            .map(|r| r.state.clone())
    }

    /// The state and the time of the id's latest row that is a row of its own.
    pub(super) fn latest_row(&self, id: &str) -> Option<(String, String)> {
        self.lock()
            .rows
            .iter()
            .rev()
            .find(|r| r.id == id && !r.is_annotation())
            .map(|r| (r.state.clone(), r.utc.clone()))
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

    /// Reads a hand-kept ledger's lines in. A line whose lead or area is not registered, and an
    /// areas line of a code a registered lead already holds, are skipped and listed with the file
    /// and line they came from; the rest of the file is taken. Nothing is written when `dry_run`.
    /// Returns the lines a write takes (or would take) and the lines skipped, both in the order
    /// of the files.
    fn import(
        &self,
        ledger: Ledger,
        dry_run: bool,
    ) -> Result<(Vec<LedgerRow>, Vec<SkippedRow>), ManagerError> {
        let mut st = self.lock();
        st.refuse_if_read_only()?;
        // What registers a code: an ordinary row of its kind, in the store or in this file. An
        // annotation row registers nothing; the id it names is still checked against what is
        // registered.
        let ordinary = |l: &LedgerRow| !l.is_annotation();
        let mut leads: HashSet<String> = st
            .rows
            .iter()
            .filter(|r| r.kind == Kind::Lead && !r.is_annotation())
            .map(|r| r.id.clone())
            .collect();
        for l in ledger.rows.iter().filter(|l| ordinary(l)) {
            if l.row.kind == Kind::Lead {
                leads.insert(l.row.id.clone());
            }
        }
        let mut areas: HashSet<String> = st
            .rows
            .iter()
            .filter(|r| r.kind == Kind::Area && !r.is_annotation())
            .map(|r| r.id.clone())
            .collect();
        for l in ledger.rows.iter().filter(|l| ordinary(l)) {
            if l.row.kind == Kind::Area && leads.contains(&l.row.by) {
                areas.insert(l.row.id.clone());
            }
        }
        let mut kept: Vec<LedgerRow> = Vec::new();
        let mut skipped = ledger.skipped;
        for l in ledger.rows {
            let prefix = parse_id(&l.row.id).map(|p| p.prefix).unwrap_or_default();
            let missing = match l.row.kind {
                Kind::Run | Kind::Change | Kind::Version if !leads.contains(&prefix) => {
                    Some(format!("lead {prefix} is not registered"))
                }
                Kind::Finding if !areas.contains(&prefix) => {
                    Some(format!("area {prefix} is not registered"))
                }
                Kind::Area if ordinary(&l) && !leads.contains(&l.row.by) => {
                    Some(format!("lead {} is not registered", l.row.by))
                }
                _ => None,
            };
            match missing {
                // The file keeps going: a lead the operator has not registered is their
                // business, not a reason to refuse the lines around it.
                Some(reason) => skipped.push(SkippedRow {
                    file: l.source.file,
                    line: l.source.line,
                    reason,
                }),
                None => kept.push(l),
            }
        }
        // A code both a lead's and an area's is a real clash: every id of either kind it owns
        // would be ambiguous. A ledger line that takes such a code as a lead is still refused
        // whole, and nothing is written. An areas line of it registers nothing and is listed, so
        // the areas around it are still taken: one such line is no reason to lose a file.
        let mut taken: Vec<LedgerRow> = Vec::new();
        for l in kept {
            if ordinary(&l) && l.row.kind == Kind::Area && leads.contains(&l.row.id) {
                skipped.push(SkippedRow {
                    file: l.source.file,
                    line: l.source.line,
                    reason: lead_code_clash(&l.row.id),
                });
                continue;
            }
            if ordinary(&l) && l.row.kind == Kind::Lead && areas.contains(&l.row.id) {
                return Err(invalid(format!(
                    "\"{}\" is an area code: a lead code must differ from every area code",
                    l.row.id
                )));
            }
            taken.push(l);
        }
        let mut plan = import_plan(&st.rows, taken);
        for l in &mut plan {
            l.row.declared_by = Some("import".to_string());
        }
        if !dry_run && !plan.is_empty() {
            let mut rows: Vec<IdRow> = plan.iter().map(|l| l.row.clone()).collect();
            st.write(&self.dir, &mut rows)?;
        }
        skipped.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        Ok((plan, skipped))
    }
}

#[cfg(test)]
impl Ids {
    /// The seam of [`State::write_and_sync`]: the next append puts `after` bytes on disk and
    /// then answers an error, as a disk that fills in the middle of a line.
    fn fail_next_write(&self, after: usize) {
        self.lock().fail_write_after = Some(after);
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
    /// The name the annotation and skip listings quote the lines by. Only the text of a ledger
    /// is read: a path here is the caller's own, nothing of it is opened.
    #[serde(default)]
    pub file: String,
    /// Say what the import would take; write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The name a ledger's lines are quoted by: the last component of the name the caller gave, or
/// the name of the format when none was given.
fn ledger_name(given: &str, areas: bool) -> String {
    let g = given.trim();
    if g.is_empty() {
        return if areas { "areas.tsv" } else { "ids.tsv" }.to_string();
    }
    Path::new(g)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| g.to_string())
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
        let file = ledger_name(&req.file, req.areas);
        let ledger = if req.areas {
            parse_areas_tsv(&file, &req.tsv)
        } else {
            parse_tsv(&file, &req.tsv)
        };
        let (plan, skipped) = self.ids().import(ledger, req.dry_run)?;
        let annotations: Vec<Value> = plan
            .iter()
            .filter(|l| l.is_annotation())
            .map(|l| {
                json!({
                    "file": l.source.file,
                    "line": l.source.line,
                    "id": l.row.id,
                    "reason": l.why,
                })
            })
            .collect();
        // A line that gives a state the same file already gave under another title stays the
        // row it is, but the change of words is worth a line of its own in the summary.
        let mut titled: HashMap<(String, String, String), (usize, String)> = HashMap::new();
        let mut title_edits: Vec<Value> = Vec::new();
        for l in plan.iter().filter(|l| !l.is_annotation()) {
            if l.row.title.is_empty() {
                continue;
            }
            let key = (l.source.file.clone(), l.row.id.clone(), l.row.state.clone());
            let earlier = titled
                .get(&key)
                .map(|(line, title)| (l.row.title != *title, *line));
            match earlier {
                Some((true, line)) => title_edits.push(json!({
                    "file": l.source.file,
                    "line": l.source.line,
                    "id": l.row.id,
                    "earlierLine": line,
                })),
                Some((false, _)) => {}
                None => {
                    titled.insert(key, (l.source.line, l.row.title.clone()));
                }
            }
        }
        Ok(json!({
            "imported": plan.len(),
            "annotations": annotations,
            "titleEdits": title_edits,
            "skipped": skipped,
        }))
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
                    "no change {change}: {}",
                    change_hint(lead)
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
                    // Only a full commit id is asked of git: anything else would be taken as
                    // an option or as a ref of this repository, and its answer would be a lie.
                    if is_full_sha(&base) {
                        out["descendsFromBase"] = match git_code(
                            repo,
                            &["merge-base", "--is-ancestor", "--", &base, &full],
                        )
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

/// How a work job gets the change it is missing, spelled out for the lead it works for.
pub(super) fn change_hint(lead: &str) -> String {
    format!(
        "a work job needs --change {lead}-C<nnn>; open one with: lagent id new change --lead {lead} --title \"<what the change does>\""
    )
}

/// A full commit id: exactly 40 lowercase hex characters, the shape `git rev-parse HEAD`
/// answers. Nothing of another shape may be handed to git as a revision.
fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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

    #[test]
    fn an_append_that_stops_mid_line_freezes_the_store_and_the_number_is_not_reissued() {
        let d = dir();
        let ids = open(d.path());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        let one_line = std::fs::read_to_string(file(d.path())).unwrap();
        let last_hash = sha256_hex(one_line.trim_end().as_bytes());

        // The first byte reaches the file, the rest of the line does not.
        ids.fail_next_write(1);
        let err = ids
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("one"))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("cannot write the id store: "),
            "the append's error: {err}"
        );
        let partial = std::fs::read_to_string(file(d.path())).unwrap();
        assert_eq!(partial, one_line + "{", "a prefix of the line is on disk");

        // The store is read-only for the life of the process, with that very error, and
        // nothing was remembered of the attempt: no row, and `last_hash` still the old line.
        let st = ids.lock();
        assert_eq!(st.read_only.as_deref(), Some(err.as_str()));
        assert_eq!(st.rows.len(), 1, "the failed row is not in memory");
        assert_eq!(st.lines, 1);
        assert_eq!(st.last_hash, last_hash);
        drop(st);

        // Every write route refuses, so no second start hands out `LA-R001` again.
        let e = ids
            .issue(Kind::Run, "LA", None, "daemon", |_| run_row("two"))
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert_eq!(e, err, "issue");
        let e = ids
            .append(
                "LA-R001",
                IdRow {
                    state: "verified".into(),
                    ..IdRow::default()
                },
                "daemon",
            )
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert_eq!(e, err, "append");
        let e = ids
            .import(
                parse_tsv(
                    "LA.tsv",
                    "LA-R009\t2026-11-18T09:00:00Z\trun\tstarted\timported\n",
                ),
                false,
            )
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert_eq!(e, err, "import");
        assert_eq!(
            std::fs::read_to_string(file(d.path())).unwrap(),
            partial,
            "nothing more was written"
        );
        // A restart finds the partial line and stays read-only: the number is still unissued.
        assert!(open(d.path()).lock().read_only.is_some());
    }

    /// `LB.tsv:54` is the last ordinary row of `LB-R012` and `LB.tsv:138` the last row of all:
    /// `removed` is no run state, so that line is kept as an annotation.
    #[test]
    fn the_latest_row_of_an_id_skips_its_annotations() {
        let d = dir();
        let ids = open(d.path());
        ids.issue(Kind::Lead, "LA", None, "lead", |_| lead_row("LA"))
            .unwrap();
        ids.issue(Kind::Run, "LA", None, "daemon", |_| run_row("one"))
            .unwrap();
        ids.import(
            parse_tsv(
                "LA.tsv",
                "LA-R001\t2026-11-17T14:18:56Z\trun\tremoved\t/home/nez/scratch/amazon-p1-target\n",
            ),
            false,
        )
        .unwrap();
        let st = ids.lock();
        assert_eq!(st.rows.len(), 3);
        assert!(st.rows[2].is_annotation(), "the line is an annotation");
        drop(st);
        assert_eq!(ids.latest_state("LA-R001").as_deref(), Some("started"));
        assert_eq!(
            ids.latest_row("LA-R001").map(|(s, _)| s).as_deref(),
            Some("started")
        );
    }
}
