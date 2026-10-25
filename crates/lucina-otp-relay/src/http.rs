//! The thin HTTP layer: tokens, routing, the `paused` file and the
//! `tiny_http` loop around [`Store`].
//!
//! [`Relay::handle`] maps one request to one [`Reply`] without touching the
//! network, so every route and status code is unit-tested; [`serve`] only
//! reads requests, stamps the clock and writes replies.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lucina_phone_core::proto::{Incoming, MAX_BODY_BYTES};
use serde_json::{json, Value};

use crate::store::{Store, DEFAULT_ARM_SECONDS};

/// Where Caddy forwards `https://getsecretary.tech/v1/phone/otp/*`.
pub const LISTEN: &str = "127.0.0.1:18866";
/// State directory when `LUCINA_OTP_STATE_DIR` is unset.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/lucina-otp";
/// Path prefix Caddy normally strips; accepted too.
const PREFIX: &str = "/v1/phone/otp";
/// Tokens shorter than this are refused at start.
pub const MIN_TOKEN_CHARS: usize = 40;

/// Unix ms.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A new window id: 24 random bytes as lower-case hex.
pub fn random_window_id() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why the relay cannot start. Never carries a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// `owner.token` or `phone.token` cannot be read.
    MissingToken(&'static str),
    /// A token is shorter than [`MIN_TOKEN_CHARS`], or both are the same.
    InvalidTokens,
    /// The listener cannot bind.
    Listen(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToken(name) => write!(f, "{name} pairing missing"),
            Self::InvalidTokens => f.write_str("invalid or identical pairing tokens"),
            Self::Listen(addr) => write!(f, "cannot listen on {addr}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Environment: `LUCINA_OTP_STATE_DIR` and `LUCINA_OTP_CONTINUOUS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Holds `owner.token`, `phone.token` and `paused`.
    pub state_dir: PathBuf,
    /// `LUCINA_OTP_CONTINUOUS=true`.
    pub continuous: bool,
}

impl Config {
    /// The configuration from the environment.
    pub fn from_env() -> Self {
        Self {
            state_dir: std::env::var_os("LUCINA_OTP_STATE_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_STATE_DIR), PathBuf::from),
            continuous: std::env::var("LUCINA_OTP_CONTINUOUS").as_deref() == Ok("true"),
        }
    }
}

/// Constant-time comparison (the length is not secret).
fn equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (a, b)| x | (a ^ b)) == 0
}

/// The two bearer tokens. `Debug` hides them.
struct Tokens {
    owner: String,
    phone: String,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens(..)")
    }
}

impl Tokens {
    fn load(dir: &Path) -> Result<Self, StartError> {
        let read = |name: &'static str| {
            fs::read_to_string(dir.join(name))
                .map(|t| t.trim().to_owned())
                .map_err(|_| StartError::MissingToken(name))
        };
        let tokens = Self {
            owner: read("owner.token")?,
            phone: read("phone.token")?,
        };
        if tokens.owner.len() < MIN_TOKEN_CHARS
            || tokens.phone.len() < MIN_TOKEN_CHARS
            || equal(&tokens.owner, &tokens.phone)
        {
            return Err(StartError::InvalidTokens);
        }
        Ok(tokens)
    }
}

/// A request body as read from the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body<'a> {
    Bytes(&'a [u8]),
    /// Longer than [`MAX_BODY_BYTES`].
    TooLarge,
    /// The connection failed while reading.
    Unreadable,
}

/// One request, already read.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub method: &'a str,
    /// May include the `/v1/phone/otp` prefix and a query.
    pub path: &'a str,
    /// The `Authorization` header, empty when absent.
    pub authorization: &'a str,
    pub body: Body<'a>,
}

/// One response. Every response is JSON with `Cache-Control: no-store`;
/// 204 has an empty body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    fn json(status: u16, value: &impl serde::Serialize) -> Self {
        Self {
            status,
            body: serde_json::to_string(value).expect("relay bodies serialize"),
        }
    }

    fn error(status: u16, error: &str) -> Self {
        Self::json(status, &json!({ "error": error }))
    }
}

/// The relay: the store plus what the HTTP layer needs around it.
#[derive(Debug)]
pub struct Relay {
    store: Store,
    tokens: Tokens,
    paused_path: PathBuf,
    new_window_id: fn() -> String,
}

impl Relay {
    /// Loads the tokens, reads the `paused` file and arms continuous
    /// forwarding when it is configured and not paused.
    pub fn open(config: &Config, now: u64) -> Result<Self, StartError> {
        let tokens = Tokens::load(&config.state_dir)?;
        let paused_path = config.state_dir.join("paused");
        let mut store = Store::new(config.continuous, paused_path.exists());
        if store.starts_armed() {
            store.arm(0, now, random_window_id());
        }
        Ok(Self {
            store,
            tokens,
            paused_path,
            new_window_id: random_window_id,
        })
    }

    /// The store, for inspection.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Answers one request at time `now`.
    pub fn handle(&mut self, req: &Request<'_>, now: u64) -> Reply {
        let path = req.path.split('?').next().unwrap_or("");
        let path = path.strip_prefix(PREFIX).unwrap_or(path);
        let token = if matches!(path, "/window" | "/incoming") {
            &self.tokens.phone
        } else {
            &self.tokens.owner
        };
        let authorized = req
            .authorization
            .strip_prefix("Bearer ")
            .is_some_and(|t| equal(t, token));
        if !authorized {
            return Reply::error(401, "unauthorized");
        }
        let body = match req.body {
            Body::Bytes(b) => b,
            Body::TooLarge => return Reply::error(413, "body_limit"),
            Body::Unreadable => return Reply::error(400, "invalid_request"),
        };
        match (req.method, path) {
            ("GET", "/status") => Reply::json(200, &self.store.status(now)),
            ("GET", "/window") => Reply::json(200, &self.store.window(now)),
            ("POST", "/arm") => {
                let seconds = if body.iter().all(u8::is_ascii_whitespace) {
                    DEFAULT_ARM_SECONDS
                } else {
                    match serde_json::from_slice::<Value>(body) {
                        Ok(v) => v["seconds"].as_u64().unwrap_or(DEFAULT_ARM_SECONDS),
                        Err(_) => return Reply::error(400, "invalid_json"),
                    }
                };
                if self.store.continuous() {
                    let _ = fs::remove_file(&self.paused_path);
                }
                let window = self.store.arm(seconds, now, (self.new_window_id)());
                Reply::json(200, &window)
            }
            ("POST", "/disarm") => {
                if self.store.continuous() && fs::write(&self.paused_path, b"paused\n").is_err() {
                    return Reply::error(503, "pause_unavailable");
                }
                self.store.disarm();
                Reply::json(200, &json!({ "disarmed": true }))
            }
            ("GET", "/otp") => match self.store.take(now) {
                Some(code) => Reply::json(200, &code),
                None => Reply {
                    status: 204,
                    body: String::new(),
                },
            },
            ("POST", "/incoming") => {
                let Ok(event) = serde_json::from_slice::<Incoming>(body) else {
                    return Reply::error(400, "invalid_request");
                };
                match self.store.receive(&event, now) {
                    Ok(ack) => Reply::json(200, &ack),
                    Err(refusal) => Reply::error(refusal.status(), refusal.error()),
                }
            }
            _ => Reply::error(404, "not_found"),
        }
    }
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name, value).expect("static header")
}

/// Serves requests until the listener closes.
pub fn serve(server: &tiny_http::Server, relay: &mut Relay) {
    for mut req in server.incoming_requests() {
        let method = req.method().as_str().to_owned();
        let path = req.url().to_owned();
        let authorization = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_owned())
            .unwrap_or_default();
        let mut bytes = Vec::new();
        let body = if req.body_length().is_some_and(|n| n > MAX_BODY_BYTES) {
            Body::TooLarge
        } else if req
            .as_reader()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            Body::Unreadable
        } else if bytes.len() > MAX_BODY_BYTES {
            Body::TooLarge
        } else {
            Body::Bytes(&bytes)
        };
        let reply = relay.handle(
            &Request {
                method: &method,
                path: &path,
                authorization: &authorization,
                body,
            },
            now_ms(),
        );
        let response = tiny_http::Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(header("Content-Type", "application/json"))
            .with_header(header("Cache-Control", "no-store"));
        let _ = req.respond(response);
    }
}

/// Opens the relay from `config` and serves on `addr`.
pub fn run(config: &Config, addr: &str) -> Result<(), StartError> {
    let mut relay = Relay::open(config, now_ms())?;
    let server = tiny_http::Server::http(addr).map_err(|_| StartError::Listen(addr.into()))?;
    serve(&server, &mut relay);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_phone_core::proto::{Delivered, IncomingAck, RelayStatus, WindowState};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NOW: u64 = 1_700_000_000_000;
    const OWNER: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const PHONE: &str = "0000000000000000000000000000000000000000000000000000000000000002";
    const WINDOW: &str = "00112233445566778899aabbccddeeff0011223344556677";

    fn temp_dir() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lucina-otp-relay-unit-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn relay(continuous: bool) -> (Relay, PathBuf) {
        let dir = temp_dir();
        fs::write(dir.join("owner.token"), format!("{OWNER}\n")).unwrap();
        fs::write(dir.join("phone.token"), PHONE).unwrap();
        let config = Config {
            state_dir: dir.clone(),
            continuous,
        };
        let mut relay = Relay::open(&config, NOW).unwrap();
        relay.new_window_id = || WINDOW.into();
        (relay, dir)
    }

    fn call(
        relay: &mut Relay,
        method: &str,
        path: &str,
        token: &str,
        body: &str,
        now: u64,
    ) -> Reply {
        let auth = format!("Bearer {token}");
        relay.handle(
            &Request {
                method,
                path,
                authorization: &auth,
                body: Body::Bytes(body.as_bytes()),
            },
            now,
        )
    }

    fn value(reply: &Reply) -> Value {
        serde_json::from_str(&reply.body).unwrap()
    }

    #[test]
    fn start_requires_distinct_long_tokens() {
        let dir = temp_dir();
        let config = Config {
            state_dir: dir.clone(),
            continuous: false,
        };
        assert_eq!(
            Relay::open(&config, NOW).unwrap_err(),
            StartError::MissingToken("owner.token")
        );
        fs::write(dir.join("owner.token"), OWNER).unwrap();
        fs::write(dir.join("phone.token"), OWNER).unwrap();
        assert_eq!(
            Relay::open(&config, NOW).unwrap_err(),
            StartError::InvalidTokens
        );
        fs::write(dir.join("phone.token"), "short").unwrap();
        assert_eq!(
            Relay::open(&config, NOW).unwrap_err(),
            StartError::InvalidTokens
        );
        fs::write(dir.join("phone.token"), PHONE).unwrap();
        let relay = Relay::open(&config, NOW).unwrap();
        assert!(!format!("{relay:?}").contains(OWNER));
        assert!(!StartError::InvalidTokens.to_string().contains(OWNER));
    }

    #[test]
    fn tokens_are_per_route() {
        let (mut r, _) = relay(false);
        for (method, path, token) in [
            ("GET", "/status", PHONE),
            ("POST", "/arm", PHONE),
            ("POST", "/disarm", PHONE),
            ("GET", "/otp", PHONE),
            ("GET", "/window", OWNER),
            ("POST", "/incoming", OWNER),
            ("GET", "/nothing", PHONE),
            ("GET", "/status", ""),
        ] {
            let reply = call(&mut r, method, path, token, "", NOW);
            assert_eq!(reply.status, 401, "{method} {path}");
            assert_eq!(value(&reply), json!({"error": "unauthorized"}));
        }
        let no_scheme = r.handle(
            &Request {
                method: "GET",
                path: "/status",
                authorization: OWNER,
                body: Body::Bytes(b""),
            },
            NOW,
        );
        assert_eq!(no_scheme.status, 401);
        assert_eq!(call(&mut r, "GET", "/nothing", OWNER, "", NOW).status, 404);
        assert_eq!(call(&mut r, "DELETE", "/otp", OWNER, "", NOW).status, 404);
        assert_eq!(
            call(&mut r, "GET", "/v1/phone/otp/status?x=1", OWNER, "", NOW).status,
            200
        );
    }

    #[test]
    fn body_limits() {
        let (mut r, _) = relay(false);
        let auth = format!("Bearer {PHONE}");
        for (body, status) in [(Body::TooLarge, 413), (Body::Unreadable, 400)] {
            let reply = r.handle(
                &Request {
                    method: "POST",
                    path: "/incoming",
                    authorization: &auth,
                    body,
                },
                NOW,
            );
            assert_eq!(reply.status, status);
        }
        for bad in ["", "{", "[]", r#"{"eventId":"0123456789abcdef"}"#] {
            assert_eq!(
                call(&mut r, "POST", "/incoming", PHONE, bad, NOW).status,
                400,
                "{bad}"
            );
        }
        assert_eq!(call(&mut r, "POST", "/arm", OWNER, "{", NOW).status, 400);
    }

    #[test]
    fn protocol_one_flow_as_app_0_4_0() {
        let (mut r, _) = relay(false);
        let status: RelayStatus =
            serde_json::from_str(&call(&mut r, "GET", "/status", OWNER, "", NOW).body).unwrap();
        assert!(!status.armed && status.protocol == 2);
        assert_eq!(
            value(&call(&mut r, "GET", "/window", PHONE, "", NOW)),
            json!({"armed": false, "expiresAt": null, "continuous": false})
        );
        // Lucina arms with `{"seconds":300}`.
        let armed = call(&mut r, "POST", "/arm", OWNER, r#"{"seconds":300}"#, NOW);
        assert_eq!(
            value(&armed),
            json!({"armed": true, "windowId": WINDOW, "expiresAt": NOW + 300_000, "continuous": false})
        );
        let window: WindowState =
            serde_json::from_str(&call(&mut r, "GET", "/window", PHONE, "", NOW).body).unwrap();
        assert_eq!(window.window_id.as_deref(), Some(WINDOW));
        // NetForwarder.java: JSONObject with windowId, eventId (UUID), code, sender.
        let body = format!(
            r#"{{"windowId":"{WINDOW}","eventId":"1b4e28ba-2fa1-11d2-883f-0016d3cca427","code":"482917","sender":"Bank\/Info"}}"#
        );
        let ack = call(&mut r, "POST", "/incoming", PHONE, &body, NOW + 5);
        assert_eq!((ack.status, value(&ack)), (200, json!({"accepted": true})));
        let dup = call(&mut r, "POST", "/incoming", PHONE, &body, NOW + 6);
        assert_eq!(value(&dup), json!({"accepted": true, "duplicate": true}));
        let wrong = body.replace(WINDOW, "ffffffffffffffffffff");
        let wrong = wrong.replace("cca427", "cca428");
        assert_eq!(
            call(&mut r, "POST", "/incoming", PHONE, &wrong, NOW).status,
            409
        );
        let got = call(&mut r, "GET", "/otp", OWNER, "", NOW + 7);
        let d: Delivered = serde_json::from_str(&got.body).unwrap();
        assert_eq!(
            (d.code.as_str(), d.sender.as_str(), d.received_at),
            ("482917", "Bank/Info", NOW + 5)
        );
        assert_eq!(d.expires_at, NOW + 5 + 120_000);
        assert_eq!(value(&got)["source"], "sms");
        let empty = call(&mut r, "GET", "/otp", OWNER, "", NOW + 8);
        assert_eq!((empty.status, empty.body.as_str()), (204, ""));
        let disarmed = call(&mut r, "POST", "/disarm", OWNER, r#"{"seconds":0}"#, NOW);
        assert_eq!(value(&disarmed), json!({"disarmed": true}));
        assert_eq!(
            call(
                &mut r,
                "POST",
                "/incoming",
                PHONE,
                &body.replace("cca427", "cca429"),
                NOW
            )
            .status,
            409
        );
        // Without a body the window is 180 s.
        assert_eq!(
            value(&call(&mut r, "POST", "/arm", OWNER, "", NOW))["expiresAt"],
            NOW + 180_000
        );
    }

    #[test]
    fn protocol_two_codes() {
        let (mut r, _) = relay(false);
        call(&mut r, "POST", "/arm", OWNER, r#"{"seconds":300}"#, NOW);
        let event = |id: u32, received: u64, extra: &str| {
            format!(
                r#"{{"eventId":"8c1f0a52-7d2e-4f0b-9a51-{id:012}","code":"X8H2K9","sender":"Bank","receivedAt":{received},"source":"notification"{extra}}}"#
            )
        };
        let test = call(
            &mut r,
            "POST",
            "/incoming",
            PHONE,
            &event(1, NOW, r#","test":true"#),
            NOW,
        );
        assert_eq!(
            (test.status, value(&test)),
            (200, json!({"accepted": true, "test": true}))
        );
        assert_eq!(call(&mut r, "GET", "/otp", OWNER, "", NOW).status, 204);
        let old = call(
            &mut r,
            "POST",
            "/incoming",
            PHONE,
            &event(2, NOW - 120_000, ""),
            NOW,
        );
        assert_eq!(
            (old.status, value(&old)),
            (410, json!({"error": "expired"}))
        );
        let ok = call(
            &mut r,
            "POST",
            "/incoming",
            PHONE,
            &event(3, NOW - 30_000, ""),
            NOW,
        );
        assert_eq!(value(&ok), json!({"accepted": true}));
        let got = value(&call(&mut r, "GET", "/otp", OWNER, "", NOW + 1));
        assert_eq!(
            got,
            json!({"sender": "Bank", "code": "X8H2K9", "receivedAt": NOW - 30_000, "expiresAt": NOW + 90_000, "source": "notification"})
        );
        // Past its expiry a code is not delivered.
        call(&mut r, "POST", "/incoming", PHONE, &event(4, NOW, ""), NOW);
        assert_eq!(
            call(&mut r, "GET", "/otp", OWNER, "", NOW + 120_000).status,
            204
        );
    }

    #[test]
    fn continuous_pause_file() {
        let (mut r, dir) = relay(true);
        let paused = dir.join("paused");
        let status = value(&call(&mut r, "GET", "/status", OWNER, "", NOW));
        assert_eq!(
            (status["armed"].clone(), status["continuous"].clone()),
            (json!(true), json!(true))
        );
        let w = value(&call(&mut r, "GET", "/window", PHONE, "", NOW));
        assert!(w["armed"] == true && w["continuous"] == true && w["expiresAt"].is_null());
        assert_eq!(call(&mut r, "POST", "/disarm", OWNER, "", NOW).status, 200);
        assert!(paused.exists());
        assert_eq!(
            value(&call(&mut r, "GET", "/status", OWNER, "", NOW))["armed"],
            false
        );
        // A restart stays paused.
        let config = Config {
            state_dir: dir.clone(),
            continuous: true,
        };
        let mut restarted = Relay::open(&config, NOW).unwrap();
        assert!(!restarted.store().armed(NOW));
        let armed = value(&call(
            &mut restarted,
            "POST",
            "/arm",
            OWNER,
            r#"{"seconds":300}"#,
            NOW,
        ));
        assert!(armed["continuous"] == true && armed["expiresAt"].is_null());
        assert!(!paused.exists());
        assert!(Relay::open(&config, NOW).unwrap().store().armed(NOW));
        // The pause cannot be written: nothing changes, 503.
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            call(&mut restarted, "POST", "/disarm", OWNER, "", NOW).status,
            503
        );
        assert!(restarted.store().armed(NOW));
        let ack: IncomingAck = serde_json::from_str(
            &call(
                &mut restarted,
                "POST",
                "/incoming",
                PHONE,
                r#"{"eventId":"0123456789abcdef","code":"482917"}"#,
                NOW,
            )
            .body,
        )
        .unwrap();
        assert!(ack.accepted && !ack.duplicate);
    }
}
