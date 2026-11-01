//! Rich replies, Lucina message format v1 (`LUCINA_RICH_REPLIES=true`; Node
//! `src/lucina-rich-reply.js`, contract `docs/lucina-message-format.md`).
//!
//! The planner's reply is the draft. A short conversational draft is split
//! into bubbles without a model call; a long or structured one is restyled by
//! DeepSeek in JSON output mode under the v1 schema, with one retry on invalid
//! JSON, and the deterministic splitter as the fallback. The old
//! "💡 Posso anche" footer becomes buttons; a pending calendar confirmation
//! gets Sì / No / Modifica. Formatting never fails a turn.

use std::sync::Arc;
use std::time::Duration;

use lucina_intent::hints::{extract_capability_hints, output_language};
use lucina_llm::{ChatModel, ChatRequest, Message};
use lucina_store::rich::{self, RichMessage};
use serde_json::{json, Value};

/// Conversational style for every Lucina prompt (only with rich replies on).
pub const STYLE_RULES: &str = "How you talk (every channel): like a person texting, warm and conversational, first person. Answer first in one or two short sentences; give details only when asked, and offer them instead of dumping them. No headings in chat. At most 3-5 bullets; if there is more, summarize the rest. Emojis are signposts, not decoration: ✅ done, ⚠️ problem, 📅 dates, 🛒 shopping, 💪 training, 📝 notes. One or two per message at most. Always answer in the language of the owner's latest message (Italian, English or Danish); earlier turns, the device and his location do not change it.";

/// The Secretary app's extra system text when the app renders bubbles and widgets.
pub const SECRETARY_EXTRA_SYSTEM_RICH: &str = "This conversation comes from the Secretary app (web and iPhone), which shows your reply as short chat bubbles plus widgets and buttons. Reply in the language of the message. Lead with the answer in 1-2 short sentences. Inline markdown (bold, italic, links) is fine; no headings and no tables. A short list is fine when the content is a list. If you need a password, key or token from Giuseppe, never ask him to write it in chat: ask him to say \"salva un segreto NOME\" (\"save a secret NAME\"), which opens a card where he pastes it.";

const DANISH_WORDS: [&str; 10] = [
    "jeg", "ikke", "og", "skal", "kan", "har", "det", "til", "hvad", "tak",
];

/// What the composer reads of a draft. A longer one goes out whole, as plain text.
const COMPOSER_DRAFT_CHARS: usize = 6000;

/// `it`, `en` or `da`.
pub fn guess_language(reply: &str, user: &str) -> &'static str {
    let lower = reply.to_lowercase();
    let italian_accents = lower.chars().any(|c| "àèéìòù".contains(c));
    if !italian_accents {
        let hits = lower.chars().filter(|c| "æøå".contains(*c)).count()
            + lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| DANISH_WORDS.contains(w))
                .count();
        if hits >= 2 {
            return "da";
        }
    }
    output_language(reply, user)
}

/// The turn left an action waiting for the owner's yes or no.
pub fn is_pending_confirmation(action: &str, action_result: &str) -> bool {
    (action == "calendar_confirm" || action == "meshy")
        && action_result
            .to_lowercase()
            .contains("awaiting confirmation")
}

/// Sì / No / Modifica (yes and no send the words the yes/no flow understands).
pub fn confirm_actions(lang: &str) -> Vec<Value> {
    let (yes, no, edit) = match lang {
        "en" => (("Yes", "yes"), ("No", "no"), ("Change it", "Change it:")),
        "da" => (("Ja", "ja"), ("Nej", "nej"), ("Ret det", "Ret det:")),
        _ => (("Sì", "sì"), ("No", "no"), ("Modifica", "Modifica:")),
    };
    vec![
        json!({"id": "confirm-yes", "label": yes.0, "emoji": "✅", "kind": "confirm", "payload": {"answer": "yes", "text": yes.1}}),
        json!({"id": "confirm-no", "label": no.0, "emoji": "✖️", "kind": "confirm", "payload": {"answer": "no", "text": no.1}}),
        json!({"id": "confirm-edit", "label": edit.0, "emoji": "✏️", "kind": "confirm", "payload": {"answer": "edit", "text": edit.1}}),
    ]
}

/// Capability hints as buttons; anything that starts work or posts publicly
/// only prefills the composer.
pub fn hint_actions(ids: &[&str], lang: &str) -> Vec<Value> {
    let en = lang == "en";
    ids.iter()
        .filter_map(|id| {
            let (emoji, it, en_l, prefill) = match *id {
                "reminder" => ("⏰", ("Promemoria", "Ricordami alle 21:30 di "), ("Reminder", "Remind me at 21:30 to "), true),
                "list-reminders" => ("📋", ("I miei promemoria", "Quali promemoria ho?"), ("My reminders", "What reminders do I have?"), false),
                "sheet" => ("⚖️", ("Registra nel foglio", "Registra "), ("Log to the sheet", "Log "), true),
                "graph" => ("📈", ("Grafico", "Fammi un grafico di "), ("Chart", "Make a chart of "), true),
                "image" => ("🎨", ("Immagine", "Genera un'immagine di "), ("Image", "Generate an image of "), true),
                "harness" => ("🛠️", ("Task di codice", "Con Harness: "), ("Code task", "With Harness: "), true),
                "poko" => ("📰", ("News di Poko", "Fai postare a Poko le news su "), ("Poko news", "Have Poko post the news about "), true),
                "menu" => ("💡", ("Cosa sai fare?", "Cosa sai fare?"), ("What can you do?", "What can you do?"), false),
                _ => return None,
            };
            let (label, text) = if en { en_l } else { it };
            let payload = if prefill {
                json!({"text": text, "prefill": true})
            } else {
                json!({"text": text})
            };
            Some(json!({"id": format!("hint-{id}"), "label": label, "emoji": emoji, "kind": "reply", "payload": payload}))
        })
        .collect()
}

fn details(lang: &str) -> (&'static str, &'static str) {
    match lang {
        "en" => ("Details", "Give me the details"),
        "da" => ("Detaljer", "Giv mig detaljerne"),
        _ => ("Dettagli", "Dammi i dettagli"),
    }
}

/// The composer's system prompt (same rules as Node's `buildComposerSystemPrompt`).
pub fn composer_system_prompt(lang: &str) -> String {
    let (label, text) = details(lang);
    [
        "You turn a draft reply by Lucina (Giuseppe's personal assistant) into a chat message for the Secretary app.".to_owned(),
        "Return ONLY one json object in \"Lucina message format v1\":".into(),
        "{\"bubbles\":[\"…\"],\"blocks\":[…],\"actions\":[…]}".into(),
        "Rules:".into(),
        "- Keep every fact, number, name and caveat of the draft. Never add facts, promises or claims that are not in the draft. You may drop filler and repetition.".into(),
        format!("- bubbles: 1 to 3 short chunks, ideally under 160 characters and never over 280, written like texting: warm, first person, in the draft's language ({lang}). Lead with the answer. Inline markdown only (**bold**, *italic*, [label](https://…)). No headings, no bullet lists, no tables inside bubbles."),
        "- Bubbles carry the gist and the tone; the details live in blocks. Never repeat block content in the bubbles.".into(),
        "- Emojis are signposts, not decoration (✅ ⚠️ 📅 🛒 💪 📝 💡): at most one per bubble.".into(),
        "- Everything structured goes into blocks instead of text walls. Block types (use \"type\"):".into(),
        "  {\"type\":\"checklist\",\"title\":\"…\",\"items\":[{\"text\":\"…\",\"done\":true}]}  — things done / to do, present / missing".into(),
        "  {\"type\":\"card\",\"emoji\":\"📅\",\"title\":\"…\",\"subtitle\":\"…\",\"body\":\"…\",\"tone\":\"info|success|warning\"}  — one topic; warning for limits and problems".into(),
        "  {\"type\":\"stat\",\"items\":[{\"label\":\"…\",\"value\":\"…\",\"delta\":\"…\"}]}  — 1-4 numbers in a row".into(),
        "  {\"type\":\"list\",\"title\":\"…\",\"items\":[{\"emoji\":\"…\",\"text\":\"…\",\"meta\":\"…\"}]}  — at most 5 items".into(),
        "  {\"type\":\"table\",\"title\":\"…\",\"columns\":[\"…\"],\"rows\":[[\"…\"]]}  — at most 4 columns and 6 rows".into(),
        "  {\"type\":\"steps\",\"title\":\"…\",\"items\":[\"…\"]}  — numbered steps".into(),
        "  {\"type\":\"quote\",\"text\":\"…\",\"cite\":\"…\"}".into(),
        "  {\"type\":\"link_preview\",\"url\":\"https://…\",\"title\":\"…\"}".into(),
        "  {\"type\":\"place\",\"title\":\"…\",\"address\":\"…\",\"when\":\"<RFC 3339 if it's an appointment>\",\"kind\":\"event|place\"}  — an appointment with a location, an address, a shop or restaurant mentioned in the draft (only places the draft names; never invent an address; at most 3)".into(),
        "- Use 0-4 blocks; none when the draft is a simple answer. Group related lines into one block. A \"present vs missing\" inventory is best as a checklist (done = present).".into(),
        "- actions: 0-3 buttons {\"id\":\"…\",\"label\":\"…\",\"emoji\":\"…\",\"kind\":\"reply\",\"payload\":{\"text\":\"…\"}}. kind \"reply\" sends payload.text as Giuseppe's next message; add \"prefill\":true to payload to put it in his composer instead of sending (use it for anything that would start real work). kind \"route\" opens an app page: payload {\"path\":\"/notes/<slug>\"} and the like (/notes, /shopping, /links, /gym, /weight).".into(),
        format!("- If you left details out of the bubbles and blocks, add {{\"id\":\"details\",\"label\":\"{label}\",\"emoji\":\"🔎\",\"kind\":\"reply\",\"payload\":{{\"text\":\"{text}\"}}}}. Offer at most one natural follow-up question as a reply button."),
        "- Do not repeat buttons listed under \"Buttons the app already shows\".".into(),
    ]
    .join("\n")
}

fn composer_user_prompt(draft: &str, user_text: &str, lang: &str, preset: &[Value]) -> String {
    let mut parts = vec![
        format!(
            "Giuseppe wrote:\n{}",
            user_text.chars().take(2000).collect::<String>()
        ),
        format!(
            "Lucina's draft reply ({lang}):\n{}",
            draft.chars().take(COMPOSER_DRAFT_CHARS).collect::<String>()
        ),
    ];
    if !preset.is_empty() {
        let labels: Vec<&str> = preset.iter().filter_map(|a| a["label"].as_str()).collect();
        parts.push(format!(
            "Buttons the app already shows: {}",
            labels.join(", ")
        ));
    }
    parts.push("Return the json object now.".into());
    parts.join("\n\n")
}

/// The first `{…}` object in a reply (fenced or not).
fn parse_json_object(content: &str) -> Result<Value, String> {
    let text = content.trim();
    let inner = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|t| t.rsplit_once("```").map(|(a, _)| a))
        .unwrap_or(text);
    let (start, end) = (inner.find('{'), inner.rfind('}'));
    match (start, end) {
        (Some(s), Some(e)) if e > s => {
            serde_json::from_str(&inner[s..=e]).map_err(|e| e.to_string())
        }
        _ => Err("no JSON object in the reply".into()),
    }
}

/// Ask the model for v1 JSON (JSON output mode); one retry on invalid JSON or schema errors.
pub async fn compose_with_model(
    model: &Arc<dyn ChatModel>,
    draft: &str,
    user_text: &str,
    lang: &str,
    preset: &[Value],
    timeout: Duration,
) -> Option<RichMessage> {
    let mut messages = vec![
        Message::system(composer_system_prompt(lang)),
        Message::user(composer_user_prompt(draft, user_text, lang, preset)),
    ];
    for attempt in 1..=2 {
        let request = ChatRequest {
            messages: messages.clone(),
            temperature: Some(0.3),
            purpose: "rich-reply",
            json_object: true,
            ..ChatRequest::default()
        };
        let content = match tokio::time::timeout(timeout, model.complete(request)).await {
            Ok(Ok(r)) => r.message.text().to_owned(),
            Ok(Err(e)) => {
                tracing::warn!(attempt, error = %e, "rich compose failed");
                return None;
            }
            Err(_) => {
                tracing::warn!(attempt, "rich compose timed out");
                return None;
            }
        };
        let problem = match parse_json_object(&content) {
            Ok(mut parsed) => {
                if let Value::Object(m) = &mut parsed {
                    m.insert("lang".into(), json!(lang));
                }
                match rich::normalize(&parsed) {
                    Ok((message, dropped)) => {
                        if !dropped.is_empty() {
                            tracing::warn!(dropped = ?dropped, "rich compose dropped parts");
                        }
                        // The format's limits would cut the text: better none of its widgets.
                        if rich::loses_text(&parsed, &message) {
                            tracing::warn!("rich compose over the format's limits");
                            return None;
                        }
                        return Some(message);
                    }
                    Err(errors) => errors.join("; "),
                }
            }
            Err(e) => e,
        };
        tracing::warn!(attempt, problem = %problem, "rich compose invalid");
        messages.push(Message::assistant(
            content.chars().take(4000).collect::<String>(),
        ));
        messages.push(Message::user(format!(
            "That was not a valid Lucina message format v1 json object ({problem}). Return only the corrected json object."
        )));
    }
    None
}

/// What [`compose`] needs about the turn.
pub struct Draft<'a> {
    pub text: &'a str,
    pub user_text: &'a str,
    pub action: &'a str,
    pub action_result: &'a str,
    /// `false`: deterministic formatting only (job results).
    pub use_model: bool,
}

/// Turn a finished reply into a v1 message. `None`: send it as plain text (empty, or v1 would cut it).
pub async fn compose(
    model: &Arc<dyn ChatModel>,
    d: Draft<'_>,
    timeout: Duration,
) -> Option<RichMessage> {
    let source = d.text.trim();
    if source.is_empty() {
        return None;
    }
    let (body, ids, hint_lang) = extract_capability_hints(source);
    let mut body = if body.trim().is_empty() {
        source.to_owned()
    } else {
        body
    };
    let lang = guess_language(&body, d.user_text);
    // Code, and anything the composer could not read to the end, stays whole: v1 clips a card at
    // 600 characters, and the clipped text is what the timeline and the recent turns would keep.
    if body.contains("```") || body.chars().count() > COMPOSER_DRAFT_CHARS {
        return None;
    }
    let mut preset = Vec::new();
    if is_pending_confirmation(d.action, d.action_result) {
        preset.extend(confirm_actions(lang));
        for tail in [
            "Rispondi sì o no.",
            "Reply yes or no.",
            "Svar ja eller nej.",
        ] {
            if let Some(stripped) = body.trim_end().strip_suffix(tail) {
                if !stripped.trim().is_empty() {
                    body = stripped.trim_end().to_owned();
                }
            }
        }
    }
    preset.extend(hint_actions(&ids, hint_lang.unwrap_or(lang)));
    let preset: Vec<Value> = preset
        .iter()
        .enumerate()
        .filter_map(|(i, a)| rich::normalize_action(a, i))
        .collect();

    let mut message = None;
    if d.use_model && !rich::is_conversational(&body) {
        message = compose_with_model(model, &body, d.user_text, lang, &preset, timeout).await;
    }
    let mut message = match message {
        Some(m) => m,
        None => {
            let m = rich::from_plain_text(&body, lang, 3, Vec::new());
            if rich::loses_draft(&body, &m) {
                return None;
            }
            m
        }
    };
    if message.lang.is_none() {
        message.lang = Some(lang.to_owned());
    }
    message.actions = rich::merge_actions(preset, std::mem::take(&mut message.actions));
    Some(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_llm::{LlmError, ScriptedModel};

    fn model(s: ScriptedModel) -> Arc<dyn ChatModel> {
        Arc::new(s)
    }

    const WALL: &str = "**Inventario fattuale — cosa supporto ORA**\n\n**Canali**\n- Telegram (chat + foto con vision) e Discord.\n- Nessun altro canale push confermato.\n\n**Memoria**\n- Honcho, workspace `hermes-peer1`.\n\n**Limiti**\n- Nessuna email.\n\nSe vuoi, il prossimo passo è decidere *quale* buco colmare.\n\n—\n💡 Posso anche:\n• Promemoria Telegram — es. «avvisami alle 21:30 di …»";

    #[tokio::test]
    async fn short_replies_never_call_the_model() {
        let s = ScriptedModel::new();
        let m = model(s);
        let r = compose(
            &m,
            Draft {
                text: "Fatto ✅ Ti ricordo la spesa domani alle 9.",
                user_text: "",
                action: "reply",
                action_result: "",
                use_model: true,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            r.bubbles,
            vec!["Fatto ✅ Ti ricordo la spesa domani alle 9."]
        );
    }

    #[tokio::test]
    async fn invalid_json_gets_one_retry_in_json_mode() {
        let s = Arc::new(ScriptedModel::new());
        s.push_text("not json");
        s.push_text(r#"{"bubbles":["Ecco l'inventario ✅"],"blocks":[{"type":"checklist","items":[{"text":"Notifiche app","done":true},{"text":"Morning brief"}]}]}"#);
        let m: Arc<dyn ChatModel> = s.clone();
        let r = compose(
            &m,
            Draft {
                text: WALL,
                user_text: "inventario",
                action: "reply",
                action_result: "",
                use_model: true,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let reqs = s.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(reqs.iter().all(|r| r.json_object));
        assert_eq!(
            reqs[1].messages.len(),
            4,
            "the retry carries the bad answer and the error"
        );
        assert!(reqs[0].messages[0].text().contains("json object"));
        assert_eq!(r.bubbles, vec!["Ecco l'inventario ✅"]);
        assert_eq!(r.blocks[0]["type"], "checklist");
        // The hint footer became a (prefill) button, not text.
        assert_eq!(r.actions[0]["id"], "hint-reminder");
        assert_eq!(r.actions[0]["payload"]["prefill"], true);
    }

    #[tokio::test]
    async fn failures_fall_back_to_bubbles_and_widgets() {
        let s = ScriptedModel::new();
        s.push(Err(LlmError::Timeout));
        let m = model(s);
        let r = compose(
            &m,
            Draft {
                text: WALL,
                user_text: "inventario",
                action: "reply",
                action_result: "",
                use_model: true,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(r.bubbles.len() <= 3, "{:?}", r.bubbles);
        assert!(!r.blocks.is_empty());
        assert!(!r.plain_text(false).contains("Posso anche"));
        let s2 = ScriptedModel::new();
        s2.push_text(r#"{"bubbles": []}"#);
        s2.push_text("{}");
        let r2 = compose(
            &model(s2),
            Draft {
                text: WALL,
                user_text: "inventario",
                action: "reply",
                action_result: "",
                use_model: true,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(r2, r);
    }

    #[tokio::test]
    async fn a_pending_confirmation_gets_confirm_buttons() {
        let m = model(ScriptedModel::new());
        let r = compose(&m, Draft {
            text: "Confermi? Aggiungo «Dentista» domani alle 9:30 su Google Calendar. Rispondi sì o no.",
            user_text: "aggiungi dentista domani alle 9:30",
            action: "calendar_confirm",
            action_result: "[calendar event awaiting confirmation]",
            use_model: true,
        }, Duration::from_secs(5)).await.unwrap();
        assert_eq!(
            r.bubbles,
            vec!["Confermi? Aggiungo «Dentista» domani alle 9:30 su Google Calendar."]
        );
        let labels: Vec<&str> = r
            .actions
            .iter()
            .map(|a| a["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Sì", "No", "Modifica"]);
        assert_eq!(
            r.actions[0]["payload"],
            json!({"answer": "yes", "text": "sì"})
        );
        assert!(r
            .plain_text(true)
            .ends_with("↪︎ rispondi: «sì» · «no» · «Modifica …»"));
    }

    fn draft(text: &str) -> Draft<'_> {
        Draft {
            text,
            user_text: "domanda",
            action: "reply",
            action_result: "",
            use_model: true,
        }
    }

    #[tokio::test]
    async fn code_and_very_long_drafts_stay_whole_without_a_model_call() {
        let code = format!("Ecco:\n```rust\n{}\n```", "let x = 1;\n".repeat(200));
        let long = "Una frase qualsiasi che si ripete. ".repeat(300);
        for text in [code.as_str(), long.as_str()] {
            let s = ScriptedModel::new();
            let m = model(s);
            assert!(compose(&m, draft(text), Duration::from_secs(5)).await.is_none());
        }
    }

    #[tokio::test]
    async fn a_composed_card_over_the_limit_is_not_clipped() {
        let text = "Una frase qualsiasi che si ripete. ".repeat(60);
        let body = "x".repeat(900);
        let s = ScriptedModel::new();
        s.push_text(&format!(
            r#"{{"bubbles": ["Ecco"], "blocks": [{{"type": "card", "title": "T", "body": "{body}"}}]}}"#
        ));
        let r = compose(&model(s), draft(&text), Duration::from_secs(5)).await;
        assert!(r.is_none(), "{r:?}");
    }

    #[tokio::test]
    async fn a_composed_message_within_the_limits_is_kept() {
        let text = "Una frase qualsiasi che si ripete. ".repeat(40);
        let s = ScriptedModel::new();
        s.push_text(r#"{"bubbles": ["Ecco"], "blocks": [{"type": "card", "title": "T", "body": "corto"}]}"#);
        let r = compose(&model(s), draft(&text), Duration::from_secs(5)).await.unwrap();
        assert_eq!(r.bubbles, ["Ecco"]);
        assert_eq!(r.blocks.len(), 1);
    }

    #[test]
    fn the_composer_knows_place_blocks() {
        let p = composer_system_prompt("it");
        assert!(p.contains(r#"{"type":"place","title":"…","address":"…","when":"<RFC 3339 if it's an appointment>","kind":"event|place"}"#), "{p}");
        assert!(p.contains("never invent an address; at most 3"));
    }

    #[test]
    fn languages() {
        assert_eq!(
            guess_language("Skal jeg tilføje det til kalenderen?", ""),
            "da"
        );
        assert_eq!(guess_language("Shall I add it to your calendar?", ""), "en");
        assert_eq!(guess_language("Lo aggiungo?", "aggiungi dentista"), "it");
    }
}
