//! Typed client for Lucina's `/api/secretary/*` contract.
//!
//! A request is described by a [`Call`], built only through validating
//! constructors, so every path, query and body that reaches Lucina has been
//! checked. [`LucinaClient::call`] sends it with the server-held Bearer
//! token, unwraps the `{ok, data}` envelope and classifies failures into
//! [`Error`] (Lucina status/code, unreachable, timeout, decode).

use std::fmt;
use std::time::Duration;

use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::models::{
    Agenda, AgendaQuery, CancelledReminder, ChatReply, ChatRequest, CreateReminder,
    CreatedReminder, Envelope, Health, Job, ReminderList, ReminderQuery, ReminderScope,
    ShoppingItem, TimelinePage, TimelineQuery,
};
use crate::validate;

/// Per-class request deadlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// `POST /chat` (the LLM turn): 120 s by default.
    pub chat: Duration,
    /// Everything else: 15 s by default.
    pub default: Duration,
    /// TCP/TLS connect: 5 s by default.
    pub connect: Duration,
    /// TCP/TLS connect of the timeline event stream: 10 s by default.
    pub stream_connect: Duration,
    /// The event stream ends when Lucina sends nothing (not even its 20 s
    /// `: ping`) for this long: 60 s by default. Also bounds the wait for
    /// the response headers. There is no total deadline on the stream.
    pub stream_idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            chat: Duration::from_secs(120),
            default: Duration::from_secs(15),
            connect: Duration::from_secs(5),
            stream_connect: Duration::from_secs(10),
            stream_idle: Duration::from_secs(60),
        }
    }
}

/// Which deadline a [`Call`] runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutClass {
    Chat,
    Default,
}

/// One validated request against the contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    method: Method,
    segments: Vec<String>,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    class: TimeoutClass,
    /// Unwrap this key of `data` (`{items}` -> array, `{item}` -> item).
    extract: Option<&'static str>,
}

impl Call {
    fn new(method: Method, segments: &[&str]) -> Self {
        Self {
            method,
            segments: segments.iter().map(|s| (*s).to_owned()).collect(),
            query: Vec::new(),
            body: None,
            class: TimeoutClass::Default,
            extract: None,
        }
    }

    fn extracting(mut self, key: &'static str) -> Self {
        self.extract = Some(key);
        self
    }

    fn with_body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    /// `GET /health`.
    pub fn health() -> Self {
        Self::new(Method::GET, &["health"])
    }

    /// `POST /chat`.
    pub fn chat(req: &ChatRequest) -> Result<Self> {
        validate::text("message", &req.message, validate::MAX_MESSAGE_CHARS)?;
        if let Some(sid) = req.session_id.as_deref() {
            validate::session_id("sessionId", sid)?;
        }
        validate::opt_id("clientMessageId", req.client_message_id.as_deref())?;
        if let Some(c) = req.channel.as_deref() {
            validate::chat_channel("channel", c)?;
        }
        let body = serde_json::to_value(req).map_err(|e| Error::Decode(e.to_string()))?;
        let mut call = Self::new(Method::POST, &["chat"]).with_body(body);
        call.class = TimeoutClass::Chat;
        Ok(call)
    }

    /// `GET /jobs/{id}`.
    pub fn job(id: &str) -> Result<Self> {
        validate::id("id", id)?;
        Ok(Self::new(Method::GET, &["jobs", id]))
    }

    /// `GET /reminders?scope=&includeSent=`.
    pub fn reminders(q: &ReminderQuery) -> Result<Self> {
        let mut call = Self::new(Method::GET, &["reminders"]);
        if let Some(scope) = q.scope.as_deref() {
            let scope = ReminderScope::parse(scope).ok_or(Error::Invalid {
                field: "scope",
                message: "must be upcoming, today or all".into(),
            })?;
            call.query.push(("scope", scope.as_str().to_owned()));
        }
        if let Some(f) = q.include_sent.as_deref() {
            call.query
                .push(("includeSent", validate::flag("includeSent", f)?.to_owned()));
        }
        Ok(call)
    }

    /// `POST /reminders` with `{text, dueAt}` or `{naturalText}`.
    pub fn create_reminder(req: &CreateReminder) -> Result<Self> {
        match req {
            CreateReminder::Explicit { text, due_at } => {
                validate::text("text", text, validate::MAX_TEXT_CHARS)?;
                validate::timestamp("dueAt", due_at)?;
            }
            CreateReminder::Natural { natural_text } => {
                validate::text("naturalText", natural_text, validate::MAX_TEXT_CHARS)?;
            }
        }
        let body = serde_json::to_value(req).map_err(|e| Error::Decode(e.to_string()))?;
        Ok(Self::new(Method::POST, &["reminders"]).with_body(body))
    }

    /// `DELETE /reminders/{id}`.
    pub fn cancel_reminder(id: &str) -> Result<Self> {
        validate::id("id", id)?;
        Ok(Self::new(Method::DELETE, &["reminders", id]))
    }

    /// `GET /agenda?date=&refresh=`.
    pub fn agenda(q: &AgendaQuery) -> Result<Self> {
        let mut call = Self::new(Method::GET, &["agenda"]);
        if let Some(d) = q.date.as_deref() {
            validate::date("date", d)?;
            call.query.push(("date", d.to_owned()));
        }
        if let Some(f) = q.refresh.as_deref() {
            call.query
                .push(("refresh", validate::flag("refresh", f)?.to_owned()));
        }
        Ok(call)
    }

    /// `GET /shopping`; Lucina's `{items}` is unwrapped to the array.
    pub fn shopping() -> Self {
        Self::new(Method::GET, &["shopping"]).extracting("items")
    }

    /// `POST /shopping` `{rawName, product?}`; `{item}` is unwrapped.
    pub fn shopping_add(body: Value) -> Result<Self> {
        validate::shopping_item(&body)?;
        Ok(Self::new(Method::POST, &["shopping"])
            .with_body(body)
            .extracting("item"))
    }

    /// `PATCH /shopping/{itemId}` (body passed through; must be an object,
    /// Lucina accepts `{status: needed|purchased}`); `{item}` is unwrapped.
    pub fn shopping_update(item_id: &str, body: Value) -> Result<Self> {
        validate::id("itemId", item_id)?;
        validate::object("body", &body)?;
        Ok(Self::new(Method::PATCH, &["shopping", item_id])
            .with_body(body)
            .extracting("item"))
    }

    /// `GET /timeline?before=&limit=` (newest first, `{items, nextBefore}`).
    pub fn timeline(q: &TimelineQuery) -> Result<Self> {
        let mut call = Self::new(Method::GET, &["timeline"]);
        if let Some(b) = q.before.as_deref() {
            validate::id("before", b)?;
            call.query.push(("before", b.to_owned()));
        }
        if let Some(l) = q.limit.as_deref() {
            let n = validate::bounded_int("limit", l, 1, validate::MAX_TIMELINE_LIMIT)?;
            call.query.push(("limit", n.to_string()));
        }
        Ok(call)
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn timeout_class(&self) -> TimeoutClass {
        self.class
    }

    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    /// Path below `/api/secretary/`, for diagnostics (ids included).
    pub fn path(&self) -> String {
        format!("/api/secretary/{}", self.segments.join("/"))
    }
}

/// Client for Lucina's secretary API. Holds the server-side Lucina token;
/// `Debug` never prints it.
#[derive(Clone)]
pub struct LucinaClient {
    http: reqwest::Client,
    /// For the long-lived event stream: no gzip, its own connect and idle
    /// timeouts, no total deadline.
    stream_http: reqwest::Client,
    base: Url,
    token: String,
    timeouts: Timeouts,
}

impl fmt::Debug for LucinaClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LucinaClient")
            .field("base", &self.base.as_str())
            .field("token", &"<redacted>")
            .field("timeouts", &self.timeouts)
            .finish()
    }
}

impl LucinaClient {
    /// `base_url` is Lucina's origin (e.g. `http://127.0.0.1:18788`); the
    /// `/api/secretary` prefix is added here.
    pub fn new(base_url: &str, token: impl Into<String>, timeouts: Timeouts) -> Result<Self> {
        let base = Url::parse(base_url).map_err(|e| Error::Config(format!("lucina url: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") || base.cannot_be_a_base() {
            return Err(Error::Config("lucina url must be http(s)".into()));
        }
        let token = token.into();
        if token.is_empty() {
            return Err(Error::Config("lucina token is empty".into()));
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("secretary-core/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(timeouts.connect)
            .build()
            .map_err(|e| Error::Config(e.without_url().to_string()))?;
        let stream_http = reqwest::Client::builder()
            .user_agent(concat!("secretary-core/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(timeouts.stream_connect)
            .read_timeout(timeouts.stream_idle)
            .no_gzip()
            .build()
            .map_err(|e| Error::Config(e.without_url().to_string()))?;
        Ok(Self {
            http,
            stream_http,
            base,
            token,
            timeouts,
        })
    }

    pub fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    fn url(&self, call: &Call) -> Url {
        let mut url = self.base.clone();
        {
            // `cannot_be_a_base` was rejected in `new`, so this cannot fail.
            let mut segs = url.path_segments_mut().expect("base url");
            segs.pop_if_empty().extend(["api", "secretary"]);
            segs.extend(call.segments.iter());
        }
        if !call.query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(call.query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        url
    }

    /// Send `call` under its class deadline and decode `data` as `T`
    /// (`serde_json::Value` for a pure passthrough).
    pub async fn call<T: DeserializeOwned>(&self, call: &Call) -> Result<T> {
        Ok(self.call_with_status(call).await?.1)
    }

    /// Like [`Self::call`], also returning Lucina's 2xx status (201 for a
    /// newly created reminder or shopping item, 200 otherwise).
    pub async fn call_with_status<T: DeserializeOwned>(&self, call: &Call) -> Result<(u16, T)> {
        let timeout = match call.class {
            TimeoutClass::Chat => self.timeouts.chat,
            TimeoutClass::Default => self.timeouts.default,
        };
        self.send(call, timeout).await
    }

    /// Like [`Self::call`] with an explicit deadline (e.g. a health probe).
    pub async fn call_with_timeout<T: DeserializeOwned>(
        &self,
        call: &Call,
        timeout: Duration,
    ) -> Result<T> {
        Ok(self.send(call, timeout).await?.1)
    }

    async fn send<T: DeserializeOwned>(&self, call: &Call, timeout: Duration) -> Result<(u16, T)> {
        let mut req = self
            .http
            .request(call.method.clone(), self.url(call))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout);
        if let Some(body) = &call.body {
            req = req.json(body);
        }
        let resp = req.send().await.map_err(Error::from_reqwest)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(Error::from_reqwest)?;
        let data: Value = decode_response(status.as_u16(), &bytes)?;
        let data = match call.extract {
            None => data,
            Some(key) => match data {
                Value::Object(mut m) => m
                    .remove(key)
                    .ok_or_else(|| Error::Decode(format!("data has no `{key}` field")))?,
                _ => return Err(Error::Decode(format!("data is not an object with `{key}`"))),
            },
        };
        let out = serde_json::from_value(data).map_err(|e| Error::Decode(e.to_string()))?;
        Ok((status.as_u16(), out))
    }

    // -- typed helpers ----------------------------------------------------

    pub async fn health(&self) -> Result<Health> {
        self.call(&Call::health()).await
    }

    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatReply> {
        self.call(&Call::chat(req)?).await
    }

    pub async fn job(&self, id: &str) -> Result<Job> {
        self.call(&Call::job(id)?).await
    }

    pub async fn reminders(&self, q: &ReminderQuery) -> Result<ReminderList> {
        self.call(&Call::reminders(q)?).await
    }

    pub async fn create_reminder(&self, req: &CreateReminder) -> Result<CreatedReminder> {
        self.call(&Call::create_reminder(req)?).await
    }

    pub async fn cancel_reminder(&self, id: &str) -> Result<CancelledReminder> {
        self.call(&Call::cancel_reminder(id)?).await
    }

    pub async fn agenda(&self, q: &AgendaQuery) -> Result<Agenda> {
        self.call(&Call::agenda(q)?).await
    }

    pub async fn shopping(&self) -> Result<Vec<ShoppingItem>> {
        self.call(&Call::shopping()).await
    }

    pub async fn timeline(&self, q: &TimelineQuery) -> Result<TimelinePage> {
        self.call(&Call::timeline(q)?).await
    }

    /// Open `GET /timeline/stream` (Server-Sent Events) and return the live
    /// response once Lucina answered 2xx with `text/event-stream`; read it
    /// with `bytes_stream()`. `last_event_id` (an id) is sent as
    /// `Last-Event-ID` so Lucina replays what was missed.
    ///
    /// Non-2xx answers are classified like [`Self::call`]; a body read that
    /// stalls longer than [`Timeouts::stream_idle`] fails with a timeout.
    pub async fn timeline_stream(&self, last_event_id: Option<&str>) -> Result<reqwest::Response> {
        validate::opt_id("lastEventId", last_event_id)?;
        let call = Call::new(Method::GET, &["timeline", "stream"]);
        let mut req = self
            .stream_http
            .get(self.url(&call))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::CACHE_CONTROL, "no-cache");
        if let Some(id) = last_event_id {
            req = req.header("last-event-id", id);
        }
        let resp = req.send().await.map_err(Error::from_reqwest)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let bytes = resp.bytes().await.map_err(Error::from_reqwest)?;
            // Non-2xx always decodes to an error.
            return Err(decode_response::<Value>(status, &bytes)
                .err()
                .unwrap_or(Error::Api {
                    status,
                    code: format!("http_{status}"),
                    message: None,
                }));
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with("text/event-stream"));
        if !is_sse {
            return Err(Error::Decode(
                "timeline stream is not text/event-stream".into(),
            ));
        }
        Ok(resp)
    }
}

/// Envelope handling, separate from I/O so it can be unit-tested.
fn decode_response<T: DeserializeOwned>(status: u16, bytes: &[u8]) -> Result<T> {
    if (200..300).contains(&status) {
        let env: Envelope<Value> =
            serde_json::from_slice(bytes).map_err(|e| Error::Decode(e.to_string()))?;
        if !env.ok {
            return Err(Error::Api {
                status: 502,
                code: env.error.unwrap_or_else(|| "lucina_error".into()),
                message: env.message,
            });
        }
        serde_json::from_value(env.data.unwrap_or(Value::Null))
            .map_err(|e| Error::Decode(e.to_string()))
    } else {
        let (code, message) = match serde_json::from_slice::<Envelope<Value>>(bytes) {
            Ok(env) => (
                env.error.unwrap_or_else(|| format!("http_{status}")),
                env.message,
            ),
            Err(_) => (format!("http_{status}"), None),
        };
        Err(Error::Api {
            status,
            code,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn client(base: &str) -> LucinaClient {
        LucinaClient::new(base, "t", Timeouts::default()).unwrap()
    }

    #[test]
    fn urls_are_built_from_encoded_segments() {
        let c = client("http://127.0.0.1:18788");
        let call = Call::job("j-1").unwrap();
        assert_eq!(
            c.url(&call).as_str(),
            "http://127.0.0.1:18788/api/secretary/jobs/j-1"
        );
        let c = client("http://h:1/prefix/");
        let q = ReminderQuery {
            scope: Some("today".into()),
            include_sent: Some("true".into()),
        };
        assert_eq!(
            c.url(&Call::reminders(&q).unwrap()).as_str(),
            "http://h:1/prefix/api/secretary/reminders?scope=today&includeSent=1"
        );
        let q = TimelineQuery {
            before: Some("tl_9".into()),
            limit: Some("050".into()),
        };
        assert_eq!(
            c.url(&Call::timeline(&q).unwrap()).as_str(),
            "http://h:1/prefix/api/secretary/timeline?before=tl_9&limit=50"
        );
    }

    #[test]
    fn validation_happens_before_sending() {
        assert!(Call::job("../admin").is_err());
        assert!(Call::chat(&ChatRequest {
            message: " ".into(),
            session_id: None,
            client_message_id: None,
            channel: None
        })
        .is_err());
        assert!(Call::agenda(&AgendaQuery {
            date: Some("tomorrow".into()),
            refresh: None
        })
        .is_err());
        assert!(Call::create_reminder(&CreateReminder::Explicit {
            text: "x".into(),
            due_at: "soon".into()
        })
        .is_err());
        assert!(Call::shopping_add(json!("milk")).is_err());
        let chat = |channel: &str| {
            Call::chat(&ChatRequest {
                message: "hi".into(),
                session_id: None,
                client_message_id: None,
                channel: Some(channel.into()),
            })
        };
        assert!(chat("ios").is_ok());
        assert!(chat("telegram").is_err());
        let tq = |before: Option<&str>, limit: Option<&str>| TimelineQuery {
            before: before.map(str::to_owned),
            limit: limit.map(str::to_owned),
        };
        assert!(Call::timeline(&tq(Some("a/b"), None)).is_err());
        assert!(Call::timeline(&tq(None, Some("0"))).is_err());
        assert!(Call::timeline(&tq(None, Some("201"))).is_err());
        assert!(Call::shopping_add(json!({"rawName": "latte"})).is_ok());
        assert_eq!(
            Call::chat(&ChatRequest {
                message: "hi".into(),
                session_id: None,
                client_message_id: None,
                channel: None
            })
            .unwrap()
            .timeout_class(),
            TimeoutClass::Chat
        );
    }

    #[test]
    fn envelope_decoding() {
        let v: Value = decode_response(200, br#"{"ok":true,"data":{"a":1}}"#).unwrap();
        assert_eq!(v, json!({"a": 1}));
        let v: Value = decode_response(200, br#"{"ok":true}"#).unwrap();
        assert_eq!(v, Value::Null);
        match decode_response::<Value>(404, br#"{"ok":false,"error":"not_found"}"#) {
            Err(Error::Api { status, code, .. }) => {
                assert_eq!((status, code.as_str()), (404, "not_found"))
            }
            other => panic!("{other:?}"),
        }
        match decode_response::<Value>(500, b"<html>") {
            Err(Error::Api { status, code, .. }) => {
                assert_eq!((status, code.as_str()), (500, "http_500"))
            }
            other => panic!("{other:?}"),
        }
        match decode_response::<Value>(200, br#"{"ok":false,"error":"busy"}"#) {
            Err(Error::Api { status, code, .. }) => {
                assert_eq!((status, code.as_str()), (502, "busy"))
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            decode_response::<Value>(200, b"nope"),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn debug_redacts_token() {
        let c = LucinaClient::new("http://h", "supersecret", Timeouts::default()).unwrap();
        assert!(!format!("{c:?}").contains("supersecret"));
    }
}
