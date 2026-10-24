//! End-to-end through the HTTP router: upload (chunked, resumed, deduped, edited), list/search
//! by date and place, Range on the original, thumbnails, albums, trash, external memos, and the
//! upstream key.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::{http, store};
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn setup() -> (tempfile::TempDir, Library, Router) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None).merge(http::internal_router(lib.clone()));
    (tmp, lib, app)
}

async fn put_bytes(app: &Router, uri: &str, data: &[u8]) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let len = data.len().to_string();
    call(app, "PUT", uri, Some(Body::from(data.to_vec())), &[("content-length", &len)]).await
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Body>, headers: &[(&str, &str)]) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let mut b = Request::builder().method(method).uri(uri).header("x-media-upstream-key", KEY);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let res = app.clone().oneshot(b.body(body.unwrap_or_else(Body::empty)).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes, headers)
}

async fn json_call(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let (s, b, _) = call(app, method, uri, Some(Body::from(body.to_string())), &[("content-type", "application/json")]).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (s, b, _) = call(app, "GET", uri, None, &[]).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([shade, 100, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn upload(app: &Router, name: &str, data: &[u8], meta: Value) -> Value {
    let hash = blake3::hash(data).to_hex().to_string();
    let (s, created) =
        json_call(app, "POST", "/v1/media/uploads", json!({"filename": name, "size": data.len(), "hash": hash, "meta": meta})).await;
    assert_eq!(s, StatusCode::OK, "{created}");
    if created["duplicate"] == true {
        return created;
    }
    let id = created["upload_id"].as_str().unwrap().to_string();
    // Two chunks, with a retry of the first one in between.
    let mid = data.len() / 2;
    let (s, _, _) = put_bytes(app, &format!("/v1/media/uploads/{id}?offset=0"), &data[..mid]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, b, _) = put_bytes(app, &format!("/v1/media/uploads/{id}?offset=0"), &data[..mid]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["offset"], mid as u64);
    let (s, _, _) = put_bytes(app, &format!("/v1/media/uploads/{id}?offset={}", mid + 1), &[1u8]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _, _) = put_bytes(app, &format!("/v1/media/uploads/{id}?offset={mid}"), &data[mid..]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, done) = json_call(app, "POST", &format!("/v1/media/uploads/{id}/finish"), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{done}");
    // A retried finish answers with the same item.
    let (s, again) = json_call(app, "POST", &format!("/v1/media/uploads/{id}/finish"), json!({})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["item_id"], done["item_id"]);
    done
}

async fn wait_thumbs(lib: &Library) {
    // Run the worker for a moment.
    let h = tokio::spawn(ingest::thumb_worker(lib.clone()));
    for _ in 0..50 {
        let pending: i64 = lib
            .db
            .with(|c| c.query_row("SELECT COUNT(*) FROM items WHERE thumb_state = 'pending'", [], |r| r.get(0)))
            .unwrap();
        if pending == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    h.abort();
}

#[tokio::test]
async fn upstream_key_is_required() {
    let (_t, _lib, app) = setup();
    let res = app
        .clone()
        .oneshot(Request::builder().uri("/v1/media/items").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app
        .clone()
        .oneshot(Request::builder().uri("/v1/media/items").header("x-media-upstream-key", "wrong").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = app.clone().oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn phone_backup_flow() {
    let (_t, lib, app) = setup();
    let photo = png(1200, 800, 10);
    let meta = json!({"device": "iphone-test", "asset_id": "A1/L0/001", "taken_at": "2025-07-12T14:03:22+02:00",
                      "lat": 55.6761, "lon": 12.5683, "source": "phone-ios"});
    let done = upload(&app, "IMG_0001.PNG", &photo, meta.clone()).await;
    let item_id = done["item_id"].as_str().unwrap().to_string();
    assert_eq!(done["duplicate"], false);

    // Same bytes again: no upload needed.
    let dup = upload(&app, "IMG_0001.PNG", &photo, meta.clone()).await;
    assert_eq!(dup["duplicate"], true);
    assert_eq!(dup["item_id"], item_id.as_str());

    // Another photo, Rome, 2024.
    let other = upload(&app, "IMG_0002.PNG", &png(640, 480, 99),
        json!({"device": "iphone-test", "asset_id": "B2", "taken_at": "2024-03-01T10:00:00Z", "lat": 41.9028, "lon": 12.4964, "source": "phone-ios"})).await;
    let other_id = other["item_id"].as_str().unwrap().to_string();

    wait_thumbs(&lib).await;

    // Timeline: newest first, with thumbnails ready.
    let (_, page) = get_json(&app, "/v1/media/items").await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], item_id.as_str());
    assert_eq!(items[0]["thumb_state"], "ready", "{}", items[0]);
    assert_eq!(items[0]["place"]["country"], "DK");
    assert_eq!(page["total"], 2);

    // Date and place searches.
    let (_, p) = get_json(&app, "/v1/media/items?from=2025-06&to=2025-08").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    let (_, p) = get_json(&app, "/v1/media/items?near=55.68,12.57,20").await;
    assert_eq!(p["items"][0]["id"], item_id.as_str());
    let (_, p) = get_json(&app, "/v1/media/items?q=Denmark").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    let (_, p) = get_json(&app, "/v1/media/items?q=July%202025").await;
    assert_eq!(p["items"][0]["id"], item_id.as_str());
    let (_, p) = get_json(&app, "/v1/media/items?q=Italia").await;
    assert_eq!(p["items"][0]["id"], other_id.as_str());

    // Pagination.
    let (_, p1) = get_json(&app, "/v1/media/items?limit=1").await;
    let cursor = p1["next_cursor"].as_str().unwrap().to_string();
    let (_, p2) = get_json(&app, &format!("/v1/media/items?limit=1&cursor={}", urlencode(&cursor))).await;
    assert_eq!(p2["items"][0]["id"], other_id.as_str());
    assert!(p2["next_cursor"].is_null());

    // Original with Range, ETag, 304.
    let (s, body, h) = call(&app, "GET", &format!("/v1/media/items/{item_id}/original"), None, &[("range", "bytes=0-9")]).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, photo[..10]);
    assert_eq!(h["content-range"], format!("bytes 0-9/{}", photo.len()).as_str());
    let etag = h["etag"].to_str().unwrap().to_string();
    let (s, _, _) = call(&app, "GET", &format!("/v1/media/items/{item_id}/original"), None, &[("if-none-match", &etag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    let (s, thumb, _) = call(&app, "GET", &format!("/v1/media/items/{item_id}/thumb?size=256"), None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&thumb[..2], &[0xFF, 0xD8]);

    // Edited on the phone: same asset, new bytes → same item, version 2.
    let edited = upload(&app, "IMG_0001.PNG", &png(1200, 800, 200), meta.clone()).await;
    assert_eq!(edited["item_id"], item_id.as_str());
    assert_eq!(edited["version"], 2);

    // Albums and search by album title.
    let (s, album) = json_call(&app, "POST", "/v1/media/albums", json!({"title": "Copenhagen summer", "item_ids": [item_id]})).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(album["count"], 1);
    let (_, p) = get_json(&app, "/v1/media/items?album=copenhagen%20summer").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    assert!(p["albums"].as_array().unwrap().iter().any(|a| a == "Copenhagen summer"));
    let (_, p) = get_json(&app, "/v1/media/items?q=summer").await;
    assert_eq!(p["items"][0]["id"], item_id.as_str());
    let (s, r) = json_call(&app, "PUT", "/v1/media/albums/sys-lucina/items", json!({"item_ids": [other_id, "itm_missing"]})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["items"], 1);
    let (_, p) = get_json(&app, "/v1/media/items?album=sys-lucina").await;
    assert_eq!(p["items"][0]["id"], other_id.as_str());
    let (_, albums) = get_json(&app, "/v1/media/albums").await;
    let camera = albums["albums"].as_array().unwrap().iter().find(|a| a["system_key"] == "camera").unwrap();
    assert_eq!(camera["count"], 2);

    // Trash hides, restore brings back, nothing is removed from disk.
    let (s, _, _) = call(&app, "DELETE", &format!("/v1/media/items/{other_id}"), None, &[]).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, p) = get_json(&app, "/v1/media/items").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    let (_, p) = get_json(&app, "/v1/media/items?trashed=true").await;
    assert_eq!(p["items"][0]["id"], other_id.as_str());
    let (s, _) = json_call(&app, "POST", &format!("/v1/media/items/{other_id}/restore"), json!({})).await;
    assert_eq!(s, StatusCode::OK);

    // Device asset list for reinstall recovery.
    let (_, assets) = get_json(&app, "/v1/media/devices/iphone-test/assets").await;
    assert_eq!(assets["assets"].as_array().unwrap().len(), 2);

    let (_, sum) = get_json(&app, "/v1/media/summary").await;
    assert_eq!(sum["kinds"][0]["count"], 2);
}

#[tokio::test]
async fn bad_hash_is_refused() {
    let (_t, _lib, app) = setup();
    let data = png(10, 10, 1);
    let (_, created) = json_call(&app, "POST", "/v1/media/uploads",
        json!({"filename": "x.png", "size": data.len(), "hash": "0".repeat(64)})).await;
    let id = created["upload_id"].as_str().unwrap();
    let (s, _, _) = call(&app, "PUT", &format!("/v1/media/uploads/{id}"), Some(Body::from(data.clone())), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = json_call(&app, "POST", &format!("/v1/media/uploads/{id}/finish"), json!({})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, p) = get_json(&app, "/v1/media/items").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn external_voice_memo_is_indexed_in_place() {
    let (t, _lib, app) = setup();
    let memo = t.path().join("memo.m4a");
    std::fs::write(&memo, b"not really audio").unwrap();
    let body = json!({"kind": "audio", "title": "Groceries idea", "created_at": "2026-10-24T09:00:00Z",
                      "duration_ms": 4200, "mime": "audio/mp4", "path": memo, "links": {"pdf": "/v1/memos/m1/pdf"}});
    let (s, r) = json_call(&app, "PUT", "/internal/v1/external/voice-memo/m1", body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, r2) = json_call(&app, "PUT", "/internal/v1/external/voice-memo/m1", body).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["item_id"], r2["item_id"]);
    let (_, p) = get_json(&app, "/v1/media/items?album=sys-voice-memos").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    assert_eq!(p["items"][0]["links"]["pdf"], "/v1/memos/m1/pdf");
    let (_, p) = get_json(&app, "/v1/media/items?q=groceries").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    let id = r["item_id"].as_str().unwrap();
    let (s, b, _) = call(&app, "GET", &format!("/v1/media/items/{id}/original"), None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"not really audio");
    assert!(memo.exists());
    let (s, _, _) = call(&app, "DELETE", "/internal/v1/external/voice-memo/m1", None, &[]).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(memo.exists(), "unregistering must not delete the memo service's file");
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}
