//! `photo_search`, `photo_get` (scope `photos`): held by nobody who was not given it by name, against a
//! fake of the media library behind Caddy.

mod common;

use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use common::*;
use serde_json::json;

const JPEG: &[u8] = b"\xff\xd8fake-jpeg";

fn device(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(&*format!("Bearer {DEVICE_TOKEN}"))
}

async fn library() -> String {
    let app = Router::new()
        .route(
            "/v1/media/items",
            get(|h: HeaderMap| async move {
                if !device(&h) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(json!({"items": [{"id": "itm_ph1", "kind": "photo", "title": "IMG_1.HEIC",
                    "taken_at": "2026-11-02T10:00:00Z", "place": {"name": "Aarhus", "lat": 56.1, "lon": 10.2},
                    "urls": {"original": "/secret"}}]}))
                .into_response()
            }),
        )
        .route(
            "/v1/media/items/{id}",
            get(|Path(id): Path<String>| async move {
                let kind = if id == "itm_vid" { "video" } else { "photo" };
                Json(json!({"id": id, "kind": kind, "title": "IMG_1.HEIC"}))
            }),
        )
        .route(
            "/v1/media/items/{id}/thumb",
            get(|| async { ([(header::CONTENT_TYPE, "image/jpeg")], JPEG) }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

#[tokio::test]
async fn photos_need_their_own_scope_and_come_back_as_a_picture() {
    let url = library().await;
    let h = start_with(|cfg, _| cfg.media_url = Some(url)).await;

    // Nothing else implies it, whatever else a connector holds.
    let others = h.issue("mistral", "read,chat,write,computer,agents,notebooks");
    assert!(!h.tool_names(&others).await.iter().any(|n| n.starts_with("photo_")));
    let (err, text) = h.call(&others, "photo_get", json!({"item_id": "itm_ph1"})).await;
    assert!(err && text.contains("`photos` scope"), "{text}");

    let agent = h.issue("zcode", "photos");
    let names = h.tool_names(&agent).await;
    assert!(names.iter().any(|n| n == "photo_search") && names.iter().any(|n| n == "photo_get"), "{names:?}");

    let (err, text) = h.call(&agent, "photo_search", json!({"query": "aarhus"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("itm_ph1") && text.contains("Aarhus"), "{text}");
    assert!(!text.contains("56.1") && !text.contains("/secret"), "no coordinates or links: {text}");

    let r = h
        .rpc(&agent, "tools/call", json!({"name": "photo_get", "arguments": {"item_id": "itm_ph1"}}))
        .await;
    let blocks = r["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "image", "{r}");
    assert_eq!(blocks[0]["mimeType"], "image/jpeg");
    let want = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, JPEG);
    assert_eq!(blocks[0]["data"], want.as_str());

    for bad in ["../etc", "itm_a/b", "x"] {
        let (err, text) = h.call(&agent, "photo_get", json!({"item_id": bad})).await;
        assert!(err && text.contains("item_id"), "{bad}: {text}");
    }
    let (err, text) = h.call(&agent, "photo_get", json!({"item_id": "itm_vid"})).await;
    assert!(err && text.contains("not a photo"), "{text}");
}
