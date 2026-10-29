//! `/api/secretary/agentd/*` → lucina-agentd's socket: paths, methods, bodies, status
//! codes and the origin pass through; streams stream; a missing or forged origin and
//! a stopped daemon are refused.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use futures_util::StreamExt;
use lucina_server::agentd::forward_to;
use serde_json::{json, Value};

/// A stand-in daemon that echoes what it received, plus a two-event SSE stream.
async fn fake_daemon(dir: &Path) -> PathBuf {
    let socket = dir.join("agentd.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let app = Router::new()
        .route(
            "/stream",
            get(|headers: HeaderMap| async move {
                let last = headers
                    .get("last-event-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let events = futures_util::stream::iter(vec![
                    Ok::<_, std::convert::Infallible>(
                        Event::default()
                            .id("7")
                            .data(format!("{{\"after\":\"{last}\"}}")),
                    ),
                    Ok(Event::default().id("8").data("{\"type\":\"agent_state\"}")),
                ]);
                Sse::new(events.chain(futures_util::stream::pending()))
            }),
        )
        .route(
            "/{*rest}",
            any(|req: Request| async move {
                let (parts, body) = req.into_parts();
                let body = to_bytes(body, 1 << 20).await.unwrap();
                let status = if parts.uri.path() == "/agents/missing" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::CREATED
                };
                let header = |n: &str| {
                    parts
                        .headers
                        .get(n)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                (
                    status,
                    Json(json!({
                        "method": parts.method.as_str(),
                        "target": parts.uri.to_string(),
                        "origin": header("x-lucina-origin"),
                        "authorization": header("authorization"),
                        "cookie": header("cookie"),
                        "body": String::from_utf8_lossy(&body),
                    })),
                )
                    .into_response()
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    socket
}

fn headers(origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("content-type", HeaderValue::from_static("application/json"));
    h.insert(
        "authorization",
        HeaderValue::from_static("Bearer lucina-secretary-token"),
    );
    h.insert("cookie", HeaderValue::from_static("sec_session=x"));
    if let Some(o) = origin {
        h.insert("x-lucina-origin", HeaderValue::from_str(o).unwrap());
    }
    h
}

async fn call(
    socket: &Path,
    method: Method,
    uri: &str,
    origin: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let uri: Uri = uri.parse().unwrap();
    let resp = forward_to(
        socket,
        &method,
        &uri,
        &headers(origin),
        Body::from(body.to_string()),
    )
    .await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn requests_pass_through_with_the_origin_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let socket = fake_daemon(dir.path()).await;
    let (status, v) = call(
        &socket,
        Method::POST,
        "/api/secretary/agentd/agents/abc/send?x=1",
        Some("device:dev_1"),
        r#"{"prompt":"hi"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v["method"], "POST");
    assert_eq!(v["target"], "/agents/abc/send?x=1");
    assert_eq!(v["origin"], "device:dev_1");
    assert_eq!(v["body"], r#"{"prompt":"hi"}"#);
    assert_eq!(
        v["authorization"],
        Value::Null,
        "the Secretary bearer stays here"
    );
    assert_eq!(v["cookie"], Value::Null);
    let (status, v) = call(
        &socket,
        Method::PATCH,
        "/api/secretary/agentd/agents/abc",
        Some("mcp:hermes"),
        "{}",
    )
    .await;
    assert_eq!(
        (status, v["method"].clone(), v["origin"].clone()),
        (StatusCode::CREATED, json!("PATCH"), json!("mcp:hermes"))
    );
    let (status, _) = call(
        &socket,
        Method::GET,
        "/api/secretary/agentd/agents/missing",
        Some("service:infisical-broker"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "status codes pass through");
}

#[tokio::test]
async fn missing_or_forged_origins_and_bad_paths_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let socket = fake_daemon(dir.path()).await;
    for origin in [
        None,
        Some("local"),
        Some(""),
        Some("device:"),
        Some("device:a b"),
        Some("owner:me"),
    ] {
        let (status, v) = call(
            &socket,
            Method::GET,
            "/api/secretary/agentd/agents",
            origin,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{origin:?}: {v}");
    }
    for path in [
        "/api/secretary/agentd",
        "/api/secretary/agentd/../etc",
        "/api/secretary/agentd/a//b",
    ] {
        let (status, _) = call(&socket, Method::GET, path, Some("device:d"), "").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn streams_stream_and_carry_last_event_id() {
    let dir = tempfile::tempdir().unwrap();
    let socket = fake_daemon(dir.path()).await;
    let mut h = headers(Some("device:dev_1"));
    h.insert("last-event-id", HeaderValue::from_static("6"));
    let uri: Uri = "/api/secretary/agentd/stream".parse().unwrap();
    let resp = forward_to(&socket, &Method::GET, &uri, &h, Body::empty()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    assert_eq!(resp.headers()["cache-control"], "no-store");
    assert_eq!(resp.headers()["x-accel-buffering"], "no");
    // The stream never ends: read what arrives within a moment.
    let mut body = resp.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !text.contains("id: 8") {
        match tokio::time::timeout_at(deadline, body.next()).await {
            Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }
    assert!(text.contains("id: 7\ndata: {\"after\":\"6\"}"), "{text}");
    assert!(
        text.contains("id: 8\ndata: {\"type\":\"agent_state\"}"),
        "{text}"
    );
}

#[tokio::test]
async fn a_stopped_daemon_is_503() {
    let dir = tempfile::tempdir().unwrap();
    let (status, v) = call(
        &dir.path().join("nothing.sock"),
        Method::GET,
        "/api/secretary/agentd/agents",
        Some("device:d"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"]["code"], "agentd_unavailable", "{v}");
}
