//! `/v1/parcels*` against a mock Lucina: method, path and body reach her; the 503 and the 404 pass
//! through as the contract says; only owner devices get in.

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

const ID: &str = "pc-1a2b3c4d";

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<(String, String, Value)>>,
}

fn envelope_error(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": {"code": code, "message": "nope"}})))
}

async fn lucina(State(m): State<Arc<Mock>>, method: Method, OriginalUri(uri): OriginalUri, _headers: HeaderMap, body: Option<Json<Value>>) -> (StatusCode, Json<Value>) {
    let body = body.map_or(Value::Null, |b| b.0);
    m.seen.lock().unwrap().push((method.to_string(), uri.to_string(), body));
    let path = uri.path();
    if path.contains("pc-00000000") {
        return envelope_error(StatusCode::NOT_FOUND, "not_found");
    }
    if path.contains("pc-5e5e5e5e") {
        return envelope_error(StatusCode::NOT_FOUND, "parcels_disabled");
    }
    if path.contains("pc-bbbbbbbb") {
        return envelope_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    }
    if path.ends_with("/parcels") && method == Method::GET {
        return (StatusCode::OK, Json(json!({"ok": true, "data": {"parcels": [], "digestHour": 9}})));
    }
    if method == Method::DELETE {
        return (StatusCode::OK, Json(json!({"ok": true, "data": {"ok": true}})));
    }
    (StatusCode::OK, Json(json!({"ok": true, "data": {"parcel": {"id": ID}}})))
}

struct H {
    base: String,
    token: String,
    svc: String,
    mock: Arc<Mock>,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

async fn harness_to(lucina_url: Option<String>) -> H {
    let mock = Arc::new(Mock::default());
    let url = match lucina_url {
        Some(u) => u,
        None => {
            let router = Router::new().fallback(any(lucina)).with_state(mock.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
            url
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let svc_path = dir.path().join("service.token");
    secretary_server::service::write_new(&svc_path, false).unwrap();
    cfg.service_token = Some(ServiceToken::read_file(svc_path.to_str().unwrap()).unwrap());
    let svc = std::fs::read_to_string(&svc_path).unwrap().trim().to_owned();
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("iPhone").unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move { serve_on(l, app(state), std::future::pending()).await.unwrap() });
    H { base, token, svc, mock, http: reqwest::Client::new(), _dir: dir }
}

async fn harness() -> H {
    harness_to(None).await
}

async fn send(h: &H, method: Method, path: &str, body: Option<&Value>) -> (u16, Value) {
    let mut r = h.http.request(method, format!("{}{path}", h.base)).bearer_auth(&h.token);
    if let Some(b) = body {
        r = r.json(b);
    }
    let r = r.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json::<Value>().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn every_route_forwards_method_path_and_body() {
    let h = harness().await;
    let add = json!({"number": "3UW1S8IAAKA23", "carrier": "poste-italiane", "label": "Shoes"});
    let rename = json!({"label": null});
    let cases: Vec<(Method, String, Option<Value>, &str)> = vec![
        (Method::GET, "/v1/parcels".into(), None, "/api/secretary/parcels"),
        (Method::POST, "/v1/parcels".into(), Some(add), "/api/secretary/parcels"),
        (Method::GET, format!("/v1/parcels/{ID}"), None, "/api/secretary/parcels/pc-1a2b3c4d"),
        (Method::POST, format!("/v1/parcels/{ID}/refresh"), None, "/api/secretary/parcels/pc-1a2b3c4d/refresh"),
        (Method::PATCH, format!("/v1/parcels/{ID}"), Some(rename), "/api/secretary/parcels/pc-1a2b3c4d"),
        (Method::DELETE, format!("/v1/parcels/{ID}"), None, "/api/secretary/parcels/pc-1a2b3c4d"),
    ];
    for (method, path, body, upstream) in cases {
        let (status, data) = send(&h, method.clone(), &path, body.as_ref()).await;
        assert_eq!(status, 200, "{method} {path}: {data}");
        let seen = h.mock.seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert_eq!((last.0.as_str(), last.1.as_str()), (method.as_str(), upstream), "{path}");
        if let Some(b) = body {
            assert_eq!(last.2, b, "{path}");
        }
    }
    let (_, list) = send(&h, Method::GET, "/v1/parcels", None).await;
    assert_eq!(list, json!({"parcels": [], "digestHour": 9}), "Lucina's JSON passes through");
    let (_, gone) = send(&h, Method::DELETE, &format!("/v1/parcels/{ID}"), None).await;
    assert_eq!(gone, json!({"ok": true}));
}

#[tokio::test]
async fn bad_requests_stop_here() {
    let h = harness().await;
    for (method, path, body) in [
        (Method::POST, "/v1/parcels", json!({"number": "abc"})),
        (Method::POST, "/v1/parcels", json!({"number": "ABCDE", "extra": 1})),
        (Method::POST, "/v1/parcels", json!([1])),
        (Method::PATCH, "/v1/parcels/pc-1a2b3c4d", json!({"label": "x".repeat(81)})),
        (Method::PATCH, "/v1/parcels/pc-1a2b3c4d", json!({})),
    ] {
        let (status, b) = send(&h, method.clone(), path, Some(&body)).await;
        assert_eq!((status, b["error"]["code"].as_str()), (400, Some("bad_request")), "{method} {path}: {b}");
    }
    for path in ["/v1/parcels/NOPE", "/v1/parcels/pc-1A2B3C4D", "/v1/parcels/NOPE/refresh"] {
        let method = if path.ends_with("refresh") { Method::POST } else { Method::GET };
        let (status, _) = send(&h, method, path, None).await;
        assert_eq!(status, 400, "{path}");
    }
    assert!(h.mock.seen.lock().unwrap().is_empty(), "nothing reached Lucina");
}

#[tokio::test]
async fn an_unknown_parcel_is_a_404_but_a_disabled_feature_is_a_503() {
    let h = harness().await;
    for (method, path) in [
        (Method::GET, "/v1/parcels/pc-00000000"),
        (Method::DELETE, "/v1/parcels/pc-00000000"),
        (Method::POST, "/v1/parcels/pc-00000000/refresh"),
    ] {
        let (status, b) = send(&h, method.clone(), path, None).await;
        assert_eq!((status, b["error"]["code"].as_str()), (404, Some("not_found")), "{method} {path}: {b}");
    }
    for (method, path) in [
        (Method::GET, "/v1/parcels/pc-5e5e5e5e"),
        (Method::GET, "/v1/parcels/pc-bbbbbbbb"),
        (Method::DELETE, "/v1/parcels/pc-bbbbbbbb"),
    ] {
        let (status, b) = send(&h, method.clone(), path, None).await;
        assert_eq!((status, b["error"]["code"].as_str()), (503, Some("parcels_unavailable")), "{method} {path}: {b}");
    }
}

#[tokio::test]
async fn lucina_unreachable_is_a_503_on_every_route() {
    let gone = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let h = harness_to(Some(gone)).await;
    for (method, path, body) in [
        (Method::GET, "/v1/parcels".to_owned(), None),
        (Method::POST, "/v1/parcels".to_owned(), Some(json!({"number": "ABCDE"}))),
        (Method::GET, format!("/v1/parcels/{ID}"), None),
        (Method::POST, format!("/v1/parcels/{ID}/refresh"), None),
        (Method::PATCH, format!("/v1/parcels/{ID}"), Some(json!({"label": "x"}))),
        (Method::DELETE, format!("/v1/parcels/{ID}"), None),
    ] {
        let (status, b) = send(&h, method.clone(), &path, body.as_ref()).await;
        assert_eq!((status, b["error"]["code"].as_str()), (503, Some("parcels_unavailable")), "{method} {path}: {b}");
    }
}

#[tokio::test]
async fn only_owner_devices_get_in() {
    let h = harness().await;
    for (method, path) in [(Method::GET, "/v1/parcels"), (Method::DELETE, "/v1/parcels/pc-1a2b3c4d")] {
        let r = h.http.request(method.clone(), format!("{}{path}", h.base)).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 401, "{method} {path}");
    }
    let r = h.http.get(format!("{}/v1/parcels", h.base)).bearer_auth(&h.svc).send().await.unwrap();
    assert_ne!(r.status().as_u16(), 200, "Lucina's service token isn't an owner device");
    assert!(h.mock.seen.lock().unwrap().is_empty());
}
