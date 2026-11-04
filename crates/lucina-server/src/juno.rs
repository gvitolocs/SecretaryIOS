//! The Juno bridge: Lucina's `juno_*` tool calls to the owner's laptop
//! (`docs/research/juno-lucina-tools.md` in LucinaOS, "How a call travels").
//!
//! No port listens on the laptop: `lucina-devd` long-polls secretary-server
//! with its device token, which stamps `device: {id, name}` and forwards here.
//!
//! - `POST /api/secretary/device/juno/next {device, waitSecs}`: registers the
//!   device (its `last_seen`), then waits up to `waitSecs` (≤ 25) for a call
//!   queued for it: `{"call": {id, tool, arguments, deadlineMs}}`, or
//!   `{"call": null}`.
//! - `POST /api/secretary/device/juno/result {device, id, ok, result?, error?}`
//!   (≤ [`MAX_RESULT_BYTES`]): hands the result to the waiting tool call. An
//!   unknown or expired id, or one queued for another device: 404 `unknown_call`.
//! - `GET /api/secretary/juno/screenshots/{id}.png`: a screenshot a call
//!   returned, kept for the reply's image block ([`MAX_IMAGES`], [`IMAGE_TTL`]).
//!
//! [`JunoBridge::call`] queues a call and waits for it: nobody polled in
//! [`ONLINE_WINDOW`], or nobody picked it up in [`PICKUP`]: offline. Each call
//! is logged as `juno_audit` (device, tool, a payload-free summary, ok, ms).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use lucina_tools::tools::juno::{driver_summary, JunoLink};
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

/// Why a call did not return a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JunoError {
    /// No device polled in [`ONLINE_WINDOW`], or nobody picked the call up.
    Offline(String),
    Timeout,
    /// The device ran the tool and reported an error.
    DeviceError(String),
}

impl std::fmt::Display for JunoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JunoError::Offline(why) => write!(f, "Juno is offline ({why})"),
            JunoError::Timeout => f.write_str("Juno did not answer in time"),
            JunoError::DeviceError(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for JunoError {}

struct Device {
    name: String,
    last_seen: Instant,
    queue: VecDeque<String>,
    wake: Arc<Notify>,
}

struct Pending {
    device: String,
    tool: String,
    arguments: Value,
    deadline_ms: i64,
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

fn ago(d: Duration) -> String {
    match d.as_secs() {
        s if s < 120 => format!("last seen {s} s ago"),
        s if s < 7200 => format!("last seen {} min ago", s / 60),
        s => format!("last seen {} h ago", s / 3600),
    }
}

impl JunoBridge {
    pub fn new() -> Self {
        Self::default()
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
            return Err(JunoError::Offline(match named {
                Some(_) => "this laptop has not connected since Lucina started".into(),
                None => "no laptop has connected since Lucina started".into(),
            }));
        };
        let idle = d.last_seen.elapsed();
        if idle > ONLINE_WINDOW {
            return Err(JunoError::Offline(ago(idle)));
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
    ) -> Result<Value, JunoError> {
        let started = Instant::now();
        let summary = driver_summary(tool, &arguments);
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
        let out = self.wait(&id, picked_rx, done_rx, timeout).await;
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
        id: &str,
        picked: oneshot::Receiver<()>,
        done: oneshot::Receiver<Result<Value, String>>,
        timeout: Duration,
    ) -> Result<Value, JunoError> {
        let started = Instant::now();
        let pickup = PICKUP.min(timeout);
        if !matches!(tokio::time::timeout(pickup, picked).await, Ok(Ok(()))) {
            return Err(JunoError::Offline(format!(
                "call {id} was not picked up in {} s",
                pickup.as_secs()
            )));
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
                    .or_insert_with(|| Device {
                        name: String::new(),
                        last_seen: Instant::now(),
                        queue: VecDeque::new(),
                        wake: Arc::new(Notify::new()),
                    });
                d.name = name.to_owned();
                d.last_seen = Instant::now();
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
                    return Some(json!({
                        "id": id,
                        "tool": p.tool,
                        "arguments": p.arguments,
                        "deadlineMs": p.deadline_ms,
                    }));
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
            json!({"id": id, "name": d.name, "connected": idle <= ONLINE_WINDOW, "lastSeenSecs": idle.as_secs()})
        }).collect::<Vec<_>>()})
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

fn audit(device: &str, tool: &str, summary: &str, ok: bool, started: Instant) {
    tracing::info!(
        target: "juno_audit",
        device,
        tool,
        summary,
        ok,
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
    ) -> Result<Value, String> {
        JunoBridge::call(self, device.as_deref(), tool, args, timeout)
            .await
            .map_err(|e| e.to_string())
    }

    fn status(&self) -> Value {
        JunoBridge::status(self)
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
        Some("next") | Some("result") if method != Method::POST => {
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
            Ok(ok(json!({ "call": call })))
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
            .call(Some("dev1"), "open_app", json!({"id": "gallery"}), SECS)
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
            .call(None, "open_app", json!({"id": "x"}), SECS)
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
            .call(Some("dev1"), "status", json!({}), SECS)
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
        let out = b.call(None, "status", json!({}), SECS).await.unwrap();
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
            b.call(Some(&named), "status", json!({}), SECS)
                .await
                .unwrap()["text"],
            "hi"
        );
        assert!(pc.await.unwrap());
    }

    #[tokio::test]
    async fn juno_offline_when_nobody_polled() {
        let b = JunoBridge::new();
        let e = b.call(None, "status", json!({}), SECS).await.unwrap_err();
        assert!(matches!(e, JunoError::Offline(_)), "{e:?}");
        assert!(e.to_string().starts_with("Juno is offline"));
        polled(&b, "dev1").await;
        let e = b
            .call(Some("other"), "status", json!({}), SECS)
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
            )
            .await
            .unwrap_err();
        assert!(
            matches!(e, JunoError::Offline(ref m) if m.contains("picked up")),
            "{e:?}"
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
        let out = b.call(None, "screenshot", json!({}), SECS).await.unwrap();
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
}
