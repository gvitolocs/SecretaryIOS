//! The photo editor's server side, end to end through the HTTP router with synthetic pictures only:
//! the edit list is validated and stored, the full-resolution derivative and the renditions are made
//! from it, the original stays untouched, and a revert brings the original look (and its URLs) back.

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, store, thumbs};
use rusqlite::params;
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
    body: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    let body = match body {
        Some(text) => {
            b = b.header("content-type", "application/json");
            Body::from(text.to_owned())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    (
        status,
        headers,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn json_of(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Value) {
    let (s, _, bytes) = call(app, method, uri, body).await;
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn gradient(w: u32, h: u32, shade: u8) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x * 200 / w) as u8, (y * 200 / h) as u8, shade])
    })
}

fn encode(img: &image::RgbImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img.clone())
        .write_to(&mut out, format)
        .unwrap();
    out.into_inner()
}

/// Ingest a synthetic picture of `w`×`h`, make its renditions like the thumbnail worker does and
/// return its id.
fn add(tmp: &tempfile::TempDir, lib: &Library, name: &str, w: u32, h: u32, shade: u8) -> String {
    let src = tmp.path().join(format!("src-{shade}-{name}"));
    std::fs::write(
        &src,
        encode(&gradient(w, h, shade), image::ImageFormat::Png),
    )
    .unwrap();
    let client = ClientMeta {
        filename: Some(name.into()),
        device: Some("ios-1".into()),
        asset_id: Some(format!("ios-1/{name}")),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    make_renditions(lib, &id);
    id
}

fn make_renditions(lib: &Library, id: &str) {
    let (path, _) = ingest::original_path(lib, id).unwrap().unwrap();
    let state = thumbs::generate(&lib.cfg, id, "photo", &path, None).unwrap();
    let hash = match &state {
        thumbs::ThumbState::Ready(h) => Some(h.clone()),
        thumbs::ThumbState::None => None,
    };
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET thumb_state = ?2, thumbhash = ?3 WHERE id = ?1",
                params![id, state.as_str(), hash],
            )
        })
        .unwrap();
}

fn count(lib: &Library, sql: &str) -> i64 {
    lib.db.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
}

fn decode(bytes: &[u8]) -> image::RgbImage {
    image::load_from_memory(bytes).unwrap().to_rgb8()
}

fn mean(img: &image::RgbImage) -> f32 {
    img.pixels()
        .map(|p| p[0] as f32 + p[1] as f32 + p[2] as f32)
        .sum::<f32>()
        / (img.width() * img.height() * 3) as f32
}

fn url(item: &Value, key: &str) -> Option<String> {
    item["urls"][key].as_str().map(String::from)
}

const CROP_BRIGHT: &str =
    r#"{"v":1,"crop":{"x":0.25,"y":0,"w":0.5,"h":1},"adjust":{"exposure":1.0}}"#;

#[tokio::test]
async fn an_edit_renders_a_derivative_and_leaves_the_original_alone() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 120, 80, 60);
    let (_, before) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    assert_eq!(before["edited"], false);
    assert!(before["urls"]["edited"].is_null() && before["urls"]["download_edited"].is_null());
    assert_eq!(
        (before["width"].as_i64(), before["height"].as_i64()),
        (Some(120), Some(80))
    );
    let (_, _, original_before) = call(&app, "GET", &url(&before, "original").unwrap(), None).await;
    let grid_before = std::fs::read(thumbs::dir_for(&lib.cfg, &id).join("g.jxl")).unwrap();

    let (s, put) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{put}");
    assert_eq!(put["edited"], true);
    let item = &put["item"];
    assert_eq!(item["edited"], true);
    assert_eq!(
        (item["width"].as_i64(), item["height"].as_i64()),
        (Some(60), Some(80)),
        "the item is as big as its derivative"
    );
    // The original is still the original; the derivative has its own URLs.
    assert_eq!(url(item, "original"), url(&before, "original"));
    assert_eq!(url(item, "download"), url(&before, "download"));
    assert!(
        url(item, "edited").unwrap().contains("/r/edited?v="),
        "{item}"
    );
    assert_eq!(
        url(item, "download_edited").as_deref(),
        Some(format!("/v1/media/items/{id}/edits/download").as_str())
    );
    assert_ne!(
        url(item, "grid"),
        url(&before, "grid"),
        "new URLs, so no cache mixes the looks"
    );
    assert_ne!(url(item, "thumb"), url(&before, "thumb"));
    assert_ne!(url(item, "preview"), url(&before, "preview"));

    // The derivative: the cropped, brighter picture, as a JPEG.
    let (s, headers, bytes) = call(&app, "GET", &url(item, "edited").unwrap(), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    let edited = decode(&bytes);
    assert_eq!((edited.width(), edited.height()), (60, 80));
    let plain = gradient(120, 80, 60);
    let middle = image::imageops::crop_imm(&plain, 30, 0, 60, 80).to_image();
    assert!(
        mean(&edited) > mean(&middle) * 1.3,
        "+1 EV: {} vs {}",
        mean(&edited),
        mean(&middle)
    );

    // The original file did not change by a byte.
    let (_, _, original_after) = call(&app, "GET", &url(item, "original").unwrap(), None).await;
    assert_eq!(original_after, original_before);

    // The renditions were made again from the edit.
    let dir = thumbs::dir_for(&lib.cfg, &id);
    assert_ne!(std::fs::read(dir.join("g.jxl")).unwrap(), grid_before);
    assert_eq!(
        image::image_dimensions(dir.join("l.jpg")).unwrap(),
        (60, 80)
    );
    let (s, headers, grid) = call(&app, "GET", &url(item, "grid").unwrap(), None).await;
    assert_eq!(
        (s, headers["content-type"].to_str().unwrap()),
        (StatusCode::OK, "image/jxl")
    );
    assert_eq!(&grid[..2], &[0xFF, 0x0A]);

    // The list comes back as it was stored; the working copy is the unedited picture.
    let (s, got) = json_of(&app, "GET", &format!("/v1/media/items/{id}/edits"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(got["edited"], true);
    assert_eq!(got["edits"]["crop"]["w"], 0.5);
    assert_eq!(got["edits"]["adjust"]["exposure"], 1.0);
    assert_eq!(got["base_url"], format!("/v1/media/items/{id}/edits/base"));
    let (s, headers, base) = call(&app, "GET", got["base_url"].as_str().unwrap(), None).await;
    assert_eq!(
        (s, headers["content-type"].to_str().unwrap()),
        (StatusCode::OK, "image/jpeg")
    );
    assert_eq!(
        (decode(&base).width(), decode(&base).height()),
        (120, 80),
        "the base is not cropped"
    );
    assert!(
        (mean(&decode(&base)) - mean(&plain)).abs() < 3.0,
        "nor brightened"
    );

    // Downloads offer both files.
    let (s, headers, bytes) = call(
        &app,
        "GET",
        &format!("/v1/media/items/{id}/edits/download"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment;") && disposition.contains("-edited.jpg"),
        "{disposition}"
    );
    assert_eq!(decode(&bytes).width(), 60);
}

#[tokio::test]
async fn reverting_brings_the_original_look_and_urls_back() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 120, 80, 60);
    let (_, before) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    let dir = thumbs::dir_for(&lib.cfg, &id);
    let (grid, small, large, preview) = ["g.jxl", "s.jpg", "l.jpg", "p.jxl"]
        .map(|f| std::fs::read(dir.join(f)).unwrap())
        .into();
    let hash_before = before["thumbhash"].clone();

    json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(r#"{"v":1,"rotate":1,"adjust":{"saturation":-100}}"#),
    )
    .await;
    assert_ne!(std::fs::read(dir.join("g.jxl")).unwrap(), grid);

    let (s, del) = json_of(&app, "DELETE", &format!("/v1/media/items/{id}/edits"), None).await;
    assert_eq!(s, StatusCode::OK, "{del}");
    assert_eq!(del["edited"], false);
    assert!(del["edits"].is_null());
    let item = &del["item"];
    assert_eq!(item["edited"], false);
    assert!(item["urls"]["edited"].is_null() && item["urls"]["download_edited"].is_null());
    assert_eq!(
        (item["width"].as_i64(), item["height"].as_i64()),
        (Some(120), Some(80)),
        "the size before the edit"
    );
    // The URLs are the ones from before the edit, and so are the bytes behind them.
    for key in ["grid", "preview", "thumb", "large", "original"] {
        assert_eq!(url(item, key), url(&before, key), "{key}");
    }
    assert_eq!(item["thumbhash"], hash_before);
    assert_eq!(std::fs::read(dir.join("g.jxl")).unwrap(), grid);
    assert_eq!(std::fs::read(dir.join("s.jpg")).unwrap(), small);
    assert_eq!(std::fs::read(dir.join("l.jpg")).unwrap(), large);
    assert_eq!(std::fs::read(dir.join("p.jxl")).unwrap(), preview);
    assert!(!dir.join("edited.jpg").exists() && !dir.join("base.jpg").exists());
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/items/{id}/r/edited?v=1"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/items/{id}/edits/download"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, got) = json_of(&app, "GET", &format!("/v1/media/items/{id}/edits"), None).await;
    assert_eq!(
        (got["edited"].clone(), got["edits"].clone()),
        (json!(false), Value::Null)
    );
    // The editor's working copy of an unedited photo is the ordinary large rendition.
    let (s, _, base) = call(
        &app,
        "GET",
        &format!("/v1/media/items/{id}/edits/base"),
        None,
    )
    .await;
    assert_eq!((s, base), (StatusCode::OK, large));

    // Reverting twice is fine.
    let (s, again) = json_of(&app, "DELETE", &format!("/v1/media/items/{id}/edits"), None).await;
    assert_eq!((s, again["edited"].clone()), (StatusCode::OK, json!(false)));
}

#[tokio::test]
async fn an_identity_list_is_a_revert() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 64, 48, 10);
    json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 1);
    let (s, put) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(r#"{"v":1,"adjust":{"exposure":0}}"#),
    )
    .await;
    assert_eq!((s, put["edited"].clone()), (StatusCode::OK, json!(false)));
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);
    // An identity on a photo that was never edited does nothing, and renders nothing.
    let (s, put) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some("{}"),
    )
    .await;
    assert_eq!((s, put["edited"].clone()), (StatusCode::OK, json!(false)));
    assert!(!thumbs::dir_for(&lib.cfg, &id).join("edited.jpg").exists());
}

#[tokio::test]
async fn bad_lists_and_wrong_items_are_refused() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 64, 48, 10);
    let put = |body: &'static str| {
        let app = app.clone();
        let uri = format!("/v1/media/items/{id}/edits");
        async move { json_of(&app, "PUT", &uri, Some(body)).await }
    };
    for body in [
        "not json",
        r#"{"v":2}"#,
        r#"{"v":1,"rotate":7}"#,
        r#"{"v":1,"straighten":90}"#,
        r#"{"v":1,"crop":{"x":0.9,"y":0,"w":0.5,"h":1}}"#,
        r#"{"v":1,"adjust":{"exposure":9}}"#,
        r#"{"v":1,"adjust":{"nonsense":1}}"#,
        r#"{"v":1,"filter":{"name":"nope","strength":1}}"#,
        r#"{"v":1,"markup":[{"t":"pen","color":"blue","width":0.01,"points":[[0,0]]}]}"#,
    ] {
        let (s, err) = put(body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
        assert!(err["error"].as_str().is_some_and(|m| !m.is_empty()));
    }
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);
    assert!(!thumbs::dir_for(&lib.cfg, &id).join("edited.jpg").exists());

    // Unknown items.
    for method in ["GET", "PUT", "DELETE"] {
        let (s, _) = json_of(
            &app,
            method,
            "/v1/media/items/itm_nope/edits",
            Some(CROP_BRIGHT),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method}");
    }
    let (s, _, _) = call(&app, "GET", "/v1/media/items/itm_nope/edits/base", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Only photos.
    lib.db
        .with(|c| {
            c.execute("INSERT OR IGNORE INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h-song', 1, 'mp3', 'store', 'x')", [])?;
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 VALUES ('itm_song', 'audio', 'song', 'h-song', 'audio/mpeg', 1, 'x', 'phone-ios', 'x', 'x')",
                [],
            )
        })
        .unwrap();
    let (s, _) = json_of(
        &app,
        "PUT",
        "/v1/media/items/itm_song/edits",
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Not while it is in the Trash.
    json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(&json!({"op": "trash", "ids": [id]}).to_string()),
    )
    .await;
    let (s, _) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_second_edit_replaces_the_first_and_a_revert_remembers_the_first_size() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 120, 80, 60);
    let size = |v: &Value| {
        (
            v["item"]["width"].as_i64().unwrap(),
            v["item"]["height"].as_i64().unwrap(),
        )
    };
    let (_, a) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(r#"{"v":1,"rotate":1}"#),
    )
    .await;
    assert_eq!(size(&a), (80, 120));
    let (_, b) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(r#"{"v":1,"crop":{"x":0,"y":0,"w":0.5,"h":0.5}}"#),
    )
    .await;
    assert_eq!(size(&b), (60, 40));
    assert_eq!(
        b["edits"]["rotate"], 0,
        "the second list replaces the first, it does not add to it"
    );
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 1);
    let (_, bytes) = {
        let (_, _, bytes) = call(
            &app,
            "GET",
            b["item"]["urls"]["edited"].as_str().unwrap(),
            None,
        )
        .await;
        ((), bytes)
    };
    assert_eq!((decode(&bytes).width(), decode(&bytes).height()), (60, 40));
    let (_, gone) = json_of(&app, "DELETE", &format!("/v1/media/items/{id}/edits"), None).await;
    assert_eq!(size(&gone), (120, 80));
}

#[tokio::test]
async fn markup_and_straightening_render_on_the_server_too() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 200, 100, 60);
    let body = r##"{"v":1,"straighten":10,"filter":{"name":"vintage","strength":0.5},
        "markup":[{"t":"pen","color":"#ff0000","width":0.05,"points":[[0.2,0.5],[0.8,0.5]]},
                  {"t":"text","color":"#ffffff","size":0.1,"x":0.05,"y":0.05,"text":"Ciao àè"}]}"##;
    let (s, put) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{put}");
    let (_, _, bytes) = call(
        &app,
        "GET",
        put["item"]["urls"]["edited"].as_str().unwrap(),
        None,
    )
    .await;
    let img = decode(&bytes);
    // Straightened by 10 degrees: smaller than the picture, same aspect.
    assert!(
        img.width() < 200 && (img.width() as f32 / img.height() as f32 - 2.0).abs() < 0.05,
        "{}x{}",
        img.width(),
        img.height()
    );
    let reds = img
        .pixels()
        .filter(|p| p[0] > 180 && p[1] < 80 && p[2] < 80)
        .count();
    assert!(reds > 100, "the pen stroke is there: {reds}");
}

#[tokio::test]
async fn formats_the_server_cannot_decode_are_edited_from_the_phones_rendering() {
    let (tmp, lib, app) = setup();
    let src = tmp.path().join("img.heic");
    std::fs::write(&src, b"not really a heic file, no decoder reads this").unwrap();
    let client = ClientMeta {
        filename: Some("img.heic".into()),
        device: Some("ios-1".into()),
        asset_id: Some("ios-1/img.heic".into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(&lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    // Nothing to decode yet: the edit is refused, not faked.
    let (s, err) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);

    // With the phone's rendering next to it, it works, from that resolution.
    let dir = thumbs::dir_for(&lib.cfg, &id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("preview.jpg"),
        encode(&gradient(96, 64, 30), image::ImageFormat::Jpeg),
    )
    .unwrap();
    let (s, put) = json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{put}");
    let (_, _, bytes) = call(
        &app,
        "GET",
        put["item"]["urls"]["edited"].as_str().unwrap(),
        None,
    )
    .await;
    assert_eq!((decode(&bytes).width(), decode(&bytes).height()), (48, 64));
    // The unedited working copy is made from the same rendering, not from the derivative.
    let (s, _, base) = call(
        &app,
        "GET",
        &format!("/v1/media/items/{id}/edits/base"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((decode(&base).width(), decode(&base).height()), (96, 64));
}

#[tokio::test]
async fn purging_an_edited_photo_removes_its_edit_and_its_derivative() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 64, 48, 10);
    json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    let dir = thumbs::dir_for(&lib.cfg, &id);
    assert!(dir.join("edited.jpg").is_file());
    let batch = |op: &str| json!({"op": op, "ids": [id]}).to_string();
    json_of(&app, "POST", "/v1/media/items/batch", Some(&batch("trash"))).await;
    let (_, r) = json_of(&app, "POST", "/v1/media/items/batch", Some(&batch("purge"))).await;
    assert_eq!(r["done"], json!([id]));
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);
    assert!(!dir.exists());
}

#[tokio::test]
async fn a_new_original_replaces_the_edit_of_the_old_one() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", 64, 48, 10);
    json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{id}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 1);
    // The phone edited the photo and sent it again under the same asset: a new version.
    let again = add(&tmp, &lib, "a.png", 80, 60, 11);
    assert_eq!(again, id, "same asset, same item");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_edits"), 0);
    let (_, item) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    assert_eq!(item["edited"], false);
    assert!(!thumbs::dir_for(&lib.cfg, &id).join("edited.jpg").exists());
    assert_eq!(
        (item["width"].as_i64(), item["height"].as_i64()),
        (Some(80), Some(60))
    );
}

#[tokio::test]
async fn listings_and_details_carry_the_edited_flag() {
    let (tmp, lib, app) = setup();
    let edited = add(&tmp, &lib, "a.png", 64, 48, 10);
    let plain = add(&tmp, &lib, "b.png", 64, 48, 20);
    json_of(
        &app,
        "PUT",
        &format!("/v1/media/items/{edited}/edits"),
        Some(CROP_BRIGHT),
    )
    .await;
    let (_, list) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&kind=photo",
        None,
    )
    .await;
    let flag = |id: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap()["edited"]
            .clone()
    };
    assert_eq!((flag(&edited), flag(&plain)), (json!(true), json!(false)));
    let (_, details) = json_of(
        &app,
        "GET",
        &format!("/v1/media/items/{edited}/details"),
        None,
    )
    .await;
    assert_eq!(details["item"]["edited"], true);
}
