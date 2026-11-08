//! `/v1/flaws/*` against a mock Lucina: bodies, the caller and her status reach through; only
//! owner devices get in.

use std::sync::{Arc, Mutex};

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::any;
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::service::ServiceToken;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<(String, String, String, Value)>>,
}

async fn lucina(State(m): State<Arc<Mock>>, method: Method, OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Option<Json<Value>>) -> (StatusCode, Json<Value>) {
    let caller = headers.get("x-lucina-caller").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let body = body.map_or(Value::Null, |b| b.0);
    let refused = body["status"] == "verified";
    m.seen.lock().unwrap().push((method.to_string(), uri.to_string(), caller, body));
    if refused {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "error": {"code": "evidence_required", "message": "verified needs evidence"}})));
    }
    (StatusCode::OK, Json(json!({"ok": true, "data": {"id": "fl-0123abcd"}})))
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
    let router = Router::new().fallback(any(lucina)).with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let svc_path = dir.path().join("service.token");
    secretary_server::service::write_new(&svc_path, false).unwrap();
    cfg.service_token = Some(ServiceToken::read_file(svc_path.to_str().unwrap()).unwrap());
    let svc = std::fs::read_to_string(&svc_path).unwrap().trim().to_owned();
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("Secretary MCP").unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move { serve_on(l, app(state), std::future::pending()).await.unwrap() });
    H { base, token, svc, mock, http: reqwest::Client::new(), _dir: dir }
}

#[tokio::test]
async fn every_route_forwards_with_the_caller() {
    let h = harness().await;
    let id = "fl-0123abcd";
    let cases: Vec<(Method, String, Option<Value>, &str)> = vec![
        (Method::POST, "/v1/flaws".into(), Some(json!({"words": "x"})), "/api/secretary/flaws"),
        (Method::GET, "/v1/flaws?q=box&state=open&platform=web&status=fixed&limit=5".into(), None, "/api/secretary/flaws?q=box&state=open&platform=web&status=fixed&limit=5"),
        (Method::GET, "/v1/flaws/matrix".into(), None, "/api/secretary/flaws/matrix"),
        (Method::GET, format!("/v1/flaws/{id}"), None, "/api/secretary/flaws/fl-0123abcd"),
        (Method::PATCH, format!("/v1/flaws/{id}"), Some(json!({"severity": "low"})), "/api/secretary/flaws/fl-0123abcd"),
        (Method::PUT, format!("/v1/flaws/{id}/targets/web"), Some(json!({"layer": "client"})), "/api/secretary/flaws/fl-0123abcd/targets/web"),
        (Method::POST, format!("/v1/flaws/{id}/dispatch"), Some(json!({"platform": "web"})), "/api/secretary/flaws/fl-0123abcd/dispatch"),
    ];
    for (method, path, body, upstream) in cases {
        let mut r = h.http.request(method.clone(), format!("{}{path}", h.base)).bearer_auth(&h.token).header("x-secretary-caller", "mcp:claude");
        if let Some(b) = &body {
            r = r.json(b);
        }
        let r = r.send().await.unwrap();
        assert!(r.status().is_success(), "{method} {path}: {}", r.status());
        let seen = h.mock.seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert_eq!((last.0.as_str(), last.1.as_str()), (method.as_str(), upstream), "{path}");
        assert_eq!(last.2, "mcp:claude", "{path}");
        if let Some(b) = body {
            assert_eq!(last.3, b, "{path}");
        }
    }
}

#[tokio::test]
async fn her_refusal_reaches_the_client() {
    let h = harness().await;
    let r = h.http.put(format!("{}/v1/flaws/fl-0123abcd/targets/web", h.base)).bearer_auth(&h.token).json(&json!({"status": "verified"})).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 422);
}

#[tokio::test]
async fn bad_ids_and_outsiders_stay_out() {
    let h = harness().await;
    for path in ["/v1/flaws/NOPE", "/v1/flaws/fl-0123ABCD", "/v1/flaws/a%2F..%2Fx"] {
        let r = h.http.get(format!("{}{path}", h.base)).bearer_auth(&h.token).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "{path}");
    }
    assert_eq!(h.http.get(format!("{}/v1/flaws", h.base)).send().await.unwrap().status().as_u16(), 401);
    let r = h.http.get(format!("{}/v1/flaws", h.base)).bearer_auth(&h.svc).send().await.unwrap();
    assert_ne!(r.status().as_u16(), 200, "Lucina's service token isn't an owner device");
    assert!(h.mock.seen.lock().unwrap().is_empty());
}
