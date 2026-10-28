//! The record a caller reports ([`NewRecord`]), its validation, and the stored
//! form ([`StoredRecord`]).

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::OpsError;

pub const MAX_TITLE: usize = 200;
pub const MAX_BODY: usize = 20_000;
pub const MAX_LINKS: usize = 20;
pub const MAX_LINK: usize = 500;
pub const MAX_TIMELINE: usize = 50;
pub const MAX_LINE: usize = 500;
pub const MAX_CAUSE: usize = 4000;
pub const MAX_FOLLOW_UPS: usize = 20;
pub const MAX_DEDUP_KEY: usize = 200;
/// How far in the future `at` may be (clock skew between hosts).
pub const MAX_FUTURE_MINUTES: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Deploy,
    Incident,
    Decision,
    Status,
    Finding,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Deploy,
        Kind::Incident,
        Kind::Decision,
        Kind::Status,
        Kind::Finding,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Deploy => "deploy",
            Kind::Incident => "incident",
            Kind::Decision => "decision",
            Kind::Status => "status",
            Kind::Finding => "finding",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s.trim().to_ascii_lowercase())
    }

    /// The project page section of this kind.
    pub fn section(self) -> &'static str {
        match self {
            Kind::Status => "Status",
            Kind::Decision => "Decisions",
            Kind::Incident => "Incidents",
            Kind::Deploy => "Deploys",
            Kind::Finding => "Findings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// What a caller reports (the wire contract of `POST /api/secretary/ops/records`
/// and the MCP tool `ops_record`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct NewRecord {
    pub kind: Kind,
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub timeline: Vec<String>,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub fix: Option<String>,
    #[serde(default)]
    pub follow_ups: Vec<String>,
    #[serde(default)]
    pub at: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub dedup_key: Option<String>,
}

impl NewRecord {
    /// A record with only the required fields.
    pub fn new(kind: Kind, project: &str, title: &str) -> Self {
        Self {
            kind,
            project: project.into(),
            title: title.into(),
            body: String::new(),
            links: Vec::new(),
            commit: None,
            host: None,
            severity: None,
            timeline: Vec::new(),
            cause: None,
            fix: None,
            follow_ups: Vec::new(),
            at: None,
            dedup_key: None,
        }
    }
}

/// Vault pages of a record, relative to the vault root (`Log/2026-10-28.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paths {
    pub log: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident: Option<String>,
}

/// One line of `records.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRecord {
    pub id: String,
    pub kind: Kind,
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub timeline: Vec<String>,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub fix: Option<String>,
    #[serde(default)]
    pub follow_ups: Vec<String>,
    /// In the owner's time zone.
    pub at: DateTime<FixedOffset>,
    pub recorded_at: DateTime<Utc>,
    pub source: String,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default)]
    pub redacted: bool,
    #[serde(default)]
    pub honcho_synced: bool,
    pub paths: Paths,
}

/// What `record` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub id: String,
    pub duplicate: bool,
    pub redacted: bool,
    /// `ok` | `pending` | `off`
    pub honcho: String,
    pub paths: Paths,
}

fn invalid(field: &'static str, message: impl Into<String>) -> OpsError {
    OpsError::Invalid {
        field,
        message: message.into(),
    }
}

/// Newlines (and other control characters) become spaces; trimmed.
pub fn single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.push(if c.is_control() { ' ' } else { c });
    }
    out.trim().to_owned()
}

/// Remove every `<!-- ops:` from caller text, so nobody can forge a marker.
pub fn strip_markers(s: &str) -> String {
    let mut out = s.to_owned();
    while let Some(i) = out.find(crate::render::MARKER_OPEN) {
        out.replace_range(i..i + crate::render::MARKER_OPEN.len(), "");
    }
    out
}

fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=48).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `"Lucina Agents"` -> `"lucina-agents"` (for the error message).
fn suggest_slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(48).collect()
}

fn check_len(field: &'static str, s: &str, max: usize) -> Result<(), OpsError> {
    let n = s.chars().count();
    if n > max {
        return Err(invalid(
            field,
            format!("must be at most {max} characters (got {n})"),
        ));
    }
    Ok(())
}

fn opt_text(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// A validated record, before redaction.
#[derive(Debug, Clone)]
pub(crate) struct Valid {
    pub kind: Kind,
    pub project: String,
    pub title: String,
    pub body: String,
    pub links: Vec<String>,
    pub commit: Option<String>,
    pub host: Option<String>,
    pub severity: Option<Severity>,
    pub timeline: Vec<String>,
    pub cause: Option<String>,
    pub fix: Option<String>,
    pub follow_ups: Vec<String>,
    pub at: DateTime<Utc>,
    pub dedup_key: Option<String>,
}

pub(crate) fn validate(r: NewRecord, now: DateTime<Utc>) -> Result<Valid, OpsError> {
    let project = r.project.trim().to_lowercase();
    if !is_slug(&project) {
        let hint = suggest_slug(&project);
        let msg = if hint.is_empty() || hint == project {
            "must be a slug: lowercase letters, digits and '-', 1 to 48 characters".to_owned()
        } else {
            format!("must be a slug (lowercase letters, digits and '-', 1 to 48 characters), e.g. \"{hint}\"")
        };
        return Err(invalid("project", msg));
    }
    let title = single_line(&r.title);
    if title.is_empty() {
        return Err(invalid("title", "is required"));
    }
    check_len("title", &title, MAX_TITLE)?;
    let body = r.body.trim_end().to_owned();
    check_len("body", &body, MAX_BODY)?;
    if r.links.len() > MAX_LINKS {
        return Err(invalid("links", format!("at most {MAX_LINKS} links")));
    }
    let mut links = Vec::with_capacity(r.links.len());
    for l in r.links {
        let l = l.trim().to_owned();
        if l.is_empty() {
            return Err(invalid("links", "a link is empty"));
        }
        if l.chars().any(char::is_control) {
            return Err(invalid("links", "a link contains a newline"));
        }
        check_len("links", &l, MAX_LINK)?;
        links.push(l);
    }
    let commit = match opt_text(r.commit) {
        None => None,
        Some(c) => {
            let c = c.to_ascii_lowercase();
            if !(7..=40).contains(&c.len()) || !c.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("commit", "must be 7 to 40 hex characters"));
            }
            Some(c)
        }
    };
    let host = match opt_text(r.host) {
        None => None,
        Some(h) => {
            if !(1..=64).contains(&h.len())
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                return Err(invalid(
                    "host",
                    "must be 1 to 64 characters of letters, digits, '.', '_' or '-'",
                ));
            }
            Some(h)
        }
    };
    let severity = match r.kind {
        Kind::Incident => Some(r.severity.unwrap_or(Severity::Medium)),
        _ => None,
    };
    if r.timeline.len() > MAX_TIMELINE {
        return Err(invalid("timeline", format!("at most {MAX_TIMELINE} lines")));
    }
    let mut timeline = Vec::new();
    for t in r.timeline {
        let t = single_line(&t);
        check_len("timeline", &t, MAX_LINE)?;
        if !t.is_empty() {
            timeline.push(t);
        }
    }
    let cause = opt_text(r.cause);
    if let Some(c) = &cause {
        check_len("cause", c, MAX_CAUSE)?;
    }
    let fix = opt_text(r.fix);
    if let Some(f) = &fix {
        check_len("fix", f, MAX_CAUSE)?;
    }
    if r.follow_ups.len() > MAX_FOLLOW_UPS {
        return Err(invalid(
            "follow_ups",
            format!("at most {MAX_FOLLOW_UPS} items"),
        ));
    }
    let mut follow_ups = Vec::new();
    for f in r.follow_ups {
        let f = single_line(&f);
        check_len("follow_ups", &f, MAX_LINE)?;
        if !f.is_empty() {
            follow_ups.push(f);
        }
    }
    let at = match r.at {
        None => now,
        Some(at) => {
            let at = at.with_timezone(&Utc);
            if at > now + chrono::Duration::minutes(MAX_FUTURE_MINUTES) {
                return Err(invalid("at", "is in the future"));
            }
            at
        }
    };
    let dedup_key = opt_text(r.dedup_key);
    if let Some(k) = &dedup_key {
        check_len("dedup_key", k, MAX_DEDUP_KEY)?;
    }
    Ok(Valid {
        kind: r.kind,
        project,
        title,
        body,
        links,
        commit,
        host,
        severity,
        timeline,
        cause,
        fix,
        follow_ups,
        at,
        dedup_key,
    })
}

/// Redact every string field (and strip forged markers from free text).
/// `true` when the redactor changed something.
pub(crate) fn redact(v: &mut Valid) -> Result<bool, OpsError> {
    let r = lucina_secrets::redactor();
    let mut changed = false;
    // Structured fields must survive as they are: a secret there is refused.
    for (field, value) in [
        ("project", Some(&v.project)),
        ("commit", v.commit.as_ref()),
        ("host", v.host.as_ref()),
    ] {
        if let Some(s) = value {
            if r.contains_secret(s) {
                return Err(invalid(field, "looks like a secret"));
            }
        }
    }
    let mut text = |s: &mut String| {
        changed |= r.redact_string(s);
        *s = strip_markers(s);
    };
    text(&mut v.title);
    text(&mut v.body);
    v.links.iter_mut().for_each(&mut text);
    v.timeline.iter_mut().for_each(&mut text);
    v.follow_ups.iter_mut().for_each(&mut text);
    if let Some(c) = v.cause.as_mut() {
        text(c);
    }
    if let Some(f) = v.fix.as_mut() {
        text(f);
    }
    if let Some(k) = v.dedup_key.as_mut() {
        text(k);
    }
    Ok(changed)
}

/// Lowercased, whitespace collapsed, trailing punctuation trimmed.
pub fn normalize_title(title: &str) -> String {
    let lower = title.to_lowercase();
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c == '…' || c.is_whitespace())
        .to_owned()
}

/// `dedup_key`, else `kind|project|normalized title|commit`.
pub fn dedup_key(
    kind: Kind,
    project: &str,
    title: &str,
    commit: Option<&str>,
    explicit: Option<&str>,
) -> String {
    match explicit {
        Some(k) => k.to_owned(),
        None => format!(
            "{}|{project}|{}|{}",
            kind.as_str(),
            normalize_title(title),
            commit.unwrap_or("")
        ),
    }
}

/// First 16 hex characters of sha256(key).
pub fn record_id(key: &str) -> String {
    let d = Sha256::digest(key.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Is `s` a record id (16 lowercase hex)?
pub fn is_record_id(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_hints() {
        let now = Utc::now();
        let mut r = NewRecord::new(Kind::Decision, " Lucina Agents ", "x");
        let e = validate(r.clone(), now).unwrap_err();
        assert!(e.to_string().contains("\"lucina-agents\""), "{e}");
        r.project = "  Lucina-Agents ".into();
        assert_eq!(validate(r, now).unwrap().project, "lucina-agents");
        let r = NewRecord::new(Kind::Decision, "-x", "x");
        assert!(validate(r, now).is_err());
    }

    #[test]
    fn titles_and_keys() {
        assert_eq!(
            normalize_title("  Lucina   Agents LIVE!! "),
            "lucina agents live"
        );
        assert_eq!(
            dedup_key(Kind::Deploy, "lucina", "Live.", Some("abc1234"), None),
            "deploy|lucina|live|abc1234"
        );
        let id = record_id("x");
        assert!(is_record_id(&id));
        assert_eq!(strip_markers("a <!-- <!-- ops:ops:dead -->"), "a dead -->");
    }

    #[test]
    fn field_limits() {
        let now = Utc::now();
        let mut r = NewRecord::new(Kind::Incident, "pi", "line\none");
        r.commit = Some("ABCDEF1".into());
        let v = validate(r.clone(), now).unwrap();
        assert_eq!(v.title, "line one");
        assert_eq!(v.commit.as_deref(), Some("abcdef1"));
        assert_eq!(v.severity, Some(Severity::Medium));
        r.commit = Some("xyz".into());
        assert!(matches!(
            validate(r.clone(), now),
            Err(OpsError::Invalid {
                field: "commit",
                ..
            })
        ));
        r.commit = None;
        r.at = Some((now + chrono::Duration::minutes(30)).fixed_offset());
        assert!(matches!(
            validate(r.clone(), now),
            Err(OpsError::Invalid { field: "at", .. })
        ));
        r.at = None;
        r.links = vec!["a\nb".into()];
        assert!(validate(r.clone(), now).is_err());
        r.links = vec![];
        r.host = Some("bad host".into());
        assert!(validate(r, now).is_err());
        let mut d = NewRecord::new(Kind::Deploy, "x", "t");
        d.severity = Some(Severity::High);
        assert_eq!(validate(d, now).unwrap().severity, None);
    }
}
