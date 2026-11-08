//! "About you": what Lucina knows about the owner, as facts she keeps up to date herself.
//!
//! One JSON file (`<data>/about-you.json`, mode 0600), written whole through a temporary. Each fact has
//! its text, a category, where it came from, when it was learned and last confirmed, how long it holds
//! (a date or a condition such as "until he graduates") and a status. A fact that stops holding is
//! never deleted: it moves to history (`expired` or `retracted`) so she can say "you used to…", and
//! every change is logged on the fact. Values that look like secrets are refused: those go to Infisical.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;

/// Longest fact text.
pub const MAX_TEXT: usize = 400;
/// Facts kept, active and history together (the oldest history goes first).
pub const MAX_FACTS: usize = 2000;
/// The always-on summary's size.
pub const SUMMARY_CHARS: usize = 3000;
/// The summary's first words: [`crate::recent::clip_memory_for_reply`] keeps that block whole.
pub const SUMMARY_MARKER: &str = "About Giuseppe";

pub const CATEGORIES: &[&str] = &[
    "identity",
    "education",
    "work",
    "money",
    "health",
    "home",
    "people",
    "preferences",
    "schedule",
    "other",
];
pub const SOURCES: &[&str] = &[
    "chat",
    "email",
    "document",
    "calendar",
    "owner-edit",
    "seed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Expired,
    Retracted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// One of [`SOURCES`].
    pub kind: String,
    /// What it came from: a timeline item, a share, a calendar event, a note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

/// How long a fact holds: a last day, a condition, or both.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Validity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub at: DateTime<Utc>,
    /// `lucina`, `owner`, `extractor`, `expiry`, `seed`.
    pub by: String,
    /// `added`, `updated`, `confirmed`, `ended`, `retracted`, `expired`.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The text before an update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: String,
    pub text: String,
    pub category: String,
    pub source: Source,
    pub learned_at: DateTime<Utc>,
    pub last_confirmed: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<Validity>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub changes: Vec<Change>,
}

/// A new fact or a change to one, as a tool, the extractor or the page sends it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// `YYYY-MM-DD`; an empty string clears it.
    #[serde(default)]
    pub valid_until: Option<String>,
    /// An empty string clears it.
    #[serde(default)]
    pub valid_until_condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AboutError {
    #[error("{0}")]
    Invalid(String),
    #[error("that looks like a secret: secrets go to Infisical, never into the memory about him")]
    Secret,
    #[error("no such fact")]
    NotFound,
    #[error("the memory could not be written")]
    Io,
}

fn bad(m: impl Into<String>) -> AboutError {
    AboutError::Invalid(m.into())
}

/// A text that may be kept: one line or a few, no secret shapes, no known secret value.
pub fn check_text(text: &str, max: usize) -> Result<String, AboutError> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return Err(bad("the text is empty"));
    }
    if t.chars().count() > max {
        return Err(bad(format!("at most {max} characters")));
    }
    if !lucina_secrets::redact::detect(&t).is_empty()
        || !lucina_secrets::labelled::detect(&t).is_empty()
        || lucina_secrets::redactor().redact(&t).as_ref() != t
    {
        return Err(AboutError::Secret);
    }
    Ok(t)
}

fn check_category(c: &str) -> Result<String, AboutError> {
    let c = c.trim().to_lowercase();
    CATEGORIES
        .contains(&c.as_str())
        .then_some(c)
        .ok_or_else(|| bad(format!("category: one of {}", CATEGORIES.join(", "))))
}

fn random_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    format!(
        "af-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// Apply a draft's validity to `current`: `None` keeps it, `""` clears a part.
fn validity(current: Option<Validity>, d: &Draft) -> Result<Option<Validity>, AboutError> {
    let mut v = current.unwrap_or_default();
    if let Some(date) = d.valid_until.as_deref().map(str::trim) {
        v.date = if date.is_empty() {
            None
        } else {
            Some(
                NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .map_err(|_| bad("validUntil: YYYY-MM-DD"))?,
            )
        };
    }
    if let Some(c) = d.valid_until_condition.as_deref() {
        v.condition = if c.trim().is_empty() {
            None
        } else {
            Some(check_text(c, 200)?)
        };
    }
    Ok((v.date.is_some() || v.condition.is_some()).then_some(v))
}

pub struct AboutStore {
    path: PathBuf,
    facts: Mutex<Option<Vec<Fact>>>,
}

impl std::fmt::Debug for AboutStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AboutStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl AboutStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            facts: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with<T>(&self, write: bool, f: impl FnOnce(&mut Vec<Fact>) -> T) -> Result<T, AboutError> {
        let mut g = self.facts.lock().unwrap_or_else(|e| e.into_inner());
        let facts = g.get_or_insert_with(|| {
            std::fs::read(&self.path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        });
        let out = f(facts);
        if write {
            let excess = facts.len().saturating_sub(MAX_FACTS);
            if excess > 0 {
                let mut drop = excess;
                facts.retain(|x| {
                    if drop > 0 && x.status != Status::Active {
                        drop -= 1;
                        return false;
                    }
                    true
                });
            }
            let bytes = serde_json::to_vec_pretty(facts).map_err(|_| AboutError::Io)?;
            write_atomic(&self.path, &bytes).map_err(|e| {
                tracing::warn!(path = %self.path.display(), error = %e.kind(), "about-you memory not written");
                AboutError::Io
            })?;
        }
        Ok(out)
    }

    /// Every fact, active first, then history newest first.
    pub fn all(&self) -> Vec<Fact> {
        let mut v = self.with(false, |f| f.clone()).unwrap_or_default();
        v.sort_by(|a, b| {
            (a.status != Status::Active)
                .cmp(&(b.status != Status::Active))
                .then(
                    b.ended_at
                        .unwrap_or(b.learned_at)
                        .cmp(&a.ended_at.unwrap_or(a.learned_at)),
                )
        });
        v
    }

    pub fn active(&self) -> Vec<Fact> {
        self.all()
            .into_iter()
            .filter(|f| f.status == Status::Active)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Fact> {
        self.with(false, |f| f.iter().find(|x| x.id == id).cloned())
            .ok()
            .flatten()
    }

    /// A new active fact. The same text already active is confirmed instead.
    pub fn add(
        &self,
        d: &Draft,
        source: Source,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Fact, AboutError> {
        let text = check_text(d.text.as_deref().unwrap_or(""), MAX_TEXT)?;
        let category = check_category(d.category.as_deref().unwrap_or("other"))?;
        if !SOURCES.contains(&source.kind.as_str()) {
            return Err(bad(format!("source: one of {}", SOURCES.join(", "))));
        }
        let valid_until = validity(None, d)?;
        self.with(true, |facts| {
            if let Some(f) = facts
                .iter_mut()
                .find(|f| f.status == Status::Active && f.text.eq_ignore_ascii_case(&text))
            {
                f.last_confirmed = now;
                f.changes.push(Change {
                    at: now,
                    by: by.into(),
                    action: "confirmed".into(),
                    note: None,
                    before: None,
                });
                if valid_until.is_some() {
                    f.valid_until = valid_until.clone();
                }
                return f.clone();
            }
            let f = Fact {
                id: random_id(),
                text,
                category,
                source,
                learned_at: now,
                last_confirmed: now,
                valid_until,
                status: Status::Active,
                ended_at: None,
                changes: vec![Change {
                    at: now,
                    by: by.into(),
                    action: "added".into(),
                    note: None,
                    before: None,
                }],
            };
            facts.push(f.clone());
            f
        })
    }

    fn change(
        &self,
        id: &str,
        f: impl FnOnce(&mut Fact) -> Result<(), AboutError>,
    ) -> Result<Fact, AboutError> {
        self.with(true, |facts| {
            let fact = facts
                .iter_mut()
                .find(|x| x.id == id)
                .ok_or(AboutError::NotFound)?;
            f(fact)?;
            Ok(fact.clone())
        })?
    }

    /// New text, category or validity for an active fact. The old text stays in its change log.
    pub fn update(
        &self,
        id: &str,
        d: &Draft,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Fact, AboutError> {
        let text = d
            .text
            .as_deref()
            .map(|t| check_text(t, MAX_TEXT))
            .transpose()?;
        let category = d.category.as_deref().map(check_category).transpose()?;
        self.change(id, |f| {
            if f.status != Status::Active {
                return Err(bad("only an active fact can change; add a new one"));
            }
            let before = text
                .as_ref()
                .filter(|t| **t != f.text)
                .map(|_| f.text.clone());
            if let Some(t) = text {
                f.text = t;
            }
            if let Some(c) = category {
                f.category = c;
            }
            f.valid_until = validity(f.valid_until.take(), d)?;
            f.last_confirmed = now;
            f.changes.push(Change {
                at: now,
                by: by.into(),
                action: "updated".into(),
                note: None,
                before,
            });
            Ok(())
        })
    }

    pub fn confirm(&self, id: &str, by: &str, now: DateTime<Utc>) -> Result<Fact, AboutError> {
        self.change(id, |f| {
            f.last_confirmed = now;
            f.changes.push(Change {
                at: now,
                by: by.into(),
                action: "confirmed".into(),
                note: None,
                before: None,
            });
            Ok(())
        })
    }

    /// It held and stopped holding: history, "you used to".
    pub fn end(
        &self,
        id: &str,
        reason: Option<&str>,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Fact, AboutError> {
        self.close(id, Status::Expired, "ended", reason, by, now)
    }

    /// It was never true: history, marked wrong.
    pub fn retract(
        &self,
        id: &str,
        reason: Option<&str>,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Fact, AboutError> {
        self.close(id, Status::Retracted, "retracted", reason, by, now)
    }

    fn close(
        &self,
        id: &str,
        to: Status,
        action: &str,
        reason: Option<&str>,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Fact, AboutError> {
        let note = reason.map(|r| check_text(r, 300)).transpose()?;
        self.change(id, |f| {
            if f.status != Status::Active {
                return Err(bad("that fact is already in history"));
            }
            f.status = to;
            f.ended_at = Some(now);
            f.changes.push(Change {
                at: now,
                by: by.into(),
                action: action.into(),
                note,
                before: None,
            });
            Ok(())
        })
    }

    /// Facts whose last day is before `today` go to history. Returns them.
    pub fn expire_due(&self, today: NaiveDate, now: DateTime<Utc>) -> Vec<Fact> {
        self.with(true, |facts| {
            let mut out = Vec::new();
            for f in facts.iter_mut().filter(|f| f.status == Status::Active) {
                if f.valid_until
                    .as_ref()
                    .and_then(|v| v.date)
                    .is_some_and(|d| d < today)
                {
                    f.status = Status::Expired;
                    f.ended_at = Some(now);
                    f.changes.push(Change {
                        at: now,
                        by: "expiry".into(),
                        action: "expired".into(),
                        note: None,
                        before: None,
                    });
                    out.push(f.clone());
                }
            }
            out
        })
        .unwrap_or_default()
    }

    /// Facts that hold until something happens, for the extractor to re-check against an event.
    pub fn conditional(&self) -> Vec<Fact> {
        self.active()
            .into_iter()
            .filter(|f| {
                f.valid_until
                    .as_ref()
                    .is_some_and(|v| v.condition.is_some())
            })
            .collect()
    }

    /// Facts matching words of `q` (all of them), active first.
    pub fn search(&self, q: &str, include_history: bool) -> Vec<Fact> {
        let words: Vec<String> = q
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        self.all()
            .into_iter()
            .filter(|f| include_history || f.status == Status::Active)
            .filter(|f| {
                let hay = format!("{} {}", f.text, f.category).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }
}

/// One line per fact, for prompts: `[id] (category, until …) text`.
pub fn line(f: &Fact) -> String {
    let until = match &f.valid_until {
        Some(Validity {
            date: Some(d),
            condition: Some(c),
        }) => format!(", until {d} or {c}"),
        Some(Validity { date: Some(d), .. }) => format!(", until {d}"),
        Some(Validity {
            condition: Some(c), ..
        }) => format!(", until {c}"),
        _ => String::new(),
    };
    let ended = match (f.status, f.ended_at) {
        (Status::Expired, Some(at)) => format!(" [no longer true since {}]", at.date_naive()),
        (Status::Retracted, _) => " [was wrong]".to_owned(),
        _ => String::new(),
    };
    format!("[{}] ({}{until}) {}{ended}", f.id, f.category, f.text)
}

/// The always-on block: active facts, grouped by category, at most `max` characters.
pub fn summary(facts: &[Fact], max: usize) -> String {
    let active: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.status == Status::Active)
        .collect();
    if active.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "{SUMMARY_MARKER} (what you know about him; facts in your memory, kept up to date by you; connect them to what he says \
         without being asked; look up detail and history with about_me):"
    );
    for c in CATEGORIES {
        for f in active.iter().filter(|f| f.category == *c) {
            let l = format!("\n- {}", line(f));
            if out.chars().count() + l.chars().count() > max {
                out.push_str("\n- … (more with about_me)");
                return out;
            }
            out.push_str(&l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AboutStore) {
        let d = tempfile::tempdir().unwrap();
        let s = AboutStore::new(d.path().join("about-you.json"));
        (d, s)
    }

    fn draft(text: &str, cat: &str) -> Draft {
        Draft {
            text: Some(text.into()),
            category: Some(cat.into()),
            ..Draft::default()
        }
    }

    fn chat() -> Source {
        Source {
            kind: "chat".into(),
            reference: "01M6".into(),
        }
    }

    #[test]
    fn a_fact_lives_and_moves_to_history_never_deleted() {
        let (_d, s) = store();
        let now = Utc::now();
        let f = s
            .add(
                &draft("Works at AU, 12 h/week", "work"),
                chat(),
                "lucina",
                now,
            )
            .unwrap();
        let u = s
            .update(
                &f.id,
                &draft("Works at AU (IMC), 12 h/week", "work"),
                "owner",
                now,
            )
            .unwrap();
        assert_eq!(
            u.changes.last().unwrap().before.as_deref(),
            Some("Works at AU, 12 h/week")
        );
        let e = s.end(&f.id, Some("contract ended"), "lucina", now).unwrap();
        assert_eq!(e.status, Status::Expired);
        assert!(s.active().is_empty());
        assert_eq!(s.all().len(), 1, "kept in history");
        assert!(line(&s.all()[0]).contains("no longer true since"));
        assert!(s.update(&f.id, &draft("x", "work"), "owner", now).is_err());
        // Saved and read back.
        let again = AboutStore::new(s.path());
        assert_eq!(again.all()[0].changes.len(), 3);
    }

    #[test]
    fn the_same_text_confirms_instead_of_doubling() {
        let (_d, s) = store();
        let now = Utc::now();
        let a = s
            .add(&draft("Lives in Aarhus", "home"), chat(), "lucina", now)
            .unwrap();
        let b = s
            .add(&draft("lives in aarhus", "home"), chat(), "extractor", now)
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(s.active().len(), 1);
        assert_eq!(b.changes.last().unwrap().action, "confirmed");
    }

    #[test]
    fn dates_expire_facts() {
        let (_d, s) = store();
        let now = Utc::now();
        let mut d = draft("Contract at IMC", "work");
        d.valid_until = Some("2026-12-31".into());
        let f = s.add(&d, chat(), "lucina", now).unwrap();
        assert!(
            s.expire_due(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(), now)
                .is_empty(),
            "holds on its last day"
        );
        let gone = s.expire_due(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), now);
        assert_eq!(gone[0].id, f.id);
        assert_eq!(s.get(&f.id).unwrap().changes.last().unwrap().by, "expiry");
    }

    #[test]
    fn secrets_are_refused() {
        let (_d, s) = store();
        for t in [
            "my github token is ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "the wifi password is Cavolo-23",
        ] {
            assert_eq!(
                s.add(&draft(t, "home"), chat(), "lucina", Utc::now())
                    .unwrap_err(),
                AboutError::Secret,
                "{t}"
            );
        }
        assert!(s.all().is_empty());
    }

    #[test]
    fn bad_input_is_refused() {
        let (_d, s) = store();
        assert!(s
            .add(&draft("x", "astrology"), chat(), "lucina", Utc::now())
            .is_err());
        assert!(s
            .add(&draft("", "work"), chat(), "lucina", Utc::now())
            .is_err());
        let mut d = draft("y", "work");
        d.valid_until = Some("31/12/2026".into());
        assert!(s.add(&d, chat(), "lucina", Utc::now()).is_err());
        assert!(s
            .add(
                &draft("y", "work"),
                Source {
                    kind: "rumour".into(),
                    reference: String::new()
                },
                "lucina",
                Utc::now()
            )
            .is_err());
    }

    #[test]
    fn the_summary_is_bounded_and_marked() {
        let (_d, s) = store();
        for i in 0..80 {
            s.add(
                &draft(&format!("fact number {i} with some words"), "other"),
                chat(),
                "seed",
                Utc::now(),
            )
            .unwrap();
        }
        let out = summary(&s.active(), 1000);
        assert!(out.starts_with(SUMMARY_MARKER));
        assert!(out.chars().count() <= 1030);
        assert!(out.ends_with("(more with about_me)"));
        assert!(summary(&[], 1000).is_empty());
    }
}
