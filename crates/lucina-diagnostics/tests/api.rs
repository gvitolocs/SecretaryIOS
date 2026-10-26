//! The HTTP API end to end: ingest with attachments, caps, scrubbing, dedupe,
//! listing filters, retention, auth, rate limit and the crash notification.
use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_diagnostics::{api, App, Config};
use serde_json::{json, Value};
use tower::ServiceExt;

const SERVICE: &str = "svc-token-0123456789abcdef0123";
const GATEWAY: &str = "gw-token-0123456789abcdef01234";

struct H {
    app: Arc<App>,
    router: Router,
    _dir: tempfile::TempDir,
}

fn harness_with(extra: &[(&str, String)]) -> H {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data").to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = vec![
        ("LUCINA_DIAGNOSTICS_DIR".into(), data),
        (
            "LUCINA_DIAGNOSTICS_TIMEZONE".into(),
            "Europe/Copenhagen".into(),
        ),
        ("LUCINA_DIAGNOSTICS_NOTIFY_TELEGRAM".into(), "off".into()),
    ];
    env.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let cfg = Config::from_lookup(get);
    let app = App::new(cfg)
        .unwrap()
        .with_tokens(Some(GATEWAY), Some(SERVICE));
    H {
        router: api::router(app.clone()),
        app,
        _dir: dir,
    }
}

fn harness() -> H {
    harness_with(&[])
}

impl H {
    async fn send(&self, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let r = self.router.clone().oneshot(req).await.unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let body = r.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, headers, body)
    }

    async fn json(&self, req: Request<Body>) -> (StatusCode, Value) {
        let (s, _, b) = self.send(req).await;
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.json(
            Request::get(path)
                .header("authorization", format!("Bearer {SERVICE}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post(&self, report: Value) -> (StatusCode, Value) {
        self.json(
            Request::post("/v1/diagnostics/reports")
                .header("authorization", format!("Bearer {SERVICE}"))
                .header("content-type", "application/json")
                .body(Body::from(report.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn post_multipart(
        &self,
        report: &Value,
        files: &[(&str, &str, Vec<u8>)],
    ) -> (StatusCode, Value) {
        let (ct, body) = multipart(report, files);
        self.json(
            Request::post("/v1/diagnostics/reports")
                .header("authorization", format!("Bearer {SERVICE}"))
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }
}

fn multipart(report: &Value, files: &[(&str, &str, Vec<u8>)]) -> (String, Vec<u8>) {
    let b = "XxDiagBoundary7";
    let mut body = Vec::new();
    write!(
        body,
        "--{b}\r\nContent-Disposition: form-data; name=\"report\"\r\nContent-Type: application/json\r\n\r\n{report}\r\n"
    )
    .unwrap();
    for (name, ct, data) in files {
        write!(
            body,
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {ct}\r\n\r\n"
        )
        .unwrap();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{b}--\r\n").unwrap();
    (format!("multipart/form-data; boundary={b}"), body)
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

const STACK: &str = "Error: background service timed out\n    at waitReady (/Users/nez/Applications/Secretary.app/Contents/Resources/app.asar/out/main/index-3f9a1c2b.js:812:11)\n    at async startSidecar (/Users/nez/Applications/Secretary.app/Contents/Resources/app.asar/out/main/index-3f9a1c2b.js:640:5)";

fn crash(device: &str) -> Value {
    json!({
        "app": "secretary-code",
        "app_version": "0.9.3",
        "os": "macOS 26.1 (arm64)",
        "device": device,
        "kind": "crash",
        "message": "the background service timed out after 30000 ms",
        "stack": STACK,
        "context": {"window": "main", "route": "/session/abc", "engine_version": "1.4.2"},
        "occurred_at": "2026-10-26T18:44:00Z",
    })
}

#[tokio::test]
async fn ingest_with_attachments_and_download() {
    let h = harness();
    let log = b"19:43:58 starting sidecar\n19:44:00 OPENCODE_SERVER_PASSWORD=hunter2hunter2\n19:44:30 Authorization: Bearer abcdefghijklmnop123\n19:44:30 ready check failed\n".to_vec();
    let dump = {
        let mut d = b"MDMP\x93\xa7\x00\x00".to_vec();
        d.extend((0..5000u32).map(|i| (i % 251) as u8));
        d
    };
    let (s, v) = h
        .post_multipart(
            &crash("mac-1"),
            &[
                ("main.log.gz", "application/gzip", gz(&log)),
                ("crash.dmp", "application/octet-stream", dump.clone()),
                ("../../evil name.txt", "text/plain", b"x".to_vec()),
            ],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["duplicate"], false);
    assert_eq!(v["count"], 1);
    let id = v["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("dg_") && id.len() == 19);

    let (s, d) = h.get(&format!("/v1/diagnostics/reports/{id}")).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["report"]["app"], "secretary-code");
    assert_eq!(d["report"]["kind"], "crash");
    assert_eq!(d["report"]["context"]["engine_version"], "1.4.2");
    assert_eq!(d["count"], 1);
    let names: Vec<&str> = d["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["main.log", "crash.dmp", "evil_name.txt"]);
    assert_eq!(d["attachments"][0]["gzip"], true);
    assert_eq!(d["attachments"][0]["text"], true);
    assert_eq!(d["attachments"][1]["text"], false);
    assert_eq!(d["report"]["redactions"], 2);

    // the stored tree: <root>/<app>/<YYYY-MM-DD>/<id>/
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let dir = h
        .app
        .cfg
        .data_dir
        .join("secretary-code")
        .join(&day)
        .join(&id);
    assert!(dir.join("report.json").is_file());
    assert!(dir.join("main.log").is_file());

    // the log came back decompressed and scrubbed
    let (s, headers, body) = h
        .send(
            Request::get(format!("/v1/diagnostics/reports/{id}/files/main.log"))
                .header("authorization", format!("Bearer {SERVICE}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let text = String::from_utf8(body).unwrap();
    assert!(
        text.contains("OPENCODE_SERVER_PASSWORD=[redacted]"),
        "{text}"
    );
    assert!(text.contains("Bearer [redacted]"), "{text}");
    assert!(!text.contains("hunter2") && !text.contains("abcdefghijklmnop123"));

    // tail of a text file
    let (s, headers, body) = h
        .send(
            Request::get(format!(
                "/v1/diagnostics/reports/{id}/files/main.log?tail=22"
            ))
            .header("authorization", format!("Bearer {SERVICE}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["x-slice"], "tail");
    assert_eq!(String::from_utf8(body).unwrap(), "30 ready check failed\n");

    // the minidump is byte for byte what was sent
    let (s, headers, body) = h
        .send(
            Request::get(format!("/v1/diagnostics/reports/{id}/files/crash.dmp"))
                .header("authorization", format!("Bearer {SERVICE}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert_eq!(body, dump);
    let (s, _) = h
        .get(&format!(
            "/v1/diagnostics/reports/{id}/files/crash.dmp?tail=10"
        ))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no text slices of binaries");

    for bad in [
        format!("/v1/diagnostics/reports/{id}/files/report.json"),
        format!("/v1/diagnostics/reports/{id}/files/nope.txt"),
        "/v1/diagnostics/reports/dg_0000000000000000/files/main.log".to_owned(),
        "/v1/diagnostics/reports/..%2F..%2Fetc/files/passwd".to_owned(),
    ] {
        let (s, _) = h.get(&bad).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{bad}");
    }
}

#[tokio::test]
async fn whole_body_gzip_and_size_caps() {
    let h = harness_with(&[("LUCINA_DIAGNOSTICS_MAX_ATTACHMENT_BYTES", "4096".into())]);
    // Content-Encoding: gzip on the whole request
    let (ct, body) = multipart(
        &crash("mac-1"),
        &[("a.log", "text/plain", b"hello".to_vec())],
    );
    let (s, v) = h
        .json(
            Request::post("/v1/diagnostics/reports")
                .header("authorization", format!("Bearer {SERVICE}"))
                .header("content-type", ct)
                .header("content-encoding", "gzip")
                .body(Body::from(gz(&body)))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // two files that are each under the cap but together over it
    let mut r = crash("mac-2");
    r["stack"] = json!("other");
    let (s, v) = h
        .post_multipart(
            &r,
            &[
                ("a.log", "text/plain", vec![b'a'; 3000]),
                ("b.log", "text/plain", vec![b'b'; 3000]),
            ],
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "too_large");

    // a small gzip that inflates past the cap
    let (s, v) = h
        .post_multipart(
            &r,
            &[(
                "bomb.log.gz",
                "application/gzip",
                gz(&vec![b'x'; 1_000_000]),
            )],
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");

    // a body over the cap is refused before parsing
    let (s, v) = h
        .post_multipart(
            &r,
            &[("big.bin", "application/octet-stream", vec![0; 2 << 20])],
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");

    // nothing of the refused reports was stored
    let (_, list) = h.get("/v1/diagnostics/reports").await;
    assert_eq!(list["reports"].as_array().unwrap().len(), 1);

    // bad input
    let (s, v) = h.post(json!({"app": "windows", "app_version": "1", "device": "d", "kind": "crash", "message": "m"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = h.post(json!({"app": "ios", "app_version": "1", "device": "d", "kind": "panic", "message": "m"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.post(json!({"app": "ios", "app_version": "1", "device": "d", "kind": "crash", "message": "m", "extra": 1})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown fields are refused");
    let (s, _) = h
        .json(
            Request::post("/v1/diagnostics/reports")
                .header("authorization", format!("Bearer {SERVICE}"))
                .header("content-type", "text/plain")
                .body(Body::from("hi"))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn scrubbing_of_fields_and_context() {
    let h = harness();
    let (s, v) = h
        .post(json!({
            "app": "web",
            "appVersion": "2026.10.26",
            "device": "chrome-abc",
            "kind": "error",
            "message": "fetch https://owner:s3cretpw@getsecretary.tech/v1/x failed; key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
            "stack": "Error: x\n    at f (app.js:1:1) token=abcdef0123456789",
            "context": {"headers": {"authorization": "Bearer abc123def456"}, "password": "pw", "route": "/chat"},
        }))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, d) = h
        .get(&format!(
            "/v1/diagnostics/reports/{}",
            v["id"].as_str().unwrap()
        ))
        .await;
    let r = &d["report"];
    let all = r.to_string();
    for secret in [
        "s3cretpw",
        "sk-ant-api03",
        "abcdef0123456789",
        "abc123def456",
        "\"pw\"",
    ] {
        assert!(!all.contains(secret), "{secret} leaked: {all}");
    }
    assert_eq!(
        r["message"],
        "fetch https://[redacted]@getsecretary.tech/v1/x failed; key [redacted]"
    );
    assert_eq!(r["context"]["password"], "[redacted]");
    assert_eq!(r["context"]["headers"]["authorization"], "[redacted]");
    assert_eq!(r["context"]["route"], "/chat");
    assert_eq!(r["redactions"], 5);
    // what is on disk is the scrubbed version too
    let raw = std::fs::read_to_string(
        h.app
            .cfg
            .data_dir
            .join("web")
            .join(chrono::Utc::now().format("%Y-%m-%d").to_string())
            .join(r["id"].as_str().unwrap())
            .join("report.json"),
    )
    .unwrap();
    assert!(!raw.contains("s3cretpw"));
}

#[tokio::test]
async fn dedupe_counts_repeats() {
    let h = harness();
    let (s, first) = h.post(crash("mac-1")).await;
    assert_eq!(s, StatusCode::CREATED);
    // the same crash on another machine: other install path, other line numbers
    let mut again = crash("mac-2");
    again["stack"] = json!(STACK
        .replace("/Users/nez/Applications", "/Applications")
        .replace("index-3f9a1c2b.js:812:11", "index-77aa88bb.js:813:2"));
    let (s, second) = h
        .post_multipart(&again, &[("x.log", "text/plain", b"ignored".to_vec())])
        .await;
    assert_eq!(s, StatusCode::OK, "{second}");
    assert_eq!(second["duplicate"], true);
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["count"], 2);
    let (_, d) = h
        .get(&format!(
            "/v1/diagnostics/reports/{}",
            first["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(d["count"], 2);
    assert_eq!(d["last_device"], "mac-2");
    assert_eq!(
        d["attachments"].as_array().unwrap().len(),
        0,
        "a repeat stores nothing"
    );

    // another version is another fingerprint
    let mut newer = crash("mac-1");
    newer["app_version"] = json!("0.9.4");
    let (s, v) = h.post(newer).await;
    assert_eq!((s, &v["duplicate"]), (StatusCode::CREATED, &json!(false)));

    // user reports are never merged
    let mut ur = crash("mac-1");
    ur["kind"] = json!("user_report");
    let (s1, _) = h.post(ur.clone()).await;
    let (s2, v2) = h.post(ur).await;
    assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(v2["duplicate"], false);

    let (_, list) = h.get("/v1/diagnostics/reports").await;
    assert_eq!(list["reports"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn listing_filters() {
    let h = harness();
    for (app, kind, msg) in [
        ("secretary-code", "crash", "a"),
        ("secretary-code", "error", "b"),
        ("ios", "crash", "c"),
        ("android", "hang", "d"),
        ("web", "error", "e"),
    ] {
        let (s, v) = h
            .post(json!({"app": app, "app_version": "1", "device": "dev", "kind": kind, "message": msg}))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let count = |v: &Value| v["reports"].as_array().unwrap().len();
    let (_, v) = h.get("/v1/diagnostics/reports?app=secretary-code").await;
    assert_eq!(count(&v), 2);
    let (_, v) = h.get("/v1/diagnostics/reports?kind=crash").await;
    assert_eq!(count(&v), 2);
    let (_, v) = h
        .get("/v1/diagnostics/reports?app=secretary-code&kind=crash")
        .await;
    assert_eq!(count(&v), 1);
    assert_eq!(v["reports"][0]["message"], "a");
    assert_eq!(v["reports"][0]["count"], 1);
    let (_, v) = h.get("/v1/diagnostics/reports?since=1h").await;
    assert_eq!(count(&v), 5);
    let (_, v) = h
        .get("/v1/diagnostics/reports?since=2099-01-01T00:00:00Z")
        .await;
    assert_eq!(count(&v), 0);
    let (_, v) = h.get("/v1/diagnostics/reports?app=&kind=").await;
    assert_eq!(count(&v), 5, "empty filters are no filters");

    // pages, newest first
    let (_, p1) = h.get("/v1/diagnostics/reports?limit=3").await;
    assert_eq!(count(&p1), 3);
    assert_eq!(p1["reports"][0]["message"], "e");
    let next = p1["next"].as_str().unwrap();
    let (_, p2) = h
        .get(&format!("/v1/diagnostics/reports?limit=3&before={next}"))
        .await;
    assert_eq!(count(&p2), 2, "{p2}");
    assert_eq!(p2["reports"][1]["message"], "a");
    assert!(p2["next"].is_null());

    for bad in ["app=windows", "kind=panic", "since=yesterday"] {
        let (s, _) = h.get(&format!("/v1/diagnostics/reports?{bad}")).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn retention_deletes_old_reports() {
    let h = harness();
    let (_, v) = h
        .post_multipart(&crash("mac-1"), &[("a.log", "text/plain", b"x".to_vec())])
        .await;
    let id = v["id"].as_str().unwrap().to_owned();
    let app_dir = h.app.cfg.data_dir.join("secretary-code");
    assert!(app_dir.exists());
    let now = chrono::Utc::now();
    assert_eq!(
        h.app
            .store
            .prune(now + chrono::Duration::days(89), 90)
            .unwrap(),
        0
    );
    assert_eq!(
        h.app
            .store
            .prune(now + chrono::Duration::days(91), 90)
            .unwrap(),
        1
    );
    let (s, _) = h.get(&format!("/v1/diagnostics/reports/{id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!app_dir.exists(), "empty day and app directories go too");
    assert_eq!(h.app.cfg.retention_days, 90, "the default");
    // after retention, the same crash is a new report
    let (s, v2) = h.post(crash("mac-1")).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_ne!(v2["id"], json!(id));
    let h2 = harness_with(&[("LUCINA_DIAGNOSTICS_RETENTION_DAYS", "7".into())]);
    assert_eq!(h2.app.cfg.retention_days, 7);
}

#[tokio::test]
async fn auth_gateway_and_csrf() {
    let h = harness();
    let (s, _) = h
        .json(
            Request::get("/v1/diagnostics/reports")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .json(
            Request::get("/v1/diagnostics/reports")
                .header("authorization", "Bearer wrong-token-0123456789abcdef")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .json(
            Request::get("/v1/diagnostics/reports")
                .header("x-diagnostics-gateway", GATEWAY)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // a cookie-only write through the gateway needs the CSRF header
    let req = |csrf: bool| {
        let mut b = Request::post("/v1/diagnostics/reports")
            .header("x-diagnostics-gateway", GATEWAY)
            .header("cookie", "secretary_session=x")
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-secretary-csrf", "1");
        }
        b.body(Body::from(crash("web-1").to_string())).unwrap()
    };
    let (s, v) = h.json(req(false)).await;
    assert_eq!(
        (s, &v["error"]["code"]),
        (StatusCode::FORBIDDEN, &json!("csrf"))
    );
    let (s, _) = h.json(req(true)).await;
    assert_eq!(s, StatusCode::CREATED);
    // health is open
    let (s, v) = h
        .json(Request::get("/health").body(Body::empty()).unwrap())
        .await;
    assert_eq!((s, &v["reports"]), (StatusCode::OK, &json!(1)));
}

#[tokio::test]
async fn rate_limit_per_device() {
    let h = harness_with(&[
        ("LUCINA_DIAGNOSTICS_RATE_BURST", "2".into()),
        ("LUCINA_DIAGNOSTICS_RATE_PER_HOUR", "1".into()),
    ]);
    assert_eq!(h.post(crash("loop")).await.0, StatusCode::CREATED);
    assert_eq!(h.post(crash("loop")).await.0, StatusCode::OK);
    let (s, headers, body) = h
        .send(
            Request::post("/v1/diagnostics/reports")
                .header("authorization", format!("Bearer {SERVICE}"))
                .header("content-type", "application/json")
                .body(Body::from(crash("loop").to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!(wait > 60, "{wait}");
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "rate_limited");
    assert_eq!(
        h.post(crash("other")).await.0,
        StatusCode::OK,
        "another device is not limited"
    );
    let (_, d) = h.get("/v1/diagnostics/reports").await;
    assert_eq!(
        d["reports"][0]["count"], 3,
        "the refused one was not counted"
    );
}

/// A stand-in for secretary-server's `POST /v1/notify`.
async fn mock_notify() -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let s = seen.clone();
    let app = Router::new().route(
        "/v1/notify",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(v): axum::Json<Value>| {
                let s = s.clone();
                async move {
                    let auth = headers["authorization"].to_str().unwrap().to_owned();
                    s.lock().unwrap().push((auth, v));
                    axum::Json(json!({"ok": true}))
                }
            },
        ),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

async fn wait_for(seen: &Arc<Mutex<Vec<(String, Value)>>>, n: usize) {
    for _ in 0..100 {
        if seen.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn new_crash_notifies_the_owner_once() {
    let (url, seen) = mock_notify().await;
    let tokdir = tempfile::tempdir().unwrap();
    let tf = tokdir.path().join("svc.token");
    std::fs::write(&tf, "lucina-service-token\n").unwrap();
    let h = harness_with(&[
        ("LUCINA_SECRETARY_SERVER_URL", url),
        (
            "LUCINA_SECRETARY_SERVICE_TOKEN_FILE",
            tf.to_string_lossy().into_owned(),
        ),
    ]);
    assert!(h.app.cfg.notify.enabled());
    let (_, first) = h.post(crash("mac-1")).await;
    wait_for(&seen, 1).await;
    // a repeat, an error, and a second new crash inside the interval: no more notices
    h.post(crash("mac-1")).await;
    let mut err = crash("mac-1");
    err["kind"] = json!("error");
    h.post(err).await;
    let mut other = crash("mac-1");
    other["stack"] = json!("Error: another\n    at x (a.js:1:1)");
    h.post(other).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let (auth, body) = &seen[0];
    assert_eq!(auth, "Bearer lucina-service-token");
    assert_eq!(body["category"], "lucina_message");
    assert_eq!(body["title"], "Secretary Code crashed");
    let text = body["body"].as_str().unwrap();
    assert!(
        text.starts_with(
            "Secretary Code crashed at 26 Oct 19:44: the background service timed out after 30000 ms\nat waitReady (index.js)"
        ) || text.starts_with(
            "Secretary Code crashed at 19:44: the background service timed out after 30000 ms\nat waitReady (index.js)"
        ),
        "{text}"
    );
    assert!(text.contains("v0.9.3 · macOS 26.1 (arm64)"), "{text}");
    assert!(text.contains(first["id"].as_str().unwrap()), "{text}");
    assert_eq!(body["data"]["reportId"], first["id"]);
}
