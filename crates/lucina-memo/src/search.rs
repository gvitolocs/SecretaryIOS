//! Memo search for Lucina's tools ("what did I say in yesterday's memo
//! about X"): a date range, optional words, and back come each memo's
//! summary plus the transcript lines that match, with their times.
//!
//! Matching is accent- and case-insensitive; every word must appear in the
//! memo (title, summary or transcript), and the lines shown are those with
//! the most words. With no words, every memo in the range is returned with
//! its summary, so the model can read and answer itself.
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{clock, Memo, Status, Summary, Transcript};
use crate::store::{read_json, Store};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub at: f64,
    pub clock: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub event: Option<crate::model::EventLink>,
    pub id: String,
    pub title: String,
    pub recorded_at: DateTime<Utc>,
    pub duration: String,
    pub status: Status,
    pub language: Option<String>,
    pub overview: Option<String>,
    pub key_points: Vec<String>,
    pub action_items: Vec<String>,
    pub lines: Vec<Line>,
    pub url: String,
}

/// Lowercase, accents folded (à→a, ø→o, å→a, æ→ae), punctuation to spaces.
pub fn fold(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => o.push('a'),
            'è' | 'é' | 'ê' | 'ë' => o.push('e'),
            'ì' | 'í' | 'î' | 'ï' => o.push('i'),
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' => o.push('o'),
            'ù' | 'ú' | 'û' | 'ü' => o.push('u'),
            'æ' => o.push_str("ae"),
            'ç' => o.push('c'),
            'ñ' => o.push('n'),
            c if c.is_alphanumeric() => o.push(c),
            _ => o.push(' '),
        }
    }
    o
}

fn words(q: &str) -> Vec<String> {
    fold(q)
        .split_whitespace()
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
}

fn contains_word(hay: &str, w: &str) -> bool {
    // Prefix match on word starts: "preventiv" finds "preventivo".
    hay.split_whitespace().any(|h| h.starts_with(w))
}

pub struct Query<'a> {
    pub q: Option<&'a str>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: usize,
}

pub fn search(store: &Store, query: &Query<'_>, url: impl Fn(&str) -> String) -> Vec<Hit> {
    let ws = query.q.map(words).unwrap_or_default();
    let mut out = Vec::new();
    for m in store.list() {
        if query.from.is_some_and(|f| m.recorded_at < f)
            || query.to.is_some_and(|t| m.recorded_at >= t)
        {
            continue;
        }
        if m.status == Status::Uploading {
            continue;
        }
        let t: Option<Transcript> = read_json(&store.transcript_path(&m.id)).ok().flatten();
        let s: Option<Summary> = read_json(&store.summary_path(&m.id)).ok().flatten();
        if let Some(hit) = hit(&m, t.as_ref(), s.as_ref(), &ws, &url) {
            out.push(hit);
            if out.len() >= query.limit {
                break;
            }
        }
    }
    out
}

fn hit(
    m: &Memo,
    t: Option<&Transcript>,
    s: Option<&Summary>,
    ws: &[String],
    url: &impl Fn(&str) -> String,
) -> Option<Hit> {
    let summary_text = s
        .map(|s| {
            let mut v = vec![s.title.clone(), s.overview.clone()];
            v.extend(
                s.key_points
                    .iter()
                    .chain(&s.decisions)
                    .chain(&s.action_items)
                    .chain(&s.definitions)
                    .chain(&s.questions)
                    .map(|i| i.text.clone()),
            );
            v.extend(s.tags.iter().cloned());
            v.join(" ")
        })
        .unwrap_or_default();
    let event_text = m
        .event
        .as_ref()
        .map(|e| {
            format!(
                "{} {} {} {} {}",
                e.title,
                e.series,
                e.location.as_deref().unwrap_or_default(),
                e.attendees.join(" "),
                e.calendar.as_deref().unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let all = fold(&format!(
        "{} {summary_text} {event_text} {}",
        m.title,
        t.map(Transcript::text).unwrap_or_default()
    ));
    if !ws.iter().all(|w| contains_word(&all, w)) {
        return None;
    }
    let mut lines: Vec<(usize, Line)> = Vec::new();
    if let (Some(t), false) = (t, ws.is_empty()) {
        for seg in &t.segments {
            let f = fold(&seg.text);
            let n = ws.iter().filter(|w| contains_word(&f, w)).count();
            if n > 0 {
                lines.push((
                    n,
                    Line {
                        at: seg.start,
                        clock: clock(seg.start),
                        text: seg.text.clone(),
                    },
                ));
            }
        }
        lines.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.at.total_cmp(&b.1.at)));
        lines.truncate(8);
        lines.sort_by(|a, b| a.1.at.total_cmp(&b.1.at));
    }
    Some(Hit {
        event: m.event.clone(),
        id: m.id.clone(),
        title: m.title.clone(),
        recorded_at: m.recorded_at,
        duration: clock(m.duration_sec),
        status: m.status,
        language: m.language.clone(),
        overview: s.map(|s| s.overview.clone()),
        key_points: s
            .map(|s| s.key_points.iter().map(|i| i.text.clone()).collect())
            .unwrap_or_default(),
        action_items: s
            .map(|s| s.action_items.iter().map(|i| i.text.clone()).collect())
            .unwrap_or_default(),
        lines: lines.into_iter().map(|(_, l)| l).collect(),
        url: url(&m.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold("Perché? Ålborg, Æble!"), "perche  alborg  aeble ");
        assert!(contains_word(
            &fold("il preventivo della cucina"),
            "preventiv"
        ));
        assert!(!contains_word(&fold("anteprima"), "prima"));
        assert_eq!(words("a la Cucina"), vec!["la", "cucina"]);
    }
}
