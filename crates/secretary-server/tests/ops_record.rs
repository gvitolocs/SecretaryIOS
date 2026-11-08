//! `/v1/ops/*` against a mock Lucina: the record body and the MCP's caller reach her
//! unchanged with her status code, a non-object body never leaves the server, the
//! search query is forwarded, and Lucina's service token is not an owner device.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    /// `(path, query, x-lucina-caller, body)` of every request.
    seen: Mutex<Vec<(String, String, String, Value)>>,
}

fn lucina_caller(h: &HeaderMap) -> String {
    h.get("x-lucina-caller")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

async fn records(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    m.seen.lock().unwrap().push((
        "records".into(),
        String::new(),
        lucina_caller(&headers),
        body.clone(),
    ));
    if body["kind"] == "bogus" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "bad_request", "message": "kind: unknown variant"})),
        );
    }
    let duplicate = body["dedup_key"] == "seen";
    (
        if duplicate {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(json!({"ok": true, "data": {
            "id": "0123456789abcdef", "duplicate": duplicate, "redacted": false, "honcho": "ok",
            "paths": {"log": "Log/2026-10-28.md", "project": "Projects/p1.md"},
        }})),
    )
}

async fn record(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Json<Value> {
    m.seen.lock().unwrap().push((
        id.clone(),
        String::new(),
        lucina_caller(&headers),
        Value::Null,
    ));
    Json(json!({"ok": true, "data": {"id": id, "kind": "deploy", "project": "p1", "title": "x"}}))
}

async fn search(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Json<Value> {
    m.seen.lock().unwrap().push((
        "search".into(),
        q.unwrap_or_default(),
        lucina_caller(&headers),
        Value::Null,
    ));
    Json(json!({"ok": true, "data": {"items": [], "honcho": "ok"}}))
}

struct H {
    base: String,
    token: String,
    svc: String,
    mock: Arc<Mock>,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let mock = Arc::new(Mock::default());
    let lucina = Router::new()
        .route("/api/secretary/ops/records", post(records))
        .route("/api/secretary/ops/records/{id}", get(record))
        .route("/api/secretary/ops/search", get(search))
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, lucina).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let svc_path = dir.path().join("service.token");
    secretary_server::service::write_new(&svc_path, false).unwrap();
    cfg.service_token = Some(ServiceToken::read_file(svc_path.to_str().unwrap()).unwrap());
    let svc = std::fs::read_to_string(&svc_path)
        .unwrap()
        .trim()
        .to_owned();
    let (_, token) = DeviceStore::new(&cfg.data_dir)
        .issue("Secretary MCP")
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
        token,
        svc,
        mock,
        http: reqwest::Client::new(),
        _dir: dir,
    }
}

#[tokio::test]
async fn a_record_reaches_lucina_with_the_caller_and_her_status() {
    let h = harness().await;
    let body = json!({
        "kind": "incident", "project": "p1", "title": "Secretary down", "severity": "high",
        "timeline": ["10:00 alert", "10:05 restart"], "at": "2026-10-28T10:00:00+01:00",
    });
    let r = h
        .http
        .post(format!("{}/v1/ops/records", h.base))
        .bearer_auth(&h.token)
        .header("x-secretary-caller", "mcp:claude")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["id"], "0123456789abcdef");
    assert_eq!(v["paths"]["project"], "Projects/p1.md");
    let seen = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.2, "mcp:claude");
    assert_eq!(seen.3, body, "the body travels unchanged");

    // A duplicate is Lucina's 200; without an MCP caller the app is named.
    let r = h
        .http
        .post(format!("{}/v1/ops/records", h.base))
        .bearer_auth(&h.token)
        .json(&json!({"kind": "status", "project": "p1", "title": "done", "dedup_key": "seen"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["duplicate"], true);
    assert_eq!(h.mock.seen.lock().unwrap().last().unwrap().2, "app");

    // Lucina's own 400 names the field.
    let r = h
        .http
        .post(format!("{}/v1/ops/records", h.base))
        .bearer_auth(&h.token)
        .json(&json!({"kind": "bogus", "project": "p1", "title": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("kind"),
        "{v}"
    );
}

#[tokio::test]
async fn a_body_that_is_not_an_object_never_reaches_lucina() {
    let h = harness().await;
    for body in [json!(["deploy"]), json!("deploy"), json!(null)] {
        let r = h
            .http
            .post(format!("{}/v1/ops/records", h.base))
            .bearer_auth(&h.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{body}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"]["code"],
            "bad_request"
        );
    }
    let r = h
        .http
        .post(format!("{}/v1/ops/records", h.base))
        .bearer_auth(&h.token)
        .header("content-type", "application/json")
        .body(vec![b' '; 300 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert!(h.mock.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn search_and_get_are_forwarded() {
    let h = harness().await;
    let r = h
        .http
        .get(format!(
            "{}/v1/ops/search?q=go%20live&project=p1&kind=deploy&since=2026-10-01&until=2026-10-28&limit=5",
            h.base
        ))
        .bearer_auth(&h.token)
        .header("x-secretary-caller", "mcp:codex")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap(),
        json!({"items": [], "honcho": "ok"})
    );
    let seen = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.0, "search");
    assert_eq!(
        seen.1,
        "q=go+live&project=p1&kind=deploy&since=2026-10-01&until=2026-10-28&limit=5"
    );
    assert_eq!(seen.2, "mcp:codex");

    let r = h
        .http
        .get(format!("{}/v1/ops/search?limit=500", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    let r = h
        .http
        .get(format!("{}/v1/ops/records/0123456789abcdef", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["kind"], "deploy");
    let r = h
        .http
        .get(format!("{}/v1/ops/records/NOT-AN-ID", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(h.mock.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn owner_devices_only() {
    let h = harness().await;
    for token in [None, Some(h.svc.as_str()), Some("not-a-token")] {
        let mut req = h
            .http
            .post(format!("{}/v1/ops/records", h.base))
            .json(&json!({"kind": "status", "project": "p1", "title": "x"}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        assert_eq!(req.send().await.unwrap().status(), 401);
        let mut req = h.http.get(format!("{}/v1/ops/search?q=x", h.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        assert_eq!(req.send().await.unwrap().status(), 401);
    }
    assert!(h.mock.seen.lock().unwrap().is_empty());
}
