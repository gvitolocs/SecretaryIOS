//! The JPEG XL archive format end to end, with synthetic pictures only: apply converts and verifies,
//! the original route serves the identical bytes and ETag once the original file is gone, a
//! corrupted JXL is refused (and never lets an original be deleted), purge takes the archive copy
//! with it, and the dry run reads the library without writing to it.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::archive_jxl::{self, ApplyOpts};
use lucina_media::archive_jxl_measure::{self, DryRunOpts, Stratum};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, store, trash};
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

fn jpeg(seed: u32) -> Vec<u8> {
    let (w, h) = (96u32, 64u32);
    let mut img = image::RgbImage::new(w, h);
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
    for (x, y, p) in img.enumerate_pixels_mut() {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        let n = (s >> 28) as u8;
        *p = image::Rgb([
            ((x * 255 / w) as u8).wrapping_add(n),
            ((y * 255 / h) as u8).wrapping_add(n / 2),
            (seed as u8).wrapping_add(n),
        ]);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img)
        .unwrap();
    out
}

fn png(seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(40, 30, |x, y| {
        image::Rgb([seed, (x * 6) as u8, (y * 8) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// Ingest bytes as a phone upload; returns (item id, blob hash).
fn add(
    tmp: &tempfile::TempDir,
    lib: &Library,
    name: &str,
    source: &str,
    bytes: &[u8],
) -> (String, String) {
    let src = tmp.path().join(format!("src-{name}"));
    std::fs::write(&src, bytes).unwrap();
    let client = ClientMeta {
        filename: Some(name.into()),
        device: Some("dev1".into()),
        asset_id: Some(format!("dev1/{name}")),
        source: Some(source.into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    let hash = lib
        .db
        .with(|c| c.query_row("SELECT hash FROM items WHERE id = ?1", [&id], |r| r.get(0)))
        .unwrap();
    (id, hash)
}

fn opts(delete: bool) -> ApplyOpts {
    ApplyOpts {
        i_have_a_backup: true,
        delete_originals: delete,
        limit: None,
        max_temp_c: 1000,
    }
}

fn original_file(lib: &Library, hash: &str, ext: &str) -> std::path::PathBuf {
    lib.cfg.store_dir.join(store::object_rel(hash, ext))
}

async fn get(
    app: &Router,
    uri: &str,
    extra: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut b = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (s, h) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (s, h, bytes)
}

fn archive_rows(lib: &Library) -> i64 {
    lib.db
        .with(|c| c.query_row("SELECT count(*) FROM archive_blobs", [], |r| r.get(0)))
        .unwrap()
}

#[test]
fn apply_refuses_without_the_backup_flag_and_changes_nothing() {
    let (tmp, lib, _app) = setup();
    let (_, hash) = add(&tmp, &lib, "a.jpg", "phone-archive", &jpeg(1));
    let mut o = opts(true);
    o.i_have_a_backup = false;
    let err = archive_jxl::apply(&lib, &o).unwrap_err().to_string();
    assert!(err.contains("--i-have-a-backup"), "{err}");
    assert_eq!(archive_rows(&lib), 0);
    assert!(original_file(&lib, &hash, "jpg").is_file());
    assert!(!lib.cfg.store_dir.join("archive-jxl").exists());
}

#[tokio::test]
async fn convert_records_the_jxl_and_leaves_the_originals() {
    let (tmp, lib, app) = setup();
    let originals: Vec<Vec<u8>> = (1..=3).map(jpeg).collect();
    let items: Vec<(String, String)> = originals
        .iter()
        .enumerate()
        .map(|(i, b)| add(&tmp, &lib, &format!("p{i}.jpg"), "phone-archive", b))
        .collect();
    let stats = archive_jxl::apply(&lib, &opts(false)).unwrap();
    assert_eq!(
        (stats.candidates, stats.converted, stats.originals_deleted),
        (3, 3, 0)
    );
    assert!(stats.failed.is_empty(), "{:?}", stats.failed);
    assert!(stats.jxl_bytes_written < stats.original_bytes_converted);
    for ((id, hash), bytes) in items.iter().zip(&originals) {
        let row = lib
            .db
            .with(|c| archive_jxl::archive_row(c, hash))
            .unwrap()
            .expect("archive row");
        // One identity: the item still points at the original's hash, which the JXL rebuilds to.
        assert_eq!(row.reconstructs_to, *hash);
        assert_eq!(row.kind, "jpeg_recompress");
        assert!(row.original_deleted_at.is_none());
        assert!(archive_jxl::jxl_path(&lib.cfg, hash).is_file());
        assert_eq!(
            std::fs::read(original_file(&lib, hash, "jpg")).unwrap(),
            *bytes
        );
        let (s, h, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h[header::ETAG], format!("\"{hash}\""));
        assert_eq!(body, *bytes);
    }
    // A second run has nothing left to do.
    let again = archive_jxl::apply(&lib, &opts(false)).unwrap();
    assert_eq!((again.converted, again.already_archived), (0, 3));
}

#[tokio::test]
async fn once_the_original_is_deleted_the_route_serves_the_same_bytes_and_etag() {
    let (tmp, lib, app) = setup();
    let bytes = jpeg(11);
    let (id, hash) = add(&tmp, &lib, "x.jpg", "phone-ios", &bytes);
    let before = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!((stats.converted, stats.originals_deleted), (1, 1));
    assert_eq!(stats.original_bytes_freed, bytes.len() as u64);
    assert!(
        !original_file(&lib, &hash, "jpg").exists(),
        "the original is gone"
    );
    let row = lib
        .db
        .with(|c| archive_jxl::archive_row(c, &hash))
        .unwrap()
        .unwrap();
    assert!(row.original_deleted_at.is_some());

    let (s, h, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body, bytes, "bit-exact");
    assert_eq!(blake3::hash(&body).to_hex().to_string(), hash);
    assert_eq!(
        h[header::ETAG],
        before.1[header::ETAG],
        "same ETag as before"
    );
    assert_eq!(h[header::ETAG], format!("\"{hash}\""));
    assert_eq!(h[header::CONTENT_TYPE], before.1[header::CONTENT_TYPE]);

    // Conditional and range requests behave as for a stored file.
    let (s, _, body) = get(
        &app,
        &format!("/v1/media/items/{id}/original"),
        &[("if-none-match", &format!("\"{hash}\""))],
    )
    .await;
    assert_eq!((s, body.len()), (StatusCode::NOT_MODIFIED, 0));
    let (s, _, body) = get(
        &app,
        &format!("/v1/media/items/{id}/original"),
        &[("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, bytes[..10]);
    let (s, _, dl) = get(
        &app,
        &format!("/v1/media/items/{id}/original?download=1"),
        &[],
    )
    .await;
    assert_eq!((s, dl), (StatusCode::OK, bytes.clone()));

    // The details route (EXIF) and the thumbnails read the original too.
    let (s, _, _) = get(&app, &format!("/v1/media/items/{id}/details"), &[]).await;
    assert_eq!(s, StatusCode::OK);

    // A second run neither converts nor deletes again.
    let again = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!(
        (
            again.converted,
            again.originals_deleted,
            again.already_archived
        ),
        (0, 0, 1)
    );
}

#[tokio::test]
async fn a_corrupted_jxl_is_refused_and_never_served() {
    let (tmp, lib, app) = setup();
    let bytes = jpeg(21);
    let (id, hash) = add(&tmp, &lib, "c.jpg", "phone-ios", &bytes);
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert!(!original_file(&lib, &hash, "jpg").exists());
    // Flip bytes in the middle of the JXL and drop the cache of the rebuilt original.
    let jxl = archive_jxl::jxl_path(&lib.cfg, &hash);
    let mut data = std::fs::read(&jxl).unwrap();
    let mid = data.len() / 2;
    for b in &mut data[mid..mid + 8] {
        *b ^= 0xA5;
    }
    std::fs::write(&jxl, &data).unwrap();
    let _ = std::fs::remove_dir_all(archive_jxl::cache_dir(&lib.cfg));
    let (s, _, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    assert_eq!(
        s,
        StatusCode::SERVICE_UNAVAILABLE,
        "wrong bytes are never served"
    );
    assert!(body.len() < 200, "no picture in the error");
    // The rebuild API says so too.
    let row = lib
        .db
        .with(|c| archive_jxl::archive_row(c, &hash))
        .unwrap()
        .unwrap();
    assert!(archive_jxl::reconstruct(&lib.cfg, &row).is_err());
    assert!(archive_jxl::materialize(&lib.cfg, &lib.db, &hash).is_none());
}

#[test]
fn a_jxl_that_does_not_verify_never_lets_an_original_go() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(31);
    let (_, hash) = add(&tmp, &lib, "d.jpg", "phone-archive", &bytes);
    // Convert first, keeping the original.
    archive_jxl::apply(&lib, &opts(false)).unwrap();
    let jxl = archive_jxl::jxl_path(&lib.cfg, &hash);
    let mut data = std::fs::read(&jxl).unwrap();
    let n = data.len();
    data[n / 2] ^= 0xFF;
    data[n / 2 + 1] ^= 0xFF;
    std::fs::write(&jxl, &data).unwrap();
    // The deleting run re-verifies from disk and refuses.
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!(stats.originals_deleted, 0);
    assert_eq!(
        stats.failed.values().sum::<usize>(),
        1,
        "{:?}",
        stats.failed
    );
    assert_eq!(
        std::fs::read(original_file(&lib, &hash, "jpg")).unwrap(),
        bytes
    );
    let row = lib
        .db
        .with(|c| archive_jxl::archive_row(c, &hash))
        .unwrap()
        .unwrap();
    assert!(row.original_deleted_at.is_none());
}

#[test]
fn a_stored_object_that_is_not_what_the_catalog_says_is_left_alone() {
    let (tmp, lib, _app) = setup();
    let (_, hash) = add(&tmp, &lib, "e.jpg", "phone-archive", &jpeg(41));
    // Another valid JPEG under the same name: the catalog hash no longer matches the file.
    std::fs::write(original_file(&lib, &hash, "jpg"), jpeg(42)).unwrap();
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!((stats.converted, stats.originals_deleted), (0, 0));
    assert_eq!(stats.failed.get("roundtrip_mismatch"), Some(&1));
    assert_eq!(archive_rows(&lib), 0);
}

#[test]
fn png_is_measured_but_apply_leaves_it_alone() {
    let (tmp, lib, _app) = setup();
    let (_, jhash) = add(&tmp, &lib, "f.jpg", "phone-archive", &jpeg(51));
    let (_, phash) = add(&tmp, &lib, "g.png", "phone-ios", &png(9));
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!((stats.candidates, stats.converted), (1, 1));
    assert_eq!(stats.pixel_exact_left_alone, 1);
    assert!(original_file(&lib, &phash, "png").is_file());
    assert!(!original_file(&lib, &jhash, "jpg").exists());
}

#[test]
fn restore_puts_the_deleted_originals_back() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(61);
    let (_, hash) = add(&tmp, &lib, "h.jpg", "phone-archive", &bytes);
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert!(!original_file(&lib, &hash, "jpg").exists());
    assert_eq!(archive_jxl::restore_originals(&lib).unwrap(), 1);
    assert_eq!(
        std::fs::read(original_file(&lib, &hash, "jpg")).unwrap(),
        bytes
    );
    let row = lib
        .db
        .with(|c| archive_jxl::archive_row(c, &hash))
        .unwrap()
        .unwrap();
    assert!(row.original_deleted_at.is_none());
    assert_eq!(archive_jxl::restore_originals(&lib).unwrap(), 0);
}

#[test]
fn a_limit_makes_a_batch() {
    let (tmp, lib, _app) = setup();
    for i in 0..4 {
        add(
            &tmp,
            &lib,
            &format!("b{i}.jpg"),
            "phone-archive",
            &jpeg(70 + i),
        );
    }
    let mut o = opts(false);
    o.limit = Some(3);
    assert_eq!(archive_jxl::apply(&lib, &o).unwrap().converted, 3);
    assert_eq!(archive_jxl::apply(&lib, &o).unwrap().converted, 1);
    assert_eq!(archive_rows(&lib), 4);
}

#[test]
fn purging_an_item_takes_its_archive_copy_with_it() {
    let (tmp, lib, _app) = setup();
    let (id, hash) = add(&tmp, &lib, "i.jpg", "phone-archive", &jpeg(81));
    let (_, other_hash) = add(&tmp, &lib, "j.jpg", "phone-archive", &jpeg(82));
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    // Rebuild once so a cache file exists, then purge the first item.
    assert!(archive_jxl::materialize(&lib.cfg, &lib.db, &hash).is_some());
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
                [&id],
            )
        })
        .unwrap();
    let report = lib
        .db
        .with(|c| trash::purge_trashed(c, std::slice::from_ref(&id)))
        .unwrap();
    assert_eq!(report.done, vec![id]);
    trash::finish_purge(&lib.cfg, &report);
    assert!(!archive_jxl::jxl_path(&lib.cfg, &hash).exists());
    assert!(!archive_jxl::cache_dir(&lib.cfg)
        .join(format!("{hash}.jpg"))
        .exists());
    assert!(lib
        .db
        .with(|c| archive_jxl::archive_row(c, &hash))
        .unwrap()
        .is_none());
    // The other item is untouched.
    assert!(archive_jxl::jxl_path(&lib.cfg, &other_hash).is_file());
    assert!(lib
        .db
        .with(|c| archive_jxl::archive_row(c, &other_hash))
        .unwrap()
        .is_some());
    assert!(archive_jxl::materialize(&lib.cfg, &lib.db, &other_hash).is_some());
}

#[test]
fn an_original_another_item_still_uses_survives_the_purge_of_one() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(91);
    let (a, hash) = add(&tmp, &lib, "k1.jpg", "phone-archive", &bytes);
    // A second item row on the same bytes (the catalog allows it, e.g. a Drive copy).
    lib.db
        .with(|c| {
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 VALUES ('twin', 'photo', 'twin', ?1, 'image/jpeg', 1, 'now', 'upload', 'now', 'now')",
                [&hash],
            )
        })
        .unwrap();
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
                [&a],
            )
        })
        .unwrap();
    let report = lib.db.with(|c| trash::purge_trashed(c, &[a])).unwrap();
    trash::finish_purge(&lib.cfg, &report);
    assert!(
        archive_jxl::jxl_path(&lib.cfg, &hash).is_file(),
        "still referenced"
    );
    assert!(archive_jxl::materialize(&lib.cfg, &lib.db, &hash).is_some());
}

#[test]
fn the_dry_run_reads_the_library_and_writes_nothing_to_it() {
    let (tmp, lib, _app) = setup();
    for i in 0..6 {
        add(
            &tmp,
            &lib,
            &format!("m{i}.jpg"),
            "phone-archive",
            &jpeg(100 + i),
        );
    }
    for i in 0..2 {
        add(
            &tmp,
            &lib,
            &format!("n{i}.jpg"),
            "phone-ios",
            &jpeg(200 + i),
        );
    }
    add(&tmp, &lib, "o.png", "phone-ios", &png(5));
    add(
        &tmp,
        &lib,
        "q.heic",
        "phone-ios",
        b"\0\0\0\x18ftypheic\0\0\0\0mif1heic not a real heic",
    );
    let tree = |root: &std::path::Path| {
        walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| (e.path().to_path_buf(), e.metadata().unwrap().len()))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let store_before = tree(&lib.cfg.store_dir);
    let rep = archive_jxl_measure::dry_run(
        &lib.cfg,
        &DryRunOpts {
            sample: 600,
            seed: 7,
            jobs: 2,
            max_temp_c: 1000,
            effort9_per_stratum: 2,
            checkpoint: None,
        },
    )
    .unwrap();
    assert_eq!(
        tree(&lib.cfg.store_dir),
        store_before,
        "the store is untouched"
    );
    assert_eq!(archive_rows(&lib), 0);
    assert!(!lib.cfg.store_dir.join("archive-jxl").exists());
    let s = &rep.summary;
    assert_eq!(s.owned_files, 10);
    assert_eq!(rep.sample_size, 10);
    assert_eq!(s.round_trip_failures, 0);
    let arch = &s.strata[&Stratum::JpegArchive];
    assert_eq!((arch.sampled, arch.converted_ok), (6, 6));
    assert!(arch.total_saving_pct.unwrap() > 0.0);
    assert_eq!(arch.effort9_files, 2);
    assert!(arch.projected_saved_bytes.unwrap() > 0);
    let ios = &s.strata[&Stratum::JpegIos];
    assert_eq!((ios.sampled, ios.converted_ok), (2, 2));
    assert_eq!(s.strata[&Stratum::Lossless].converted_ok, 1);
    // The fake HEIC has no decoder and no phone preview: a counted failure, not a crash.
    let heic = &s.strata[&Stratum::HeicRaw];
    assert_eq!((heic.sampled, heic.converted_ok), (1, 0));
    assert_eq!(heic.failures.values().sum::<usize>(), 1);
    // Records carry opaque ids and no file names.
    let json = serde_json::to_string(&rep).unwrap();
    assert!(!json.contains("m0.jpg") && !json.contains(&tmp.path().to_string_lossy().to_string()));
}

#[test]
fn a_killed_dry_run_resumes_from_its_checkpoint() {
    let (tmp, lib, _app) = setup();
    for i in 0..5 {
        add(
            &tmp,
            &lib,
            &format!("r{i}.jpg"),
            "phone-archive",
            &jpeg(300 + i),
        );
    }
    let checkpoint = tmp.path().join("report.jsonl");
    let run = || {
        archive_jxl_measure::dry_run(
            &lib.cfg,
            &DryRunOpts {
                sample: 600,
                seed: 3,
                jobs: 2,
                max_temp_c: 1000,
                effort9_per_stratum: 0,
                checkpoint: Some(checkpoint.clone()),
            },
        )
        .unwrap()
    };
    let first = run();
    let lines = |p: &std::path::Path| std::fs::read_to_string(p).unwrap().lines().count();
    assert_eq!(lines(&checkpoint), 5);
    // Nothing left to measure: no new lines, the same report.
    let again = run();
    assert_eq!(lines(&checkpoint), 5);
    let ids = |r: &archive_jxl_measure::Report| {
        r.records.iter().map(|x| x.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&first), ids(&again));
    // A run killed after two files: the other three are measured on the next run.
    let kept: Vec<String> = std::fs::read_to_string(&checkpoint)
        .unwrap()
        .lines()
        .take(2)
        .map(String::from)
        .collect();
    std::fs::write(&checkpoint, kept.join("\n") + "\n").unwrap();
    let resumed = run();
    assert_eq!(lines(&checkpoint), 5);
    assert_eq!(ids(&first), ids(&resumed));
    assert_eq!(
        resumed.summary.strata[&Stratum::JpegArchive].converted_ok,
        5
    );
}
