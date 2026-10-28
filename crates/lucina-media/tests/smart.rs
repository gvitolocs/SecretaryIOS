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
use lucina_media::smart::{ModelChoice, PreviousModel, SmartConfig};
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

#[test]
fn purging_an_item_forgets_its_vectors() {
    let env = setup();
    let id = add(&env, "gone", [200, 30, 30], "2024-05-01T10:00:00Z");
    let kept = add(&env, "kept", [30, 30, 200], "2024-05-02T10:00:00Z");
    let count = |item: &str| -> i64 {
        env.lib
            .db
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM ml_embeddings WHERE item_id = ?1",
                    [item],
                    |r| r.get(0),
                )
            })
            .unwrap()
    };
    env.lib
        .db
        .with(|c| {
            for item in [&id, &kept] {
                c.execute(
                    "INSERT INTO ml_embeddings (item_id, model, dim, vec, created_at) VALUES (?1, 'test', 2, x'00000000', '2024-05-03T00:00:00Z')",
                    [item],
                )?;
            }
            c.execute(
                "UPDATE items SET trashed_at = '2024-05-03T00:00:00Z' WHERE id = ?1",
                [&id],
            )?;
            lucina_media::trash::purge_in(c, std::slice::from_ref(&id))?;
            Ok(())
        })
        .unwrap();
    assert_eq!(count(&id), 0, "the purged item has no vectors left");
    assert_eq!(count(&kept), 1, "other items keep theirs");
}

// ---- model change: catch-up, per-model rules

/// A fake model with its own name, query rules, score scale and default minimums. Pictures are
/// their mean colour, like `Colours`; the words are "red", "green" and "blue" (spelt "Red",
/// "Green", "Blue" when `exact_case`, and anything else fails).
struct Tone {
    name: &'static str,
    exact_case: bool,
    scale: f32,
    text_min: f32,
    similar_min: f32,
    released: std::sync::atomic::AtomicUsize,
}

impl Tone {
    fn old() -> Arc<Tone> {
        Arc::new(Tone {
            name: "old-test",
            exact_case: false,
            scale: 1.0,
            text_min: 0.5,
            similar_min: 0.9,
            released: Default::default(),
        })
    }
    /// Halves every score, keeps the case of a query and has lower default minimums.
    fn new_model() -> Arc<Tone> {
        Arc::new(Tone {
            name: "new-test",
            exact_case: true,
            scale: 0.5,
            text_min: 0.3,
            similar_min: 0.8,
            released: Default::default(),
        })
    }
    fn released(&self) -> usize {
        self.released.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Embedder for Tone {
    fn model(&self) -> &str {
        self.name
    }
    fn dim(&self) -> usize {
        3
    }
    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Colours.embed_images(images)
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
        match (self.exact_case, text) {
            (true, "Red") => Colours.embed_text("red"),
            (true, "Green") => Colours.embed_text("green"),
            (true, "Blue") => Colours.embed_text("blue"),
            (true, _) => Err(MlError::Failed("this model wants the case as typed".into())),
            (false, word) => Colours.embed_text(word),
        }
    }
    fn relevance(&self, cosine: f32) -> f32 {
        cosine * self.scale
    }
    fn normalize_query(&self, text: &str) -> String {
        if self.exact_case {
            text.trim().to_string()
        } else {
            text.trim().to_lowercase()
        }
    }
    fn default_text_min(&self) -> f32 {
        self.text_min
    }
    fn default_similar_min(&self) -> f32 {
        self.similar_min
    }
    fn release_idle(&self, _idle: std::time::Duration, _vision: bool) {
        self.released
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Ten photos: two reds, a pink, a green, a blue and five more reds, newest last.
fn ten(env: &Env) -> Vec<String> {
    let rgbs: [[u8; 3]; 10] = [
        [250, 10, 10],
        [240, 90, 90],
        [10, 240, 10],
        [10, 10, 250],
        [230, 20, 20],
        [235, 25, 25],
        [245, 15, 15],
        [225, 30, 30],
        [238, 12, 12],
        [248, 8, 8],
    ];
    rgbs.iter()
        .enumerate()
        .map(|(i, rgb)| {
            add(
                env,
                &format!("p{i}"),
                *rgb,
                &format!("2024-01-{:02}T10:00:00Z", i + 1),
            )
        })
        .collect()
}

fn scores(v: &Value) -> Vec<f64> {
    v["scores"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

fn previous(e: &Arc<Tone>) -> Option<PreviousModel> {
    Some(PreviousModel {
        embedder: e.clone(),
        text_min: e.text_min,
        similar_min: e.similar_min,
    })
}

#[tokio::test]
async fn search_is_served_by_the_previous_model_until_the_new_one_has_caught_up() {
    let env = setup();
    let all = ten(&env);
    let old = Tone::old();
    let s_old = smart::install(&env.lib, old.clone(), 0.5, 0.9).unwrap();
    assert_eq!(embed_all(&env, &s_old), 10);
    drop(s_old);
    let (_, before) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(before["total"], 8, "everything but the blue and green ones");

    // The configured model changes: nothing is embedded with it yet.
    let new = Tone::new_model();
    let s = smart::install_with(&env.lib, new.clone(), 0.3, 0.8, previous(&old)).unwrap();
    assert!(s.catching_up());
    assert_eq!(s.serving_model(), "old-test");
    let (st, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["model"], "new-test");
    assert_eq!(v["serving_model"], "old-test");
    assert_eq!(v["catching_up"], true);
    assert_eq!(v["indexed"], 0, "the configured model's own index");
    assert_eq!(v["indexed_by_model"]["old-test"], 10);
    assert_eq!(v["catch_up"]["from"], "old-test");
    assert_eq!(v["catch_up"]["to"], "new-test");
    assert_eq!(v["catch_up"]["remaining"], 10);
    assert_eq!(v["catch_up"]["progress"], 0.0);

    // Search answers exactly as before: the old index, the old embedder's scores and rules
    // ("Red" is lower-cased for it, the new model would refuse the lower-case word).
    let (st, v) = get(&env.app, "/v1/media/items/search?q=Red").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 8);
    assert_eq!(ids(&v), ids(&before));
    assert_eq!(scores(&v), scores(&before));
    assert_eq!(v["status"]["serving_model"], "old-test");
    let (st, v) = get(&env.app, &format!("/v1/media/items/{}/similar", all[0])).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["total"].as_u64().unwrap() >= 5);

    // One batch of eight: still catching up, two to go.
    assert_eq!(smart::embed_pending(&env.lib, &s).unwrap(), 8);
    smart::refresh_counts(&env.lib, &s);
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["serving_model"], "old-test");
    assert_eq!(v["catch_up"]["remaining"], 2);
    assert_eq!(v["catch_up"]["total"], 10);
    assert_eq!(v["catch_up"]["progress"], 0.8);
    assert_eq!(v["indexed"], 8);
    assert_eq!(v["indexed_by_model"]["new-test"], 8);
    assert_eq!(v["indexed_by_model"]["old-test"], 10);
    let (_, still) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(scores(&still), scores(&before));
    // Both models are asked to give memory back while both are in use.
    s.release_idle(std::time::Duration::ZERO, true);
    assert_eq!((old.released(), new.released()), (1, 1));

    // The rest: the new model serves and the old one is dropped from memory.
    assert_eq!(smart::embed_pending(&env.lib, &s).unwrap(), 2);
    smart::refresh_counts(&env.lib, &s);
    assert!(!s.catching_up());
    assert_eq!(s.serving_model(), "new-test");
    assert_eq!(
        Arc::strong_count(&old),
        1,
        "nothing keeps the previous embedder alive"
    );
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["serving_model"], "new-test");
    assert_eq!(v["catching_up"], false);
    assert_eq!(v["catch_up"], Value::Null);
    assert_eq!(v["indexed"], 10);
    // The old vectors stay stored (a rollback needs them).
    assert_eq!(v["indexed_by_model"]["old-test"], 10);
    assert_eq!(v["indexed_by_model"]["new-test"], 10);

    // Now the new model's own rules and scale: the case as typed, half the old score.
    let (st, v) = get(&env.app, "/v1/media/items/search?q=Red").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 8);
    for (a, b) in scores(&v).iter().zip(scores(&before)) {
        assert!((a - b / 2.0).abs() < 2e-3, "{a} vs {b}");
    }
    let (st, _) = get(&env.app, "/v1/media/items/search?q=red").await;
    assert_eq!(
        st,
        StatusCode::SERVICE_UNAVAILABLE,
        "this fake refuses 'red'"
    );
    s.release_idle(std::time::Duration::ZERO, true);
    assert_eq!((old.released(), new.released()), (1, 2));
}

#[tokio::test]
async fn photos_the_new_model_cannot_embed_or_the_old_one_never_had_do_not_hold_up_the_switch() {
    let env = setup();
    let _ = ten(&env);
    let old = Tone::old();
    let s_old = smart::install(&env.lib, old.clone(), 0.5, 0.9).unwrap();
    embed_all(&env, &s_old);
    // A photo added after the switch of models: the old model never saw it.
    let fresh = add(&env, "fresh", [250, 5, 5], "2024-03-01T10:00:00Z");
    // A rendition that went missing: the new model records a failure.
    let lost = add(&env, "lost", [5, 250, 5], "2024-03-02T10:00:00Z");
    let dir = thumbs::dir_for(&env.lib.cfg, &lost);
    for f in ["s.jpg", "l.jpg"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    env.lib
        .db
        .with(|c| {
            c.execute(
                "INSERT INTO ml_embeddings (item_id, model, dim, vec, created_at) VALUES (?1, 'old-test', 3, x'000000000000000000000000', ?2)",
                params![lost, lucina_media::db::now_str()],
            )
        })
        .unwrap();
    let new = Tone::new_model();
    let s = smart::install_with(&env.lib, new.clone(), 0.3, 0.8, previous(&old)).unwrap();
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(
        v["catch_up"]["remaining"], 11,
        "the ten the old model had, plus the one it embedded without a picture to show"
    );
    // New model: newest first, so the two newest go first.
    let mut done = 0;
    loop {
        let n = smart::embed_pending(&env.lib, &s).unwrap();
        if n == 0 {
            break;
        }
        done += n;
    }
    assert_eq!(done, 12, "all twelve are handled, one of them as a failure");
    smart::refresh_counts(&env.lib, &s);
    assert!(!s.catching_up(), "a failed photo does not wait for ever");
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["failed"], 1);
    assert_eq!(v["indexed"], 11);
    let (_, v) = get(&env.app, "/v1/media/items/search?q=Red").await;
    assert!(ids(&v).contains(&fresh));
}

#[tokio::test]
async fn nothing_to_hand_over_means_no_catch_up() {
    let env = setup();
    let _ = ten(&env);
    let old = Tone::old();
    let s_old = smart::install(&env.lib, old.clone(), 0.5, 0.9).unwrap();
    embed_all(&env, &s_old);
    let new = Tone::new_model();
    let s = smart::install_with(&env.lib, new.clone(), 0.3, 0.8, previous(&old)).unwrap();
    assert!(s.catching_up());
    embed_all(&env, &s);
    smart::refresh_counts(&env.lib, &s);
    assert!(!s.catching_up());

    // Rolling back is setting the model back: the old vectors are all still there, so the old
    // model serves at once and the new model's vectors wait for the next change.
    let s = smart::install_with(&env.lib, old.clone(), 0.5, 0.9, previous(&new)).unwrap();
    assert!(!s.catching_up());
    assert_eq!(s.serving_model(), "old-test");
    let (_, v) = get(&env.app, "/v1/media/embeddings/status").await;
    assert_eq!(v["serving_model"], "old-test");
    assert_eq!(v["indexed"], 10);
    assert_eq!(v["catch_up"], Value::Null);

    // A previous model that is the configured one is no previous model, and a fresh catalog has none.
    let s = smart::install_with(&env.lib, old.clone(), 0.5, 0.9, previous(&old)).unwrap();
    assert!(!s.catching_up());
    let fresh = setup();
    let _ = ten(&fresh);
    let s = smart::install_with(&fresh.lib, new.clone(), 0.3, 0.8, previous(&old)).unwrap();
    assert!(!s.catching_up(), "the old model had no vectors here");
}

#[tokio::test]
async fn similar_waits_for_the_serving_models_own_vector() {
    let env = setup();
    let all = ten(&env);
    let old = Tone::old();
    let s_old = smart::install(&env.lib, old.clone(), 0.5, 0.9).unwrap();
    embed_all(&env, &s_old);
    let fresh = add(&env, "fresh", [250, 5, 5], "2024-03-01T10:00:00Z");
    let new = Tone::new_model();
    let s = smart::install_with(&env.lib, new.clone(), 0.3, 0.8, previous(&old)).unwrap();
    // The new model embeds the newest photo first; the old index has never seen it.
    assert_eq!(smart::embed_pending(&env.lib, &s).unwrap(), 8);
    let (st, _) = get(&env.app, &format!("/v1/media/items/{fresh}/similar")).await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "the serving index has no vector of it yet"
    );
    let (st, _) = get(&env.app, &format!("/v1/media/items/{}/similar", all[0])).await;
    assert_eq!(st, StatusCode::OK);
    while smart::embed_pending(&env.lib, &s).unwrap() > 0 {}
    smart::refresh_counts(&env.lib, &s);
    let (st, _) = get(&env.app, &format!("/v1/media/items/{fresh}/similar")).await;
    assert_eq!(st, StatusCode::OK);
}

#[test]
fn the_model_comes_from_the_environment_and_defaults_to_gemma() {
    let cfg = |pairs: &'static [(&'static str, &'static str)]| {
        SmartConfig::from_lookup(&|k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        })
    };
    assert_eq!(cfg(&[]).model, ModelChoice::Gemma);
    assert!(!cfg(&[]).enabled);
    assert!(cfg(&[("LUCINA_MEDIA_ML", "1")]).enabled);
    for gemma in [
        "embeddinggemma2",
        "EmbeddingGemma2",
        " embeddinggemma-2 ",
        "gemma",
    ] {
        assert_eq!(
            ModelChoice::parse(gemma),
            Some(ModelChoice::Gemma),
            "{gemma}"
        );
    }
    for sig in ["siglip2", "SigLIP2", "siglip-2", " siglip "] {
        assert_eq!(ModelChoice::parse(sig), Some(ModelChoice::Siglip), "{sig}");
    }
    assert_eq!(ModelChoice::parse("clip"), None);
    assert_eq!(
        cfg(&[("LUCINA_MEDIA_ML_MODEL", "siglip2")]).model,
        ModelChoice::Siglip
    );
    assert_eq!(
        cfg(&[("LUCINA_MEDIA_ML_MODEL", "embeddinggemma2")]).model,
        ModelChoice::Gemma
    );
    // Empty or unknown: the default, not an error.
    assert_eq!(
        cfg(&[("LUCINA_MEDIA_ML_MODEL", "")]).model,
        ModelChoice::Gemma
    );
    assert_eq!(
        cfg(&[("LUCINA_MEDIA_ML_MODEL", "clip-vit")]).model,
        ModelChoice::Gemma
    );
    assert_eq!(ModelChoice::DEFAULT, ModelChoice::Gemma);
}

#[test]
fn the_cosine_minimums_are_the_models_own_unless_the_environment_overrides() {
    let none = SmartConfig::from_lookup(&|_| None);
    assert_eq!((none.text_min, none.similar_min), (None, None));
    let old = Tone::old();
    let new = Tone::new_model();
    assert_eq!(none.minimums_for(&*old), (0.5, 0.9));
    assert_eq!(none.minimums_for(&*new), (0.3, 0.8));
    let set = SmartConfig::from_lookup(&|k| match k {
        "LUCINA_MEDIA_ML_TEXT_MIN" => Some("0.42".into()),
        "LUCINA_MEDIA_ML_SIMILAR_MIN" => Some("nonsense".into()),
        _ => None,
    });
    assert_eq!(set.minimums_for(&*old), (0.42, 0.9));
    assert_eq!(set.minimums_for(&*new), (0.42, 0.8));
}

#[test]
fn stored_model_ids_find_their_embedder() {
    let dir = tempfile::tempdir().unwrap();
    let siglip = lucina_media_ml::siglip::SiglipConfig {
        ort_lib: dir.path().join("none.so"),
        model_dir: dir.path().to_path_buf(),
        threads: 1,
    };
    let gemma = lucina_media_ml::gemma::GemmaConfig {
        ort_lib: dir.path().join("none.so"),
        model_dir: dir.path().to_path_buf(),
        threads: 1,
        budget: 70,
    };
    for id in [
        lucina_media_ml::siglip::MODEL.to_string(),
        lucina_media_ml::gemma::model_id(70),
        lucina_media_ml::gemma::model_id(140),
    ] {
        // Known ids are recognised; here their files are missing, which is "unavailable".
        match smart::load_embedder(&id, &siglip, &gemma) {
            Err(MlError::Unavailable(e)) => assert!(e.contains("not found"), "{id}: {e}"),
            _ => panic!("{id}: expected unavailable"),
        }
    }
    match smart::load_embedder("clip-vit-b32", &siglip, &gemma) {
        Err(MlError::Unavailable(e)) => assert!(e.contains("no embedder"), "{e}"),
        _ => panic!("an unknown model has no embedder"),
    }
}
