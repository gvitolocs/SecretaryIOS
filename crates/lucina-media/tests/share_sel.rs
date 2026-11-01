//! Links over chosen photos (`POST shares`): the hidden album behind them, the owner's list of both kinds,
//! and that the public side stays exactly the album-share rule. Synthetic pictures only.

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

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>, key: bool) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(uri);
    if key {
        b = b.header("x-media-upstream-key", KEY);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = res.status();
    (status, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn json_of(app: &Router, method: &str, uri: &str, body: Option<Value>, key: bool) -> (StatusCode, Value) {
    let (s, b) = call(app, method, uri, body, key).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn jpeg(shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb([shade, 120, 80]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
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

/// Three photos (two in the album "Trip") and an album "Trip"; returns (trip album id, item ids).
async fn library(tmp: &tempfile::TempDir, lib: &Library, app: &Router) -> (String, Vec<String>) {
    let mut ids = Vec::new();
    for (name, album, shade) in [("a.jpg", Some("Trip"), 10u8), ("b.jpg", Some("Trip"), 20), ("c.jpg", None, 30)] {
        let src = tmp.path().join(name);
        std::fs::write(&src, jpeg(shade)).unwrap();
        let client = ClientMeta {
            filename: Some(name.into()),
            source: Some("phone-ios".into()),
            device: Some("ios-1".into()),
            asset_id: Some(format!("asset-{name}")),
            album: album.map(Into::into),
            caption: Some("private caption".into()),
            ..Default::default()
        };
        ids.push(ingest::ingest_file(lib, &src, None, &client, None).unwrap().item_id);
    }
    wait_thumbs(lib).await;
    let (_, list) = json_of(app, "GET", "/v1/media/albums", None, true).await;
    let trip = list["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["title"] == "Trip")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    (trip, ids)
}

async fn make(app: &Router, body: Value) -> Value {
    let (s, v) = json_of(app, "POST", "/v1/media/shares", Some(body), true).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

#[tokio::test]
async fn a_selection_becomes_a_public_link_with_a_hidden_album() {
    let (tmp, lib, app) = setup();
    let (_, ids) = library(&tmp, &lib, &app).await;
    // The key is needed to make one.
    let (s, _) = call(&app, "POST", "/v1/media/shares", Some(json!({"item_ids": [ids[0]]})), false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let v = make(&app, json!({"item_ids": [ids[2], ids[0]], "title": "  Weekend  "})).await;
    let token = v["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 22);
    assert_eq!(v["kind"], "selection");
    assert_eq!(v["title"], "Weekend");
    assert_eq!(v["count"], 2);
    assert_eq!(v["public_path"], format!("/a/{token}"));
    assert_eq!(v["url_path"], format!("/v1/media/a/{token}"));
    assert_eq!(v["live"], true);
    assert!(v["expires_at"].is_null() && v["album_id"].is_null());
    assert_eq!(v["skipped"], json!([]));
    assert!(v["cover_item"].is_string() && v["cover_thumb"].is_string());

    // The public page and list serve those two and nothing else, without the owner's words.
    let (s, page) = call(&app, "GET", &format!("/v1/media/a/{token}"), None, false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(page).unwrap().contains("<title>Weekend</title>"));
    let (s, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 2);
    assert_eq!(list["title"], "Weekend");
    let text = list.to_string();
    for leak in ["private caption", "a.jpg", "c.jpg", "asset-", "ios-1"] {
        assert!(!text.contains(leak), "{leak}: {text}");
    }
    for id in [&ids[0], &ids[2]] {
        let (s, img) = call(&app, "GET", &format!("/v1/media/a/{token}/i/{id}/thumb"), None, false).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!img.windows(4).any(|w| w == b"Exif"));
    }
    let (s, _) = call(&app, "GET", &format!("/v1/media/a/{token}/i/{}/thumb", ids[1]), None, false).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "an item that was not chosen");
    // Originals only when the link says so.
    let (s, _) = call(&app, "GET", &format!("/v1/media/a/{token}/i/{}/original", ids[0]), None, false).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let v2 = make(&app, json!({"item_ids": [ids[0]], "allow_original": true})).await;
    let t2 = v2["token"].as_str().unwrap();
    assert_eq!(v2["title"], "Shared photos");
    let (s, orig) = call(&app, "GET", &format!("/v1/media/a/{t2}/i/{}/original", ids[0]), None, false).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(orig, jpeg(10));
}

#[tokio::test]
async fn the_hidden_album_is_nowhere_a_client_can_see_or_use() {
    let (tmp, lib, app) = setup();
    let (trip, ids) = library(&tmp, &lib, &app).await;
    let v = make(&app, json!({"item_ids": [ids[0], ids[1]], "title": "Hidden words"})).await;
    let token = v["token"].as_str().unwrap();
    let hidden = format!("share:{token}");

    // Not in any album list, scoped or not, and no title match.
    for uri in ["/v1/media/albums", "/v1/media/albums?scope=gallery", "/v1/media/albums?scope=drive"] {
        let (_, l) = json_of(&app, "GET", uri, None, true).await;
        let text = l.to_string();
        assert!(!text.contains("Hidden words") && !text.contains("share:"), "{uri}: {text}");
    }
    let (_, l) = json_of(&app, "GET", "/v1/media/albums", None, true).await;
    assert!(l["albums"].as_array().unwrap().iter().any(|a| a["id"] == trip.as_str()));
    // An item's own albums and its first album title do not mention the link.
    let (_, item) = json_of(&app, "GET", &format!("/v1/media/items/{}", ids[0]), None, true).await;
    assert_eq!(item["album"], "Trip");
    assert!(!item.to_string().contains("share:"), "{item}");
    // Search and the album filter do not find the hidden album's title.
    let (_, found) = json_of(&app, "GET", "/v1/media/items?q=Hidden", None, true).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 0);
    let (_, found) = json_of(&app, "GET", "/v1/media/items?album=Hidden%20words", None, true).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 0);
    // The album routes do not know it: read, rename, add, replace, remove, delete, share again.
    let a = format!("/v1/media/albums/{hidden}");
    for (m, uri, body) in [
        ("GET", a.clone(), None),
        ("PATCH", a.clone(), Some(json!({"title": "x"}))),
        ("POST", format!("{a}/items"), Some(json!({"item_ids": [ids[2]]}))),
        ("PUT", format!("{a}/items"), Some(json!({"item_ids": [ids[2]]}))),
        ("POST", format!("{a}/shares"), Some(json!({}))),
    ] {
        let (s, _) = json_of(&app, m, &uri, body, true).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {uri}");
    }
    let (s, _) = json_of(&app, "DELETE", &a, None, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, b) = json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "album_add", "ids": [ids[2]], "album": hidden})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    // Nothing changed: the link still serves exactly the two chosen items under its title.
    let (_, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!((list["total"].as_i64(), list["title"].as_str()), (Some(2), Some("Hidden words")));
    // A client upload that names an album with the same title makes its own album.
    let src = tmp.path().join("d.jpg");
    std::fs::write(&src, jpeg(99)).unwrap();
    let client = ClientMeta {
        filename: Some("d.jpg".into()),
        album: Some("Hidden words".into()),
        ..Default::default()
    };
    let d = ingest::ingest_file(&lib, &src, None, &client, None).unwrap().item_id;
    let (_, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!(list["total"], 2, "the shared link did not take the new photo");
    let (_, item) = json_of(&app, "GET", &format!("/v1/media/items/{d}"), None, true).await;
    assert_eq!(item["album"], "Hidden words");
}

#[tokio::test]
async fn the_owner_lists_both_kinds_and_revokes_either() {
    let (tmp, lib, app) = setup();
    let (trip, ids) = library(&tmp, &lib, &app).await;
    let (s, album_share) = json_of(
        &app,
        "POST",
        &format!("/v1/media/albums/{trip}/shares"),
        Some(json!({"expires_in_days": 7})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(album_share["kind"], "album");
    assert_eq!(album_share["album_id"], trip.as_str());
    assert_eq!(album_share["title"], "Trip");
    assert_eq!(album_share["count"], 2);
    let sel = make(&app, json!({"item_ids": [ids[2]], "expires_in_days": 30})).await;
    let (a_tok, s_tok) = (album_share["token"].as_str().unwrap(), sel["token"].as_str().unwrap());

    let (_, all) = json_of(&app, "GET", "/v1/media/shares", None, true).await;
    let rows = all["shares"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let find = |t: &str| rows.iter().find(|r| r["token"] == t).unwrap();
    assert_eq!((find(a_tok)["kind"].as_str(), find(s_tok)["kind"].as_str()), (Some("album"), Some("selection")));
    assert_eq!(find(s_tok)["title"], "Shared photos");
    assert_eq!(find(s_tok)["count"], 1);
    // Expiry is in days from now.
    for (row, days) in [(find(a_tok), 7), (find(s_tok), 30)] {
        let exp = chrono::DateTime::parse_from_rfc3339(row["expires_at"].as_str().unwrap()).unwrap();
        let left = exp.signed_duration_since(chrono::Utc::now());
        assert!(left > chrono::Duration::days(days - 1) && left <= chrono::Duration::days(days), "{left}");
    }
    // The album's own list shows its link, not the selection's.
    let (_, mine) = json_of(&app, "GET", &format!("/v1/media/albums/{trip}/shares"), None, true).await;
    assert_eq!(mine["shares"].as_array().unwrap().len(), 1);

    // Views are counted per link.
    let _ = call(&app, "GET", &format!("/v1/media/a/{s_tok}"), None, false).await;
    let (_, all) = json_of(&app, "GET", "/v1/media/shares", None, true).await;
    let row = all["shares"].as_array().unwrap().iter().find(|r| r["token"] == s_tok).unwrap().clone();
    assert_eq!(row["views"], 1);

    for t in [a_tok, s_tok] {
        let (s, _) = call(&app, "DELETE", &format!("/v1/media/shares/{t}"), None, true).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = call(&app, "GET", &format!("/v1/media/a/{t}/items"), None, false).await;
        assert_eq!(s, StatusCode::GONE);
    }
    let (_, all) = json_of(&app, "GET", "/v1/media/shares", None, true).await;
    assert!(all["shares"].as_array().unwrap().iter().all(|r| r["live"] == false && r["revoked_at"].is_string()));
}

#[tokio::test]
async fn bad_requests_are_refused_and_unshareable_items_are_skipped() {
    let (tmp, lib, app) = setup();
    let (_, ids) = library(&tmp, &lib, &app).await;
    let post = |body: Value| {
        let app = app.clone();
        async move { json_of(&app, "POST", "/v1/media/shares", Some(body), true).await }
    };
    assert_eq!(post(json!({})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(json!({"item_ids": []})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(json!({"item_ids": ["itm_nope"]})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(json!({"item_ids": [ids[0]], "title": "x".repeat(201)})).await.0, StatusCode::BAD_REQUEST);
    let many: Vec<String> = (0..501).map(|i| format!("itm_{i}")).collect();
    assert_eq!(post(json!({"item_ids": many})).await.0, StatusCode::BAD_REQUEST);
    // Nothing was created by any of that.
    let (_, all) = json_of(&app, "GET", "/v1/media/shares", None, true).await;
    assert_eq!(all["shares"].as_array().unwrap().len(), 0);
    let rows: i64 = lib
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM albums WHERE system_key LIKE 'share:%'", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(rows, 0);

    // A trashed item and an unknown id are skipped; the rest is shared.
    let (s, _) = call(&app, "DELETE", &format!("/v1/media/items/{}", ids[1]), None, true).await;
    assert!(s.is_success());
    let (s, v) = post(json!({"item_ids": [ids[0], ids[1], "itm_nope", ids[0]]})).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["count"], 1);
    assert_eq!(v["skipped"], json!([ids[1], "itm_nope"]));
    // Only trashed items: refused.
    assert_eq!(post(json!({"item_ids": [ids[1]]})).await.0, StatusCode::BAD_REQUEST);

    // An absurd expiry is clamped, never a panic; zero days is one day.
    for days in [u32::MAX, 0] {
        let (s, v) = post(json!({"item_ids": [ids[0]], "expires_in_days": days})).await;
        assert_eq!(s, StatusCode::CREATED, "{days}: {v}");
        let exp = chrono::DateTime::parse_from_rfc3339(v["expires_at"].as_str().unwrap()).unwrap();
        let left = exp.signed_duration_since(chrono::Utc::now());
        assert!(left > chrono::Duration::hours(23) && left <= chrono::Duration::days(3650), "{left}");
    }
}

#[tokio::test]
async fn trashing_a_shared_photo_takes_it_off_the_link_and_a_purge_clears_the_album() {
    let (tmp, lib, app) = setup();
    let (_, ids) = library(&tmp, &lib, &app).await;
    let v = make(&app, json!({"item_ids": [ids[0], ids[2]]})).await;
    let token = v["token"].as_str().unwrap();
    let (s, _) = call(&app, "DELETE", &format!("/v1/media/items/{}", ids[0]), None, true).await;
    assert!(s.is_success());
    let (_, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!(list["total"], 1);
    let (s, _) = call(&app, "GET", &format!("/v1/media/a/{token}/i/{}/thumb", ids[0]), None, false).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Restoring brings it back; purging removes the link rows for it.
    let (s, _) = call(&app, "POST", &format!("/v1/media/items/{}/restore", ids[0]), None, true).await;
    assert!(s.is_success());
    let (_, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!(list["total"], 2);
    let (s, b) = json_of(&app, "POST", "/v1/media/items/batch", Some(json!({"op": "trash", "ids": [ids[0]]})), true).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = json_of(&app, "POST", "/v1/media/items/batch", Some(json!({"op": "purge", "ids": [ids[0]]})), true).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["done"], json!([ids[0]]));
    let (_, list) = json_of(&app, "GET", &format!("/v1/media/a/{token}/items"), None, false).await;
    assert_eq!(list["total"], 1);
}
