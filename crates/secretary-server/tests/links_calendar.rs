//! Links and calendar end to end: the real router against a mock Google
//! (OAuth token endpoint, revoke, Calendar API v3), a mock ICS feed and a
//! mock secretary-mcp status endpoint.
//!
//! Covers: the OAuth code exchange with PKCE and a single-use `state`,
//! secrets sealed at rest, full then incremental sync with sync tokens, the
//! `410 Gone` full resync, token refresh, revocation (→ reconnect), writes
//! with `confirm`, the service token, proposals, ICS subscriptions with
//! conditional GET, the iPhone snapshot and custom links.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::Client;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::google::GoogleConfig;
use secretary_server::links::McpStatusConfig;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Default)]
struct G {
    /// code_challenge the test read from the consent URL.
    challenge: Option<String>,
    /// `invalid_grant` on refresh.
    revoked: bool,
    /// The next API call answers 401 (expired access token).
    expire_once: bool,
    /// The next sync-token request answers 410.
    gone: bool,
    /// Incremental phase: what `syncToken=s1` returns.
    phase: u32,
    revoke_calls: Vec<String>,
    event_queries: Vec<HashMap<String, String>>,
    writes: Vec<(String, String, Value)>,
    access: String,
    refreshes: u32,
}

type Mock = Arc<Mutex<G>>;

fn ev_master() -> Value {
    json!({"id": "gym", "status": "confirmed", "summary": "Gym",
           "start": {"dateTime": "2026-11-02T18:00:00+01:00", "timeZone": "Europe/Rome"},
           "end": {"dateTime": "2026-11-02T19:00:00+01:00", "timeZone": "Europe/Rome"},
           "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=4"]})
}

fn ev_single() -> Value {
    json!({"id": "dentist", "status": "confirmed", "summary": "Dentist",
           "start": {"dateTime": "2026-11-03T09:30:00+01:00"}, "end": {"dateTime": "2026-11-03T10:00:00+01:00"}})
}

async fn token(State(m): State<Mock>, body: String) -> Response {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let mut g = m.lock().unwrap();
    assert_eq!(form["client_id"], "cid.apps.googleusercontent.com");
    assert_eq!(form["client_secret"], "test-client-secret");
    match form["grant_type"].as_str() {
        "authorization_code" => {
            if form["code"] != "good-code" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            }
            let want = g.challenge.clone().unwrap();
            let got = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
            assert_eq!(got, want, "PKCE verifier matches the challenge");
            g.access = "at1".into();
            Json(json!({"access_token": "at1", "expires_in": 3599, "refresh_token": "rt1",
                        "scope": "https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.readonly",
                        "token_type": "Bearer"}))
            .into_response()
        }
        "refresh_token" => {
            assert_eq!(form["refresh_token"], "rt1");
            if g.revoked {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}))).into_response();
            }
            g.refreshes += 1;
            g.access = format!("at{}", g.refreshes + 1);
            Json(json!({"access_token": g.access, "expires_in": 3599})).into_response()
        }
        other => panic!("grant {other}"),
    }
}

async fn revoke(State(m): State<Mock>, body: String) -> StatusCode {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    m.lock().unwrap().revoke_calls.push(form["token"].clone());
    StatusCode::OK
}

fn authed(m: &Mock, h: &HeaderMap) -> Option<Response> {
    let mut g = m.lock().unwrap();
    let auth = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if g.expire_once {
        g.expire_once = false;
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"code": 401, "message": "Invalid Credentials"}})),
            )
                .into_response(),
        );
    }
    if auth != format!("Bearer {}", g.access) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"code": 401, "message": "Invalid Credentials"}})),
            )
                .into_response(),
        );
    }
    None
}

async fn primary(State(m): State<Mock>, h: HeaderMap) -> Response {
    if let Some(r) = authed(&m, &h) {
        return r;
    }
    Json(json!({"id": "owner@example.com", "timeZone": "Europe/Rome"})).into_response()
}

async fn events(
    State(m): State<Mock>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Some(r) = authed(&m, &h) {
        return r;
    }
    let mut g = m.lock().unwrap();
    g.event_queries.push(q.clone());
    assert_eq!(q.get("singleEvents").map(String::as_str), Some("false"));
    match q.get("syncToken").map(String::as_str) {
        None => {
            assert!(q.contains_key("timeMin"), "a full sync is bounded");
            if !q.contains_key("pageToken") {
                Json(json!({"timeZone": "Europe/Rome", "items": [ev_master()], "nextPageToken": "p2"})).into_response()
            } else {
                Json(json!({"timeZone": "Europe/Rome", "items": [ev_single()], "nextSyncToken": "s1"})).into_response()
            }
        }
        Some(_) if g.gone => {
            g.gone = false;
            (StatusCode::GONE, Json(json!({"error": {"code": 410, "message": "Sync token is no longer valid, a full sync is required."}}))).into_response()
        }
        Some("s1") => {
            g.phase += 1;
            let items = if g.phase == 1 {
                vec![
                    json!({"id": "dentist", "status": "cancelled"}),
                    json!({"id": "lunch", "status": "confirmed", "summary": "Lunch with Anna",
                           "start": {"dateTime": "2026-11-04T12:30:00+01:00"}, "end": {"dateTime": "2026-11-04T13:30:00+01:00"}}),
                    json!({"id": "gym_20261109T170000Z", "recurringEventId": "gym", "status": "cancelled",
                           "originalStartTime": {"dateTime": "2026-11-09T18:00:00+01:00", "timeZone": "Europe/Rome"}}),
                ]
            } else {
                vec![]
            };
            Json(json!({"timeZone": "Europe/Rome", "items": items, "nextSyncToken": "s1"}))
                .into_response()
        }
        Some(other) => panic!("sync token {other}"),
    }
}

async fn create(State(m): State<Mock>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = authed(&m, &h) {
        return r;
    }
    m.lock()
        .unwrap()
        .writes
        .push(("POST".into(), String::new(), body.clone()));
    let mut ev = body.clone();
    ev["id"] = json!("new1");
    ev["status"] = json!("confirmed");
    ev["htmlLink"] = json!("https://calendar.google.com/event?eid=new1");
    (StatusCode::OK, Json(ev)).into_response()
}

async fn one(
    State(m): State<Mock>,
    h: HeaderMap,
    Path(id): Path<String>,
    method: axum::http::Method,
    body: Bytes,
) -> Response {
    if let Some(r) = authed(&m, &h) {
        return r;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.lock()
        .unwrap()
        .writes
        .push((method.to_string(), id.clone(), body.clone()));
    match method.as_str() {
        "GET" => Json(json!({"id": id, "summary": "Lunch with Anna",
                             "start": {"dateTime": "2026-11-04T12:30:00+01:00"}, "end": {"dateTime": "2026-11-04T13:30:00+01:00"}})).into_response(),
        "PATCH" => {
            let mut ev = json!({"id": id, "status": "confirmed", "summary": "Lunch with Anna"});
            for (k, v) in body.as_object().unwrap() {
                ev[k] = v.clone();
            }
            Json(ev).into_response()
        }
        "DELETE" => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

const FEED: &str = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:Calcio\r\nBEGIN:VEVENT\r\nUID:match@x\r\nSUMMARY:Match\r\nDTSTART;TZID=Europe/Rome:20261105T203000\r\nDTEND;TZID=Europe/Rome:20261105T223000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

async fn feed(State(hits): State<Arc<Mutex<Vec<Option<String>>>>>, h: HeaderMap) -> Response {
    let inm = h
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    hits.lock().unwrap().push(inm.clone());
    if inm.as_deref() == Some("\"v1\"") {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [("etag", "\"v1\""), ("content-type", "text/calendar")],
        FEED,
    )
        .into_response()
}

async fn mcp_status(h: HeaderMap) -> Response {
    if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer status-token") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"clients": [
        {"caller": "claude-code", "via": "token", "scopes": ["read", "chat", "write"], "active": true, "createdAt": 1792500000000_i64},
        {"caller": "chatgpt", "via": "oauth", "scopes": ["read", "chat"], "active": true},
    ]}))
    .into_response()
}

async fn spawn(router: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    base
}

struct T {
    base: String,
    http: Client,
    token: String,
    svc: String,
    mock: Mock,
    ics_hits: Arc<Mutex<Vec<Option<String>>>>,
    ics_base: String,
    data: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl T {
    async fn new() -> Self {
        let mock: Mock = Arc::default();
        let g = Router::new()
            .route("/token", post(token))
            .route("/revoke", post(revoke))
            .route("/calendar/v3/calendars/primary", get(primary))
            .route(
                "/calendar/v3/calendars/primary/events",
                get(events).post(create),
            )
            .route(
                "/calendar/v3/calendars/primary/events/{id}",
                get(one).patch(one).delete(one),
            )
            .route("/internal/status", get(mcp_status))
            .with_state(mock.clone());
        let gbase = spawn(g).await;
        let ics_hits: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let ics_base = spawn(
            Router::new()
                .route("/feed.ics", get(feed))
                .with_state(ics_hits.clone()),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        let mut gc = GoogleConfig::new(
            "cid.apps.googleusercontent.com".into(),
            "test-client-secret".into(),
        );
        gc.auth_url = format!("{gbase}/auth");
        gc.token_url = format!("{gbase}/token");
        gc.revoke_url = format!("{gbase}/revoke");
        gc.api_base = gbase.clone();
        cfg.google = Some(gc);
        cfg.ics_allow_local = true;
        cfg.mcp_status = Some(McpStatusConfig {
            url: format!("{gbase}/internal/status"),
            token: "status-token".into(),
        });
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let svc = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let state = AppState::new(&cfg).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            serve_on(l, app(state), std::future::pending())
                .await
                .unwrap()
        });
        Self {
            base,
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token,
            svc,
            mock,
            ics_hits,
            ics_base,
            data: cfg.data_dir.clone(),
            _dir: dir,
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        rb = rb.bearer_auth(bearer.unwrap_or(&self.token));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let s = r.status();
        (
            StatusCode::from_u16(s.as_u16()).unwrap(),
            r.json().await.unwrap_or(Value::Null),
        )
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, None, None).await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, Some(body), None)
            .await
    }

    async fn location(&self, path: &str) -> (u16, String) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        (
            r.status().as_u16(),
            r.headers()
                .get("location")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default(),
        )
    }

    fn card(links: &Value, id: &str) -> Value {
        links["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["cards"].as_array().unwrap().clone())
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("card {id}"))
    }

    async fn connect(&self) {
        let (s, v) = self.post("/v1/links/google/connect", json!({})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let url = reqwest::Url::parse(v["authorizeUrl"].as_str().unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        self.mock.lock().unwrap().challenge = Some(q["code_challenge"].clone());
        let (st, loc) = self
            .location(&format!(
                "/v1/links/google/callback?code=good-code&state={}",
                q["state"]
            ))
            .await;
        assert_eq!((st, loc.as_str()), (303, "/links?google=connected"));
    }

    fn titles(v: &Value) -> Vec<String> {
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["title"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[tokio::test]
async fn google_oauth_sync_writes_and_revocation() {
    let t = T::new().await;

    // Before connecting: Google configured, calendar disconnected; AI clients from the MCP.
    let (s, links) = t.get("/v1/links").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(links["google"]["configured"], true);
    assert_eq!(T::card(&links, "google-calendar")["status"], "disconnected");
    assert_eq!(T::card(&links, "google-gmail")["status"], "available");
    assert_eq!(T::card(&links, "ai:claude-code")["status"], "connected");
    assert_eq!(T::card(&links, "ai:grok")["status"], "disconnected");
    assert_eq!(T::card(&links, "channel:alexa")["status"], "planned");

    // The consent URL.
    let (_, v) = t
        .post(
            "/v1/links/google/connect",
            json!({"services": ["calendar"]}),
        )
        .await;
    let url = reqwest::Url::parse(v["authorizeUrl"].as_str().unwrap()).unwrap();
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert!(url.as_str().contains("/auth?"));
    assert_eq!(q["access_type"], "offline");
    assert_eq!(q["prompt"], "consent");
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["scope"].contains("calendar.events") && q["scope"].contains("calendar.readonly"));
    assert!(!v.to_string().contains("test-client-secret"));
    // A forged state is refused; the real one works once.
    let (st, loc) = t
        .location("/v1/links/google/callback?code=good-code&state=forged")
        .await;
    assert_eq!(
        (st, loc.as_str()),
        (303, "/links?google=error&reason=expired_or_unknown_state")
    );
    t.mock.lock().unwrap().challenge = Some(q["code_challenge"].clone());
    let cb = format!(
        "/v1/links/google/callback?code=good-code&state={}",
        q["state"]
    );
    assert_eq!(
        t.location(&cb).await,
        (303, "/links?google=connected".into())
    );
    assert_eq!(
        t.location(&cb).await.1,
        "/links?google=error&reason=expired_or_unknown_state"
    );
    // Bad services, unauthenticated connect.
    assert_eq!(
        t.post("/v1/links/google/connect", json!({"services": ["photos"]}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let anon = t
        .http
        .post(format!("{}/v1/links/google/connect", t.base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status().as_u16(), 401);

    // Secrets are sealed at rest and never returned.
    let raw = std::fs::read_to_string(t.data.join("links.json")).unwrap();
    assert!(!raw.contains("rt1") && raw.contains("enc1:"));
    assert_eq!(
        std::fs::metadata(t.data.join("links.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(t.data.join("links.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // The callback starts a full sync (two pages) in the background.
    let mut links = Value::Null;
    for _ in 0..100 {
        links = t.get("/v1/links").await.1;
        if !T::card(&links, "google-calendar")["lastSyncAt"].is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(t.mock.lock().unwrap().event_queries.len() == 2);
    let gc = T::card(&links, "google-calendar");
    assert_eq!(
        (gc["status"].as_str(), gc["account"].as_str()),
        (Some("connected"), Some("owner@example.com"))
    );
    assert!(!links.to_string().contains("rt1"));
    let (s, ev) = t
        .get("/v1/calendar/events?from=2026-11-02&to=2026-11-30")
        .await;
    assert_eq!(s, StatusCode::OK, "{ev}");
    assert_eq!(T::titles(&ev), ["Gym", "Dentist", "Gym", "Gym", "Gym"]);
    assert_eq!(ev["events"][0]["id"], "google:gym_20261102T170000Z");
    assert_eq!(ev["events"][0]["editable"], true);

    // Incremental: the dentist is deleted, lunch added, one gym instance cancelled.
    let (_, r) = t.post("/v1/calendar/sync", json!({})).await;
    assert_eq!(r["google"]["changed"], 3, "{r}");
    let last = t
        .mock
        .lock()
        .unwrap()
        .event_queries
        .last()
        .cloned()
        .unwrap();
    assert_eq!(last.get("syncToken").map(String::as_str), Some("s1"));
    let (_, ev) = t
        .get("/v1/calendar/events?from=2026-11-02&to=2026-11-30")
        .await;
    assert_eq!(T::titles(&ev), ["Gym", "Lunch with Anna", "Gym", "Gym"]);

    // 410 Gone: a full resync.
    t.mock.lock().unwrap().gone = true;
    let (_, r) = t.post("/v1/calendar/sync", json!({})).await;
    assert_eq!(r["google"]["ok"], true, "{r}");
    let qs = t.mock.lock().unwrap().event_queries.clone();
    assert!(
        !qs[qs.len() - 2].contains_key("syncToken") && qs[qs.len() - 1].contains_key("pageToken")
    );
    let (_, ev) = t
        .get("/v1/calendar/events?from=2026-11-02&to=2026-11-30")
        .await;
    assert_eq!(T::titles(&ev), ["Gym", "Dentist", "Gym", "Gym", "Gym"]);

    // An expired access token is refreshed transparently.
    t.mock.lock().unwrap().expire_once = true;
    let (_, r) = t.post("/v1/calendar/sync", json!({})).await;
    assert_eq!(r["google"]["ok"], true, "{r}");
    assert_eq!(t.mock.lock().unwrap().refreshes, 1);

    // Writes to Google (new events go to Lucina's own calendar unless the target is Google).
    let (s, c) = t
        .post(
            "/v1/calendar/events",
            json!({"title": "Call Marco", "start": "2026-11-06T15:00:00+01:00", "target": "google"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["event"]["id"], "google:new1");
    assert_eq!(c["event"]["end"], "2026-11-06T15:00:00.000Z");
    let w = t.mock.lock().unwrap().writes.last().cloned().unwrap();
    assert_eq!(
        w.2["start"],
        json!({"dateTime": "2026-11-06T15:00:00+01:00", "timeZone": "Europe/Copenhagen"})
    );
    assert_eq!(w.2["extendedProperties"]["private"]["addedBy"], "test iPhone");
    let (s, e) = t
        .call(
            reqwest::Method::PATCH,
            "/v1/calendar/events/google:lunch",
            Some(json!({"start": "2026-11-04T13:00:00+01:00"})),
            None,
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            Some("confirmation_required")
        )
    );
    let (s, e) = t
        .call(
            reqwest::Method::PATCH,
            "/v1/calendar/events/google:lunch",
            Some(json!({"confirm": true, "start": "2026-11-04T13:00:00+01:00"})),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{e}");
    let w = t.mock.lock().unwrap().writes.last().cloned().unwrap();
    assert_eq!((w.0.as_str(), w.1.as_str()), ("PATCH", "lunch"));
    // the one-hour duration is kept
    assert_eq!(w.2["end"]["dateTime"], "2026-11-04T14:00:00+01:00");
    let (s, _) = t
        .call(
            reqwest::Method::DELETE,
            "/v1/calendar/events/google:lunch",
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = t
        .call(
            reqwest::Method::DELETE,
            "/v1/calendar/events/google:lunch?confirm=true",
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.mock.lock().unwrap().writes.last().unwrap().0, "DELETE");
    let (s, e) = t
        .call(
            reqwest::Method::DELETE,
            "/v1/calendar/events/ics:x:y?confirm=true",
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("read_only_source"))
    );

    // The service token reads the calendar and files proposals, but cannot accept them or touch links.
    let svc = t.svc.clone();
    assert_eq!(
        t.call(
            reqwest::Method::GET,
            "/v1/calendar/events",
            None,
            Some(&svc)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.call(reqwest::Method::GET, "/v1/links", None, Some(&svc))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // The owner's own turn with Lucina: she creates the event directly with the service token,
    // in her own calendar (nothing reaches Google).
    let writes = t.mock.lock().unwrap().writes.len();
    let (s, c) = t
        .call(reqwest::Method::POST, "/v1/calendar/events", Some(json!({"title": "Claude Max 5x si sblocca", "start": "2026-10-30T23:00:00+01:00"})), Some(&svc))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let own_id = c["event"]["id"].as_str().unwrap().to_owned();
    assert!(own_id.starts_with("lucina:ev_"), "{c}");
    assert_eq!(
        (c["event"]["addedBy"].as_str(), c["event"]["editable"].as_bool()),
        (Some("Lucina"), Some(true))
    );
    assert_eq!(t.mock.lock().unwrap().writes.len(), writes, "nothing reached Google");
    assert_eq!(
        t.get("/v1/calendar/proposals").await.1["proposals"],
        json!([])
    );
    // Undo: the service token deletes it again.
    assert_eq!(
        t.call(reqwest::Method::DELETE, &format!("/v1/calendar/events/{own_id}?confirm=true"), None, Some(&svc))
            .await
            .0,
        StatusCode::OK
    );
    // An agent (MCP caller) proposes instead.
    let (s, p) = t
        .call(reqwest::Method::POST, "/v1/calendar/proposals", Some(json!({"title": "Padel", "start": "2026-11-07T10:00:00+01:00", "from": "mcp:claude-code"})), Some(&svc))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let pid = p["id"].as_str().unwrap().to_owned();
    let (_, links) = t.get("/v1/links").await;
    assert_eq!(links["proposals"][0]["from"], "mcp:claude-code");
    let accept = format!("/v1/calendar/proposals/{pid}/accept");
    assert_eq!(
        t.call(
            reqwest::Method::POST,
            &accept,
            Some(json!({"confirm": true})),
            Some(&svc)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.post(&accept, json!({})).await.0,
        StatusCode::PRECONDITION_REQUIRED
    );
    let writes = t.mock.lock().unwrap().writes.len();
    let (s, r) = t.post(&accept, json!({"confirm": true})).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    // Accepted into Lucina's own calendar, crediting who proposed it.
    assert!(r["event"]["id"].as_str().unwrap().starts_with("lucina:"), "{r}");
    assert_eq!(r["event"]["addedBy"], "mcp:claude-code");
    assert_eq!(t.mock.lock().unwrap().writes.len(), writes, "nothing reached Google");
    assert_eq!(
        t.get("/v1/calendar/proposals").await.1["proposals"],
        json!([])
    );

    // Free slots.
    let (s, f) = t
        .get("/v1/calendar/free?from=2027-01-04&to=2027-01-04&minutes=90&dayStart=9&dayEnd=12")
        .await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!(
        f["slots"][0],
        json!({"start": "2027-01-04T08:00:00.000Z", "end": "2027-01-04T11:00:00.000Z"})
    );

    // Revocation: refresh fails with invalid_grant → reconnect; cached events stay readable.
    {
        let mut g = t.mock.lock().unwrap();
        g.revoked = true;
        g.expire_once = true;
    }
    let (_, r) = t.post("/v1/calendar/sync", json!({})).await;
    assert_eq!(r["google"]["error"], "reconnect_needed", "{r}");
    let (_, links) = t.get("/v1/links").await;
    let gc = T::card(&links, "google-calendar");
    assert_eq!(gc["status"], "reconnect");
    assert_eq!(gc["actions"], json!(["reconnect", "disconnect"]));
    let (s, e) = t
        .post(
            "/v1/calendar/events",
            json!({"title": "x", "start": "2026-11-06T15:00:00+01:00", "target": "google"}),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("google_reconnect_needed"))
    );
    // A Google write by Lucina fails the same way, so she files a proposal with the reason.
    let writes = t.mock.lock().unwrap().writes.len();
    let (s, e) = t
        .call(reqwest::Method::POST, "/v1/calendar/events", Some(json!({"title": "Dentist", "start": "2026-11-06T15:00:00+01:00", "target": "google"})), Some(&svc))
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("google_reconnect_needed"))
    );
    let (s, p) = t
        .call(reqwest::Method::POST, "/v1/calendar/proposals", Some(json!({"title": "Dentist", "start": "2026-11-06T15:00:00+01:00", "from": "lucina:ios", "reason": "Google Calendar needs reconnecting"})), Some(&svc))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(t.mock.lock().unwrap().writes.len(), writes, "nothing reached Google");
    let (_, links) = t.get("/v1/links").await;
    assert_eq!(
        (links["proposals"][0]["from"].as_str(), links["proposals"][0]["reason"].as_str()),
        (Some("lucina:ios"), Some("Google Calendar needs reconnecting"))
    );
    let (s, _) = t
        .call(reqwest::Method::POST, "/v1/calendar/proposals", Some(json!({"title": "x", "start": "2026-11-06T15:00:00+01:00", "reason": "a\u{7}b"})), Some(&svc))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let pid = p["id"].as_str().unwrap().to_owned();
    t.call(reqwest::Method::DELETE, &format!("/v1/calendar/proposals/{pid}"), None, None)
        .await;
    assert!(!T::titles(
        &t.get("/v1/calendar/events?from=2026-11-02&to=2026-11-30")
            .await
            .1
    )
    .is_empty());

    // Reconnect, then disconnect: the grant is revoked at Google and the cache dropped.
    t.mock.lock().unwrap().revoked = false;
    t.connect().await;
    assert_eq!(
        T::card(&t.get("/v1/links").await.1, "google-calendar")["status"],
        "connected"
    );
    let (s, d) = t
        .call(reqwest::Method::DELETE, "/v1/links/google", None, None)
        .await;
    assert_eq!(
        (s, d["revokedAtGoogle"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert_eq!(t.mock.lock().unwrap().revoke_calls, vec!["rt1".to_string()]);
    assert_eq!(
        T::card(&t.get("/v1/links").await.1, "google-calendar")["status"],
        "disconnected"
    );
    // Google's events are gone; Lucina's own calendar (the accepted proposal) stays.
    assert_eq!(
        T::titles(
            &t.get("/v1/calendar/events?from=2026-11-02&to=2026-11-30")
                .await
                .1
        ),
        ["Padel"]
    );
}

#[tokio::test]
async fn ics_iphone_custom_links() {
    let t = T::new().await;

    // ICS: validated, fetched once, sealed; then conditional GETs.
    let url = format!("{}/feed.ics", t.ics_base);
    let (s, r) = t
        .post("/v1/links/ics", json!({"label": "Serie A", "url": url}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(
        (r["eventCount"].as_u64(), r["feedName"].as_str()),
        (Some(1), Some("Calcio"))
    );
    let id = r["id"].as_str().unwrap().to_owned();
    assert!(!std::fs::read_to_string(t.data.join("links.json"))
        .unwrap()
        .contains("feed.ics"));
    let (_, r) = t.post("/v1/calendar/sync", json!({})).await;
    assert_eq!(r["ics"][0]["changed"], false, "{r}");
    assert_eq!(
        t.ics_hits.lock().unwrap().last().unwrap().as_deref(),
        Some("\"v1\"")
    );
    let bad = t
        .post(
            "/v1/links/ics",
            json!({"url": format!("{}/missing.ics", t.ics_base)}),
        )
        .await;
    assert_eq!(
        (bad.0, bad.1["error"]["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("ics_unavailable"))
    );
    assert_eq!(
        t.post("/v1/links/ics", json!({"url": "ftp://x/y"})).await.0,
        StatusCode::BAD_REQUEST
    );

    // iPhone snapshot (Lucina is not configured here: the forward fails, the calendar keeps the events).
    let (s, _) = t
        .post("/v1/device/state", json!({"events": [
            {"id": "EK1", "title": "Padel", "start": "2026-11-05T17:00:00Z", "end": "2026-11-05T18:00:00Z", "calendar": "Casa"},
            {"id": "EK2", "title": "Birthday", "start": "2026-11-05", "allDay": true},
            // the next occurrence of a recurring EventKit event: same identifier
            {"id": "EK1", "title": "Padel", "start": "2026-11-12T17:00:00Z", "end": "2026-11-12T18:00:00Z"},
        ]}))
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (_, ev) = t
        .get("/v1/calendar/events?from=2026-11-05&to=2026-11-05")
        .await;
    assert_eq!(T::titles(&ev), ["Birthday", "Padel", "Match"]);
    let sources: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["iphone", "iphone", "ics"]);
    assert_eq!(ev["events"][2]["calendar"], "Serie A");
    let (_, wk) = t
        .get("/v1/calendar/events?from=2026-11-05&to=2026-11-12&sources=iphone")
        .await;
    assert_eq!(T::titles(&wk), ["Birthday", "Padel", "Padel"]);
    let (_, only) = t
        .get("/v1/calendar/events?from=2026-11-05&to=2026-11-05&sources=ics")
        .await;
    assert_eq!(T::titles(&only), ["Match"]);
    let (_, links) = t.get("/v1/links").await;
    let ip = T::card(&links, "iphone-calendar");
    assert_eq!(
        (
            ip["status"].as_str(),
            ip["account"].as_str(),
            ip["count"].as_u64()
        ),
        (Some("connected"), Some("test iPhone"), Some(3))
    );
    let ics = T::card(&links, &format!("ics:{id}"));
    assert_eq!(
        (ics["status"].as_str(), ics["account"].as_str()),
        (Some("connected"), Some("127.0.0.1"))
    );
    assert!(!links.to_string().contains("feed.ics"));
    assert_eq!(
        t.call(
            reqwest::Method::DELETE,
            &format!("/v1/links/ics/{id}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        T::titles(
            &t.get("/v1/calendar/events?from=2026-11-05&to=2026-11-05")
                .await
                .1
        ),
        ["Birthday", "Padel"]
    );

    // Range validation.
    assert_eq!(
        t.get("/v1/calendar/events?from=2026-11-05&to=2026-11-01")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.get("/v1/calendar/events?days=900").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.get("/v1/calendar/events?from=tomorrow").await.0,
        StatusCode::BAD_REQUEST
    );

    // Custom links: bookmarks show their URL; MCP servers are sealed and https only.
    let (s, b) = t.post("/v1/links/custom", json!({"label": "Bank", "url": "https://bank.example/login", "icon": "🏦", "tags": ["Money", "money"]})).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["tags"], json!(["money"]));
    let (s, m) = t.post("/v1/links/custom", json!({"kind": "mcp", "label": "Notion MCP", "url": "https://mcp.notion.example/sse?key=SECRETKEY"})).await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert!(m["url"].is_null());
    assert_eq!(
        t.post(
            "/v1/links/custom",
            json!({"kind": "mcp", "label": "x", "url": "http://mcp.example"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.post(
            "/v1/links/custom",
            json!({"label": "x", "url": "javascript:alert(1)"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let raw = std::fs::read_to_string(t.data.join("links.json")).unwrap();
    assert!(raw.contains("bank.example/login") && !raw.contains("SECRETKEY"));
    let (_, links) = t.get("/v1/links").await;
    assert!(!links.to_string().contains("SECRETKEY"));
    let bid = b["id"].as_str().unwrap();
    let card = T::card(&links, &format!("custom:{bid}"));
    assert_eq!(
        (card["kind"].as_str(), card["url"].as_str()),
        (Some("bookmark"), Some("https://bank.example/login"))
    );
    let (s, u) = t
        .call(
            reqwest::Method::PATCH,
            &format!("/v1/links/custom/{bid}"),
            Some(json!({"label": "My bank"})),
            None,
        )
        .await;
    assert_eq!((s, u["label"].as_str()), (StatusCode::OK, Some("My bank")));
    assert_eq!(
        t.call(
            reqwest::Method::DELETE,
            &format!("/v1/links/custom/{bid}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        t.call(
            reqwest::Method::DELETE,
            &format!("/v1/links/custom/{bid}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // Google writes without a connection.
    let (s, e) = t
        .post(
            "/v1/calendar/events",
            json!({"title": "x", "start": "2026-11-06T15:00:00+01:00", "target": "google"}),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("google_not_connected"))
    );
}

#[tokio::test]
async fn lucina_own_calendar() {
    let t = T::new().await;
    // No Google connection is needed: the owner's events live in Lucina's own calendar.
    let (s, c) = t
        .post(
            "/v1/calendar/events",
            json!({"title": "Dentist", "start": "2026-11-06T09:30:00+01:00", "location": "Aarhus C",
                   "description": "Bring the card", "conferenceUrl": "https://meet.google.com/abc-defg-hij"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let ev = &c["event"];
    let id = ev["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("lucina:ev_"), "{c}");
    assert_eq!(
        (ev["source"].as_str(), ev["calendar"].as_str(), ev["editable"].as_bool()),
        (Some("lucina"), Some("Lucina"), Some(true))
    );
    assert_eq!(ev["end"], "2026-11-06T09:30:00.000Z", "one hour by default");
    assert_eq!(ev["conferenceUrl"], "https://meet.google.com/abc-defg-hij");
    assert_eq!(ev["addedBy"], "test iPhone");
    let file = t.data.join("calendar-events.json");
    assert!(std::fs::read_to_string(&file).unwrap().contains("Dentist"));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let (_, list) = t
        .get("/v1/calendar/events?from=2026-11-06&to=2026-11-06")
        .await;
    assert_eq!(T::titles(&list), ["Dentist"]);
    assert_eq!(list["sources"]["lucina"]["count"], 1);
    let (_, only) = t
        .get("/v1/calendar/events?from=2026-11-06&to=2026-11-06&sources=iphone")
        .await;
    assert!(T::titles(&only).is_empty());

    // Edits need confirm; moving the start keeps the duration; "" clears a field.
    let path = format!("/v1/calendar/events/{id}");
    let (s, e) = t
        .call(reqwest::Method::PATCH, &path, Some(json!({"start": "2026-11-06T11:00:00+01:00"})), None)
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (StatusCode::PRECONDITION_REQUIRED, Some("confirmation_required"))
    );
    let (s, u) = t
        .call(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"confirm": true, "start": "2026-11-06T11:00:00+01:00", "location": "", "title": "Dentist (moved)"})),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{u}");
    assert_eq!(
        (u["event"]["start"].as_str(), u["event"]["end"].as_str()),
        (Some("2026-11-06T10:00:00.000Z"), Some("2026-11-06T11:00:00.000Z"))
    );
    assert_eq!(u["event"]["title"], "Dentist (moved)");
    assert!(u["event"].get("location").is_none(), "{u}");
    assert_eq!(u["event"]["description"], "Bring the card");
    assert_eq!(u["event"]["addedBy"], "test iPhone", "edits keep who added it");
    // Resize: a new end; the video link can be removed alone.
    let (s, u) = t
        .call(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"confirm": true, "start": "2026-11-06T11:00:00+01:00", "end": "2026-11-06T12:30:00+01:00"})),
            None,
        )
        .await;
    assert_eq!((s, u["event"]["end"].as_str()), (StatusCode::OK, Some("2026-11-06T11:30:00.000Z")));
    let (s, u) = t
        .call(reqwest::Method::PATCH, &path, Some(json!({"confirm": true, "conferenceUrl": null})), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{u}");
    assert!(u["event"].get("conferenceUrl").is_none(), "{u}");
    // All day.
    let (s, u) = t
        .call(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"confirm": true, "start": "2026-11-07", "allDay": true})),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{u}");
    assert_eq!(
        (u["event"]["allDay"].as_bool(), u["event"]["start"].as_str(), u["event"]["end"].as_str()),
        (Some(true), Some("2026-11-07"), Some("2026-11-08"))
    );
    // Bad input.
    for body in [
        json!({"confirm": true}),
        json!({"confirm": true, "conferenceUrl": "javascript:alert(1)"}),
        json!({"confirm": true, "title": ""}),
    ] {
        let (s, e) = t.call(reqwest::Method::PATCH, &path, Some(body.clone()), None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body} {e}");
    }
    let (s, _) = t
        .call(reqwest::Method::PATCH, "/v1/calendar/events/lucina:ev_missing", Some(json!({"confirm": true, "title": "x"})), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Delete needs confirm, then it is gone (and gone again is fine).
    let (s, _) = t.call(reqwest::Method::DELETE, &path, None, None).await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    for _ in 0..2 {
        let (s, d) = t
            .call(reqwest::Method::DELETE, &format!("{path}?confirm=true"), None, None)
            .await;
        assert_eq!((s, d["deleted"].as_bool()), (StatusCode::OK, Some(true)));
    }
    assert!(T::titles(
        &t.get("/v1/calendar/events?from=2026-11-01&to=2026-11-30")
            .await
            .1
    )
    .is_empty());
    assert!(!std::fs::read_to_string(&file).unwrap().contains("Dentist"));

    // Unknown targets are refused.
    let (s, _) = t
        .post("/v1/calendar/events", json!({"title": "x", "start": "2026-11-06", "target": "outlook"}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
