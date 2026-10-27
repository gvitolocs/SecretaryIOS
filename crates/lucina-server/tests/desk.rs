//! LucinaDesk end to end (`crate::desk`): tickets, the socket, the takeover lease and
//! the audit, against a fake `lucinadesk-gateway` on a Unix socket.

mod common;

use std::time::Duration;

use common::{harness, script, TOKEN};
use futures_util::{SinkExt, StreamExt};
use lucina_computer_use::{control, Cancel, Error, SandboxId};
use serde_json::{json, Value};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const SB: SandboxId = SandboxId::Workspace;

/// A gateway that says `login`, sends one video frame, and reports what it received.
fn fake_gateway(listener: UnixListener) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = listener.accept().await else { return };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
                ws.send(Message::text(r#"{"type":"login","hostname":"lucina-workspace"}"#))
                    .await
                    .unwrap();
                ws.send(Message::binary(vec![1u8, 1, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xAA]))
                    .await
                    .unwrap();
                while let Some(Ok(m)) = ws.next().await {
                    match m {
                        Message::Text(t) => {
                            let _ = tx.send(t.as_str().to_owned());
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                let _ = tx.send("<closed>".into());
            });
        }
    });
    rx
}

async fn mint(h: &common::H, mode: &str, bearer: bool) -> (u16, Value) {
    let mut rb = h
        .http
        .post(h.url("/computer/desk"))
        .json(&json!({"sandbox": "workspace", "mode": mode, "device": "test iPhone"}));
    if bearer {
        rb = rb.bearer_auth(TOKEN);
    }
    let r = rb.send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

async fn connect(h: &common::H, protocols: &str, bearer: bool) -> Result<Client, u16> {
    let url = h.url("/computer/desk/ws").replace("http://", "ws://");
    let mut req = url.into_client_request().unwrap();
    if !protocols.is_empty() {
        req.headers_mut()
            .insert("sec-websocket-protocol", protocols.parse().unwrap());
    }
    if bearer {
        req.headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, resp)) => {
            assert_eq!(resp.headers()["sec-websocket-protocol"], "lucinadesk.v1");
            Ok(ws)
        }
        Err(tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

async fn next_text(ws: &mut Client) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message in time")
            .expect("open")
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

async fn next_binary(ws: &mut Client) -> Vec<u8> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message in time")
            .expect("open")
            .unwrap();
        if let Message::Binary(b) = m {
            return b.to_vec();
        }
    }
}

/// The close code, skipping whatever arrives before it.
async fn close_code(ws: &mut Client, within: Duration) -> u16 {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("closed in time");
        match m {
            Some(Ok(Message::Close(Some(f)))) => return u16::from(f.code),
            Some(Ok(_)) => continue,
            other => panic!("no close frame: {other:?}"),
        }
    }
}

async fn wait_until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

fn recent_event(event: &str) -> Option<Value> {
    control()
        .recent(SB, 50)
        .into_iter()
        .find(|e| e["event"] == event)
}

/// One test: the lease and the audit ring are process-wide.
#[tokio::test]
async fn desk_tickets_lease_and_audit() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let sock = h.dir.path().join("gateway.sock");
    std::env::set_var("LUCINA_DESK_GATEWAY", &sock);

    // No gateway yet: 503 desk_unavailable.
    let (s, b) = mint(&h, "watch", true).await;
    assert_eq!((s, b["error"].as_str()), (503, Some("desk_unavailable")), "{b}");
    let mut gw = fake_gateway(UnixListener::bind(&sock).unwrap());

    // Auth: no bearer, no ticket, a bad ticket, the wrong sandbox.
    assert_eq!(mint(&h, "watch", false).await.0, 401);
    let (s, b) = mint(&h, "drive", true).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("bad_request")));
    let r = h
        .http
        .post(h.url("/computer/desk"))
        .bearer_auth(TOKEN)
        .json(&json!({"sandbox": "android"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "desk_unsupported");
    let (s, b) = mint(&h, "watch", true).await;
    assert_eq!(s, 200, "{b}");
    let ticket = b["data"]["ticket"].as_str().unwrap().to_owned();
    assert_eq!(ticket.len(), 43);
    assert_eq!(b["data"]["protocol"], "lucinadesk.v1");
    assert_eq!(b["data"]["expiresInSecs"], 30);
    let proto = format!("lucinadesk.v1, ticket.{ticket}");
    assert_eq!(connect(&h, &proto, false).await.err(), Some(401), "the bearer still applies");
    assert_eq!(connect(&h, "lucinadesk.v1", true).await.err(), Some(410), "no ticket");
    assert_eq!(
        connect(&h, &format!("lucinadesk.v1, ticket.{}", "A".repeat(43)), true).await.err(),
        Some(410),
        "unknown ticket"
    );

    // Watch: hello, the gateway's frames, input refused and never forwarded, no lease.
    let mut ws = connect(&h, &proto, true).await.expect("watch socket");
    assert_eq!(connect(&h, &proto, true).await.err(), Some(410), "single use");
    assert_eq!(next_text(&mut ws).await, json!({"type": "hello", "v": 1, "mode": "watch", "sandbox": "workspace"}));
    assert_eq!(next_text(&mut ws).await["type"], "login");
    assert_eq!(next_binary(&mut ws).await[..3], [1, 1, 3]);
    ws.send(Message::text(r#"{"type":"mouse","mask":9,"x":5,"y":5}"#)).await.unwrap();
    assert_eq!(next_text(&mut ws).await, json!({"type": "refused", "what": "mouse", "why": "watch"}));
    ws.send(Message::text(r#"{"type":"ack"}"#)).await.unwrap();
    assert_eq!(gw.recv().await.unwrap(), r#"{"type":"ack"}"#, "only the ack reached the gateway");
    assert!(!control().owner_driving(SB), "watching never takes the lease");
    ws.close(None).await.unwrap();
    assert_eq!(gw.recv().await.unwrap(), "<closed>");
    wait_until("desk_closed (watch)", || recent_event("desk_closed").is_some()).await;
    let closed = recent_event("desk_closed").unwrap();
    assert_eq!(closed["mode"], "watch");
    assert_eq!(closed["device"], "test iPhone");
    assert_eq!(closed["via"], "p1");
    assert_eq!(closed["mouse"], 0);
    assert!(closed["bytesOut"].as_u64().unwrap() > 13, "{closed}");
    assert!(closed["openMs"].is_u64());
    let open = recent_event("desk_open").unwrap();
    assert_eq!((open["mode"].as_str(), open["via"].as_str()), (Some("watch"), Some("p1")));

    // Control: Lucina's task is stopped, agents are refused, input reaches the gateway,
    // and closing the socket gives the desktop back.
    let cancel = Cancel::new();
    let task = control().begin_task(SB, "lucina", "open example.com", cancel.clone()).unwrap();
    let (_, b) = mint(&h, "control", true).await;
    let proto = format!("lucinadesk.v1, ticket.{}", b["data"]["ticket"].as_str().unwrap());
    let mut ws = connect(&h, &proto, true).await.expect("control socket");
    assert_eq!(next_text(&mut ws).await["mode"], "control");
    wait_until("the lease", || control().owner_driving(SB)).await;
    assert!(cancel.is_cancelled(), "Lucina's task was stopped");
    assert!(task.preempted());
    drop(task);
    assert_eq!(control().agent_call(SB, "mcp:claude"), Err(Error::OwnerDriving));
    let mouse = r#"{"type":"mouse","mask":9,"x":5,"y":5}"#;
    ws.send(Message::text(mouse)).await.unwrap();
    ws.send(Message::text(r#"{"type":"text","text":"ciao"}"#)).await.unwrap();
    assert_eq!(gw.recv().await.unwrap(), mouse);
    assert_eq!(gw.recv().await.unwrap(), r#"{"type":"text","text":"ciao"}"#);
    ws.close(None).await.unwrap();
    assert_eq!(gw.recv().await.unwrap(), "<closed>");
    wait_until("the give-back", || !control().owner_driving(SB)).await;
    assert!(control().agent_call(SB, "mcp:claude").is_ok());
    let release = recent_event("release").unwrap();
    assert_eq!(release["device"], "test iPhone");
    wait_until("desk_closed (control)", || {
        recent_event("desk_closed").is_some_and(|e| e["mode"] == "control")
    })
    .await;
    let closed = recent_event("desk_closed").unwrap();
    assert_eq!((closed["mouse"].as_u64(), closed["textChars"].as_u64()), (Some(1), Some(4)), "{closed}");
    let take = control()
        .recent(SB, 50)
        .into_iter()
        .find(|e| e["event"] == "take" && e["device"] == "test iPhone")
        .expect("the take is in the audit ring");
    assert_eq!(take["stoppedTask"], "lucina: open example.com");

    // Giving the desktop back elsewhere ends the control socket with 4001.
    let (_, b) = mint(&h, "control", true).await;
    let proto = format!("lucinadesk.v1, ticket.{}", b["data"]["ticket"].as_str().unwrap());
    let mut ws = connect(&h, &proto, true).await.expect("control socket");
    wait_until("the lease", || control().owner_driving(SB)).await;
    control().release(SB, "Mac (other tab)");
    assert_eq!(close_code(&mut ws, Duration::from_secs(5)).await, 4001);
    wait_until("desk_closed 4001", || {
        recent_event("desk_closed").is_some_and(|e| e["code"] == 4001)
    })
    .await;
    assert!(!control().owner_driving(SB));

    // A lease the owner held before (a Cua takeover) outlives the desk socket.
    control().take(SB, "Mac (Cua view)");
    let (_, b) = mint(&h, "control", true).await;
    let proto = format!("lucinadesk.v1, ticket.{}", b["data"]["ticket"].as_str().unwrap());
    let mut ws = connect(&h, &proto, true).await.expect("control socket");
    assert_eq!(next_text(&mut ws).await["type"], "hello");
    ws.close(None).await.unwrap();
    wait_until("the socket's end", || {
        recent_event("desk_closed").is_some_and(|e| e["code"] == 1000 && e["mode"] == "control")
    })
    .await;
    assert!(control().owner_driving(SB), "the Cua takeover keeps its lease");
    control().release(SB, "Mac (Cua view)");

    // A gateway that went away: the socket opens, says closed, ends with 4003.
    std::fs::remove_file(&sock).unwrap();
    std::fs::write(&sock, b"").unwrap();
    let (s, b) = mint(&h, "watch", true).await;
    assert_eq!(s, 200, "{b}");
    let proto = format!("lucinadesk.v1, ticket.{}", b["data"]["ticket"].as_str().unwrap());
    let mut ws = connect(&h, &proto, true).await.expect("socket");
    assert_eq!(next_text(&mut ws).await["type"], "hello");
    assert_eq!(next_text(&mut ws).await["type"], "closed");
    assert_eq!(close_code(&mut ws, Duration::from_secs(10)).await, 4003);
}
