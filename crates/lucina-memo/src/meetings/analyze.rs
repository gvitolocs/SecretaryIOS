//! Speaker roles and utterance kinds: an LLM pass (Lucina's DeepSeek model, text only) over the
//! diarized transcript and the calendar event, answering in a strict JSON schema:
//!
//! ```json
//! {"speakers":[{"id":"S1","role":"lecturer","name":null}],
//!  "utterances":[{"i":12,"kind":"question"}]}
//! ```
//!
//! Only utterances that are not plain statements are listed. A reply that is not exactly this
//! (unknown keys, ids, roles or kinds, indices out of range, a name not said in the meeting or
//! the invitation) is rejected as a whole and the deterministic [`fallback`] is used instead.
use std::collections::HashMap;

use lucina_llm::{ChatModel, ChatRequest, LlmError, Message};
use serde::Deserialize;

use super::model::{Kind, Meeting, Speaker, Utterance, ROLES};
use crate::model::{clock, EventKind};
use crate::search::fold;

/// Utterances per model call.
pub const CHUNK: usize = 300;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Analysis {
    pub roles: HashMap<String, String>,
    /// Names the model heard ("I'm Anna"), checked against the text.
    pub suggested: HashMap<String, String>,
    pub kinds: Vec<Kind>,
    /// `llm` or `fallback`.
    pub source: String,
    /// Why the model's answer was not used.
    pub rejected: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    speakers: Vec<ReplySpeaker>,
    utterances: Vec<ReplyUtterance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplySpeaker {
    id: String,
    role: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyUtterance {
    i: usize,
    kind: String,
}

const QUESTION_START: &[&str] = &[
    "what", "why", "how", "when", "where", "who", "which", "can", "could", "do", "does", "is", "are", "should",
    "perche", "come", "quando", "dove", "chi", "cosa", "hvad", "hvorfor", "hvordan", "hvornar", "hvem", "kan",
];
const DECISION: &[&str] = &[
    "we decided", "we agreed", "let's go with", "lets go with", "decision is", "we will go with", "agreed",
    "abbiamo deciso", "deciso", "d accordo", "vi har besluttet", "besluttet", "aftalt",
];
const ACTION: &[&str] = &[
    "i will", "i ll", "you will", "will send", "please send", "can you send", "by friday", "by monday",
    "deadline", "homework", "todo", "to do", "follow up", "faro", "mandami", "entro", "scadenza", "compito",
    "jeg sender", "jeg vil", "inden", "afleveres", "lektie",
];

fn contains_phrase(text: &str, phrases: &[&str]) -> bool {
    let t = format!(" {} ", fold(text).split_whitespace().collect::<Vec<_>>().join(" "));
    phrases.iter().any(|p| t.contains(&format!(" {p} ")))
}

/// The deterministic kinds: a `?` (or a question word first) is a question, the next line by
/// someone else answers it; decision and action phrases (English, Italian, Danish); the rest
/// are statements.
pub fn fallback_kinds(utts: &[Utterance]) -> Vec<Kind> {
    let mut out = Vec::with_capacity(utts.len());
    let mut open_question: Option<&str> = None;
    for u in utts {
        let text = u.text.trim();
        let first = fold(text).split_whitespace().next().unwrap_or_default().to_owned();
        let kind = if text.ends_with('?') || (QUESTION_START.contains(&first.as_str()) && text.contains('?')) {
            Kind::Question
        } else if contains_phrase(text, DECISION) {
            Kind::Decision
        } else if contains_phrase(text, ACTION) {
            Kind::ActionItem
        } else if open_question.is_some_and(|asker| asker != u.speaker) {
            Kind::Answer
        } else {
            Kind::Statement
        };
        open_question = (kind == Kind::Question).then_some(u.speaker.as_str());
        out.push(kind);
    }
    out
}

/// The deterministic roles: in a lecture the other speaker who talks most is the lecturer and
/// everyone else (the owner included) a student; otherwise the owner is `owner` and the others
/// participants.
pub fn fallback_roles(speakers: &[Speaker], kind: Option<EventKind>) -> HashMap<String, String> {
    let lecture = kind == Some(EventKind::Lecture);
    let lecturer = speakers
        .iter()
        .filter(|s| s.id != "me")
        .max_by(|a, b| a.talk_sec.total_cmp(&b.talk_sec))
        .map(|s| s.id.clone());
    speakers
        .iter()
        .map(|s| {
            let role = match (lecture, s.id == "me") {
                (true, _) if Some(&s.id) == lecturer.as_ref() => "lecturer",
                (true, _) => "student",
                (false, true) => "owner",
                (false, false) => "participant",
            };
            (s.id.clone(), role.to_owned())
        })
        .collect()
}

pub fn fallback(utts: &[Utterance], speakers: &[Speaker], kind: Option<EventKind>, why: Option<String>) -> Analysis {
    Analysis {
        roles: fallback_roles(speakers, kind),
        suggested: HashMap::new(),
        kinds: fallback_kinds(utts),
        source: "fallback".into(),
        rejected: why,
    }
}

/// A validated chunk: roles and suggested names by speaker id, kinds by utterance index.
pub type Checked = (HashMap<String, String>, HashMap<String, String>, Vec<(usize, Kind)>);

/// Checks one chunk's reply. `offset`: index of the chunk's first utterance; `allowed_names`:
/// folded text of the transcript and the invitation (a suggested name must occur in it).
pub fn validate(
    reply: &str,
    offset: usize,
    len: usize,
    speaker_ids: &[&str],
    allowed_names: &str,
) -> Result<Checked, String> {
    let r: Reply = serde_json::from_str(crate::summarize::json_object(reply)).map_err(|e| format!("schema: {e}"))?;
    let mut roles = HashMap::new();
    let mut names = HashMap::new();
    for s in r.speakers {
        if !speaker_ids.contains(&s.id.as_str()) {
            return Err(format!("unknown speaker {}", s.id));
        }
        if !ROLES.contains(&s.role.as_str()) {
            return Err(format!("unknown role {}", s.role));
        }
        if let Some(n) = s.name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()) {
            let f = fold(&n).split_whitespace().collect::<Vec<_>>().join(" ");
            if n.chars().count() > 60 || f.is_empty() || !allowed_names.contains(&format!(" {f} ")) {
                return Err(format!("name {n:?} is not in the meeting"));
            }
            names.insert(s.id.clone(), n);
        }
        roles.insert(s.id, s.role);
    }
    let mut kinds = Vec::new();
    for u in r.utterances {
        if u.i < offset || u.i >= offset + len {
            return Err(format!("utterance {} out of range", u.i));
        }
        kinds.push((u.i, Kind::parse(&u.kind).ok_or_else(|| format!("unknown kind {}", u.kind))?));
    }
    Ok((roles, names, kinds))
}

const SYSTEM: &str = r##"You label a diarized meeting transcript for Giuseppe's assistant Lucina. Each line is "#<index> [mm:ss] <speaker id> (<label>): text". The speaker "me" is Giuseppe himself (his microphone). Speaker labels come from voice clustering and may be imperfect.

Return ONE JSON object, nothing else, exactly this shape:
{"speakers":[{"id":"S1","role":"lecturer","name":null}],"utterances":[{"i":12,"kind":"question"}]}

- "speakers": every speaker id that appears, with "role" one of: owner, professor, lecturer, teaching_assistant, student, client, colleague, manager, interviewer, interviewee, candidate, presenter, host, participant, guest. Use the calendar event as context (a university lecture: lecturer/professor and students; a job interview: interviewer and candidate). Giuseppe ("me") gets his own role in this meeting (e.g. student in a lecture), or "owner" when nothing fits better.
- "name": only when the speaker's name is said in the transcript (an introduction, someone addressing them) or is clearly the invitation's only other attendee; otherwise null. Never guess.
- "utterances": ONLY the lines that are not plain statements, each with "i" (the #index) and "kind": question, answer (replies to a question), decision (something agreed or decided), action_item (someone has to do something). Omit statements.
- No other keys."##;

/// Event context for the prompt.
pub fn event_context(m: &Meeting) -> String {
    let mut o = format!("Meeting title: {}\n", m.title);
    if let Some(app) = &m.source_app.name {
        o.push_str(&format!("Recorded from: {app}\n"));
    }
    if let Some(e) = &m.event {
        o.push_str(&format!("Calendar event: {} ({:?})\n", e.title, e.kind));
        if !e.attendees.is_empty() {
            o.push_str(&format!("Invited: {}\n", e.attendees.join(", ")));
        }
        if let Some(c) = &e.calendar {
            o.push_str(&format!("Calendar: {c}\n"));
        }
    }
    if let Some(c) = &m.company {
        o.push_str(&format!("Company: {c}\n"));
    }
    o
}

pub fn line(i: usize, u: &Utterance) -> String {
    format!("#{i} [{}] {} ({}): {}", clock(u.start), u.speaker, u.speaker_name, u.text)
}

/// Runs the pass. A model that cannot be reached is an error (the step is retried); a reply
/// that fails validation gives the fallback.
pub async fn analyze(
    llm: &dyn ChatModel,
    m: &Meeting,
    utts: &[Utterance],
    speakers: &[Speaker],
) -> Result<Analysis, LlmError> {
    let kind = m.event.as_ref().map(|e| e.kind);
    if utts.is_empty() {
        return Ok(fallback(utts, speakers, kind, None));
    }
    let ids: Vec<&str> = speakers.iter().map(|s| s.id.as_str()).collect();
    let mut allowed = String::from(" ");
    for u in utts {
        allowed.push_str(&fold(&u.text).split_whitespace().collect::<Vec<_>>().join(" "));
        allowed.push(' ');
    }
    for a in m.event.iter().flat_map(|e| e.attendees.iter()) {
        allowed.push_str(&fold(a).split_whitespace().collect::<Vec<_>>().join(" "));
        allowed.push(' ');
    }
    let context = event_context(m);
    let mut roles: HashMap<String, String> = HashMap::new();
    let mut suggested = HashMap::new();
    let mut kinds = vec![Kind::Statement; utts.len()];
    for (c, chunk) in utts.chunks(CHUNK).enumerate() {
        let offset = c * CHUNK;
        let lines: Vec<String> = chunk.iter().enumerate().map(|(k, u)| line(offset + k, u)).collect();
        let req = ChatRequest {
            messages: vec![
                Message::system(SYSTEM),
                Message::user(format!("{context}\nTranscript lines #{offset}–#{}:\n{}", offset + chunk.len() - 1, lines.join("\n"))),
            ],
            temperature: Some(0.0),
            max_tokens: Some(4000),
            purpose: "meeting_analysis",
            json_object: true,
            ..Default::default()
        };
        let reply = llm.complete(req).await?;
        match validate(reply.message.text(), offset, chunk.len(), &ids, &allowed) {
            Ok((r, n, k)) => {
                for (id, role) in r {
                    roles.entry(id).or_insert(role);
                }
                for (id, name) in n {
                    suggested.entry(id).or_insert(name);
                }
                for (i, kind) in k {
                    kinds[i] = kind;
                }
            }
            Err(why) => {
                tracing::warn!(meeting = %m.id, %why, "analysis reply rejected; deterministic fallback");
                return Ok(fallback(utts, speakers, kind, Some(why)));
            }
        }
    }
    // Speakers the model left out get the fallback role.
    for (id, role) in fallback_roles(speakers, kind) {
        roles.entry(id).or_insert(role);
    }
    Ok(Analysis {
        roles,
        suggested,
        kinds,
        source: "llm".into(),
        rejected: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(speaker: &str, text: &str) -> Utterance {
        Utterance {
            start: 0.0,
            end: 1.0,
            speaker: speaker.into(),
            speaker_name: speaker.into(),
            role: None,
            kind: Kind::Statement,
            text: text.into(),
            track: "system".into(),
            words: vec![],
        }
    }

    fn sp(id: &str, talk: f64) -> Speaker {
        Speaker {
            id: id.into(),
            label: id.into(),
            name: None,
            name_source: None,
            suggested_name: None,
            role: None,
            track: "system".into(),
            profile_id: None,
            voice_id: None,
            similarity: None,
            talk_sec: talk,
            utterances: 1,
        }
    }

    #[test]
    fn deterministic_kinds_and_roles() {
        let utts = [
            u("S1", "Today we look at heaps."),
            u("me", "Is the exam open book?"),
            u("S1", "Yes, you can bring notes."),
            u("S1", "We agreed to move the deadline."),
            u("S2", "I will send the slides by Friday."),
        ];
        assert_eq!(
            fallback_kinds(&utts),
            [Kind::Statement, Kind::Question, Kind::Answer, Kind::Decision, Kind::ActionItem]
        );
        let speakers = [sp("me", 10.0), sp("S1", 300.0), sp("S2", 20.0)];
        let r = fallback_roles(&speakers, Some(EventKind::Lecture));
        assert_eq!((r["S1"].as_str(), r["S2"].as_str(), r["me"].as_str()), ("lecturer", "student", "student"));
        let r = fallback_roles(&speakers, None);
        assert_eq!((r["me"].as_str(), r["S1"].as_str()), ("owner", "participant"));
    }

    #[test]
    fn strict_validation() {
        let ids = ["me", "S1"];
        let names = " hi i m anna welcome ";
        let ok = r#"```json
{"speakers":[{"id":"S1","role":"lecturer","name":"Anna"},{"id":"me","role":"student"}],"utterances":[{"i":11,"kind":"question"}]}
```"#;
        let (roles, n, k) = validate(ok, 10, 5, &ids, names).unwrap();
        assert_eq!(roles["S1"], "lecturer");
        assert_eq!(n["S1"], "Anna");
        assert_eq!(k, vec![(11, Kind::Question)]);
        for bad in [
            r#"{"speakers":[{"id":"S9","role":"lecturer"}],"utterances":[]}"#,
            r#"{"speakers":[{"id":"S1","role":"wizard"}],"utterances":[]}"#,
            r#"{"speakers":[],"utterances":[{"i":15,"kind":"question"}]}"#,
            r#"{"speakers":[],"utterances":[{"i":11,"kind":"joke"}]}"#,
            r#"{"speakers":[{"id":"S1","role":"lecturer","name":"Bob"}],"utterances":[]}"#,
            r#"{"speakers":[],"utterances":[],"extra":1}"#,
            "not json",
        ] {
            assert!(validate(bad, 10, 5, &ids, names).is_err(), "{bad}");
        }
    }
}
