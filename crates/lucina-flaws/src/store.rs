//! The flaw database: one SQLite file with `flaws`, `flaw_targets` and `flaw_events`.
//! The server, the `lucina-rs flaws` CLI and agentd's calls to it share the file (WAL, busy
//! timeout). `flaw_events` is append-only, enforced by triggers.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    check_transition, Evidence, FlawError, FlawState, Gate, Layer, Platform, Result, Severity,
    Step, TargetStatus,
};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS flaws (
    id TEXT PRIMARY KEY,
    words TEXT NOT NULL,
    title TEXT NOT NULL,
    severity TEXT NOT NULL,
    state TEXT NOT NULL,
    duplicate_of TEXT REFERENCES flaws(id),
    discovered_from TEXT REFERENCES flaws(id),
    thread TEXT,
    reporter TEXT NOT NULL,
    gate TEXT,
    decision_id TEXT,
    triage_agent TEXT,
    triage_step INTEGER,
    triaged_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS flaw_targets (
    flaw_id TEXT NOT NULL REFERENCES flaws(id),
    platform TEXT NOT NULL,
    layer TEXT,
    status TEXT NOT NULL,
    evidence TEXT,
    workspace TEXT,
    agent TEXT,
    session TEXT,
    repo TEXT,
    commit_sha TEXT,
    deploy_tag TEXT,
    build TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    claimed_at TEXT,
    told_status TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (flaw_id, platform)
);
CREATE INDEX IF NOT EXISTS flaw_targets_status ON flaw_targets(status);
CREATE TABLE IF NOT EXISTS flaw_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    flaw_id TEXT NOT NULL REFERENCES flaws(id),
    platform TEXT,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail TEXT NOT NULL,
    at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS flaw_events_flaw ON flaw_events(flaw_id, seq);
CREATE TRIGGER IF NOT EXISTS flaw_events_no_update BEFORE UPDATE ON flaw_events
    BEGIN SELECT RAISE(ABORT, 'flaw_events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS flaw_events_no_delete BEFORE DELETE ON flaw_events
    BEGIN SELECT RAISE(ABORT, 'flaw_events is append-only'); END;
"#;

/// The owner's words, at most.
const MAX_WORDS: usize = 8000;
const MAX_TITLE: usize = 120;

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn new_id() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("the OS has randomness");
    format!(
        "fl-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// A flaw id: `fl-` and 8 lowercase hex digits.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("fl-").is_some_and(|h| {
        h.len() == 8
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The title when none is given: the first line of his words.
pub fn title_from(words: &str) -> String {
    clip(
        words
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(""),
        MAX_TITLE,
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flaw {
    pub id: String,
    /// The owner's exact words.
    pub words: String,
    pub title: String,
    pub severity: Severity,
    pub state: FlawState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// The flaw whose work turned this one up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovered_from: Option<String>,
    /// The conversation or thread it came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub reporter: String,
    /// Why its fix needs the owner's go; `None`: it starts without asking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
    /// The `lucina_decide` question asked for the gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// The agent looking at which clients have it, or `manual`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage_agent: Option<String>,
    /// Which step of the chain that agent runs on, counted from 0. `None`: it was set before the
    /// chain, which is the first step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage_step: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triaged_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub flaw_id: String,
    pub platform: Platform,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    pub status: TargetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The Lucina Agents agent the dispatcher started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// A session that took it on by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(rename = "commit", skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub flaw_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    pub kind: String,
    pub actor: String,
    pub detail: Value,
    pub at: String,
}

/// A flaw with its targets and history.
#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub flaw: Flaw,
    pub targets: Vec<Target>,
    pub events: Vec<Event>,
}

/// What a report brings. Every tracked platform gets a row; `affected` start as affected
/// (the owner saw it there), the rest as untriaged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFlaw {
    pub words: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub affected: Vec<Platform>,
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub discovered_from: Option<String>,
    #[serde(default)]
    pub gate: Option<Gate>,
}

/// A new flaw and older open ones that use the same words (nothing is merged by itself).
#[derive(Debug, Clone, Serialize)]
pub struct Reported {
    pub flaw: Detail,
    pub possible_duplicates: Vec<Flaw>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlawPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Marks it a duplicate of another flaw; the dispatcher stops working on it.
    #[serde(default)]
    pub duplicate_of: Option<String>,
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub gate: Option<Gate>,
    #[serde(default)]
    pub decision_id: Option<String>,
    /// Why nobody will fix it: the state becomes `dismissed`.
    #[serde(default)]
    pub dismissed: Option<String>,
}

/// A change to one target. `None` leaves a field alone.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetUpdate {
    #[serde(default)]
    pub status: Option<TargetStatus>,
    #[serde(default)]
    pub layer: Option<Layer>,
    #[serde(default)]
    pub evidence: Option<Evidence>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub deploy_tag: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub q: Option<String>,
    pub state: Option<FlawState>,
    pub platform: Option<Platform>,
    pub status: Option<TargetStatus>,
    pub limit: Option<u32>,
}

/// A target that changed to something the owner hears about.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub flaw: Flaw,
    pub target: Target,
}

pub struct Flaws {
    conn: Mutex<Connection>,
}

fn parse_opt<T>(v: Option<String>, f: impl Fn(&str) -> Result<T>) -> Option<T> {
    v.and_then(|s| f(&s).ok())
}

fn flaw_row(r: &Row<'_>) -> rusqlite::Result<Flaw> {
    Ok(Flaw {
        id: r.get("id")?,
        words: r.get("words")?,
        title: r.get("title")?,
        severity: parse_opt(r.get("severity")?, Severity::parse).unwrap_or(Severity::Low),
        state: parse_opt(r.get("state")?, FlawState::parse).unwrap_or(FlawState::Open),
        duplicate_of: r.get("duplicate_of")?,
        discovered_from: r.get("discovered_from")?,
        thread: r.get("thread")?,
        reporter: r.get("reporter")?,
        gate: parse_opt(r.get("gate")?, Gate::parse),
        decision_id: r.get("decision_id")?,
        triage_agent: r.get("triage_agent")?,
        triage_step: r.get("triage_step")?,
        triaged_at: r.get("triaged_at")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn target_row(r: &Row<'_>) -> rusqlite::Result<Target> {
    let evidence: Option<String> = r.get("evidence")?;
    Ok(Target {
        flaw_id: r.get("flaw_id")?,
        platform: parse_opt(r.get("platform")?, Platform::parse).unwrap_or(Platform::Web),
        layer: parse_opt(r.get("layer")?, Layer::parse),
        status: parse_opt(r.get("status")?, TargetStatus::parse).unwrap_or(TargetStatus::Untriaged),
        evidence: evidence.and_then(|e| serde_json::from_str(&e).ok()),
        workspace: r.get("workspace")?,
        agent: r.get("agent")?,
        session: r.get("session")?,
        repo: r.get("repo")?,
        commit_sha: r.get("commit_sha")?,
        deploy_tag: r.get("deploy_tag")?,
        build: r.get("build")?,
        attempts: r.get("attempts")?,
        claimed_at: r.get("claimed_at")?,
        updated_at: r.get("updated_at")?,
    })
}

fn event(
    c: &Connection,
    flaw: &str,
    platform: Option<Platform>,
    kind: &str,
    actor: &str,
    detail: Value,
) -> Result<()> {
    c.execute(
        "INSERT INTO flaw_events (flaw_id, platform, kind, actor, detail, at) VALUES (?1,?2,?3,?4,?5,?6)",
        params![flaw, platform.map(|p| p.as_str()), kind, actor, detail.to_string(), now()],
    )?;
    Ok(())
}

fn flaw_of(c: &Connection, id: &str) -> Result<Flaw> {
    c.query_row("SELECT * FROM flaws WHERE id = ?1", [id], flaw_row)
        .optional()?
        .ok_or(FlawError::NotFound)
}

fn targets_of(c: &Connection, id: &str) -> Result<Vec<Target>> {
    let mut s = c.prepare("SELECT * FROM flaw_targets WHERE flaw_id = ?1")?;
    let mut out = s
        .query_map([id], target_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The order the platforms are declared in, so every answer reads the same.
    out.sort_by_key(|t| t.platform);
    Ok(out)
}

fn target_of(c: &Connection, id: &str, p: Platform) -> Result<Target> {
    c.query_row(
        "SELECT * FROM flaw_targets WHERE flaw_id = ?1 AND platform = ?2",
        params![id, p.as_str()],
        target_row,
    )
    .optional()?
    .ok_or(FlawError::NotFound)
}

/// A flaw's state follows from its targets, unless it is a duplicate or dismissed. `resolved`
/// needs every target closed, and each closed target went through the evidence rule.
fn recompute_state(c: &Connection, id: &str, actor: &str) -> Result<()> {
    let f = flaw_of(c, id)?;
    if matches!(f.state, FlawState::Duplicate | FlawState::Dismissed) {
        return Ok(());
    }
    let ts = targets_of(c, id)?;
    let state = if ts.iter().any(|t| t.status == TargetStatus::Fixing) {
        FlawState::Fixing
    } else if !ts.is_empty() && ts.iter().all(|t| t.status.closed()) {
        FlawState::Resolved
    } else {
        FlawState::Open
    };
    if state != f.state {
        c.execute(
            "UPDATE flaws SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![state.as_str(), now(), id],
        )?;
        event(
            c,
            id,
            None,
            "state",
            actor,
            json!({"from": f.state, "to": state}),
        )?;
    }
    Ok(())
}

fn words_of(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect()
}

/// A database from before the chain: `CREATE TABLE IF NOT EXISTS` never changes a table that is
/// already there, so the step a triage agent ran on is added here. Nothing in it is a step yet.
fn migrate(c: &Connection) -> Result<()> {
    let has = c
        .prepare("SELECT 1 FROM pragma_table_info('flaws') WHERE name = 'triage_step'")?
        .exists([])?;
    if !has {
        c.execute("ALTER TABLE flaws ADD COLUMN triage_step INTEGER", [])?;
    }
    Ok(())
}

impl Flaws {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                FlawError::Invalid(format!("no directory for the flaw database: {e}"))
            })?;
        }
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(c: Connection) -> Result<Self> {
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch(SCHEMA)?;
        migrate(&c)?;
        Ok(Self {
            conn: Mutex::new(c),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut c = self.lock();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Record a flaw: his words as they are, one row per tracked platform.
    pub fn report(&self, new: NewFlaw, reporter: &str) -> Result<Reported> {
        let words = new.words.trim().to_owned();
        if words.is_empty() {
            return Err(FlawError::Invalid("a flaw needs the owner's words".into()));
        }
        if words.chars().count() > MAX_WORDS {
            return Err(FlawError::Invalid(format!(
                "words must be at most {MAX_WORDS} characters"
            )));
        }
        let title = new
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map_or_else(|| title_from(&words), |t| clip(t, MAX_TITLE));
        let id = self.tx(|c| {
            if let Some(from) = &new.discovered_from {
                flaw_of(c, from)?;
            }
            let id = new_id();
            let at = now();
            c.execute(
                "INSERT INTO flaws (id, words, title, severity, state, discovered_from, thread, reporter, gate, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,'open',?5,?6,?7,?8,?9,?9)",
                params![
                    id,
                    words,
                    title,
                    new.severity.unwrap_or(Severity::Low).as_str(),
                    new.discovered_from,
                    new.thread,
                    reporter,
                    new.gate.map(|g| g.as_str()),
                    at
                ],
            )?;
            event(c, &id, None, "reported", reporter, json!({"title": title, "words": words, "discovered_from": new.discovered_from}))?;
            for p in Platform::ALL {
                let affected = new.affected.contains(p);
                let (status, evidence) = if affected {
                    let e = Evidence { kind: crate::EvidenceKind::OwnerReport, reference: new.thread.clone().unwrap_or_else(|| "his report".into()), build: None, note: None };
                    (TargetStatus::Affected, Some(serde_json::to_string(&e).expect("evidence serializes")))
                } else {
                    (TargetStatus::Untriaged, None)
                };
                c.execute(
                    "INSERT INTO flaw_targets (flaw_id, platform, status, evidence, updated_at) VALUES (?1,?2,?3,?4,?5)",
                    params![id, p.as_str(), status.as_str(), evidence, at],
                )?;
            }
            Ok(id)
        })?;
        let mine = words_of(&format!("{title} {words}"));
        let possible_duplicates = self
            .list(&ListQuery::default())?
            .into_iter()
            .filter(|f| f.id != id && matches!(f.state, FlawState::Open | FlawState::Fixing))
            .filter(|f| {
                let theirs = words_of(&format!("{} {}", f.title, f.words));
                mine.intersection(&theirs).count() * 2 >= mine.len().max(1)
            })
            .take(3)
            .collect();
        Ok(Reported {
            flaw: self.get(&id)?,
            possible_duplicates,
        })
    }

    pub fn get(&self, id: &str) -> Result<Detail> {
        let c = self.lock();
        let flaw = flaw_of(&c, id)?;
        let targets = targets_of(&c, id)?;
        let mut s = c.prepare("SELECT * FROM flaw_events WHERE flaw_id = ?1 ORDER BY seq")?;
        let events = s
            .query_map([id], |r| {
                let d: String = r.get("detail")?;
                Ok(Event {
                    seq: r.get("seq")?,
                    flaw_id: r.get("flaw_id")?,
                    platform: parse_opt(r.get("platform")?, Platform::parse),
                    kind: r.get("kind")?,
                    actor: r.get("actor")?,
                    detail: serde_json::from_str(&d).unwrap_or(Value::Null),
                    at: r.get("at")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Detail {
            flaw,
            targets,
            events,
        })
    }

    /// Worst first, then newest. Words match the title and the owner's words.
    pub fn list(&self, q: &ListQuery) -> Result<Vec<Flaw>> {
        let c = self.lock();
        let mut sql = String::from("SELECT DISTINCT f.* FROM flaws f LEFT JOIN flaw_targets t ON t.flaw_id = f.id WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if let Some(w) = q.q.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            args.push(format!(
                "%{}%",
                w.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            ));
            let n = args.len();
            sql.push_str(&format!(
                " AND (f.title LIKE ?{n} ESCAPE '\\' OR f.words LIKE ?{n} ESCAPE '\\')"
            ));
        }
        for (column, value) in [
            ("f.state", q.state.map(|s| s.as_str())),
            ("t.platform", q.platform.map(|p| p.as_str())),
            ("t.status", q.status.map(|s| s.as_str())),
        ] {
            if let Some(v) = value {
                args.push(v.to_owned());
                sql.push_str(&format!(" AND {column} = ?{}", args.len()));
            }
        }
        sql.push_str(" ORDER BY f.created_at DESC, f.id");
        let mut s = c.prepare(&sql)?;
        let mut out = s
            .query_map(rusqlite::params_from_iter(args.iter()), flaw_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        out.truncate(q.limit.unwrap_or(50).clamp(1, 200) as usize);
        Ok(out)
    }

    pub fn patch(&self, id: &str, p: FlawPatch, actor: &str) -> Result<Flaw> {
        self.tx(|c| {
            flaw_of(c, id)?;
            let at = now();
            if let Some(t) = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                c.execute(
                    "UPDATE flaws SET title = ?1 WHERE id = ?2",
                    params![clip(t, MAX_TITLE), id],
                )?;
                event(c, id, None, "title", actor, json!({"title": t}))?;
            }
            if let Some(s) = p.severity {
                c.execute(
                    "UPDATE flaws SET severity = ?1 WHERE id = ?2",
                    params![s.as_str(), id],
                )?;
                event(c, id, None, "severity", actor, json!({"severity": s}))?;
            }
            if let Some(t) = &p.thread {
                c.execute("UPDATE flaws SET thread = ?1 WHERE id = ?2", params![t, id])?;
                event(c, id, None, "thread", actor, json!({"thread": t}))?;
            }
            if let Some(g) = p.gate {
                c.execute(
                    "UPDATE flaws SET gate = ?1 WHERE id = ?2",
                    params![g.as_str(), id],
                )?;
                event(c, id, None, "gate", actor, json!({"gate": g}))?;
            }
            if let Some(d) = &p.decision_id {
                c.execute(
                    "UPDATE flaws SET decision_id = ?1 WHERE id = ?2",
                    params![d, id],
                )?;
                event(c, id, None, "decision", actor, json!({"decision_id": d}))?;
            }
            if let Some(of) = &p.duplicate_of {
                if of == id {
                    return Err(FlawError::Invalid(
                        "a flaw can't be a duplicate of itself".into(),
                    ));
                }
                // No cycles: the flaw it points at must not lead back here.
                let mut at_flaw = flaw_of(c, of)?;
                while let Some(next) = at_flaw.duplicate_of.clone() {
                    if next == id {
                        return Err(FlawError::Invalid(format!(
                            "{of} is already a duplicate of {id}"
                        )));
                    }
                    at_flaw = flaw_of(c, &next)?;
                }
                c.execute(
                    "UPDATE flaws SET duplicate_of = ?1, state = 'duplicate' WHERE id = ?2",
                    params![of, id],
                )?;
                event(c, id, None, "duplicate", actor, json!({"of": of}))?;
            }
            if let Some(why) = p
                .dismissed
                .as_deref()
                .map(str::trim)
                .filter(|w| !w.is_empty())
            {
                c.execute(
                    "UPDATE flaws SET state = 'dismissed' WHERE id = ?1",
                    params![id],
                )?;
                event(c, id, None, "dismissed", actor, json!({"why": why}))?;
            }
            c.execute(
                "UPDATE flaws SET updated_at = ?1 WHERE id = ?2",
                params![at, id],
            )?;
            flaw_of(c, id)
        })
    }

    /// Change one target. The evidence rule is checked here, so no caller can skip it.
    pub fn set_target(
        &self,
        id: &str,
        p: Platform,
        u: TargetUpdate,
        actor: &str,
    ) -> Result<Target> {
        self.tx(|c| {
            flaw_of(c, id)?;
            let cur = target_of(c, id, p)?;
            let commit = u.commit.as_deref().map(str::trim).filter(|s| !s.is_empty());
            if let Some(sha) = commit {
                if sha.len() < 7 || sha.len() > 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(FlawError::Invalid("commit must be 7-40 hex digits".into()));
                }
            }
            let has_commit = commit.is_some() || cur.commit_sha.is_some();
            if let Some(to) = u.status.filter(|to| *to != cur.status) {
                check_transition(cur.status, to, u.evidence.as_ref(), has_commit)?;
            } else if let Some(e) = &u.evidence {
                // Re-closing with weaker evidence would hide what closed it.
                if cur.status.closed() {
                    crate::check_closing_evidence(cur.status, Some(e))?;
                }
            }
            let status = u.status.unwrap_or(cur.status);
            let reopened = status != cur.status && !matches!(status, TargetStatus::Fixing);
            // Letting go of the target ends the claim: a new task may start.
            let (agent, session, claimed_at) = if status == TargetStatus::Fixing {
                (cur.agent.clone(), u.session.clone().or(cur.session.clone()), cur.claimed_at.clone().or_else(|| Some(now())))
            } else if reopened {
                (None, None, None)
            } else {
                (cur.agent.clone(), u.session.clone().or(cur.session.clone()), cur.claimed_at.clone())
            };
            let evidence = match &u.evidence {
                Some(e) => Some(serde_json::to_string(e).expect("evidence serializes")),
                None => cur.evidence.as_ref().map(|e| serde_json::to_string(e).expect("evidence serializes")),
            };
            c.execute(
                "UPDATE flaw_targets SET status=?1, layer=?2, evidence=?3, workspace=?4, session=?5, agent=?6, claimed_at=?7,
                    commit_sha=?8, deploy_tag=?9, build=?10, updated_at=?11 WHERE flaw_id=?12 AND platform=?13",
                params![
                    status.as_str(),
                    u.layer.or(cur.layer).map(|l| l.as_str()),
                    evidence,
                    u.workspace.clone().or(cur.workspace.clone()),
                    session,
                    agent,
                    claimed_at,
                    commit.map(str::to_owned).or(cur.commit_sha.clone()),
                    u.deploy_tag.clone().or(cur.deploy_tag.clone()),
                    u.build.clone().or(cur.build.clone()),
                    now(),
                    id,
                    p.as_str()
                ],
            )?;
            event(
                c,
                id,
                Some(p),
                "target",
                actor,
                json!({
                    "from": cur.status, "to": status, "layer": u.layer, "evidence": u.evidence,
                    "workspace": u.workspace, "session": u.session, "commit": commit,
                    "deploy_tag": u.deploy_tag, "build": u.build, "note": u.note,
                }),
            )?;
            recompute_state(c, id, actor)?;
            target_of(c, id, p)
        })
    }

    /// Claim a target for a new task, atomically: only one caller gets it. `affected` with a
    /// layer, on a live flaw, held by nobody.
    pub fn claim(&self, id: &str, p: Platform, repo: &str, actor: &str) -> Result<Target> {
        self.tx(|c| {
            let f = flaw_of(c, id)?;
            if matches!(f.state, FlawState::Duplicate | FlawState::Dismissed) {
                return Err(FlawError::NotClaimable(format!("{id} is {}", f.state)));
            }
            let t = target_of(c, id, p)?;
            if t.status != TargetStatus::Affected {
                return Err(FlawError::NotClaimable(format!("{id} on {p} is {}, not affected", t.status)));
            }
            if t.layer.is_none() {
                return Err(FlawError::NotClaimable(format!("{id} on {p} has no layer yet (server, core or client)")));
            }
            let at = now();
            let n = c.execute(
                "UPDATE flaw_targets SET status='fixing', repo=?1, claimed_at=?2, attempts=attempts+1, updated_at=?2
                 WHERE flaw_id=?3 AND platform=?4 AND status='affected' AND agent IS NULL AND session IS NULL",
                params![repo, at, id, p.as_str()],
            )?;
            if n == 0 {
                return Err(FlawError::NotClaimable(format!("{id} on {p} is held already")));
            }
            event(c, id, Some(p), "claimed", actor, json!({"repo": repo, "attempt": t.attempts + 1}))?;
            recompute_state(c, id, actor)?;
            target_of(c, id, p)
        })
    }

    /// The task and agent a claim turned into, and which step of the chain runs it: his history
    /// says what the work was done with.
    pub fn attach(
        &self,
        id: &str,
        p: Platform,
        workspace: &str,
        agent: &str,
        run: &Step,
        actor: &str,
    ) -> Result<Target> {
        self.tx(|c| {
            c.execute(
                "UPDATE flaw_targets SET workspace=?1, agent=?2, updated_at=?3 WHERE flaw_id=?4 AND platform=?5",
                params![workspace, agent, now(), id, p.as_str()],
            )?;
            let started_by = json!({"workspace": workspace, "agent": agent, "provider": run.provider, "model": run.model, "probe_lane": run.probe_lane});
            event(c, id, Some(p), "started", actor, started_by)?;
            target_of(c, id, p)
        })
    }

    /// A claimed task ended without a fix: back to `affected` for another try, or `blocked`
    /// after `max_attempts`.
    pub fn release(
        &self,
        id: &str,
        p: Platform,
        why: &str,
        max_attempts: u32,
        actor: &str,
    ) -> Result<Target> {
        self.tx(|c| {
            let t = target_of(c, id, p)?;
            if t.status != TargetStatus::Fixing {
                return Ok(t);
            }
            let to = if t.attempts >= max_attempts { TargetStatus::Blocked } else { TargetStatus::Affected };
            c.execute(
                "UPDATE flaw_targets SET status=?1, agent=NULL, session=NULL, workspace=NULL, claimed_at=NULL, updated_at=?2
                 WHERE flaw_id=?3 AND platform=?4",
                params![to.as_str(), now(), id, p.as_str()],
            )?;
            event(c, id, Some(p), "released", actor, json!({"to": to, "why": why, "attempts": t.attempts}))?;
            recompute_state(c, id, actor)?;
            target_of(c, id, p)
        })
    }

    /// The task never got to work (agentd down, the lane out of quota): the claim goes back and
    /// the try doesn't count, so an outage can't block a flaw.
    pub fn give_back(&self, id: &str, p: Platform, why: &str, actor: &str) -> Result<Target> {
        self.tx(|c| {
            let t = target_of(c, id, p)?;
            if t.status != TargetStatus::Fixing || t.session.is_some() {
                return Ok(t);
            }
            c.execute(
                "UPDATE flaw_targets SET status='affected', agent=NULL, workspace=NULL, claimed_at=NULL, attempts=MAX(attempts-1,0), updated_at=?1
                 WHERE flaw_id=?2 AND platform=?3",
                params![now(), id, p.as_str()],
            )?;
            event(c, id, Some(p), "not_started", actor, json!({"why": why}))?;
            recompute_state(c, id, actor)?;
            target_of(c, id, p)
        })
    }

    /// `lagent merge` landed: the commit, and `fixed` if it was being worked on. Repeats are fine.
    /// A server or core fix is one fix: every target of the flaw waiting on that layer gets it.
    pub fn record_merge(&self, id: &str, p: Platform, commit: &str, actor: &str) -> Result<Target> {
        let targets = self.get(id)?.targets;
        let cur = targets
            .iter()
            .find(|t| t.platform == p)
            .ok_or(FlawError::NotFound)?
            .clone();
        let shared = cur
            .layer
            .filter(|l| matches!(l, Layer::Server | Layer::Core));
        for t in targets
            .iter()
            .filter(|t| t.platform != p && shared.is_some() && t.layer == shared)
        {
            if matches!(t.status, TargetStatus::Fixing | TargetStatus::Affected) {
                let note = format!("merged with {p}: the same fix");
                self.set_target(
                    id,
                    t.platform,
                    TargetUpdate {
                        status: Some(TargetStatus::Fixed),
                        commit: Some(commit.to_owned()),
                        note: Some(note),
                        ..TargetUpdate::default()
                    },
                    actor,
                )?;
            }
        }
        let status = matches!(cur.status, TargetStatus::Fixing | TargetStatus::Affected)
            .then_some(TargetStatus::Fixed);
        self.set_target(
            id,
            p,
            TargetUpdate {
                status,
                commit: Some(commit.to_owned()),
                note: Some("merged".into()),
                ..TargetUpdate::default()
            },
            actor,
        )
    }

    /// `lagent deploy` recorded a tag holding this target's fix.
    pub fn record_deploy(&self, id: &str, p: Platform, tag: &str, actor: &str) -> Result<Target> {
        self.set_target(
            id,
            p,
            TargetUpdate {
                deploy_tag: Some(tag.to_owned()),
                note: Some("deployed".into()),
                ..TargetUpdate::default()
            },
            actor,
        )
    }

    fn pairs(&self, wher: &str) -> Result<Vec<(Flaw, Target)>> {
        let c = self.lock();
        let mut s = c.prepare(&format!("SELECT flaw_id, platform FROM flaw_targets t JOIN flaws f ON f.id = t.flaw_id WHERE {wher}"))?;
        let keys = s
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(keys.len());
        for (id, p) in keys {
            out.push((flaw_of(&c, &id)?, target_of(&c, &id, Platform::parse(&p)?)?));
        }
        Ok(out)
    }

    /// Targets ready for a task: affected, with a layer, held by nobody, on a live flaw.
    pub fn due(&self) -> Result<Vec<(Flaw, Target)>> {
        self.pairs("t.status = 'affected' AND t.layer IS NOT NULL AND t.agent IS NULL AND t.session IS NULL AND f.state IN ('open','fixing') \
             AND NOT (t.layer IN ('server','core') AND EXISTS (SELECT 1 FROM flaw_targets o WHERE o.flaw_id = t.flaw_id AND o.layer = t.layer AND o.status = 'fixing'))")
    }

    /// Everything being worked on, by agents and by sessions.
    pub fn in_progress(&self) -> Result<Vec<(Flaw, Target)>> {
        self.pairs("t.status = 'fixing'")
    }

    /// Live flaws nobody has triaged, with a client still unlooked at or without a layer.
    pub fn triage_due(&self) -> Result<Vec<Flaw>> {
        let c = self.lock();
        let mut s = c.prepare(
            "SELECT DISTINCT f.* FROM flaws f JOIN flaw_targets t ON t.flaw_id = f.id
             WHERE f.state IN ('open','fixing') AND f.triage_agent IS NULL
               AND (t.status = 'untriaged' OR (t.status = 'affected' AND t.layer IS NULL))
             ORDER BY f.created_at",
        )?;
        let out = s
            .query_map([], flaw_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// Triage agents not finished yet.
    pub fn triaging(&self) -> Result<Vec<Flaw>> {
        let c = self.lock();
        let mut s =
            c.prepare("SELECT * FROM flaws WHERE triage_agent IS NOT NULL AND triaged_at IS NULL")?;
        let out = s
            .query_map([], flaw_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    /// An agent is looking at it; or, `done`, someone already did (`manual`). `step`: which step of
    /// the chain that agent runs on, so a limit of its lane is remembered for the next round.
    pub fn start_triage(
        &self,
        id: &str,
        agent: &str,
        done: bool,
        step: Option<usize>,
        actor: &str,
    ) -> Result<()> {
        self.tx(|c| {
            let at = now();
            c.execute(
                "UPDATE flaws SET triage_agent=?1, triage_step=?2, triaged_at=?3, updated_at=?4 WHERE id=?5",
                params![agent, step.map(|s| s as i64), done.then_some(at.clone()), at, id],
            )?;
            event(c, id, None, "triage", actor, json!({"agent": agent, "done": done}))
        })
    }

    /// The triage agent never got to work: another one may start.
    pub fn reset_triage(&self, id: &str, why: &str, actor: &str) -> Result<()> {
        self.tx(|c| {
            c.execute(
                "UPDATE flaws SET triage_agent=NULL, triage_step=NULL, triaged_at=NULL WHERE id=?1",
                params![id],
            )?;
            event(c, id, None, "triage_reset", actor, json!({"why": why}))
        })
    }

    pub fn finish_triage(&self, id: &str, why: &str, actor: &str) -> Result<()> {
        self.tx(|c| {
            c.execute(
                "UPDATE flaws SET triaged_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
            event(c, id, None, "triaged", actor, json!({"why": why}))
        })
    }

    /// What changed since the owner last heard: fixed, closed or blocked targets.
    pub fn untold(&self) -> Result<Vec<Outcome>> {
        Ok(self
            .pairs("t.status IN ('fixed','verified','not_affected','blocked') AND (t.told_status IS NULL OR t.told_status <> t.status)")?
            .into_iter()
            .map(|(flaw, target)| Outcome { flaw, target })
            .collect())
    }

    pub fn mark_told(&self, id: &str, p: Platform, status: TargetStatus) -> Result<()> {
        self.lock().execute(
            "UPDATE flaw_targets SET told_status=?1 WHERE flaw_id=?2 AND platform=?3",
            params![status.as_str(), id, p.as_str()],
        )?;
        Ok(())
    }

    /// One row per live flaw, one cell per platform, for the owner to read at a glance.
    pub fn matrix(&self) -> Result<Value> {
        let flaws = self.list(&ListQuery {
            limit: Some(200),
            ..ListQuery::default()
        })?;
        let c = self.lock();
        let mut rows = Vec::new();
        for f in flaws
            .iter()
            .filter(|f| !matches!(f.state, FlawState::Duplicate | FlawState::Dismissed))
        {
            let mut cells = serde_json::Map::new();
            for t in targets_of(&c, &f.id)? {
                cells.insert(
                    t.platform.as_str().to_owned(),
                    json!({
                        "status": t.status, "layer": t.layer, "evidence": t.evidence.as_ref().map(|e| e.kind),
                        "commit": t.commit_sha, "deploy_tag": t.deploy_tag, "workspace": t.workspace,
                        "agent": t.agent, "session": t.session,
                    }),
                );
            }
            rows.push(json!({"id": f.id, "title": f.title, "severity": f.severity, "state": f.state, "cells": cells}));
        }
        Ok(json!({"platforms": Platform::ALL, "rows": rows}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EvidenceKind;

    fn report(s: &Flaws, words: &str, affected: &[Platform]) -> Flaw {
        s.report(
            NewFlaw {
                words: words.into(),
                affected: affected.to_vec(),
                ..NewFlaw::default()
            },
            "owner",
        )
        .unwrap()
        .flaw
        .flaw
    }

    fn layer(s: &Flaws, id: &str, p: Platform) {
        s.set_target(
            id,
            p,
            TargetUpdate {
                layer: Some(Layer::Client),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
    }

    fn shot() -> Evidence {
        Evidence {
            kind: EvidenceKind::Screenshot,
            reference: "a.png".into(),
            build: None,
            note: None,
        }
    }

    #[test]
    fn a_report_keeps_his_words_and_opens_every_platform() {
        let s = Flaws::in_memory().unwrap();
        let d = s
            .report(
                NewFlaw {
                    words: "  gli emoji sono quadratini\nsu Juno ".into(),
                    affected: vec![Platform::Juno],
                    ..NewFlaw::default()
                },
                "owner",
            )
            .unwrap()
            .flaw;
        assert_eq!(d.flaw.words, "gli emoji sono quadratini\nsu Juno");
        assert_eq!(d.flaw.title, "gli emoji sono quadratini");
        assert_eq!(d.flaw.severity, Severity::Low);
        assert_eq!(d.targets.len(), Platform::ALL.len());
        let juno = d
            .targets
            .iter()
            .find(|t| t.platform == Platform::Juno)
            .unwrap();
        assert_eq!(juno.status, TargetStatus::Affected);
        assert_eq!(
            juno.evidence.as_ref().unwrap().kind,
            EvidenceKind::OwnerReport
        );
        assert!(d
            .targets
            .iter()
            .filter(|t| t.platform != Platform::Juno)
            .all(|t| t.status == TargetStatus::Untriaged));
        assert!(s
            .report(
                NewFlaw {
                    words: "  ".into(),
                    ..NewFlaw::default()
                },
                "o"
            )
            .is_err());
    }

    #[test]
    fn the_store_refuses_closing_without_evidence() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[]);
        let e = s
            .set_target(
                &f.id,
                Platform::Web,
                TargetUpdate {
                    status: Some(TargetStatus::NotAffected),
                    ..TargetUpdate::default()
                },
                "t",
            )
            .unwrap_err();
        assert_eq!(e.code(), "evidence_required");
        let read = Evidence {
            kind: EvidenceKind::CodeReading,
            reference: "rich.rs".into(),
            build: None,
            note: None,
        };
        let e = s
            .set_target(
                &f.id,
                Platform::Web,
                TargetUpdate {
                    status: Some(TargetStatus::NotAffected),
                    evidence: Some(read.clone()),
                    ..TargetUpdate::default()
                },
                "t",
            )
            .unwrap_err();
        assert_eq!(e.code(), "evidence_required");
        // Nothing moved, nothing logged for the refusals.
        let d = s.get(&f.id).unwrap();
        assert_eq!(
            d.targets
                .iter()
                .find(|t| t.platform == Platform::Web)
                .unwrap()
                .status,
            TargetStatus::Untriaged
        );
        assert_eq!(d.events.len(), 1);
        // Reading code opens it.
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::Affected),
                evidence: Some(read),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
    }

    #[test]
    fn a_closed_target_can_not_be_rewritten_with_weak_evidence() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[]);
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::NotAffected),
                evidence: Some(shot()),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        let read = Evidence {
            kind: EvidenceKind::CodeReading,
            reference: "rich.rs".into(),
            build: None,
            note: None,
        };
        assert_eq!(
            s.set_target(
                &f.id,
                Platform::Web,
                TargetUpdate {
                    evidence: Some(read),
                    ..TargetUpdate::default()
                },
                "t"
            )
            .unwrap_err()
            .code(),
            "evidence_required"
        );
    }

    #[test]
    fn resolved_only_when_every_target_closed() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[]);
        for p in Platform::ALL {
            assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Open, "{p}");
            s.set_target(
                &f.id,
                *p,
                TargetUpdate {
                    status: Some(TargetStatus::NotAffected),
                    evidence: Some(shot()),
                    ..TargetUpdate::default()
                },
                "t",
            )
            .unwrap();
        }
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Resolved);
        // A regression reopens it.
        s.set_target(
            &f.id,
            Platform::Ios,
            TargetUpdate {
                status: Some(TargetStatus::Affected),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Open);
    }

    #[test]
    fn events_are_append_only() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[]);
        let c = s.lock();
        assert!(c.execute("UPDATE flaw_events SET actor='me'", []).is_err());
        assert!(c.execute("DELETE FROM flaw_events", []).is_err());
        drop(c);
        assert_eq!(s.get(&f.id).unwrap().events[0].kind, "reported");
    }

    #[test]
    fn a_claim_is_taken_once_and_needs_a_layer() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web]);
        assert_eq!(
            s.claim(&f.id, Platform::Web, "/r", "d").unwrap_err().code(),
            "not_claimable"
        );
        assert!(s.due().unwrap().is_empty(), "no layer, not due");
        layer(&s, &f.id, Platform::Web);
        assert_eq!(s.due().unwrap().len(), 1);
        let t = s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        assert_eq!((t.status, t.attempts), (TargetStatus::Fixing, 1));
        assert_eq!(
            s.claim(&f.id, Platform::Web, "/r", "d").unwrap_err().code(),
            "not_claimable"
        );
        assert!(s.due().unwrap().is_empty());
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Fixing);
    }

    #[test]
    fn a_session_holding_a_target_stops_the_dispatcher() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Juno]);
        layer(&s, &f.id, Platform::Juno);
        s.set_target(
            &f.id,
            Platform::Juno,
            TargetUpdate {
                status: Some(TargetStatus::Fixing),
                session: Some("local_x".into()),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        assert!(s.due().unwrap().is_empty());
        assert_eq!(
            s.claim(&f.id, Platform::Juno, "/r", "d")
                .unwrap_err()
                .code(),
            "not_claimable"
        );
    }

    #[test]
    fn release_retries_then_blocks() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web]);
        layer(&s, &f.id, Platform::Web);
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        s.attach(
            &f.id,
            Platform::Web,
            "/w",
            "a1",
            &Step::new("claude", Some("claude-opus-5-5"), Some("claude-cli")),
            "d",
        )
        .unwrap();
        let t = s
            .release(&f.id, Platform::Web, "ended without a commit", 2, "d")
            .unwrap();
        assert_eq!((t.status, t.agent), (TargetStatus::Affected, None));
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        let t = s.release(&f.id, Platform::Web, "again", 2, "d").unwrap();
        assert_eq!(t.status, TargetStatus::Blocked);
        assert!(s.due().unwrap().is_empty());
    }

    #[test]
    fn merge_and_deploy_are_recorded_and_merge_repeats_are_fine() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web]);
        layer(&s, &f.id, Platform::Web);
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        let t = s
            .record_merge(&f.id, Platform::Web, "abcdef1234", "lagent")
            .unwrap();
        assert_eq!(
            (t.status, t.commit_sha.as_deref()),
            (TargetStatus::Fixed, Some("abcdef1234"))
        );
        assert_eq!(
            s.record_merge(&f.id, Platform::Web, "abcdef1234", "lagent")
                .unwrap()
                .status,
            TargetStatus::Fixed
        );
        let t = s
            .record_deploy(
                &f.id,
                Platform::Web,
                "deploy/web/20261102T0100Z-abcdef12",
                "lagent",
            )
            .unwrap();
        assert_eq!(
            t.deploy_tag.as_deref(),
            Some("deploy/web/20261102T0100Z-abcdef12")
        );
        // Merged is not closed: only evidence closes it.
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Open);
    }

    #[test]
    fn duplicates_stop_work_and_never_form_a_cycle() {
        let s = Flaws::in_memory().unwrap();
        let (a, b) = (
            report(&s, "aaaa one", &[Platform::Web]),
            report(&s, "bbbb two", &[Platform::Web]),
        );
        layer(&s, &b.id, Platform::Web);
        s.patch(
            &b.id,
            FlawPatch {
                duplicate_of: Some(a.id.clone()),
                ..FlawPatch::default()
            },
            "t",
        )
        .unwrap();
        assert_eq!(s.get(&b.id).unwrap().flaw.state, FlawState::Duplicate);
        assert!(s.due().unwrap().is_empty());
        assert_eq!(
            s.claim(&b.id, Platform::Web, "/r", "d").unwrap_err().code(),
            "not_claimable"
        );
        assert!(s
            .patch(
                &a.id,
                FlawPatch {
                    duplicate_of: Some(b.id.clone()),
                    ..FlawPatch::default()
                },
                "t"
            )
            .is_err());
        assert!(s
            .patch(
                &a.id,
                FlawPatch {
                    duplicate_of: Some(a.id.clone()),
                    ..FlawPatch::default()
                },
                "t"
            )
            .is_err());
    }

    #[test]
    fn a_new_report_shows_likely_duplicates_and_discovered_from_links() {
        let s = Flaws::in_memory().unwrap();
        let a = report(&s, "emoji rendering as boxes", &[]);
        let r = s
            .report(
                NewFlaw {
                    words: "emoji rendering boxes again".into(),
                    discovered_from: Some(a.id.clone()),
                    ..NewFlaw::default()
                },
                "agent",
            )
            .unwrap();
        assert_eq!(r.possible_duplicates[0].id, a.id);
        assert_eq!(r.flaw.flaw.discovered_from.as_deref(), Some(a.id.as_str()));
        assert!(s
            .report(
                NewFlaw {
                    words: "x".into(),
                    discovered_from: Some("fl-00000000".into()),
                    ..NewFlaw::default()
                },
                "a"
            )
            .is_err());
    }

    #[test]
    fn search_and_the_matrix() {
        let s = Flaws::in_memory().unwrap();
        let a = report(&s, "emoji 100% boxes", &[Platform::Web]);
        report(&s, "line breaks lost", &[]);
        let q = |w: &str| ListQuery {
            q: Some(w.into()),
            ..ListQuery::default()
        };
        assert_eq!(s.list(&q("boxes")).unwrap()[0].id, a.id);
        assert_eq!(s.list(&q("100%")).unwrap().len(), 1);
        assert_eq!(s.list(&q("0%")).unwrap().len(), 1);
        assert!(s.list(&q("zzz")).unwrap().is_empty());
        assert_eq!(
            s.list(&ListQuery {
                platform: Some(Platform::Web),
                status: Some(TargetStatus::Affected),
                ..ListQuery::default()
            })
            .unwrap()
            .len(),
            1
        );
        let m = s.matrix().unwrap();
        assert_eq!(
            m["platforms"].as_array().unwrap().len(),
            Platform::ALL.len()
        );
        assert_eq!(m["rows"].as_array().unwrap().len(), 2);
        let row = m["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == a.id.as_str())
            .unwrap();
        assert_eq!(row["cells"]["web"]["status"], "affected");
    }

    #[test]
    fn outcomes_are_told_once_per_change() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web]);
        layer(&s, &f.id, Platform::Web);
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        assert!(s.untold().unwrap().is_empty());
        s.record_merge(&f.id, Platform::Web, "abcdef1", "l")
            .unwrap();
        assert_eq!(s.untold().unwrap().len(), 1);
        s.mark_told(&f.id, Platform::Web, TargetStatus::Fixed)
            .unwrap();
        assert!(s.untold().unwrap().is_empty());
        s.set_target(
            &f.id,
            Platform::Web,
            TargetUpdate {
                status: Some(TargetStatus::Verified),
                evidence: Some(shot()),
                ..TargetUpdate::default()
            },
            "t",
        )
        .unwrap();
        assert_eq!(s.untold().unwrap().len(), 1);
    }

    #[test]
    fn two_handles_share_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/flaws.db");
        let a = Flaws::open(&p).unwrap();
        let f = report(&a, "x", &[]);
        let b = Flaws::open(&p).unwrap();
        assert_eq!(b.get(&f.id).unwrap().flaw.id, f.id);
    }

    #[test]
    fn a_task_that_never_started_costs_no_try() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web]);
        layer(&s, &f.id, Platform::Web);
        for _ in 0..5 {
            s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
            let t = s
                .give_back(&f.id, Platform::Web, "agentd is down", "d")
                .unwrap();
            assert_eq!((t.status, t.attempts), (TargetStatus::Affected, 0));
        }
        assert_eq!(s.due().unwrap().len(), 1);
        assert_eq!(s.get(&f.id).unwrap().flaw.state, FlawState::Open);
    }

    #[test]
    fn a_server_fix_is_one_task_for_every_client() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[Platform::Web, Platform::Ios]);
        for p in [Platform::Web, Platform::Ios] {
            s.set_target(
                &f.id,
                p,
                TargetUpdate {
                    layer: Some(Layer::Server),
                    ..TargetUpdate::default()
                },
                "t",
            )
            .unwrap();
        }
        assert_eq!(s.due().unwrap().len(), 2);
        s.claim(&f.id, Platform::Web, "/r", "d").unwrap();
        assert!(
            s.due().unwrap().is_empty(),
            "the sibling waits for the shared fix"
        );
        s.record_merge(&f.id, Platform::Web, "abcdef1", "l")
            .unwrap();
        let d = s.get(&f.id).unwrap();
        for p in [Platform::Web, Platform::Ios] {
            let t = d.targets.iter().find(|t| t.platform == p).unwrap();
            assert_eq!(
                (t.status, t.commit_sha.as_deref()),
                (TargetStatus::Fixed, Some("abcdef1")),
                "{p}"
            );
        }
    }

    #[test]
    fn triage_runs_once() {
        let s = Flaws::in_memory().unwrap();
        let f = report(&s, "x", &[]);
        assert_eq!(s.triage_due().unwrap().len(), 1);
        s.start_triage(&f.id, "a1", false, Some(1), "d").unwrap();
        assert!(s.triage_due().unwrap().is_empty());
        assert_eq!(
            s.triaging().unwrap()[0].triage_step,
            Some(1),
            "which step of the chain it runs on"
        );
        s.reset_triage(&f.id, "the lane is out", "d").unwrap();
        assert_eq!(
            s.triage_due().unwrap()[0].triage_step,
            None,
            "no agent holds it, so no step either"
        );
        s.start_triage(&f.id, "a1", false, Some(1), "d").unwrap();
        s.finish_triage(&f.id, "ended", "d").unwrap();
        assert!(s.triaging().unwrap().is_empty());
        assert!(
            s.triage_due().unwrap().is_empty(),
            "the rows it left open stay open: no second agent"
        );
    }

    #[test]
    fn a_database_from_before_the_chain_gains_the_step() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(OLD_SCHEMA).unwrap();
        let s = Flaws::with(c).unwrap();
        let f = report(&s, "x", &[]);
        assert_eq!(
            s.triaging().unwrap().len(),
            0,
            "nothing in it is a step yet"
        );
        s.start_triage(&f.id, "manual", true, None, "d").unwrap();
        let reread = s.get(&f.id).unwrap().flaw;
        assert_eq!(
            (reread.triage_agent.as_deref(), reread.triage_step),
            (Some("manual"), None)
        );
    }

    /// The table as it was before the chain: the same columns, no `triage_step`.
    const OLD_SCHEMA: &str = r#"
CREATE TABLE flaws (
    id TEXT PRIMARY KEY,
    words TEXT NOT NULL,
    title TEXT NOT NULL,
    severity TEXT NOT NULL,
    state TEXT NOT NULL,
    duplicate_of TEXT REFERENCES flaws(id),
    discovered_from TEXT REFERENCES flaws(id),
    thread TEXT,
    reporter TEXT NOT NULL,
    gate TEXT,
    decision_id TEXT,
    triage_agent TEXT,
    triaged_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE flaw_targets (
    flaw_id TEXT NOT NULL REFERENCES flaws(id),
    platform TEXT NOT NULL,
    layer TEXT,
    status TEXT NOT NULL,
    evidence TEXT,
    workspace TEXT,
    agent TEXT,
    session TEXT,
    repo TEXT,
    commit_sha TEXT,
    deploy_tag TEXT,
    build TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    claimed_at TEXT,
    told_status TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (flaw_id, platform)
);
CREATE TABLE flaw_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    flaw_id TEXT NOT NULL REFERENCES flaws(id),
    platform TEXT,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail TEXT NOT NULL,
    at TEXT NOT NULL
);
"#;

    #[test]
    fn ids() {
        assert!(valid_id("fl-0123abcd"));
        assert!(!valid_id("fl-0123ABCD"));
        assert!(!valid_id("fl-0123abc"));
        assert!(!valid_id("x"));
    }
}
