//! Writing the message. DeepSeek (`lucina-llm`, `deepseek-flash`) is called
//! only here, for candidates that already passed the gate, with Lucina's
//! persona and only the event's facts; templates when it is off, has no
//! key or fails. Also the "resolved / still open" judge for open threads.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::lang;
use crate::model::{Kind, Priority};
use crate::sources::threads::{Judge, Judgement};
use crate::text;

pub const PERSONA_DEFAULT: &str = include_str!("../../lucina-server/assets/lucina-persona.md");

#[derive(Debug, Clone)]
pub struct ComposeRequest {
    pub kind: Kind,
    pub language: String,
    pub priority: Priority,
    /// The event's subject and payload (or the Pulse's sections).
    pub facts: Value,
    /// Local date and time, e.g. "giovedì 22/10 13:05" (for "in an hour").
    pub local_time: String,
    /// Meeting prep: also write the prep summary.
    pub want_prep: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Composed {
    pub message: String,
    /// Markdown prep summary (meeting prep), saved as a note.
    pub prep: Option<String>,
    /// `llm` or `template`.
    pub via: &'static str,
}

#[async_trait::async_trait]
pub trait Composer: Send + Sync {
    async fn compose(&self, req: &ComposeRequest) -> Composed;
}

fn lang_name(l: &str) -> &'static str {
    match l {
        "en" => "English",
        "da" => "Danish",
        _ => "Italian",
    }
}

// ------------------------------------------------------------------ templates

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn bullets(items: &Value) -> Vec<String> {
    items
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("line").and_then(Value::as_str))
                .map(|l| format!("• {l}"))
                .collect()
        })
        .unwrap_or_default()
}

/// The fallback text (no model).
pub fn template(req: &ComposeRequest) -> Composed {
    let f = &req.facts;
    let l = req.language.as_str();
    let pick = |it: String, en: String, da: String| match l {
        "en" => en,
        "da" => da,
        _ => it,
    };
    let title = s(f, "/event/title");
    let mins = f
        .pointer("/event/startsInMinutes")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let message = match req.kind {
        Kind::MeetingPrep => {
            let notes = f.get("notes").and_then(Value::as_array).map_or(0, Vec::len);
            if notes > 0 {
                pick(
                    format!("Tra {mins} minuti hai «{title}». Ti ho preparato un riepilogo da {notes} nota/e: lo trovi nelle note."),
                    format!("\"{title}\" starts in {mins} minutes. I put together a short prep from {notes} note(s); it's in your notes."),
                    format!("\"{title}\" starter om {mins} minutter. Jeg har samlet et kort overblik fra {notes} note(r) i dine noter."),
                )
            } else {
                pick(
                    format!("Tra {mins} minuti hai «{title}»."),
                    format!("\"{title}\" starts in {mins} minutes."),
                    format!("\"{title}\" starter om {mins} minutter."),
                )
            }
        }
        Kind::ReminderOverdue => {
            let t = s(f, "/reminder/text");
            pick(
                format!("Questo promemoria doveva arrivarti e non è partito: «{t}»."),
                format!("This reminder should have reached you and didn't go out: \"{t}\"."),
                format!("Denne påmindelse skulle være kommet frem, men blev ikke sendt: \"{t}\"."),
            )
        }
        Kind::DueNoPlan => {
            let t = s(f, "/reminder/text");
            pick(
                format!("In scadenza a breve, e non vedo niente in calendario per farlo: «{t}»."),
                format!("Due soon, and I don't see time blocked for it: \"{t}\"."),
                format!("Snart deadline, og jeg kan ikke se tid sat af til det: \"{t}\"."),
            )
        }
        Kind::NoteForEvent => pick(
            format!(
                "Per «{}» hai la nota fissata «{}».",
                s(f, "/event/title"),
                s(f, "/note/title")
            ),
            format!(
                "For \"{}\" you have the pinned note \"{}\".",
                s(f, "/event/title"),
                s(f, "/note/title")
            ),
            format!(
                "Til \"{}\" har du den fastgjorte note \"{}\".",
                s(f, "/event/title"),
                s(f, "/note/title")
            ),
        ),
        Kind::ThreadResolved => {
            let sum = s(f, "/summary");
            pick(
                format!("Quella cosa che seguivamo sembra risolta. {sum}"),
                format!("Something we were following looks solved. {sum}"),
                format!("Noget, vi holdt øje med, ser ud til at være løst. {sum}"),
            )
        }
        Kind::ThreadOpen => {
            let sum = s(f, "/summary");
            pick(
                format!("Ancora aperto: {sum}"),
                format!("Still open: {sum}"),
                format!("Stadig åbent: {sum}"),
            )
        }
        Kind::ServiceDown => {
            let (svc, m) = (
                s(f, "/service"),
                f.get("minutesDown").and_then(Value::as_i64).unwrap_or(0),
            );
            pick(
                format!("{svc} non risponde da circa {m} minuti (controlli ripetuti, non un singolo errore)."),
                format!("{svc} has not been answering for about {m} minutes (repeated checks, not a blip)."),
                format!("{svc} har ikke svaret i cirka {m} minutter (gentagne tjek, ikke et enkelt udfald)."),
            )
        }
        Kind::ServiceRecovered => {
            let svc = s(f, "/service");
            pick(
                format!("{svc} è tornato su."),
                format!("{svc} is back up."),
                format!("{svc} kører igen."),
            )
        }
        Kind::Pulse => {
            let [h1, h2, h3] = lang::pulse_headers(l);
            let mut parts = vec![pick(
                "Buongiorno. Il punto di oggi:".into(),
                "Good morning. Today in short:".into(),
                "Godmorgen. Dagen kort:".into(),
            )];
            for (h, key) in [(h1, "important"), (h2, "completed"), (h3, "watch")] {
                let b = bullets(&f[key]);
                if !b.is_empty() {
                    parts.push(format!("{h}\n{}", b.join("\n")));
                }
            }
            parts.join("\n\n")
        }
    };
    let prep = (req.want_prep && req.kind == Kind::MeetingPrep).then(|| prep_template(req));
    Composed {
        message: text::clip(&message, 1200),
        prep,
        via: "template",
    }
}

/// A prep note without a model: the material, as is.
pub fn prep_template(req: &ComposeRequest) -> String {
    let f = &req.facts;
    let mut out = format!("# Prep: {}\n\n", s(f, "/event/title"));
    out.push_str(&format!("- When: {}\n", s(f, "/event/start")));
    let loc = s(f, "/event/location");
    if !loc.is_empty() {
        out.push_str(&format!("- Where: {loc}\n"));
    }
    if let Some(notes) = f.get("notes").and_then(Value::as_array) {
        if !notes.is_empty() {
            out.push_str("\n## Notes\n");
            for n in notes {
                out.push_str(&format!(
                    "- [{}](/notes/{}): {}\n",
                    s(n, "/title"),
                    s(n, "/slug"),
                    text::clip(&s(n, "/content").replace('\n', " "), 300)
                ));
            }
        }
    }
    for (h, key) in [("Memory", "memory"), ("He mentioned", "mentions")] {
        if let Some(a) = f
            .get(key)
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        {
            out.push_str(&format!("\n## {h}\n"));
            for m in a.iter().filter_map(Value::as_str) {
                out.push_str(&format!("- {}\n", text::clip(m, 300)));
            }
        }
    }
    out
}

// ------------------------------------------------------------------ DeepSeek

pub struct LlmComposer {
    pub model: Arc<dyn lucina_llm::ChatModel>,
    pub persona: String,
    pub max_chars: usize,
    /// The owner's time zone: every timestamp in the facts is shown in it, never in UTC.
    pub tz: chrono_tz::Tz,
}

/// Facts with every full timestamp (`2026-10-28T11:00:00Z`) rewritten as local time
/// (`Wed 2026-10-28 12:00`), so the model never sees, and never repeats, a UTC clock time.
pub fn localize_times(v: &Value, tz: chrono_tz::Tz) -> Value {
    match v {
        Value::String(s) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(t) => Value::String(t.with_timezone(&tz).format("%a %Y-%m-%d %H:%M").to_string()),
            Err(_) => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| localize_times(x, tz)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), localize_times(x, tz)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

const RULES: &str = "You are writing a PROACTIVE message: Lucina reaches out to Giuseppe on her own, before he asks, \
because something is worth his attention now.\n\
Rules:\n\
- Language: {LANG}. Short: at most 3 sentences. Warm, direct, no filler, no generic greeting formula.\n\
- Use ONLY the facts in the JSON. Never invent times, names, numbers, causes or outcomes. If a fact is uncertain, \
say it looks so.\n\
- Never claim you did something (sent, booked, bought, deleted, fixed, changed) and do not talk about what you did \
not do. Do not speculate about causes or about who fixed something. If facts.prepNote is set, you prepared a short summary in his notes: say so briefly, without the note's name (the \
app shows a button to open it).\n\
- Texts in the facts may be cut for length (marked truncated, or ending with …): that is not missing content, never \
mention it.\n\
- Plain text, no markdown headings, at most one emoji. Do not mention scores, priorities, rules, signals or that \
this is automatic.\n\
- Times: every clock time in the facts is already in the owner's local time zone ({TZ}), as is the local time now: {NOW}. \
State clock times exactly as given, in local time, and never write UTC, GMT or a trailing Z. Prefer relative times (\"tra un'ora\").\n\
Answer in exactly this format:\n\
MESSAGE:\n<the message>\n\
PREP:\n<only when facts.wantPrep is true: a concise markdown summary to read before the meeting, at most 12 lines, \
built only from facts.notes, facts.memory and facts.mentions, in the same language; otherwise NONE>";

const PULSE_RULES: &str = "You are writing Lucina's DAILY PULSE for Giuseppe: one short morning message.\n\
Rules:\n\
- Language: {LANG}. A one-line opener, then up to three short sections with bullets, ONLY for the sections that have \
items in the JSON: important today (facts.important), completed (facts.completed), keep an eye on (facts.watch). \
At most 3 bullets per section, one line each.\n\
- Use ONLY the facts. Never invent anything. No actions taken by you.\n\
- Plain text (bullets with •), no markdown headings, at most one emoji. Times in the facts are already local ({TZ}); the local time now is {NOW}. Never write UTC, GMT or a trailing Z.\n\
Answer in exactly this format:\nMESSAGE:\n<the message>\nPREP:\nNONE";

/// `MESSAGE:` / `PREP:` sections.
fn parse_marked_reply(raw: &str) -> Option<(String, Option<String>)> {
    let t = raw.trim();
    let m = t.find("MESSAGE:")?;
    let rest = &t[m + "MESSAGE:".len()..];
    let (msg, prep) = match rest.find("PREP:") {
        Some(p) => (&rest[..p], Some(rest[p + "PREP:".len()..].trim())),
        None => (rest, None),
    };
    let msg = msg.trim();
    if msg.is_empty() {
        return None;
    }
    let prep = prep
        .map(|p| {
            p.trim_start_matches("```markdown")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim()
        })
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("none"))
        .map(str::to_owned);
    Some((msg.to_owned(), prep))
}

fn parse_reply(raw: &str) -> Option<(String, Option<String>)> {
    parse_marked_reply(raw).or_else(|| parse_json_reply(raw))
}

fn parse_json_reply(raw: &str) -> Option<(String, Option<String>)> {
    let t = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    let v: Value = serde_json::from_str(&t[start..=end]).ok()?;
    let msg = v.get("message")?.as_str()?.trim().to_owned();
    if msg.is_empty() {
        return None;
    }
    let prep = v
        .get("prep")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned);
    Some((msg, prep))
}

#[async_trait::async_trait]
impl Composer for LlmComposer {
    async fn compose(&self, req: &ComposeRequest) -> Composed {
        let rules = if req.kind == Kind::Pulse {
            PULSE_RULES
        } else {
            RULES
        }
        .replace("{LANG}", lang_name(&req.language))
        .replace("{TZ}", self.tz.name())
        .replace("{NOW}", &req.local_time);
        // The facts, the template fallbacks and the prep note all use local times.
        let localized = ComposeRequest {
            facts: localize_times(&req.facts, self.tz),
            ..req.clone()
        };
        let req = &localized;
        let mut facts = req.facts.clone();
        if let Some(o) = facts.as_object_mut() {
            o.insert("kind".into(), json!(req.kind.as_str()));
            o.insert("wantPrep".into(), json!(req.want_prep));
        }
        let request = lucina_llm::ChatRequest {
            messages: vec![
                lucina_llm::Message::system(format!("{}\n\n{rules}", self.persona.trim())),
                lucina_llm::Message::user(serde_json::to_string(&facts).unwrap_or_default()),
            ],
            temperature: Some(0.4),
            max_tokens: Some(if req.want_prep { 1600 } else { 500 }),
            purpose: "proactive",
            ..Default::default()
        };
        match self.model.complete(request).await {
            Ok(r) => match parse_reply(r.message.text()) {
                Some((message, prep)) => Composed {
                    message: text::clip(&message, self.max_chars),
                    prep: if req.want_prep {
                        prep.or_else(|| Some(prep_template(req)))
                    } else {
                        None
                    },
                    via: "llm",
                },
                None => {
                    tracing::warn!(
                        kind = %req.kind,
                        finish = ?r.finish_reason,
                        chars = r.message.text().chars().count(),
                        "compose: unparseable model reply, using the template"
                    );
                    template(req)
                }
            },
            Err(e) => {
                tracing::warn!(kind = %req.kind, error = %e, "compose: model failed, using the template");
                template(req)
            }
        }
    }
}

pub struct TemplateComposer;

#[async_trait::async_trait]
impl Composer for TemplateComposer {
    async fn compose(&self, req: &ComposeRequest) -> Composed {
        template(req)
    }
}

pub struct LlmJudge {
    pub model: Arc<dyn lucina_llm::ChatModel>,
    pub language: String,
}

const JUDGE: &str = "You check whether something Giuseppe was following is now resolved. Use ONLY the evidence in the \
JSON: his message, Lucina's reply, later related messages, and the current health of his services. \
\"resolved\" needs explicit evidence (a later message saying it is fixed, done, arrived or answered; or a service he \
said was down that now checks fine). \"open\" when it clearly still needs his attention. Otherwise \"unknown\" \
(small talk, a question already answered in Lucina's reply, nothing to follow). Never guess.\n\
Answer with JSON only: {\"status\": \"resolved\"|\"open\"|\"unknown\", \"confidence\": 0.0-1.0, \"summary\": \"one short \
sentence in {LANG}, without quoting private details beyond what is needed\"}";

#[async_trait::async_trait]
impl Judge for LlmJudge {
    async fn judge(&self, facts: &Value) -> Option<Judgement> {
        let request = lucina_llm::ChatRequest {
            messages: vec![
                lucina_llm::Message::system(JUDGE.replace("{LANG}", lang_name(&self.language))),
                lucina_llm::Message::user(serde_json::to_string(facts).ok()?),
            ],
            temperature: Some(0.0),
            max_tokens: Some(200),
            purpose: "proactive_judge",
            ..Default::default()
        };
        let r = match self.model.complete(request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "judge: model failed");
                return None;
            }
        };
        let raw = r.message.text();
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        let j: Judgement = serde_json::from_str(&raw[start..=end]).ok()?;
        matches!(j.status.as_str(), "resolved" | "open" | "unknown").then_some(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: Kind, lang: &str, facts: Value) -> ComposeRequest {
        ComposeRequest {
            kind,
            language: lang.into(),
            priority: Priority::Normal,
            facts,
            local_time: "giovedì 22/10 13:05".into(),
            want_prep: kind == Kind::MeetingPrep,
        }
    }

    #[tokio::test]
    async fn meeting_prep_input_shows_local_time_not_utc() {
        let m = Arc::new(lucina_llm::ScriptedModel::new());
        m.push_text("MESSAGE:\nTherese zoom è tra un'ora, alle 12:00.\nPREP:\nNONE");
        let c = LlmComposer {
            model: m.clone(),
            persona: PERSONA_DEFAULT.into(),
            max_chars: 700,
            tz: chrono_tz::Europe::Copenhagen,
        };
        let f = json!({"event": {"title": "Therese zoom", "start": "2026-10-28T11:00:00Z", "end": "2026-10-28T12:00:00Z", "startsInMinutes": 60}, "notes": []});
        let out = c.compose(&req(Kind::MeetingPrep, "it", f)).await;
        assert_eq!(out.via, "llm");
        let sent = m.requests.lock().unwrap().clone();
        let system = sent[0].messages[0].text().to_owned();
        let input = sent[0].messages[1].text().to_owned();
        assert!(input.contains("12:00"), "{input}");
        assert!(input.contains("13:00"), "the end time is local too: {input}");
        assert!(!input.contains("11:00"), "{input}");
        assert!(!input.contains("UTC") && !input.contains("00Z"), "{input}");
        assert!(system.contains("never write UTC, GMT or a trailing Z"));
        assert!(system.contains("Europe/Copenhagen"));
        assert!(!system.contains("facts are in UTC"));
        // The prep note built without the model is local as well.
        let prep = prep_template(&ComposeRequest {
            facts: localize_times(
                &json!({"event": {"title": "x", "start": "2026-10-28T11:00:00Z"}}),
                chrono_tz::Europe::Copenhagen,
            ),
            ..req(Kind::MeetingPrep, "it", json!({}))
        });
        assert!(prep.contains("When: Wed 2026-10-28 12:00"), "{prep}");
    }

    #[test]
    fn templates_in_three_languages() {
        let f = json!({"event": {"title": "CRAFT-IT4SD", "start": "2026-10-22T12:00:00Z", "startsInMinutes": 55}, "notes": [{"slug": "craftit-meeting", "title": "CRAFT", "content": "x"}]});
        let it = template(&req(Kind::MeetingPrep, "it", f.clone()));
        assert!(it.message.starts_with("Tra 55 minuti"));
        assert!(it
            .prep
            .as_deref()
            .unwrap()
            .contains("/notes/craftit-meeting"));
        assert!(template(&req(Kind::MeetingPrep, "en", f.clone()))
            .message
            .contains("starts in 55"));
        assert!(template(&req(Kind::MeetingPrep, "da", f))
            .message
            .contains("starter om 55"));
        let p = template(&req(
            Kind::Pulse,
            "it",
            json!({"important": [{"line": "a"}], "completed": [], "watch": [{"line": "b"}]}),
        ));
        assert!(
            p.message.contains("Oggi\n• a")
                && p.message.contains("Da tenere d'occhio\n• b")
                && !p.message.contains("Fatto")
        );
    }

    #[tokio::test]
    async fn llm_reply_parsing_and_fallback() {
        let m = Arc::new(lucina_llm::ScriptedModel::new());
        m.push_text("```json\n{\"message\": \"Tra un'ora hai CRAFT.\", \"prep\": \"# Prep\"}\n```");
        m.push_text("MESSAGE:\nFra en time har du CRAFT.\nPREP:\nNONE");
        m.push_text("not json at all");
        let c = LlmComposer {
            model: m.clone(),
            persona: PERSONA_DEFAULT.into(),
            max_chars: 700,
            tz: chrono_tz::Europe::Copenhagen,
        };
        let f = json!({"event": {"title": "CRAFT", "startsInMinutes": 60}, "notes": []});
        let a = c.compose(&req(Kind::MeetingPrep, "it", f.clone())).await;
        assert_eq!(
            (a.message.as_str(), a.prep.as_deref(), a.via),
            ("Tra un'ora hai CRAFT.", Some("# Prep"), "llm")
        );
        let m2 = c.compose(&req(Kind::MeetingPrep, "da", f.clone())).await;
        assert_eq!(m2.message, "Fra en time har du CRAFT.");
        assert!(
            m2.prep.unwrap().starts_with("# Prep"),
            "template prep when the model gives none"
        );
        let b = c.compose(&req(Kind::MeetingPrep, "it", f)).await;
        assert_eq!(b.via, "template");

        m.push_text("{\"status\": \"resolved\", \"confidence\": 0.8, \"summary\": \"Il checkout ora funziona.\"}");
        m.push_text("{\"status\": \"maybe\", \"confidence\": 0.8, \"summary\": \"?\"}");
        let j = LlmJudge {
            model: m,
            language: "it".into(),
        };
        assert_eq!(j.judge(&json!({})).await.unwrap().status, "resolved");
        assert!(j.judge(&json!({})).await.is_none());
    }
}
