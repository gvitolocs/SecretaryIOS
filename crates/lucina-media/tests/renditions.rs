//! `GET items/{id}/renditions` (the Gallery's Server copy / Device copy view), `original?version=N` and
//! `GET items/{id}/device-preview`, through the router with the real upload flow. Synthetic pictures only.

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
    let app = http::router(lib.clone(), None);
    (tmp, lib, app)
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    raw: Option<Vec<u8>>,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    let body = match (body, raw) {
        (Some(v), _) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        (None, Some(bytes)) => {
            b = b.header("content-length", bytes.len().to_string());
            Body::from(bytes)
        }
        _ => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = res.status();
    (status, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn json_of(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (s, b) = call(app, method, uri, body, None).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn picture(w: u32, h: u32, shade: u8, fmt: image::ImageFormat) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([shade, 100, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, fmt).unwrap();
    out.into_inner()
}

/// One upload, the way the phone does it: create, send, optionally the phone's own rendering, finish.
async fn upload(app: &Router, name: &str, data: &[u8], preview: Option<Vec<u8>>, meta: Value) -> Value {
    let hash = blake3::hash(data).to_hex().to_string();
    let (s, created) = json_of(
        app,
        "POST",
        "/v1/media/uploads",
        Some(json!({"filename": name, "size": data.len(), "hash": hash, "meta": meta})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{created}");
    let id = created["upload_id"].as_str().unwrap().to_string();
    let (s, _) = call(app, "PUT", &format!("/v1/media/uploads/{id}?offset=0"), None, Some(data.to_vec())).await;
    assert_eq!(s, StatusCode::OK);
    if let Some(p) = preview {
        let (s, _) = call(app, "PUT", &format!("/v1/media/uploads/{id}/preview"), None, Some(p)).await;
        assert!(s.is_success(), "{s}");
    }
    let (s, done) = json_of(app, "POST", &format!("/v1/media/uploads/{id}/finish"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{done}");
    done
}

async fn wait_thumbs(lib: &Library) {
    let h = tokio::spawn(ingest::thumb_worker(lib.clone()));
    for _ in 0..600 {
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

fn named<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["renditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no rendition {name} in {v}"))
}

#[tokio::test]
async fn an_item_lists_its_original_its_renditions_and_what_the_phone_reported() {
    let (tmp, lib, app) = setup();
    let (s, _) = json_of(
        &app,
        "PUT",
        "/v1/media/devices/ios-1",
        Some(json!({"name": "Test phone", "kind": "iphone"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let data = picture(120, 80, 40, image::ImageFormat::Png);
    let done = upload(
        &app,
        "IMG_0001.PNG",
        &data,
        None,
        json!({"device": "ios-1", "asset_id": "ASSET-1", "mime": "image/png", "uti": "public.png", "width": 120, "height": 80}),
    )
    .await;
    wait_thumbs(&lib).await;
    let id = done["item_id"].as_str().unwrap();

    let (s, v) = json_of(&app, "GET", &format!("/v1/media/items/{id}/renditions"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["item_id"], id);
    let o = &v["original"];
    assert_eq!(o["format"], "png");
    assert_eq!(o["mime"], "image/png");
    assert_eq!(o["bytes"], data.len());
    assert_eq!(o["blake3"], blake3::hash(&data).to_hex().to_string());
    assert_eq!((o["width"].as_i64(), o["height"].as_i64()), (Some(120), Some(80)));
    assert_eq!(o["version"], 1);
    assert_eq!(o["reachable"], true);
    assert!(matches!(o["stored"].as_str(), Some("store" | "spool")), "{o}");
    assert_eq!(o["url"], format!("/v1/media/items/{id}/original"));
    assert_eq!(v["versions"], json!([]));

    // The display renditions that exist, with their format, size in bytes and size in pixels.
    let grid = named(&v, "grid");
    assert_eq!((grid["format"].as_str(), grid["mime"].as_str()), (Some("jxl"), Some("image/jxl")));
    assert_eq!((grid["width"].as_i64(), grid["height"].as_i64()), (Some(256), Some(256)));
    assert!(grid["bytes"].as_u64().unwrap() > 0);
    let preview = named(&v, "preview");
    assert_eq!((preview["width"].as_i64(), preview["height"].as_i64()), (Some(120), Some(80)));
    let thumb = named(&v, "thumb");
    assert_eq!(thumb["format"], "jpeg");
    // Same shape as the picture (3:2), whatever the size.
    assert_eq!(thumb["width"].as_i64().unwrap() * 2, thumb["height"].as_i64().unwrap() * 3);
    let large = named(&v, "large");
    assert_eq!((large["width"].as_i64(), large["height"].as_i64()), (Some(120), Some(80)));
    // No poster for a photo, no rendering from the phone when it sent none.
    assert!(v["renditions"].as_array().unwrap().iter().all(|r| r["name"] != "poster" && r["name"] != "device_preview"));
    // Each rendition's URL answers.
    for r in v["renditions"].as_array().unwrap() {
        let (s, bytes) = call(&app, "GET", r["url"].as_str().unwrap(), None, None).await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(bytes.len() as u64, r["bytes"].as_u64().unwrap(), "{r}");
    }

    // What the phone reported at upload: its id, the registry's name, the asset, file name, size, hash.
    let d = &v["device"];
    assert_eq!(d["id"], "ios-1");
    assert_eq!(d["name"], "Test phone");
    assert_eq!(d["kind"], "iphone");
    assert_eq!(d["asset_id"], "ASSET-1");
    let latest = &d["latest"];
    assert_eq!(latest["filename"], "IMG_0001.PNG");
    assert_eq!(latest["mime"], "image/png");
    assert_eq!(latest["uti"], "public.png");
    assert_eq!(latest["bytes"], data.len());
    assert_eq!(latest["blake3"], blake3::hash(&data).to_hex().to_string());
    assert_eq!((latest["width"].as_i64(), latest["height"].as_i64()), (Some(120), Some(80)));
    assert_eq!(latest["matches_version"], 1, "the phone's hash is the server's copy");

    // No path of the host in the answer.
    let text = v.to_string();
    let root = tmp.path().to_string_lossy().to_string();
    assert!(!text.contains(&root) && !text.contains("/store/") && !text.contains("/thumbs/"), "{text}");
}

#[tokio::test]
async fn an_item_without_a_device_has_no_device_copy_and_unknown_items_are_404() {
    let (_tmp, lib, app) = setup();
    let data = picture(40, 30, 90, image::ImageFormat::Png);
    let done = upload(&app, "web.png", &data, None, json!({})).await;
    wait_thumbs(&lib).await;
    let id = done["item_id"].as_str().unwrap();
    let (s, v) = json_of(&app, "GET", &format!("/v1/media/items/{id}/renditions"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["device"].is_null(), "{v}");
    for uri in [
        "/v1/media/items/itm_nope/renditions",
        "/v1/media/items/..%2Fx/renditions",
        "/v1/media/items/itm_nope/device-preview",
    ] {
        let (s, _) = call(&app, "GET", uri, None, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn an_edited_photo_keeps_its_older_versions_and_serves_them() {
    let (_tmp, lib, app) = setup();
    let first = picture(64, 48, 10, image::ImageFormat::Png);
    let meta = json!({"device": "ios-2", "asset_id": "ASSET-2"});
    let done = upload(&app, "IMG_7.PNG", &first, None, meta.clone()).await;
    let id = done["item_id"].as_str().unwrap().to_string();
    let second = picture(64, 48, 200, image::ImageFormat::Png);
    let again = upload(&app, "IMG_7.PNG", &second, None, meta).await;
    assert_eq!(again["item_id"], id.as_str(), "same asset, new bytes: a new version");
    assert_eq!(again["version"], 2);
    wait_thumbs(&lib).await;

    let (_, v) = json_of(&app, "GET", &format!("/v1/media/items/{id}/renditions"), None).await;
    assert_eq!(v["original"]["version"], 2);
    assert_eq!(v["original"]["blake3"], blake3::hash(&second).to_hex().to_string());
    let versions = v["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1, "{v}");
    assert_eq!(versions[0]["version"], 1);
    assert_eq!(versions[0]["blake3"], blake3::hash(&first).to_hex().to_string());
    assert_eq!(versions[0]["bytes"], first.len());
    assert_eq!(versions[0]["format"], "png");
    assert_eq!(versions[0]["reachable"], true);
    // Both uploads are on record; each says which stored version it is.
    let uploads = v["device"]["uploads"].as_array().unwrap();
    assert_eq!(uploads.len(), 2);
    assert_eq!(uploads[0]["matches_version"], 1);
    assert_eq!(uploads[1]["matches_version"], 2);

    // The version URL gives the old bytes, the current URL the new ones, a missing version is 404.
    let (s, old) = call(&app, "GET", versions[0]["url"].as_str().unwrap(), None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(old, first);
    let (s, cur) = call(&app, "GET", &format!("/v1/media/items/{id}/original"), None, None).await;
    assert_eq!((s, cur), (StatusCode::OK, second.clone()));
    let (s, cur2) = call(&app, "GET", &format!("/v1/media/items/{id}/original?version=2"), None, None).await;
    assert_eq!((s, cur2), (StatusCode::OK, second));
    let (s, _) = call(&app, "GET", &format!("/v1/media/items/{id}/original?version=9"), None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app, "GET", &format!("/v1/media/items/{id}/original?version=0"), None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_heic_original_comes_with_the_rendering_the_phone_sent() {
    let (_tmp, lib, app) = setup();
    // The server cannot decode HEIC: the phone's JPEG is what every rendition is made from.
    let heic = b"not a real heic, only bytes".to_vec();
    let jpeg = picture(200, 100, 70, image::ImageFormat::Jpeg);
    let done = upload(
        &app,
        "IMG_9.HEIC",
        &heic,
        Some(jpeg.clone()),
        json!({"device": "ios-3", "asset_id": "ASSET-3", "uti": "public.heic", "width": 4032, "height": 3024}),
    )
    .await;
    wait_thumbs(&lib).await;
    let id = done["item_id"].as_str().unwrap();
    let (_, v) = json_of(&app, "GET", &format!("/v1/media/items/{id}/renditions"), None).await;
    assert_eq!(v["original"]["format"], "heic");
    assert_eq!(v["original"]["mime"], "image/heic");
    let dp = named(&v, "device_preview");
    assert_eq!(dp["format"], "jpeg");
    assert_eq!((dp["width"].as_i64(), dp["height"].as_i64()), (Some(200), Some(100)));
    assert_eq!(dp["bytes"], jpeg.len());
    let (s, bytes) = call(&app, "GET", &format!("/v1/media/items/{id}/device-preview"), None, None).await;
    assert_eq!((s, bytes), (StatusCode::OK, jpeg));
    assert_eq!(v["device"]["latest"]["uti"], "public.heic");
    assert_eq!(v["device"]["latest"]["width"], 4032);
}

#[tokio::test]
async fn a_trashed_item_still_lists_its_forms() {
    let (_tmp, lib, app) = setup();
    let data = picture(50, 50, 33, image::ImageFormat::Png);
    let done = upload(&app, "t.png", &data, None, json!({"device": "ios-4", "asset_id": "A4"})).await;
    wait_thumbs(&lib).await;
    let id = done["item_id"].as_str().unwrap();
    let (s, _) = call(&app, "DELETE", &format!("/v1/media/items/{id}"), None, None).await;
    assert!(s.is_success());
    let (s, v) = json_of(&app, "GET", &format!("/v1/media/items/{id}/renditions"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["original"]["bytes"], data.len());
}
