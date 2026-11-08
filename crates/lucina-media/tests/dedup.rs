//! Merged duplicates through the HTTP router with synthetic files: the listing hides a hidden copy,
//! `GET items/{id}` of a copy answers with the survivor, `items/{id}/duplicates` lists the copies, the
//! copy's bytes stay reachable, a batch purge of the survivor takes the copies along, and a later
//! upload of the same device asset lands on the survivor.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{dedup_merge, http, store};
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

fn png(shade: u8, side: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(side, side, image::Rgb([shade, 90, 160]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn add(
    tmp: &tempfile::TempDir,
    lib: &Library,
    shade: u8,
    side: u32,
    source: &str,
    device: Option<(&str, &str)>,
) -> ingest::IngestResult {
    let src = tmp.path().join(format!("src-{shade}-{side}-{source}"));
    std::fs::write(&src, png(shade, side)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{source}-{shade}.png")),
        source: Some(source.into()),
        device: device.map(|d| d.0.into()),
        asset_id: device.map(|d| d.1.into()),
        ..Default::default()
    };
    ingest::ingest_file(lib, &src, None, &client, None).unwrap()
}

fn ids(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_merged_copy_is_hidden_redirected_and_reachable() {
    let (tmp, lib, app) = setup();
    lib.db
        .with(|c| lucina_media::db::upsert_device(c, "iphone", "My iPhone", "iphone"))
        .unwrap();
    let phone = add(&tmp, &lib, 10, 8, "phone-ios", Some(("iphone", "A1"))).item_id;
    let backup = add(&tmp, &lib, 11, 16, "archive-nespc", None).item_id;
    lib.db
        .with(|c| {
            dedup_merge::merge(
                c,
                &backup,
                &phone,
                "same_photo_reencoded",
                0.93,
                "test",
                false,
            )
        })
        .unwrap()
        .unwrap();

    // Listings show the survivor once, in the Gallery (it took the phone's device) and everywhere.
    for uri in [
        "/v1/media/items",
        "/v1/media/items?scope=gallery",
        "/v1/media/items?origin=device:iphone",
    ] {
        let (s, v) = json_of(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
        assert_eq!(ids(&v), vec![backup.clone()], "{uri}");
    }
    let (_, src) = json_of(&app, "GET", "/v1/media/sources", None).await;
    assert_eq!(src["all"]["photos"], 1);

    // The copy's own id answers with the survivor, and says which id was asked.
    let (s, v) = json_of(&app, "GET", &format!("/v1/media/items/{phone}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["id"], backup.as_str());
    assert_eq!(v["merged_from"], phone.as_str());
    let (_, v) = json_of(&app, "GET", &format!("/v1/media/items/{backup}"), None).await;
    assert_eq!(v["id"], backup.as_str());
    assert!(v.get("merged_from").is_none());

    // The compare view's list of copies, from either id.
    for id in [&backup, &phone] {
        let (s, v) = json_of(
            &app,
            "GET",
            &format!("/v1/media/items/{id}/duplicates"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["item_id"], backup.as_str());
        let d = v["duplicates"].as_array().unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["id"], phone.as_str());
        assert_eq!(d[0]["kind"], "same_photo_reencoded");
        assert_eq!(d[0]["device_name"], "My iPhone");
    }
    let (s, _) = json_of(&app, "GET", "/v1/media/items/itm_nope/duplicates", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // The copy's bytes are still served.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/media/items/{phone}/original"))
                .header("x-media-upstream-key", KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.into_body().collect().await.unwrap().to_bytes().as_ref(),
        png(10, 8).as_slice()
    );

    // The device still sees its asset, now under the survivor.
    let (_, v) = json_of(&app, "GET", "/v1/media/devices/iphone/assets", None).await;
    let assets = v["assets"].as_array().unwrap();
    assert!(assets.iter().any(|a| a["asset_id"] == "A1"), "{assets:?}");

    // A repeated upload of the asset is a duplicate of the survivor; nothing new appears.
    let again = add(&tmp, &lib, 10, 8, "phone-ios", Some(("iphone", "A1")));
    assert!(again.duplicate);
    assert_eq!(again.item_id, backup);
    let (_, v) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids(&v).len(), 1);
}

#[tokio::test]
async fn purging_the_survivor_purges_its_copies_and_undo_brings_a_copy_back() {
    let (tmp, lib, app) = setup();
    let phone = add(&tmp, &lib, 20, 8, "phone-ios", Some(("iphone", "A2"))).item_id;
    let backup = add(&tmp, &lib, 21, 16, "archive-nespc", None).item_id;
    let spare = add(&tmp, &lib, 22, 12, "folder", None).item_id;
    let merge = |s: &str, d: &str| {
        lib.db
            .with(|c| dedup_merge::merge(c, s, d, "exact", 1.0, "test", false))
            .unwrap()
    };
    merge(&backup, &phone).unwrap();
    merge(&backup, &spare).unwrap();
    assert!(lib.db.with(|c| dedup_merge::undo(c, &spare)).unwrap());
    let (_, v) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(
        ids(&v).len(),
        2,
        "the survivor and the copy that was split off"
    );

    let (s, v) = json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "trash", "ids": [backup]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "purge", "ids": [backup]})),
    )
    .await;
    assert_eq!(v["failed"].as_array().unwrap().len(), 0, "{v}");
    let done: Vec<&str> = v["done"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(
        done.contains(&backup.as_str()) && done.contains(&phone.as_str()),
        "{done:?}"
    );
    let (_, v) = json_of(&app, "GET", &format!("/v1/media/items/{phone}"), None).await;
    assert_eq!(v["error"], "not found");
    let (_, v) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids(&v), vec![spare.clone()]);
    let links: i64 = lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM item_links WHERE undone_at IS NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(links, 0);
}
