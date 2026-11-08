//! LucinaDesk on p1 (`src/desk.rs`): minting for owner devices only, and the socket
//! spliced to Lucina with the server's bearer and the ticket subprotocol, against a mock
//! Lucina.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

const LUCINA_TOKEN: &str = "lucina-server-secret";
const TICKET: &str = "Zk3aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa43";

/// What Lucina's socket saw: `Authorization`, `Sec-WebSocket-Protocol`, `Cookie`.
type SocketSeen = (Option<String>, Option<String>, Option<String>);

#[derive(Default)]
struct Mock {
    down: AtomicBool,
    minted: Mutex<Vec<(Option<String>, Value)>>,
    sockets: Mutex<Vec<SocketSeen>>,
}

fn header(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

async fn mint(State(m): State<Arc<Mock>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    m.minted.lock().unwrap().push((header(&headers, "authorization"), body.clone()));
    if m.down.load(Ordering::Relaxed) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "desk_unavailable", "message": "gateway down"})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "data": {
        "ticket": TICKET, "protocol": "lucinadesk.v1", "expiresInSecs": 30,
        "sandbox": "workspace", "mode": body["mode"],
    }}))
    .into_response()
}

async fn socket(State(m): State<Arc<Mock>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let proto = header(&headers, "sec-websocket-protocol");
    m.sockets.lock().unwrap().push((
        header(&headers, "authorization"),
        proto.clone(),
        header(&headers, "cookie"),
    ));
    if !proto.unwrap_or_default().contains(&format!("ticket.{TICKET}")) {
        return (StatusCode::GONE, Json(json!({"ok": false, "error": "ticket_refused"}))).into_response();
    }
    ws.protocols(["lucinadesk.v1"]).on_upgrade(|mut s| async move {
        let _ = s.send(AxMessage::Text(r#"{"type":"hello","v":1}"#.into())).await;
        while let Some(Ok(m)) = s.recv().await {
            if let AxMessage::Text(t) = m {
                let _ = s.send(AxMessage::Text(format!("echo {}", t.as_str()).into())).await;
            }
        }
    })
}

struct H {
    base: String,
    http: reqwest::Client,
    token: String,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let mock = Arc::new(Mock::default());
    let lucina = Router::new()
        .route("/api/secretary/computer/desk", post(mint))
        .route("/api/secretary/computer/desk/ws", any(socket))
        // The door, with the house at the test client's address.
        .route(
            "/api/secretary/computer/lan",
            get(|| async { Json(json!({"ok": true, "data": {"url": "https://lan.getsecretary.tech:9443", "publicIp": "127.0.0.1"}})) }),
        )
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, lucina).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test iPhone").unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move { serve_on(l, app(state), std::future::pending()).await.unwrap() });
    H { base, http: reqwest::Client::new(), token, mock, _dir: dir }
}

impl H {
    async fn mint(&self, body: Value, auth: bool) -> (u16, Value) {
        let mut rb = self.http.post(format!("{}/v1/computer/desk", self.base)).json(&body);
        if auth {
            rb = rb.bearer_auth(&self.token);
        }
        let r = rb.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn connect(&self, protocols: &str) -> Result<(tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String), u16> {
        let mut req = format!("{}/v1/computer/desk/ws", self.base.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("sec-websocket-protocol", protocols.parse().unwrap());
        req.headers_mut().insert("cookie", "secretary_session=s3cret".parse().unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, resp)) => Ok((ws, header(resp.headers(), "sec-websocket-protocol").unwrap_or_default())),
            Err(tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
            Err(e) => panic!("{e}"),
        }
    }
}

#[tokio::test]
async fn desk_tickets_for_owner_devices_only() {
    let h = harness().await;
    assert_eq!(h.mint(json!({"mode": "control"}), false).await.0, 401);
    assert!(h.mock.minted.lock().unwrap().is_empty(), "Lucina never asked without a device");

    let (s, b) = h.mint(json!({"mode": "control", "device": "forged", "codecs": ["vp9", "h264", 3]}), true).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["ticket"], TICKET);
    assert_eq!(b["protocol"], "lucinadesk.v1");
    assert_eq!(b["wsPath"], "/v1/computer/desk/ws");
    assert_eq!(b["expiresInSecs"], 30);
    assert_eq!(b["mode"], "control");
    assert_eq!(b["sandbox"], "workspace");
    assert_eq!(b["lan"], json!({"url": "https://lan.getsecretary.tech:9443"}), "at home: the door");
    let (auth, body) = h.mock.minted.lock().unwrap().last().cloned().unwrap();
    assert_eq!(auth.as_deref(), Some(&*format!("Bearer {LUCINA_TOKEN}")));
    assert_eq!(body["device"], "test iPhone", "the device is the authenticated one, not the body's");
    assert_eq!(body["mode"], "control");
    assert_eq!(body["codecs"], json!(["vp9", "h264"]), "the browser's codecs go to Lucina");

    let r = h
        .http
        .post(format!("{}/v1/computer/desk", h.base))
        .bearer_auth(&h.token)
        .header("x-secretary-caller", "mcp:claude")
        .json(&json!({"mode": "watch"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert!(r.json::<Value>().await.unwrap()["lan"].is_null(), "agents never get the door");

    let (s, b) = h.mint(json!({"sandbox": "android"}), true).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("desk_unsupported")));
    let (s, b) = h.mint(json!({"mode": "drive"}), true).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("bad_request")));

    h.mock.down.store(true, Ordering::Relaxed);
    let (s, b) = h.mint(json!({}), true).await;
    assert_eq!((s, b["error"]["code"].as_str()), (503, Some("desk_unavailable")), "{b}");
}

#[tokio::test]
async fn the_socket_is_spliced_with_the_ticket_and_without_cookies() {
    let h = harness().await;
    // A refused ticket fails the handshake with Lucina's 410.
    assert_eq!(h.connect("lucinadesk.v1, ticket.nope").await.err(), Some(410));
    let plain = h.http.get(format!("{}/v1/computer/desk/ws", h.base)).send().await.unwrap();
    assert_eq!(plain.status().as_u16(), 400, "only WebSocket upgrades");

    let (mut ws, proto) = h.connect(&format!("lucinadesk.v1, ticket.{TICKET}")).await.expect("open");
    assert_eq!(proto, "lucinadesk.v1");
    let Some(Ok(Message::Text(hello))) = ws.next().await else { panic!("no hello") };
    assert_eq!(hello.as_str(), r#"{"type":"hello","v":1}"#);
    ws.send(Message::text(r#"{"type":"ack"}"#)).await.unwrap();
    let Some(Ok(Message::Text(echo))) = ws.next().await else { panic!("no echo") };
    assert_eq!(echo.as_str(), r#"echo {"type":"ack"}"#);

    let seen = h.mock.sockets.lock().unwrap().clone();
    for (auth, proto, cookie) in &seen {
        assert_eq!(auth.as_deref(), Some(&*format!("Bearer {LUCINA_TOKEN}")), "the server's bearer");
        assert!(proto.as_deref().unwrap_or("").contains("lucinadesk.v1"));
        assert!(cookie.is_none(), "the owner's cookies never reach Lucina");
    }
    assert_eq!(seen.len(), 2);
}
