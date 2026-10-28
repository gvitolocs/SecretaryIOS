//! The Gallery's server side, end to end through the HTTP router with synthetic files only: the item
//! fields (device, origin, rotation, trash expiry), details with EXIF, batch operations and purge,
//! the Trash's retention, device deletion requests, the source switcher with its origin filters, and
//! the month counts.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, import, store, trash};
use rusqlite::params;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn setup_with(trash_days: u32) -> (tempfile::TempDir, Library, Router) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    cfg.trash_days = trash_days;
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None);
    (tmp, lib, app)
}

fn setup() -> (tempfile::TempDir, Library, Router) {
    setup_with(30)
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

/// Ingest a synthetic picture; returns the item id.
fn add(
    tmp: &tempfile::TempDir,
    lib: &Library,
    name: &str,
    source: &str,
    device: Option<&str>,
    shade: u8,
) -> String {
    let src = tmp.path().join(format!("src-{shade}-{name}"));
    std::fs::write(&src, png(shade)).unwrap();
    let client = ClientMeta {
        filename: Some(name.into()),
        device: device.map(Into::into),
        asset_id: device.map(|d| format!("{d}/{name}")),
        source: Some(source.into()),
        ..Default::default()
    };
    ingest::ingest_file(lib, &src, None, &client, None)
        .unwrap()
        .item_id
}

/// A catalog row without a file, for listing tests (kind, source, device and date of our choosing).
fn row(lib: &Library, id: &str, kind: &str, source: &str, device: Option<&str>, sort_at: &str) {
    lib.db
        .with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO blobs (hash, bytes, ext, location, created_at) VALUES (?1, 1, 'bin', 'store', ?2)",
                params![format!("h-{id}"), sort_at],
            )?;
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, taken_at, sort_at, source, device, added_at, updated_at)
                 VALUES (?1, ?2, ?1, ?3, 'x/y', 1, ?4, ?4, ?5, ?6, ?4, ?4)",
                params![id, kind, format!("h-{id}"), sort_at, source, device],
            )
        })
        .unwrap();
}

fn ids(v: &Value) -> Vec<String> {
    let mut t: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort();
    v.into_iter().map(String::from).collect()
}

async fn batch(app: &Router, body: Value) -> (StatusCode, Value) {
    json_of(app, "POST", "/v1/media/items/batch", Some(body)).await
}

fn failed_of(v: &Value, id: &str) -> Option<String> {
    v["failed"]
        .as_array()?
        .iter()
        .find(|f| f["id"] == id)
        .map(|f| f["error"].as_str().unwrap().to_string())
}

// ---- item fields ---------------------------------------------------------------------------

#[tokio::test]
async fn an_item_names_its_device_origin_rotation_and_trash_expiry() {
    let (tmp, lib, app) = setup();
    json_of(
        &app,
        "PUT",
        "/v1/media/devices/ios-1",
        Some(json!({"name": "Test iPhone", "kind": "iphone"})),
    )
    .await;
    let id = add(&tmp, &lib, "IMG_0001.PNG", "phone-ios", Some("ios-1"), 10);
    let (s, it) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(it["device"], "ios-1");
    assert_eq!(it["device_name"], "Test iPhone");
    assert_eq!(it["origin"]["filename"], "IMG_0001.PNG");
    assert_eq!(it["origin"]["folder"], Value::Null);
    assert_eq!(it["origin"]["imported_from"], Value::Null);
    assert_eq!(it["rotation"], 0);
    assert_eq!(it["trash_expires_at"], Value::Null);

    // An item with no device or origin says null, not a made-up value.
    row(
        &lib,
        "plain",
        "photo",
        "upload",
        None,
        "2026-01-01T00:00:00.000Z",
    );
    let (_, p) = json_of(&app, "GET", "/v1/media/items/plain", None).await;
    assert_eq!(
        (&p["device"], &p["device_name"], &p["origin"]["filename"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );

    // In the Trash it expires `trash_days` after it was trashed.
    let (s, _) = json_of(&app, "DELETE", &format!("/v1/media/items/{id}"), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, t) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    let trashed: DateTime<Utc> = t["trashed_at"].as_str().unwrap().parse().unwrap();
    let expires: DateTime<Utc> = t["trash_expires_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(expires - trashed, Duration::days(30));
    // The list view carries the same fields.
    let (_, l) = json_of(&app, "GET", "/v1/media/items?trashed=true", None).await;
    assert_eq!(l["items"][0]["trash_expires_at"], t["trash_expires_at"]);
    assert_eq!(l["items"][0]["device_name"], "Test iPhone");
}

#[tokio::test]
async fn the_trash_kept_forever_has_no_expiry() {
    let (tmp, lib, app) = setup_with(0);
    let id = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 11);
    json_of(&app, "DELETE", &format!("/v1/media/items/{id}"), None).await;
    let (_, t) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    assert!(t["trashed_at"].is_string());
    assert_eq!(t["trash_expires_at"], Value::Null);
}

#[tokio::test]
async fn imports_show_their_folder_relative_to_the_import_root() {
    let (tmp, lib, app) = setup();
    let root = tmp.path().join("OldBackup");
    std::fs::create_dir_all(root.join("DCIM/2019")).unwrap();
    std::fs::write(root.join("DCIM/2019/IMG_0001.png"), png(21)).unwrap();
    std::fs::write(root.join("top.png"), png(22)).unwrap();
    let stats = import::import_path(
        &lib,
        &root,
        &import::ImportOpts {
            source: "nextcloud".into(),
            album: None,
        },
    )
    .unwrap();
    assert_eq!(stats.added, 2);
    let (_, l) = json_of(&app, "GET", "/v1/media/items?source=nextcloud", None).await;
    let origin = |name: &str| -> Value {
        l["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["origin"]["filename"] == name)
            .unwrap_or_else(|| panic!("{name} in {l}"))["origin"]
            .clone()
    };
    assert_eq!(
        origin("IMG_0001.png"),
        json!({"filename": "IMG_0001.png", "folder": "DCIM/2019", "imported_from": "OldBackup"})
    );
    assert_eq!(
        origin("top.png"),
        json!({"filename": "top.png", "folder": null, "imported_from": "OldBackup"})
    );
    // No absolute host path anywhere in the answer.
    let host = tmp.path().to_string_lossy().to_string();
    assert!(!l.to_string().contains(&host), "a host path leaked: {l}");
}

#[tokio::test]
async fn a_client_cannot_put_a_host_path_in_the_origin() {
    let (tmp, lib, app) = setup();
    let src = tmp.path().join("x.png");
    std::fs::write(&src, png(31)).unwrap();
    let client = ClientMeta {
        filename: Some("/home/nez/secret/x.png".into()),
        origin_folder: Some("/home/nez/secret".into()),
        origin_root: Some("/home/nez".into()),
        source: Some("upload".into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(&lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    let (_, it) = json_of(&app, "GET", &format!("/v1/media/items/{id}"), None).await;
    assert_eq!(it["origin"]["filename"], "x.png");
    assert_eq!(it["origin"]["folder"], Value::Null);
    assert_eq!(it["origin"]["imported_from"], Value::Null);
}

#[tokio::test]
async fn rotation_is_one_of_four_angles() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 41);
    let uri = format!("/v1/media/items/{id}");
    for r in [90, 180, 270, 0] {
        let (s, it) = json_of(&app, "PATCH", &uri, Some(json!({"rotation": r}))).await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(it["rotation"], r);
        let (_, got) = json_of(&app, "GET", &uri, None).await;
        assert_eq!(got["rotation"], r);
    }
    json_of(&app, "PATCH", &uri, Some(json!({"rotation": 90}))).await;
    for bad in [json!(45), json!(-90), json!(360), json!(91)] {
        let (s, _) = json_of(&app, "PATCH", &uri, Some(json!({"rotation": bad}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (_, got) = json_of(&app, "GET", &uri, None).await;
    assert_eq!(got["rotation"], 90, "a refused rotation changes nothing");
    // Other fields do not reset it.
    let (_, it) = json_of(&app, "PATCH", &uri, Some(json!({"caption": "x"}))).await;
    assert_eq!(it["rotation"], 90);
}

// ---- details ------------------------------------------------------------------------------

enum V<'a> {
    Ascii(&'a str),
    Short(u16),
    Long(u32),
    Rational(u32, u32),
}

/// One TIFF IFD (little endian) starting at `start` in the TIFF block; out-of-line values follow
/// the entry table.
fn ifd(entries: &[(u16, V)], start: u32) -> Vec<u8> {
    let table = 2 + entries.len() as u32 * 12 + 4;
    let mut head = (entries.len() as u16).to_le_bytes().to_vec();
    let mut data: Vec<u8> = Vec::new();
    for (tag, v) in entries {
        let (typ, count, bytes): (u16, u32, Vec<u8>) = match v {
            V::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len() as u32, b)
            }
            V::Short(x) => (3, 1, x.to_le_bytes().to_vec()),
            V::Long(x) => (4, 1, x.to_le_bytes().to_vec()),
            V::Rational(n, d) => (5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()),
        };
        head.extend(tag.to_le_bytes());
        head.extend(typ.to_le_bytes());
        head.extend(count.to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes;
            inline.resize(4, 0);
            head.extend(inline);
        } else {
            head.extend((start + table + data.len() as u32).to_le_bytes());
            data.extend(bytes);
        }
    }
    head.extend(0u32.to_le_bytes());
    head.extend(data);
    head
}

/// A small JPEG with EXIF in an APP1 segment.
fn jpeg_with_exif() -> Vec<u8> {
    let exif_entries = [
        (0x829a, V::Rational(1, 125)),
        (0x829d, V::Rational(18, 10)),
        (0x8827, V::Short(400)),
        (0x9003, V::Ascii("2025:07:12 14:03:22")),
        (0x9011, V::Ascii("+02:00")),
        (0x9209, V::Short(1)),
        (0x920a, V::Rational(35, 1)),
        (0xa434, V::Ascii("TestLens 35mm")),
    ];
    let ifd0 = |exif_at: u32| {
        ifd(
            &[
                (0x010f, V::Ascii("TestCam")),
                (0x0110, V::Ascii("TC-1")),
                (0x0112, V::Short(6)),
                (0x0131, V::Ascii("synthetic 1.0")),
                (0x8769, V::Long(exif_at)),
            ],
            8,
        )
    };
    let exif_at = 8 + ifd0(0).len() as u32;
    let mut tiff = b"II*\0".to_vec();
    tiff.extend(8u32.to_le_bytes());
    tiff.extend(ifd0(exif_at));
    tiff.extend(ifd(&exif_entries, exif_at));
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);

    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 80, 40]));
    let mut plain = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut plain, image::ImageFormat::Jpeg)
        .unwrap();
    let plain = plain.into_inner();
    assert_eq!(&plain[..2], &[0xff, 0xd8]);
    let mut out = vec![0xff, 0xd8, 0xff, 0xe1];
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&plain[2..]);
    out
}

#[tokio::test]
async fn details_read_the_exif_of_the_original() {
    let (tmp, lib, app) = setup();
    let src = tmp.path().join("exif.jpg");
    std::fs::write(&src, jpeg_with_exif()).unwrap();
    let client = ClientMeta {
        filename: Some("DSC_0042.JPG".into()),
        device: Some("cam-1".into()),
        asset_id: Some("a1".into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(&lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    let (s, d) = json_of(&app, "GET", &format!("/v1/media/items/{id}/details"), None).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["item"]["id"], id);
    assert_eq!(d["item"]["origin"]["filename"], "DSC_0042.JPG");
    assert_eq!(d["people"], json!([]));
    let e = &d["exif"];
    assert_eq!(e["make"], "TestCam");
    assert_eq!(e["model"], "TC-1");
    assert_eq!(e["lens"], "TestLens 35mm");
    assert_eq!(e["iso"], 400);
    assert_eq!(e["exposure_s"], 0.008);
    assert_eq!(e["f_number"], 1.8);
    assert_eq!(e["focal_mm"], 35.0);
    assert_eq!(e["flash"], true);
    assert_eq!(e["orientation"], 6);
    assert_eq!(e["software"], "synthetic 1.0");
    assert_eq!(e["taken_at"], "2025-07-12T12:03:22Z");
    assert_eq!(e["offset"], "+02:00");
    // Every contract field is present, a missing one is null rather than absent.
    for k in [
        "make",
        "model",
        "lens",
        "iso",
        "exposure_s",
        "f_number",
        "focal_mm",
        "flash",
        "orientation",
        "software",
        "taken_at",
        "offset",
    ] {
        assert!(e.get(k).is_some(), "{k} missing in {e}");
    }
}

#[tokio::test]
async fn details_without_exif_are_null_and_unknown_items_are_404() {
    let (tmp, lib, app) = setup();
    let id = add(&tmp, &lib, "plain.png", "phone-ios", Some("ios-1"), 51);
    let (s, d) = json_of(&app, "GET", &format!("/v1/media/items/{id}/details"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["exif"], Value::Null);
    assert_eq!(d["item"]["id"], id);
    let (s, _) = json_of(&app, "GET", "/v1/media/items/nope/details", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = json_of(&app, "GET", "/v1/media/items/..%2Fx/details", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// ---- batch ----------------------------------------------------------------------------------

#[tokio::test]
async fn batch_trash_restore_and_favorite_report_each_id() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 61);
    let b = add(&tmp, &lib, "b.png", "phone-ios", Some("ios-1"), 62);
    let (s, r) = batch(
        &app,
        json!({"op": "trash", "ids": [a, b, "missing", "bad id", a]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["done"], json!([a, b]));
    assert_eq!(failed_of(&r, "missing").as_deref(), Some("not_found"));
    assert_eq!(failed_of(&r, "bad id").as_deref(), Some("invalid_id"));
    let (_, t) = json_of(&app, "GET", "/v1/media/items?trashed=true", None).await;
    assert_eq!(ids(&t), sorted(vec![&a, &b]));
    // Trashing again is reported, not repeated.
    let (_, r) = batch(&app, json!({"op": "trash", "ids": [a]})).await;
    assert_eq!(failed_of(&r, &a).as_deref(), Some("already_trashed"));
    // Restore brings back one; restoring an item that is not trashed fails.
    let (_, r) = batch(&app, json!({"op": "restore", "ids": [a, "nope1"]})).await;
    assert_eq!(r["done"], json!([a]));
    let (_, r) = batch(&app, json!({"op": "restore", "ids": [a]})).await;
    assert_eq!(failed_of(&r, &a).as_deref(), Some("not_trashed"));
    let (_, live) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids(&live), vec![a.clone()]);
    // Favourites follow the single route: the flag and the Favourites album.
    let (_, r) = batch(&app, json!({"op": "favorite", "ids": [a, b]})).await;
    assert_eq!(r["done"], json!([a, b]));
    let (_, fav) = json_of(&app, "GET", "/v1/media/items?favorite=true", None).await;
    assert_eq!(ids(&fav), vec![a.clone()], "the trashed one is not listed");
    let (_, it) = json_of(&app, "GET", &format!("/v1/media/items/{a}"), None).await;
    assert_eq!(it["favorite"], true);
    assert!(it["albums"]
        .as_array()
        .unwrap()
        .contains(&json!("sys-favorites")));
    let (_, r) = batch(&app, json!({"op": "unfavorite", "ids": [a]})).await;
    assert_eq!(r["done"], json!([a]));
    let (_, it) = json_of(&app, "GET", &format!("/v1/media/items/{a}"), None).await;
    assert_eq!(it["favorite"], false);
    assert!(!it["albums"]
        .as_array()
        .unwrap()
        .contains(&json!("sys-favorites")));
}

#[tokio::test]
async fn batch_adds_to_and_removes_from_an_album() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 71);
    let b = add(&tmp, &lib, "b.png", "phone-ios", Some("ios-1"), 72);
    let (_, alb) = json_of(
        &app,
        "POST",
        "/v1/media/albums",
        Some(json!({"title": "Trip"})),
    )
    .await;
    let album = alb["id"].as_str().unwrap().to_string();
    let (_, r) = batch(
        &app,
        json!({"op": "album_add", "ids": [a, b], "album": album}),
    )
    .await;
    assert_eq!(r["done"], json!([a, b]));
    let (_, l) = json_of(&app, "GET", &format!("/v1/media/items?album={album}"), None).await;
    assert_eq!(ids(&l), sorted(vec![&a, &b]));
    let (_, r) = batch(
        &app,
        json!({"op": "album_remove", "ids": [a], "album": album}),
    )
    .await;
    assert_eq!(r["done"], json!([a]));
    let (_, l) = json_of(&app, "GET", &format!("/v1/media/items?album={album}"), None).await;
    assert_eq!(ids(&l), vec![b.clone()]);
    // An unknown album changes nothing and says so.
    let (s, _) = batch(
        &app,
        json!({"op": "album_add", "ids": [a], "album": "alb_nope"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = batch(&app, json!({"op": "album_add", "ids": [a]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn batch_refuses_bad_requests() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 81);
    for body in [
        json!({"op": "delete", "ids": [a]}),
        json!({"op": "trash", "ids": []}),
        json!({"op": "trash"}),
        json!({"op": "favorite", "ids": [a], "also_from_device": true}),
        json!({"op": "purge", "ids": [a], "also_from_device": true}),
    ] {
        let (s, _) = batch(&app, body.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    }
    let many: Vec<String> = (0..501).map(|i| format!("itm_{i}")).collect();
    let (s, _) = batch(&app, json!({"op": "trash", "ids": many})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let ok: Vec<String> = (0..500).map(|i| format!("itm_{i}")).collect();
    let (s, r) = batch(&app, json!({"op": "trash", "ids": ok})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["failed"].as_array().unwrap().len(), 500, "all unknown");
    // None of that touched the real item.
    let (_, live) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids(&live), vec![a]);
}

// ---- purge ----------------------------------------------------------------------------------

fn blob_files(lib: &Library) -> Vec<std::path::PathBuf> {
    lib.db
        .with(|c| {
            let mut st = c.prepare("SELECT hash, ext, location, path, bytes FROM blobs")?;
            let rows = st.query_map([], |r| {
                Ok(lucina_media::db::BlobLoc {
                    hash: r.get(0)?,
                    ext: r.get(1)?,
                    location: r.get(2)?,
                    path: r.get(3)?,
                    bytes: r.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .iter()
        .map(|b| store::path_of(&lib.cfg, b))
        .collect()
}

fn count(lib: &Library, sql: &str) -> i64 {
    lib.db.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
}

#[tokio::test]
async fn purge_only_acts_on_items_already_in_the_trash() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 91);
    let files = blob_files(&lib);
    assert_eq!(files.len(), 1);
    assert!(files[0].is_file());
    let (s, r) = batch(&app, json!({"op": "purge", "ids": [a, "ghost"]})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["done"], json!([]));
    assert_eq!(failed_of(&r, &a).as_deref(), Some("not_trashed"));
    assert_eq!(failed_of(&r, "ghost").as_deref(), Some("not_found"));
    // The item, its row and its original are all still there.
    let (s, _) = json_of(&app, "GET", &format!("/v1/media/items/{a}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(files[0].is_file());
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM blobs"), 1);
}

#[tokio::test]
async fn purge_deletes_the_item_and_everything_that_hangs_off_it() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 92);
    let keep = add(&tmp, &lib, "keep.png", "phone-ios", Some("ios-1"), 93);
    let (_, alb) = json_of(
        &app,
        "POST",
        "/v1/media/albums",
        Some(json!({"title": "T"})),
    )
    .await;
    let album = alb["id"].as_str().unwrap().to_string();
    batch(
        &app,
        json!({"op": "album_add", "ids": [a, keep], "album": album}),
    )
    .await;
    batch(&app, json!({"op": "favorite", "ids": [a]})).await;
    let thumb_dir = lucina_media::thumbs::dir_for(&lib.cfg, &a);
    std::fs::create_dir_all(&thumb_dir).unwrap();
    std::fs::write(thumb_dir.join("s.jpg"), b"t").unwrap();
    let before = blob_files(&lib).len();
    batch(&app, json!({"op": "trash", "ids": [a]})).await;
    let (_, r) = batch(&app, json!({"op": "purge", "ids": [a]})).await;
    assert_eq!(r["done"], json!([a]), "{r}");
    let (s, _) = json_of(&app, "GET", &format!("/v1/media/items/{a}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        count(
            &lib,
            &format!("SELECT COUNT(*) FROM album_items WHERE item_id = '{a}'")
        ),
        0
    );
    assert_eq!(
        count(
            &lib,
            &format!("SELECT COUNT(*) FROM items_fts WHERE item_id = '{a}'")
        ),
        0
    );
    assert!(!thumb_dir.exists(), "thumbnails go with the item");
    assert_eq!(blob_files(&lib).len(), before - 1);
    // The other item is untouched and still in the album.
    let (_, l) = json_of(&app, "GET", &format!("/v1/media/items?album={album}"), None).await;
    assert_eq!(ids(&l), vec![keep]);
}

#[tokio::test]
async fn purge_keeps_an_original_another_item_still_uses() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 94);
    let hash: String = lib
        .db
        .with(|c| c.query_row("SELECT hash FROM items WHERE id = ?1", [&a], |r| r.get(0)))
        .unwrap();
    // A second item row on the same bytes (the catalog allows it, e.g. a Drive copy).
    lib.db
        .with(|c| {
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 VALUES ('twin', 'photo', 'twin', ?1, 'image/png', 1, 'now', 'upload', 'now', 'now')",
                [&hash],
            )
        })
        .unwrap();
    let file = blob_files(&lib).remove(0);
    batch(&app, json!({"op": "trash", "ids": [a, "twin"]})).await;
    let (_, r) = batch(&app, json!({"op": "purge", "ids": [a]})).await;
    assert_eq!(r["done"], json!([a]));
    assert!(file.is_file(), "the twin still references the bytes");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM blobs"), 1);
    let (_, r) = batch(&app, json!({"op": "purge", "ids": ["twin"]})).await;
    assert_eq!(r["done"], json!(["twin"]));
    assert!(!file.exists(), "the last reference is gone");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM blobs"), 0);
}

#[tokio::test]
async fn purge_removes_the_older_versions_of_an_edited_photo() {
    let (tmp, lib, app) = setup();
    let first = {
        let src = tmp.path().join("v1.png");
        std::fs::write(&src, png(95)).unwrap();
        let client = ClientMeta {
            filename: Some("e.png".into()),
            device: Some("ios-1".into()),
            asset_id: Some("edited".into()),
            source: Some("phone-ios".into()),
            ..Default::default()
        };
        ingest::ingest_file(&lib, &src, None, &client, None).unwrap()
    };
    let src = tmp.path().join("v2.png");
    std::fs::write(&src, png(96)).unwrap();
    let client = ClientMeta {
        filename: Some("e.png".into()),
        device: Some("ios-1".into()),
        asset_id: Some("edited".into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let second = ingest::ingest_file(&lib, &src, None, &client, None).unwrap();
    assert_eq!(first.item_id, second.item_id, "the same asset is one item");
    assert_eq!(second.version, 2);
    let files = blob_files(&lib);
    assert_eq!(files.len(), 2, "both versions are stored");
    assert!(files.iter().all(|f| f.is_file()));
    let id = second.item_id;
    batch(&app, json!({"op": "trash", "ids": [id]})).await;
    let (_, r) = batch(&app, json!({"op": "purge", "ids": [id]})).await;
    assert_eq!(r["done"], json!([id]));
    assert!(
        files.iter().all(|f| !f.exists()),
        "no version is left behind"
    );
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM item_versions"), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM blobs"), 0);
}

#[tokio::test]
async fn purge_never_deletes_a_file_another_service_owns() {
    let (tmp, lib, app) = setup();
    let memo = tmp.path().join("memo.m4a");
    std::fs::write(&memo, b"not really audio").unwrap();
    let r = ingest::register_external(
        &lib,
        "voice-memo",
        "m1",
        &ingest::ExternalReq {
            kind: Some("audio".into()),
            title: Some("memo".into()),
            caption: None,
            created_at: None,
            duration_ms: None,
            mime: Some("audio/mp4".into()),
            path: memo.to_string_lossy().to_string(),
            links: Value::Null,
            album: None,
        },
    )
    .unwrap();
    batch(&app, json!({"op": "trash", "ids": [r.item_id]})).await;
    let (_, p) = batch(&app, json!({"op": "purge", "ids": [r.item_id]})).await;
    assert_eq!(p["done"], json!([r.item_id]));
    assert!(memo.is_file(), "an in-place file is not ours to delete");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM externals"), 0);
}

// ---- retention -----------------------------------------------------------------------------

fn trashed_at(lib: &Library, id: &str, at: DateTime<Utc>) {
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = ?2 WHERE id = ?1",
                params![id, at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)],
            )
        })
        .unwrap();
}

#[tokio::test]
async fn retention_purges_only_what_has_been_in_the_trash_too_long() {
    let (tmp, lib, app) = setup_with(1);
    let old = add(&tmp, &lib, "old.png", "phone-ios", Some("ios-1"), 101);
    let fresh = add(&tmp, &lib, "fresh.png", "phone-ios", Some("ios-1"), 102);
    let live = add(&tmp, &lib, "live.png", "phone-ios", Some("ios-1"), 103);
    let now = Utc::now();
    trashed_at(&lib, &old, now - Duration::hours(25));
    trashed_at(&lib, &fresh, now - Duration::hours(12));
    assert_eq!(blob_files(&lib).len(), 3);
    assert_eq!(trash::purge_expired(&lib, now).unwrap(), 1);
    let (s, _) = json_of(&app, "GET", &format!("/v1/media/items/{old}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for kept in [&fresh, &live] {
        let (s, _) = json_of(&app, "GET", &format!("/v1/media/items/{kept}"), None).await;
        assert_eq!(s, StatusCode::OK);
    }
    assert_eq!(blob_files(&lib).len(), 2);
    // Nothing more to do at the same moment; a day later the other trashed one expires too, and
    // the item that was never trashed never does.
    assert_eq!(trash::purge_expired(&lib, now).unwrap(), 0);
    assert_eq!(
        trash::purge_expired(&lib, now + Duration::days(2)).unwrap(),
        1
    );
    let (_, rest) = json_of(&app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids(&rest), vec![live]);
}

#[tokio::test]
async fn retention_of_zero_days_never_purges() {
    let (tmp, lib, _app) = setup_with(0);
    let a = add(&tmp, &lib, "a.png", "phone-ios", Some("ios-1"), 104);
    trashed_at(&lib, &a, Utc::now() - Duration::days(3650));
    assert_eq!(trash::purge_expired(&lib, Utc::now()).unwrap(), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 1);
}

// ---- device deletions ----------------------------------------------------------------------

#[tokio::test]
async fn also_from_device_queues_a_request_the_device_can_fetch_and_ack() {
    let (tmp, lib, app) = setup();
    let on_phone = add(&tmp, &lib, "p.png", "phone-ios", Some("ios-1"), 111);
    let on_phone2 = add(&tmp, &lib, "q.png", "phone-ios", Some("ios-1"), 112);
    let chat = add(&tmp, &lib, "c.png", "ios-chat", None, 113);
    // A plain trash asks nothing of the device.
    let (_, r) = batch(&app, json!({"op": "trash", "ids": [on_phone2]})).await;
    assert_eq!(r["queued"], 0);
    let (_, q) = json_of(&app, "GET", "/v1/media/devices/ios-1/deletions", None).await;
    assert_eq!(q["pending"], json!([]));
    // With the option, the items that have a device asset are queued; the chat one has none.
    let (s, r) = batch(
        &app,
        json!({"op": "trash", "ids": [on_phone, chat], "also_from_device": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["done"], json!([on_phone, chat]));
    assert_eq!(r["queued"], 1);
    let (_, q) = json_of(&app, "GET", "/v1/media/devices/ios-1/deletions", None).await;
    let pending = q["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["asset_id"], "ios-1/p.png");
    assert_eq!(pending[0]["item_id"], on_phone);
    assert!(pending[0]["requested_at"].is_string());
    // Another device's queue is its own.
    let (_, other) = json_of(&app, "GET", "/v1/media/devices/mac-9/deletions", None).await;
    assert_eq!(other["pending"], json!([]));
    // The request survives purging the item: it is for the device.
    batch(&app, json!({"op": "purge", "ids": [on_phone]})).await;
    let (_, q) = json_of(&app, "GET", "/v1/media/devices/ios-1/deletions", None).await;
    assert_eq!(q["pending"].as_array().unwrap().len(), 1);
    // Ack: only pending assets count, a second ack finds nothing.
    let (s, a) = json_of(
        &app,
        "POST",
        "/v1/media/devices/ios-1/deletions/ack",
        Some(json!({"asset_ids": ["ios-1/p.png", "ios-1/never-asked.png"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["acked"], 1);
    let (_, q) = json_of(&app, "GET", "/v1/media/devices/ios-1/deletions", None).await;
    assert_eq!(q["pending"], json!([]));
    let (_, a) = json_of(
        &app,
        "POST",
        "/v1/media/devices/ios-1/deletions/ack",
        Some(json!({"asset_ids": ["ios-1/p.png"]})),
    )
    .await;
    assert_eq!(a["acked"], 0);
}

#[tokio::test]
async fn trashing_again_after_a_restore_asks_the_device_again() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "p.png", "phone-ios", Some("ios-1"), 114);
    let ask = json!({"op": "trash", "ids": [a], "also_from_device": true});
    batch(&app, ask.clone()).await;
    json_of(
        &app,
        "POST",
        "/v1/media/devices/ios-1/deletions/ack",
        Some(json!({"asset_ids": ["ios-1/p.png"]})),
    )
    .await;
    batch(&app, json!({"op": "restore", "ids": [a]})).await;
    batch(&app, ask).await;
    let (_, q) = json_of(&app, "GET", "/v1/media/devices/ios-1/deletions", None).await;
    assert_eq!(q["pending"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn deletion_routes_refuse_bad_device_ids() {
    let (_tmp, _lib, app) = setup();
    let (s, _) = json_of(&app, "GET", "/v1/media/devices/has%20space/deletions", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = json_of(
        &app,
        "POST",
        "/v1/media/devices/has%20space/deletions/ack",
        Some(json!({"asset_ids": []})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_plain_delete_never_queues_a_device_deletion() {
    let (tmp, lib, app) = setup();
    let a = add(&tmp, &lib, "p.png", "phone-ios", Some("ios-1"), 115);
    json_of(&app, "DELETE", &format!("/v1/media/items/{a}"), None).await;
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM device_deletions"), 0);
}

// ---- sources, origin filters, months ---------------------------------------------------------

fn seed_sources(lib: &Library) {
    row(
        lib,
        "ios-photo",
        "photo",
        "phone-ios",
        Some("ios-1"),
        "2026-10-05T10:00:00.000Z",
    );
    row(
        lib,
        "ios-video",
        "video",
        "phone-ios",
        Some("ios-1"),
        "2026-09-20T10:00:00.000Z",
    );
    row(
        lib,
        "mac-photo",
        "photo",
        "mac",
        Some("mac-1"),
        "2026-10-07T10:00:00.000Z",
    );
    row(
        lib,
        "chat-photo",
        "photo",
        "ios-chat",
        None,
        "2026-10-09T10:00:00.000Z",
    );
    row(
        lib,
        "share-photo",
        "photo",
        "web-share",
        None,
        "2026-08-01T10:00:00.000Z",
    );
    row(
        lib,
        "chat-doc",
        "file",
        "ios-share",
        None,
        "2026-10-01T10:00:00.000Z",
    );
    row(
        lib,
        "lucina-photo",
        "photo",
        "lucina",
        None,
        "2026-10-08T10:00:00.000Z",
    );
    row(
        lib,
        "drive-photo",
        "photo",
        "folder",
        None,
        "2026-10-10T10:00:00.000Z",
    );
    row(
        lib,
        "selftest",
        "photo",
        "ios-selftest-1",
        None,
        "2026-10-11T10:00:00.000Z",
    );
    row(
        lib,
        "hidden-live",
        "video",
        "phone-ios",
        Some("ios-1"),
        "2026-10-05T10:00:01.000Z",
    );
    lib.db
        .with(|c| c.execute("UPDATE items SET hidden = 1 WHERE id = 'hidden-live'", []))
        .unwrap();
}

#[tokio::test]
async fn sources_list_devices_chat_and_lucina_with_counts() {
    let (_tmp, lib, app) = setup();
    json_of(
        &app,
        "PUT",
        "/v1/media/devices/ios-1",
        Some(json!({"name": "Test iPhone", "kind": "iphone"})),
    )
    .await;
    json_of(
        &app,
        "PUT",
        "/v1/media/devices/mac-1",
        Some(json!({"name": "Test Mac", "kind": "mac"})),
    )
    .await;
    seed_sources(&lib);
    let (s, v) = json_of(&app, "GET", "/v1/media/sources", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let src = |id: &str| -> Value {
        v["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} in {v}"))
    };
    let phone = src("device:ios-1");
    assert_eq!(phone["label"], "Test iPhone");
    assert_eq!(phone["kind"], "iphone");
    assert_eq!(
        (phone["photos"].as_i64(), phone["videos"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(phone["latest_taken_at"], "2026-10-05T10:00:00.000Z");
    assert_eq!(src("device:mac-1")["photos"], 1);
    let chat = src("chat");
    assert_eq!(chat["label"], "Chat & shares");
    assert_eq!(chat["kind"], "chat");
    assert_eq!(
        (chat["photos"].as_i64(), chat["videos"].as_i64()),
        (Some(2), Some(0))
    );
    assert_eq!(chat["latest_taken_at"], "2026-10-09T10:00:00.000Z");
    assert_eq!(src("lucina")["photos"], 1);
    // The devices come before Chat and Lucina; the Drive's photo and the self-test are nowhere.
    let order: Vec<&str> = v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(order.len(), 4, "{order:?}");
    assert_eq!(&order[2..], ["chat", "lucina"]);
    // All = the Gallery: devices + chat photos/videos + Lucina, not the Drive, not hidden parts.
    assert_eq!(v["all"], json!({"photos": 5, "videos": 1}));
}

#[tokio::test]
async fn groups_with_nothing_in_them_are_left_out() {
    let (_tmp, lib, app) = setup();
    row(
        &lib,
        "only",
        "photo",
        "phone-ios",
        Some("ios-1"),
        "2026-10-05T10:00:00.000Z",
    );
    let (_, v) = json_of(&app, "GET", "/v1/media/sources", None).await;
    let ids: Vec<&str> = v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["device:ios-1"]);
    let (_t, _l, app2) = setup();
    let (_, empty) = json_of(&app2, "GET", "/v1/media/sources", None).await;
    assert_eq!(
        empty,
        json!({"sources": [], "all": {"photos": 0, "videos": 0}})
    );
}

#[tokio::test]
async fn the_origin_filter_picks_a_device_chat_lucina_or_the_whole_gallery() {
    let (_tmp, lib, app) = setup();
    seed_sources(&lib);
    let list = |q: &'static str| {
        let app = app.clone();
        async move {
            let (s, v) = json_of(&app, "GET", &format!("/v1/media/items?{q}"), None).await;
            assert_eq!(s, StatusCode::OK, "{q}: {v}");
            ids(&v)
        }
    };
    assert_eq!(
        list("origin=device:ios-1").await,
        sorted(vec!["ios-photo", "ios-video"])
    );
    assert_eq!(list("origin=device:mac-1").await, vec!["mac-photo"]);
    assert_eq!(
        list("origin=chat").await,
        sorted(vec!["chat-photo", "share-photo"])
    );
    assert_eq!(list("origin=lucina").await, vec!["lucina-photo"]);
    let all = sorted(vec![
        "ios-photo",
        "ios-video",
        "mac-photo",
        "chat-photo",
        "share-photo",
        "lucina-photo",
    ]);
    assert_eq!(list("origin=all").await, all);
    // `scope=gallery` is the same set; a Drive item is not in it, a chat photo is not in the Drive.
    assert_eq!(list("scope=gallery").await, all);
    assert_eq!(
        list("scope=drive").await,
        sorted(vec!["chat-doc", "drive-photo", "selftest"])
    );
    // Kinds are a comma list.
    assert_eq!(list("origin=all&kind=video").await, vec!["ios-video"]);
    assert_eq!(
        list("origin=all&kind=photo,video").await,
        all,
        "photos and videos together"
    );
    assert_eq!(
        list("origin=device:ios-1&kind=photo").await,
        vec!["ios-photo"]
    );
    // Origin and an unknown one.
    let (s, _) = json_of(&app, "GET", "/v1/media/items?origin=drive", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = json_of(&app, "GET", "/v1/media/items?origin=device:", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A blank origin means none.
    assert_eq!(list("origin=").await.len(), 9);
}

#[tokio::test]
async fn the_gallery_listing_pages_newest_first_with_a_cursor() {
    let (_tmp, lib, app) = setup();
    for i in 0..7 {
        row(
            &lib,
            &format!("p{i}"),
            "photo",
            "phone-ios",
            Some("ios-1"),
            &format!("2026-10-0{}T10:00:00.000Z", i + 1),
        );
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!(
                "/v1/media/items?origin=all&limit=3&cursor={}",
                c.replace('|', "%7C")
            ),
            None => "/v1/media/items?origin=all&limit=3".to_string(),
        };
        let (_, v) = json_of(&app, "GET", &uri, None).await;
        for i in v["items"].as_array().unwrap() {
            seen.push(i["id"].as_str().unwrap().to_string());
        }
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, vec!["p6", "p5", "p4", "p3", "p2", "p1", "p0"]);
}

#[tokio::test]
async fn months_count_the_items_of_each_month_under_the_listing_filters() {
    let (_tmp, lib, app) = setup();
    seed_sources(&lib);
    let (s, v) = json_of(&app, "GET", "/v1/media/items/months?origin=all", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["months"],
        json!([
            {"month": "2026-10", "count": 4},
            {"month": "2026-09", "count": 1},
            {"month": "2026-08", "count": 1},
        ])
    );
    let (_, v) = json_of(&app, "GET", "/v1/media/items/months?origin=chat", None).await;
    assert_eq!(
        v["months"],
        json!([{"month": "2026-10", "count": 1}, {"month": "2026-08", "count": 1}])
    );
    let (_, v) = json_of(
        &app,
        "GET",
        "/v1/media/items/months?origin=device:ios-1&kind=video",
        None,
    )
    .await;
    assert_eq!(v["months"], json!([{"month": "2026-09", "count": 1}]));
    // The Trash has its own months; trashed items leave the others.
    batch(&app, json!({"op": "trash", "ids": ["share-photo"]})).await;
    let (_, v) = json_of(&app, "GET", "/v1/media/items/months?origin=all", None).await;
    assert_eq!(v["months"].as_array().unwrap().len(), 2);
    let (_, v) = json_of(&app, "GET", "/v1/media/items/months?trashed=true", None).await;
    assert_eq!(v["months"], json!([{"month": "2026-08", "count": 1}]));
    let (s, _) = json_of(&app, "GET", "/v1/media/items/months?origin=nope", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---- upgrade --------------------------------------------------------------------------------

#[tokio::test]
async fn an_older_catalog_upgrades_in_place_without_losing_items() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("catalog.db");
    {
        let db = lucina_media::Db::open(&path).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h', 1, 'bin', 'store', 'now')",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 VALUES ('old', 'photo', 'IMG_9.JPG', 'h', 'image/jpeg', 1, '2020-01-01T00:00:00.000Z', 'takeout', 'now', 'now')",
                [],
            )?;
            // Back to the schema before the Gallery.
            for col in ["rotation", "origin_name", "origin_folder", "origin_root"] {
                c.execute(&format!("ALTER TABLE items DROP COLUMN {col}"), [])?;
            }
            c.execute("DROP TABLE device_deletions", [])
        })
        .unwrap();
    }
    let db = lucina_media::Db::open(&path).unwrap();
    let (title, rotation, name): (String, i64, Option<String>) = db
        .with(|c| {
            c.query_row(
                "SELECT title, rotation, origin_name FROM items WHERE id = 'old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!((title.as_str(), rotation, name), ("IMG_9.JPG", 0, None));
    let n: i64 = db
        .with(|c| c.query_row("SELECT COUNT(*) FROM device_deletions", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 0);
    // Opening it a second time changes nothing.
    drop(db);
    lucina_media::Db::open(&path).unwrap();
}
