//! The owner's queue of secret requests: "enter the secret for this", paste, OK, saved, next.
//!
//! A request (an agent's `secret_request`, Lucina's own tool, or the owner's "salva un segreto")
//! names a secret and never carries a value. **Only one request is on screen at a time**: the
//! active one has a card in the owner's timeline, and the next card is posted when the active one
//! is saved, dismissed or expires. So clients stay simple: each card is one timeline item, and old
//! clients (the Mac app's bundled sheet) still see a plain `secret` action they can open.
//!
//! The queue lives in `<data_dir>/secret-requests.json` (mode 0600, names only), so a restart
//! loses nothing: the active card is still in the timeline and the queued ones still follow. A
//! value the owner had pasted but not sent lived only on his device and is not touched by this.
//!
//! The status of a card (`GET /api/secretary/secrets/requests`) is what clients draw: an open field
//! for the active request, "saved" with the name and fingerprint, or "not asked any more".
//!
//! A save closes a request by its id (new clients send it), else by its names, else by the name alone:
//! the owner may save a secret somewhere other than where the request said, and it is still the secret
//! that was asked for, and the receipt says where it went. At the first look after a start, a receipt
//! already in the timeline closes what is still open under that name (saves from before this rule).
//! Old sheets can't report "Not now": the card then stays active until another client dismisses it
//! or [`REQUEST_TTL`] passes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_store::timeline::TimelineStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::ApiError;
use crate::bridges::Secretary;
use crate::owner_secrets::{lucina_item, parse_request, project_known, request_card, rich_meta, SecretRequest};

/// A repeat of a request closed this recently answers with the first card.
pub const REQUEST_REPEAT: Duration = Duration::from_secs(600);
/// How long a request stays open (active or queued) with nobody answering.
pub const REQUEST_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Open requests at most: an agent in a loop can't fill the owner's queue.
pub const MAX_OPEN: usize = 30;
/// Closed requests kept for the cards already in the timeline (the oldest go first).
pub const MAX_CLOSED: usize = 200;
const FILE: &str = "secret-requests.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Active,
    Saved,
    Dismissed,
    Expired,
}

impl Status {
    fn open(self) -> bool {
        matches!(self, Status::Queued | Status::Active)
    }
}

/// One request: names and what the card says, never a value. `name`, `project` and `environment`
/// are empty only when the owner asked in words without saying them: his card asks for them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub project: String,
    pub environment: String,
    pub path: String,
    pub reason: String,
    pub who: String,
    #[serde(default)]
    pub new_project: bool,
    pub status: Status,
    pub asked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// What the bubble says instead of the agent's text (the owner's own request, his language).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bubble: Option<String>,
    /// After a save: the broker's first 8 hex of the value's sha256, and created / updated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl Entry {
    fn request(&self) -> SecretRequest {
        SecretRequest {
            name: self.name.clone(),
            project: self.project.clone(),
            environment: self.environment.clone(),
            path: self.path.clone(),
            reason: self.reason.clone(),
            who: self.who.clone(),
            new_project: self.new_project,
        }
    }

    fn key(&self) -> String {
        request_key(&self.request())
    }
}

fn request_key(r: &SecretRequest) -> String {
    format!("{}\n{}\n{}\n{}", r.name, r.project.to_lowercase(), r.environment, r.path)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn random_id() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).expect("OS random");
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("sr-{hex}")
}

/// The answer to one request.
#[derive(Debug, Clone)]
pub struct Asked {
    pub entry: Entry,
    /// `asked` (its card was just posted), `queued` or `already_asked`.
    pub status: &'static str,
    pub project_known: Option<bool>,
    pub create_project: bool,
    /// Requests ahead of this one.
    pub ahead: usize,
    /// The posted card's message (the chat response carries it).
    pub rich: Option<Value>,
}

impl Asked {
    pub fn to_json(&self) -> Value {
        let e = &self.entry;
        json!({"status": self.status, "requestId": e.id, "itemId": e.item_id, "name": e.name, "project": e.project,
               "environment": e.environment, "path": e.path, "projectKnown": self.project_known,
               "createProject": self.create_project, "ahead": self.ahead})
    }
}

pub struct SecretRequests {
    file: PathBuf,
    timeline: Arc<TimelineStore>,
    secretary: Secretary,
    /// Loaded on first use.
    entries: Mutex<Option<Vec<Entry>>>,
    /// Receipts in the timeline are read once per process.
    receipts_checked: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for SecretRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretRequests").field("file", &self.file).finish_non_exhaustive()
    }
}

/// What a state change leaves to do outside the lock.
#[derive(Default)]
struct Next {
    posted: Option<Entry>,
}

impl SecretRequests {
    pub fn new(data_dir: &Path, timeline: Arc<TimelineStore>, secretary: Secretary) -> Self {
        Self {
            file: data_dir.join(FILE),
            timeline,
            secretary,
            entries: Mutex::new(None),
            receipts_checked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Vec<Entry>) -> T) -> T {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.get_or_insert_with(|| self.load());
        f(entries)
    }

    fn load(&self) -> Vec<Entry> {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                // Said plainly: the cards still in the timeline then read "not asked any more".
                tracing::warn!(path = %self.file.display(), error = %e, "secret request queue unreadable: starting empty");
                Vec::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(path = %self.file.display(), error = %e.kind(), "secret request queue unreadable: starting empty");
                Vec::new()
            }
        }
    }

    /// Whole file through a private temporary: a crash never leaves half a queue.
    fn persist(&self, entries: &[Entry]) {
        // Dismissed, expired or saved elsewhere: no timeline item says so, the cards are told here.
        crate::card_state::changed();
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.file.with_extension("json.tmp");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec(entries)?)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.file)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %self.file.display(), error = %e.kind(), "secret request queue not written");
        }
    }

    /// Post the card of the oldest queued request when nothing is active.
    fn advance(&self, entries: &mut [Entry], next: &mut Next) {
        if entries.iter().any(|e| e.status == Status::Active) {
            return;
        }
        if let Some(e) = entries.iter_mut().find(|e| e.status == Status::Queued) {
            self.post_card(e);
            next.posted = Some(e.clone());
        }
    }

    fn post_card(&self, e: &mut Entry) -> Option<Value> {
        let (text, mut actions, _) = request_card(&e.request(), project_known(&e.project));
        let text = e.bubble.clone().unwrap_or(text);
        // New clients draw the card from the request's status; old ones just see `secret`.
        for a in &mut actions {
            if a["kind"] == "secret" && a["payload"]["mode"] == "save" {
                a["payload"]["request"] = json!(e.id);
            }
        }
        let meta = rich_meta(vec![text.clone()], actions);
        // The chat response carries the message at its top level, like any rich reply.
        let rich = meta.as_ref().and_then(|m| serde_json::to_value(m).ok()).map(|v| {
            let keep = ["format", "bubbles", "lang", "blocks", "actions"];
            Value::Object(v.as_object().into_iter().flatten().filter(|(k, _)| keep.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect())
        });
        e.item_id = lucina_item(&self.timeline, &text, "secret_request", meta);
        e.status = Status::Active;
        e.shown_at = Some(now());
        tracing::info!(name = %e.name, project = %e.project, environment = %e.environment, who = %e.who,
                       request = %e.id, "secret request card posted");
        rich
    }

    /// Close what waited too long, and forget the oldest closed ones.
    fn sweep(&self, entries: &mut Vec<Entry>, next: &mut Next) -> bool {
        let t = now();
        let ttl = REQUEST_TTL.as_secs() as i64;
        let mut changed = false;
        for e in entries.iter_mut().filter(|e| e.status.open()) {
            if t - e.shown_at.unwrap_or(e.asked_at) >= ttl {
                e.status = Status::Expired;
                e.closed_at = Some(t);
                changed = true;
            }
        }
        let closed = entries.iter().filter(|e| !e.status.open()).count();
        if closed > MAX_CLOSED {
            let mut drop = closed - MAX_CLOSED;
            entries.retain(|e| {
                if drop > 0 && !e.status.open() {
                    drop -= 1;
                    return false;
                }
                true
            });
            changed = true;
        }
        if changed {
            self.advance(entries, next);
        }
        changed
    }

    /// A card that followed another one: its push goes out on the side, the save doesn't wait.
    fn notify_later(&self, next: Next) {
        let Some(e) = next.posted else { return };
        let secretary = self.secretary.clone();
        tokio::spawn(async move { notify_card(&secretary, &e).await });
    }

    /// Ask the owner: a card now, or a place in the queue. `owner`: his own request, which may
    /// repeat one he closed a moment ago.
    pub async fn ask(&self, r: SecretRequest, bubble: Option<String>, owner: bool) -> Result<Asked, ApiError> {
        let known = if r.project.is_empty() { None } else { project_known(&r.project) };
        let missing = known == Some(false) || (known.is_none() && r.new_project);
        let key = request_key(&r);
        let mut next = Next::default();
        let (out, posted) = self.with(|entries| {
            let swept = self.sweep(entries, &mut next);
            let t = now();
            let repeat = entries.iter().rev().find(|e| {
                e.key() == key
                    && (e.status.open()
                        || (!owner && t - e.closed_at.unwrap_or(0) < REQUEST_REPEAT.as_secs() as i64))
            });
            if let Some(e) = repeat {
                let ahead = ahead_of(entries, &e.id);
                let a = Asked { entry: e.clone(), status: "already_asked", project_known: known, create_project: false, ahead, rich: None };
                if swept {
                    self.persist(entries);
                }
                return (Ok(a), false);
            }
            if entries.iter().filter(|e| e.status.open()).count() >= MAX_OPEN {
                if swept {
                    self.persist(entries);
                }
                return (Err(ApiError::new(429, "too_many_requests", "the owner already has too many secret requests open")), false);
            }
            let mut e = Entry {
                id: random_id(),
                name: r.name.clone(),
                project: r.project.clone(),
                environment: r.environment.clone(),
                path: r.path.clone(),
                reason: r.reason.clone(),
                who: r.who.clone(),
                new_project: r.new_project,
                status: Status::Queued,
                asked_at: t,
                shown_at: None,
                closed_at: None,
                item_id: None,
                bubble,
                fingerprint: None,
                action: None,
            };
            let free = !entries.iter().any(|x| x.status == Status::Active) && !entries.iter().any(|x| x.status == Status::Queued);
            let rich = free.then(|| self.post_card(&mut e)).flatten();
            let ahead = entries.iter().filter(|x| x.status.open()).count();
            entries.push(e.clone());
            self.persist(entries);
            let status = if free { "asked" } else { "queued" };
            (Ok(Asked { entry: e, status, project_known: known, create_project: missing, ahead, rich }), free)
        });
        self.notify_later(next);
        let out = out?;
        if posted {
            notify_card(&self.secretary, &out.entry).await;
        }
        Ok(out)
    }

    /// The owner's own "here, take this key": his card shows **now**, at the bottom of his chat.
    /// An open request for the same secret (by its names, else by its name) is brought down instead
    /// of asked again; another active card waits behind his (its old card item stays where it was).
    pub fn show_now(&self, r: SecretRequest, bubble: String) -> Result<Asked, ApiError> {
        let mut next = Next::default();
        let out = self.with(|entries| {
            self.sweep(entries, &mut next);
            let key = request_key(&r);
            let name = r.name.to_lowercase();
            let same = entries
                .iter()
                .rposition(|e| e.status.open() && e.key() == key)
                .or_else(|| entries.iter().rposition(|e| e.status.open() && !name.is_empty() && e.name.to_lowercase() == name));
            if same.is_none() && entries.iter().filter(|e| e.status.open()).count() >= MAX_OPEN {
                return Err(ApiError::new(429, "too_many_requests", "there are too many secret requests open"));
            }
            for e in entries.iter_mut().filter(|e| e.status == Status::Active) {
                e.status = Status::Queued;
            }
            let (i, status) = match same {
                Some(i) => (i, "shown"),
                None => {
                    entries.push(Entry {
                        id: random_id(),
                        name: r.name.clone(),
                        project: r.project.clone(),
                        environment: r.environment.clone(),
                        path: r.path.clone(),
                        reason: r.reason.clone(),
                        who: r.who.clone(),
                        new_project: r.new_project,
                        status: Status::Queued,
                        asked_at: now(),
                        shown_at: None,
                        closed_at: None,
                        item_id: None,
                        bubble: Some(bubble.clone()),
                        fingerprint: None,
                        action: None,
                    });
                    (entries.len() - 1, "asked")
                }
            };
            let e = &mut entries[i];
            // An agent's card keeps its own text; his card says what he asked for.
            if e.who.is_empty() {
                e.bubble = Some(bubble);
            }
            let rich = self.post_card(e);
            let e = e.clone();
            let ahead = entries.iter().filter(|x| x.status.open() && x.id != e.id).count();
            self.persist(entries);
            tracing::info!(name = %e.name, project = %e.project, environment = %e.environment, status, "owner's secret card shown");
            Ok(Asked { entry: e, status, project_known: None, create_project: false, ahead, rich })
        });
        self.notify_later(next);
        out
    }

    /// Where secrets already live, for picking a place: `(project, environment, path)` with the
    /// names asked or saved there, newest first. Names only.
    pub fn places(&self) -> Vec<(String, String, String, Vec<String>)> {
        self.with(|entries| {
            let mut out: Vec<(String, String, String, Vec<String>)> = Vec::new();
            for e in entries.iter().rev().filter(|e| !e.project.is_empty() && !e.environment.is_empty()) {
                let project = e.project.to_lowercase();
                match out.iter_mut().find(|p| p.0 == project && p.1 == e.environment && p.2 == e.path) {
                    Some(p) => {
                        if p.3.len() < 6 && !p.3.contains(&e.name) {
                            p.3.push(e.name.clone());
                        }
                    }
                    None => out.push((project, e.environment.clone(), e.path.clone(), vec![e.name.clone()])),
                }
            }
            out
        })
    }

    /// The open requests: `(name, project, environment, path, status, who)`.
    pub fn open_requests(&self) -> Vec<(String, String, String, String, &'static str, String)> {
        self.with(|entries| {
            entries
                .iter()
                .filter(|e| e.status.open())
                .map(|e| {
                    let s = if e.status == Status::Active { "active" } else { "queued" };
                    (e.name.clone(), e.project.clone(), e.environment.clone(), e.path.clone(), s, e.who.clone())
                })
                .collect()
        })
    }

    /// Where a name lives: an open request for it, else the newest closed one.
    fn place_of(&self, name: &str) -> Option<(String, String, String)> {
        let n = name.to_lowercase();
        self.with(|entries| {
            let found = |open: bool| {
                entries
                    .iter()
                    .rev()
                    .find(|e| e.status.open() == open && e.name.to_lowercase() == n && !e.project.is_empty() && !e.environment.is_empty())
                    .map(|e| (e.project.clone(), e.environment.clone(), e.path.clone()))
            };
            found(true).or_else(|| found(false))
        })
    }

    /// Every request, oldest first: what the cards draw.
    /// Open requests whose secret has a receipt in the timeline since they were asked.
    fn close_from_receipts(&self, entries: &mut [Entry]) -> bool {
        if !entries.iter().any(|e| e.status.open() && !e.name.is_empty()) {
            return false;
        }
        let Ok(items) = self.timeline.read_all() else { return false };
        let receipts: Vec<(i64, String)> = items
            .iter()
            .filter_map(|i| serde_json::from_str::<Value>(i.raw_value().get()).ok())
            .filter(|i| i["meta"]["kind"] == "secret_receipt")
            .filter_map(|i| {
                let at = chrono::DateTime::parse_from_rfc3339(i["ts"].as_str()?).ok()?.timestamp();
                Some((at, i["text"].as_str()?.to_owned()))
            })
            .collect();
        let mut changed = false;
        for e in entries.iter_mut().filter(|e| e.status.open() && !e.name.is_empty()) {
            let quoted = format!("`{}`", e.name);
            let Some((at, text)) = receipts.iter().find(|(at, text)| *at >= e.asked_at && text.contains(&quoted)) else {
                continue;
            };
            e.status = Status::Saved;
            e.closed_at = Some(*at);
            e.fingerprint = text
                .rsplit_once("(fingerprint ")
                .and_then(|(_, f)| f.split(')').next())
                .map(str::to_owned);
            e.action = Some(if text.contains("Updated `") { "updated" } else { "created" }.to_owned());
            tracing::info!(request = %e.id, name = %e.name, "secret request closed by a receipt in the timeline");
            changed = true;
        }
        changed
    }

    pub fn list(&self) -> Value {
        let mut next = Next::default();
        let out = self.with(|entries| {
            let healed = !self.receipts_checked.swap(true, std::sync::atomic::Ordering::Relaxed)
                && self.close_from_receipts(entries);
            if healed {
                self.advance(entries, &mut next);
            }
            if self.sweep(entries, &mut next) || healed {
                self.persist(entries);
            }
            let queued = entries.iter().filter(|e| e.status == Status::Queued).count();
            let active = entries.iter().find(|e| e.status == Status::Active).map(|e| e.id.clone());
            json!({"requests": entries, "active": active, "queued": queued})
        });
        self.notify_later(next);
        out
    }

    /// "Not now". Answering twice is fine; the next card follows.
    pub fn dismiss(&self, id: &str) -> Result<Value, ApiError> {
        let mut next = Next::default();
        let out = self.with(|entries| {
            let e = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| ApiError::new(404, "not_found", "no such secret request"))?;
            if e.status.open() {
                e.status = Status::Dismissed;
                e.closed_at = Some(now());
                tracing::info!(name = %e.name, request = %id, "secret request dismissed");
            }
            let status = e.status;
            self.advance(entries, &mut next);
            self.persist(entries);
            let next_item = next.posted.as_ref().and_then(|n| n.item_id.clone());
            Ok(json!({"status": status, "nextItemId": next_item}))
        });
        self.notify_later(next);
        out
    }

    /// A save went through: close its request (by id, else by the names the broker returned) and
    /// post the next card. `result` is the broker's answer: names and fingerprint, never a value.
    pub fn saved(&self, request: Option<&str>, result: &Value) -> Option<String> {
        let key = result["key"].as_str().unwrap_or("");
        let env = result["environment"].as_str().unwrap_or("");
        let path = result["path"].as_str().unwrap_or("/");
        let project_keys: Vec<String> = ["id", "name", "slug", "alias"]
            .iter()
            .filter_map(|k| result["project"][*k].as_str())
            .map(str::to_lowercase)
            .collect();
        let mut next = Next::default();
        let out = self.with(|entries| {
            let by_id = request.and_then(|id| {
                entries
                    .iter()
                    .position(|e| e.id == id && e.status.open() && (e.name.is_empty() || e.name == key))
            });
            let by_names = || {
                entries.iter().position(|e| {
                    e.status.open()
                        && e.name == key
                        && e.environment == env
                        && e.path == path
                        && project_keys.contains(&e.project.to_lowercase())
                })
            };
            let by_name_alone = || {
                let same = |e: &Entry| e.status.open() && !e.name.is_empty() && e.name == key;
                entries
                    .iter()
                    .position(|e| same(e) && e.status == Status::Active)
                    .or_else(|| entries.iter().position(same))
            };
            let i = by_id.or_else(by_names).or_else(by_name_alone)?;
            // Saved outside the project this request named: the save answers every request for that
            // name, or the same name asked for another project becomes a card for a secret he just saved.
            let elsewhere = !project_keys.contains(&entries[i].project.to_lowercase());
            let others: Vec<usize> = if elsewhere {
                let same = |(j, e): &(usize, &Entry)| *j != i && e.status.open() && e.name == key;
                entries.iter().enumerate().filter(same).map(|(j, _)| j).collect()
            } else {
                Vec::new()
            };
            for j in others {
                let e = &mut entries[j];
                e.status = Status::Saved;
                e.closed_at = Some(now());
                e.fingerprint = result["fingerprint"].as_str().map(str::to_owned);
                e.action = result["action"].as_str().map(str::to_owned);
                tracing::info!(request = %e.id, name = %e.name, "secret request closed by a save under another project");
            }
            let e = &mut entries[i];
            e.status = Status::Saved;
            e.closed_at = Some(now());
            e.name = key.to_owned();
            e.environment = env.to_owned();
            e.path = path.to_owned();
            if let Some(n) = result["project"]["name"].as_str() {
                e.project = n.to_owned();
            }
            e.fingerprint = result["fingerprint"].as_str().map(str::to_owned);
            e.action = result["action"].as_str().map(str::to_owned);
            self.advance(entries, &mut next);
            self.persist(entries);
            next.posted.as_ref().and_then(|n| n.item_id.clone())
        });
        self.notify_later(next);
        out
    }
}

fn ahead_of(entries: &[Entry], id: &str) -> usize {
    entries.iter().take_while(|e| e.id != id).filter(|e| e.status.open()).count()
}

/// The push for a card: names and the reason.
async fn notify_card(secretary: &Secretary, e: &Entry) {
    // The owner's own request: he is in the chat.
    if e.who.is_empty() {
        return;
    }
    let title = format!("Secret requested: {}", e.name);
    let body = format!("{} asks for {} in {}/{}: {}", e.who, e.name, e.project, e.environment, e.reason);
    let mut push = json!({
        "category": "lucina_message",
        "title": title,
        "body": body,
        "route": "/",
        "channel": "web",
        "data": {"kind": "secret_request", "name": e.name, "project": e.project, "environment": e.environment},
    });
    if let Some(id) = &e.item_id {
        // The timeline ingest pushes the same item: one dedup key for both.
        push["timelineId"] = json!(id);
        push["dedupKey"] = json!(format!("tl:{id}"));
    }
    lucina_secrets::redactor().redact_json(&mut push);
    let pushed = crate::bridges::notify(secretary, &push).await;
    if !pushed.ok {
        tracing::warn!(status = pushed.status, code = %pushed.code, "secret request push failed (the timeline item still notifies)");
    }
}

/// Lucina's `secret_tile`: the owner's own cards. A self-test turn gets a queue of its own, which
/// writes to the self-test conversation, so a test never shows in his chat or moves his cards.
pub struct OwnerCards {
    pub owner: Arc<SecretRequests>,
    pub selftest: Arc<SecretRequests>,
    /// Where his own secrets go when nothing names a place (`LUCINA_SECRET_KEEP_PLACE`).
    pub default_project: String,
    pub default_environment: String,
}

impl OwnerCards {
    pub fn queue(&self, session_id: &str) -> &Arc<SecretRequests> {
        if crate::selftest::is_session(session_id) {
            &self.selftest
        } else {
            &self.owner
        }
    }

    /// The names and places Lucina picks from, for her system text. Names only.
    pub fn guide(&self, session_id: &str) -> String {
        let q = self.queue(session_id);
        let mut out = String::new();
        let open = q.open_requests();
        if !open.is_empty() {
            out.push_str("Open secret cards (reuse the name when he means one of these): ");
            let list: Vec<String> = open
                .iter()
                .take(10)
                .map(|(n, p, e, f, s, who)| {
                    let by = if who.is_empty() { "him".to_owned() } else { who.clone() };
                    format!("{n} in {p}/{e}{} ({s}, asked by {by})", if f == "/" { "" } else { f })
                })
                .collect();
            out.push_str(&list.join("; "));
            out.push_str(". ");
        }
        let places = q.places();
        out.push_str(&format!(
            "Places in use: {}/{} for his own keys and passwords (the default)",
            self.default_project, self.default_environment
        ));
        for (p, e, f, names) in places.iter().take(8) {
            out.push_str(&format!("; {p}/{e}{}: {}", if f == "/" { "" } else { f }, names.join(", ")));
        }
        out.push('.');
        out
    }
}

/// The card's text, in his language (English unless he wrote Danish).
fn card_bubble(name: &str, place: &str, danish: bool) -> String {
    if danish {
        format!("🔐 Kortet til `{name}` i {place} er klar: indsæt værdien her. Den går direkte til Infisical, aldrig til denne chat eller min hukommelse.")
    } else {
        format!("🔐 Paste `{name}` here. It goes straight to Infisical ({place}), never to this chat or my memory.")
    }
}

#[async_trait::async_trait]
impl lucina_tools::tools::secret_tile::SecretCardOpener for OwnerCards {
    async fn open(&self, session_id: &str, body: Value) -> Result<Value, String> {
        let text = |k: &str| body[k].as_str().map(str::trim).unwrap_or("").to_owned();
        let name = text("name");
        if !crate::owner_secrets::slug_ok(&name, 256) {
            return Err("name: letters, digits, _ . @ -, e.g. GOOGLE_MAPS_SERVER_KEY".into());
        }
        let q = self.queue(session_id);
        let (mut project, mut environment, mut path) = (text("project").to_lowercase(), text("environment"), text("path"));
        for (what, v) in [("project", &project), ("environment", &environment)] {
            if !v.is_empty() && !crate::owner_secrets::slug_ok(v, 128) {
                return Err(format!("{what}: letters, digits, _ . @ -"));
            }
        }
        if project.is_empty() {
            match q.place_of(&name) {
                Some((p, e, f)) => {
                    project = p.to_lowercase();
                    environment = e;
                    if path.is_empty() {
                        path = f;
                    }
                }
                None => {
                    project = self.default_project.clone();
                    environment = self.default_environment.clone();
                }
            }
        } else if environment.is_empty() {
            environment = q
                .places()
                .into_iter()
                .find(|p| p.0 == project)
                .map(|p| p.1)
                .unwrap_or_else(|| "prod".to_owned());
        }
        let path = if path.is_empty() {
            "/".to_owned()
        } else {
            crate::owner_secrets::request_folder(&path).ok_or("path: folders of letters, digits, _ . @ -")?
        };
        let place = format!("{project} / {environment} / {path}");
        let danish = body["lang"] == "da";
        let r = SecretRequest {
            name: name.clone(),
            project,
            environment,
            path,
            // His own request: no "who", no "why", and his phone no separate push (the card is in the chat).
            reason: String::new(),
            who: String::new(),
            new_project: false,
        };
        let a = q.show_now(r, card_bubble(&name, &place, danish)).map_err(|e| e.message)?;
        let e = &a.entry;
        Ok(json!({"status": a.status, "name": e.name, "project": e.project, "environment": e.environment,
                  "path": e.path, "itemId": e.item_id, "othersWaiting": a.ahead}))
    }
}

/// `POST /api/secretary/secrets/request`, and Lucina's own tool: a body of names.
pub async fn ask_body(queue: &SecretRequests, body: &Value) -> Result<Value, ApiError> {
    let r = parse_request(body)?;
    Ok(queue.ask(r, None, false).await?.to_json())
}

#[async_trait::async_trait]
impl lucina_tools::tools::secret_request::SecretAsker for SecretRequests {
    async fn ask(&self, session_id: &str, body: Value) -> Result<Value, String> {
        if crate::selftest::is_session(session_id) {
            return Err("self-test sessions don't ask the owner for secrets".into());
        }
        ask_body(self, &body).await.map_err(|e| e.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(dir: &Path) -> SecretRequests {
        let timeline = Arc::new(TimelineStore::new(dir.join("timeline.jsonl"), 1 << 20, true));
        let secretary = Secretary { http: reqwest::Client::new(), url: None, token_file: None };
        SecretRequests::new(dir, timeline, secretary)
    }

    fn req(name: &str) -> SecretRequest {
        SecretRequest {
            name: name.into(),
            project: "lucinaos".into(),
            environment: "prod".into(),
            path: "/".into(),
            reason: "the image needs it".into(),
            who: "claude-code".into(),
            new_project: false,
        }
    }

    fn receipt(key: &str) -> Value {
        json!({"key": key, "project": {"id": "p-1", "name": "LucinaOS"}, "environment": "prod", "path": "/",
               "action": "created", "fingerprint": "ab12cd34"})
    }

    fn cards(q: &SecretRequests) -> Vec<Value> {
        q.timeline
            .read_all()
            .unwrap()
            .iter()
            .filter_map(|i| serde_json::from_str::<Value>(i.raw_value().get()).ok())
            .filter(|i| i["meta"]["kind"] == "secret_request")
            .collect()
    }

    #[tokio::test]
    async fn one_card_at_a_time_and_the_next_follows() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        let c = q.ask(req("C"), None, false).await.unwrap();
        assert_eq!((a.status, b.status, c.status), ("asked", "queued", "queued"));
        assert_eq!((a.ahead, b.ahead, c.ahead), (0, 1, 2));
        assert_eq!(cards(&q).len(), 1, "only the active one has a card");
        assert!(a.rich.is_some() && b.rich.is_none());
        assert_eq!(cards(&q)[0]["meta"]["actions"][0]["payload"]["request"], json!(a.entry.id));

        // Saved by id: the next card appears. Dismissed: the one after it.
        let next = q.saved(Some(&a.entry.id), &receipt("A")).expect("B's card");
        assert_eq!(cards(&q).len(), 2);
        assert_eq!(q.list()["active"], json!(b.entry.id));
        assert_eq!(q.list()["queued"], 1);
        assert!(cards(&q).iter().any(|i| i["id"] == json!(next)));
        let d = q.dismiss(&b.entry.id).unwrap();
        assert_eq!(d["status"], "dismissed");
        assert!(d["nextItemId"].is_string());
        assert_eq!(q.list()["active"], json!(c.entry.id));
        assert_eq!(q.dismiss(&b.entry.id).unwrap()["status"], "dismissed", "twice is fine");
        assert_eq!(cards(&q).len(), 3, "and posts nothing more");
        assert_eq!(q.dismiss("sr-nope").unwrap_err().code, "not_found");

        let list = q.list();
        let saved = &list["requests"][0];
        assert_eq!((saved["status"].as_str(), saved["fingerprint"].as_str()), (Some("saved"), Some("ab12cd34")));
        assert_eq!(saved["project"], "LucinaOS");
    }

    #[tokio::test]
    async fn a_restart_keeps_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        drop(q);
        let q = queue(dir.path());
        let list = q.list();
        assert_eq!(list["active"], json!(a.entry.id));
        assert_eq!(list["queued"], 1);
        assert_eq!(cards(&q).len(), 1, "a restart posts nothing");
        // Repeat protection survives too.
        assert_eq!(q.ask(req("B"), None, false).await.unwrap().status, "already_asked");
        q.saved(Some(&a.entry.id), &receipt("A")).expect("B's card");
        assert_eq!(q.list()["active"], json!(b.entry.id));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A broken file says so in the log and starts empty; the save path still works.
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        let q = queue(dir.path());
        assert_eq!(q.list()["requests"], json!([]));
    }

    #[tokio::test]
    async fn repeats_answer_with_the_first_card() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let again = q.ask(req("A"), None, false).await.unwrap();
        assert_eq!(again.status, "already_asked");
        assert_eq!(again.entry.item_id, a.entry.item_id);
        // Closed a moment ago: an agent can't re-ask at once, the owner can.
        q.dismiss(&a.entry.id).unwrap();
        assert_eq!(q.ask(req("A"), None, false).await.unwrap().status, "already_asked");
        assert_eq!(q.ask(req("A"), None, true).await.unwrap().status, "asked");
        assert_eq!(cards(&q).len(), 2);
    }

    #[tokio::test]
    async fn an_old_sheet_closes_the_request_by_its_names() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        // Another secret saved from the sheet: the card stays.
        assert!(q.saved(None, &receipt("OTHER")).is_none());
        assert_eq!(q.list()["active"], json!(a.entry.id));
        // The same names: closed.
        q.saved(None, &receipt("A"));
        assert_eq!(q.list()["active"], Value::Null);
        // A request id naming something else is not believed.
        let b = q.ask(req("B"), None, false).await.unwrap();
        q.saved(Some(&b.entry.id), &receipt("WRONG"));
        assert_eq!(q.list()["active"], json!(b.entry.id));
    }

    #[tokio::test]
    async fn the_same_name_saved_anywhere_closes_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        q.ask(req("A"), None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        let mut elsewhere = receipt("A");
        elsewhere["project"] = json!({"id": "p-9", "name": "Lucina"});
        elsewhere["path"] = json!("/other");
        q.saved(None, &elsewhere);
        let list = q.list();
        let a = list["requests"].as_array().unwrap().iter().find(|e| e["name"] == "A").unwrap().clone();
        assert_eq!((a["status"].as_str(), a["project"].as_str()), (Some("saved"), Some("Lucina")));
        // The next card follows, and a save of another name leaves it open.
        assert_eq!(list["active"], json!(b.entry.id));
        q.saved(None, &elsewhere);
        assert_eq!(q.list()["active"], json!(b.entry.id));
    }

    #[tokio::test]
    async fn a_save_under_another_project_closes_the_same_name_asked_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let mut other = req("A");
        other.project = "pokoin".into();
        let same_name = q.ask(other, None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        let mut elsewhere = receipt("A");
        elsewhere["project"] = json!({"id": "p-9", "name": "Lucina"});
        q.saved(Some(&a.entry.id), &elsewhere);
        let list = q.list();
        let found = list["requests"].as_array().unwrap().iter().find(|e| e["id"] == json!(same_name.entry.id)).unwrap().clone();
        assert_eq!(found["status"].as_str(), Some("saved"));
        assert_eq!(list["active"], json!(b.entry.id));
    }

    #[tokio::test]
    async fn a_save_in_the_named_project_leaves_the_same_name_asked_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let mut other = req("A");
        other.project = "pokoin".into();
        let same_name = q.ask(other, None, false).await.unwrap();
        let mut here = receipt("A");
        here["project"] = json!({"id": "p-1", "name": "lucinaos"});
        q.saved(Some(&a.entry.id), &here);
        assert_eq!(q.list()["active"], json!(same_name.entry.id));
    }

    #[tokio::test]
    async fn a_receipt_from_before_a_start_closes_what_is_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        lucina_item(&q.timeline, "✅ Saved `OTHER` in X / prod / / (fingerprint 00000000)", "secret_receipt", None);
        lucina_item(&q.timeline, "✅ Saved `A` in Lucina / prod / / (fingerprint 53e709e8)", "secret_receipt", None);
        // A later process, with the same files.
        let timeline = Arc::new(TimelineStore::new(dir.path().join("timeline.jsonl"), 1 << 20, true));
        let again = SecretRequests::new(dir.path(), timeline, Secretary { http: reqwest::Client::new(), url: None, token_file: None });
        let list = again.list();
        let found = list["requests"].as_array().unwrap().iter().find(|e| e["id"] == json!(a.entry.id)).unwrap().clone();
        assert_eq!((found["status"].as_str(), found["fingerprint"].as_str()), (Some("saved"), Some("53e709e8")));
        assert_eq!(list["active"], json!(b.entry.id));
        // It stays closed on disk.
        let third = queue(dir.path());
        assert_eq!(third.list()["active"], json!(b.entry.id));
    }

    #[tokio::test]
    async fn what_nobody_answers_expires_and_the_queue_moves_on() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        let a = q.ask(req("A"), None, false).await.unwrap();
        let b = q.ask(req("B"), None, false).await.unwrap();
        q.with(|entries| entries[0].shown_at = Some(now() - REQUEST_TTL.as_secs() as i64 - 1));
        let list = q.list();
        assert_eq!(list["requests"][0]["status"], "expired");
        assert_eq!(list["active"], json!(b.entry.id));
        assert_ne!(a.entry.id, b.entry.id);
    }

    #[tokio::test]
    async fn the_queue_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let q = queue(dir.path());
        for i in 0..MAX_OPEN {
            q.ask(req(&format!("N{i}")), None, false).await.unwrap();
        }
        assert_eq!(q.ask(req("ONE_MORE"), None, false).await.unwrap_err().code, "too_many_requests");
    }
}
