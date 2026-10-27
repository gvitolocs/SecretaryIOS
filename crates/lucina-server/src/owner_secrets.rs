//! "Add secret": the owner stores secrets (and creates projects) in Infisical
//! from the apps, and Lucina keeps every value away from her sinks.
//!
//! - `POST /api/secretary/secrets/grant {grant, value? | held?}`: relays an
//!   owner grant (signed by secretary-server after the owner confirmed on a
//!   device) to infisical-brokerd as op `grant`. Lucina cannot make a grant;
//!   she only passes it on with the value. After a save she writes a receipt
//!   (names and fingerprint, never the value) to the timeline and learns the
//!   value in the redactor under its name.
//! - **`/secret NAME VALUE [target]`** in any chat is answered here, before
//!   the model, the timeline, Honcho, recent turns and the replay cache: the
//!   value is held in memory ([`HeldSecrets`], 10 minutes, zeroized) and
//!   Lucina posts a card that opens the app's sheet with it.
//! - **Token shapes in a normal message** (the redactor's detector) are
//!   masked in the turn text before anything runs; each one is held the
//!   same way and Lucina offers "Save as secret?".
//! - **"crea un progetto Infisical per X"** gets a card that opens the
//!   sheet's new-project step; nothing is created without the owner's tap.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lucina_secrets::SecretValue;
use lucina_store::timeline::{Meta, NewItem, TimelineStore};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use zeroize::Zeroize;

use crate::api::ApiError;

/// Default broker socket.
pub const DEFAULT_SOCKET: &str = "/run/infisical-broker/broker.sock";
/// How long a held value waits for the owner.
pub const HOLD_TTL: Duration = Duration::from_secs(600);
/// Held values at most (the oldest go first).
pub const HOLD_MAX: usize = 20;
/// Largest broker answer.
const MAX_LINE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct BrokerConfig {
    /// `LUCINA_BROKER_SOCKET`.
    pub socket: PathBuf,
    /// `LUCINA_BROKER_GRANTS_TOKEN_FILE`: the relay caller's token (a caller
    /// with no grants of its own; unset: "Add secret" answers 503).
    pub token_file: Option<PathBuf>,
}

impl BrokerConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        Self {
            socket: get("LUCINA_BROKER_SOCKET")
                .map(|p| PathBuf::from(p.trim()))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET)),
            token_file: get("LUCINA_BROKER_GRANTS_TOKEN_FILE")
                .map(|p| PathBuf::from(p.trim()))
                .filter(|p| !p.as_os_str().is_empty()),
        }
    }

    /// The relay token, registered with the redactor (never a sink).
    fn token(&self) -> Result<SecretValue, ApiError> {
        let file = self.token_file.as_ref().ok_or_else(|| {
            ApiError::new(
                503,
                "secrets_unconfigured",
                "LUCINA_BROKER_GRANTS_TOKEN_FILE is not set",
            )
        })?;
        let mut raw = std::fs::read_to_string(file).map_err(|e| {
            tracing::warn!(error = %e.kind(), "broker grants token unreadable");
            ApiError::new(
                503,
                "secrets_unconfigured",
                "the broker token is unreadable",
            )
        })?;
        let token = raw.trim().to_owned();
        raw.zeroize();
        lucina_secrets::redactor().register("LUCINA_BROKER_GRANTS_TOKEN", &token);
        Ok(SecretValue::new(token))
    }
}

struct Held {
    at: Instant,
    value: SecretValue,
}

/// Values the owner typed or pasted into chat, waiting for "Save as
/// secret". Memory only; zeroized when taken, expired or evicted.
#[derive(Default)]
pub struct HeldSecrets {
    map: Mutex<HashMap<String, Held>>,
}

impl std::fmt::Debug for HeldSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldSecrets").finish_non_exhaustive()
    }
}

fn random_id() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).expect("OS random");
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("held-{hex}")
}

impl HeldSecrets {
    /// Hold `value`; returns its id.
    pub fn hold(&self, value: &str) -> String {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, h| h.at.elapsed() < HOLD_TTL);
        while map.len() >= HOLD_MAX {
            let oldest = map.iter().min_by_key(|(_, h)| h.at).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
        let id = random_id();
        map.insert(
            id.clone(),
            Held {
                at: Instant::now(),
                value: SecretValue::new(value.to_owned()),
            },
        );
        id
    }

    /// The value (a copy) while it is held.
    pub fn get(&self, id: &str) -> Option<SecretValue> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, h| h.at.elapsed() < HOLD_TTL);
        map.get(id).map(|h| h.value.duplicate())
    }

    /// Forget it (after a save).
    pub fn drop_id(&self, id: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Zero every string in a JSON value (before dropping a value-bearing one).
fn wipe(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe),
        Value::Object(o) => o.values_mut().for_each(wipe),
        _ => {}
    }
}

/// One request to the broker; `Err((code, message))` for its refusals.
async fn broker_call(socket: &std::path::Path, mut req: Value) -> Result<Value, (String, String)> {
    let fut = async {
        let stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| {
            (
                "broker_unavailable".to_owned(),
                format!("connect: {}", e.kind()),
            )
        })?;
        let (rd, mut wr) = stream.into_split();
        let mut line = serde_json::to_vec(&req).unwrap_or_default();
        line.push(b'\n');
        let w = wr.write_all(&line).await;
        line.zeroize();
        w.map_err(|e| {
            (
                "broker_unavailable".to_owned(),
                format!("write: {}", e.kind()),
            )
        })?;
        let _ = wr.flush().await;
        let mut buf = Vec::new();
        BufReader::new(rd)
            .take(MAX_LINE)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(|e| {
                (
                    "broker_unavailable".to_owned(),
                    format!("read: {}", e.kind()),
                )
            })?;
        let v: Value = serde_json::from_slice(&buf).map_err(|_| {
            (
                "broker_unavailable".to_owned(),
                "malformed broker answer".to_owned(),
            )
        })?;
        buf.zeroize();
        if v["ok"] == Value::Bool(true) {
            return Ok(v["result"].clone());
        }
        Err((
            v["error"]["code"]
                .as_str()
                .unwrap_or("broker_error")
                .to_owned(),
            v["error"]["message"].as_str().unwrap_or("").to_owned(),
        ))
    };
    let out = tokio::time::timeout(Duration::from_secs(60), fut)
        .await
        .unwrap_or_else(|_| {
            Err((
                "broker_unavailable".into(),
                "the broker did not answer".into(),
            ))
        });
    wipe(&mut req);
    out
}

fn broker_error(code: &str, message: &str) -> ApiError {
    let status = match code {
        c if c.starts_with("grant_") => 403,
        "not_allowed" | "unauthorized" => 403,
        "exists" => 409,
        "unknown_project" | "unknown_environment" | "not_found" => 404,
        "bad_request" | "bad_ref" => 400,
        "broker_unavailable" | "upstream_unavailable" => 503,
        "upstream_unauthorized" => 502,
        _ => 502,
    };
    let message = if code == "upstream_unauthorized" {
        format!("Infisical refused the owner identity ({message}). Creating projects needs an organization role that allows it.")
    } else {
        message.to_owned()
    };
    let mut message = message;
    lucina_secrets::redactor().redact_string(&mut message);
    ApiError::new(status, code, message)
}

/// The receipt Lucina writes after a save: names and fingerprint only.
pub fn receipt_text(r: &Value) -> String {
    let project = r["project"]["name"].as_str().unwrap_or("?");
    let verb = if r["action"] == "updated" {
        "Updated"
    } else {
        "Saved"
    };
    format!(
        "{verb} `{}` in {project} / {} / {} (fingerprint {})",
        r["key"].as_str().unwrap_or("?"),
        r["environment"].as_str().unwrap_or("?"),
        r["path"].as_str().unwrap_or("/"),
        r["fingerprint"].as_str().unwrap_or("?"),
    )
}

/// Write a Lucina item; its id (for the chat response, so clients dedupe it).
fn lucina_item(
    timeline: &TimelineStore,
    text: &str,
    kind: &str,
    meta: Option<Meta>,
) -> Option<String> {
    let mut m = Meta::kind(kind);
    if let Some(extra) = meta {
        m = m.merge(extra);
    }
    timeline
        .record(NewItem {
            channel: "web".into(),
            role: "lucina".into(),
            text: text.to_owned(),
            meta: Some(m),
            ..NewItem::default()
        })
        .map(|i| i.id)
}

fn rich_meta(bubbles: Vec<String>, actions: Vec<Value>) -> Option<Meta> {
    let raw = json!({"format": "v1", "bubbles": bubbles, "blocks": [], "actions": actions});
    lucina_store::rich::normalize(&raw)
        .ok()
        .map(|(m, _)| Meta::rich(&m))
}

/// `POST /api/secretary/secrets/grant`.
pub async fn relay(st: &crate::AppState, mut body: Value) -> Result<Value, ApiError> {
    let grant = body["grant"]
        .as_str()
        .filter(|g| g.starts_with("g1.") && g.len() <= 4096)
        .ok_or_else(|| ApiError::bad_request("grant is required"))?
        .to_owned();
    let held = body["held"].as_str().map(str::to_owned);
    let value: Option<SecretValue> = match (body.get("value"), &held) {
        (Some(Value::String(v)), None) => Some(SecretValue::new(v.clone())),
        (None, Some(id)) => Some(st.held_secrets.get(id).ok_or_else(|| {
            ApiError::new(
                410,
                "held_expired",
                "Lucina no longer holds that value (10 minutes); paste it into the sheet",
            )
        })?),
        (None, None) => None,
        _ => return Err(ApiError::bad_request("give value or held, not both")),
    };
    wipe(&mut body);
    let token = st.cfg.broker.token()?;
    let mut req = json!({"token": token.expose_secret(), "op": "grant", "grant": grant});
    if let Some(v) = &value {
        req["value"] = json!(v.expose_secret());
    }
    let result = broker_call(&st.cfg.broker.socket, req)
        .await
        .map_err(|(c, m)| broker_error(&c, &m))?;
    if result.get("fingerprint").is_some() {
        // A save: learn the value under its name, forget the held copy, write the receipt.
        let name = result["key"].as_str().unwrap_or("SECRET").to_owned();
        if let Some(v) = &value {
            if let Err(why) = lucina_secrets::redactor().try_register(&name, v.expose_secret()) {
                tracing::warn!(name = %name, error = %why, "saved secret not registered with the redactor");
            }
        }
        if let Some(id) = &held {
            st.held_secrets.drop_id(id);
        }
        let text = receipt_text(&result);
        let _ = lucina_item(&st.timeline, &format!("✅ {text}"), "secret_receipt", None);
        tracing::info!(name = %name, action = result["action"].as_str().unwrap_or(""), "owner secret saved");
        let mut out = result;
        out["receipt"] = json!(text);
        return Ok(out);
    }
    if let Some(p) = result
        .get("project")
        .filter(|_| result.get("access").is_some())
    {
        let envs: Vec<&str> = p["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["slug"].as_str())
            .collect();
        let text = format!(
            "✅ Created Infisical project {} ({}). Agents and services have no access to it; tell me if one should.",
            p["name"].as_str().unwrap_or("?"),
            envs.join(", ")
        );
        let _ = lucina_item(&st.timeline, &text, "secret_project", None);
        let mut out = result.clone();
        out["receipt"] = json!(text);
        return Ok(out);
    }
    Ok(result)
}

// ---------------------------------------------------------------- chat guard

/// A parsed `/secret NAME VALUE [target]` (the value stays here).
pub struct SecretCommand {
    pub name: Option<String>,
    pub value: Option<SecretValue>,
    pub target: Option<String>,
}

/// `/secret` (any case) followed by whitespace or the end.
pub fn is_secret_command(text: &str) -> bool {
    let t = text.trim_start();
    let Some(head) = t.get(..7) else {
        return false;
    };
    head.eq_ignore_ascii_case("/secret") && t[7..].chars().next().is_none_or(char::is_whitespace)
}

pub fn parse_secret_command(text: &str) -> SecretCommand {
    let rest = text.trim_start().get(7..).unwrap_or("");
    let mut parts = rest.split_whitespace();
    let name = parts.next().map(str::to_owned);
    let value = parts.next().map(|v| SecretValue::new(v.to_owned()));
    let target = parts.next().map(str::to_owned);
    SecretCommand {
        name,
        value,
        target,
    }
}

/// The language of Lucina's secret cards: the owner's (Italian by default).
pub fn lang_of(text: &str) -> &'static str {
    crate::rich::guess_language(text, text)
}

/// Lucina's fixed texts for the secret cards, per language.
struct Texts {
    save: &'static str,
    add: &'static str,
    no: &'static str,
    save_n: &'static str,
    create: &'static str,
    not_now: &'static str,
    it_word: &'static str,
    held: &'static str,
    usage: &'static str,
    offer: &'static str,
    chars: &'static str,
    project: &'static str,
    chat_app: &'static str,
}

fn texts(lang: &str) -> Texts {
    match lang {
        "en" => Texts {
            save: "Save as secret",
            add: "Add secret",
            no: "No",
            save_n: "Save secret {n}",
            create: "Create project",
            not_now: "Not now",
            it_word: "it",
            held: "🔐 I didn't read or store that. Tap \"Save as secret\" to pick where `{name}` goes (I keep the value for 10 minutes, in memory only).",
            usage: "🔐 Use /secret NAME VALUE [project/env/path or alias], or open \"Add secret\" in the app.",
            offer: "🔐 Your message had something that looks like a secret ({what}). I hid it from my memory and from the model. Save it as a secret?",
            chars: "characters",
            project: "Create the Infisical project \"{name}\"? Tap to confirm in the app. It gets no access for agents or services until you say so.",
            chat_app: " The card is in the Secretary app. This chat app keeps your message: delete it there.",
        },
        "da" => Texts {
            save: "Gem som hemmelighed",
            add: "Tilføj hemmelighed",
            no: "Nej",
            save_n: "Gem hemmelighed {n}",
            create: "Opret projekt",
            not_now: "Ikke nu",
            it_word: "den",
            held: "🔐 Jeg har hverken læst eller gemt det. Tryk \"Gem som hemmelighed\" for at vælge, hvor `{name}` skal hen (jeg holder værdien i 10 minutter, kun i hukommelsen).",
            usage: "🔐 Brug /secret NAVN VÆRDI [projekt/miljø/sti eller alias], eller åbn \"Tilføj hemmelighed\" i appen.",
            offer: "🔐 Din besked indeholdt noget, der ligner en hemmelighed ({what}). Jeg har skjult det for min hukommelse og for modellen. Skal jeg gemme det som hemmelighed?",
            chars: "tegn",
            project: "Skal jeg oprette Infisical-projektet \"{name}\"? Bekræft i appen. Agenter og tjenester får ingen adgang, før du siger til.",
            chat_app: " Kortet ligger i Secretary-appen. Denne chat-app gemmer din besked: slet den der.",
        },
        _ => Texts {
            save: "Salva come segreto",
            add: "Aggiungi segreto",
            no: "No",
            save_n: "Salva segreto {n}",
            create: "Crea progetto",
            not_now: "Non ora",
            it_word: "il segreto",
            held: "🔐 Non l'ho letto né salvato. Tocca \"Salva come segreto\" per scegliere dove va `{name}` (tengo il valore 10 minuti, solo in memoria).",
            usage: "🔐 Usa /secret NOME VALORE [progetto/ambiente/percorso o alias], oppure apri \"Aggiungi segreto\" nell'app.",
            offer: "🔐 Nel tuo messaggio c'era qualcosa che sembra un segreto ({what}). L'ho nascosto alla mia memoria e al modello. Lo salvo come segreto?",
            chars: "caratteri",
            project: "Creo il progetto Infisical \"{name}\"? Confermalo nell'app. Agenti e servizi non ci avranno accesso finché non lo dici tu.",
            chat_app: " La scheda è nell'app Secretary. Quest'app di chat conserva il tuo messaggio: cancellalo lì.",
        },
    }
}

/// The note added to a `/secret` answer sent to a chat app (Telegram, Discord).
pub fn chat_app_note(lang: &str) -> &'static str {
    texts(lang).chat_app
}

fn save_action(label: &str, held: &str, name: Option<&str>, target: Option<&str>) -> Value {
    let mut payload = json!({"mode": "save", "held": held});
    if let Some(n) = name {
        payload["name"] = json!(n);
    }
    if let Some(t) = target {
        payload["target"] = json!(t);
    }
    json!({"id": "save-secret", "label": label, "emoji": "🔐", "kind": "secret", "payload": payload})
}

/// The answer to a `/secret` message (before any sink): the reply and the
/// id of Lucina's card. `timeline`: where the card goes (the owner's
/// timeline, so the app shows it).
pub fn answer_secret_command(
    st: &crate::AppState,
    text: &str,
    timeline: &TimelineStore,
) -> (String, Option<String>) {
    let cmd = parse_secret_command(text);
    let tx = texts(lang_of(text.trim_start().get(7..).unwrap_or("")));
    let name_ok = cmd.name.as_deref().is_some_and(|n| {
        n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
    });
    let out = match &cmd.value {
        Some(v) => {
            let held = st.held_secrets.hold(v.expose_secret());
            let name = cmd.name.as_deref().filter(|_| name_ok);
            let reply = tx.held.replace("{name}", name.unwrap_or(tx.it_word));
            let meta = rich_meta(
                vec![reply.clone()],
                vec![save_action(tx.save, &held, name, cmd.target.as_deref())],
            );
            let id = lucina_item(timeline, &reply, "secret_offer", meta);
            (reply, id)
        }
        None => {
            let reply = tx.usage.to_owned();
            let meta = rich_meta(
                vec![reply.clone()],
                vec![
                    json!({"id": "add-secret", "label": tx.add, "emoji": "🔐", "kind": "secret", "payload": {"mode": "save"}}),
                ],
            );
            let id = lucina_item(timeline, &reply, "secret_offer", meta);
            (reply, id)
        }
    };
    tracing::info!(
        has_value = cmd.value.is_some(),
        "secret command answered before the planner"
    );
    out
}

/// Mask token shapes in a normal message. Returns the masked text and, when
/// something was masked, the held ids with a short hint (`sk-…`, 51 chars).
pub fn mask_message(st: &crate::AppState, text: &str) -> (String, Vec<(String, String)>) {
    let spans = lucina_secrets::redact::detect(text);
    if spans.is_empty() {
        return (lucina_secrets::redactor().redact(text).into_owned(), vec![]);
    }
    let mut held = Vec::new();
    for d in spans.iter().take(3) {
        let v = &text[d.start..d.end];
        let prefix: String = v.chars().take(3).collect();
        let hint = format!("{prefix}…, {}", v.chars().count());
        held.push((st.held_secrets.hold(v), hint));
    }
    (lucina_secrets::redactor().redact(text).into_owned(), held)
}

/// After a turn whose message had tokens: Lucina's "Save as secret?" card.
pub fn offer_held(timeline: &TimelineStore, held: &[(String, String)], lang: &str) {
    if held.is_empty() {
        return;
    }
    let tx = texts(lang);
    let what = held
        .iter()
        .map(|(_, h)| format!("{h} {}", tx.chars))
        .collect::<Vec<_>>()
        .join("; ");
    let reply = tx.offer.replace("{what}", &what);
    let mut actions: Vec<Value> = held
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, (id, _))| {
            let mut a = save_action(tx.save, id, None, None);
            a["id"] = json!(format!("save-secret-{}", i + 1));
            if held.len() > 1 {
                a["label"] = json!(tx.save_n.replace("{n}", &(i + 1).to_string()));
            }
            a
        })
        .collect();
    actions.push(json!({"id": "dismiss-secret", "label": tx.no, "kind": "dismiss", "payload": {}}));
    let _ = lucina_item(
        timeline,
        &reply,
        "secret_offer",
        rich_meta(vec![reply.clone()], actions),
    );
}

/// "crea un progetto Infisical per X" / "create an Infisical project called X":
/// the project name, when the message asks for that.
pub fn project_request(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if !lower.contains("infisical") {
        return None;
    }
    let asks = ["crea", "create", "make", "new", "nuovo", "opret", "lav"]
        .iter()
        .any(|w| lower.split(|c: char| !c.is_alphanumeric()).any(|t| t == *w));
    let project_word = ["progetto", "project", "projekt"]
        .iter()
        .any(|w| lower.contains(w));
    if !asks || !project_word {
        return None;
    }
    // The name: after "per", "for", "called", "chiamato", "named", "til", "kaldet".
    let words: Vec<&str> = text.split_whitespace().collect();
    let markers = [
        "per", "for", "called", "chiamato", "named", "nominato", "til", "kaldet",
    ];
    let i = words.iter().rposition(|w| {
        markers.contains(
            &w.to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric()),
        )
    })?;
    let name = words[i + 1..]
        .join(" ")
        .trim_matches(|c: char| {
            c == '"'
                || c == '\''
                || c == '`'
                || c == '.'
                || c == '?'
                || c == '!'
                || c == '«'
                || c == '»'
        })
        .trim()
        .to_owned();
    let n = name.chars().count();
    ((1..=64).contains(&n) && !name.chars().any(char::is_control)).then_some(name)
}

/// The card for [`project_request`]: the reply and the card's item id.
pub fn answer_project_request(
    timeline: &TimelineStore,
    name: &str,
    lang: &str,
) -> (String, Option<String>) {
    let tx = texts(lang);
    let reply = tx.project.replace("{name}", name);
    let meta = rich_meta(
        vec![reply.clone()],
        vec![
            json!({"id": "create-project", "label": tx.create, "emoji": "🗂️", "kind": "secret",
                   "payload": {"mode": "project", "projectName": name}}),
            json!({"id": "dismiss-project", "label": tx.not_now, "kind": "dismiss", "payload": {}}),
        ],
    );
    let id = lucina_item(timeline, &reply, "secret_offer", meta);
    (reply, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_command_shapes() {
        for yes in ["/secret", "/secret A b", "  /SECRET\tA b c", "/Secret\nX"] {
            assert!(is_secret_command(yes), "{yes}");
        }
        for no in [
            "/secrets",
            "/secretary",
            "secret A b",
            "hello /secret",
            "/se",
            "",
        ] {
            assert!(!is_secret_command(no), "{no}");
        }
        let c = parse_secret_command("/secret POKO_KEY sk-abc poko/prod/Pokobot");
        assert_eq!(c.name.as_deref(), Some("POKO_KEY"));
        assert_eq!(c.value.unwrap().expose_secret(), "sk-abc");
        assert_eq!(c.target.as_deref(), Some("poko/prod/Pokobot"));
        let c = parse_secret_command("/secret");
        assert!(c.name.is_none() && c.value.is_none());
    }

    #[test]
    fn project_requests() {
        assert_eq!(
            project_request("crea un progetto Infisical per Poko Shorts").as_deref(),
            Some("Poko Shorts")
        );
        assert_eq!(
            project_request("Create an Infisical project called \"CardRails\".").as_deref(),
            Some("CardRails")
        );
        assert_eq!(
            project_request(
                "Lucina, puoi creare... no: crea nuovo progetto infisical chiamato tradera?"
            )
            .as_deref(),
            Some("tradera")
        );
        assert_eq!(
            project_request("opret et Infisical projekt til Homelab").as_deref(),
            Some("Homelab")
        );
        for no in [
            "come funziona il progetto Infisical?",
            "crea un promemoria per domani",
            "list my Infisical projects",
            "create a project for the gym",
        ] {
            assert_eq!(project_request(no), None, "{no}");
        }
    }

    #[test]
    fn held_values_expire_and_are_bounded() {
        let h = HeldSecrets::default();
        let id = h.hold("sk-value-1");
        assert_eq!(h.get(&id).unwrap().expose_secret(), "sk-value-1");
        for i in 0..(HOLD_MAX + 5) {
            h.hold(&format!("v{i}"));
        }
        assert_eq!(h.len(), HOLD_MAX);
        assert!(h.get(&id).is_none(), "the oldest went first");
        let id = h.hold("x");
        h.drop_id(&id);
        assert!(h.get(&id).is_none());
    }

    #[test]
    fn receipts_name_no_value() {
        let r = json!({"key": "POKO_KEY", "project": {"id": "p1", "name": "Poko"}, "environment": "prod",
                       "path": "/Pokobot", "action": "created", "fingerprint": "ab12cd34"});
        assert_eq!(
            receipt_text(&r),
            "Saved `POKO_KEY` in Poko / prod / /Pokobot (fingerprint ab12cd34)"
        );
    }
}
