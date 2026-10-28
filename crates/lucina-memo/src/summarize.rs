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

/// Pulls the first balanced `{...}` object out of a reply (models sometimes wrap it in
/// prose or ``` fences); braces inside JSON strings don't count.
pub fn json_object(s: &str) -> &str {
    let s = s.trim();
    let Some(a) = s.find('{') else {
        return s;
    };
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for (i, c) in s[a..].char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[a..=a + i];
                }
            }
            _ => {}
        }
    }
    // Cut off: hand the parser what there is, so the error says where it ends.
    &s[a..]
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

/// Transcripts up to this length are retried whole (with [`COMPACT`]); longer ones are
/// retried from notes on parts of [`RETRY_CHUNK_CHARS`], so the final prompt is much shorter.
const SHORT_CHARS: usize = 12_000;
const RETRY_CHUNK_CHARS: usize = 10_000;

/// Added to the prompt of the second attempt, after a reply that was cut off or not JSON.
const COMPACT: &str = "Keep this version short: \"overview\" at most 3 sentences; at most 8 key points, 8 decisions and 8 action items (for a lecture at most 6 definitions and 4 questions). Answer with the JSON object only, no other text.";

/// Completion budget for the JSON summary: a base plus a share of the prompt (about a
/// token per 8 characters), clamped. Good replies use 400 to 3800 tokens; an hour-long
/// lecture's notes overran a flat 4000 (2026-10-27).
fn summary_max_tokens(prompt_chars: usize) -> u32 {
    (3000 + prompt_chars / 8).clamp(3000, 8000) as u32
}

/// The transcript itself, or (when longer than `chunk`) the model's notes on each part.
async fn body(
    llm: &dyn ChatModel,
    m: &Memo,
    t: &Transcript,
    tz: Tz,
    lines: &[String],
    chunk: usize,
    compact: bool,
) -> Result<String, SummaryError> {
    let joined = lines.join("\n");
    if joined.chars().count() <= chunk {
        return Ok(format!("Transcript:\n{joined}"));
    }
    let mut notes = Vec::new();
    let mut part = String::new();
    let mut parts: Vec<String> = Vec::new();
    for l in lines {
        if part.chars().count() + l.chars().count() > chunk && !part.is_empty() {
            parts.push(std::mem::take(&mut part));
        }
        let mut rest = l.as_str();
        while let Some((end, _)) = rest.char_indices().nth(chunk) {
            parts.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
        part.push_str(rest);
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
                Message::user(format!(
                    "{}Part {} of {n}:\n{p}{}",
                    header(m, t, tz),
                    i + 1,
                    if compact {
                        "\nKeep these notes to at most 8 short lines."
                    } else {
                        ""
                    }
                )),
            ],
            temperature: Some(0.2),
            max_tokens: Some(if compact { 500 } else { 2000 }),
            purpose: "memo_summary_part",
            thinking: Some(false),
            ..Default::default()
        };
        let r = llm.complete(req).await?;
        notes.push(format!(
            "Notes on part {} of {n}:\n{}",
            i + 1,
            r.message.text().trim()
        ));
    }
    Ok(format!("The memo is long, so here are your notes on each part instead of the full transcript.\n\n{}", notes.join("\n\n")))
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
    let chars = lines.join("\n").chars().count();
    let prompt = |body: &str, compact: bool| {
        let mut user = format!("{}\n{body}", header(m, t, tz));
        user.push_str(&event_brief(m, tz, previous));
        if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
            user.push_str(&format!(
                "\n\nGiuseppe asked for this version: {}",
                extra.trim()
            ));
        }
        if compact {
            user.push_str(&format!("\n\n{COMPACT}"));
        }
        user
    };
    let first = body(llm, m, t, tz, &lines, CHUNK_CHARS, false).await?;
    let mut last_err = None;
    for attempt in 0..2 {
        // The retry is a different, shorter request: the same one fails the same way.
        let user = if attempt == 0 {
            prompt(&first, false)
        } else if chars <= SHORT_CHARS {
            prompt(&first, true)
        } else {
            prompt(
                &body(llm, m, t, tz, &lines, RETRY_CHUNK_CHARS, true).await?,
                true,
            )
        };
        let max_tokens = summary_max_tokens(user.chars().count());
        let req = ChatRequest {
            messages: vec![Message::system(SYSTEM), Message::user(user)],
            temperature: Some(0.3),
            max_tokens: Some(max_tokens),
            purpose: "memo_summary",
            json_object: true,
            // DeepSeek defaults to thinking; the client previously omitted this flag.
            // Reasoning can exhaust max_tokens while text() reads only content, leaving
            // empty JSON input. Disable thinking and log both lengths to distinguish
            // that failure from JSON mode producing whitespace.
            thinking: Some(false),
            ..Default::default()
        };
        let r = llm.complete(req).await?;
        let label = if r.model.is_empty() {
            model_label
        } else {
            &r.model
        };
        let text = r.message.text();
        let result = if r.finish_reason.as_deref() == Some("length") {
            Err(SummaryError::Json("reply cut off at max_tokens".into()))
        } else if text.trim().is_empty() {
            Err(SummaryError::Json("empty reply".into()))
        } else {
            parse(text, t, label, instructions)
        };
        match result {
            Ok(mut s) => {
                // The template follows the linked event, not what the model echoes back.
                s.template = template(m).map(str::to_owned);
                return Ok(s);
            }
            Err(e) => {
                tracing::warn!(
                    attempt,
                    error = %e,
                    finish_reason = r.finish_reason.as_deref().unwrap_or(""),
                    completion_tokens = r.usage.as_ref().map_or(0, |u| u.completion_tokens),
                    text_chars = text.chars().count(),
                    reasoning_chars = r.message.reasoning_content.as_deref().map_or(0, |s| s.chars().count()),
                    "memo summary was not valid JSON"
                );
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
                words: vec![],
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

    /// Returns the scripted replies in order and keeps every request.
    struct Fake {
        replies: std::sync::Mutex<Vec<lucina_llm::ChatResponse>>,
        requests: std::sync::Mutex<Vec<ChatRequest>>,
    }

    impl Fake {
        fn new(replies: Vec<(&str, &str)>) -> Self {
            Self {
                replies: std::sync::Mutex::new(
                    replies
                        .into_iter()
                        .rev()
                        .map(|(text, finish)| lucina_llm::ChatResponse {
                            message: Message::assistant(text),
                            finish_reason: Some(finish.into()),
                            usage: None,
                            model: "fake".into(),
                        })
                        .collect(),
                ),
                requests: Default::default(),
            }
        }

        fn user(&self, i: usize) -> String {
            self.requests.lock().unwrap()[i].messages[1]
                .text()
                .to_owned()
        }
    }

    #[async_trait::async_trait]
    impl ChatModel for Fake {
        async fn complete(
            &self,
            request: ChatRequest,
        ) -> Result<lucina_llm::ChatResponse, LlmError> {
            self.requests.lock().unwrap().push(request);
            self.replies
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| LlmError::Decode("no more replies".into()))
        }
    }

    const GOOD: &str = r#"{"title":"Spesa per la cena","overview":"Parli della spesa.","keyPoints":[{"text":"Comprare il pane","at":2}],"tags":["spesa"]}"#;

    async fn run(llm: &Fake, t: &Transcript) -> Result<Summary, SummaryError> {
        let m = crate::obsidian::tests::memo();
        summarize(llm, "deepseek", &m, t, chrono_tz::Europe::Rome, None, &[]).await
    }

    #[tokio::test]
    async fn a_cut_off_reply_is_retried_with_a_compact_prompt() {
        let llm = Fake::new(vec![("", "length"), (GOOD, "stop")]);
        let s = run(&llm, &transcript()).await.unwrap();
        assert_eq!(s.title, "Spesa per la cena");
        assert_eq!(s.key_points.len(), 1);
        let reqs = llm.requests.lock().unwrap().len();
        assert_eq!(reqs, 2);
        assert!(!llm.user(0).contains(COMPACT) && llm.user(1).contains(COMPACT));
        assert!(llm
            .user(1)
            .contains(&format!("Transcript:\n{}", timed_lines(&transcript())[0])));
        let first = &llm.requests.lock().unwrap()[0];
        assert_eq!(first.thinking, Some(false));
        assert!(first.json_object);
    }

    #[tokio::test]
    async fn a_long_transcript_is_retried_from_part_notes() {
        let mut t = transcript();
        t.duration_sec = 3100.0;
        t.segments = (0..170)
            .map(|i| Segment {
                start: i as f64 * 10.0,
                end: i as f64 * 10.0 + 9.0,
                text: "parole ".repeat(10),
                speaker: None,
                words: vec![],
            })
            .collect();
        let llm = Fake::new(vec![
            ("   ", "stop"),
            ("note 1", "stop"),
            ("note 2", "stop"),
            (GOOD, "stop"),
        ]);
        run(&llm, &t).await.unwrap();
        assert_eq!(llm.requests.lock().unwrap().len(), 4);
        assert_eq!(llm.requests.lock().unwrap()[1].purpose, "memo_summary_part");
        let (first, last) = (llm.user(0), llm.user(3));
        assert!(last.contains("Notes on part 2 of 2:\nnote 2") && last.contains(COMPACT));
        assert!(
            last.len() * 10 < first.len(),
            "{} vs {}",
            last.len(),
            first.len()
        );
    }

    #[tokio::test]
    async fn a_fenced_reply_is_parsed_in_one_call() {
        let llm = Fake::new(vec![(
            &format!("Ecco:\n```json\n{GOOD}\n```\nFatto {{ok}}."),
            "stop",
        )]);
        let s = run(&llm, &transcript()).await.unwrap();
        assert_eq!(s.title, "Spesa per la cena");
        assert_eq!(llm.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn two_bad_replies_are_an_error() {
        let llm = Fake::new(vec![("", "length"), ("{\"title\": \"Spe", "stop")]);
        assert!(matches!(
            run(&llm, &transcript()).await,
            Err(SummaryError::Json(_))
        ));
        assert_eq!(llm.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_very_long_segment_gets_smaller_parts_on_retry() {
        let mut t = transcript();
        t.segments[0].text = "è".repeat(CHUNK_CHARS + 1);
        let llm = Fake::new(vec![
            ("first notes", "stop"),
            ("second notes", "stop"),
            (GOOD, "length"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            ("retry notes", "stop"),
            (GOOD, "stop"),
        ]);
        run(&llm, &t).await.unwrap();
        let requests = llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 11);
        assert_eq!(requests[2].purpose, "memo_summary");
        assert_eq!(requests[10].purpose, "memo_summary");
        for r in &requests[3..10] {
            assert_eq!(r.purpose, "memo_summary_part");
            assert_eq!(r.max_tokens, Some(500));
            assert!(r.messages[1].text().chars().count() < RETRY_CHUNK_CHARS + 300);
        }
    }

    #[tokio::test]
    async fn retry_preserves_event_instructions_and_previous_lectures() {
        let llm = Fake::new(vec![("invalid JSON", "stop"), (GOOD, "stop")]);
        let s = summarize(
            &llm,
            "deepseek",
            &linked(EventKind::Lecture),
            &transcript(),
            chrono_tz::Europe::Rome,
            Some("Focus on definitions"),
            &["Previous sorting lecture".into()],
        )
        .await
        .unwrap();
        assert_eq!(s.template.as_deref(), Some("lecture"));
        assert_eq!(s.instructions.as_deref(), Some("Focus on definitions"));
        for i in 0..2 {
            let user = llm.user(i);
            assert!(user.contains("Focus on definitions"));
            assert!(user.contains("Previous sorting lecture"));
            assert!(user.contains("Algorithms – Lecture 5"));
        }
    }

    #[tokio::test]
    async fn budget_grows_with_the_transcript() {
        let llm = Fake::new(vec![(GOOD, "stop"), (GOOD, "stop")]);
        let mut t = transcript();
        run(&llm, &t).await.unwrap();
        t.segments[0].text = "parole ".repeat(4000);
        run(&llm, &t).await.unwrap();
        let requests = llm.requests.lock().unwrap();
        let (short, long) = (
            requests[0].max_tokens.unwrap(),
            requests[1].max_tokens.unwrap(),
        );
        assert!(short < long, "{short} {long}");
        for b in [
            short,
            long,
            summary_max_tokens(0),
            summary_max_tokens(500_000),
        ] {
            assert!((3000..=8000).contains(&b), "{b}");
        }
    }

    #[test]
    fn finds_the_first_balanced_object() {
        assert_eq!(
            json_object("Sure: {\"a\": {\"b\": \"}\"}} and {\"c\": 1}"),
            "{\"a\": {\"b\": \"}\"}}"
        );
        assert_eq!(json_object("{\"a\": \"x"), "{\"a\": \"x");
        assert_eq!(json_object("  none "), "none");
    }
}
