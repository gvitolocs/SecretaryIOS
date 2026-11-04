//! The Juno bridge: Lucina's `juno_*` tool calls to the owner's laptop
//! (`docs/research/juno-lucina-tools.md` in LucinaOS, "How a call travels").
//!
//! No port listens on the laptop: `lucina-devd` long-polls secretary-server
//! with its device token, which stamps `device: {id, name}` and forwards here.
//!
//! - `POST /api/secretary/device/juno/next {device, waitSecs}`: registers the
//!   device (its `last_seen`), then waits up to `waitSecs` (≤ 25) for a call
//!   queued for it: `{"call": {id, tool, arguments, deadlineMs, decisionId?,
//!   internal?}, "events": []}`, or `{"call": null, "events": []}`. `events`
//!   is reserved for Lucina → laptop and always empty for now.
//! - `POST /api/secretary/device/juno/result {device, id, ok, result?, error?}`
//!   (≤ [`MAX_RESULT_BYTES`]): hands the result to the waiting tool call. An
//!   unknown or expired id, or one queued for another device: 404 `unknown_call`.
//! - `POST /api/secretary/device/juno/events {device, events, lost}`: the
//!   laptop's semantic events (secretary-server validated them); the last
//!   [`MAX_EVENTS`] per device are kept in RAM for `juno_events` and the
//!   device fact of a turn ([`JunoBridge::device_fact`]).
//! - `GET /api/secretary/juno/screenshots/{id}.png`: a screenshot a call
//!   returned, kept for the reply's image block ([`MAX_IMAGES`], [`IMAGE_TTL`]).
//!
//! [`JunoBridge::call`] queues a call and waits for it: nobody polled in
//! [`ONLINE_WINDOW`], or nobody picked it up in [`PICKUP`]: offline. Each call
//! is logged as `juno_audit` (device, tool, a payload-free summary, ok, ms,
//! and `target`, `undo`, `effect`, empty for now). An `internal` call (Lucina's
//! own step, secret_keep's `juno:` source) is logged without its summary.
//!
//! Risky calls wait for a decision ([`JunoLink::ask`]): it is filed with
//! `crate::decide::create` (topic `juno`, asking client `lucina`) and carries
//! the call's hash (and the device's, when the device asked) in its context;
//! [`JunoLink::decided`] lets the call run only when that decision was answered
//! "Go" for the same hash.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use lucina_tools::tools::juno::{driver_summary, Ask, CallOpts, EventQuery, JunoLink};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

use crate::api::{ok, read_json, ApiError};
use crate::AppState;

type ApiResult = Result<Response, ApiError>;

/// A device counts as connected while it polled this recently.
pub const ONLINE_WINDOW: Duration = Duration::from_secs(40);
/// A queued call nobody picks up in this time fails as offline.
pub const PICKUP: Duration = Duration::from_secs(10);
/// Longest long poll.
pub const MAX_WAIT_SECS: u64 = 25;
/// Body limit of a result (a base64 PNG screenshot of up to 8 MiB, and its envelope).
pub const MAX_RESULT_BYTES: usize = 12 * 1024 * 1024;
/// Largest screenshot kept.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Screenshots kept for replies.
pub const MAX_IMAGES: usize = 8;
pub const IMAGE_TTL: Duration = Duration::from_secs(3600);
const MAX_ERROR_CHARS: usize = 2000;
/// Events kept per device.
pub const MAX_EVENTS: usize = 500;
/// Largest events body Lucina takes (secretary-server caps it at 256 KiB).
pub const MAX_EVENTS_BODY: usize = 320 * 1024;
/// The decisions' topic.
pub const TOPIC: &str = "juno";

/// Why a call did not return a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JunoError {
    /// No device polled in [`ONLINE_WINDOW`], or nobody picked the call up:
    /// when it was last seen (`None`: never).
    Offline(Option<DateTime<Utc>>),
    Timeout,
    /// The device ran the tool and reported an error.
    DeviceError(String),
}

impl std::fmt::Display for JunoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JunoError::Offline(seen) => write!(
                f,
                "Juno is offline (last seen {}).",
                seen.map_or("never".to_owned(), |t| t
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            ),
            JunoError::Timeout => f.write_str("Juno did not answer in time"),
            JunoError::DeviceError(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for JunoError {}

struct Device {
    name: String,
    last_seen: Instant,
    seen_at: DateTime<Utc>,
    /// The latest events, oldest first.
    events: VecDeque<Value>,
    /// Events the laptop could not send, and those dropped here.
    lost: u64,
    /// Events not read yet (juno_tools says how many).
    unread: u64,
    queue: VecDeque<String>,
    wake: Arc<Notify>,
}

struct Pending {
    device: String,
    tool: String,
    arguments: Value,
    deadline_ms: i64,
    decision_id: Option<String>,
    internal: bool,
    picked: Option<oneshot::Sender<()>>,
    done: Option<oneshot::Sender<Result<Value, String>>>,
}

struct Image {
    id: String,
    at: Instant,
    png: Arc<Vec<u8>>,
}

#[derive(Default)]
struct State {
    devices: HashMap<String, Device>,
    calls: HashMap<String, Pending>,
    images: VecDeque<Image>,
}

/// Per device a queue of calls; per call a slot for its result.
#[derive(Default)]
pub struct JunoBridge {
    state: Mutex<State>,
    /// The server, for decisions (set once it is built).
    app: OnceLock<Weak<crate::Inner>>,
}

/// 16 random letters and digits.
fn call_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut n = uuid::Uuid::new_v4().as_u128();
    (0..16)
        .map(|_| {
            let c = ALPHABET[(n % 62) as usize] as char;
            n /= 62;
            c
        })
        .collect()
}

/// A device id as Juno's Chat puts it in its session (`session_id_for`):
/// letters, digits, `_` and `-`, at most 58.
fn session_form(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(58)
        .collect()
}

pub fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl JunoBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give the bridge the server it files decisions with.
    pub fn attach(&self, app: Weak<crate::Inner>) {
        let _ = self.app.set(app);
    }

    fn app(&self) -> Result<crate::AppState, String> {
        self.app
            .get()
            .and_then(Weak::upgrade)
            .map(crate::AppState)
            .ok_or_else(|| "decisions are not available on this server".to_owned())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The device a call goes to: `named` (its id, or the form Juno's Chat
    /// session carries), else the one that polled most recently.
    fn pick(st: &State, named: Option<&str>) -> Result<String, JunoError> {
        let found = match named {
            Some(n) => st
                .devices
                .iter()
                .find(|(id, _)| id.as_str() == n || session_form(id) == n),
            None => st.devices.iter().max_by_key(|(_, d)| d.last_seen),
        };
        let Some((id, d)) = found else {
            return Err(JunoError::Offline(None));
        };
        if d.last_seen.elapsed() > ONLINE_WINDOW {
            return Err(JunoError::Offline(Some(d.seen_at)));
        }
        Ok(id.clone())
    }

    /// Run `tool` on a Juno device and wait at most `timeout` for its result.
    pub async fn call(
        &self,
        device: Option<&str>,
        tool: &str,
        arguments: Value,
        timeout: Duration,
        opts: &CallOpts,
    ) -> Result<Value, JunoError> {
        let started = Instant::now();
        let summary = if opts.internal {
            "internal".to_owned()
        } else {
            driver_summary(tool, &arguments)
        };
        let (picked_rx, done_rx, id, dev) = {
            let mut st = self.lock();
            let dev = match Self::pick(&st, device) {
                Ok(d) => d,
                Err(e) => {
                    drop(st);
                    audit(device.unwrap_or(""), tool, &summary, false, started);
                    return Err(e);
                }
            };
            let id = call_id();
            let (picked_tx, picked_rx) = oneshot::channel();
            let (done_tx, done_rx) = oneshot::channel();
            let deadline_ms = chrono::Utc::now().timestamp_millis()
                + i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX / 2);
            st.calls.insert(
                id.clone(),
                Pending {
                    device: dev.clone(),
                    tool: tool.to_owned(),
                    arguments,
                    deadline_ms,
                    decision_id: opts.decision_id.clone(),
                    internal: opts.internal,
                    picked: Some(picked_tx),
                    done: Some(done_tx),
                },
            );
            if let Some(d) = st.devices.get_mut(&dev) {
                d.queue.push_back(id.clone());
                d.wake.notify_one();
            }
            (picked_rx, done_rx, id, dev)
        };
        let out = self.wait(picked_rx, done_rx, timeout).await;
        let out = match out {
            Err(JunoError::Offline(_)) => Err(JunoError::Offline(
                self.lock().devices.get(&dev).map(|d| d.seen_at),
            )),
            other => other,
        };
        // An expired call is forgotten: its late result is `unknown_call`.
        {
            let mut st = self.lock();
            st.calls.remove(&id);
            if let Some(d) = st.devices.get_mut(&dev) {
                d.queue.retain(|q| q != &id);
            }
        }
        let out = out.map(|v| self.keep_image(v));
        audit(&dev, tool, &summary, out.is_ok(), started);
        out
    }

    async fn wait(
        &self,
        picked: oneshot::Receiver<()>,
        done: oneshot::Receiver<Result<Value, String>>,
        timeout: Duration,
    ) -> Result<Value, JunoError> {
        let started = Instant::now();
        let pickup = PICKUP.min(timeout);
        if !matches!(tokio::time::timeout(pickup, picked).await, Ok(Ok(()))) {
            return Err(JunoError::Offline(None));
        }
        match tokio::time::timeout(timeout.saturating_sub(started.elapsed()), done).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(JunoError::DeviceError(e)),
            Ok(Err(_)) | Err(_) => Err(JunoError::Timeout),
        }
    }

    /// The long poll: refresh `device`, then hand out its next call within `wait`.
    pub async fn next(&self, device: &str, name: &str, wait: Duration) -> Option<Value> {
        let until = Instant::now() + wait;
        loop {
            let wake = {
                let mut st = self.lock();
                let st = &mut *st;
                let d = st
                    .devices
                    .entry(device.to_owned())
                    .or_insert_with(|| Device::new(name));
                d.name = name.to_owned();
                d.last_seen = Instant::now();
                d.seen_at = Utc::now();
                while let Some(id) = d.queue.pop_front() {
                    let Some(p) = st.calls.get_mut(&id) else {
                        continue;
                    };
                    let Some(picked) = p.picked.take() else {
                        continue;
                    };
                    if picked.send(()).is_err() {
                        continue;
                    }
                    let mut call = json!({
                        "id": id,
                        "tool": p.tool,
                        "arguments": p.arguments,
                        "deadlineMs": p.deadline_ms,
                    });
                    if let Some(d) = &p.decision_id {
                        call["decisionId"] = json!(d);
                    }
                    if p.internal {
                        call["internal"] = json!(true);
                    }
                    return Some(call);
                }
                d.wake.clone()
            };
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || tokio::time::timeout(left, wake.notified()).await.is_err() {
                return None;
            }
        }
    }

    /// Deliver a result. `false`: no call `id` of `device` is waiting.
    pub fn result(&self, device: &str, id: &str, outcome: Result<Value, String>) -> bool {
        let tx = {
            let mut st = self.lock();
            match st.calls.get_mut(id) {
                Some(p) if p.device == device && p.picked.is_none() => p.done.take(),
                _ => None,
            }
        };
        tx.is_some_and(|tx| tx.send(outcome).is_ok())
    }

    /// The devices and when each was last seen.
    pub fn status(&self) -> Value {
        let st = self.lock();
        let mut devices: Vec<(&String, &Device)> = st.devices.iter().collect();
        devices.sort_by_key(|(_, d)| std::cmp::Reverse(d.last_seen));
        json!({"devices": devices.iter().map(|(id, d)| {
            let idle = d.last_seen.elapsed();
            json!({"id": id, "name": d.name, "connected": idle <= ONLINE_WINDOW, "lastSeenSecs": idle.as_secs(), "newEvents": d.unread})
        }).collect::<Vec<_>>()})
    }

    /// A Juno device has polled since Lucina started.
    pub fn ever_seen(&self) -> bool {
        !self.lock().devices.is_empty()
    }

    /// Store a batch of the laptop's events (validated by secretary-server;
    /// anything not an object with a `kind` is counted as lost). The device
    /// need not have polled: its events still count it as seen.
    pub fn add_events(&self, device: &str, name: &str, events: &[Value], lost: u64) -> usize {
        let mut st = self.lock();
        let d = st
            .devices
            .entry(device.to_owned())
            .or_insert_with(|| Device::new(name));
        d.lost = d.lost.saturating_add(lost);
        let mut kept = 0;
        for e in events {
            if !e.is_object() || !e["kind"].is_string() {
                d.lost = d.lost.saturating_add(1);
                continue;
            }
            if d.events.len() >= MAX_EVENTS {
                d.events.pop_front();
            }
            d.events.push_back(e.clone());
            kept += 1;
        }
        d.unread = d.unread.saturating_add(kept as u64).min(MAX_EVENTS as u64);
        kept
    }

    /// The device events go to and come from: `named`, else the most recent.
    fn pick_any<'a>(st: &'a mut State, named: Option<&str>) -> Option<&'a mut Device> {
        let id = match named {
            Some(n) => st
                .devices
                .keys()
                .find(|id| id.as_str() == n || session_form(id) == n)
                .cloned(),
            None => st
                .devices
                .iter()
                .max_by_key(|(_, d)| d.last_seen)
                .map(|(id, _)| id.clone()),
        }?;
        st.devices.get_mut(&id)
    }

    /// `juno_events`: the matching events, newest last; they count as read.
    pub fn events(&self, device: Option<&str>, q: &EventQuery) -> Value {
        let mut st = self.lock();
        let Some(d) = Self::pick_any(&mut st, device) else {
            return json!({"events": [], "lost": 0});
        };
        d.unread = 0;
        let kind_ok = |k: &str| {
            q.kind.as_deref().is_none_or(|want| {
                k == want || k.strip_prefix(want).is_some_and(|r| r.starts_with('.'))
            })
        };
        let picked: Vec<&Value> = d
            .events
            .iter()
            .filter(|e| {
                q.since
                    .is_none_or(|s| e["seq"].as_u64().is_some_and(|n| n > s))
            })
            .filter(|e| kind_ok(e["kind"].as_str().unwrap_or("")))
            .filter(|e| {
                q.app
                    .as_deref()
                    .is_none_or(|a| e["app"].as_str() == Some(a))
            })
            .collect();
        let skip = picked.len().saturating_sub(q.max.max(1));
        json!({"events": picked[skip..], "lost": d.lost})
    }

    /// The Juno fact of a turn: online or offline since when, and the sticky
    /// state of its latest events ("In front: Video; on battery 64 %").
    /// `None`: no Juno device was ever seen.
    pub fn device_fact(&self) -> Option<String> {
        let st = self.lock();
        let (_, d) = st.devices.iter().max_by_key(|(_, d)| d.last_seen)?;
        let online = d.last_seen.elapsed() <= ONLINE_WINDOW;
        let mut fact = if online {
            "Juno, the owner's laptop, is online: juno_* tools reach it.".to_owned()
        } else {
            format!(
                "Juno, the owner's laptop, is offline since {}; juno_* tools will say so.",
                d.seen_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )
        };
        if online {
            if let Some(sticky) = sticky_state(&d.events) {
                fact = format!("{fact} {sticky}.");
            }
        }
        Some(fact)
    }

    /// The sticky state line alone (for Juno's own sessions).
    pub fn sticky_line(&self) -> Option<String> {
        let st = self.lock();
        let (_, d) = st.devices.iter().max_by_key(|(_, d)| d.last_seen)?;
        sticky_state(&d.events).map(|s| format!("{s}."))
    }

    /// A result's base64 PNG is kept here and replaced by its `src`: the model
    /// never reads the picture's bytes, the reply shows it.
    fn keep_image(&self, mut v: Value) -> Value {
        let Some(data) = v["image"]["data"].as_str() else {
            return v;
        };
        let png = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .ok()
            .filter(|b| b.len() <= MAX_IMAGE_BYTES && b.starts_with(b"\x89PNG\r\n\x1a\n"));
        let image = match png {
            Some(png) => {
                let id = call_id();
                let bytes = png.len();
                let mut st = self.lock();
                st.images.retain(|i| i.at.elapsed() < IMAGE_TTL);
                while st.images.len() >= MAX_IMAGES {
                    st.images.pop_front();
                }
                st.images.push_back(Image {
                    id: id.clone(),
                    at: Instant::now(),
                    png: Arc::new(png),
                });
                json!({"src": format!("/v1/juno/screenshots/{id}.png"), "mimeType": "image/png", "bytes": bytes})
            }
            None => json!({"error": "the laptop sent an image that is not a PNG"}),
        };
        v["image"] = image;
        v
    }

    /// A kept screenshot.
    pub fn screenshot(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        let st = self.lock();
        st.images
            .iter()
            .find(|i| i.id == id && i.at.elapsed() < IMAGE_TTL)
            .map(|i| i.png.clone())
    }
}

impl Device {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            last_seen: Instant::now(),
            seen_at: Utc::now(),
            events: VecDeque::new(),
            lost: 0,
            unread: 0,
            queue: VecDeque::new(),
            wake: Arc::new(Notify::new()),
        }
    }
}

/// What stays true until another event says otherwise, from the latest
/// events: the app in front (`app.focused`), power (`power.*`: on battery and
/// its percent, or charging), the network (`net.*`) and media playing
/// (`media.started` without its `media.ended`/`media.stopped`).
pub fn sticky_state(events: &VecDeque<Value>) -> Option<String> {
    let field = |e: &Value, k: &str| -> Option<String> {
        e["fields"][k]
            .as_str()
            .map(str::to_owned)
            .or_else(|| e["fields"][k].as_u64().map(|n| n.to_string()))
            .or_else(|| e["fields"][k].as_f64().map(|n| format!("{n:.0}")))
    };
    let short = |t: &str| t.chars().take(60).collect::<String>();
    let latest = |pred: &dyn Fn(&str) -> bool| {
        events
            .iter()
            .rev()
            .find(|e| pred(e["kind"].as_str().unwrap_or("")))
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(e) = latest(&|k| k == "app.focused") {
        if let Some(app) = e["subject"].as_str().or(e["app"].as_str()) {
            parts.push(format!("In front: {}", short(app)));
        }
    }
    if let Some(e) = latest(&|k| k.starts_with("media.")) {
        if e["kind"] == "media.started" {
            let what = e["subject"]
                .as_str()
                .or(e["app"].as_str())
                .unwrap_or("media");
            parts.push(format!("playing {}", short(what)));
        }
    }
    if let Some(e) = latest(&|k| k.starts_with("power.")) {
        let pct = field(e, "percent")
            .map(|p| format!(" {} %", short(&p)))
            .unwrap_or_default();
        let on_ac = e["fields"]["ac"] == json!(true)
            || e["fields"]["charging"] == json!(true)
            || matches!(
                e["kind"].as_str(),
                Some("power.ac" | "power.charging" | "power.plugged")
            );
        parts.push(if on_ac {
            format!("charging{pct}")
        } else {
            format!("on battery{pct}")
        });
    }
    if let Some(e) = latest(&|k| k.starts_with("net.")) {
        let up = !matches!(
            e["kind"].as_str(),
            Some("net.down" | "net.offline" | "net.disconnected")
        );
        let ssid = field(e, "ssid")
            .map(|s| format!(" ({})", short(&s)))
            .unwrap_or_default();
        parts.push(if up {
            format!("online{ssid}")
        } else {
            "no network".to_owned()
        });
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn audit(device: &str, tool: &str, summary: &str, ok: bool, started: Instant) {
    tracing::info!(
        target: "juno_audit",
        device,
        tool,
        summary,
        ok,
        target = "",
        undo = "",
        effect = "",
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
}

#[async_trait::async_trait]
impl JunoLink for JunoBridge {
    async fn call(
        &self,
        device: Option<String>,
        tool: &str,
        args: Value,
        timeout: Duration,
        opts: &CallOpts,
    ) -> Result<Value, String> {
        JunoBridge::call(self, device.as_deref(), tool, args, timeout, opts)
            .await
            .map_err(|e| e.to_string())
    }

    fn status(&self) -> Value {
        JunoBridge::status(self)
    }

    async fn ask(&self, ask: Ask) -> Result<String, String> {
        let st = self.app()?;
        let d = crate::decide::create(&st, &decision_body(&ask))
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(d.decision_id)
    }

    fn decided(&self, id: &str, hash: &str) -> Result<(), String> {
        let st = self.app()?;
        if !crate::decide::valid_id(id) {
            return Err("a decision id is dc- and 12 hex digits".into());
        }
        let d = crate::decide::store(&st.cfg.data_dir)
            .get(id, Utc::now())
            .ok_or("no such decision")?;
        go_for(&d, hash)
    }

    fn events(&self, device: Option<&str>, q: &EventQuery) -> Value {
        JunoBridge::events(self, device, q)
    }
}

/// The line of a decision's context that ties it to one call.
fn hash_line(hash: &str) -> String {
    format!("Call hash: {hash}")
}

/// `POST /api/secretary/decisions` for a risky call.
pub fn decision_body(ask: &Ask) -> Value {
    let mut context = format!("{}\n{}", ask.context, hash_line(&ask.hash));
    if let Some(h) = &ask.device_hash {
        context.push_str(&format!("\nDevice call hash: {h}"));
    }
    json!({
        "question": ask.summary.chars().take(crate::decide::MAX_QUESTION).collect::<String>(),
        "options": ["Go", "Don't"],
        "context": context,
        "reversible": false,
        "impact": "high",
        "urgency": "normal",
        "topic": TOPIC,
        "asking_client": "lucina",
        "asking_session": ask.session.chars().take(120).collect::<String>(),
    })
}

/// `Ok`: `d` is a Juno decision filed by Lucina for the call with `hash`,
/// answered with its first option ("Go").
pub fn go_for(d: &crate::decide::Decision, hash: &str) -> Result<(), String> {
    if d.topic.as_deref() != Some(TOPIC) || d.asking_client != "lucina" {
        return Err("it is not a decision on a Juno call".into());
    }
    if !d.context.lines().any(|l| l == hash_line(hash)) {
        return Err("it was filed for another call".into());
    }
    match (d.status.as_str(), d.chosen_index) {
        ("decided", Some(1)) => Ok(()),
        ("decided", _) => Err("the answer was Don't".into()),
        (s, _) => Err(format!("it is {s}, not decided")),
    }
}

/// `{device: {id, name}}` as secretary-server stamps it.
fn device_of(body: &Value) -> Result<(String, String), ApiError> {
    let id = body["device"]["id"].as_str().unwrap_or("");
    if !valid_id(id) {
        return Err(ApiError::bad_request("device.id is required"));
    }
    let name: String = body["device"]["name"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    Ok((id.to_owned(), name))
}

/// `/api/secretary/device/juno/{next|result}`.
pub async fn device_route(
    st: &AppState,
    method: &Method,
    op: Option<&str>,
    body: Body,
) -> ApiResult {
    let unknown = || {
        ApiError::new(
            404,
            "unknown_call",
            "No such Juno call (unknown or expired)",
        )
    };
    match op {
        Some("next") | Some("result") | Some("events") if method != Method::POST => {
            Err(ApiError::method_not_allowed(&["POST"]))
        }
        Some("next") => {
            let body = read_json(body, crate::api::MAX_BODY_BYTES).await?;
            let (id, name) = device_of(&body)?;
            let wait = match &body["waitSecs"] {
                Value::Null => MAX_WAIT_SECS,
                v => v
                    .as_u64()
                    .ok_or_else(|| ApiError::bad_request("waitSecs must be a whole number"))?
                    .min(MAX_WAIT_SECS),
            };
            let call = st.juno.next(&id, &name, Duration::from_secs(wait)).await;
            Ok(ok(json!({ "call": call, "events": [] })))
        }
        Some("result") => {
            let body = read_json(body, MAX_RESULT_BYTES).await?;
            let (device, _) = device_of(&body)?;
            let id = body["id"].as_str().unwrap_or("");
            if !valid_id(id) {
                return Err(unknown());
            }
            let okv = body["ok"]
                .as_bool()
                .ok_or_else(|| ApiError::bad_request("ok must be a boolean"))?;
            let outcome = if okv {
                Ok(match &body["result"] {
                    Value::Object(_) => body["result"].clone(),
                    Value::Null => json!({"text": "done"}),
                    other => json!({"text": other.to_string()}),
                })
            } else {
                let e: String = body["error"]
                    .as_str()
                    .unwrap_or("the laptop reported an error")
                    .chars()
                    .take(MAX_ERROR_CHARS)
                    .collect();
                Err(e)
            };
            if !st.juno.result(&device, id, outcome) {
                return Err(unknown());
            }
            Ok(ok(json!({"delivered": true})))
        }
        Some("events") => {
            let body = read_json(body, MAX_EVENTS_BODY).await?;
            let (device, name) = device_of(&body)?;
            let events = body["events"]
                .as_array()
                .ok_or_else(|| ApiError::bad_request("events must be an array"))?;
            if events.len() > 64 {
                return Err(ApiError::bad_request("at most 64 events"));
            }
            let lost = body["lost"].as_u64().unwrap_or(0);
            let stored = st.juno.add_events(&device, &name, events, lost);
            Ok(ok(json!({"stored": stored})))
        }
        _ => Err(ApiError::not_found()),
    }
}

/// `GET /api/secretary/juno/screenshots/{id}.png`.
pub fn screenshot_route(st: &AppState, method: &Method, file: &str) -> ApiResult {
    if method != Method::GET {
        return Err(ApiError::method_not_allowed(&["GET"]));
    }
    let png = file
        .strip_suffix(".png")
        .filter(|id| valid_id(id))
        .and_then(|id| st.juno.screenshot(id))
        .ok_or_else(|| {
            ApiError::new(
                404,
                "screenshot_not_found",
                "No such Juno screenshot (kept an hour)",
            )
        })?;
    let mut resp = (*png).clone().into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: Duration = Duration::from_secs(5);

    /// The laptop: polls once and answers its call with `answer`.
    fn laptop(
        b: &Arc<JunoBridge>,
        device: &'static str,
        answer: impl FnOnce(&Value) -> Result<Value, String> + Send + 'static,
    ) -> tokio::task::JoinHandle<Option<Value>> {
        let b = b.clone();
        tokio::spawn(async move {
            let call = b.next(device, "Juno", SECS).await?;
            let id = call["id"].as_str().unwrap().to_owned();
            assert!(b.result(device, &id, answer(&call)));
            Some(call)
        })
    }

    async fn polled(b: &JunoBridge, device: &str) {
        assert!(b.next(device, "Juno", Duration::ZERO).await.is_none());
    }

    #[tokio::test]
    async fn juno_call_reaches_the_polling_device_and_returns() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        let pc = laptop(&b, "dev1", |c| {
            Ok(json!({"text": format!("opened {}", c["arguments"]["id"])}))
        });
        let out = b
            .call(
                Some("dev1"),
                "open_app",
                json!({"id": "gallery"}),
                SECS,
                &CallOpts::default(),
            )
            .await
            .unwrap();
        assert_eq!(out["text"], "opened \"gallery\"");
        let call = pc.await.unwrap().unwrap();
        assert_eq!(call["tool"], "open_app");
        assert_eq!(call["id"].as_str().unwrap().len(), 16);
        assert!(call["id"]
            .as_str()
            .unwrap()
            .bytes()
            .all(|c| c.is_ascii_alphanumeric()));
        assert!(call["deadlineMs"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis());
        // The result slot is gone: a second delivery is refused.
        assert!(!b.result("dev1", call["id"].as_str().unwrap(), Ok(json!({}))));
    }

    #[tokio::test]
    async fn juno_device_error_is_returned() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        let pc = laptop(&b, "dev1", |_| Err("no such app".into()));
        let e = b
            .call(
                None,
                "open_app",
                json!({"id": "x"}),
                SECS,
                &CallOpts::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(e, JunoError::DeviceError("no such app".into()));
        pc.await.unwrap();
    }

    #[tokio::test]
    async fn juno_wrong_devices_result_is_refused() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        polled(&b, "dev2").await;
        let b2 = b.clone();
        let pc = tokio::spawn(async move {
            let call = b2.next("dev1", "Juno", SECS).await.unwrap();
            let id = call["id"].as_str().unwrap().to_owned();
            assert!(!b2.result("dev2", &id, Ok(json!({"text": "forged"}))));
            assert!(!b2.result("dev1", "nope", Ok(json!({"text": "x"}))));
            assert!(b2.result("dev1", &id, Ok(json!({"text": "real"}))));
        });
        let out = b
            .call(
                Some("dev1"),
                "status",
                json!({}),
                SECS,
                &CallOpts::default(),
            )
            .await
            .unwrap();
        assert_eq!(out["text"], "real");
        pc.await.unwrap();
    }

    #[tokio::test]
    async fn juno_most_recent_device_is_chosen() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "old").await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        polled(&b, "new").await;
        let pc = laptop(&b, "new", |_| Ok(json!({"text": "from new"})));
        let out = b
            .call(None, "status", json!({}), SECS, &CallOpts::default())
            .await
            .unwrap();
        assert_eq!(out["text"], "from new");
        pc.await.unwrap();
        let s = b.status();
        assert_eq!(s["devices"][0]["id"], "new");
        assert_eq!(s["devices"][0]["connected"], true);
        assert_eq!(s["devices"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn juno_session_form_of_a_long_id_finds_the_device() {
        let b = Arc::new(JunoBridge::new());
        let long = "d".repeat(64);
        assert!(b.next(&long, "Juno", Duration::ZERO).await.is_none());
        let b2 = b.clone();
        let l2 = long.clone();
        let pc = tokio::spawn(async move {
            let c = b2.next(&l2, "Juno", SECS).await.unwrap();
            b2.result(&l2, c["id"].as_str().unwrap(), Ok(json!({"text": "hi"})))
        });
        let named = "d".repeat(58);
        assert_eq!(
            b.call(
                Some(&named),
                "status",
                json!({}),
                SECS,
                &CallOpts::default()
            )
            .await
            .unwrap()["text"],
            "hi"
        );
        assert!(pc.await.unwrap());
    }

    #[tokio::test]
    async fn juno_offline_when_nobody_polled() {
        let b = JunoBridge::new();
        let e = b
            .call(None, "status", json!({}), SECS, &CallOpts::default())
            .await
            .unwrap_err();
        assert_eq!(e, JunoError::Offline(None));
        assert_eq!(e.to_string(), "Juno is offline (last seen never).");
        polled(&b, "dev1").await;
        let e = b
            .call(
                Some("other"),
                "status",
                json!({}),
                SECS,
                &CallOpts::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, JunoError::Offline(_)));
    }

    #[tokio::test]
    async fn juno_offline_when_not_picked_up() {
        let b = JunoBridge::new();
        polled(&b, "dev1").await;
        // Within the pickup window of a short call: nobody polls, so offline.
        let e = b
            .call(
                Some("dev1"),
                "status",
                json!({}),
                Duration::from_millis(200),
                &CallOpts::default(),
            )
            .await
            .unwrap_err();
        let seen = b.lock().devices["dev1"].seen_at;
        assert_eq!(e, JunoError::Offline(Some(seen)));
        let text = e.to_string();
        assert!(
            text.starts_with("Juno is offline (last seen 20") && text.ends_with("Z)."),
            "{text}"
        );
        assert!(b.lock().calls.is_empty());
        assert!(b.lock().devices["dev1"].queue.is_empty());
    }

    #[tokio::test]
    async fn juno_call_expires_and_its_late_result_is_refused() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        let b2 = b.clone();
        let pc = tokio::spawn(async move { b2.next("dev1", "Juno", SECS).await.unwrap() });
        let e = b
            .call(
                Some("dev1"),
                "wait",
                json!({"timeout_ms": 100}),
                Duration::from_millis(300),
                &CallOpts::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(e, JunoError::Timeout);
        let call = pc.await.unwrap();
        assert!(!b.result(
            "dev1",
            call["id"].as_str().unwrap(),
            Ok(json!({"text": "late"}))
        ));
    }

    #[tokio::test]
    async fn juno_next_times_out_with_no_call() {
        let b = JunoBridge::new();
        let t = Instant::now();
        assert!(b
            .next("dev1", "Juno", Duration::from_millis(50))
            .await
            .is_none());
        assert!(t.elapsed() >= Duration::from_millis(50));
        assert_eq!(b.status()["devices"][0]["name"], "Juno");
    }

    #[tokio::test]
    async fn juno_screenshot_png_is_kept_and_replaced_by_its_src() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        let png = b"\x89PNG\r\n\x1a\nrest-of-png".to_vec();
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let pc = laptop(&b, "dev1", move |_| {
            Ok(json!({"text": "Home, 1920x1080", "image": {"data": data, "mimeType": "image/png"}}))
        });
        let out = b
            .call(None, "screenshot", json!({}), SECS, &CallOpts::default())
            .await
            .unwrap();
        pc.await.unwrap();
        assert!(out["image"].get("data").is_none());
        let src = out["image"]["src"].as_str().unwrap();
        assert!(lucina_store::rich::valid_image_src(src), "{src}");
        let id = src
            .trim_start_matches("/v1/juno/screenshots/")
            .trim_end_matches(".png");
        assert_eq!(*b.screenshot(id).unwrap(), png);
        // Not a PNG: dropped, never kept.
        let v = b.keep_image(json!({"image": {"data": "aGVsbG8="}}));
        assert!(v["image"].get("src").is_none() && v["image"]["error"].is_string());
        for _ in 0..MAX_IMAGES + 2 {
            b.keep_image(
                json!({"image": {"data": base64::engine::general_purpose::STANDARD.encode(&png)}}),
            );
        }
        assert_eq!(b.lock().images.len(), MAX_IMAGES);
        assert!(b.screenshot(id).is_none(), "the oldest went first");
    }
    #[tokio::test]
    async fn juno_next_carries_the_decision_and_internal() {
        let b = Arc::new(JunoBridge::new());
        polled(&b, "dev1").await;
        let pc = laptop(&b, "dev1", |_| Ok(json!({"text": "ok"})));
        let opts = CallOpts {
            decision_id: Some("dc-0123456789ab".into()),
            internal: true,
        };
        b.call(
            None,
            "secret_source",
            json!({"path": "/data/home/k"}),
            SECS,
            &opts,
        )
        .await
        .unwrap();
        let call = pc.await.unwrap().unwrap();
        assert_eq!(call["decisionId"], "dc-0123456789ab");
        assert_eq!(call["internal"], true);
        let pc = laptop(&b, "dev1", |_| Ok(json!({"text": "ok"})));
        b.call(None, "status", json!({}), SECS, &CallOpts::default())
            .await
            .unwrap();
        let call = pc.await.unwrap().unwrap();
        assert!(call.get("decisionId").is_none() && call.get("internal").is_none());
    }

    fn ev(seq: u64, kind: &str, app: &str, subject: &str, fields: Value) -> Value {
        json!({"seq": seq, "src": "devd", "src_seq": seq, "t_ms": 1000 + seq, "kind": kind,
               "app": app, "subject": subject, "fields": fields})
    }

    #[tokio::test]
    async fn juno_events_are_kept_filtered_and_counted() {
        let b = JunoBridge::new();
        assert!(!b.ever_seen());
        assert_eq!(b.device_fact(), None, "never seen: no fact");
        let batch: Vec<Value> = (1..=40)
            .map(|i| {
                ev(
                    i,
                    if i % 2 == 0 {
                        "app.focused"
                    } else {
                        "media.started"
                    },
                    "video",
                    "Film",
                    json!({}),
                )
            })
            .chain([json!("junk"), json!({"seq": 99})])
            .collect();
        assert_eq!(b.add_events("dev1", "Juno (Galaxy Book Go)", &batch, 3), 40);
        assert!(b.ever_seen());
        assert_eq!(b.status()["devices"][0]["newEvents"], 40);
        let q = |kind: Option<&str>, since: Option<u64>, max: usize| EventQuery {
            since,
            kind: kind.map(str::to_owned),
            app: None,
            max,
        };
        let out = b.events(None, &q(Some("app"), Some(30), 3));
        let seqs: Vec<u64> = out["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [36, 38, 40], "newest last, at most max");
        assert_eq!(out["lost"], 5, "3 the laptop lost and 2 dropped here");
        assert_eq!(b.status()["devices"][0]["newEvents"], 0, "read");
        assert_eq!(
            b.events(None, &q(Some("med"), None, 100))["events"],
            json!([]),
            "a kind family is whole words"
        );
        for _ in 0..13 {
            b.add_events(
                "dev1",
                "Juno",
                &(0..40)
                    .map(|i| ev(100 + i, "net.up", "", "", json!({})))
                    .collect::<Vec<_>>(),
                0,
            );
        }
        assert_eq!(b.lock().devices["dev1"].events.len(), MAX_EVENTS);
    }

    #[test]
    fn juno_sticky_state_reads_the_latest_events() {
        let events: VecDeque<Value> = [
            ev(1, "app.focused", "files", "Files", json!({})),
            ev(2, "power.battery", "", "", json!({"percent": 80})),
            ev(3, "media.started", "video", "Film", json!({})),
            ev(4, "app.focused", "video", "Video", json!({})),
            ev(5, "power.battery", "", "", json!({"percent": 64})),
            ev(6, "net.up", "", "", json!({"ssid": "Home"})),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            sticky_state(&events).as_deref(),
            Some("In front: Video; playing Film; on battery 64 %; online (Home)")
        );
        let mut ended = events.clone();
        ended.push_back(ev(7, "media.ended", "video", "Film", json!({})));
        ended.push_back(ev(8, "power.ac", "", "", json!({"percent": 65})));
        assert_eq!(
            sticky_state(&ended).as_deref(),
            Some("In front: Video; charging 65 %; online (Home)")
        );
        assert_eq!(sticky_state(&VecDeque::new()), None);
    }

    #[tokio::test]
    async fn juno_device_fact_says_online_with_its_state() {
        let b = JunoBridge::new();
        polled(&b, "dev1").await;
        b.add_events(
            "dev1",
            "Juno",
            &[ev(1, "app.focused", "video", "Video", json!({}))],
            0,
        );
        assert_eq!(
            b.device_fact().unwrap(),
            "Juno, the owner's laptop, is online: juno_* tools reach it. In front: Video."
        );
        b.lock().devices.get_mut("dev1").unwrap().last_seen =
            Instant::now() - Duration::from_secs(60);
        let fact = b.device_fact().unwrap();
        assert!(
            fact.starts_with("Juno, the owner's laptop, is offline since 20"),
            "{fact}"
        );
        assert!(fact.ends_with("; juno_* tools will say so."), "{fact}");
    }

    fn decision(status: &str, chosen: Option<usize>, context: &str) -> crate::decide::Decision {
        serde_json::from_value(json!({
            "decision_id": "dc-0123456789ab", "record_id": "dc-0123456789ab", "status": status,
            "decision": null, "chosen_option": null, "chosen_index": chosen, "reason": "",
            "decided_by": null, "confidence": null, "asked_owner": true, "owner_only": false,
            "question": "Juno: reboot", "options": ["Go", "Don't"], "recommendation": null,
            "context": context, "impact": "high", "reversible": false, "urgency": "normal",
            "topic": "juno", "asking_client": "lucina", "asking_session": null,
            "created_at": "2026-11-04T00:00:00Z", "decided_at": null, "expires_at": null,
        }))
        .unwrap()
    }

    #[test]
    fn juno_decision_must_be_go_for_the_same_call() {
        let ask = Ask {
            session: "secretary_ios".into(),
            summary: "Juno: reboot".into(),
            context: "cannot be undone".into(),
            hash: "aaa".into(),
            device_hash: Some("ddd".into()),
        };
        let body = decision_body(&ask);
        let r = crate::decide::parse_request(&body).unwrap();
        assert_eq!(r.options, ["Go", "Don't"]);
        assert!(!r.reversible && r.topic.as_deref() == Some("juno") && r.asking_client == "lucina");
        assert!(r.context.contains("Call hash: aaa\nDevice call hash: ddd"));
        let ctx = r.context.as_str();
        assert_eq!(go_for(&decision("decided", Some(1), ctx), "aaa"), Ok(()));
        assert!(
            go_for(&decision("decided", Some(1), ctx), "bbb").is_err(),
            "another call"
        );
        assert!(
            go_for(&decision("decided", Some(2), ctx), "aaa").is_err(),
            "Don't"
        );
        assert!(go_for(&decision("pending", None, ctx), "aaa").is_err());
        assert!(go_for(&decision("decided", Some(1), "Call hash: aaab"), "aaa").is_err());
        let mut other = decision("decided", Some(1), ctx);
        other.topic = Some("deploy".into());
        assert!(go_for(&other, "aaa").is_err());
    }
}
