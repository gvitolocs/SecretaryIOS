//! The background perceptual-hash step (`dedup_hash.rs`) with synthetic pictures: it hashes the
//! Gallery's photos from their grid JPEG in batches, skips unreadable ones without retrying them,
//! leaves trashed photos and Drive files alone, hashes a photo again when its bytes change, and
//! the duplicate review then sees the hashes.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use image::RgbImage;
use lucina_media::config::Config;
use lucina_media::dedup_hash;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, store, thumbs};
use rusqlite::params;
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn setup() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    (tmp, lib)
}

/// A phone photo with a gradient (so its hashes are not flat) and its renditions.
fn add(tmp: &tempfile::TempDir, lib: &Library, name: &str, shade: u8) -> String {
    let img = RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8 ^ shade, (y * 5) as u8, shade])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    let src = tmp.path().join(format!("src-{name}.png"));
    std::fs::write(&src, out.into_inner()).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{name}.png")),
        source: Some("phone-ios".into()),
        device: Some("ios-1".into()),
        asset_id: Some(format!("ios-1/{name}")),
        taken_at: Some("2024-01-01T10:00:00Z".into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    let (path, _) = ingest::original_path(lib, &id).unwrap().unwrap();
    let state = thumbs::generate(&lib.cfg, &id, "photo", &path, None).unwrap();
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET thumb_state = ?2 WHERE id = ?1",
                params![id, state.as_str()],
            )
        })
        .unwrap();
    id
}

fn hashed(lib: &Library) -> Vec<String> {
    lib.db
        .with(|c| {
            let mut st = c.prepare("SELECT item_id FROM item_hashes ORDER BY item_id")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .unwrap()
}

#[test]
fn hashes_new_photos_skips_bad_ones_and_follows_new_bytes() {
    let (tmp, lib) = setup();
    let a = add(&tmp, &lib, "a", 10);
    let b = add(&tmp, &lib, "b", 200);
    let broken = add(&tmp, &lib, "broken", 90);
    let trashed = add(&tmp, &lib, "trashed", 150);
    std::fs::write(
        thumbs::dir_for(&lib.cfg, &broken).join("s.jpg"),
        b"not a jpeg",
    )
    .unwrap();
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = '2024-02-01T00:00:00Z' WHERE id = ?1",
                [&trashed],
            )
        })
        .unwrap();
    assert_eq!(lib.db.with(|c| dedup_hash::pending_count(c)).unwrap(), 3);

    let mut skip = HashSet::new();
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 3);
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(hashed(&lib), want);
    assert!(skip.contains(&broken));
    // The unreadable one is not asked for again in this process.
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 0);

    // A hash of older bytes (the item got a new version since): hashed again.
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE item_hashes SET content_hash = 'older-bytes' WHERE item_id = ?1",
                [&a],
            )
        })
        .unwrap();
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 1);
    let (content, current): (String, String) = lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT h.content_hash, i.hash FROM item_hashes h JOIN items i ON i.id = h.item_id \
                 WHERE h.item_id = ?1",
                [&a],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(content, current);
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 0);
}

#[test]
fn batches_are_bounded() {
    let (tmp, lib) = setup();
    for n in 0..(dedup_hash::BATCH + 3) {
        add(&tmp, &lib, &format!("p{n}"), (n * 7) as u8);
    }
    let mut skip = HashSet::new();
    assert_eq!(
        dedup_hash::hash_pending(&lib, &mut skip).unwrap(),
        dedup_hash::BATCH
    );
    assert_eq!(lib.db.with(|c| dedup_hash::pending_count(c)).unwrap(), 3);
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 3);
    assert_eq!(dedup_hash::hash_pending(&lib, &mut skip).unwrap(), 0);
}

#[tokio::test]
async fn the_duplicate_review_sees_the_hashes() {
    let (tmp, lib) = setup();
    add(&tmp, &lib, "a", 10);
    add(&tmp, &lib, "b", 200);
    let app = http::router(lib.clone(), None);
    let get = |app: axum::Router| async move {
        let req = Request::builder()
            .uri("/v1/media/duplicates?status=open")
            .header("x-media-upstream-key", KEY)
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };
    let before = get(app.clone()).await;
    assert_eq!(before["hashes"]["pending"], 2, "{before}");
    let mut skip = HashSet::new();
    dedup_hash::hash_pending(&lib, &mut skip).unwrap();
    let after = get(app).await;
    assert_eq!(after["hashes"]["pending"], 0, "{after}");
    assert_eq!(after["hashes"]["hashed"], 2, "{after}");
}
