//! Smart search end to end through the HTTP router, with a fake embedder (a picture's vector is
//! its mean colour; "red", "green" and "blue" are the query words) and synthetic pictures only:
//! the worker's batches, failures and re-embedding after a change, the status route, ranked
//! search with the listing's filters and paging, similar photos, and 503 while it is off.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use image::RgbImage;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, smart, store, thumbs};
use lucina_media_ml::{Embedder, MlError};
use rusqlite::params;
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

struct Colours;

impl Embedder for Colours {
    fn model(&self) -> &str {
        "colours-test"
    }
    fn dim(&self) -> usize {
        3
    }
    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(images
            .iter()
            .map(|img| {
                let n = (img.width() * img.height()) as f32;
                let mut v = [0f32; 3];
                for p in img.pixels() {
                    for c in 0..3 {
                        v[c] += p[c] as f32 / n;
                    }
                }
                let len = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / len).collect()
            })
            .collect())
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
        Ok(match text {
            "red" => vec![1.0, 0.0, 0.0],
            "green" => vec![0.0, 1.0, 0.0],
            "blue" => vec![0.0, 0.0, 1.0],
            _ => return Err(MlError::Failed("unknown word".into())),
        })
    }
    fn relevance(&self, cosine: f32) -> f32 {
        cosine
    }
}

struct Env {
    tmp: tempfile::TempDir,
    lib: Library,
    app: Router,
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None);
    Env { tmp, lib, app }
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .uri(uri)
        .header("x-media-upstream-key", KEY)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn png(rgb: [u8; 3]) -> Vec<u8> {
    let img = RgbImage::from_pixel(32, 24, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// A phone photo of one colour with its renditions; `taken` orders the timeline.
fn add(env: &Env, name: &str, rgb: [u8; 3], taken: &str) -> String {
    let src = env.tmp.path().join(format!("src-{name}.png"));
    std::fs::write(&src, png(rgb)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{name}.png")),
        source: Some("phone-ios".into()),
        device: Some("ios-1".into()),
        asset_id: Some(format!("ios-1/{name}")),
        taken_at: Some(taken.into()),
        ..Default::default()
    };
    let id = ingest::ingest_file(&env.lib, &src, None, &client, None)
        .unwrap()
        .item_id;
    let (path, _) = ingest::original_path(&env.lib, &id).unwrap().unwrap();
    let state = thumbs::generate(&env.lib.cfg, &id, "photo", &path, None).unwrap();
    env.lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET thumb_state = ?2 WHERE id = ?1",
                params![id, state.as_str()],
            )
        })
        .unwrap();
    id
}

fn embed_all(env: &Env, s: &smart::Smart) -> usize {
    let mut total = 0;
    loop {
        let n = smart::embed_pending(&env.lib, s).unwrap();
        if n == 0 {
            return total;
        }
        total += n;
    }
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
async fn off_until_installed() {
    let env = setup();
    let (s, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["enabled"], false);
    let (s, _) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (s, _) = get(&env.app, "/v1/media/items/x/similar").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn search_ranks_filters_and_pages() {
    let env = setup();
    let red = add(&env, "red", [250, 10, 10], "2024-01-01T10:00:00Z");
    let pink = add(&env, "pink", [240, 90, 90], "2024-02-01T10:00:00Z");
    let green = add(&env, "green", [10, 240, 10], "2024-03-01T10:00:00Z");
    let blue = add(&env, "blue", [10, 10, 250], "2024-04-01T10:00:00Z");
    let s = smart::install(&env.lib, Arc::new(Colours), 0.5, 0.9).unwrap();
    assert_eq!(embed_all(&env, &s), 4);

    let (st, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["model"], "colours-test");
    assert_eq!(v["indexed"], 4);
    assert_eq!(v["pending"], 0);

    // By relevance, not by date; the green and blue photos are below the threshold.
    let (st, v) = get(&env.app, "/v1/media/items/search?q=Red").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), vec![red.clone(), pink.clone()]);
    let scores: Vec<f64> = v["scores"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(scores[0] > scores[1] && scores[1] >= 0.5, "{scores:?}");
    assert_eq!(v["total"], 2);
    assert_eq!(v["next_cursor"], Value::Null);
    assert_eq!(v["items"][0]["kind"], "photo");

    // Paging.
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red&limit=1").await;
    assert_eq!(ids(&v), vec![red.clone()]);
    assert_eq!(v["next_cursor"], "o1");
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red&limit=1&cursor=o1").await;
    assert_eq!(ids(&v), vec![pink.clone()]);
    assert_eq!(v["next_cursor"], Value::Null);
    let (st, _) = get(&env.app, "/v1/media/items/search?q=red&cursor=zz").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // The listing's filters: dates, favourites, the trash.
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red&from=2024-02").await;
    assert_eq!(ids(&v), vec![pink.clone()]);
    env.lib
        .db
        .with(|c| c.execute("UPDATE items SET favorite = 1 WHERE id = ?1", [&red]))
        .unwrap();
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red&favorite=true").await;
    assert_eq!(ids(&v), vec![red.clone()]);
    env.lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = '2024-05-01T00:00:00Z' WHERE id = ?1",
                [&pink],
            )
        })
        .unwrap();
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(ids(&v), vec![red.clone()]);
    let (st, _) = get(&env.app, "/v1/media/items/search?q=red&origin=nowhere").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = get(&env.app, "/v1/media/items/search?q=%20").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (_, v) = get(&env.app, "/v1/media/items/search?q=green").await;
    assert_eq!(ids(&v), vec![green.clone()]);
    let (_, v) = get(&env.app, "/v1/media/items/search?q=blue").await;
    assert_eq!(ids(&v), vec![blue.clone()]);
}

#[tokio::test]
async fn similar_leaves_the_photo_out() {
    let env = setup();
    let red = add(&env, "red", [250, 10, 10], "2024-01-01T10:00:00Z");
    let pink = add(&env, "pink", [240, 60, 60], "2024-02-01T10:00:00Z");
    let _blue = add(&env, "blue", [10, 10, 250], "2024-04-01T10:00:00Z");
    let s = smart::install(&env.lib, Arc::new(Colours), 0.5, 0.9).unwrap();
    embed_all(&env, &s);
    let (st, v) = get(&env.app, &format!("/v1/media/items/{red}/similar")).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), vec![pink.clone()]);
    let (st, _) = get(&env.app, "/v1/media/items/it_nope/similar").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn worker_records_failures_and_re_embeds_changed_items() {
    let env = setup();
    let red = add(&env, "red", [250, 10, 10], "2024-01-01T10:00:00Z");
    let lost = add(&env, "lost", [10, 250, 10], "2024-02-01T10:00:00Z");
    // A rendition gone missing: recorded once, not retried on every pass.
    let dir = thumbs::dir_for(&env.lib.cfg, &lost);
    for f in ["s.jpg", "l.jpg"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let s = smart::install(&env.lib, Arc::new(Colours), 0.5, 0.9).unwrap();
    assert_eq!(embed_all(&env, &s), 2);
    assert_eq!(smart::embed_pending(&env.lib, &s).unwrap(), 0);
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["indexed"], 1);
    assert_eq!(v["failed"], 1);
    assert_eq!(v["pending"], 0);

    // The item changed after it was embedded (an edit): it is pending again, then current.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let later = lucina_media::db::now_str();
    env.lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET updated_at = ?2 WHERE id = ?1",
                params![red, later],
            )
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["pending"], 1);
    assert_eq!(smart::embed_pending(&env.lib, &s).unwrap(), 1);

    // A restart loads the stored vectors.
    let s2 = smart::install(&env.lib, Arc::new(Colours), 0.5, 0.9).unwrap();
    assert_eq!(smart::embed_pending(&env.lib, &s2).unwrap(), 0);
    let (_, v) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(ids(&v), vec![red]);
}
