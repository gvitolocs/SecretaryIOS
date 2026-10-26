//! Lucina's *resoconto* of a memo: overview, key points, decisions and
//! action items, in the memo's language, as JSON. Only the transcript text
//! goes to the model (Lucina's usual DeepSeek brain); the audio never does.
//!
//! Long transcripts (over [`CHUNK_CHARS`]) are summarised in parts first and
//! the parts merged, so a two-hour recording fits the context.
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_llm::{ChatModel, ChatRequest, LlmError, Message};
use serde::Deserialize;

use crate::model::{clock, EventKind, Item, Memo, Summary, Transcript};

pub const CHUNK_CHARS: usize = 60_000;

#[derive(Debug, thiserror::Error)]
pub enum SummaryError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("summary JSON: {0}")]
    Json(String),
}

const SYSTEM: &str = r#"You are Lucina, Giuseppe's personal assistant. Giuseppe recorded a voice memo; you get Whisper's machine transcript with timestamps. Write his resoconto (report) of it.

Rules:
- Write in the language of the memo (field "language" below: it = Italian, en = English, da = Danish). If he mixes languages, use the one he speaks most.
- Be faithful: only what is in the transcript. Whisper makes mistakes with names and technical words; fix obvious ones from context, never invent facts, people, dates or numbers. If a passage is unclear, say so briefly instead of guessing.
- "title": 3 to 8 words, specific (not "Voice memo"), no date, no quotes, no trailing period.
- "overview": 2 to 5 sentences: what the memo is about and why it matters to him. Address him as "you" (tu in Italian, du in Danish) when natural.
- "keyPoints": the main points, each one sentence. "decisions": things decided. "actionItems": concrete things someone has to do, with "owner" ("me" when Giuseppe does it, otherwise the person's name) and "due" (a date as YYYY-MM-DD when one is said or clear from the recording date, otherwise the words used, or omit it).
- "at": the start second of the transcript line where it is said (a number from the [mm:ss] marks), when you can tell.
- Empty lists are fine. A memo with no real speech gets an overview saying so and empty lists.
- "tags": 1 to 5 lowercase topic words (no '#').

Answer with one JSON object only:
{"title":"...","overview":"...","keyPoints":[{"text":"...","at":12}],"decisions":[{"text":"..."}],"actionItems":[{"text":"...","owner":"me","due":"2026-10-30","at":95}],"tags":["..."]}"#;

const PART_SYSTEM: &str = r#"You are Lucina, Giuseppe's personal assistant. This is one part of the Whisper transcript of a long voice memo. Write compact notes of this part only: what is said, decisions, things to do (who, when), with the [mm:ss] mark of each. Keep names, numbers and dates exactly. Use the memo's language. Plain text, at most 40 lines."#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    definitions: Vec<RawItem>,
    #[serde(default)]
    questions: Vec<RawItem>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    key_points: Vec<RawItem>,
    #[serde(default)]
    decisions: Vec<RawItem>,
    #[serde(default)]
    action_items: Vec<RawItem>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawItem {
    Text(String),
    Full {
        #[serde(default)]
        text: String,
        #[serde(default)]
        at: Option<serde_json::Value>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        due: Option<String>,
    },
}

/// `[mm:ss] text` lines.
pub fn timed_lines(t: &Transcript) -> Vec<String> {
    t.segments
        .iter()
        .map(|s| match &s.speaker {
            Some(sp) => format!("[{}] {sp}: {}", clock(s.start), s.text),
            None => format!("[{}] {}", clock(s.start), s.text),
        })
        .collect()
}

fn local(at: DateTime<Utc>, tz: Tz) -> String {
    at.with_timezone(&tz)
        .format("%A %Y-%m-%d %H:%M")
        .to_string()
}

/// Seconds from `12`, `"12"`, `"1:05"` or `"1:02:05"`.
fn seconds(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let s = s.trim().trim_matches(|c| c == '[' || c == ']');
            if let Ok(x) = s.parse::<f64>() {
                return Some(x);
            }
            let parts: Vec<f64> = s
                .split(':')
                .map(|p| p.parse::<f64>())
                .collect::<Result<_, _>>()
                .ok()?;
            Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
        }
        _ => None,
    }
    .filter(|x| x.is_finite() && *x >= 0.0)
}

fn items(raw: Vec<RawItem>, duration: f64, keep_owner: bool) -> Vec<Item> {
    raw.into_iter()
        .filter_map(|r| {
            let (text, at, owner, due) = match r {
                RawItem::Text(t) => (t, None, None, None),
                RawItem::Full {
                    text,
                    at,
                    owner,
                    due,
                } => (text, at, owner, due),
            };
            let text = text.trim().to_owned();
            if text.is_empty() {
                return None;
            }
            let clean =
                |o: Option<String>| o.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
            Some(Item {
                text,
                at: at
                    .as_ref()
                    .and_then(seconds)
                    .filter(|x| *x <= duration + 1.0),
                owner: if keep_owner { clean(owner) } else { None },
                due: if keep_owner { clean(due) } else { None },
            })
        })
        .take(40)
        .collect()
}

/// Pulls the first `{...}` object out of a reply (models sometimes wrap it).
fn json_object(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s).trim();
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => s,
    }
}

pub fn parse(
    reply: &str,
    t: &Transcript,
    model: &str,
    instructions: Option<&str>,
) -> Result<Summary, SummaryError> {
    let raw: Raw =
        serde_json::from_str(json_object(reply)).map_err(|e| SummaryError::Json(e.to_string()))?;
    let title = crate::store::clean_title(
        raw.title
            .trim_matches(|c: char| c == '"' || c == '.' || c.is_whitespace()),
    );
    let tags = raw
        .tags
        .into_iter()
        .map(|t| {
            t.trim()
                .trim_start_matches('#')
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
        })
        .filter(|t| !t.is_empty() && t.len() <= 40)
        .take(5)
        .collect();
    Ok(Summary {
        template: raw.template.filter(|s| s == "lecture" || s == "meeting"),
        definitions: items(raw.definitions, t.duration_sec, false),
        questions: items(raw.questions, t.duration_sec, false),
        title: if title.is_empty() {
            "Voice memo".into()
        } else {
            title
        },
        language: t.language.clone(),
        overview: raw.overview.trim().to_owned(),
        key_points: items(raw.key_points, t.duration_sec, false),
        decisions: items(raw.decisions, t.duration_sec, false),
        action_items: items(raw.action_items, t.duration_sec, true),
        tags,
        model: model.to_owned(),
        created_at: Utc::now(),
        instructions: instructions.map(str::to_owned),
    })
}

fn header(m: &Memo, t: &Transcript, tz: Tz) -> String {
    format!(
        "Recorded: {} ({})\nDuration: {}\nlanguage: {}\nDevice title: {}\n",
        local(m.recorded_at, tz),
        tz.name(),
        clock(t.duration_sec),
        t.language,
        m.title
    )
}

/// `lecture` or `meeting` when the memo is linked to such an event.
pub fn template(m: &Memo) -> Option<&'static str> {
    match m.event.as_ref()?.kind {
        EventKind::Lecture => Some("lecture"),
        EventKind::Meeting => Some("meeting"),
        EventKind::Other => None,
    }
}

/// What the summary prompt adds for a memo linked to a calendar event: the event, and for a
/// lecture or a meeting the template's fields; `previous` = earlier memos of the same course.
pub fn event_brief(m: &Memo, tz: Tz, previous: &[String]) -> String {
    let Some(e) = &m.event else {
        return String::new();
    };
    let ztz = m
        .timezone
        .as_deref()
        .and_then(|z| z.parse::<Tz>().ok())
        .unwrap_or(tz);
    let at = |ms: i64| {
        DateTime::from_timestamp_millis(ms)
            .map(|d| {
                d.with_timezone(&ztz)
                    .format("%a %-d %b %Y %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let mut o = format!(
        "\n\nHe recorded it during this calendar event (context only: never take content from it that is not in the transcript):\n- {} ({} – {})",
        e.title,
        at(e.start_ms),
        at(e.end_ms)
    );
    if let Some(place) = e.location.as_deref().filter(|p| !p.trim().is_empty()) {
        o.push_str(&format!("\n- place: {place}"));
    }
    if !e.attendees.is_empty() {
        o.push_str(&format!("\n- attendees: {}", e.attendees.join(", ")));
    }
    if let Some(cal) = &e.calendar {
        o.push_str(&format!("\n- calendar: {cal}"));
    }
    match e.kind {
        EventKind::Lecture => {
            o.push_str("\n\nIt is a lecture: write lecture notes. \"overview\": structured notes of the lecture (the topics in order, 4 to 10 sentences). \"keyPoints\": the key concepts. \"definitions\": terms defined or explained, \"text\" = \"term: explanation\". \"questions\": 3 to 6 questions to review the lecture with. \"actionItems\": homework, readings, deadlines. Add \"definitions\":[{\"text\":\"...\",\"at\":12}] and \"questions\":[{\"text\":\"...\"}] to the JSON.");
            if !previous.is_empty() {
                o.push_str("\nEarlier lectures of this course (connect concepts to them where the transcript does, e.g. \"as in the lecture of 20 Oct\"):\n");
                o.push_str(
                    &previous
                        .iter()
                        .map(|p| format!("- {p}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        EventKind::Meeting => {
            o.push_str("\n\nIt is a meeting: write the minutes. \"overview\": the minutes (what was discussed, in order). \"decisions\": what was agreed. \"actionItems\": every task with \"owner\" (an attendee's name, or \"me\" for Giuseppe) and \"due\" when said.");
        }
        EventKind::Other => {}
    }
    o
}

/// Asks the model for the summary. `instructions` = the owner's wishes for a re-summary.
pub async fn summarize(
    llm: &dyn ChatModel,
    model_label: &str,
    m: &Memo,
    t: &Transcript,
    tz: Tz,
    instructions: Option<&str>,
    previous: &[String],
) -> Result<Summary, SummaryError> {
    let lines = timed_lines(t);
    let joined = lines.join("\n");
    let body = if joined.chars().count() <= CHUNK_CHARS {
        format!("Transcript:\n{joined}")
    } else {
        let mut notes = Vec::new();
        let mut part = String::new();
        let mut parts: Vec<String> = Vec::new();
        for l in &lines {
            if part.chars().count() + l.chars().count() > CHUNK_CHARS && !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
            part.push_str(l);
            part.push('\n');
        }
        if !part.is_empty() {
            parts.push(part);
        }
        let n = parts.len();
        for (i, p) in parts.into_iter().enumerate() {
            let req = ChatRequest {
                messages: vec![
                    Message::system(PART_SYSTEM),
                    Message::user(format!("{}Part {} of {n}:\n{p}", header(m, t, tz), i + 1)),
                ],
                temperature: Some(0.2),
                max_tokens: Some(2000),
                purpose: "memo_summary_part",
                ..Default::default()
            };
            let r = llm.complete(req).await?;
            notes.push(format!(
                "Notes on part {} of {n}:\n{}",
                i + 1,
                r.message.text().trim()
            ));
        }
        format!("The memo is long, so here are your notes on each part instead of the full transcript.\n\n{}", notes.join("\n\n"))
    };
    let mut user = format!("{}\n{body}", header(m, t, tz));
    user.push_str(&event_brief(m, tz, previous));
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        user.push_str(&format!(
            "\n\nGiuseppe asked for this version: {}",
            extra.trim()
        ));
    }
    let mut last_err = None;
    for attempt in 0..2 {
        let req = ChatRequest {
            messages: vec![Message::system(SYSTEM), Message::user(user.clone())],
            temperature: Some(0.3),
            max_tokens: Some(4000),
            purpose: "memo_summary",
            json_object: true,
            ..Default::default()
        };
        let r = llm.complete(req).await?;
        let label = if r.model.is_empty() {
            model_label
        } else {
            &r.model
        };
        match parse(r.message.text(), t, label, instructions) {
            Ok(mut s) => {
                // The template follows the linked event, not what the model echoes back.
                s.template = template(m).map(str::to_owned);
                return Ok(s);
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "memo summary was not valid JSON");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| SummaryError::Json("no reply".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Segment;

    fn transcript() -> Transcript {
        Transcript {
            language: "it".into(),
            engine: "test".into(),
            duration_sec: 120.0,
            segments: vec![Segment {
                start: 0.0,
                end: 5.0,
                text: "Ciao".into(),
                speaker: None,
            }],
            speakers: false,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn parses_a_wrapped_reply() {
        let reply = "```json\n{\"title\":\"Preventivo cucina con Marco.\",\"overview\":\"Parli del preventivo.\",\"keyPoints\":[\"Budget 4000\",{\"text\":\"Montaggio a novembre\",\"at\":\"1:05\"}],\"actionItems\":[{\"text\":\"Chiamare Marco\",\"owner\":\"me\",\"due\":\"2026-10-27\",\"at\":95},{\"text\":\" \"}],\"tags\":[\"#Casa\",\"cucina nuova\"]}\n```";
        let s = parse(reply, &transcript(), "deepseek", None).unwrap();
        assert_eq!(s.title, "Preventivo cucina con Marco");
        assert_eq!(s.key_points.len(), 2);
        assert_eq!(s.key_points[1].at, Some(65.0));
        assert_eq!(s.action_items.len(), 1);
        assert_eq!(s.action_items[0].owner.as_deref(), Some("me"));
        assert_eq!(s.tags, vec!["casa", "cucina-nuova"]);
        assert!(s.decisions.is_empty());
    }

    #[test]
    fn drops_times_past_the_end() {
        let s = parse(
            "{\"title\":\"x y\",\"overview\":\"o\",\"keyPoints\":[{\"text\":\"a\",\"at\":999}]}",
            &transcript(),
            "m",
            None,
        )
        .unwrap();
        assert_eq!(s.key_points[0].at, None);
    }

    fn linked(kind: EventKind) -> Memo {
        let mut m = crate::obsidian::tests::memo();
        m.timezone = Some("Europe/Copenhagen".into());
        m.event = Some(crate::model::EventLink {
            id: "google:lec_1".into(),
            source: "google".into(),
            calendar: Some("DTU skema".into()),
            title: "Algorithms – Lecture 5".into(),
            start: "2026-10-27T08:00:00Z".into(),
            end: "2026-10-27T10:00:00Z".into(),
            start_ms: 1_793_088_000_000,
            end_ms: 1_793_095_200_000,
            location: Some("Building 101".into()),
            attendees: vec!["Prof. Hansen".into()],
            recurring: true,
            series: "lec".into(),
            kind,
        });
        m
    }

    #[test]
    fn lecture_and_meeting_briefs() {
        let tz = chrono_tz::Europe::Rome;
        let lecture = linked(EventKind::Lecture);
        let b = event_brief(
            &lecture,
            tz,
            &["Algorithms · 20 Oct 2026 (20 Oct): sorting".into()],
        );
        assert!(
            b.contains("Algorithms – Lecture 5 (Tue 27 Oct 2026 09:00 – Tue 27 Oct 2026 11:00)"),
            "{b}"
        );
        assert!(b.contains("place: Building 101") && b.contains("attendees: Prof. Hansen"));
        assert!(
            b.contains("lecture notes")
                && b.contains("\"definitions\"")
                && b.contains("- Algorithms · 20 Oct 2026")
        );
        assert_eq!(template(&lecture), Some("lecture"));
        let meeting = linked(EventKind::Meeting);
        let b = event_brief(&meeting, tz, &[]);
        assert!(b.contains("minutes") && !b.contains("Earlier lectures"));
        assert_eq!(template(&meeting), Some("meeting"));
        assert_eq!(template(&linked(EventKind::Other)), None);
        assert_eq!(event_brief(&crate::obsidian::tests::memo(), tz, &[]), "");
    }

    #[test]
    fn parses_lecture_fields() {
        let reply = r#"{"title":"Algoritmi di ordinamento","overview":"Appunti.","keyPoints":[{"text":"Quicksort"}],"definitions":[{"text":"Pivot: l'elemento di confronto","at":"1:05"}],"questions":[{"text":"Perché il caso peggiore è quadratico?"}],"tags":["algoritmi"]}"#;
        let s = parse(reply, &transcript(), "deepseek", None).unwrap();
        assert_eq!(s.definitions.len(), 1);
        assert_eq!(s.definitions[0].at, Some(65.0));
        assert_eq!(s.questions.len(), 1);
        // Old summaries without the new fields still load.
        let old: Summary = serde_json::from_str(r#"{"title":"t","language":"it","overview":"o","model":"m","createdAt":"2026-10-26T00:00:00Z"}"#).unwrap();
        assert!(old.definitions.is_empty() && old.template.is_none());
    }
}
