//! Deals map: `GET /v1/grocery/deals/map` forwards Lucina's
//! `grocery/deals/map` and adds the server's client-side Google Maps keys,
//! with the radius validated here (500..=15 000) before Lucina sees it.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::{Config, MapsKeys};
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    /// Every Lucina hit: (method, path with query, Authorization header).
    seen: Mutex<Vec<(String, String, String)>>,
    /// Force the next deals/map answer (503 shape test).
    deals_status: Mutex<Option<(StatusCode, Value)>>,
}

fn ok(status: StatusCode, data: Value) -> Response {
    (status, Json(json!({ "ok": true, "data": data }))).into_response()
}

fn fail(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": code, "message": format!("lucina says {code}")})),
    )
        .into_response()
}

fn deals_body() -> Value {
    json!({
        "generatedAt": "2026-10-24T12:00:00.000Z",
        "home": {"lat": 56.19633, "lng": 10.2011, "label": "Risskov"},
        "radiusM": 3000,
        "failures": [],
        "stores": [
            {"id": "salling:0847307a-1111-4222-8333-9c0d1e2f3a4b", "chain": "føtex", "chainKey": "foetex",
             "name": "føtex Skejby", "street": "Skelagervej 1-9", "lat": 56.193657, "lng": 10.193406,
             "distanceM": 560, "listMatches": [], "clearances": [], "flyerOffers": []},
            {"id": "tjek:4412", "chain": "Netto", "chainKey": "netto",
             "name": "Netto Risskov", "street": "Skelagervej 2", "lat": 56.198171, "lng": 10.1991,
             "distanceM": 240, "listMatches": [], "clearances": [], "flyerOffers": []}
        ]
    })
}

async fn mock_handler(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    _body: Bytes,
) -> Response {
    m.seen.lock().unwrap().push((
        method.to_string(),
        uri.to_string(),
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    ));
    if uri.path() == "/api/secretary/grocery/deals/map" {
        if let Some((status, body)) = m.deals_status.lock().unwrap().take() {
            if body.is_null() {
                return fail(status, "grocery_unavailable");
            }
            return ok(status, body);
        }
        return ok(StatusCode::OK, deals_body());
    }
    fail(StatusCode::NOT_FOUND, "not_found")
}

struct H {
    base: String,
    http: reqwest::Client,
    token: String,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

async fn harness(maps: MapsKeys) -> H {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .fallback(mock_handler)
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    cfg.maps = maps;
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(listener, app(state), std::future::pending())
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

impl H {
    async fn send(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn lucina_calls(&self) -> Vec<(String, String)> {
        self.mock
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(m, p, _)| (m.clone(), p.clone()))
            .collect()
    }
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

#[tokio::test]
async fn forwards_lucina_and_injects_the_maps_keys() {
    let h = harness(MapsKeys {
        web: Some("web-key-1".into()),
        ios: Some("ios-key-2".into()),
        map_id: Some("map-3".into()),
    })
    .await;
    let (s, b) = h.send("/v1/grocery/deals/map?radiusM=2026").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["home"]["label"], "Risskov", "Lucina payload passes");
    assert_eq!(b["stores"][0]["chain"], "føtex");
    assert_eq!(
        b["maps"],
        json!({"webKey": "web-key-1", "iosKey": "ios-key-2", "mapId": "map-3"})
    );
    assert_eq!(
        h.lucina_calls(),
        vec![(
            "GET".into(),
            "/api/secretary/grocery/deals/map?radiusM=2026".into()
        )]
    );
    // the bearer at Lucina is the server's token, never the device's
    assert_eq!(h.mock.seen.lock().unwrap()[0].2, "Bearer lucina-secret");
    // no radius: Lucina gets the plain path
    let (s, b) = h.send("/v1/grocery/deals/map").await;
    assert_eq!(s, 200, "{b}");
    assert!(h
        .lucina_calls()
        .last()
        .unwrap()
        .1
        .ends_with("/grocery/deals/map"));
}

#[tokio::test]
async fn maps_keys_are_null_when_not_configured() {
    let h = harness(MapsKeys::default()).await;
    let (s, b) = h.send("/v1/grocery/deals/map").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["maps"],
        json!({"webKey": Value::Null, "iosKey": Value::Null, "mapId": Value::Null})
    );
}

#[tokio::test]
async fn the_radius_is_validated_locally() {
    let h = harness(MapsKeys::default()).await;
    for path in [
        "/v1/grocery/deals/map?radiusM=499",
        "/v1/grocery/deals/map?radiusM=15001",
        "/v1/grocery/deals/map?radiusM=abc",
        "/v1/grocery/deals/map?radiusM=",
        "/v1/grocery/deals/map?radiusM=-500",
    ] {
        let (s, b) = h.send(path).await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{path}: {b}");
        assert!(b["error"]["message"].as_str().unwrap().contains("radiusM"));
    }
    assert!(h.lucina_calls().is_empty(), "Lucina is never hit");
}

#[tokio::test]
async fn the_upstream_503_is_the_usual_error_shape() {
    let h = harness(MapsKeys::default()).await;
    *h.mock.deals_status.lock().unwrap() = Some((StatusCode::SERVICE_UNAVAILABLE, Value::Null));
    let (s, b) = h.send("/v1/grocery/deals/map").await;
    assert_eq!((s, code(&b)), (503, "grocery_unavailable"));
    assert_eq!(b["error"]["message"], "lucina says grocery_unavailable");
}
