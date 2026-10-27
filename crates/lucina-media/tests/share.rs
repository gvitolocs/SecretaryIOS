//! Album share links end to end through the router: the owner's routes need the key, the public
//! ones need only a live link, and serve only that album's items, without metadata.

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

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: bool,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
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
    let headers = res.headers().clone();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
        headers,
    )
}

async fn json_of(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: bool,
) -> (StatusCode, Value) {
    let (s, b, _) = call(app, method, uri, body, key).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

/// A JPEG with an EXIF block that names a GPS position.
fn jpeg_with_exif(shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb([shade, 120, 80]));
    let mut plain = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut plain, image::ImageFormat::Jpeg)
        .unwrap();
    let plain = plain.into_inner();
    let payload = b"Exif\0\0GPSLatitude 55.676 GPSLongitude 12.568";
    let mut out = plain[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&plain[2..]);
    out
}

async fn wait_thumbs(lib: &Library) {
    let h = tokio::spawn(ingest::thumb_worker(lib.clone()));
    for _ in 0..600 {
        let pending: i64 = lib
            .db
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM items WHERE thumb_state = 'pending'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        if pending == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    h.abort();
}

/// Two albums: "Trip" (two photos) and "Private" (one). Returns (trip id, the trip's item ids, the private item id).
async fn albums(
    tmp: &tempfile::TempDir,
    lib: &Library,
    app: &Router,
) -> (String, Vec<String>, String) {
    let mut ids = Vec::new();
    for (name, album, shade) in [
        ("a.jpg", "Trip", 10u8),
        ("b.jpg", "Trip", 20),
        ("c.jpg", "Private", 30),
    ] {
        let src = tmp.path().join(name);
        std::fs::write(&src, jpeg_with_exif(shade)).unwrap();
        let client = ClientMeta {
            filename: Some(name.into()),
            source: Some("phone-ios".into()),
            album: Some(album.into()),
            caption: Some("secret caption".into()),
            ..Default::default()
        };
        ids.push(
            ingest::ingest_file(lib, &src, None, &client, None)
                .unwrap()
                .item_id,
        );
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
    (trip, ids[..2].to_vec(), ids[2].clone())
}

async fn share(app: &Router, album: &str, body: Value) -> String {
    let (s, v) = json_of(
        app,
        "POST",
        &format!("/v1/media/albums/{album}/shares"),
        Some(body),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn owner_routes_need_the_key_and_public_ones_do_not() {
    let (tmp, lib, app) = setup();
    let (trip, _, _) = albums(&tmp, &lib, &app).await;
    let (s, _, _) = call(
        &app,
        "POST",
        &format!("/v1/media/albums/{trip}/shares"),
        Some(json!({})),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = call(&app, "GET", "/v1/media/shares", None, false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = json_of(
        &app,
        "POST",
        "/v1/media/albums/alb_nope/shares",
        Some(json!({})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let token = share(&app, &trip, json!({})).await;
    assert_eq!(token.len(), 22);
    let (s, page, headers) = call(&app, "GET", &format!("/v1/media/a/{token}"), None, false).await;
    assert_eq!(s, StatusCode::OK);
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("<title>Trip</title>") && page.contains("noindex"));
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("default-src 'none'") && csp.contains("script-src 'nonce-"),
        "{csp}"
    );
    let (_, shares) = json_of(&app, "GET", "/v1/media/shares", None, true).await;
    assert_eq!(shares["shares"][0]["views"], 1, "the page counts a view");
}

#[tokio::test]
async fn the_link_serves_only_its_album_and_nothing_private() {
    let (tmp, lib, app) = setup();
    let (trip, trip_items, private) = albums(&tmp, &lib, &app).await;
    let token = share(&app, &trip, json!({})).await;
    let (s, list) = json_of(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/items"),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 2);
    let text = list.to_string();
    for leak in [
        "secret caption",
        "a.jpg",
        "b.jpg",
        "place",
        "camera",
        "lat",
        "lon",
        "caption",
        "Private",
    ] {
        assert!(!text.contains(leak), "the item list leaks {leak}: {text}");
    }
    // A thumbnail of the album: a JPEG without EXIF.
    let (s, img, headers) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/i/{}/thumb", trip_items[0]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/jpeg");
    assert_eq!(&img[..2], &[0xFF, 0xD8]);
    assert!(!img.windows(4).any(|w| w == b"Exif") && !img.windows(3).any(|w| w == b"GPS"));
    // An item of another album, and unknown and malformed tokens.
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/i/{private}/thumb"),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(
        &app,
        "GET",
        "/v1/media/a/AAAAAAAAAAAAAAAAAAAAAA",
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(&app, "GET", "/v1/media/a/not-a-token/items", None, false).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/i/{}/exe", trip_items[0]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // An item taken out of the album after the link was made, and a trashed one.
    let (s, _, _) = call(
        &app,
        "DELETE",
        &format!("/v1/media/albums/{trip}/items"),
        Some(json!({"item_ids": [trip_items[0]]})),
        true,
    )
    .await;
    assert!(s.is_success(), "{s}");
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/i/{}/thumb", trip_items[0]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(
        &app,
        "DELETE",
        &format!("/v1/media/items/{}", trip_items[1]),
        None,
        true,
    )
    .await;
    assert!(s.is_success(), "{s}");
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/i/{}/large", trip_items[1]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, list) = json_of(
        &app,
        "GET",
        &format!("/v1/media/a/{token}/items"),
        None,
        false,
    )
    .await;
    assert_eq!(list["total"], 0);
}

#[tokio::test]
async fn originals_only_when_allowed() {
    let (tmp, lib, app) = setup();
    let (trip, items, _) = albums(&tmp, &lib, &app).await;
    let closed = share(&app, &trip, json!({})).await;
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{closed}/i/{}/original", items[0]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, list) = json_of(
        &app,
        "GET",
        &format!("/v1/media/a/{closed}/items"),
        None,
        false,
    )
    .await;
    assert!(list["items"][0]["original"].is_null());
    let open = share(&app, &trip, json!({"allow_original": true})).await;
    let (s, _, headers) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{open}/i/{}/original", items[0]),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers["content-disposition"], "attachment");
}

#[tokio::test]
async fn revoked_and_expired_links_are_gone() {
    let (tmp, lib, app) = setup();
    let (trip, items, _) = albums(&tmp, &lib, &app).await;
    let token = share(&app, &trip, json!({})).await;
    let (s, _, _) = call(
        &app,
        "DELETE",
        &format!("/v1/media/shares/{token}"),
        None,
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    for uri in [
        format!("/v1/media/a/{token}"),
        format!("/v1/media/a/{token}/items"),
        format!("/v1/media/a/{token}/i/{}/thumb", items[0]),
    ] {
        let (s, _, _) = call(&app, "GET", &uri, None, false).await;
        assert_eq!(s, StatusCode::GONE, "{uri}");
    }
    let (s, _, _) = call(
        &app,
        "DELETE",
        "/v1/media/shares/AAAAAAAAAAAAAAAAAAAAAA",
        None,
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Expired: a link whose expiry has passed (set in the catalog: the API's minimum is a day).
    let late = share(&app, &trip, json!({"expires_in_days": 1})).await;
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE album_shares SET expires_at = '2020-01-01T00:00:00.000Z' WHERE token = ?1",
                [&late],
            )
        })
        .unwrap();
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/v1/media/a/{late}/items"),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::GONE);
    let (_, shares) = json_of(
        &app,
        "GET",
        &format!("/v1/media/albums/{trip}/shares"),
        None,
        true,
    )
    .await;
    assert!(shares["shares"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["live"] == false));
}

#[tokio::test]
async fn a_link_is_rate_limited() {
    let (tmp, lib, app) = setup();
    let (trip, _, _) = albums(&tmp, &lib, &app).await;
    let token = share(&app, &trip, json!({})).await;
    let mut limited = false;
    for _ in 0..lucina_media::share::RATE_PER_MINUTE + 1 {
        let (s, _, _) = call(
            &app,
            "GET",
            &format!("/v1/media/a/{token}/items"),
            None,
            false,
        )
        .await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(
        limited,
        "more than {} requests a minute were all answered",
        lucina_media::share::RATE_PER_MINUTE
    );
}
