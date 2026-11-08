//! End to end over HTTP: the relay on an ephemeral port with temporary token
//! files, driven with raw HTTP/1.1 like Caddy forwards it.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use lucina_otp_relay::http::{now_ms, serve, Config, Relay};
use serde_json::{json, Value};

const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const PHONE: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct Response {
    status: u16,
    headers: String,
    body: String,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

fn start(continuous: bool) -> (SocketAddr, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "lucina-otp-relay-it-{}-{continuous}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("owner.token"), OWNER).unwrap();
    fs::write(dir.join("phone.token"), format!("{PHONE}\n")).unwrap();
    let config = Config {
        state_dir: dir.clone(),
        continuous,
    };
    let mut relay = Relay::open(&config, now_ms()).unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    std::thread::spawn(move || serve(&server, &mut relay));
    (addr, dir)
}

fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> Response {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: getsecretary.tech\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    // A refused oversized body may close the connection before it is all sent.
    let _ = stream.write_all(body);
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8(raw).unwrap();
    let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = headers.split(' ').nth(1).unwrap().parse().unwrap();
    Response {
        status,
        headers: headers.to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

#[test]
fn timed_relay_over_http() {
    let (addr, dir) = start(false);
    let unauthorized = request(addr, "GET", "/status", None, b"");
    assert_eq!(unauthorized.status, 401);
    assert!(unauthorized.headers.contains("cache-control: no-store"));
    assert!(unauthorized
        .headers
        .contains("content-type: application/json"));
    assert_eq!(
        request(addr, "GET", "/status", Some(PHONE), b"").status,
        401
    );

    let status = request(addr, "GET", "/status", Some(OWNER), b"").json();
    assert_eq!(
        (status["armed"].clone(), status["protocol"].clone()),
        (json!(false), json!(2))
    );
    assert_eq!(
        request(addr, "GET", "/window", Some(PHONE), b"").json()["armed"],
        false
    );

    // Protocol 1, as app 0.4.0: /window, then /incoming with the windowId.
    let armed = request(addr, "POST", "/arm", Some(OWNER), br#"{"seconds":300}"#).json();
    let window = request(addr, "GET", "/window", Some(PHONE), b"").json();
    assert_eq!(window["windowId"], armed["windowId"]);
    let window_id = window["windowId"].as_str().unwrap();
    assert_eq!(window_id.len(), 48);
    let v1 = json!({"windowId": window_id, "eventId": "1b4e28ba-2fa1-11d2-883f-0016d3cca427", "code": "482917", "sender": "Bank"});
    let ack = request(
        addr,
        "POST",
        "/incoming",
        Some(PHONE),
        v1.to_string().as_bytes(),
    );
    assert_eq!((ack.status, ack.json()), (200, json!({"accepted": true})));
    let got = request(addr, "GET", "/otp", Some(OWNER), b"");
    assert_eq!(got.status, 200);
    assert!(got.headers.contains("cache-control: no-store"));
    assert_eq!(
        (got.json()["code"].clone(), got.json()["source"].clone()),
        (json!("482917"), json!("sms"))
    );
    let empty = request(addr, "GET", "/otp", Some(OWNER), b"");
    assert_eq!((empty.status, empty.body.as_str()), (204, ""));
    assert!(empty.headers.contains("cache-control: no-store"));

    // Protocol 2: no windowId, a test event, an expired code.
    let now = now_ms();
    let test = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e11", "code": "000000", "receivedAt": now, "test": true});
    let ack = request(
        addr,
        "POST",
        "/incoming",
        Some(PHONE),
        test.to_string().as_bytes(),
    );
    assert_eq!(
        (ack.status, ack.json()),
        (200, json!({"accepted": true, "test": true}))
    );
    assert_eq!(request(addr, "GET", "/otp", Some(OWNER), b"").status, 204);
    let old = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e12", "code": "482918", "receivedAt": now - 130_000});
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            old.to_string().as_bytes()
        )
        .status,
        410
    );
    let v2 = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e13", "code": "X8H2K9", "sender": "Bank", "receivedAt": now, "source": "notification"});
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            v2.to_string().as_bytes()
        )
        .status,
        200
    );
    let dup = request(
        addr,
        "POST",
        "/incoming",
        Some(PHONE),
        v2.to_string().as_bytes(),
    )
    .json();
    assert_eq!(dup, json!({"accepted": true, "duplicate": true}));
    let got = request(addr, "GET", "/v1/phone/otp/otp", Some(OWNER), b"").json();
    assert_eq!(
        (got["code"].clone(), got["source"].clone()),
        (json!("X8H2K9"), json!("notification"))
    );

    assert_eq!(
        request(addr, "POST", "/incoming", Some(PHONE), &[b' '; 3000]).status,
        413
    );
    assert_eq!(
        request(addr, "POST", "/incoming", Some(PHONE), b"{nope").status,
        400
    );
    assert_eq!(
        request(addr, "GET", "/nothing", Some(OWNER), b"").status,
        404
    );

    assert_eq!(
        request(addr, "POST", "/disarm", Some(OWNER), b"").json(),
        json!({"disarmed": true})
    );
    assert!(
        !dir.join("paused").exists(),
        "a timed relay writes no pause"
    );
    let v2b = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e14", "code": "482919", "receivedAt": now_ms()});
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            v2b.to_string().as_bytes()
        )
        .status,
        409
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn continuous_relay_over_http() {
    let (addr, dir) = start(true);
    let status = request(addr, "GET", "/status", Some(OWNER), b"").json();
    assert_eq!(
        (status["armed"].clone(), status["continuous"].clone()),
        (json!(true), json!(true))
    );
    let code = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e21", "code": "482917", "receivedAt": now_ms()});
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            code.to_string().as_bytes()
        )
        .status,
        200
    );
    assert_eq!(
        request(addr, "POST", "/disarm", Some(OWNER), b"").status,
        200
    );
    assert!(dir.join("paused").exists());
    let next = json!({"eventId": "8c1f0a52-7d2e-4f0b-9a51-2f8f6c1d0e22", "code": "482918", "receivedAt": now_ms()});
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            next.to_string().as_bytes()
        )
        .status,
        409
    );
    assert_eq!(request(addr, "GET", "/otp", Some(OWNER), b"").status, 204);
    let armed = request(addr, "POST", "/arm", Some(OWNER), br#"{"seconds":300}"#).json();
    assert!(armed["continuous"] == true && armed["expiresAt"].is_null());
    assert!(!dir.join("paused").exists());
    assert_eq!(
        request(
            addr,
            "POST",
            "/incoming",
            Some(PHONE),
            next.to_string().as_bytes()
        )
        .status,
        200
    );
    assert_eq!(
        request(addr, "GET", "/otp", Some(OWNER), b"").json()["code"],
        "482918"
    );
    let _ = fs::remove_dir_all(&dir);
}
