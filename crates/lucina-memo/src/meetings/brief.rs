//! The brief (`brief.md`): what the meeting was about, key points by speaker, questions and
//! answers, decisions, action items with owners and dates, follow-ups. Lucina's model writes
//! it as JSON from the labelled transcript; a reply that does not validate gives a
//! deterministic brief built from the utterance kinds. Also the transcript renderings
//! (`transcript.md`, the plain text the Mac app shows).
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_llm::{ChatModel, ChatRequest, LlmError, Message};
use serde::Deserialize;

use super::model::{role_label, Brief, Kind, Meeting, Qa, Speaker, SpeakerPoints, Utterance};
use crate::model::{clock, Item};

/// Transcript characters per model call; longer meetings are noted in parts first.
pub const CHUNK_CHARS: usize = 50_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    title: String,
    about: String,
    #[serde(default)]
    key_points: Vec<RawPoints>,
    #[serde(default)]
    qa: Vec<RawQa>,
    #[serde(default)]
    decisions: Vec<RawItem>,
    #[serde(default)]
    action_items: Vec<RawItem>,
    #[serde(default)]
    follow_ups: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPoints {
    speaker: String,
    points: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQa {
    question: String,
    #[serde(default)]
    asked_by: Option<String>,
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    answered_by: Option<String>,
    #[serde(default)]
    at: Option<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    text: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due: Option<String>,
    #[serde(default)]
    at: Option<f64>,
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// Validates the model's brief: the schema, speaker ids that exist, times inside the meeting,
/// non-empty texts, bounded lengths.
pub fn validate(reply: &str, speakers: &[Speaker], duration: f64, language: &str, model: &str) -> Result<Brief, String> {
    let r: Raw = serde_json::from_str(crate::summarize::json_object(reply)).map_err(|e| format!("schema: {e}"))?;
    let known = |id: &Option<String>| id.as_deref().is_none_or(|i| speakers.iter().any(|s| s.id == i));
    let at_ok = |a: Option<f64>| a.is_none_or(|a| a.is_finite() && a >= 0.0 && a <= duration + 5.0);
    let title = crate::store::clean_title(r.title.trim_matches(|c: char| c == '"' || c == '.' || c.is_whitespace()));
    if title.is_empty() || r.about.trim().is_empty() {
        return Err("empty title or about".into());
    }
    for p in &r.key_points {
        if !speakers.iter().any(|s| s.id == p.speaker) {
            return Err(format!("key points of unknown speaker {}", p.speaker));
        }
    }
    for q in &r.qa {
        if q.question.trim().is_empty() || !known(&q.asked_by) || !known(&q.answered_by) || !at_ok(q.at) {
            return Err("a question with an unknown speaker, an empty text or a bad time".into());
        }
    }
    for i in r.decisions.iter().chain(&r.action_items) {
        if i.text.trim().is_empty() || !at_ok(i.at) {
            return Err("an item with an empty text or a bad time".into());
        }
    }
    let item = |i: RawItem| Item {
        text: clip(&i.text, 500),
        at: i.at,
        owner: i.owner.map(|o| clip(&o, 80)).filter(|o| !o.is_empty()),
        due: i.due.map(|d| clip(&d, 40)).filter(|d| !d.is_empty()),
    };
    Ok(Brief {
        title,
        language: language.to_owned(),
        about: clip(&r.about, 4000),
        key_points: r
            .key_points
            .into_iter()
            .map(|p| SpeakerPoints {
                speaker: p.speaker,
                points: p.points.iter().map(|x| clip(x, 500)).filter(|x| !x.is_empty()).take(12).collect(),
            })
            .filter(|p| !p.points.is_empty())
            .collect(),
        qa: r
            .qa
            .into_iter()
            .take(40)
            .map(|q| Qa {
                question: clip(&q.question, 500),
                asked_by: q.asked_by,
                answer: q.answer.map(|a| clip(&a, 1000)).filter(|a| !a.is_empty()),
                answered_by: q.answered_by,
                at: q.at,
            })
            .collect(),
        decisions: r.decisions.into_iter().take(40).map(item).collect(),
        action_items: r.action_items.into_iter().take(40).map(item).collect(),
        follow_ups: r.follow_ups.iter().map(|f| clip(f, 500)).filter(|f| !f.is_empty()).take(20).collect(),
        tags: r
            .tags
            .iter()
            .map(|t| t.trim().trim_start_matches('#').to_lowercase().split_whitespace().collect::<Vec<_>>().join("-"))
            .filter(|t| !t.is_empty() && t.len() <= 40)
            .take(5)
            .collect(),
        source: "llm".into(),
        model: model.to_owned(),
        created_at: Utc::now(),
    })
}

/// The deterministic brief from the utterance kinds.
pub fn fallback(m: &Meeting, utts: &[Utterance], speakers: &[Speaker], language: &str) -> Brief {
    let mut key_points = Vec::new();
    for s in speakers {
        let points: Vec<String> = utts
            .iter()
            .filter(|u| u.speaker == s.id && u.kind == Kind::Statement && u.text.split_whitespace().count() >= 8)
            .take(3)
            .map(|u| clip(&u.text, 300))
            .collect();
        if !points.is_empty() {
            key_points.push(SpeakerPoints {
                speaker: s.id.clone(),
                points,
            });
        }
    }
    let mut qa = Vec::new();
    for (i, u) in utts.iter().enumerate().filter(|(_, u)| u.kind == Kind::Question) {
        let answer = utts[i + 1..].iter().take(3).find(|a| a.speaker != u.speaker && a.kind == Kind::Answer);
        qa.push(Qa {
            question: clip(&u.text, 500),
            asked_by: Some(u.speaker.clone()),
            answer: answer.map(|a| clip(&a.text, 1000)),
            answered_by: answer.map(|a| a.speaker.clone()),
            at: Some(u.start),
        });
    }
    let items = |k: Kind, owner: bool| -> Vec<Item> {
        utts.iter()
            .filter(|u| u.kind == k)
            .map(|u| Item {
                text: clip(&u.text, 500),
                at: Some(u.start),
                owner: owner.then(|| u.speaker.clone()),
                due: None,
            })
            .collect()
    };
    let names: Vec<String> = speakers.iter().map(Speaker::display).collect();
    Brief {
        title: m.title.clone(),
        language: language.to_owned(),
        about: format!(
            "{} ({}), {} speaking: {}. Automatic brief (Lucina's model was not available or its answer did not validate).",
            m.title,
            clock(m.duration_sec),
            names.len(),
            names.join(", ")
        ),
        key_points,
        qa: qa.into_iter().take(40).collect(),
        decisions: items(Kind::Decision, false),
        action_items: items(Kind::ActionItem, true),
        follow_ups: Vec::new(),
        tags: Vec::new(),
        source: "fallback".into(),
        model: "rules".into(),
        created_at: Utc::now(),
    }
}

const SYSTEM: &str = r#"You are Lucina, Giuseppe's personal assistant. You get the diarized transcript of a meeting or lecture he recorded (speaker "me" is Giuseppe), with each speaker's role and each line's kind. Write his brief.

Rules:
- Write in the meeting's language (field "language"). Be faithful: only what was said. Names and technical words may be misheard; fix obvious ones from context, never invent facts, people, dates or numbers.
- Refer to speakers by their id ("me", "S1"...) in "speaker", "asked_by", "answered_by" and "owner".
- "title": 3 to 8 words. "about": 2 to 6 sentences: what it was about and why it matters to Giuseppe.
- "key_points": per speaker, their main points (one sentence each).
- "qa": the questions asked and the answers given (null when unanswered), with "at" = the start second.
- "decisions": what was decided or agreed. "action_items": who does what ("owner" = speaker id or a name), "due" = YYYY-MM-DD when a date is said or clear from the meeting date, otherwise the words used.
- "follow_ups": open threads worth following up. "tags": 1 to 5 lowercase topic words.

Answer with one JSON object only, exactly these keys:
{"title":"...","about":"...","key_points":[{"speaker":"S1","points":["..."]}],"qa":[{"question":"...","asked_by":"me","answer":"...","answered_by":"S1","at":120}],"decisions":[{"text":"...","at":300}],"action_items":[{"text":"...","owner":"me","due":"2026-10-30","at":400}],"follow_ups":["..."],"tags":["..."]}"#;

const PART_SYSTEM: &str = "You are Lucina. This is one part of a long diarized meeting transcript (\"me\" is Giuseppe). Write compact notes of this part: what each speaker said (by id), questions and answers, decisions, things to do (who, when), with the [mm:ss] mark of each. Keep names, numbers and dates exactly. Use the meeting's language. Plain text, at most 50 lines.";

pub fn lines(utts: &[Utterance]) -> Vec<String> {
    utts.iter()
        .map(|u| {
            let kind = if u.kind == Kind::Statement { String::new() } else { format!(" <{}>", u.kind.as_str()) };
            format!("[{}] {}{kind}: {}", clock(u.start), u.speaker, u.text)
        })
        .collect()
}

fn header(m: &Meeting, speakers: &[Speaker], language: &str, tz: Tz) -> String {
    let mut o = super::analyze::event_context(m);
    o.push_str(&format!(
        "Date: {}\nDuration: {}\nlanguage: {language}\nSpeakers:\n",
        m.started_at.with_timezone(&tz).format("%A %Y-%m-%d %H:%M"),
        clock(m.duration_sec)
    ));
    for s in speakers {
        o.push_str(&format!(
            "- {}: {}{} ({} of speech)\n",
            s.id,
            s.display(),
            s.role.as_deref().map(|r| format!(", {}", role_label(r))).unwrap_or_default(),
            clock(s.talk_sec)
        ));
    }
    o
}

/// Asks the model; `Ok(None)` when its reply did not validate (use [`fallback`]).
pub async fn write(
    llm: &dyn ChatModel,
    model_label: &str,
    m: &Meeting,
    utts: &[Utterance],
    speakers: &[Speaker],
    language: &str,
    tz: Tz,
) -> Result<Result<Brief, String>, LlmError> {
    let head = header(m, speakers, language, tz);
    let all = lines(utts);
    let joined = all.join("\n");
    let body = if joined.chars().count() <= CHUNK_CHARS {
        format!("Transcript:\n{joined}")
    } else {
        let mut parts: Vec<String> = Vec::new();
        let mut part = String::new();
        for l in &all {
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
        let mut notes = Vec::new();
        for (i, p) in parts.into_iter().enumerate() {
            let r = llm
                .complete(ChatRequest {
                    messages: vec![Message::system(PART_SYSTEM), Message::user(format!("{head}Part {} of {n}:\n{p}", i + 1))],
                    temperature: Some(0.2),
                    max_tokens: Some(3000),
                    purpose: "meeting_brief_part",
                    ..Default::default()
                })
                .await?;
            notes.push(format!("Notes on part {} of {n}:\n{}", i + 1, r.message.text()));
        }
        notes.join("\n\n")
    };
    let r = llm
        .complete(ChatRequest {
            messages: vec![Message::system(SYSTEM), Message::user(format!("{head}\n{body}"))],
            temperature: Some(0.2),
            max_tokens: Some(8000),
            purpose: "meeting_brief",
            json_object: true,
            ..Default::default()
        })
        .await?;
    Ok(validate(r.message.text(), speakers, m.duration_sec, language, model_label))
}

/// A speaker id (or a free name) as shown.
fn who(id: &str, names: &HashMap<String, String>) -> String {
    names.get(id).cloned().unwrap_or_else(|| id.to_owned())
}

pub fn names(speakers: &[Speaker]) -> HashMap<String, String> {
    speakers.iter().map(|s| (s.id.clone(), s.display())).collect()
}

fn local(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%a %-d %b %Y, %H:%M").to_string()
}

/// `brief.md`.
pub fn markdown(m: &Meeting, b: &Brief, speakers: &[Speaker], tz: Tz, transcript_url: &str) -> String {
    let n = names(speakers);
    let mut o = format!("# {}\n\n", b.title);
    let mut meta = vec![local(m.started_at, tz), clock(m.duration_sec)];
    if let Some(app) = &m.source_app.name {
        meta.push(app.clone());
    }
    if let Some(e) = &m.event {
        meta.push(format!("📅 {}", e.title));
    }
    o.push_str(&format!("{}\n\n## What it was about\n\n{}\n\n## Who spoke\n\n", meta.join(" · "), b.about));
    for s in speakers {
        o.push_str(&format!(
            "- **{}**{} — {}{}\n",
            s.display(),
            s.role.as_deref().map(|r| format!(" ({})", role_label(r))).unwrap_or_default(),
            clock(s.talk_sec),
            if s.name.is_none() && s.id != "me" {
                s.suggested_name
                    .as_deref()
                    .map(|x| format!(", maybe {x}? (name them in the app)"))
                    .unwrap_or_else(|| ", unnamed (name them in the app)".into())
            } else {
                String::new()
            }
        ));
    }
    if !b.key_points.is_empty() {
        o.push_str("\n## Key points by speaker\n");
        for p in &b.key_points {
            o.push_str(&format!("\n### {}\n\n", who(&p.speaker, &n)));
            for x in &p.points {
                o.push_str(&format!("- {x}\n"));
            }
        }
    }
    if !b.qa.is_empty() {
        o.push_str("\n## Questions and answers\n\n");
        for q in &b.qa {
            let at = q.at.map(|a| format!(" [{}]", clock(a))).unwrap_or_default();
            let by = q.asked_by.as_deref().map(|x| format!(" ({})", who(x, &n))).unwrap_or_default();
            o.push_str(&format!("- **Q**{by}{at}: {}\n", q.question));
            match &q.answer {
                Some(a) => {
                    let by = q.answered_by.as_deref().map(|x| format!(" ({})", who(x, &n))).unwrap_or_default();
                    o.push_str(&format!("  **A**{by}: {a}\n"));
                }
                None => o.push_str("  *(no answer)*\n"),
            }
        }
    }
    if !b.decisions.is_empty() {
        o.push_str("\n## Decisions\n\n");
        for d in &b.decisions {
            o.push_str(&format!("- {}{}\n", d.text, d.at.map(|a| format!(" [{}]", clock(a))).unwrap_or_default()));
        }
    }
    if !b.action_items.is_empty() {
        o.push_str("\n## Action items\n\n");
        for i in &b.action_items {
            let mut meta = Vec::new();
            if let Some(w) = &i.owner {
                meta.push(who(w, &n));
            }
            if let Some(d) = &i.due {
                meta.push(format!("due {d}"));
            }
            if let Some(a) = i.at {
                meta.push(clock(a));
            }
            let meta = if meta.is_empty() { String::new() } else { format!(" — {}", meta.join(", ")) };
            o.push_str(&format!("- [ ] {}{meta}\n", i.text));
        }
    }
    if !b.follow_ups.is_empty() {
        o.push_str("\n## Follow-ups\n\n");
        for f in &b.follow_ups {
            o.push_str(&format!("- {f}\n"));
        }
    }
    o.push_str(&format!("\n---\nTranscript: {transcript_url}\n"));
    o
}

/// The plain transcript the Mac app shows: `Professor: …` lines.
pub fn plain(utts: &[Utterance]) -> String {
    utts.iter()
        .map(|u| format!("[{}] {}: {}", clock(u.start), u.speaker_name, u.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `transcript.md`.
pub fn transcript_md(m: &Meeting, utts: &[Utterance], tz: Tz) -> String {
    let mut o = format!("# {} — transcript\n\n{} · {}\n\n", m.title, local(m.started_at, tz), clock(m.duration_sec));
    for u in utts {
        let role = u.role.as_deref().map(|r| format!(" ({})", role_label(r))).unwrap_or_default();
        let kind = if u.kind == Kind::Statement { String::new() } else { format!(" *{}*", u.kind.as_str().replace('_', " ")) };
        o.push_str(&format!("**[{}] {}**{role}{kind}: {}\n\n", clock(u.start), u.speaker_name, u.text));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(id: &str, name: Option<&str>, role: &str) -> Speaker {
        Speaker {
            id: id.into(),
            label: format!("Speaker {id}"),
            name: name.map(str::to_owned),
            name_source: None,
            suggested_name: None,
            role: Some(role.into()),
            track: "system".into(),
            profile_id: None,
            voice_id: None,
            similarity: None,
            talk_sec: 60.0,
            utterances: 3,
        }
    }

    #[test]
    fn validates_the_brief() {
        let speakers = [sp("me", Some("Giuseppe"), "student"), sp("S1", None, "professor")];
        let ok = r#"{"title":"Heaps lecture","about":"Binary heaps.","key_points":[{"speaker":"S1","points":["Heaps are trees."]}],
          "qa":[{"question":"Open book?","asked_by":"me","answer":"Yes.","answered_by":"S1","at":30}],
          "decisions":[],"action_items":[{"text":"Read chapter 6","owner":"me","due":"2026-11-02","at":50}],"follow_ups":["Ask about the project"],"tags":["Algorithms"]}"#;
        let b = validate(ok, &speakers, 100.0, "en", "m").unwrap();
        assert_eq!(b.tags, ["algorithms"]);
        let md = markdown(&dummy(), &b, &speakers, chrono_tz::UTC, "https://x/t");
        assert!(md.contains("**Q** (Giuseppe) [0:30]: Open book?"));
        assert!(md.contains("**A** (Professor): Yes."));
        assert!(md.contains("- [ ] Read chapter 6 — Giuseppe, due 2026-11-02, 0:50"));
        assert!(md.contains("### Professor"));
        for bad in [
            r#"{"title":"x","about":"y","key_points":[{"speaker":"S7","points":["a"]}]}"#,
            r#"{"title":"x","about":"y","qa":[{"question":"q","at":1000}]}"#,
            r#"{"title":"","about":"y"}"#,
            r#"{"title":"x","about":"y","oops":1}"#,
        ] {
            assert!(validate(bad, &speakers, 100.0, "en", "m").is_err(), "{bad}");
        }
    }

    fn dummy() -> Meeting {
        let mut m: Meeting = serde_json::from_value(serde_json::json!({
            "id":"mtg_0123456789abcdef","title":"Algorithms","started_at":"2026-10-27T08:00:00Z","tracks":[],
            "folder":"2026/x","status":"done","created_at":"2026-10-27T08:00:00Z","updated_at":"2026-10-27T08:00:00Z"}))
        .unwrap();
        m.duration_sec = 100.0;
        m
    }

    #[test]
    fn fallback_brief_pairs_questions_and_answers() {
        let speakers = [sp("me", Some("Giuseppe"), "student"), sp("S1", None, "lecturer")];
        let mk = |speaker: &str, kind, text: &str, start| Utterance {
            start,
            end: start + 1.0,
            speaker: speaker.into(),
            speaker_name: speaker.into(),
            role: None,
            kind,
            text: text.into(),
            track: "system".into(),
            words: vec![],
        };
        let utts = [
            mk("S1", Kind::Statement, "Today we look at binary heaps and their array layout in detail.", 0.0),
            mk("me", Kind::Question, "Is the exam open book?", 10.0),
            mk("S1", Kind::Answer, "Yes, notes allowed.", 12.0),
            mk("S1", Kind::ActionItem, "Read chapter six for Monday.", 20.0),
        ];
        let b = fallback(&dummy(), &utts, &speakers, "en");
        assert_eq!(b.source, "fallback");
        assert_eq!(b.qa[0].answered_by.as_deref(), Some("S1"));
        assert_eq!(b.action_items[0].owner.as_deref(), Some("S1"));
        assert_eq!(b.key_points[0].speaker, "S1");
        assert_eq!(plain(&utts[1..2]), "[0:10] me: Is the exam open book?");
    }
}
