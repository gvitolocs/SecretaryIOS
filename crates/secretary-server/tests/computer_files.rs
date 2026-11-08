//! Files to and from Lucina's desktop (`/v1/computer/files`, `/v1/computer/clipboard/files`):
//! owner devices only, the device named by the login, bodies streamed both ways, Lucina's
//! envelope unwrapped like every other call. Against a mock Lucina.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const LUCINA_TOKEN: &str = "lucina-server-secret";

/// What Lucina saw: `(method, query, authorization, body length)`.
type Seen = (String, HashMap<String, String>, Option<String>, usize);

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<Seen>>,
}

async fn files(
    State(m): State<Arc<Mock>>,
    method: axum::http::Method,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    m.seen
        .lock()
        .unwrap()
        .push((method.to_string(), q.clone(), auth, body.len()));
    if method == axum::http::Method::GET {
        if q.get("path").map(String::as_str) == Some("/home/cua/secret.txt") {
            return (
                StatusCode::CONFLICT,
                Json(json!({"ok": false, "error": "not_on_clipboard", "message": "only copied files"})),
            )
                .into_response();
        }
        return (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename*=UTF-8''notes.txt",
                ),
            ],
            "hello",
        )
            .into_response();
    }
    Json(json!({"ok": true, "data": {"path": "/home/cua/Desktop/From Mac/report.pdf", "name": "report.pdf", "bytes": body.len()}}))
        .into_response()
}

async fn clipboard_files(
    State(m): State<Arc<Mock>>,
    method: axum::http::Method,
    body: Bytes,
) -> Response {
    m.seen
        .lock()
        .unwrap()
        .push((method.to_string(), HashMap::new(), None, body.len()));
    if method == axum::http::Method::POST {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": "not_driving", "message": "take over before sending files"})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "data": {"generation": 4, "files": [{"path": "/home/cua/notes.txt", "name": "notes.txt", "bytes": 5}]}}))
        .into_response()
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
        .route("/api/secretary/computer/files", get(files).post(files))
        .route(
            "/api/secretary/computer/clipboard/files",
            get(clipboard_files).post(clipboard_files),
        )
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, lucina).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir)
        .issue("Secretary Code Mac")
        .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move {
        serve_on(l, app(state), std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: reqwest::Client::new(),
        token,
        mock,
        _dir: dir,
    }
}

#[tokio::test]
async fn files_stream_both_ways_for_owner_devices_only() {
    let h = harness().await;
    let url = |p: &str| format!("{}{p}", h.base);
    let r = h
        .http
        .post(url("/v1/computer/files?sandbox=workspace&name=report.pdf"))
        .body(vec![1u8; 10])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(
        h.mock.seen.lock().unwrap().is_empty(),
        "Lucina never asked without a device"
    );

    // A 3 MiB upload goes through whole, named by the login, never by the query.
    let file = vec![7u8; 3 * 1024 * 1024];
    let r = h
        .http
        .post(url(
            "/v1/computer/files?sandbox=workspace&name=report.pdf&device=forged&evil=1",
        ))
        .bearer_auth(&h.token)
        .header("content-type", "application/octet-stream")
        .body(file.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["name"], "report.pdf", "the envelope is unwrapped: {b}");
    assert_eq!(b["bytes"], file.len());
    let (method, q, auth, len) = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(method, "POST");
    assert_eq!(len, file.len());
    assert_eq!(auth.as_deref(), Some(&*format!("Bearer {LUCINA_TOKEN}")));
    assert_eq!(
        q.get("device").map(String::as_str),
        Some("Secretary Code Mac")
    );
    assert_eq!(q.get("name").map(String::as_str), Some("report.pdf"));
    assert!(!q.contains_key("evil"));

    // A download streams back with its name; refusals keep their status and code.
    let r = h
        .http
        .get(url(
            "/v1/computer/files?sandbox=workspace&path=%2Fhome%2Fcua%2Fnotes.txt",
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()["content-disposition"],
        "attachment; filename*=UTF-8''notes.txt"
    );
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "hello");
    let r = h
        .http
        .get(url(
            "/v1/computer/files?sandbox=workspace&path=%2Fhome%2Fcua%2Fsecret.txt",
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["error"]["code"], "not_on_clipboard", "{b}");

    // Her clipboard's files, and putting ours there.
    let r = h
        .http
        .get(url("/v1/computer/clipboard/files?sandbox=workspace"))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["generation"], 4);
    assert_eq!(b["files"][0]["name"], "notes.txt");
    let r = h
        .http
        .post(url("/v1/computer/clipboard/files"))
        .bearer_auth(&h.token)
        .json(&json!({"sandbox": "workspace", "paths": ["/x"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
}
