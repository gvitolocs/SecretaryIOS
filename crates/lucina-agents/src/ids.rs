//! Identifiers for worker runs, changes, versions and findings: the pure rules behind
//! `lucina-agentd`'s id store. No IO and no clock.
//!
//! The scheme (fixed):
//!
//! - A lead code is two capital letters (`LA`, `JS`). A run is `<LEAD>-R<nnn>`, a change
//!   `<LEAD>-C<nnn>`, a version `<LEAD>-V<nnn>`: the number is zero-padded to at least three
//!   digits and grows past 999 without a limit (`LA-R1000`). A finding is `<AREA>-<nn>`: an
//!   area code of two to six capital letters and a number zero-padded to at least two digits
//!   (`GC-03`, `REENT-12`, `GC-100`).
//! - A number is never reused: the next number of a kind is the highest number ever issued
//!   for that lead and kind (for findings: that area) plus one, starting at 1.
//! - A state change is a new row with the same id; earlier rows are never rewritten.
//!
//! `Lead` and `Area` are registry rows: a lead row has the code as its id and the chat's name
//! as its title; an area row has the area code as its id, what it covers as its title and the
//! owning lead's code in `by`.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::work::WorkState;

/// What a row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Run,
    Change,
    Version,
    Finding,
    Lead,
    Area,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Run => "run",
            Kind::Change => "change",
            Kind::Version => "version",
            Kind::Finding => "finding",
            Kind::Lead => "lead",
            Kind::Area => "area",
        }
    }

    /// The kind for its lowercase name.
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "run" => Kind::Run,
            "change" => Kind::Change,
            "version" => Kind::Version,
            "finding" => Kind::Finding,
            "lead" => Kind::Lead,
            "area" => Kind::Area,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The states of a kind; the first is the default.
pub fn states(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Run => &[
            "started",
            "verified",
            "accepted",
            "dismissed",
            "empty",
            "failed",
        ],
        Kind::Change => &["open", "reviewed", "merged", "reverted"],
        Kind::Version => &["built", "installed", "pushed", "pinned", "rolled-back"],
        Kind::Finding => &["reported", "confirmed", "struck", "reproduced", "fixed"],
        Kind::Lead | Kind::Area => &["registered"],
    }
}

/// The state a new row of the kind starts in.
pub fn default_state(kind: Kind) -> &'static str {
    states(kind)[0]
}

/// A lead code: exactly two capital letters.
pub fn valid_lead(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// An area code: two to six capital letters.
pub fn valid_area(s: &str) -> bool {
    (2..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// What an id says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedId {
    pub kind: Kind,
    /// The lead code (run, change, version) or the area code (finding).
    pub prefix: String,
    pub number: u32,
}

/// Reads an id of one of the four issued forms; anything else is `None`: no lower case, no
/// spaces, no padding below the minimum, no extra leading zeros, no number 0.
pub fn parse_id(s: &str) -> Option<ParsedId> {
    let (prefix, rest) = s.split_once('-')?;
    let first = rest.chars().next()?;
    let (kind, digits) = match first {
        'R' => (Kind::Run, &rest[1..]),
        'C' => (Kind::Change, &rest[1..]),
        'V' => (Kind::Version, &rest[1..]),
        c if c.is_ascii_digit() => (Kind::Finding, rest),
        _ => return None,
    };
    let prefix_ok = match kind {
        Kind::Finding => valid_area(prefix),
        _ => valid_lead(prefix),
    };
    if !prefix_ok || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number: u32 = digits.parse().ok()?;
    if number == 0 || format_id(kind, prefix, number) != s {
        return None;
    }
    Some(ParsedId {
        kind,
        prefix: prefix.to_string(),
        number,
    })
}

/// The id of a kind: `LA-R007`, `LA-C012`, `LA-V003`, `GC-03`. For a lead or an area the code.
pub fn format_id(kind: Kind, prefix: &str, number: u32) -> String {
    match kind {
        Kind::Run => format!("{prefix}-R{number:03}"),
        Kind::Change => format!("{prefix}-C{number:03}"),
        Kind::Version => format!("{prefix}-V{number:03}"),
        Kind::Finding => format!("{prefix}-{number:02}"),
        Kind::Lead | Kind::Area => prefix.to_string(),
    }
}

/// One line of the store: every field of the hand-kept ledgers, and the facts the daemon adds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdRow {
    pub id: String,
    /// RFC 3339.
    #[serde(default)]
    pub utc: String,
    pub kind: Kind,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub spec: String,
    #[serde(rename = "where", default)]
    pub where_: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub refs: Vec<String>,
    /// The lane as `lane` or the lane config spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// `daemon`, `lead` or `import`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    /// The hash of the previous line (the store fills it in).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prev: String,
}

/// The highest number issued for the kind and prefix, plus one (1 when none).
pub fn next_number(rows: &[IdRow], kind: Kind, prefix: &str) -> u32 {
    rows.iter()
        .filter_map(|r| parse_id(&r.id))
        .filter(|p| p.kind == kind && p.prefix == prefix)
        .map(|p| p.number)
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

fn registered(rows: &[IdRow], kind: Kind, code: &str) -> bool {
    rows.iter().any(|r| r.kind == kind && r.id == code)
}

/// Whether a new row of the kind may be issued. `lead_or_area` is the lead code (run, change,
/// version), the area code (finding) or the new code (lead). An area row has two codes: use
/// [`check_new_area`].
pub fn check_new(
    rows: &[IdRow],
    kind: Kind,
    lead_or_area: &str,
    title: &str,
) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("a title is required".into());
    }
    match kind {
        Kind::Run | Kind::Change | Kind::Version => {
            if !registered(rows, Kind::Lead, lead_or_area) {
                return Err(format!("lead {lead_or_area} is not registered"));
            }
        }
        Kind::Finding => {
            if !registered(rows, Kind::Area, lead_or_area) {
                return Err(format!("area {lead_or_area} is not registered"));
            }
        }
        Kind::Lead => {
            if !valid_lead(lead_or_area) {
                return Err(format!(
                    "\"{lead_or_area}\" is not a lead code: two capital letters"
                ));
            }
            if registered(rows, Kind::Lead, lead_or_area) {
                return Err(format!("lead {lead_or_area} is already registered"));
            }
            if registered(rows, Kind::Area, lead_or_area) {
                return Err(format!(
                    "\"{lead_or_area}\" is an area code: a lead code must differ from every area code"
                ));
            }
        }
        Kind::Area => {
            return Err("an area row needs its owning lead: use check_new_area".into());
        }
    }
    Ok(())
}

/// [`check_new`] for an area row: a valid code that is not registered, not a registered lead
/// code, and an owning lead that is registered.
pub fn check_new_area(rows: &[IdRow], code: &str, owner: &str, title: &str) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("a title (what the area covers) is required".into());
    }
    if !valid_area(code) {
        return Err(format!(
            "\"{code}\" is not an area code: two to six capital letters"
        ));
    }
    if registered(rows, Kind::Area, code) {
        return Err(format!("area {code} is already registered"));
    }
    if registered(rows, Kind::Lead, code) {
        return Err(format!(
            "\"{code}\" is a lead code: an area code must differ from every lead code"
        ));
    }
    if !registered(rows, Kind::Lead, owner) {
        return Err(format!("lead {owner} is not registered"));
    }
    Ok(())
}

/// The kind of an id that has a first row, when `state` belongs to that kind.
pub fn check_state(rows: &[IdRow], id: &str, state: &str) -> Result<Kind, String> {
    let kind = rows
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.kind)
        .ok_or_else(|| format!("no id {id}"))?;
    if !states(kind).contains(&state) {
        return Err(format!(
            "state \"{state}\" does not belong to a {kind}: use {}",
            states(kind).join(", ")
        ));
    }
    Ok(kind)
}

/// Lower case, every `.` and `:` replaced by `-` (`Claude-Sonnet-5.5` gives `claude-sonnet-5-5`).
pub fn normalize_model(model: &str) -> String {
    model
        .to_lowercase()
        .chars()
        .map(|c| if c == '.' || c == ':' { '-' } else { c })
        .collect()
}

/// One id as it stands now: its latest state and the latest non-empty value of each field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdView {
    pub id: String,
    pub kind: Kind,
    pub state: String,
    /// Of the latest row.
    pub utc: String,
    /// Of the first row.
    pub first_utc: String,
    pub title: String,
    pub by: String,
    pub spec: String,
    #[serde(rename = "where")]
    pub where_: String,
    pub base: String,
    pub result: String,
    pub refs: Vec<String>,
    pub rows: usize,
    /// From the latest row that has a lane; `None` for a chain whose final lane is not known.
    pub lane: Option<String>,
    /// [`normalize_model`] of `model_raw`.
    pub model: Option<String>,
    /// As stored.
    pub model_raw: Option<String>,
    pub chain: Option<String>,
    pub repo: Option<String>,
    pub workspace: Option<String>,
    pub branch: Option<String>,
    /// Of the first row.
    pub declared_by: Option<String>,
    pub agent: Option<String>,
    /// The id's first row is at or after a broken line of the store (set by the daemon).
    #[serde(default)]
    pub after_break: bool,
}

fn nonempty(v: &Option<String>) -> Option<String> {
    v.as_ref().filter(|s| !s.is_empty()).cloned()
}

fn latest(into: &mut String, from: &str) {
    if !from.is_empty() {
        *into = from.to_string();
    }
}

/// One view per id, in order of first issue.
pub fn current(rows: &[IdRow]) -> Vec<IdView> {
    let mut views: Vec<IdView> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for r in rows {
        let i = match at.get(&r.id) {
            Some(i) => *i,
            None => {
                views.push(IdView {
                    id: r.id.clone(),
                    kind: r.kind,
                    first_utc: r.utc.clone(),
                    declared_by: nonempty(&r.declared_by),
                    ..IdView::default()
                });
                at.insert(r.id.clone(), views.len() - 1);
                views.len() - 1
            }
        };
        let v = &mut views[i];
        v.rows += 1;
        v.state = r.state.clone();
        v.utc = r.utc.clone();
        latest(&mut v.title, &r.title);
        latest(&mut v.by, &r.by);
        latest(&mut v.spec, &r.spec);
        latest(&mut v.where_, &r.where_);
        latest(&mut v.base, &r.base);
        latest(&mut v.result, &r.result);
        for x in &r.refs {
            if !v.refs.contains(x) {
                v.refs.push(x.clone());
            }
        }
        if let Some(lane) = nonempty(&r.lane) {
            v.lane = Some(lane);
            v.model_raw = nonempty(&r.model);
            v.model = v.model_raw.as_deref().map(normalize_model);
        }
        for (into, from) in [
            (&mut v.chain, &r.chain),
            (&mut v.repo, &r.repo),
            (&mut v.workspace, &r.workspace),
            (&mut v.branch, &r.branch),
            (&mut v.agent, &r.agent),
        ] {
            if let Some(x) = nonempty(from) {
                *into = Some(x);
            }
        }
    }
    views
}

/// The lead that owns an id: the prefix of a run, change or version, the owner of a finding's
/// area, the code of a lead, the owner of an area; `None` when unknown.
pub fn lead_of(rows: &[IdRow], id: &str) -> Option<String> {
    match parse_id(id) {
        Some(p) if p.kind != Kind::Finding => Some(p.prefix),
        Some(p) => lead_of(rows, &p.prefix),
        None => {
            let row = rows.iter().find(|r| r.id == id)?;
            match row.kind {
                Kind::Lead => Some(row.id.clone()),
                Kind::Area => {
                    let owner = rows
                        .iter()
                        .rev()
                        .find(|r| r.id == id && !r.by.is_empty())?
                        .by
                        .clone();
                    Some(owner)
                }
                _ => None,
            }
        }
    }
}

const HEADER: &str = "id\tutc\tkind\tstate\ttitle\tby\tspec\twhere\tbase\tresult\trefs";
const AREAS_HEADER: &str = "code\tlead\tcovers";

fn id_matches_kind(id: &str, kind: Kind) -> bool {
    match kind {
        Kind::Lead => valid_lead(id),
        Kind::Area => valid_area(id),
        _ => parse_id(id).is_some_and(|p| p.kind == kind),
    }
}

/// Reads a hand-kept ledger: the header line and blank lines are skipped, trailing empty
/// columns may be missing. An unknown kind, a state that does not belong to the kind, an id
/// that does not parse or does not match its kind, or fewer than four columns is an error
/// that names the line (1-based).
pub fn parse_tsv(text: &str) -> Result<Vec<IdRow>, String> {
    let mut out = Vec::new();
    let mut seen_first = false;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if !seen_first {
            seen_first = true;
            if cols[0].trim() == "id" {
                continue;
            }
        }
        if cols.len() < 4 {
            return Err(format!(
                "line {n}: expected at least 4 columns (id, utc, kind, state), found {}",
                cols.len()
            ));
        }
        let col = |k: usize| cols.get(k).map(|c| c.trim()).unwrap_or("");
        let kind =
            Kind::parse(col(2)).ok_or_else(|| format!("line {n}: unknown kind \"{}\"", col(2)))?;
        if !states(kind).contains(&col(3)) {
            return Err(format!(
                "line {n}: state \"{}\" does not belong to a {kind}",
                col(3)
            ));
        }
        if !id_matches_kind(col(0), kind) {
            return Err(format!("line {n}: \"{}\" is not a valid {kind} id", col(0)));
        }
        out.push(IdRow {
            id: col(0).to_string(),
            utc: col(1).to_string(),
            kind,
            state: col(3).to_string(),
            title: col(4).to_string(),
            by: col(5).to_string(),
            spec: col(6).to_string(),
            where_: col(7).to_string(),
            base: col(8).to_string(),
            result: col(9).to_string(),
            refs: col(10).split_whitespace().map(str::to_string).collect(),
            ..IdRow::default()
        });
    }
    Ok(out)
}

/// Reads an area file (`code`, `lead`, `covers`) as area rows (the time stays empty so that
/// reading the same file again gives the same rows).
pub fn parse_areas_tsv(text: &str) -> Result<Vec<IdRow>, String> {
    let mut out = Vec::new();
    let mut seen_first = false;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if !seen_first {
            seen_first = true;
            if cols[0].trim() == "code" {
                continue;
            }
        }
        if cols.len() < 2 {
            return Err(format!(
                "line {n}: expected at least 2 columns (code, lead), found {}",
                cols.len()
            ));
        }
        let col = |k: usize| cols.get(k).map(|c| c.trim()).unwrap_or("");
        if !valid_area(col(0)) {
            return Err(format!("line {n}: \"{}\" is not an area code", col(0)));
        }
        if !valid_lead(col(1)) {
            return Err(format!("line {n}: \"{}\" is not a lead code", col(1)));
        }
        out.push(IdRow {
            id: col(0).to_string(),
            kind: Kind::Area,
            state: default_state(Kind::Area).to_string(),
            title: col(2).to_string(),
            by: col(1).to_string(),
            ..IdRow::default()
        });
    }
    Ok(out)
}

/// The header lines of the two hand-kept formats, for messages and tests.
pub fn tsv_headers() -> (&'static str, &'static str) {
    (HEADER, AREAS_HEADER)
}

/// The incoming rows that are not present yet (same id, time and state), in order. A row
/// repeated inside `incoming` counts once.
pub fn import_plan(existing: &[IdRow], incoming: Vec<IdRow>) -> Vec<IdRow> {
    let mut have: HashSet<(String, String, String)> = existing
        .iter()
        .map(|r| (r.id.clone(), r.utc.clone(), r.state.clone()))
        .collect();
    incoming
        .into_iter()
        .filter(|r| have.insert((r.id.clone(), r.utc.clone(), r.state.clone())))
        .collect()
}

/// One id near the subject, and how it is related.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLink {
    pub relation: String,
    pub view: IdView,
}

/// The chain from an id to the runs, changes, findings and versions around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trace {
    pub subject: IdView,
    pub links: Vec<TraceLink>,
}

/// The chain from `id`. Relations are found through `refs` in either direction (A names B, or
/// B names A). No id appears twice under the same relation; the order is first issue.
pub fn trace(rows: &[IdRow], id: &str) -> Option<Trace> {
    let views = current(rows);
    let subject = views.iter().position(|v| v.id == id)?;
    let adj = |i: usize, kind: Kind| -> BTreeSet<usize> {
        (0..views.len())
            .filter(|&j| {
                j != i
                    && views[j].kind == kind
                    && (views[i].refs.contains(&views[j].id)
                        || views[j].refs.contains(&views[i].id))
            })
            .collect()
    };
    let via = |set: &BTreeSet<usize>, kind: Kind| -> BTreeSet<usize> {
        set.iter().flat_map(|&i| adj(i, kind)).collect()
    };
    let mut sets: Vec<(&str, BTreeSet<usize>)> = Vec::new();
    match views[subject].kind {
        Kind::Finding => {
            let changes = adj(subject, Kind::Change);
            sets.push(("found by", adj(subject, Kind::Run)));
            sets.push(("fixed by", changes.clone()));
            sets.push(("written or reviewed by", via(&changes, Kind::Run)));
            sets.push(("shipped in", via(&changes, Kind::Version)));
        }
        Kind::Run => {
            let changes = adj(subject, Kind::Change);
            sets.push(("works for", changes.clone()));
            sets.push(("found", adj(subject, Kind::Finding)));
            sets.push(("shipped in", via(&changes, Kind::Version)));
        }
        Kind::Change => {
            sets.push(("runs", adj(subject, Kind::Run)));
            sets.push(("fixes", adj(subject, Kind::Finding)));
            sets.push(("shipped in", adj(subject, Kind::Version)));
        }
        Kind::Version => {
            let changes = adj(subject, Kind::Change);
            sets.push(("contains", changes.clone()));
            sets.push(("written or reviewed by", via(&changes, Kind::Run)));
        }
        Kind::Lead | Kind::Area => {}
    }
    let mut links = Vec::new();
    for (relation, set) in sets {
        for i in set {
            links.push(TraceLink {
                relation: relation.to_string(),
                view: views[i].clone(),
            });
        }
    }
    Some(Trace {
        subject: views[subject].clone(),
        links,
    })
}

/// The ids a commit message names in its trailers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitIds {
    pub lead: Option<String>,
    pub run: Option<String>,
    pub change: Option<String>,
    pub fixes: Vec<String>,
    pub refs: Vec<String>,
}

/// Reads the `Lead:`, `Run:`, `Change:`, `Fixes:` and `Refs:` lines of the LAST paragraph of
/// a commit message. For `Fixes` and `Refs` the id is the first word after the colon (`Fixes:
/// GC-03 (fl-75f1f55c)` gives `GC-03`); a key given twice keeps every value for those two and
/// the last one for the others.
pub fn trailers(message: &str) -> CommitIds {
    let lines: Vec<&str> = message.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = lines[..end]
        .iter()
        .rposition(|l| l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut out = CommitIds::default();
    for line in &lines[start..end] {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(word) = value.split_whitespace().next() else {
            continue;
        };
        let word = word.to_string();
        match key {
            "Lead" => out.lead = Some(word),
            "Run" => out.run = Some(word),
            "Change" => out.change = Some(word),
            "Fixes" => out.fixes.push(word),
            "Refs" => out.refs.push(word),
            _ => {}
        }
    }
    out
}

/// The id of the lane run for a work job: the lower-case run id, a dash and a slug of the name
/// (lower case, runs of other characters become one dash, trimmed, at most 24 characters,
/// `run` when nothing is left): `LA-R023` and `Ids writer!` give `la-r023-ids-writer`.
pub fn lane_run_id(run_id: &str, name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(24).collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "run" } else { slug };
    format!("{}-{slug}", run_id.to_lowercase())
}

/// The run state a work job's end gives, when it gives one. `Verified` is `verified` (`empty`
/// when the job left nothing); `Done` is only `empty` when it left nothing (the lead accepts
/// or dismisses it); every ending that went wrong is `failed`; `EndedEmpty` (lane itself found
/// nothing) is `empty`; a job that has not ended gives `None`.
pub fn run_state_for(work_state: WorkState, empty: Option<bool>) -> Option<&'static str> {
    let nothing = empty == Some(true);
    match work_state {
        WorkState::Verified => Some(if nothing { "empty" } else { "verified" }),
        WorkState::Done => nothing.then_some("empty"),
        WorkState::EndedEmpty => Some("empty"),
        WorkState::Failed
        | WorkState::VerifyFailed
        | WorkState::Exhausted
        | WorkState::DirtyAfterQuota
        | WorkState::EndedDirty
        | WorkState::EndedJobsQueued
        | WorkState::Lost => Some("failed"),
        WorkState::Starting | WorkState::Queued | WorkState::Running => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, utc: &str, kind: Kind, state: &str, title: &str, refs: &[&str]) -> IdRow {
        IdRow {
            id: id.into(),
            utc: utc.into(),
            kind,
            state: state.into(),
            title: title.into(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
            ..IdRow::default()
        }
    }

    fn registry() -> Vec<IdRow> {
        vec![
            row("LA", "t", Kind::Lead, "registered", "Lucina Agents", &[]),
            row("JS", "t", Kind::Lead, "registered", "LucinaJS", &[]),
            IdRow {
                by: "JS".into(),
                ..row("GC", "t", Kind::Area, "registered", "gc bugs", &[])
            },
        ]
    }

    #[test]
    fn kinds_and_states() {
        for k in [
            Kind::Run,
            Kind::Change,
            Kind::Version,
            Kind::Finding,
            Kind::Lead,
            Kind::Area,
        ] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
            assert!(states(k).contains(&default_state(k)));
        }
        assert_eq!(Kind::parse("Run"), None);
        assert_eq!(states(Kind::Run).len(), 6);
        assert_eq!(
            states(Kind::Change),
            ["open", "reviewed", "merged", "reverted"]
        );
        assert_eq!(
            states(Kind::Version),
            ["built", "installed", "pushed", "pinned", "rolled-back"]
        );
        assert_eq!(
            states(Kind::Finding),
            ["reported", "confirmed", "struck", "reproduced", "fixed"]
        );
        assert_eq!(states(Kind::Lead), ["registered"]);
        assert_eq!(default_state(Kind::Run), "started");
        assert_eq!(default_state(Kind::Change), "open");
        assert_eq!(default_state(Kind::Version), "built");
        assert_eq!(default_state(Kind::Finding), "reported");
        assert_eq!(default_state(Kind::Area), "registered");
    }

    #[test]
    fn lead_and_area_codes() {
        assert!(valid_lead("LA"));
        for bad in ["", "L", "LAB", "la", "L1", "L A", "ÅÄ"] {
            assert!(!valid_lead(bad), "{bad}");
        }
        assert!(valid_area("GC") && valid_area("REENT") && valid_area("ABCDEF"));
        for bad in ["", "G", "ABCDEFG", "gc", "G1", "G-C"] {
            assert!(!valid_area(bad), "{bad}");
        }
    }

    #[test]
    fn every_id_form_parses_and_round_trips() {
        let p = parse_id("LA-R007").unwrap();
        assert_eq!((p.kind, p.prefix.as_str(), p.number), (Kind::Run, "LA", 7));
        assert_eq!(parse_id("JS-C012").unwrap().kind, Kind::Change);
        assert_eq!(parse_id("JS-V003").unwrap().kind, Kind::Version);
        let f = parse_id("REENT-12").unwrap();
        assert_eq!(
            (f.kind, f.prefix.as_str(), f.number),
            (Kind::Finding, "REENT", 12)
        );
        assert_eq!(parse_id("GC-03").unwrap().number, 3);
        // Growth past the padding, without a limit.
        assert_eq!(parse_id("LA-R1000").unwrap().number, 1000);
        assert_eq!(parse_id("GC-100").unwrap().number, 100);
        assert_eq!(format_id(Kind::Run, "LA", 1000), "LA-R1000");
        assert_eq!(format_id(Kind::Run, "LA", 1), "LA-R001");
        assert_eq!(format_id(Kind::Change, "LA", 12), "LA-C012");
        assert_eq!(format_id(Kind::Version, "LA", 3), "LA-V003");
        assert_eq!(format_id(Kind::Finding, "GC", 3), "GC-03");
        assert_eq!(format_id(Kind::Finding, "GC", 100), "GC-100");
        assert_eq!(format_id(Kind::Lead, "LA", 9), "LA");
        for id in [
            "LA-R001",
            "LA-R999",
            "LA-R1000",
            "GC-01",
            "GC-99",
            "GC-100",
            "ABCDEF-12",
        ] {
            let p = parse_id(id).unwrap();
            assert_eq!(format_id(p.kind, &p.prefix, p.number), id);
        }
    }

    #[test]
    fn near_misses_do_not_parse() {
        for bad in [
            "",
            "LA",
            "LA-",
            "LA-R",
            "LA-R01",
            "LA-R1",
            "LA-R0001",
            "LA-R000",
            "la-R001",
            "LA-r001",
            "LA-X001",
            "LAB-R001",
            "L-R001",
            "LA-R001 ",
            " LA-R001",
            "LA - R001",
            "LA-R00x",
            "LA-R-01",
            "LA-C01",
            "LA-V01",
            "G-03",
            "GC-3",
            "GC-003",
            "GC-00",
            "GC-0",
            "gc-03",
            "ABCDEFG-03",
            "GC-03a",
            "GC-R001x",
            "GC_03",
            "GC-03-04",
            "LA-R99999999999",
        ] {
            assert!(parse_id(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn next_number_counts_the_highest_ever_issued() {
        assert_eq!(next_number(&[], Kind::Run, "LA"), 1);
        let rows = vec![
            row("LA-R001", "t", Kind::Run, "started", "a", &[]),
            row("LA-R005", "t", Kind::Run, "started", "b", &[]),
            row("LA-R005", "t", Kind::Run, "verified", "b", &[]),
            row("LA-R003", "t", Kind::Run, "started", "c", &[]),
            row("JS-R009", "t", Kind::Run, "started", "d", &[]),
            row("LA-C002", "t", Kind::Change, "open", "e", &[]),
            row("GC-04", "t", Kind::Finding, "reported", "f", &[]),
            row("GC-02", "t", Kind::Finding, "reported", "g", &[]),
            row("REENT-30", "t", Kind::Finding, "reported", "h", &[]),
            row("LA-R1000", "t", Kind::Run, "started", "big", &[]),
        ];
        assert_eq!(next_number(&rows, Kind::Run, "LA"), 1001);
        assert_eq!(next_number(&rows[..9], Kind::Run, "LA"), 6);
        assert_eq!(next_number(&rows, Kind::Run, "JS"), 10);
        assert_eq!(next_number(&rows, Kind::Change, "LA"), 3);
        assert_eq!(next_number(&rows, Kind::Change, "JS"), 1);
        assert_eq!(next_number(&rows, Kind::Version, "LA"), 1);
        assert_eq!(next_number(&rows, Kind::Finding, "GC"), 5);
        assert_eq!(next_number(&rows, Kind::Finding, "REENT"), 31);
        assert_eq!(next_number(&rows, Kind::Finding, "NEW"), 1);
    }

    #[test]
    fn check_new_refusals() {
        let rows = registry();
        assert!(check_new(&rows, Kind::Run, "LA", "a title").is_ok());
        assert!(check_new(&rows, Kind::Change, "JS", "a title").is_ok());
        assert!(check_new(&rows, Kind::Version, "LA", "a title").is_ok());
        assert!(check_new(&rows, Kind::Finding, "GC", "a title").is_ok());
        assert!(check_new(&rows, Kind::Lead, "PK", "Pokoin").is_ok());
        assert_eq!(
            check_new(&rows, Kind::Run, "LA", "  \t").unwrap_err(),
            "a title is required"
        );
        assert_eq!(
            check_new(&rows, Kind::Run, "ZZ", "t").unwrap_err(),
            "lead ZZ is not registered"
        );
        // An area is not a lead, and a lead is not an area.
        assert!(check_new(&rows, Kind::Change, "GC", "t").is_err());
        assert_eq!(
            check_new(&rows, Kind::Finding, "LA", "t").unwrap_err(),
            "area LA is not registered"
        );
        assert!(check_new(&rows, Kind::Finding, "NOPE", "t").is_err());
        assert_eq!(
            check_new(&rows, Kind::Lead, "LA", "t").unwrap_err(),
            "lead LA is already registered"
        );
        assert!(check_new(&rows, Kind::Lead, "lab", "t").is_err());
        assert!(check_new(&rows, Kind::Lead, "ABC", "t").is_err());
        assert!(check_new(&rows, Kind::Area, "NEW", "t").is_err());
    }

    #[test]
    fn a_lead_code_may_not_be_a_registered_area_code() {
        let rows = registry();
        // `GC` is two capital letters and no lead: the area side is what stops it, with the
        // area check's own wording turned around.
        assert!(valid_lead("GC"));
        assert_eq!(
            check_new(&rows, Kind::Lead, "GC", "the gc chat").unwrap_err(),
            "\"GC\" is an area code: a lead code must differ from every area code"
        );
        assert_eq!(
            check_new_area(&rows, "LA", "JS", "x").unwrap_err(),
            "\"LA\" is a lead code: an area code must differ from every lead code"
        );
        // A code that is neither is still allowed.
        assert!(check_new(&rows, Kind::Lead, "PK", "Pokoin").is_ok());
    }

    #[test]
    fn check_new_area_refusals() {
        let rows = registry();
        assert!(check_new_area(&rows, "REENT", "LA", "re-entrancy").is_ok());
        assert!(check_new_area(&rows, "FIVE5", "JS", "x").is_err());
        assert!(check_new_area(&rows, "REENT", "LA", " ").is_err());
        assert!(check_new_area(&rows, "re", "LA", "x").is_err());
        assert!(check_new_area(&rows, "R", "LA", "x").is_err());
        assert_eq!(
            check_new_area(&rows, "GC", "LA", "again").unwrap_err(),
            "area GC is already registered"
        );
        // Equal to a registered lead code.
        let err = check_new_area(&rows, "LA", "JS", "x").unwrap_err();
        assert!(err.contains("lead code"), "{err}");
        assert_eq!(
            check_new_area(&rows, "REENT", "ZZ", "x").unwrap_err(),
            "lead ZZ is not registered"
        );
    }

    #[test]
    fn check_state_refusals() {
        let mut rows = registry();
        rows.push(row("LA-R001", "t", Kind::Run, "started", "a", &[]));
        rows.push(row("GC-01", "t", Kind::Finding, "reported", "f", &[]));
        assert_eq!(check_state(&rows, "LA-R001", "accepted"), Ok(Kind::Run));
        assert_eq!(check_state(&rows, "GC-01", "fixed"), Ok(Kind::Finding));
        assert_eq!(check_state(&rows, "LA", "registered"), Ok(Kind::Lead));
        assert_eq!(
            check_state(&rows, "LA-R002", "accepted").unwrap_err(),
            "no id LA-R002"
        );
        let err = check_state(&rows, "LA-R001", "merged").unwrap_err();
        assert!(err.contains("does not belong to a run"), "{err}");
        assert!(check_state(&rows, "GC-01", "accepted").is_err());
        assert!(check_state(&rows, "LA", "open").is_err());
        assert!(check_state(&rows, "LA-R001", "").is_err());
    }

    #[test]
    fn models_are_normalised() {
        assert_eq!(normalize_model("Claude-Sonnet-5.5"), "claude-sonnet-5-5");
        assert_eq!(normalize_model("qwen3.8:27b"), "qwen3-8-27b");
        assert_eq!(normalize_model("auto-free"), "auto-free");
        assert_eq!(normalize_model(""), "");
    }

    #[test]
    fn current_merges_the_latest_values() {
        let mut first = row(
            "LA-R001",
            "2026-11-10T09:00:00Z",
            Kind::Run,
            "started",
            "Pure rules",
            &["LA-C001"],
        );
        first.by = "qoder auto-free".into();
        first.base = "a".repeat(40);
        first.declared_by = Some("daemon".into());
        first.chain = Some("deep".into());
        first.workspace = Some("/w".into());
        let mut second = row(
            "LA-R001",
            "2026-11-10T09:30:00Z",
            Kind::Run,
            "started",
            "",
            &["LA-C001", "GC-01"],
        );
        second.by = "zen-glm-flash glm-5.3-flash-free".into();
        second.lane = Some("zen-glm-flash".into());
        second.model = Some("GLM-5.3:Flash".into());
        second.declared_by = Some("lead".into());
        let mut third = row(
            "LA-R001",
            "2026-11-10T10:00:00Z",
            Kind::Run,
            "verified",
            "",
            &[],
        );
        third.result = "verified head abc".into();
        let other = row(
            "LA-C001",
            "2026-11-10T08:00:00Z",
            Kind::Change,
            "open",
            "Change",
            &[],
        );
        let views = current(&[first, other, second, third]);
        assert_eq!(views.len(), 2);
        let v = &views[0];
        assert_eq!(v.id, "LA-R001");
        assert_eq!(v.state, "verified");
        assert_eq!(v.utc, "2026-11-10T10:00:00Z");
        assert_eq!(v.first_utc, "2026-11-10T09:00:00Z");
        assert_eq!(v.title, "Pure rules");
        assert_eq!(v.by, "zen-glm-flash glm-5.3-flash-free");
        assert_eq!(v.base, "a".repeat(40));
        assert_eq!(v.result, "verified head abc");
        assert_eq!(v.refs, ["LA-C001", "GC-01"]);
        assert_eq!(v.rows, 3);
        assert_eq!(v.lane.as_deref(), Some("zen-glm-flash"));
        assert_eq!(v.model.as_deref(), Some("glm-5-3-flash"));
        assert_eq!(v.model_raw.as_deref(), Some("GLM-5.3:Flash"));
        assert_eq!(v.chain.as_deref(), Some("deep"));
        assert_eq!(v.workspace.as_deref(), Some("/w"));
        assert_eq!(v.declared_by.as_deref(), Some("daemon"));
        assert!(!v.after_break);
        assert_eq!(views[1].id, "LA-C001");
        assert_eq!(views[1].rows, 1);
        assert!(current(&[]).is_empty());
    }

    #[test]
    fn a_chain_run_has_no_lane_until_its_final_lane_is_known() {
        let mut start = row(
            "LA-R002",
            "2026-11-10T09:00:00Z",
            Kind::Run,
            "started",
            "Chain run",
            &[],
        );
        start.chain = Some("deep".into());
        start.by = "chain deep".into();
        let before = current(std::slice::from_ref(&start));
        assert_eq!(before[0].lane, None);
        assert_eq!(before[0].model, None);
        assert_eq!(before[0].model_raw, None);
        assert_eq!(before[0].chain.as_deref(), Some("deep"));
        let mut final_row = row(
            "LA-R002",
            "2026-11-10T09:05:00Z",
            Kind::Run,
            "started",
            "",
            &[],
        );
        final_row.lane = Some("qoder".into());
        final_row.model = Some("Auto.Free".into());
        final_row.by = "qoder Auto.Free".into();
        let after = current(&[start, final_row]);
        assert_eq!(after[0].lane.as_deref(), Some("qoder"));
        assert_eq!(after[0].model.as_deref(), Some("auto-free"));
        assert_eq!(after[0].model_raw.as_deref(), Some("Auto.Free"));
        assert_eq!(after[0].chain.as_deref(), Some("deep"));
        assert_eq!(after[0].by, "qoder Auto.Free");
        // The JSON says null, never the chain's name.
        let json = serde_json::to_value(&before[0]).unwrap();
        assert!(json["lane"].is_null() && json["model"].is_null(), "{json}");
        assert_eq!(json["where"], "");
        assert_eq!(json["firstUtc"], "2026-11-10T09:00:00Z");
    }

    #[test]
    fn declared_by_is_the_first_rows() {
        for who in ["daemon", "lead", "import"] {
            let mut a = row("LA-R003", "t1", Kind::Run, "started", "x", &[]);
            a.declared_by = Some(who.into());
            let mut b = row("LA-R003", "t2", Kind::Run, "verified", "", &[]);
            b.declared_by = Some("lead".into());
            let v = &current(&[a, b])[0];
            assert_eq!(v.declared_by.as_deref(), Some(who));
        }
    }

    #[test]
    fn rows_round_trip_through_json() {
        let mut r = row("LA-R001", "t", Kind::Run, "started", "x", &["A-B"]);
        r.where_ = "ws".into();
        r.prev = "p".into();
        r.declared_by = Some("daemon".into());
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("\"where\":\"ws\""), "{text}");
        assert!(text.contains("\"declaredBy\":\"daemon\""), "{text}");
        assert!(!text.contains("lane"), "{text}");
        assert_eq!(serde_json::from_str::<IdRow>(&text).unwrap(), r);
        let bare: IdRow =
            serde_json::from_str(r#"{"id":"LA-R001","kind":"run","state":"started"}"#).unwrap();
        assert_eq!(bare.refs, Vec::<String>::new());
        assert_eq!(bare.declared_by, None);
        assert!(serde_json::from_str::<IdRow>(r#"{"id":"x","kind":"nope"}"#).is_err());
    }

    /// A ledger as a lead keeps it by hand: a state change, a row with missing columns, a
    /// blank line and Windows line ends.
    const LEDGER: &str = "id\tutc\tkind\tstate\ttitle\tby\tspec\twhere\tbase\tresult\trefs
LA-R001\t2026-11-10T09:00:00Z\trun\tstarted\tIds pure rules\tqoder auto-free\t/home/nez/specs/a.md\tlagent-ids\tabc123\t\tLA-C001
LA-R001\t2026-11-10T10:00:00Z\trun\tverified\tIds pure rules\tqoder auto-free\t\t\t\tcommit def456\tLA-C001
LA-R002\t2026-11-10T10:30:00Z\trun\tstarted\tReview\topus\t\t\t\t\tLA-C001 GC-01

LA-C001\t2026-11-10T08:00:00Z\tchange\topen\tIdentifiers\tLA
LA-C001\t2026-11-11T08:00:00Z\tchange\tmerged\t\t\t\t\t\tmerge 9f9f\tLA-V001
LA-V001\t2026-11-12T08:00:00Z\tversion\tbuilt\tLA-V001 build\tLA\t\t\tmain 9f9f
GC-01\t2026-11-09T08:00:00Z\tfinding\treported\tUse after free\tJS-R004\t\t\t\t\tLA-R002 LA-C001\r
GC-01\t2026-11-12T09:00:00Z\tfinding\tfixed\t\t\t\t\t\t\tLA-C001
LA\t2026-11-01T00:00:00Z\tlead\tregistered\tLucina Agents
";

    #[test]
    fn tsv_of_a_real_looking_ledger_parses() {
        let rows = parse_tsv(LEDGER).unwrap();
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0].id, "LA-R001");
        assert_eq!(rows[0].kind, Kind::Run);
        assert_eq!(rows[0].where_, "lagent-ids");
        assert_eq!(rows[0].base, "abc123");
        assert_eq!(rows[0].refs, ["LA-C001"]);
        // The state change keeps the id and adds the result.
        assert_eq!(rows[1].state, "verified");
        assert_eq!(rows[1].result, "commit def456");
        assert_eq!(rows[2].refs, ["LA-C001", "GC-01"]);
        // Missing trailing columns.
        assert_eq!(rows[3].id, "LA-C001");
        assert_eq!(rows[3].by, "LA");
        assert_eq!(rows[3].spec, "");
        assert!(rows[3].refs.is_empty());
        assert_eq!(rows[4].refs, ["LA-V001"]);
        assert_eq!(rows[5].title, "LA-V001 build");
        assert_eq!(
            rows[6].refs,
            ["LA-R002", "LA-C001"],
            "a carriage return is dropped"
        );
        assert_eq!(rows[8].kind, Kind::Lead);
        assert_eq!(rows[8].id, "LA");
        assert!(parse_tsv("").unwrap().is_empty());
        assert!(parse_tsv("id\tutc\tkind\tstate\n").unwrap().is_empty());
        // Without a header line the first line is data.
        let no_header = LEDGER.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_tsv(&no_header).unwrap().len(), 9);
        assert_eq!(tsv_headers().0, LEDGER.lines().next().unwrap());
    }

    #[test]
    fn tsv_errors_name_the_line() {
        let head = "id\tutc\tkind\tstate\ttitle\n";
        let ok = "LA-R001\tt\trun\tstarted\tx\n";
        let cases = [
            ("LA-R002\tt\tdeed\tstarted\tx\n", 3, "unknown kind"),
            ("LA-R002\tt\trun\tmerged\tx\n", 3, "does not belong"),
            ("LA-R02\tt\trun\tstarted\tx\n", 3, "is not a valid run id"),
            ("LA-C002\tt\trun\tstarted\tx\n", 3, "is not a valid run id"),
            ("LA-R002\tt\trun\n", 3, "at least 4 columns"),
            ("just text\n", 3, "at least 4 columns"),
            (
                "GC-3\tt\tfinding\treported\tx\n",
                3,
                "is not a valid finding id",
            ),
            ("LAB\tt\tlead\tregistered\tx\n", 3, "is not a valid lead id"),
            ("gc\tt\tarea\tregistered\tx\n", 3, "is not a valid area id"),
        ];
        for (bad, line, what) in cases {
            let err = parse_tsv(&format!("{head}{ok}{bad}")).unwrap_err();
            assert!(err.starts_with(&format!("line {line}: ")), "{bad:?}: {err}");
            assert!(err.contains(what), "{bad:?}: {err}");
        }
        // A blank line before the bad one still counts.
        let err = parse_tsv(&format!("{head}\n{ok}\n\nX\n")).unwrap_err();
        assert!(err.starts_with("line 6: "), "{err}");
    }

    #[test]
    fn area_files_parse() {
        let text =
            "code\tlead\tcovers\nGC\tJS\tgarbage collector\nREENT\tLA\tre-entrancy\nOK\tLA\n";
        let rows = parse_areas_tsv(text).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "GC");
        assert_eq!(rows[0].kind, Kind::Area);
        assert_eq!(rows[0].by, "JS");
        assert_eq!(rows[0].title, "garbage collector");
        assert_eq!(rows[0].state, "registered");
        assert_eq!(rows[2].title, "");
        assert!(parse_areas_tsv("").unwrap().is_empty());
        for (bad, line) in [
            ("code\tlead\tcovers\nGC\n", 2),
            ("code\tlead\tcovers\ngc\tJS\tx\n", 2),
            ("code\tlead\tcovers\nGC\tJS\tx\nGD\tjs\tx\n", 3),
            ("code\tlead\tcovers\nGC\tJSS\tx\n", 2),
        ] {
            let err = parse_areas_tsv(bad).unwrap_err();
            assert!(err.starts_with(&format!("line {line}: ")), "{bad:?}: {err}");
        }
    }

    #[test]
    fn import_plan_twice_gives_nothing_the_second_time() {
        let incoming = parse_tsv(LEDGER).unwrap();
        let first = import_plan(&[], incoming.clone());
        assert_eq!(first, incoming);
        let second = import_plan(&first, incoming.clone());
        assert!(second.is_empty());
        // One new state row among the old ones.
        let mut more = incoming.clone();
        more.push(row(
            "LA-R002",
            "2026-11-13T00:00:00Z",
            Kind::Run,
            "accepted",
            "",
            &[],
        ));
        let plan = import_plan(&first, more);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].state, "accepted");
        // The same row twice in one file counts once; the order is kept.
        let twice = vec![
            incoming[2].clone(),
            incoming[0].clone(),
            incoming[2].clone(),
        ];
        let plan = import_plan(&[], twice);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].id, "LA-R002");
        assert_eq!(plan[1].id, "LA-R001");
    }

    fn graph() -> Vec<IdRow> {
        let mut rows = registry();
        rows.extend([
            row("GC-01", "1", Kind::Finding, "reported", "uaf", &["LA-R001"]),
            row("LA-R001", "2", Kind::Run, "started", "find it", &[]),
            row("LA-C001", "3", Kind::Change, "open", "fix", &["GC-01"]),
            row(
                "LA-R002",
                "4",
                Kind::Run,
                "started",
                "write it",
                &["LA-C001"],
            ),
            row(
                "LA-R003",
                "5",
                Kind::Run,
                "started",
                "review it",
                &["LA-C001"],
            ),
            row(
                "LA-V001",
                "6",
                Kind::Version,
                "built",
                "build",
                &["LA-C001"],
            ),
            row("LA-R004", "7", Kind::Run, "started", "unrelated", &[]),
            row(
                "LA-C001",
                "8",
                Kind::Change,
                "merged",
                "",
                &["LA-V001", "GC-01"],
            ),
        ]);
        rows
    }

    fn rel(t: &Trace, relation: &str) -> Vec<String> {
        t.links
            .iter()
            .filter(|l| l.relation == relation)
            .map(|l| l.view.id.clone())
            .collect()
    }

    #[test]
    fn trace_from_a_finding() {
        let t = trace(&graph(), "GC-01").unwrap();
        assert_eq!(t.subject.id, "GC-01");
        assert_eq!(rel(&t, "found by"), ["LA-R001"]);
        assert_eq!(rel(&t, "fixed by"), ["LA-C001"]);
        assert_eq!(rel(&t, "written or reviewed by"), ["LA-R002", "LA-R003"]);
        assert_eq!(rel(&t, "shipped in"), ["LA-V001"]);
        assert_eq!(t.links.len(), 5);
        assert!(!t.links.iter().any(|l| l.view.id == "LA-R004"));
        // A finding that nothing links to has no links.
        let lone = trace(&graph(), "LA-R004").unwrap();
        assert!(lone.links.is_empty());
    }

    #[test]
    fn trace_from_a_run_in_both_directions() {
        // LA-R001 is named by the finding (not the other way round).
        let t = trace(&graph(), "LA-R001").unwrap();
        assert_eq!(rel(&t, "found"), ["GC-01"]);
        assert!(rel(&t, "works for").is_empty());
        // LA-R002 names its change.
        let t = trace(&graph(), "LA-R002").unwrap();
        assert_eq!(rel(&t, "works for"), ["LA-C001"]);
        assert_eq!(rel(&t, "shipped in"), ["LA-V001"]);
        assert!(rel(&t, "found").is_empty());
    }

    #[test]
    fn trace_from_a_change_and_a_version() {
        let t = trace(&graph(), "LA-C001").unwrap();
        assert_eq!(rel(&t, "runs"), ["LA-R002", "LA-R003"]);
        assert_eq!(rel(&t, "fixes"), ["GC-01"]);
        assert_eq!(rel(&t, "shipped in"), ["LA-V001"]);
        let t = trace(&graph(), "LA-V001").unwrap();
        assert_eq!(rel(&t, "contains"), ["LA-C001"]);
        assert_eq!(rel(&t, "written or reviewed by"), ["LA-R002", "LA-R003"]);
        assert_eq!(t.links.len(), 3);
        assert!(trace(&graph(), "LA-R404").is_none());
        // Registry rows trace to nothing.
        assert!(trace(&graph(), "LA").unwrap().links.is_empty());
    }

    #[test]
    fn trace_never_lists_an_id_twice_under_one_relation() {
        // The change and the run name each other, and two changes share a version.
        let mut rows = registry();
        rows.extend([
            row(
                "LA-C001",
                "1",
                Kind::Change,
                "open",
                "a",
                &["LA-R001", "LA-R001"],
            ),
            row("LA-R001", "2", Kind::Run, "started", "r", &["LA-C001"]),
            row("LA-C002", "3", Kind::Change, "open", "b", &["LA-R001"]),
            row(
                "LA-V001",
                "4",
                Kind::Version,
                "built",
                "v",
                &["LA-C001", "LA-C002"],
            ),
        ]);
        let t = trace(&rows, "LA-R001").unwrap();
        assert_eq!(rel(&t, "works for"), ["LA-C001", "LA-C002"]);
        assert_eq!(rel(&t, "shipped in"), ["LA-V001"]);
        let t = trace(&rows, "LA-V001").unwrap();
        assert_eq!(rel(&t, "written or reviewed by"), ["LA-R001"]);
    }

    #[test]
    fn trailers_read_the_last_paragraph_only() {
        let msg = "ids: the store\n\nRun: LA-R999 in the body\n\nLead: LA\nRun: LA-R030\nChange: LA-C004\nFixes: GC-03 (fl-75f1f55c)\nRefs: GC-04\nCo-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>\n";
        let t = trailers(msg);
        assert_eq!(t.lead.as_deref(), Some("LA"));
        assert_eq!(t.run.as_deref(), Some("LA-R030"));
        assert_eq!(t.change.as_deref(), Some("LA-C004"));
        assert_eq!(t.fixes, ["GC-03"]);
        assert_eq!(t.refs, ["GC-04"]);
        // Trailers above the last paragraph are text, not trailers.
        let t = trailers("subject\n\nLead: LA\nRun: LA-R001\n\nJust words at the end.\n");
        assert_eq!(t, CommitIds::default());
        // Trailing blank lines do not make an empty last paragraph.
        let t = trailers("subject\n\nRun: LA-R001\n\n\n  \n");
        assert_eq!(t.run.as_deref(), Some("LA-R001"));
        assert_eq!(trailers(""), CommitIds::default());
        assert_eq!(trailers("no trailers at all"), CommitIds::default());
    }

    #[test]
    fn repeated_trailer_keys() {
        let t = trailers(
            "x\n\nRun: LA-R001\nRun: LA-R002\nLead: JS\nLead: LA\nFixes: GC-01\nFixes: GC-02 (another)\nRefs: A-1\nRefs: B-2\nChange:\nbogus line\nrun: lower\n",
        );
        assert_eq!(t.run.as_deref(), Some("LA-R002"));
        assert_eq!(t.lead.as_deref(), Some("LA"));
        assert_eq!(t.change, None, "a key without a value gives nothing");
        assert_eq!(t.fixes, ["GC-01", "GC-02"]);
        assert_eq!(t.refs, ["A-1", "B-2"]);
    }

    #[test]
    fn lane_run_ids() {
        assert_eq!(lane_run_id("LA-R023", "Ids writer!"), "la-r023-ids-writer");
        assert_eq!(
            lane_run_id("LA-R023", "  --Ids   writer--  "),
            "la-r023-ids-writer"
        );
        assert_eq!(lane_run_id("JS-R1000", "x"), "js-r1000-x");
        assert_eq!(lane_run_id("LA-R001", ""), "la-r001-run");
        assert_eq!(lane_run_id("LA-R001", "!!!"), "la-r001-run");
        assert_eq!(lane_run_id("LA-R001", "ünï"), "la-r001-n");
        let long = lane_run_id("LA-R001", &"a".repeat(60));
        assert_eq!(long, format!("la-r001-{}", "a".repeat(24)));
        // A cut that ends on a dash is trimmed again.
        let cut = lane_run_id("LA-R001", &format!("{} tail", "b".repeat(23)));
        assert_eq!(cut, format!("la-r001-{}", "b".repeat(23)));
        assert_eq!(lane_run_id("LA-R001", "A_B.c"), "la-r001-a-b-c");
    }

    #[test]
    fn work_states_give_run_states() {
        use WorkState::*;
        assert_eq!(run_state_for(Verified, None), Some("verified"));
        assert_eq!(run_state_for(Verified, Some(false)), Some("verified"));
        assert_eq!(run_state_for(Verified, Some(true)), Some("empty"));
        assert_eq!(run_state_for(Done, None), None);
        assert_eq!(run_state_for(Done, Some(false)), None);
        assert_eq!(run_state_for(Done, Some(true)), Some("empty"));
        for s in [
            Failed,
            VerifyFailed,
            Exhausted,
            DirtyAfterQuota,
            EndedDirty,
            EndedJobsQueued,
            Lost,
        ] {
            assert_eq!(run_state_for(s, None), Some("failed"), "{s:?}");
            assert_eq!(run_state_for(s, Some(true)), Some("failed"), "{s:?}");
        }
        for s in [Starting, Queued, Running] {
            assert_eq!(run_state_for(s, None), None, "{s:?}");
            assert_eq!(run_state_for(s, Some(true)), None, "{s:?}");
        }
        assert_eq!(run_state_for(EndedEmpty, None), Some("empty"));
    }

    #[test]
    fn lead_of_ids() {
        let rows = registry();
        assert_eq!(lead_of(&rows, "LA-R001").as_deref(), Some("LA"));
        assert_eq!(lead_of(&rows, "JS-V004").as_deref(), Some("JS"));
        assert_eq!(lead_of(&rows, "GC-03").as_deref(), Some("JS"));
        assert_eq!(lead_of(&rows, "NOPE-03"), None);
        assert_eq!(lead_of(&rows, "LA").as_deref(), Some("LA"));
        assert_eq!(lead_of(&rows, "GC").as_deref(), Some("JS"));
        assert_eq!(lead_of(&rows, "nonsense"), None);
    }
}
