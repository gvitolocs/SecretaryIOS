//! AU Find for the apps and the MCP: `/v1/au/{health,people,buildings,map.png}` forward Lucina's `au/*` with the
//! server's Lucina token, behind the device login; the query is validated first and only known parameters travel.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[derive(Default)]
struct Mock {
    /// (path?query, Authorization)
    seen: Mutex<Vec<(String, String)>>,
}

async fn lucina(State(m): State<Arc<Mock>>, uri: Uri, headers: axum::http::HeaderMap) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    m.seen.lock().unwrap().push((
        uri.path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        auth,
    ));
    match uri.path() {
        // bare JSON, as lucina-rs answers the AU routes
        "/api/secretary/au/health" => Json(json!({"reachable": true, "af_server": {"version": "1"}})).into_response(),
        "/api/secretary/au/people" => Json(json!({"source": "aufind", "status": "found", "query": "Jens Hansen",
            "tried": [{"q": "Jens Hansen", "total": 1}], "total": 1,
            "people": [{"uuid": "u1", "name": "Jens Hansen", "position": "Lektor", "units": ["Datalogi"],
                "email": "jh@cs.au.dk", "phone": "+45 8715 0000", "au_room": "1532-123", "source": "aufind"}]}))
        .into_response(),
        // the {ok, data} envelope is unwrapped too
        "/api/secretary/au/buildings" => Json(json!({"ok": true, "data": {"source": "aufind", "status": "found",
            "buildings": [{"id": "1532", "names": ["Nygaard"], "latitude": 56.17, "longitude": 10.19}]}}))
        .into_response(),
        "/api/secretary/au/map.png" => ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response(),
        _ => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not_found"}))).into_response(),
    }
}

async fn server(lucina_url: String) -> (String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.keep().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let s = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", s.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(s, app(state), std::future::pending())
            .await
            .unwrap()
    });
    (base, token)
}

#[tokio::test]
async fn au_routes_forward_to_lucina() {
    let mock = Arc::new(Mock::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    let router = Router::new().fallback(lucina).with_state(mock.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    let (base, token) = server(lucina_url).await;
    let http = reqwest::Client::new();
    let get = |path: &str| http.get(format!("{base}{path}")).bearer_auth(&token).send();

    let r = get("/v1/au/health").await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["reachable"], true);

    let r = get("/v1/au/people?q=Jens%20Hansen&lang=da").await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["status"], "found", "{v}");
    assert_eq!(v["people"][0]["au_room"], "1532-123");

    let r = get("/v1/au/buildings?q=1532").await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["buildings"][0]["id"], "1532", "{v}");

    let r = get("/v1/au/map.png?building=1532&z=17").await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.headers()["cache-control"], "private, max-age=3600");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &PNG);
    let r = get("/v1/au/map.png?lat=56.1697&lng=10.2039&z=15")
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // validated before anything reaches Lucina
    let before = mock.seen.lock().unwrap().len();
    for p in [
        "/v1/au/people",
        "/v1/au/people?q=",
        "/v1/au/people?q=x&lang=it",
        &format!("/v1/au/people?q={}", "x".repeat(201)),
        "/v1/au/people?q=x&extra=1",
        "/v1/au/buildings",
        "/v1/au/map.png",
        "/v1/au/map.png?building=..%2Fx",
        "/v1/au/map.png?building=1234567",
        "/v1/au/map.png?building=1532&z=13",
        "/v1/au/map.png?building=1532&z=19",
        "/v1/au/map.png?lat=53&lng=10",
        "/v1/au/map.png?lat=56&lng=17",
        "/v1/au/map.png?lat=56",
        "/v1/au/map.png?building=1532&lat=56&lng=10",
    ] {
        let r = get(p).await.unwrap();
        assert_eq!(r.status(), 400, "{p}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"]["code"],
            "bad_request",
            "{p}"
        );
    }
    // no device login: nothing reaches Lucina
    for p in [
        "/v1/au/health",
        "/v1/au/people?q=x",
        "/v1/au/buildings?q=x",
        "/v1/au/map.png?building=1532",
    ] {
        assert_eq!(
            http.get(format!("{base}{p}"))
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{p}"
        );
    }
    assert_eq!(mock.seen.lock().unwrap().len(), before);

    // Lucina sees exactly the validated query and the server's token, never the device token
    let seen = mock.seen.lock().unwrap();
    assert!(seen.iter().all(|(_, auth)| auth == "Bearer lucina-secret"));
    let paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/secretary/au/health",
            "/api/secretary/au/people?q=Jens+Hansen&lang=da",
            "/api/secretary/au/buildings?q=1532",
            "/api/secretary/au/map.png?building=1532&z=17",
            "/api/secretary/au/map.png?lat=56.1697&lng=10.2039&z=15",
        ]
    );
}

#[tokio::test]
async fn au_unreachable_is_503() {
    // a port nobody listens on
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    let (base, token) = server(lucina_url).await;
    let http = reqwest::Client::new();
    for p in [
        "/v1/au/health",
        "/v1/au/people?q=Jens",
        "/v1/au/buildings?q=1532",
        "/v1/au/map.png?building=1532",
    ] {
        let r = http
            .get(format!("{base}{p}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 503, "{p}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"]["code"],
            "au_unavailable",
            "{p}"
        );
    }
}
