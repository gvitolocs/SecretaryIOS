//! One gallery per device, all in the Lucina cloud: devices register, every upload names its device,
//! the Gallery lists one device's photos or all of them, and a Mac's photos are Gallery photos.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
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

async fn json_of(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let s = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn png(shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([shade, 90, 160]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn ingest(
    tmp: &tempfile::TempDir,
    lib: &Library,
    name: &str,
    device: Option<&str>,
    source: &str,
    shade: u8,
) {
    let src = tmp.path().join(name);
    std::fs::write(&src, png(shade)).unwrap();
    let client = ClientMeta {
        filename: Some(name.into()),
        device: device.map(Into::into),
        asset_id: device.map(|d| format!("{d}-{name}")),
        source: Some(source.into()),
        ..Default::default()
    };
    ingest::ingest_file(lib, &src, None, &client, None).unwrap();
}

fn titles(v: &Value) -> Vec<String> {
    let mut t: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

#[tokio::test]
async fn devices_register_and_list_what_they_hold() {
    let (tmp, lib, app) = setup();
    let (s, d) = json_of(
        &app,
        "PUT",
        "/v1/media/devices/ios-1",
        Some(json!({"name": "Giuseppe's iPhone", "kind": "iphone"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["name"], "Giuseppe's iPhone");
    let (s, _) = json_of(
        &app,
        "PUT",
        "/v1/media/devices/mac-1",
        Some(json!({"name": "MacBook Air", "kind": "mac"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    ingest(&tmp, &lib, "beach.png", Some("ios-1"), "phone-ios", 10);
    ingest(&tmp, &lib, "desk.png", Some("ios-1"), "phone-ios", 20);
    ingest(&tmp, &lib, "screenshot.png", Some("mac-1"), "mac", 30);
    // A rename keeps the device and its photos.
    let (_, d) = json_of(
        &app,
        "PUT",
        "/v1/media/devices/mac-1",
        Some(json!({"name": "Mac", "kind": "mac"})),
    )
    .await;
    assert_eq!(
        (d["name"].as_str(), d["photos"].as_i64()),
        (Some("Mac"), Some(1))
    );
    let (_, list) = json_of(&app, "GET", "/v1/media/devices", None).await;
    let by_id = |id: &str| {
        list["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_id("ios-1")["photos"], 2);
    assert_eq!(by_id("mac-1")["kind"], "mac");
}

#[tokio::test]
async fn the_gallery_shows_one_device_or_all_and_a_mac_is_a_device() {
    let (tmp, lib, app) = setup();
    ingest(&tmp, &lib, "beach.png", Some("ios-1"), "phone-ios", 10);
    ingest(&tmp, &lib, "screenshot.png", Some("mac-1"), "mac", 30);
    ingest(
        &tmp,
        &lib,
        "old.png",
        Some("nespc-backup"),
        "phone-archive",
        40,
    );
    ingest(&tmp, &lib, "lecture.png", None, "folder", 50);
    let (_, phone) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&device=ios-1",
        None,
    )
    .await;
    assert_eq!(titles(&phone), vec!["beach.png"]);
    let (_, mac) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&device=mac-1",
        None,
    )
    .await;
    assert_eq!(titles(&mac), vec!["screenshot.png"]);
    let (_, all) = json_of(&app, "GET", "/v1/media/items?scope=gallery", None).await;
    assert_eq!(
        titles(&all),
        vec!["beach.png", "old.png", "screenshot.png"],
        "every device's photos, not the Drive's"
    );
    let (_, drive) = json_of(&app, "GET", "/v1/media/items?scope=drive", None).await;
    assert_eq!(titles(&drive), vec!["lecture.png"]);
    // Devices seen only on items (imported before the registry) are listed with a guessed kind.
    let (_, list) = json_of(&app, "GET", "/v1/media/devices", None).await;
    let backup = list["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "nespc-backup")
        .cloned()
        .unwrap();
    assert_eq!(
        (backup["kind"].as_str(), backup["photos"].as_i64()),
        (Some("backup"), Some(1))
    );
}

#[tokio::test]
async fn an_upload_registers_its_device_and_bad_ids_are_refused() {
    let (_tmp, _lib, app) = setup();
    let (s, v) = json_of(
        &app,
        "POST",
        "/v1/media/uploads",
        Some(json!({"filename": "a.jpg", "size": 3, "meta": {"device": "android-7", "asset_id": "x"}})),
    )
    .await;
    assert!(s.is_success(), "{s} {v}");
    let (_, list) = json_of(&app, "GET", "/v1/media/devices", None).await;
    let d = list["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "android-7")
        .cloned()
        .unwrap();
    assert_eq!(
        (d["kind"].as_str(), d["name"].as_str()),
        (Some("android"), Some("android-7"))
    );
    for (uri, body) in [
        ("/v1/media/devices/has%20space", json!({"name": "x"})),
        ("/v1/media/devices/ok-id", json!({"name": ""})),
        (
            "/v1/media/devices/ok-id",
            json!({"name": "x", "kind": "toaster"}),
        ),
    ] {
        let (s, _) = json_of(&app, "PUT", uri, Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}");
    }
    let (s, _) = json_of(
        &app,
        "POST",
        "/v1/media/uploads",
        Some(json!({"filename": "a.jpg", "size": 3, "meta": {"device": "../etc"}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
