//! The HTTP API end to end: ingest (JSON, multipart, gzip), caps, scrubbing,
//! dedupe, listing filters, files, retention, rate limits, notifications.
use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_diagnostics::{api, App, Config, MB};
use serde_json::{json, Value};
use tower::ServiceExt;

const GATEWAY: &str = "gateway-token-0123456789abcdef";
const SERVICE: &str = "service-token-0123456789abcdef";
const BOUNDARY: &str = "XdiagBOUNDARYx";

struct H {
    app: Arc<App>,
    router: Router,
    dir: tempfile::TempDir,
}

fn harness(f: impl FnOnce(&mut Config)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dir(dir.path().join("data"));
    f(&mut cfg);
    let app = App::new(cfg)
        .unwrap()
        .with_tokens(Some(GATEWAY), Some(SERVICE));
    H {
        router: api::router(app.clone()),
        app,
        dir,
    }
}

async fn send(h: &H, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Bytes) {
    let r = h.router.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    (
        status,
        headers,
        r.into_body().collect().await.unwrap().to_bytes(),
    )
}

async fn call(h: &H, req: Request<Body>) -> (StatusCode, Value) {
    let (s, _, b) = send(h, req).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header("authorization", format!("Bearer {SERVICE}"))
        .body(Body::empty())
        .unwrap()
}

fn post_json(v: &Value) -> Request<Body> {
    Request::post("/v1/diagnostics/reports")
        .header("authorization", format!("Bearer {SERVICE}"))
        .header("content-type", "application/json")
        .body(Body::from(v.to_string()))
        .unwrap()
}

/// (field, filename, bytes)
fn multipart(report: &Value, files: &[(&str, &str, Vec<u8>)]) -> Vec<u8> {
    let mut b = Vec::new();
    write!(
        b,
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"report\"\r\nContent-Type: application/json\r\n\r\n{report}\r\n"
    )
    .unwrap();
    for (field, name, data) in files {
        write!(
            b,
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .unwrap();
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    write!(b, "--{BOUNDARY}--\r\n").unwrap();
    b
}

fn post_multipart(body: Vec<u8>) -> Request<Body> {
    Request::post("/v1/diagnostics/reports")
        .header("x-diagnostics-gateway", GATEWAY)
        .header("authorization", "Bearer sec_device_token_from_the_app")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn crash(version: &str, path: &str, line: u32) -> Value {
    json!({
        "app": "secretary-code",
        "app_version": version,
        "os": "macOS 15.1",
        "device": "nez-mbp",
        "kind": "crash",
        "message": "Background service timed out after 30012ms",
        "stack": format!("Error: Background service timed out after 30012ms\n    at startSidecar ({path}/out/main/index.js:{line}:5)\n    at boot ({path}/out/main/index.js:12:3)"),
        "context": {"window": "main", "route": "/session/ses_1", "engine": "1.0.200"},
        "occurred_at": "2026-10-26T18:44:00Z"
    })
}

#[tokio::test]
async fn json_ingest_detail_and_list() {
    let h = harness(|_| {});
    let (s, v) = call(
        &h,
        post_json(&crash("1.4.2", "/Applications/Secretary Code.app", 10)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("dg_") && v["duplicate"] == false && v["count"] == 1);

    let (s, d) = call(&h, get(&format!("/v1/diagnostics/reports/{id}"))).await;
    assert_eq!(s, StatusCode::OK);
    let r = &d["report"];
    assert_eq!(r["app"], "secretary-code");
    assert_eq!(r["kind"], "crash");
    assert_eq!(r["context"]["engine"], "1.0.200");
    assert_eq!(r["occurred_at"], "2026-10-26T18:44:00Z");
    assert_eq!(r["signature"][0], "at startSidecar (Secretary index.js)");
    assert_eq!(d["attachments"], json!([]));

    // on disk: <root>/<app>/<YYYY-MM-DD>/<id>/report.json
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let p = h
        .dir
        .path()
        .join("data/secretary-code")
        .join(&day)
        .join(&id)
        .join("report.json");
    assert!(p.is_file(), "{}", p.display());

    let (_, l) = call(&h, get("/v1/diagnostics/reports")).await;
    assert_eq!(l["reports"].as_array().unwrap().len(), 1);
    assert_eq!(l["reports"][0]["id"], id);
    assert_eq!(
        l["reports"][0]["message"],
        "Background service timed out after 30012ms"
    );

    let (s, _) = call(&h, get("/v1/diagnostics/reports/dg_00000000000000000000")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = call(
        &h,
        post_json(&json!({"app": "fridge", "kind": "crash", "message": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = call(&h, post_json(&json!({"app": "ios", "kind": "crash"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "message or stack required");
    // camelCase is accepted too
    let (s, v) = call(
        &h,
        post_json(&json!({"app": "ios", "appVersion": "2.1", "kind": "error", "message": "x", "occurredAt": "2026-10-26T10:00:00Z"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn auth_gateway_service_and_csrf() {
    let h = harness(|_| {});
    let anon = Request::get("/v1/diagnostics/reports")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h, anon).await.0, StatusCode::UNAUTHORIZED);
    let wrong = Request::get("/v1/diagnostics/reports")
        .header("x-diagnostics-gateway", "nope")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h, wrong).await.0, StatusCode::UNAUTHORIZED);
    let gw = Request::get("/v1/diagnostics/reports")
        .header("x-diagnostics-gateway", GATEWAY)
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&h, gw).await.0, StatusCode::OK);
    let cookie = |csrf: bool| {
        let mut b = Request::post("/v1/diagnostics/reports")
            .header("x-diagnostics-gateway", GATEWAY)
            .header("cookie", "sec_session=abc")
            .header("content-type", "application/json");
        if csrf {
            b = b.header("x-secretary-csrf", "1");
        }
        b.body(Body::from(
            json!({"app": "web", "kind": "error", "message": "boom"}).to_string(),
        ))
        .unwrap()
    };
    assert_eq!(call(&h, cookie(false)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&h, cookie(true)).await.0, StatusCode::CREATED);
    let health = Request::get("/health").body(Body::empty()).unwrap();
    let (s, v) = call(&h, health).await;
    assert_eq!((s, v["reports"].as_u64()), (StatusCode::OK, Some(1)));
}

#[tokio::test]
async fn multipart_attachments_gzip_minidump_and_downloads() {
    let h = harness(|_| {});
    let mut log = String::new();
    for i in 0..200 {
        log.push_str(&format!("2026-10-26T18:43:{:02} INFO line {i}\n", i % 60));
    }
    log.push_str("2026-10-26T18:44:00 ERROR sidecar did not answer: Authorization: Bearer abcdefghijklmnop\n");
    let mut dump = b"MDMP\x93\xa7\x00\x00".to_vec();
    dump.extend(std::iter::repeat_n(0u8, 1000));
    let body = multipart(
        &crash("1.4.2", "/Applications/Secretary Code.app", 10),
        &[
            ("file", "main.log", log.clone().into_bytes()),
            (
                "file",
                "/Users/nez/Library/Logs/sidecar.log.gz",
                gz(b"engine ready\nOPENCODE_SERVER_PASSWORD=hunter2hunter2 started\n"),
            ),
            ("minidump", "crash.dmp", dump.clone()),
            ("file", "../../report.json", b"{\"evil\": true}".to_vec()),
        ],
    );
    let (s, v) = call(&h, post_multipart(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    let (_, d) = call(&h, get(&format!("/v1/diagnostics/reports/{id}"))).await;
    let files = d["attachments"].as_array().unwrap();
    let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["main.log", "sidecar.log", "crash.dmp", "file4.txt"]);
    assert_eq!(files[1]["gzip"], true);
    assert_eq!(files[1]["kind"], "log");
    assert_eq!(files[2]["kind"], "minidump");
    assert_eq!(files[2]["text"], false);
    assert_eq!(files[2]["bytes"], dump.len());
    assert!(d["report"]["redactions"].as_u64().unwrap() >= 2);

    // whole file, scrubbed
    let (s, hd, b) = send(
        &h,
        get(&format!("/v1/diagnostics/reports/{id}/files/sidecar.log")),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hd["content-type"], "text/plain; charset=utf-8");
    assert_eq!(
        String::from_utf8(b.to_vec()).unwrap(),
        "engine ready\nOPENCODE_SERVER_PASSWORD=[redacted] started\n"
    );
    // tail starts at a whole line
    let (_, hd, b) = send(
        &h,
        get(&format!(
            "/v1/diagnostics/reports/{id}/files/main.log?tail=150"
        )),
    )
    .await;
    let tail = String::from_utf8(b.to_vec()).unwrap();
    assert!(
        tail.ends_with("ERROR sidecar did not answer: Authorization: [redacted]\n"),
        "{tail}"
    );
    assert!(
        tail.starts_with("2026-") || tail.starts_with("ERROR") || tail.starts_with("20"),
        "{tail}"
    );
    assert_eq!(hd["x-diagnostics-truncated"], "true");
    // binary as sent
    let (_, hd, b) = send(
        &h,
        get(&format!("/v1/diagnostics/reports/{id}/files/crash.dmp")),
    )
    .await;
    assert_eq!(hd["content-type"], "application/octet-stream");
    assert_eq!(b.to_vec(), dump);
    // the report.json in the directory is the real one
    let (s, _) = call(
        &h,
        get(&format!("/v1/diagnostics/reports/{id}/files/report.json")),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &h,
        get(&format!(
            "/v1/diagnostics/reports/{id}/files/..%2Freport.json"
        )),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn size_caps() {
    let h = harness(|c| {
        c.max_upload_bytes = MB;
        c.max_expanded_bytes = 2 * MB;
        c.max_files = 2;
    });
    let report =
        json!({"app": "android", "kind": "crash", "message": "boom", "stack": "at a.B.c(B.kt:1)"});
    // more than the upload cap
    let big = vec![b'x'; (MB + 100 * 1024) as usize];
    let (s, v) = call(
        &h,
        post_multipart(multipart(&report, &[("file", "big.log", big)])),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "too_large");
    // a small gzip that expands past the cap
    let bomb = gz(&vec![b'a'; (3 * MB) as usize]);
    assert!(bomb.len() < 100 * 1024);
    let (s, _) = call(
        &h,
        post_multipart(multipart(&report, &[("file", "bomb.log.gz", bomb)])),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // too many files
    let three: Vec<(&str, &str, Vec<u8>)> =
        (0..3).map(|_| ("file", "a.log", b"x".to_vec())).collect();
    let (s, _) = call(&h, post_multipart(multipart(&report, &three))).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // nothing was stored, nothing left in staging
    let (_, l) = call(&h, get("/v1/diagnostics/reports")).await;
    assert_eq!(l["reports"], json!([]));
    let staging = std::fs::read_dir(h.dir.path().join("data/.staging"))
        .unwrap()
        .count();
    assert_eq!(staging, 0);
    // within the caps: two names that collide
    let two = vec![
        ("file", "a.log", b"one".to_vec()),
        ("file", "a.log", b"two".to_vec()),
    ];
    let (s, v) = call(&h, post_multipart(multipart(&report, &two))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, d) = call(
        &h,
        get(&format!(
            "/v1/diagnostics/reports/{}",
            v["id"].as_str().unwrap()
        )),
    )
    .await;
    assert_eq!(d["attachments"][1]["name"], "a-2.log");
}

#[tokio::test]
async fn gzip_request_body() {
    let h = harness(|_| {});
    let body = gz(
        json!({"app": "web", "kind": "error", "message": "TypeError: x is undefined"})
            .to_string()
            .as_bytes(),
    );
    let req = Request::post("/v1/diagnostics/reports")
        .header("authorization", format!("Bearer {SERVICE}"))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(Body::from(body))
        .unwrap();
    let (s, v) = call(&h, req).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
}

#[tokio::test]
async fn secrets_never_reach_the_disk() {
    let h = harness(|_| {});
    let secrets = [
        "hunter2hunter2",
        "abcdefghijklmnop1234",
        "sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX",
        "pw-in-url-9",
        "sidecarpass77",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "svc_0123456789abcdefghijkl",
    ];
    let report = json!({
        "app": "secretary-code", "app_version": "1.4.2", "kind": "fatal_error",
        "message": "login failed for https://opencode:pw-in-url-9@127.0.0.1:4096 with Bearer abcdefghijklmnop1234",
        "stack": "Error: key sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX rejected\n    at call (api.ts:1:1)",
        "context": {"sidecar": {"url": "http://127.0.0.1:4096", "password": "sidecarpass77"},
                    "env": "OPENCODE_SERVER_PASSWORD=hunter2hunter2", "token": "svc_0123456789abcdefghijkl"}
    });
    let body = multipart(
        &report,
        &[(
            "file",
            "main.log",
            b"push with ghp_abcdefghijklmnopqrstuvwxyz0123456789 ok\npassword=hunter2hunter2\n"
                .to_vec(),
        )],
    );
    let (s, v) = call(&h, post_multipart(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap();
    let mut all = String::new();
    for e in walk(&h.dir.path().join("data")) {
        if e.extension()
            .is_some_and(|x| x == "sqlite" || x == "sqlite-wal" || x == "json" || x == "log")
        {
            all.push_str(&String::from_utf8_lossy(&std::fs::read(&e).unwrap()));
        }
    }
    for s in secrets {
        assert!(!all.contains(s), "{s} leaked");
    }
    let (_, d) = call(&h, get(&format!("/v1/diagnostics/reports/{id}"))).await;
    assert_eq!(d["report"]["context"]["sidecar"]["password"], "[redacted]");
    assert_eq!(
        d["report"]["context"]["sidecar"]["url"],
        "http://127.0.0.1:4096"
    );
    assert!(d["report"]["redactions"].as_u64().unwrap() >= 7);
}

fn walk(p: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(p).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

#[tokio::test]
async fn dedupe_counts_repeats() {
    let h = harness(|_| {});
    let (s, a) = call(
        &h,
        post_json(&crash("1.4.2", "/Applications/Secretary Code.app", 10)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    // same crash, other install path and line numbers, with a log: counted, not stored
    let body = multipart(
        &crash("1.4.2", "/Users/nez/Applications/Secretary Code.app", 99),
        &[("file", "main.log", b"again".to_vec())],
    );
    let (s, b) = call(&h, post_multipart(body)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["id"].clone(), b["duplicate"].clone(), b["count"].clone()),
        (a["id"].clone(), json!(true), json!(2))
    );
    let (_, d) = call(
        &h,
        get(&format!(
            "/v1/diagnostics/reports/{}",
            a["id"].as_str().unwrap()
        )),
    )
    .await;
    assert_eq!(d["report"]["count"], 2);
    assert_eq!(d["report"]["recent"].as_array().unwrap().len(), 1);
    assert_eq!(d["attachments"], json!([]));
    // another version is another report
    let (s, c) = call(
        &h,
        post_json(&crash("1.4.3", "/Applications/Secretary Code.app", 10)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_ne!(c["id"], a["id"]);
    // owner reports never merge
    let ur = json!({"app": "ios", "kind": "user_report", "message": "the memo screen froze"});
    let (s1, u1) = call(&h, post_json(&ur)).await;
    let (s2, u2) = call(&h, post_json(&ur)).await;
    assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_ne!(u1["id"], u2["id"]);
    // a retry with the same client_id changes nothing
    let mut retry = json!({"app": "ios", "kind": "user_report", "message": "once", "client_id": "q-0001-abcdef"});
    let (s, r1) = call(&h, post_json(&retry)).await;
    assert_eq!(s, StatusCode::CREATED);
    retry["message"] = json!("once (retried)");
    let (s, r2) = call(&h, post_json(&retry)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (r2["id"].clone(), r2["replay"].clone()),
        (r1["id"].clone(), json!(true))
    );
    let (_, l) = call(&h, get("/v1/diagnostics/reports")).await;
    assert_eq!(l["reports"].as_array().unwrap().len(), 5);
    // the repeated crash is the most recently seen of its kind
    let (_, l) = call(&h, get("/v1/diagnostics/reports?kind=crash")).await;
    let counts: Vec<u64> = l["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["count"].as_u64().unwrap())
        .collect();
    assert_eq!(counts.iter().sum::<u64>(), 3);
}

#[tokio::test]
async fn concurrent_copies_of_one_crash_make_one_report() {
    let h = harness(|c| c.rate_burst = 100);
    let mut tasks = Vec::new();
    for i in 0..8 {
        let r = h.router.clone();
        tasks.push(tokio::spawn(async move {
            r.oneshot(post_json(&crash("1.4.2", "/x", i)))
                .await
                .unwrap()
                .status()
        }));
    }
    let mut created = 0;
    for t in tasks {
        if t.await.unwrap() == StatusCode::CREATED {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    let (_, l) = call(&h, get("/v1/diagnostics/reports")).await;
    assert_eq!(l["reports"][0]["count"], 8);
}

#[tokio::test]
async fn listing_filters() {
    let h = harness(|_| {});
    for (app, kind, msg) in [
        ("secretary-code", "crash", "a"),
        ("secretary-code", "error", "b"),
        ("ios", "hang", "c"),
        ("android", "fatal_error", "d"),
        ("web", "error", "e"),
    ] {
        let (s, _) = call(
            &h,
            post_json(
                &json!({"app": app, "kind": kind, "message": msg, "device": format!("{app}-1")}),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let ids = |v: &Value| -> Vec<String> {
        v["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["message"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, v) = call(&h, get("/v1/diagnostics/reports?app=secretary-code")).await;
    assert_eq!(ids(&v), ["b", "a"]);
    let (_, v) = call(&h, get("/v1/diagnostics/reports?kind=crash,fatal_error")).await;
    assert_eq!(ids(&v), ["d", "a"]);
    let (_, v) = call(
        &h,
        get("/v1/diagnostics/reports?app=secretary-code&kind=error"),
    )
    .await;
    assert_eq!(ids(&v), ["b"]);
    let (_, v) = call(&h, get("/v1/diagnostics/reports?limit=2")).await;
    assert_eq!(ids(&v), ["e", "d"]);
    let (_, v) = call(&h, get("/v1/diagnostics/reports?device=ios-1")).await;
    assert_eq!(ids(&v), ["c"]);
    let (_, v) = call(&h, get("/v1/diagnostics/reports?since=1h")).await;
    assert_eq!(ids(&v).len(), 5);
    let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let (_, v) = call(
        &h,
        get(&format!("/v1/diagnostics/reports?since={tomorrow}")),
    )
    .await;
    assert_eq!(ids(&v).len(), 0);
    let (_, v) = call(
        &h,
        get("/v1/diagnostics/reports?until=2020-01-01T00:00:00Z"),
    )
    .await;
    assert_eq!(ids(&v).len(), 0);
    let (s, _) = call(&h, get("/v1/diagnostics/reports?kind=explosion")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&h, get("/v1/diagnostics/reports?since=yesterday")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retention_deletes_old_reports_and_strays() {
    let h = harness(|_| {});
    let (_, a) = call(
        &h,
        post_json(&json!({"app": "ios", "kind": "error", "message": "old"})),
    )
    .await;
    let id = a["id"].as_str().unwrap().to_owned();
    let root = h.dir.path().join("data");
    // a directory a crash left unindexed, from long ago
    let stray = root.join("web/2020-01-01/dg_aaaaaaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("report.json"), "{}").unwrap();
    // a cutoff in the past keeps today's report
    let n = h
        .app
        .store
        .purge(chrono::Utc::now() - chrono::Duration::days(90))
        .unwrap();
    assert_eq!(n, 1, "only the stray");
    assert!(!root.join("web/2020-01-01").exists());
    assert_eq!(
        call(&h, get(&format!("/v1/diagnostics/reports/{id}")))
            .await
            .0,
        StatusCode::OK
    );
    // everything last seen before the cutoff goes
    let n = h
        .app
        .store
        .purge(chrono::Utc::now() + chrono::Duration::seconds(1))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        call(&h, get(&format!("/v1/diagnostics/reports/{id}")))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(!walk(&root)
        .iter()
        .any(|p| p.to_string_lossy().contains(&id)));
    // and the index can be rebuilt from disk
    let (_, b) = call(
        &h,
        post_json(
            &json!({"app": "ios", "kind": "error", "message": "kept", "client_id": "cid-12345678"}),
        ),
    )
    .await;
    assert_eq!(h.app.store.reindex().unwrap(), 1);
    let (_, l) = call(&h, get("/v1/diagnostics/reports")).await;
    assert_eq!(l["reports"][0]["id"], b["id"]);
    let (s, _) = call(
        &h,
        post_json(
            &json!({"app": "ios", "kind": "error", "message": "kept", "client_id": "cid-12345678"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "client ids survive a reindex");
}

#[tokio::test]
async fn rate_limit_per_device() {
    let h = harness(|c| {
        c.rate_burst = 2;
        c.rate_per_hour = 1;
    });
    let r = |device: &str, msg: &str| json!({"app": "secretary-code", "kind": "error", "device": device, "message": msg});
    assert_eq!(
        call(&h, post_json(&r("mac", "first"))).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&h, post_json(&r("mac", "second"))).await.0,
        StatusCode::CREATED
    );
    let (s, hd, b) = send(&h, post_json(&r("mac", "third"))).await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        String::from_utf8_lossy(&b)
    );
    assert!(hd["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    assert_eq!(
        call(&h, post_json(&r("other-mac", "fourth"))).await.0,
        StatusCode::CREATED
    );
}

/// A secretary-server stand-in that records `POST /v1/notify`.
async fn notify_sink() -> (String, Arc<Mutex<Vec<Value>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    let app = Router::new().route(
        "/v1/notify",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(v): axum::Json<Value>| {
                let g = g.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer svc_notify_test_token");
                    g.lock().unwrap().push(v);
                    (
                        StatusCode::CREATED,
                        axum::Json(json!({"status": "created"})),
                    )
                }
            },
        ),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, got)
}

async fn wait_for(got: &Arc<Mutex<Vec<Value>>>, n: usize) {
    for _ in 0..100 {
        if got.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // and nothing more arrives
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn new_crash_signatures_notify_the_owner() {
    let (url, got) = notify_sink().await;
    let tok = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tok.path(), "svc_notify_test_token\n").unwrap();
    let path = tok.path().to_owned();
    let h = harness(move |c| {
        c.notify.secretary_url = Some(url);
        c.notify.service_token_file = Some(path);
        c.notify.per_hour = 2;
        c.timezone = chrono_tz::Europe::Copenhagen;
    });
    let now = chrono::Utc::now();
    let mut first = crash("1.4.2", "/Applications/Secretary Code.app", 10);
    first["occurred_at"] = json!(now.to_rfc3339());
    let (_, a) = call(&h, post_json(&first)).await;
    wait_for(&got, 1).await;
    {
        let g = got.lock().unwrap();
        assert_eq!(g.len(), 1);
        let n = &g[0];
        assert_eq!(n["category"], "system");
        assert_eq!(n["title"], "Secretary Code crashed");
        let local = now
            .with_timezone(&chrono_tz::Europe::Copenhagen)
            .format("%H:%M")
            .to_string();
        let body = n["body"].as_str().unwrap();
        assert!(
            body.starts_with(&format!(
                "Secretary Code crashed at {local}: Background service timed out after 30012ms"
            )),
            "{body}"
        );
        assert!(body.contains("1.4.2 · macOS 15.1 · nez-mbp"), "{body}");
        assert!(
            body.contains("at startSidecar (Secretary index.js)"),
            "{body}"
        );
        assert!(
            body.contains(&format!("diagnostics_get {}", a["id"].as_str().unwrap())),
            "{body}"
        );
        assert_eq!(n["data"]["diagnosticsReportId"], a["id"]);
    }
    // a repeat and a plain error: no notification
    call(&h, post_json(&first)).await;
    call(
        &h,
        post_json(&json!({"app": "web", "kind": "error", "message": "x"})),
    )
    .await;
    wait_for(&got, 1).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    // a new fatal error notifies; the hourly budget (2) then holds the next back
    call(&h, post_json(&json!({"app": "android", "kind": "fatal_error", "message": "OOM", "stack": "at a.B.c(B.kt:1)"}))).await;
    call(&h, post_json(&json!({"app": "ios", "kind": "crash", "message": "EXC_BAD_ACCESS", "stack": "0 Secretary 0x01 f + 1"}))).await;
    wait_for(&got, 2).await;
    let g = got.lock().unwrap();
    assert_eq!(g.len(), 2);
    assert_eq!(g[1]["title"], "The Android app hit a fatal error");
}

#[tokio::test]
async fn large_attachments_within_the_cap() {
    let h = harness(|_| {});
    let mut log = String::new();
    while log.len() < 5 * MB as usize {
        log.push_str(
            "2026-10-26T18:43:00 DEBUG a fairly long engine log line with some detail in it\n",
        );
    }
    let report = json!({"app": "secretary-code", "kind": "crash", "message": "big", "stack": "at a (b.js:1:1)"});
    let (s, v) = call(
        &h,
        post_multipart(multipart(
            &report,
            &[("file", "engine.log", log.clone().into_bytes())],
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, d) = call(
        &h,
        get(&format!(
            "/v1/diagnostics/reports/{}",
            v["id"].as_str().unwrap()
        )),
    )
    .await;
    assert_eq!(d["attachments"][0]["bytes"], log.len());
}
