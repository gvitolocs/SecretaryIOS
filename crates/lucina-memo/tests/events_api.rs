//! Memo ↔ calendar event over the HTTP API, against a fake secretary-server calendar:
//! create with `endedAt`/`location`, the candidates, the owner's choice (and "no event"),
//! and search by the event's name.
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt;
use lucina_memo::{api, search, App, Config};
use serde_json::{json, Value};
use tower::ServiceExt;

const SVC: &str = "svc-test-token-0123456789abcdef";
const CAL: &str = "cal-test-token-0123456789abcdef";

/// 27 Oct 2026, 09:00–11:00 Copenhagen: the lecture; 09:30–10:30 a meeting; an all-day event.
fn calendar() -> Value {
    json!({"events": [
        {"id": "google:lec_20261027T080000Z", "source": "google", "sourceId": "lec_20261027T080000Z",
         "calendar": "DTU skema", "title": "Algorithms – Lecture 5",
         "start": "2026-10-27T08:00:00.000Z", "end": "2026-10-27T10:00:00.000Z", "allDay": false,
         "startMs": 1793088000000i64, "endMs": 1793095200000i64, "location": "Building 101, room S01",
         "attendees": ["Prof. Hansen"], "recurring": true, "editable": false},
        {"id": "google:sync1", "source": "google", "sourceId": "sync1", "title": "Project sync",
         "start": "2026-10-27T08:30:00.000Z", "end": "2026-10-27T09:30:00.000Z", "allDay": false,
         "startMs": 1793089800000i64, "endMs": 1793093400000i64,
         "attendees": ["Anna", "Bo"], "recurring": false, "editable": true},
        {"id": "ics:h:x", "source": "ics", "sourceId": "x", "title": "Holiday",
         "start": "2026-10-27", "end": "2026-10-28", "allDay": true,
         "startMs": 1793055600000i64, "endMs": 1793142000000i64, "recurring": false, "editable": false}
    ]})
}

async fn fake_calendar() -> String {
    let app = Router::new().route(
        "/v1/calendar/events",
        get(|headers: axum::http::HeaderMap| async move {
            let ok = headers.get("authorization").and_then(|v| v.to_str().ok())
                == Some(&format!("Bearer {CAL}"));
            if ok {
                Ok(Json(calendar()))
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn app(dir: &std::path::Path, calendar_url: &str) -> Arc<App> {
    let token = dir.join("svc.token");
    std::fs::write(&token, CAL).unwrap();
    let data = dir.join("data");
    let (data, token, url) = (
        data.display().to_string(),
        token.display().to_string(),
        calendar_url.to_owned(),
    );
    let cfg = Config::from_lookup(|k| match k {
        "LUCINA_MEMO_DIR" => Some(data.clone()),
        "LUCINA_SECRETARY_SERVER_URL" => Some(url.clone()),
        "LUCINA_SECRETARY_SERVICE_TOKEN_FILE" => Some(token.clone()),
        "LUCINA_MEMO_OBSIDIAN_VAULT" => Some("off".into()),
        "LUCINA_MEMO_TIMEZONE" => Some("Europe/Copenhagen".into()),
        _ => None,
    });
    App::new(
        cfg,
        Arc::new(lucina_llm::ScriptedModel::new()),
        "test".into(),
    )
    .unwrap()
    .with_tokens(None, Some(SVC))
}

async fn call(
    app: &Arc<App>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {SVC}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = api::router(app.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn create_body(client: &str) -> Value {
    // 09:20–09:50 local: inside the lecture, its last 20 minutes in the sync.
    json!({"clientId": client, "recordedAt": "2026-10-27T08:20:00Z", "endedAt": "2026-10-27T08:50:00Z",
           "timezone": "Europe/Copenhagen", "durationSec": 1800.0, "mime": "audio/aac", "sizeBytes": 1000,
           "source": "ios", "location": {"lat": 55.785, "lon": 12.521, "accuracyM": 20.0}})
}

#[tokio::test]
async fn links_lists_and_changes_the_event() {
    let dir = tempfile::tempdir().unwrap();
    let url = fake_calendar().await;
    let app = app(dir.path(), &url).await;

    // Bad endedAt / location are refused.
    let mut bad = create_body("c-bad");
    bad["endedAt"] = json!("2026-10-27T08:00:00Z");
    assert_eq!(
        call(&app, "POST", "/v1/memos", Some(bad)).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = create_body("c-bad2");
    bad["location"] = json!({"lat": 95.0, "lon": 12.0});
    assert_eq!(
        call(&app, "POST", "/v1/memos", Some(bad)).await.0,
        StatusCode::BAD_REQUEST
    );

    let (status, created) = call(&app, "POST", "/v1/memos", Some(create_body("c-1"))).await;
    assert!(status.is_success(), "{status} {created}");
    let id = created["memo"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["memo"]["endedAt"], "2026-10-27T08:50:00Z");

    // Candidates: the lecture first (30 of 30 minutes), the sync next, the all-day event last.
    let (status, c) = call(&app, "GET", &format!("/v1/memos/{id}/events"), None).await;
    assert_eq!(status, StatusCode::OK, "{c}");
    let ids: Vec<_> = c["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["event"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["google:lec_20261027T080000Z", "google:sync1", "ics:h:x"]
    );
    let lecture = &c["candidates"][0]["event"];
    assert_eq!(lecture["kind"], "lecture");
    assert_eq!(lecture["series"], "lec");
    assert_eq!(lecture["attendees"], json!(["Prof. Hansen"]));
    assert_eq!(c["candidates"][0]["overlapMs"], 1_800_000);
    assert_eq!(c["candidates"][1]["event"]["kind"], "meeting");

    // The owner picks the sync: linked by the owner, named after it.
    let (status, m) = call(
        &app,
        "PUT",
        &format!("/v1/memos/{id}/event"),
        Some(json!({"eventId": "google:sync1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["memo"]["event"]["id"], "google:sync1");
    assert_eq!(m["memo"]["eventSource"], "owner");
    assert_eq!(m["memo"]["title"], "Project sync · 27 Oct 2026");
    assert_eq!(m["memo"]["titleSource"], "event");

    // "No event": cleared, still the owner's choice (the worker will not link one again).
    let (status, m) = call(
        &app,
        "PUT",
        &format!("/v1/memos/{id}/event"),
        Some(json!({"eventId": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert!(m["memo"]["event"].is_null());
    assert_eq!(m["memo"]["eventSource"], "owner");
    assert_ne!(m["memo"]["titleSource"], "event");

    // An event that is not around the recording.
    let (status, _) = call(
        &app,
        "PUT",
        &format!("/v1/memos/{id}/event"),
        Some(json!({"eventId": "google:nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Search finds a memo by its event's name although nothing in it says "algorithms".
    let (_, m) = call(
        &app,
        "PUT",
        &format!("/v1/memos/{id}/event"),
        Some(json!({"eventId": "google:lec_20261027T080000Z"})),
    )
    .await;
    assert_eq!(m["memo"]["event"]["kind"], "lecture");
    app.store
        .update(&id, |m| {
            // Uploaded (search skips memos still uploading), and nothing in it says "algorithms".
            m.status = lucina_memo::model::Status::Queued;
            m.title = "Untitled".into();
        })
        .unwrap();
    let from = "2026-10-27T00:00:00+01:00".parse().unwrap();
    let to = "2026-10-28T00:00:00+01:00".parse().unwrap();
    let q = search::Query {
        q: Some("algorithms"),
        from: Some(from),
        to: Some(to),
        limit: 10,
    };
    let hits = search::search(&app.store, &q, |i| format!("/memos/{i}"));
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].event.as_ref().unwrap().title,
        "Algorithms – Lecture 5"
    );
    let q = search::Query {
        q: Some("algorithms"),
        from: Some(to),
        to: None,
        limit: 10,
    };
    assert!(search::search(&app.store, &q, |i| format!("/memos/{i}")).is_empty());
}

#[tokio::test]
async fn calendar_down_is_a_bad_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), "http://127.0.0.1:9").await;
    let (_, created) = call(&app, "POST", "/v1/memos", Some(create_body("c-2"))).await;
    let id = created["memo"]["id"].as_str().unwrap().to_owned();
    let (status, _) = call(&app, "GET", &format!("/v1/memos/{id}/events"), None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn pending_memo_relinks_quietly_and_cache_survives_outage() {
    use lucina_memo::{calendar, events, model::Status, worker};
    let state = Arc::new(std::sync::Mutex::new(json!({"events": []})));
    let shared = state.clone();
    let router = Router::new().route(
        "/v1/calendar/events",
        get(move || {
            let shared = shared.clone();
            async move { Json(shared.lock().unwrap().clone()) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path(), &url).await;
    let (_, created) = call(&app, "POST", "/v1/memos", Some(create_body("pending"))).await;
    let id = created["memo"]["id"].as_str().unwrap();
    let notified = chrono::Utc::now();
    app.store
        .update(id, |m| {
            m.status = Status::Queued;
            m.ext = "m4a".into();
        })
        .unwrap();
    // Resume an already summarized/rendered generic memo through the real pipeline.
    let transcript = json!({"language":"en","engine":"test","durationSec":1800,"segments":[],"createdAt":notified});
    let summary = json!({"title":"Generic", "overview":"Unlinked memo", "keyPoints":[], "decisions":[], "actionItems":[], "tags":[], "language":"en", "model":"test", "createdAt":notified});
    std::fs::write(app.store.transcript_path(id), transcript.to_string()).unwrap();
    std::fs::write(app.store.summary_path(id), summary.to_string()).unwrap();
    std::fs::write(app.store.pdf_path(id), b"pdf").unwrap();
    app.store
        .update(id, |m| {
            m.steps.transcribed_at = Some(notified);
            m.steps.summarized_at = Some(notified);
            m.steps.rendered_at = Some(notified);
            m.steps.notified_at = Some(notified);
        })
        .unwrap();
    worker::process(&app, id).await.unwrap();
    assert_eq!(app.store.get(id).unwrap().status, Status::Done);
    assert!(app.store.get(id).unwrap().event.is_none());
    assert!(!worker::relink_one(&app).await);
    let checked = app.store.get(id).unwrap();
    assert_eq!(checked.link_checked_gen, Some(0));
    assert!(!worker::relink_one(&app).await);
    assert_eq!(checked.updated_at, app.store.get(id).unwrap().updated_at);

    let (_, owner) = call(&app, "POST", "/v1/memos", Some(create_body("owner-none"))).await;
    let owner_id = owner["memo"]["id"].as_str().unwrap();
    call(
        &app,
        "PUT",
        &format!("/v1/memos/{owner_id}/event"),
        Some(json!({"eventId":null})),
    )
    .await;
    app.store
        .update(owner_id, |m| m.status = Status::Queued)
        .unwrap();
    *state.lock().unwrap() = calendar();
    calendar::refresh(&app).await.unwrap();
    assert!(worker::relink_one(&app).await);
    let linked = app.store.get(id).unwrap();
    assert_eq!(linked.event_source.as_deref(), Some("auto"));
    assert_eq!(
        linked.event.as_ref().unwrap().kind,
        lucina_memo::model::EventKind::Lecture
    );
    assert_eq!(linked.title, "Algorithms – Lecture 5 · 27 Oct 2026");
    assert_eq!(linked.revision, checked.revision + 1);
    assert_eq!(linked.status, Status::Queued);
    assert_eq!(linked.steps.notified_at, Some(notified));
    assert!(linked.steps.summarized_at.is_none());
    assert!(!worker::relink_one(&app).await);
    assert!(app.store.get(owner_id).unwrap().event.is_none());
    server.abort();
    let _ = server.await;
    let (status, response) = call(&app, "GET", &format!("/v1/memos/{id}/events"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["stale"], true);
    assert_eq!(response["candidates"].as_array().unwrap().len(), 3);
    assert!(events::candidates(&app, &linked, 30).await.unwrap().stale);
}
