//! The JPEG XL archive format end to end, with synthetic pictures only: apply converts and verifies
//! JPEGs (bit-exact, blake3 and SHA-256) and PNGs (pixel digest), the original route serves the
//! identical bytes and ETag (JPEG) or the same picture under a weak ETag (PNG) once the original
//! file is gone, a corrupted JXL is refused (and never lets an original be deleted), the manifest
//! lists every file, purge takes the archive copy with it, and the dry run reads the library
//! without writing to it.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::archive_jxl::Samples;
use lucina_media::archive_jxl::{self, ApplyOpts, Kinds};
use lucina_media::archive_jxl_measure::{self, DryRunOpts, Stratum};
use lucina_media::archive_png::{self, PngColor, PngPixels};
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
        kinds: Kinds::default(),
        manifest: None,
    }
}

fn kinds(jpeg: bool, png: bool) -> Kinds {
    Kinds { jpeg, png }
}

/// A minimal matrix/TRC ICC profile with Display P3 colorants (D50) and the sRGB curve, the shape
/// of what an iPhone screenshot carries.
fn p3_profile() -> Vec<u8> {
    let fixed = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
    let xyz = |c: [f64; 3]| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for v in c {
            t.extend_from_slice(&fixed(v));
        }
        t
    };
    let mut trc = b"para\0\0\0\0\0\x03\0\0".to_vec();
    for v in [2.4f64, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
        trc.extend_from_slice(&fixed(v));
    }
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"rXYZ", xyz([0.5151, 0.2412, -0.0011])),
        (b"gXYZ", xyz([0.2920, 0.6922, 0.0419])),
        (b"bXYZ", xyz([0.1571, 0.0666, 0.7841])),
        (b"rTRC", trc.clone()),
        (b"gTRC", trc.clone()),
        (b"bTRC", trc),
    ];
    let mut out = vec![0u8; 128];
    out[16..20].copy_from_slice(b"RGB ");
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut offset = 132 + tags.len() * 12;
    let mut body = Vec::new();
    for (sig, data) in &tags {
        out.extend_from_slice(*sig);
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len();
        body.extend_from_slice(data);
    }
    out.extend_from_slice(&body);
    out
}

fn adobe_profile() -> Vec<u8> {
    // Same layout with Adobe RGB colorants: refused by the archive.
    let mut p = p3_profile();
    let fixed = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
    // rXYZ data starts after the 6 tag entries: 132 + 72 = 204, payload at +8.
    p[212..216].copy_from_slice(&fixed(0.6097));
    p[216..220].copy_from_slice(&fixed(0.3111));
    p[220..224].copy_from_slice(&fixed(0.0195));
    p
}

/// A PNG of the given layout with noisy, screenshot-ish content and an optional profile.
fn png_of(color: PngColor, w: u32, h: u32, seed: u32, icc: Option<Vec<u8>>) -> Vec<u8> {
    let n = w as usize * h as usize * color.channels();
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(9);
    let mut next = || {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        s >> 16
    };
    let samples = if color.wide() {
        Samples::U16((0..n).map(|_| next() as u16).collect())
    } else {
        Samples::U8(
            (0..n)
                .map(|i| ((i as u32 / 9 + next() % 5) % 256) as u8)
                .collect(),
        )
    };
    archive_png::encode(&PngPixels {
        width: w,
        height: h,
        color,
        samples,
        icc,
    })
    .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    archive_jxl::sha256_hex(bytes)
}

fn digest_of(png: &[u8]) -> String {
    archive_png::digest(&archive_png::decode(png).unwrap())
}

fn manifest_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
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
fn restore_puts_the_deleted_originals_back() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(61);
    let (_, hash) = add(&tmp, &lib, "h.jpg", "phone-archive", &bytes);
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert!(!original_file(&lib, &hash, "jpg").exists());
    let restored = archive_jxl::restore_originals(&lib).unwrap();
    assert_eq!((restored.restored, restored.png_not_restorable), (1, 0));
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
    assert_eq!(archive_jxl::restore_originals(&lib).unwrap().restored, 0);
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

// ---- 2026-10-28: JPEG SHA-256, PNG, --kinds, the manifest --------------------------------------

fn row_of(lib: &Library, hash: &str) -> archive_jxl::ArchiveRow {
    lib.db
        .with(|c| archive_jxl::archive_row(c, hash))
        .unwrap()
        .expect("archive row")
}

#[test]
fn a_jpegs_sha256_is_recorded_and_checked() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(111);
    let (_, hash) = add(&tmp, &lib, "s.jpg", "phone-archive", &bytes);
    archive_jxl::apply(&lib, &opts(false)).unwrap();
    let row = row_of(&lib, &hash);
    assert_eq!(
        row.original_sha256.as_deref(),
        Some(sha256(&bytes).as_str())
    );
    assert_eq!(row.kind, "jpeg_recompress");
    // The rebuilt file is checked against it: a wrong recorded SHA-256 makes the JXL unusable...
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE archive_blobs SET original_sha256 = ?2 WHERE hash = ?1",
                rusqlite::params![hash, "0".repeat(64)],
            )
        })
        .unwrap();
    assert!(matches!(
        archive_jxl::reconstruct(&lib.cfg, &row_of(&lib, &hash)),
        Err(archive_jxl::ConvertError::Mismatch)
    ));
    // ...so the deleting run keeps the original and lists it as a failure.
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!(stats.originals_deleted, 0);
    assert_eq!(stats.failed.get("roundtrip_mismatch"), Some(&1));
    assert_eq!(
        std::fs::read(original_file(&lib, &hash, "jpg")).unwrap(),
        bytes
    );
}

#[test]
fn a_jpeg_converted_before_the_sha256_existed_gets_it_when_its_original_goes() {
    let (tmp, lib, _app) = setup();
    let bytes = jpeg(112);
    let (_, hash) = add(&tmp, &lib, "t.jpg", "phone-archive", &bytes);
    archive_jxl::apply(&lib, &opts(false)).unwrap();
    lib.db
        .with(|c| c.execute("UPDATE archive_blobs SET original_sha256 = NULL", []))
        .unwrap();
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    let row = row_of(&lib, &hash);
    assert!(row.original_deleted_at.is_some());
    assert_eq!(
        row.original_sha256.as_deref(),
        Some(sha256(&bytes).as_str())
    );
}

#[tokio::test]
async fn a_png_is_archived_deleted_and_rebuilt_as_the_same_picture() {
    let (tmp, lib, app) = setup();
    let icc = p3_profile();
    let png = png_of(PngColor::Rgba8, 64, 48, 5, Some(icc.clone()));
    let (id, hash) = add(&tmp, &lib, "shot.png", "phone-ios", &png);
    let want = digest_of(&png);
    let before = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    assert_eq!(before.2, png);
    assert_eq!(before.1[header::ETAG], format!("\"{hash}\""));

    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!((stats.converted, stats.originals_deleted), (1, 1));
    assert_eq!(stats.converted_by_kind.get("png"), Some(&1));
    assert_eq!(stats.pixel_exact_left_alone, 0);
    assert!(stats.failed.is_empty(), "{:?}", stats.failed);
    assert!(
        !original_file(&lib, &hash, "png").exists(),
        "the PNG original is gone"
    );
    let row = row_of(&lib, &hash);
    assert_eq!(row.kind, "png_lossless");
    assert_eq!(row.pixel_digest.as_deref(), Some(want.as_str()));
    assert_eq!(row.reconstructs_to, want);
    assert_eq!(row.png_color.as_deref(), Some("rgba8"));
    assert_eq!(row.icc.as_deref(), Some(icc.as_slice()));
    assert_eq!(row.original_sha256.as_deref(), Some(sha256(&png).as_str()));
    assert!(row.original_deleted_at.is_some());
    // The identity is the original's.
    let item_hash: String = lib
        .db
        .with(|c| c.query_row("SELECT hash FROM items WHERE id = ?1", [&id], |r| r.get(0)))
        .unwrap();
    assert_eq!(item_hash, hash);

    let uri = format!("/v1/media/items/{id}/original");
    let (s, h, body) = get(&app, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        h[header::ETAG],
        format!("W/\"{hash}\""),
        "weak: not the same bytes"
    );
    // Same pixels, same layout, same profile; the file itself is not promised to be the original's.
    let rebuilt = archive_png::decode(&body).unwrap();
    assert_eq!(archive_png::digest(&rebuilt), want);
    assert_eq!(rebuilt.icc.as_deref(), Some(icc.as_slice()), "ICC kept");
    assert_eq!(rebuilt.color, PngColor::Rgba8);

    // Conditional requests: the weak and the old strong tag both match; neither mixes bytes.
    for tag in [format!("W/\"{hash}\""), format!("\"{hash}\"")] {
        let (s, h, b) = get(&app, &uri, &[("if-none-match", &tag)]).await;
        assert_eq!((s, b.len()), (StatusCode::NOT_MODIFIED, 0), "{tag}");
        assert_eq!(h[header::ETAG], format!("W/\"{hash}\""));
    }
    let (s, _, part) = get(&app, &uri, &[("range", "bytes=0-9")]).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part, body[..10]);
    // If-Range can never validate a weak tag, an old strong one included: the whole file comes.
    for validator in [format!("\"{hash}\""), format!("W/\"{hash}\"")] {
        let (s, _, all) = get(
            &app,
            &uri,
            &[("range", "bytes=0-9"), ("if-range", &validator)],
        )
        .await;
        assert_eq!((s, all.len()), (StatusCode::OK, body.len()), "{validator}");
        assert_eq!(all, body);
    }
    let (s, h, dl) = get(&app, &format!("{uri}?download=1"), &[]).await;
    assert_eq!((s, dl), (StatusCode::OK, body.clone()));
    assert!(h[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("attachment"));

    // Thumbnails and details are made from the rebuilt file.
    let (path, _) = ingest::original_path(&lib, &id).unwrap().unwrap();
    assert!(archive_jxl::is_rebuilt(&lib.cfg, &path));
    let state =
        lucina_media::thumbs::generate(&lib.cfg, &id, "photo", &path, None).expect("thumbnail");
    assert!(matches!(state, lucina_media::thumbs::ThumbState::Ready(_)));
    let (s, _, _) = get(&app, &format!("/v1/media/items/{id}/details"), &[]).await;
    assert_eq!(s, StatusCode::OK);

    // A second run neither converts nor deletes again, and the cache serves the same file.
    let again = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!(
        (
            again.converted,
            again.originals_deleted,
            again.already_archived
        ),
        (0, 0, 1)
    );
    assert_eq!(get(&app, &uri, &[]).await.2, body);
}

#[tokio::test]
async fn every_supported_png_layout_comes_back_in_its_own_layout() {
    let (tmp, lib, app) = setup();
    let layouts = [
        (PngColor::L8, None),
        (PngColor::La8, None),
        (PngColor::Rgb8, None),
        (PngColor::L16, None),
        (PngColor::Rgb16, Some(p3_profile())),
        (PngColor::Rgba16, None),
    ];
    let mut items = Vec::new();
    for (i, (color, icc)) in layouts.iter().enumerate() {
        let png = png_of(*color, 30 + i as u32, 20, i as u32, icc.clone());
        let (id, hash) = add(&tmp, &lib, &format!("l{i}.png"), "phone-ios", &png);
        items.push((id, hash, digest_of(&png), *color));
    }
    let stats = archive_jxl::apply(&lib, &opts(true)).unwrap();
    assert_eq!(stats.converted, layouts.len(), "{:?}", stats.failed);
    for (id, hash, want, color) in items {
        assert!(!original_file(&lib, &hash, "png").exists());
        let (s, _, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
        assert_eq!(s, StatusCode::OK);
        let px = archive_png::decode(&body).unwrap();
        assert_eq!(px.color, color);
        assert_eq!(archive_png::digest(&px), want, "{color:?}");
    }
}

#[test]
fn a_png_whose_jxl_does_not_decode_to_the_recorded_pixels_keeps_its_original() {
    let (tmp, lib, _app) = setup();
    let png = png_of(PngColor::Rgb8, 40, 30, 7, None);
    let (_, hash) = add(&tmp, &lib, "u.png", "phone-ios", &png);
    let mut o = opts(false);
    o.manifest = Some(tmp.path().join("m/manifest.jsonl"));
    assert_eq!(archive_jxl::apply(&lib, &o).unwrap().converted, 1);

    // Damage 1: bytes flipped in the JXL (its recorded hash no longer matches).
    let jxl = archive_jxl::jxl_path(&lib.cfg, &hash);
    let good = std::fs::read(&jxl).unwrap();
    let mut bad = good.clone();
    let mid = bad.len() / 2;
    for b in &mut bad[mid..mid + 8] {
        *b ^= 0xA5;
    }
    std::fs::write(&jxl, &bad).unwrap();
    let mut del = opts(true);
    del.manifest = o.manifest.clone();
    let stats = archive_jxl::apply(&lib, &del).unwrap();
    assert_eq!(stats.originals_deleted, 0);
    assert_eq!(
        stats.failed.values().sum::<usize>(),
        1,
        "{:?}",
        stats.failed
    );
    assert_eq!(
        std::fs::read(original_file(&lib, &hash, "png")).unwrap(),
        png
    );
    assert!(row_of(&lib, &hash).original_deleted_at.is_none());

    // Damage 2: a valid JXL of another picture, with the catalog row made to agree with its
    // bytes. Only the pixel digest can tell.
    let other = png_of(PngColor::Rgb8, 40, 30, 99, None);
    let other_jxl = archive_png::png_roundtrip(&other).unwrap().converted.jxl;
    std::fs::write(&jxl, &other_jxl).unwrap();
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE archive_blobs SET jxl_hash = ?2, jxl_bytes = ?3 WHERE hash = ?1",
                rusqlite::params![
                    hash,
                    archive_jxl::blake3_hex(&other_jxl),
                    other_jxl.len() as i64
                ],
            )
        })
        .unwrap();
    let stats = archive_jxl::apply(&lib, &del).unwrap();
    assert_eq!(stats.originals_deleted, 0);
    assert_eq!(stats.failed.get("roundtrip_mismatch"), Some(&1));
    assert_eq!(
        std::fs::read(original_file(&lib, &hash, "png")).unwrap(),
        png
    );
    assert!(archive_jxl::materialize(&lib.cfg, &lib.db, &hash).is_none());

    // Both refusals are listed, with the original's hash and no verified mark.
    let failures = manifest_lines(&tmp.path().join("m/manifest.jsonl.failures.jsonl"));
    assert_eq!(failures.len(), 2);
    for f in &failures {
        assert_eq!(f["verified"], false);
        assert_eq!(f["original_blake3"], hash);
        assert_eq!(f["original_deleted_at"], serde_json::Value::Null);
        assert!(f["error"]
            .as_str()
            .unwrap()
            .starts_with("roundtrip_mismatch"));
    }
}

#[tokio::test]
async fn a_damaged_jxl_of_a_deleted_png_is_never_served() {
    let (tmp, lib, app) = setup();
    let png = png_of(PngColor::Rgba8, 32, 32, 3, None);
    let (id, hash) = add(&tmp, &lib, "v.png", "phone-ios", &png);
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    let jxl = archive_jxl::jxl_path(&lib.cfg, &hash);
    let mut data = std::fs::read(&jxl).unwrap();
    let mid = data.len() / 2;
    data[mid] ^= 0xFF;
    std::fs::write(&jxl, &data).unwrap();
    let _ = std::fs::remove_dir_all(archive_jxl::cache_dir(&lib.cfg));
    let (s, _, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.len() < 200);
}

#[test]
fn pngs_the_archive_cannot_keep_exactly_are_left_alone_and_listed() {
    let (tmp, lib, _app) = setup();
    let adobe = png_of(PngColor::Rgb8, 20, 20, 1, Some(adobe_profile()));
    let (_, adobe_hash) = add(&tmp, &lib, "w.png", "phone-ios", &adobe);
    // A palette PNG (colour type 3) written by hand: IHDR, PLTE, one IDAT of zeros, IEND.
    let palette = {
        let img = image::GrayImage::from_fn(8, 8, |x, y| image::Luma([(x + y) as u8 % 4]));
        let mut out = Vec::new();
        let enc = png_encoder_indexed(&mut out, 8, 8);
        enc.write_header()
            .unwrap()
            .write_image_data(img.as_raw())
            .unwrap();
        out
    };
    let (_, pal_hash) = add(&tmp, &lib, "x.png", "phone-ios", &palette);
    let mut o = opts(true);
    o.manifest = Some(tmp.path().join("fail.jsonl"));
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!((stats.converted, stats.originals_deleted), (0, 0));
    assert_eq!(stats.failed.get("unsupported_icc_profile"), Some(&1));
    assert_eq!(stats.failed.get("unsupported_pixel_format"), Some(&1));
    assert_eq!(archive_rows(&lib), 0);
    assert!(original_file(&lib, &adobe_hash, "png").is_file());
    assert!(original_file(&lib, &pal_hash, "png").is_file());
    assert_eq!(
        manifest_lines(&tmp.path().join("fail.jsonl.failures.jsonl")).len(),
        2
    );
}

/// A palette-PNG encoder from the png crate (the `image` crate has no indexed writer).
fn png_encoder_indexed(out: &mut Vec<u8>, w: u32, h: u32) -> png::Encoder<'_, &mut Vec<u8>> {
    let mut enc = png::Encoder::new(out, w, h);
    enc.set_color(png::ColorType::Indexed);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_palette(vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
    enc
}

#[test]
fn kinds_choose_what_apply_touches() {
    let (tmp, lib, _app) = setup();
    let (_, jhash) = add(&tmp, &lib, "j.jpg", "phone-archive", &jpeg(121));
    let png = png_of(PngColor::Rgb8, 24, 24, 4, None);
    let (_, phash) = add(&tmp, &lib, "k.png", "phone-ios", &png);
    // TIFF-shaped and BMP files are never touched (even if present as blobs).
    let (_, bhash) = add(&tmp, &lib, "b.bmp", "phone-ios", &{
        let mut v = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4))
            .write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Bmp)
            .unwrap();
        v
    });

    let mut o = opts(true);
    o.kinds = kinds(true, false);
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(stats.converted_by_kind.get("jpeg"), Some(&1));
    assert_eq!(stats.converted_by_kind.get("png"), None);
    assert_eq!(stats.candidates, 1);
    assert_eq!(stats.pixel_exact_left_alone, 2, "the PNG and the BMP");
    assert!(!original_file(&lib, &jhash, "jpg").exists());
    assert!(original_file(&lib, &phash, "png").is_file());
    assert_eq!(archive_rows(&lib), 1);

    o.kinds = kinds(false, true);
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(stats.converted_by_kind.get("png"), Some(&1));
    assert_eq!(stats.candidates, 1);
    assert_eq!(stats.pixel_exact_left_alone, 1, "the BMP");
    assert!(!original_file(&lib, &phash, "png").exists());
    assert!(original_file(&lib, &bhash, "bmp").is_file());
    assert_eq!(archive_rows(&lib), 2);

    // A PNG that is named .jpg is a PNG: `--kinds jpeg` leaves it, `--kinds png` takes it.
    let hidden = png_of(PngColor::Rgb8, 20, 20, 8, None);
    let (_, hh) = add(&tmp, &lib, "sneaky.jpg", "phone-ios", &hidden);
    o.kinds = kinds(true, false);
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!((stats.converted, stats.not_selected), (0, 1));
    assert!(original_file(&lib, &hh, "jpg").is_file());

    assert_eq!(
        Kinds::parse(&["jpeg,png".into()]).unwrap(),
        kinds(true, true)
    );
    assert_eq!(Kinds::parse(&["PNG".into()]).unwrap(), kinds(false, true));
    assert_eq!(
        Kinds::parse(&["jpeg".into(), "png".into()]).unwrap(),
        kinds(true, true)
    );
    for bad in ["heic", "raw", "tiff", ""] {
        assert!(Kinds::parse(&[bad.into()]).is_err(), "{bad}");
    }
}

#[test]
fn heic_and_the_rest_are_never_touched() {
    let (tmp, lib, _app) = setup();
    let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic not a real heic".to_vec();
    let (_, hhash) = add(&tmp, &lib, "h.heic", "phone-ios", &heic);
    let (_, ghash) = add(
        &tmp,
        &lib,
        "g.gif",
        "phone-ios",
        b"GIF89a\x01\0\x01\0\0\0\0;",
    );
    let (_, jhash) = add(&tmp, &lib, "j.jpg", "phone-ios", &jpeg(131));
    let manifest = tmp.path().join("m.jsonl");
    let mut o = opts(true);
    o.manifest = Some(manifest.clone());
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!((stats.candidates, stats.converted), (1, 1));
    assert!(stats.failed.is_empty());
    assert_eq!(
        std::fs::read(original_file(&lib, &hhash, "heic")).unwrap(),
        heic
    );
    assert!(original_file(&lib, &ghash, "gif").is_file());
    assert!(!original_file(&lib, &jhash, "jpg").exists());
    assert_eq!(archive_rows(&lib), 1);
    let lines = manifest_lines(&manifest);
    assert_eq!(lines.len(), 1, "the HEIC and GIF have no line");
    assert_eq!(lines[0]["original_blake3"], jhash);
    // Even a HEIC file that is named .jpg is refused and listed, never converted.
    let (_, sneaky) = add(
        &tmp,
        &lib,
        "s.jpg",
        "phone-ios",
        &heic.iter().chain(b"x").copied().collect::<Vec<_>>(),
    );
    let stats = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(stats.converted, 0);
    assert_eq!(stats.failed.get("jpeg_unsupported"), Some(&1));
    assert!(original_file(&lib, &sneaky, "jpg").is_file());
}

#[test]
fn the_manifest_lists_every_file_and_the_export_matches_the_catalog() {
    let (tmp, lib, _app) = setup();
    let jpg = jpeg(141);
    let png = png_of(PngColor::Rgba8, 36, 28, 6, Some(p3_profile()));
    let (jid, jhash) = add(&tmp, &lib, "m1.jpg", "phone-ios", &jpg);
    let (pid, phash) = add(&tmp, &lib, "m2.png", "phone-ios", &png);
    // A stored object that is not what the catalog says: a failure, original kept.
    let (bid, bhash) = add(&tmp, &lib, "m3.jpg", "phone-archive", &jpeg(142));
    std::fs::write(original_file(&lib, &bhash, "jpg"), jpeg(143)).unwrap();
    // A second item on the PNG's bytes: its own line, same JXL.
    lib.db
        .with(|c| {
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, device, asset_id, added_at, updated_at)
                 VALUES ('twin', 'photo', 'twin', ?1, 'image/png', 1, 'now', 'upload', 'dev2', 'dev2/copy', 'z', 'z')",
                [&phash],
            )
        })
        .unwrap();

    let manifest = tmp.path().join("run").join("manifest.jsonl");
    let mut o = opts(true);
    o.manifest = Some(manifest.clone());
    o.limit = Some(1);
    // Batches append to the same manifest; a failure does not use up the batch.
    let first = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(first.converted, 1);
    let second = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(second.converted, 1);
    let lines = manifest_lines(&manifest);
    let fails = manifest_lines(&tmp.path().join("run/manifest.jsonl.failures.jsonl"));
    assert_eq!(first.manifest_lines + second.manifest_lines, lines.len());
    assert_eq!(fails.len(), 2, "the bad object failed in both batches");
    assert!(fails
        .iter()
        .all(|f| f["item_id"] == bid && f["verified"] == false));
    assert!(original_file(&lib, &bhash, "jpg").is_file());

    let ok: Vec<_> = lines.iter().filter(|l| l["verified"] == true).collect();
    assert_eq!(ok.len(), 3, "jpeg + png + the twin: {lines:#?}");
    let j = ok.iter().find(|l| l["item_id"] == jid).unwrap();
    assert_eq!(j["kind"], "jpeg");
    assert_eq!(j["device"], "dev1");
    assert_eq!(j["asset_id"], "dev1/m1.jpg");
    assert_eq!(
        j["original_rel_path"],
        format!("objects/{}/{}/{jhash}.jpg", &jhash[0..2], &jhash[2..4])
    );
    assert_eq!(j["original_bytes"], jpg.len());
    assert_eq!(j["original_blake3"], jhash);
    assert_eq!(j["original_sha256"], sha256(&jpg));
    assert_eq!(
        j["jxl_rel_path"],
        format!("archive-jxl/{}/{}/{jhash}.jxl", &jhash[0..2], &jhash[2..4])
    );
    let jxl_bytes = std::fs::read(archive_jxl::jxl_path(&lib.cfg, &jhash)).unwrap();
    assert_eq!(j["jxl_bytes"], jxl_bytes.len());
    assert_eq!(j["jxl_blake3"], archive_jxl::blake3_hex(&jxl_bytes));
    assert_eq!(j["verified"], true);
    assert!(j["verified_at"].is_string() && j["original_deleted_at"].is_string());
    assert!(j.get("error").is_none());
    let p = ok.iter().find(|l| l["item_id"] == pid).unwrap();
    assert_eq!(p["kind"], "png");
    assert_eq!(p["original_sha256"], sha256(&png));
    assert_eq!(p["pixel_sha256"], digest_of(&png));
    assert!(ok
        .iter()
        .any(|l| l["item_id"] == "twin" && l["device"] == "dev2"));
    // No absolute host path anywhere.
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(!text.contains(&tmp.path().to_string_lossy().to_string()));
    assert!(!text.contains("\"/"), "paths are relative to the store");

    // The export is the catalog's view: both archived blobs, one line per item, read-only.
    let export = tmp.path().join("export.jsonl");
    let n = lucina_media::archive_manifest::export(&lib.cfg, &export).unwrap();
    let exported = manifest_lines(&export);
    assert_eq!((n, exported.len()), (3, 3));
    assert!(exported
        .iter()
        .all(|l| l["verified"] == true && l["original_deleted_at"].is_string()));
    for line in &ok {
        let same = exported
            .iter()
            .find(|e| e["item_id"] == line["item_id"])
            .expect("the run's line is in the export");
        for key in [
            "kind",
            "original_rel_path",
            "original_blake3",
            "original_sha256",
            "jxl_blake3",
            "jxl_bytes",
            "device",
            "asset_id",
        ] {
            assert_eq!(same[key], line[key], "{key}");
        }
    }
    assert_eq!(archive_rows(&lib), 2);
    // Exporting again replaces the file with the same text.
    lucina_media::archive_manifest::export(&lib.cfg, &export).unwrap();
    assert_eq!(std::fs::read_to_string(&export).unwrap().lines().count(), 3);
}

#[test]
fn the_temperature_limit_pauses_until_the_cpu_is_cool() {
    use lucina_media::archive_jxl_measure::wait_for_cool_with;
    let temps = std::cell::RefCell::new(vec![Some(95.0), Some(91.0), Some(90.0), Some(89.9)]);
    let sleeps = std::cell::Cell::new(0);
    let pauses = wait_for_cool_with(
        90,
        || {
            let mut t = temps.borrow_mut();
            t.remove(0)
        },
        || sleeps.set(sleeps.get() + 1),
    );
    assert_eq!((pauses, sleeps.get()), (3, 3), "at 90 and above it waits");
    // No sensor: nothing to wait for.
    assert_eq!(wait_for_cool_with(90, || None, || panic!("no pause")), 0);
}

#[test]
fn batches_with_a_limit_cover_jpeg_and_png() {
    let (tmp, lib, _app) = setup();
    for i in 0..2 {
        add(
            &tmp,
            &lib,
            &format!("a{i}.jpg"),
            "phone-archive",
            &jpeg(150 + i),
        );
        add(
            &tmp,
            &lib,
            &format!("a{i}.png"),
            "phone-ios",
            &png_of(PngColor::Rgb8, 16, 16, i, None),
        );
    }
    let manifest = tmp.path().join("b.jsonl");
    let mut o = opts(true);
    o.limit = Some(3);
    o.manifest = Some(manifest.clone());
    let a = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!((a.converted, a.originals_deleted, a.candidates), (3, 3, 4));
    let b = archive_jxl::apply(&lib, &o).unwrap();
    assert_eq!(
        (b.converted, b.originals_deleted, b.already_archived),
        (1, 1, 3)
    );
    assert_eq!(manifest_lines(&manifest).len(), 4);
    assert_eq!(archive_rows(&lib), 4);
}

#[tokio::test]
async fn a_deleted_png_is_not_restorable_and_stays_served_from_its_jxl() {
    let (tmp, lib, app) = setup();
    let png = png_of(PngColor::Rgb8, 20, 20, 2, None);
    let (id, hash) = add(&tmp, &lib, "r.png", "phone-ios", &png);
    let (_, jh) = add(&tmp, &lib, "r.jpg", "phone-ios", &jpeg(160));
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    let r = archive_jxl::restore_originals(&lib).unwrap();
    assert_eq!((r.restored, r.png_not_restorable), (1, 1));
    assert!(original_file(&lib, &jh, "jpg").is_file());
    assert!(!original_file(&lib, &hash, "png").exists());
    assert!(row_of(&lib, &hash).original_deleted_at.is_some());
    let (s, _, body) = get(&app, &format!("/v1/media/items/{id}/original"), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(digest_of(&body), digest_of(&png));
}

#[test]
fn purging_a_png_takes_its_rebuilt_cache_file_too() {
    let (tmp, lib, _app) = setup();
    let (id, hash) = add(
        &tmp,
        &lib,
        "p.png",
        "phone-archive",
        &png_of(PngColor::Rgb8, 18, 18, 5, None),
    );
    archive_jxl::apply(&lib, &opts(true)).unwrap();
    let cached = archive_jxl::materialize(&lib.cfg, &lib.db, &hash).unwrap();
    assert!(cached.is_file() && cached.extension().unwrap() == "png");
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
    trash::finish_purge(&lib.cfg, &report);
    assert!(!cached.exists());
    assert!(!archive_jxl::jxl_path(&lib.cfg, &hash).exists());
    assert_eq!(archive_rows(&lib), 0);
}

#[test]
fn the_migration_widens_the_first_releases_table_and_keeps_its_rows() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE archive_blobs (
            hash TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('jpeg_recompress')),
            jxl_hash TEXT NOT NULL,
            jxl_bytes INTEGER NOT NULL,
            reconstructs_to TEXT NOT NULL,
            original_bytes INTEGER NOT NULL,
            original_ext TEXT NOT NULL,
            verified_at TEXT NOT NULL,
            original_deleted_at TEXT
        );
        INSERT INTO archive_blobs VALUES ('h1','jpeg_recompress','j',5,'h1',10,'jpg','t0','t1');",
    )
    .unwrap();
    archive_jxl::migrate(&c).unwrap();
    archive_jxl::migrate(&c).unwrap();
    let row = archive_jxl::archive_row(&c, "h1").unwrap().unwrap();
    assert_eq!(
        (
            row.jxl_bytes,
            row.original_deleted_at.as_deref(),
            row.original_sha256.as_deref()
        ),
        (5, Some("t1"), None)
    );
    c.execute(
        "INSERT INTO archive_blobs (hash, kind, jxl_hash, jxl_bytes, reconstructs_to, original_bytes, original_ext, verified_at, pixel_digest, png_color, icc)
         VALUES ('h2','png_lossless','j',5,'d',10,'png','t0','d','rgb8',x'0102')",
        [],
    )
    .unwrap();
    assert_eq!(
        archive_jxl::archive_row(&c, "h2").unwrap().unwrap().icc,
        Some(vec![1, 2])
    );
    assert!(c
        .execute(
            "INSERT INTO archive_blobs (hash, kind, jxl_hash, jxl_bytes, reconstructs_to, original_bytes, original_ext, verified_at) VALUES ('h3','heic','j',1,'x',1,'heic','t')",
            [],
        )
        .is_err());
}
