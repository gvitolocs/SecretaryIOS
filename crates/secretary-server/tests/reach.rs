//! `/v1/lucina/reach/*` against a mock Lucina: query values and the thread id are
//! checked before anything leaves the server, the MCP client travels to Lucina
//! inside a reply's `from`, the body is capped, and only owner devices get in.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const ID: &str = "rt-0123456789ab";

#[derive(Default)]
struct Mock {
    /// `(method, path, query, body)` of every request.
    seen: Mutex<Vec<(String, String, String, Value)>>,
}

async fn threads(State(m): State<Arc<Mock>>, RawQuery(q): RawQuery) -> Json<Value> {
    m.seen.lock().unwrap().push((
        "GET".into(),
        "threads".into(),
        q.unwrap_or_default(),
        Value::Null,
    ));
    Json(json!({"ok": true, "data": {"threads": [{
        "id": ID, "to": "claude", "kind": "issue", "subject": "Share link", "text": "It opens",
        "channel": "ios", "created": "2026-10-30T13:05:00Z", "status": "sent",
        "route": "lucina-mailbox", "detail": null, "replies": []}]}}))
}

async fn thread(State(m): State<Arc<Mock>>, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    m.seen
        .lock()
        .unwrap()
        .push(("GET".into(), id.clone(), String::new(), Value::Null));
    if id == "rt-ffffffffffff" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not_found", "message": "not found"})),
        );
    }
    (
        StatusCode::OK,
        Json(
            json!({"ok": true, "data": {"id": id, "status": "sent", "envelope": "[Lucina -> Claude] thread"}}),
        ),
    )
}

async fn reply(
    State(m): State<Arc<Mock>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    m.seen
        .lock()
        .unwrap()
        .push(("POST".into(), id.clone(), String::new(), body));
    if id == "rt-ffffffffffff" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not_found", "message": "not found"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"ok": true, "data": {"thread": id, "status": "answered", "delivered": true}})),
    )
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
        .route("/api/secretary/reach/threads", get(threads))
        .route("/api/secretary/reach/threads/{id}", get(thread))
        .route("/api/secretary/reach/threads/{id}/reply", post(reply))
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

impl H {
    fn get(&self, path_and_query: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/v1/lucina/reach/{path_and_query}", self.base))
            .bearer_auth(&self.token)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}/v1/lucina/reach/{path}", self.base))
            .bearer_auth(&self.token)
    }

    fn calls(&self) -> usize {
        self.mock.seen.lock().unwrap().len()
    }
}

#[tokio::test]
async fn the_mailbox_is_listed_with_its_query() {
    let h = harness().await;
    let r = h
        .get("threads?to=claude&status=open&limit=5")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["threads"][0]["id"], ID);
    let seen = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.2, "to=claude&status=open&limit=5");

    // No query: Lucina's own defaults apply.
    let r = h.get("threads").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(h.mock.seen.lock().unwrap().last().unwrap().2, "");
}

#[tokio::test]
async fn bad_queries_and_ids_never_reach_lucina() {
    let h = harness().await;
    for q in [
        "threads?to=owner",
        "threads?status=pending",
        "threads?limit=0",
        "threads?limit=101",
        "threads?limit=ten",
        "threads?extra=1",
        "threads/rt-0123456789AB",
        "threads/rt-0123456789a",
        "threads/dc-0123456789ab",
    ] {
        let r = h.get(q).send().await.unwrap();
        assert_eq!(r.status(), 400, "{q}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"]["code"],
            "bad_request",
            "{q}"
        );
    }
    for id in ["rt-0123456789AB", "rt-0123456789abc", "dc-0123456789ab"] {
        let r = h
            .post(&format!("threads/{id}/reply"))
            .json(&json!({"text": "ok"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{id}");
    }
    assert_eq!(h.calls(), 0);
}

#[tokio::test]
async fn one_thread_and_lucinas_404() {
    let h = harness().await;
    let r = h.get(&format!("threads/{ID}")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.json::<Value>().await.unwrap()["envelope"].is_string());
    let r = h.get("threads/rt-ffffffffffff").send().await.unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "thread_not_found"
    );
    let r = h
        .post("threads/rt-ffffffffffff/reply")
        .json(&json!({"text": "ok"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn a_reply_names_the_session_and_the_mcp_client() {
    let h = harness().await;
    let r = h
        .post(&format!("threads/{ID}/reply"))
        .header("x-secretary-caller", "mcp:claude")
        .json(&json!({"from": "coordinator", "text": "  Fixed in 0.9.  "}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["delivered"], true);
    assert_eq!(v["status"], "answered");
    let seen = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!((seen.0.as_str(), seen.1.as_str()), ("POST", ID));
    assert_eq!(
        seen.3,
        json!({"from": "coordinator (mcp:claude)", "text": "Fixed in 0.9."})
    );

    // No `from`: Claude, still with the client. An app caller is the name alone.
    h.post(&format!("threads/{ID}/reply"))
        .header("x-secretary-caller", "mcp:codex")
        .json(&json!({"text": "ok"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        h.mock.seen.lock().unwrap().last().unwrap().3["from"],
        "Claude (mcp:codex)"
    );
    h.post(&format!("threads/{ID}/reply"))
        .json(&json!({"text": "ok", "from": "owner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        h.mock.seen.lock().unwrap().last().unwrap().3["from"],
        "owner"
    );

    // A caller header that is not a plain MCP name counts as the app, not as a name.
    h.post(&format!("threads/{ID}/reply"))
        .header("x-secretary-caller", "Evil Name")
        .json(&json!({"text": "ok", "from": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(h.mock.seen.lock().unwrap().last().unwrap().3["from"], "x");
}

#[tokio::test]
async fn bad_replies_and_big_bodies_never_reach_lucina() {
    let h = harness().await;
    for body in [
        json!({}),
        json!({"text": "   "}),
        json!({"text": "x".repeat(8001)}),
        json!({"text": "ok", "from": "f".repeat(61)}),
        json!({"text": "ok", "from": "two\nlines"}),
        json!({"text": "ok", "thread": "rt-ffffffffffff"}),
        json!({"text": "a\u{7}b"}),
        json!(["ok"]),
    ] {
        let r = h
            .post(&format!("threads/{ID}/reply"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{body}");
    }
    let r = h
        .post(&format!("threads/{ID}/reply"))
        .header("content-type", "application/json")
        .body(vec![b' '; 100 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(h.calls(), 0);

    // 8000 multi-byte characters still fit.
    let r = h
        .post(&format!("threads/{ID}/reply"))
        .json(&json!({"text": "é".repeat(8000)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn owner_devices_only() {
    let h = harness().await;
    for token in [None, Some(h.svc.as_str()), Some("not-a-token")] {
        let with = |mut req: reqwest::RequestBuilder| {
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            req
        };
        let base = &h.base;
        for req in [
            h.http.get(format!("{base}/v1/lucina/reach/threads")),
            h.http.get(format!("{base}/v1/lucina/reach/threads/{ID}")),
            h.http
                .post(format!("{base}/v1/lucina/reach/threads/{ID}/reply"))
                .json(&json!({"text": "ok"})),
        ] {
            assert_eq!(with(req).send().await.unwrap().status(), 401);
        }
    }
    assert_eq!(h.calls(), 0);
}
