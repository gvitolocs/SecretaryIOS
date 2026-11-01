//! "Add secret": the owner stores secrets (and creates projects) in Infisical
//! from the apps, and Lucina keeps every value away from her sinks.
//!
//! - `POST /api/secretary/secrets/grant {grant, value? | held?}`: relays an
//!   owner grant (signed by secretary-server after the owner confirmed on a
//!   device) to infisical-brokerd as op `grant`. Lucina cannot make a grant;
//!   she only passes it on with the value. After a save she writes a receipt
//!   (names and fingerprint, never the value) to the timeline and learns the
//!   value in the redactor under its name. Every relayed grant leaves one
//!   line in [`grants_file`] (what it named and how it went, never the value,
//!   the grant itself or a token): his own acts, evidence for decisions.
//! - **`/secret NAME VALUE [target]`** in any chat is answered here, before
//!   the model, the timeline, Honcho, recent turns and the replay cache: the
//!   value is held in memory ([`HeldSecrets`], 10 minutes, zeroized) and
//!   Lucina posts a card that opens the app's sheet with it.
//! - **Token shapes in a normal message** (the redactor's detector) are
//!   masked in the turn text before anything runs; each one is held the
//!   same way and Lucina offers "Save as secret?".
//! - **"crea un progetto Infisical per X"** gets a card that opens the
//!   sheet's new-project step; nothing is created without the owner's tap.
//! - `POST /api/secretary/secrets/request {name, project, environment, path?,
//!   reason, from?, new_project?}`: an agent (secretary-mcp `secret_request`)
//!   asks the owner for a secret **by name**. Lucina posts a card that opens
//!   the sheet prefilled; he pastes the value and saves it the usual way (a
//!   grant). A request that carries a value, or a reason that looks like one,
//!   is refused: the value never passes through the agent or the timeline.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
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

/// `LUCINA_SECRET_GRANTS_FILE`, default `<data_dir>/secret-grants.jsonl`: one line per relayed grant.
pub fn grants_file(data_dir: &Path) -> PathBuf {
    std::env::var("LUCINA_SECRET_GRANTS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("secret-grants.jsonl"))
}

/// What a grant names, for [`grants_file`]: the action and its names only (project, environment,
/// path, key, the caller it is for). Never the value, its hash, a held id, the signature or a token.
fn grant_record(grant: &str) -> Value {
    let payload = grant
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null);
    let text = |key: &str| payload[key].as_str().map(|s| s.chars().take(256).collect::<String>());
    let mut out = json!({"ts": lucina_store::timeutil::now_iso(), "op": text("act").unwrap_or_else(|| "unknown".into())});
    for key in ["project", "environment", "path", "key", "project_name", "slug"] {
        if let Some(v) = text(key) {
            out[key] = json!(v);
        }
    }
    if let Some(caller) = ["caller", "service", "grantee"].iter().find_map(|k| text(k)) {
        out["caller"] = json!(caller);
    }
    out
}

/// Append one line to [`grants_file`] (mode 0600); a failure is logged, never the caller's problem.
fn record_grant(path: &Path, line: &Value) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut bytes = serde_json::to_vec(line)?;
        bytes.push(b'\n');
        f.write_all(&bytes)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e.kind(), "secret grant record not written");
    }
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
    let mut record = grant_record(&grant);
    let mut req = json!({"token": token.expose_secret(), "op": "grant", "grant": grant});
    if let Some(v) = &value {
        req["value"] = json!(v.expose_secret());
    }
    let answer = broker_call(&st.cfg.broker.socket, req).await;
    match &answer {
        Ok(_) => record["outcome"] = json!("ok"),
        Err((code, _)) => {
            record["outcome"] = json!("error");
            record["error"] = json!(code);
        }
    }
    record_grant(&grants_file(&st.cfg.data_dir), &record);
    let result = answer.map_err(|(c, m)| broker_error(&c, &m))?;
    if let Some(list) = result.get("projects").and_then(Value::as_array) {
        remember_projects(list, true);
    }
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
        remember_projects(std::slice::from_ref(p), false);
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

// ---------------------------------------------------------------- agent requests

/// Project keys (lowercase id, name, slug, alias) from the last catalog the
/// owner's sheet loaded through Lucina: names only, memory only. `None`
/// until one passes; then an agent's request knows whether its project exists.
static KNOWN_PROJECTS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Requests posted in the last [`REQUEST_REPEAT`]: (when, name/project/env/path, card id).
static RECENT_REQUESTS: Mutex<Vec<(Instant, String, Option<String>)>> = Mutex::new(Vec::new());
/// A repeat of the same request inside this window answers with the first card.
const REQUEST_REPEAT: Duration = Duration::from_secs(600);

/// The fields a request may carry. Anything else is refused, above all a value.
const REQUEST_FIELDS: [&str; 8] = [
    "name",
    "project",
    "environment",
    "path",
    "reason",
    "from",
    "asking_client",
    "new_project",
];

/// `catalog`: the whole list (replaces what was known); else one project to add.
fn remember_projects(list: &[Value], catalog: bool) {
    let mut known = KNOWN_PROJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let keys = known.get_or_insert_with(Vec::new);
    if catalog {
        keys.clear();
    }
    for p in list {
        for k in ["id", "name", "slug", "alias"] {
            if let Some(v) = p[k].as_str().filter(|v| !v.is_empty()) {
                keys.push(v.to_lowercase());
            }
        }
    }
}

/// Whether `project` is in the last catalog; `None` when no catalog passed yet.
fn project_known(project: &str) -> Option<bool> {
    let known = KNOWN_PROJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let p = project.to_lowercase();
    known.as_ref().map(|keys| keys.contains(&p))
}

/// A secret name or a project / environment slug: `[A-Za-z0-9_.@-]`, 1-`max`.
fn slug_ok(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// `/`, `/a/b`, `a/b/` -> `/a/b` (segments like names, not `.`/`..`), at most 512 bytes.
fn request_folder(path: &str) -> Option<String> {
    let t = path.trim().trim_matches('/');
    if t.is_empty() {
        return Some("/".into());
    }
    let segs: Vec<&str> = t.split('/').collect();
    let ok = t.len() < 512 && segs.iter().all(|s| slug_ok(s, 128) && *s != "." && *s != "..");
    ok.then(|| format!("/{}", segs.join("/")))
}

/// What an agent asks for: names, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequest {
    pub name: String,
    pub project: String,
    pub environment: String,
    pub path: String,
    pub reason: String,
    /// Who asks: the MCP client, with the session it named.
    pub who: String,
    pub new_project: bool,
}

fn request_error(code: &str, message: &str) -> ApiError {
    ApiError::new(400, code, message)
}

/// One line of at most `max` characters (whitespace collapsed); `None` when empty.
fn request_text(body: &Value, key: &str, max: usize) -> Result<Option<String>, ApiError> {
    let Some(v) = body.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let s = v
        .as_str()
        .ok_or_else(|| request_error("bad_request", &format!("{key} must be a string")))?;
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        return Err(request_error(
            "bad_request",
            &format!("{key}: at most {max} characters"),
        ));
    }
    // A token shape, or a value Lucina already knows: the agent pasted a secret.
    if !lucina_secrets::redact::detect(&s).is_empty()
        || lucina_secrets::redactor().redact(&s).as_ref() != s
    {
        return Err(request_error(
            "value_refused",
            &format!("{key} looks like it contains a secret: a request names the secret, the owner pastes the value in the app"),
        ));
    }
    Ok((!s.is_empty()).then_some(s))
}

/// Check a request body. Refuses unknown fields (a value above all) and token-like text.
pub fn parse_request(body: &Value) -> Result<SecretRequest, ApiError> {
    let Value::Object(m) = body else {
        return Err(request_error("bad_request", "the body must be a JSON object"));
    };
    if let Some(k) = m.keys().find(|k| !REQUEST_FIELDS.contains(&k.as_str())) {
        let lower = k.to_lowercase();
        let valueish = ["value", "secret", "password", "passphrase", "psk", "token", "held", "key"]
            .iter()
            .any(|w| lower.contains(w));
        return Err(if valueish {
            request_error(
                "value_refused",
                "a secret request carries names only: the owner pastes the value in the app",
            )
        } else {
            request_error("bad_request", &format!("unknown field {k:?}"))
        });
    }
    let slug = |key: &str, max: usize| -> Result<String, ApiError> {
        body[key].as_str().map(str::trim).filter(|s| slug_ok(s, max)).map(str::to_owned).ok_or_else(|| {
            request_error(
                "bad_request",
                &format!("{key}: required, 1-{max} of letters, digits, _ . @ -"),
            )
        })
    };
    let name = slug("name", 256)?;
    let project = slug("project", 128)?;
    let environment = slug("environment", 128)?;
    let path = match body.get("path").filter(|v| !v.is_null()) {
        None => "/".to_owned(),
        Some(v) => v.as_str().and_then(request_folder).ok_or_else(|| {
            request_error("bad_request", "path: folders of letters, digits, _ . @ -")
        })?,
    };
    let reason = request_text(body, "reason", 500)?
        .ok_or_else(|| request_error("bad_request", "reason: required, why you need it (1-500 characters)"))?;
    let from = request_text(body, "from", 120)?;
    let client = request_text(body, "asking_client", 32)?.unwrap_or_else(|| "an agent".into());
    let who = match from {
        Some(f) if f != client => format!("{client} ({f})"),
        _ => client,
    };
    let new_project = match body.get("new_project") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(request_error("bad_request", "new_project must be true or false")),
    };
    Ok(SecretRequest {
        name,
        project,
        environment,
        path,
        reason,
        who,
        new_project,
    })
}

/// The card: Lucina's text and its buttons. `missing`: offer "Create project".
fn request_card(r: &SecretRequest, known: Option<bool>) -> (String, Vec<Value>, bool) {
    let place = format!("{} / {} / {}", r.project, r.environment, r.path);
    let missing = known == Some(false) || (known.is_none() && r.new_project);
    let mut text = format!(
        "🔐 {} asks for the secret `{}` in Infisical {place}.\nWhy: {}\nTap \"Add {}\" and paste the value: it goes straight to Infisical, never to {}, this chat or my memory.",
        r.who, r.name, r.reason, r.name, r.who
    );
    if known == Some(false) {
        text.push_str(&format!("\nThe project {} isn't in your Infisical yet: create it first.", r.project));
    } else if missing {
        text.push_str(&format!("\nThe project {} may not exist yet.", r.project));
    }
    // `target` too: sheets bundled before 2026-11-01 (the Mac app's) match `project` by id only,
    // but resolve a target by slug or name; newer sheets ignore it when `project` is set.
    let target = format!("{}/{}{}", r.project, r.environment, if r.path == "/" { "" } else { &r.path });
    let mut actions = vec![json!({
        "id": "secret-request",
        "label": format!("Add {}", r.name),
        "emoji": "🔐",
        "kind": "secret",
        "payload": {"mode": "save", "name": r.name, "project": r.project,
                    "environment": r.environment, "path": r.path, "target": target},
    })];
    if missing {
        actions.push(json!({
            "id": "secret-request-project",
            "label": format!("Create project {}", r.project),
            "emoji": "🗂️",
            "kind": "secret",
            "payload": {"mode": "project", "projectName": r.project},
        }));
    }
    actions.push(json!({"id": "secret-request-dismiss", "label": "Not now", "kind": "dismiss", "payload": {}}));
    (text, actions, missing)
}

/// `POST /api/secretary/secrets/request`: post the card to the owner's chat and push it.
pub async fn request(st: &crate::AppState, body: &Value) -> Result<Value, ApiError> {
    let r = parse_request(body)?;
    let key = format!("{}\n{}\n{}\n{}", r.name, r.project.to_lowercase(), r.environment, r.path);
    let known = project_known(&r.project);
    let answer = |status: &str, id: &Option<String>, missing: bool| {
        json!({"status": status, "itemId": id, "name": r.name, "project": r.project,
               "environment": r.environment, "path": r.path, "projectKnown": known,
               "createProject": missing})
    };
    {
        let mut recent = RECENT_REQUESTS.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|(at, _, _)| at.elapsed() < REQUEST_REPEAT);
        if let Some((_, _, id)) = recent.iter().find(|(_, k, _)| *k == key) {
            return Ok(answer("already_asked", id, false));
        }
    }
    let (text, actions, missing) = request_card(&r, known);
    let meta = rich_meta(vec![text.clone()], actions);
    let id = lucina_item(&st.timeline, &text, "secret_request", meta);
    RECENT_REQUESTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((Instant::now(), key, id.clone()));
    tracing::info!(name = %r.name, project = %r.project, environment = %r.environment,
                   who = %r.who, "agent secret request posted");
    let mut push = json!({
        "category": "lucina_message",
        "title": format!("Secret requested: {}", r.name),
        "body": format!("{} asks for {} in {}/{}: {}", r.who, r.name, r.project, r.environment, r.reason),
        "route": "/",
        "channel": "web",
        "data": {"kind": "secret_request", "name": r.name, "project": r.project, "environment": r.environment},
    });
    if let Some(id) = &id {
        // The timeline ingest pushes the same item: one dedup key for both.
        push["timelineId"] = json!(id);
        push["dedupKey"] = json!(format!("tl:{id}"));
    }
    lucina_secrets::redactor().redact_json(&mut push);
    let pushed = crate::bridges::notify(&st.secretary, &push).await;
    if !pushed.ok {
        tracing::warn!(status = pushed.status, code = %pushed.code, "secret request push failed (the timeline item still notifies)");
    }
    Ok(answer("asked", &id, missing))
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

    #[test]
    fn grant_records_name_no_value() {
        let payload = json!({"v": 1, "aud": "infisical-brokerd", "jti": "abc123", "iat": 1, "exp": 91,
            "sub": "device:dev_1", "act": "put_secret", "project": "lucina", "environment": "prod",
            "path": "/", "key": "JEV_API_KEY_LUCINA", "caller": "svc-gallery", "overwrite": false,
            "value_sha256": "5e8f", "held": "held-0123456789", "value": "test-secret-value",
            "signature": "test-signature", "token": "test-token"});
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let grant = format!("g1.{}.{}", b64(&serde_json::to_vec(&payload).unwrap()), b64(b"signature-bytes"));
        let r = grant_record(&grant);
        assert_eq!(r["op"], "put_secret");
        assert_eq!(r["key"], "JEV_API_KEY_LUCINA");
        assert_eq!(r["caller"], "svc-gallery");
        assert_eq!((r["project"].as_str(), r["environment"].as_str(), r["path"].as_str()), (Some("lucina"), Some("prod"), Some("/")));
        let line = r.to_string();
        for leak in ["value", "5e8f", "held", "jti", "abc123", "sub", "dev_1", "grant", "sig", "token", "test-secret-value", "test-signature", "test-token", &b64(b"signature-bytes")] {
            assert!(!line.contains(leak), "{leak} in {line}");
        }
        assert_eq!(grant_record("g1.not-base64!.x")["op"], "unknown");
    }

    #[test]
    fn grant_log_is_private_even_when_it_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret-grants.jsonl");
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        record_grant(&file, &json!({"op": "catalog", "outcome": "ok"}));
        record_grant(&file, &json!({"op": "put_secret", "outcome": "error"}));
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["outcome"], "error");
    }

    fn wifi_request() -> Value {
        json!({"name": "GBGO_WIFI_PSK", "project": "lucinaos", "environment": "prod",
               "reason": "The Galaxy Book Go image joins the home Wi-Fi at boot",
               "from": "gbgo-wifi", "asking_client": "claude-code"})
    }

    #[test]
    fn requests_take_names_and_refuse_values() {
        let r = parse_request(&wifi_request()).unwrap();
        assert_eq!((r.name.as_str(), r.project.as_str(), r.environment.as_str(), r.path.as_str()),
                   ("GBGO_WIFI_PSK", "lucinaos", "prod", "/"));
        assert_eq!(r.who, "claude-code (gbgo-wifi)");
        assert!(!r.new_project);
        let mut b = wifi_request();
        b["path"] = json!("gbgo/wifi/");
        assert_eq!(parse_request(&b).unwrap().path, "/gbgo/wifi");
        for field in ["value", "secretValue", "psk", "Password", "held", "api_key"] {
            let mut b = wifi_request();
            b[field] = json!("hunter2-hunter2");
            let e = parse_request(&b).unwrap_err();
            assert_eq!(e.code, "value_refused", "{field}");
            assert!(!e.message.contains("hunter2"), "{field}");
        }
        let mut b = wifi_request();
        b["extra"] = json!(1);
        assert_eq!(parse_request(&b).unwrap_err().code, "bad_request");
        // A token (or a value Lucina knows) in the reason: the agent pasted a secret.
        let mut b = wifi_request();
        b["reason"] = json!("use sk-abcdefghijklmnop0123456789 for it");
        let e = parse_request(&b).unwrap_err();
        assert_eq!(e.code, "value_refused");
        assert!(!e.message.contains("sk-abc"));
        for (k, v) in [("name", json!("a b")), ("name", json!("A/B")), ("project", json!("")), ("environment", json!(3)),
                       ("path", json!("/a/../b")), ("reason", json!("  ")), ("reason", json!("x".repeat(501))),
                       ("new_project", json!("yes"))] {
            let mut b = wifi_request();
            b[k] = v;
            assert_eq!(parse_request(&b).unwrap_err().code, "bad_request", "{k}");
        }
        let mut b = wifi_request();
        b.as_object_mut().unwrap().remove("asking_client");
        b.as_object_mut().unwrap().remove("from");
        assert_eq!(parse_request(&b).unwrap().who, "an agent");
    }

    #[test]
    fn request_cards_open_the_sheet_prefilled() {
        let r = parse_request(&wifi_request()).unwrap();
        let (text, actions, missing) = request_card(&r, Some(true));
        assert!(!missing && actions.len() == 2, "{actions:?}");
        assert!(text.contains("claude-code (gbgo-wifi) asks for the secret `GBGO_WIFI_PSK` in Infisical lucinaos / prod / /"), "{text}");
        let meta = rich_meta(vec![text], actions).expect("a valid rich message");
        let rich = serde_json::to_value(&meta).unwrap();
        let a = &rich["actions"][0];
        assert_eq!(a["kind"], "secret");
        assert_eq!(a["label"], "Add GBGO_WIFI_PSK");
        assert_eq!(a["payload"], json!({"mode": "save", "name": "GBGO_WIFI_PSK", "project": "lucinaos",
                                         "environment": "prod", "path": "/", "target": "lucinaos/prod"}));
        let mut deep = r.clone();
        deep.path = "/gbgo/wifi".into();
        let (_, actions, _) = request_card(&deep, Some(true));
        assert_eq!(actions[0]["payload"]["target"], "lucinaos/prod/gbgo/wifi");
        // A project the last catalog didn't list, or one the agent says is new: "Create project".
        let (text, actions, missing) = request_card(&r, Some(false));
        assert!(missing && text.contains("isn't in your Infisical yet"));
        assert_eq!(actions[1]["payload"], json!({"mode": "project", "projectName": "lucinaos"}));
        assert_eq!(request_card(&r, None).1.len(), 2);
        let mut r2 = r.clone();
        r2.new_project = true;
        assert!(request_card(&r2, None).2);
        assert!(!request_card(&r2, Some(true)).2);
    }

    #[test]
    fn catalogs_teach_project_names() {
        remember_projects(&[json!({"id": "a1b2", "name": "LucinaOS", "slug": "lucinaos-x1", "alias": null})], true);
        assert_eq!(project_known("lucinaos"), Some(true));
        assert_eq!(project_known("LUCINAOS-X1"), Some(true));
        assert_eq!(project_known("poko"), Some(false));
        remember_projects(&[json!({"id": "c3", "name": "Poko"})], false);
        assert_eq!(project_known("poko"), Some(true));
        assert_eq!(project_known("lucinaos"), Some(true));
    }

    #[test]
    fn mail_and_outlook_are_not_secrets() {
        for t in [
            "Quick check only: has Giuseppe already SENT an email this morning (28 Oct 2026) from givi@au.dk to Marianne/Arkaitz about CRAFT self-assessment / \"Ecosystem self assessment-tool for replication\"? If you can see Outlook Sent or notes, say yes/no. If you cannot check, say cannot check. One short answer.",
            "Explode outlook in your sandbox",
            "Explore Outlook in your sandbox",
        ] {
            assert!(!is_secret_command(t), "{t}");
            assert_eq!(project_request(t), None, "{t}");
            assert!(lucina_secrets::reveal::classify(t).is_none(), "{t}");
            assert!(lucina_secrets::redact::detect(t).is_empty(), "{t}");
            assert_eq!(lucina_secrets::redactor().redact(t).as_ref(), t, "{t}");
        }
    }
}
