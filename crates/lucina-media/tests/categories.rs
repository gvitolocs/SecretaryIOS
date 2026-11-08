//! Automatic categories end to end through the HTTP router, with a fake embedder (a picture's
//! vector is the category its mean colour stands for, a prompt's vector is the category it
//! belongs to) and synthetic pictures only: the background step, the status, the counts and the
//! category listing with its filters and paging, the owner's corrections that no decider
//! overwrites, a second decider, a model change that decides again, the sensitive categories
//! absent from Places and the memories strips, nothing on share routes, and purges.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{categories, http, smart, store, thumbs};
use lucina_media_ml::categories::{
    Category, Decider, DeciderKind, Decision, Features, RulesDecider,
};
use lucina_media_ml::categories_store as cs;
use lucina_media_ml::{Embedder, MlError};
use rusqlite::params;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const N: usize = Category::ALL.len();

fn axis(c: Category) -> Vec<f32> {
    let mut v = vec![0.0; N];
    v[c.index()] = 1.0;
    v
}

/// A picture's vector is the category of the palette colour nearest to its mean colour; a
/// prompt's vector is the category the prompt belongs to.
struct Look {
    model: &'static str,
    palette: Vec<([u8; 3], Category)>,
}

fn look(model: &'static str, red: Category) -> Look {
    Look {
        model,
        palette: vec![
            ([250, 10, 10], red),
            ([10, 250, 10], Category::Food),
            ([10, 10, 250], Category::Pet),
            ([250, 250, 250], Category::Document),
            ([5, 5, 5], Category::Note),
            ([128, 128, 128], Category::Photo),
            ([250, 250, 10], Category::Meme),
            ([250, 10, 250], Category::Map),
            ([10, 250, 250], Category::Ticket),
        ],
    }
}

impl Embedder for Look {
    fn model(&self) -> &str {
        self.model
    }
    fn dim(&self) -> usize {
        N
    }
    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(images
            .iter()
            .map(|img| {
                let n = (img.width() * img.height()) as f32;
                let mut m = [0f32; 3];
                for p in img.pixels() {
                    for c in 0..3 {
                        m[c] += p[c] as f32 / n;
                    }
                }
                let dist = |c: &[u8; 3]| (0..3).map(|i| (m[i] - c[i] as f32).powi(2)).sum::<f32>();
                let best = self
                    .palette
                    .iter()
                    .min_by(|a, b| dist(&a.0).total_cmp(&dist(&b.0)))
                    .unwrap();
                axis(best.1)
            })
            .collect())
    }
    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
        Category::ALL
            .into_iter()
            .find(|c| c.prompts().contains(&text))
            .map(axis)
            .ok_or_else(|| MlError::Failed("unknown prompt".into()))
    }
    fn relevance(&self, cosine: f32) -> f32 {
        cosine
    }
}

/// A decider that answers one category for everything, and fails for photos that look like pets.
struct Fixed(Category);

impl Decider for Fixed {
    fn kind(&self) -> DeciderKind {
        DeciderKind::Jev
    }
    fn id(&self) -> String {
        format!("jev-test-{}", self.0.as_str())
    }
    fn decide(&self, f: &Features) -> Result<Decision, MlError> {
        if f.top_category == Category::Pet {
            return Err(MlError::Failed("the model is busy".into()));
        }
        Ok(Decision {
            category: self.0,
            confidence: 0.9,
            reason: "fixed".into(),
        })
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

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: bool,
) -> (StatusCode, Vec<u8>) {
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
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn j(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (s, b) = call(app, method, uri, body, true).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    j(app, "GET", uri, None).await
}

/// A flat picture; `seed` changes one corner pixel so that two pictures of one colour are two different files
/// (the catalog merges identical bytes).
fn picture(w: u32, h: u32, rgb: [u8; 3], fmt: ImageFormat, seed: u32) -> Vec<u8> {
    let mut img = RgbImage::from_pixel(w, h, Rgb(rgb));
    for (k, b) in seed.to_le_bytes().iter().enumerate() {
        img.put_pixel(k as u32, 0, Rgb([*b, 255 - *b, *b ^ 0x5a]));
    }
    let mut out = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(img)
        .write_to(&mut out, fmt)
        .unwrap();
    out.into_inner()
}

/// A picture to add: a 64×48 JPEG from an iPhone backup with camera data, unless said otherwise.
#[derive(Clone)]
struct Spec {
    name: &'static str,
    rgb: [u8; 3],
    size: (u32, u32),
    png: bool,
    taken: &'static str,
    source: &'static str,
    camera: Option<&'static str>,
    gps: Option<(f64, f64)>,
}

fn spec(name: &'static str, rgb: [u8; 3], taken: &'static str) -> Spec {
    Spec {
        name,
        rgb,
        size: (64, 48),
        png: false,
        taken,
        source: "phone-ios",
        camera: Some("Apple iPhone 16e"),
        gps: None,
    }
}

const RED: [u8; 3] = [250, 10, 10];
const GREEN: [u8; 3] = [10, 250, 10];
const BLUE: [u8; 3] = [10, 10, 250];
const GRAY: [u8; 3] = [128, 128, 128];
const WHITE: [u8; 3] = [250, 250, 250];
const BLACK: [u8; 3] = [5, 5, 5];

fn add(env: &Env, s: &Spec) -> String {
    let ext = if s.png { "png" } else { "jpg" };
    let fmt = if s.png {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    let src = env.tmp.path().join(format!("src-{}.{ext}", s.name));
    // FNV-1a with a final mix, so names one letter apart differ in every byte (JPEG would blur a small difference away).
    let mut seed = s.name.bytes().fold(0x811c9dc5u32, |a, b| {
        (a ^ b as u32).wrapping_mul(0x01000193)
    });
    seed ^= seed >> 15;
    seed = seed.wrapping_mul(0x2c1b3c6d);
    seed ^= seed >> 12;
    std::fs::write(&src, picture(s.size.0, s.size.1, s.rgb, fmt, seed)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{}.{ext}", s.name)),
        source: Some(s.source.into()),
        device: Some("ios-1".into()),
        asset_id: Some(format!("ios-1/{}", s.name)),
        taken_at: Some(s.taken.into()),
        lat: s.gps.map(|g| g.0),
        lon: s.gps.map(|g| g.1),
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
                "UPDATE items SET thumb_state = ?2, camera = ?3 WHERE id = ?1",
                params![id, state.as_str(), s.camera],
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

/// Runs the categories step until nothing is left (a failed photo is skipped, not retried).
fn decide_all(env: &Env, s: &smart::Smart) -> usize {
    let mut skip = HashSet::new();
    let mut total = 0;
    for _ in 0..100 {
        let n = categories::step(&env.lib, s, &mut skip).unwrap();
        if n == 0 {
            return total;
        }
        total += n;
    }
    panic!("the step never finished");
}

/// Installs smart search with `look` and embeds everything.
fn start(env: &Env, l: Look) -> Arc<smart::Smart> {
    let s = smart::install(&env.lib, Arc::new(l), 0.5, 0.9).unwrap();
    embed_all(env, &s);
    s
}

fn category_of(env: &Env, id: &str) -> Option<Category> {
    env.lib
        .db
        .with(|c| cs::get(c, id))
        .unwrap()
        .map(|r| r.category)
}

fn ids(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

struct World {
    env: Env,
    s: Arc<smart::Smart>,
    receipt: String,
    receipt2: String,
    food: String,
    pet: String,
    photo: String,
    shot: String,
    doc: String,
    note: String,
}

/// Nine photos, embedded and decided by the rules: two receipts, food, a pet, a plain photo, a
/// screenshot (screen-sized PNG without camera data), a scanned-looking document with a position
/// and a note.
fn world() -> World {
    let env = setup();
    let mut shot = spec("Screenshot_Maria_passport", GRAY, "2024-06-01T10:00:00Z");
    shot.size = (1170, 2532);
    shot.png = true;
    shot.camera = None;
    let mut doc = spec("scan-contract", WHITE, "2024-07-01T10:00:00Z");
    doc.gps = Some((55.68, 12.57));
    let receipt = add(&env, &spec("r1", RED, "2024-01-01T10:00:00Z"));
    let receipt2 = add(&env, &spec("r2", RED, "2024-02-01T10:00:00Z"));
    let food = add(&env, &spec("f1", GREEN, "2024-03-01T10:00:00Z"));
    let pet = add(&env, &spec("p1", BLUE, "2024-04-01T10:00:00Z"));
    let photo = add(&env, &spec("g1", GRAY, "2024-05-01T10:00:00Z"));
    let shot = add(&env, &shot);
    let doc = add(&env, &doc);
    let note = add(&env, &spec("n1", BLACK, "2024-08-01T10:00:00Z"));
    let s = start(&env, look("look-1", Category::Receipt));
    assert_eq!(decide_all(&env, &s), 8);

    World {
        env,
        s,
        receipt,
        receipt2,
        food,
        pet,
        photo,
        shot,
        doc,
        note,
    }
}

#[tokio::test]
async fn the_routes_answer_while_smart_search_is_off() {
    let env = setup();
    let (s, v) = get(&env.app, "/v1/media/categories/status").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["enabled"], false);
    assert_eq!(v["decided"], 0);
    assert_eq!(v["pending"], 0);
    assert_eq!(v["decider"]["kind"], "rules");
    assert_eq!(v["decider"]["id"], "rules-1");
    let (s, v) = get(&env.app, "/v1/media/categories").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["categories"], json!([]));
    let (s, _) = get(&env.app, "/v1/media/categories/nope/items").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = get(&env.app, "/v1/media/categories/receipt/items").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids(&v), Vec::<String>::new());
    let (s, _) = j(
        &env.app,
        "PUT",
        "/v1/media/items/it_nope/category",
        Some(json!({"category": "receipt"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = get(&env.app, "/v1/media/items/it_nope/details").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_step_decides_every_photo_and_the_status_counts_them() {
    let w = world();
    let want = [
        (&w.receipt, Category::Receipt),
        (&w.receipt2, Category::Receipt),
        (&w.food, Category::Food),
        (&w.pet, Category::Pet),
        (&w.photo, Category::Photo),
        (&w.shot, Category::Screenshot),
        (&w.doc, Category::Document),
        (&w.note, Category::Note),
    ];
    for (id, cat) in want {
        assert_eq!(category_of(&w.env, id), Some(cat), "{id}");
    }
    let (s, v) = get(&w.env.app, "/v1/media/categories/status").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["model"], "look-1");
    assert_eq!(v["decided"], 8);
    assert_eq!(v["pending"], 0);
    assert_eq!(v["stale"], 0);
    assert_eq!(v["by_decider"], json!({"rules": 8, "jev": 0, "owner": 0}));
    assert_eq!(v["features_version"], 1);
    // Nothing is left for the step to do.
    assert_eq!(decide_all(&w.env, &w.s), 0);

    // The record is stored with the decision, and holds no file name.
    let (json, by, model, version): (String, String, String, i64) = w
        .env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT features_json, decided_by, model, features_version FROM ml_categories WHERE item_id = ?1",
                [&w.shot],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        })
        .unwrap();
    assert_eq!(
        (by.as_str(), model.as_str(), version),
        ("rules", "look-1", 1)
    );
    let f: Features = serde_json::from_str(&json).unwrap();
    assert!(f.screen_sized && !f.camera_exif);
    assert_eq!(
        f.name_hint,
        lucina_media_ml::categories::NameHint::Screenshot
    );
    for leak in ["maria", "passport", "Screenshot_", ".png"] {
        assert!(!json.contains(leak), "{leak} in {json}");
    }

    // A new photo is picked up by the next step.
    let late = add(&w.env, &spec("late", GREEN, "2024-09-01T10:00:00Z"));
    embed_all(&w.env, &w.s);
    let (_, v) = get(&w.env.app, "/v1/media/categories/status").await;
    assert_eq!(v["pending"], 1);
    assert_eq!(decide_all(&w.env, &w.s), 1);
    assert_eq!(category_of(&w.env, &late), Some(Category::Food));
}

#[tokio::test]
async fn the_automatic_albums_count_cover_and_list() {
    let w = world();
    let app = &w.env.app;
    let (s, v) = get(app, "/v1/media/categories").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let rows = v["categories"].as_array().unwrap();
    let got: Vec<(&str, i64)> = rows
        .iter()
        .map(|r| {
            (
                r["category"].as_str().unwrap(),
                r["count"].as_i64().unwrap(),
            )
        })
        .collect();
    // In the apps' order; `photo` is not an album.
    assert_eq!(
        got,
        [
            ("screenshot", 1),
            ("receipt", 2),
            ("document", 1),
            ("note", 1),
            ("food", 1),
            ("pet", 1)
        ]
    );
    let receipts = &rows[1];
    // The newest receipt is the cover.
    assert_eq!(receipts["cover"]["id"], w.receipt2.as_str());
    assert_eq!(receipts["cover"]["kind"], "photo");

    // The listing is the Gallery's page shape.
    let (s, v) = get(app, "/v1/media/categories/receipt/items").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids(&v), [w.receipt2.clone(), w.receipt.clone()]);
    assert_eq!(v["total"], 2);
    assert_eq!(v["next_cursor"], Value::Null);
    assert!(v["albums"].is_array());
    // Paging by cursor.
    let (_, p1) = get(app, "/v1/media/categories/receipt/items?limit=1").await;
    assert_eq!(ids(&p1), vec![w.receipt2.clone()]);
    let cursor = p1["next_cursor"].as_str().unwrap().to_string();
    let enc = cursor.replace('|', "%7C").replace(':', "%3A");
    let (_, p2) = get(
        app,
        &format!("/v1/media/categories/receipt/items?limit=1&cursor={enc}"),
    )
    .await;
    assert_eq!(ids(&p2), vec![w.receipt.clone()]);
    assert_eq!(p2["next_cursor"], Value::Null);
    // The listing's filters apply: dates, favourites, origin.
    let (_, v) = get(
        app,
        "/v1/media/categories/receipt/items?from=2024-02&to=2024-03",
    )
    .await;
    assert_eq!(ids(&v), vec![w.receipt2.clone()]);
    let (s, _) = j(
        app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "favorite", "ids": [w.receipt]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = get(app, "/v1/media/categories/receipt/items?favorite=true").await;
    assert_eq!(ids(&v), vec![w.receipt.clone()]);
    let (s, _) = get(app, "/v1/media/categories/receipt/items?origin=bogus").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = get(app, "/v1/media/categories/receipt/items?origin=chat").await;
    assert_eq!(ids(&v), Vec::<String>::new());
    // `photo` is what no other category claims.
    let (_, v) = get(app, "/v1/media/categories/photo/items").await;
    assert_eq!(ids(&v), vec![w.photo.clone()]);
    // A trashed photo leaves the counts.
    j(
        app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "trash", "ids": [w.receipt2]})),
    )
    .await;
    let (_, v) = get(app, "/v1/media/categories").await;
    assert_eq!(v["categories"][1]["count"], 1);
    assert_eq!(v["categories"][1]["cover"]["id"], w.receipt.as_str());
    // The category listing is read-only.
    let (s, _) = j(
        app,
        "POST",
        "/v1/media/categories/receipt/items",
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    // And no album was made for it.
    let (_, albums) = get(app, "/v1/media/albums").await;
    let titles = albums.to_string().to_lowercase();
    // (The catalog has its own built-in "screenshots" album; the automatic ones add none.)
    for word in ["receipt", "document", "ticket", "meme", "pet", "food"] {
        assert!(!titles.contains(word), "{word} in {titles}");
    }
    let user_albums: i64 = w
        .env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM albums WHERE system_key IS NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(user_albums, 0);
}

#[tokio::test]
async fn details_carry_the_category() {
    let w = world();
    let (s, v) = get(&w.env.app, &format!("/v1/media/items/{}/details", w.food)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["category"]["category"], "food");
    assert_eq!(v["category"]["decided_by"], "rules");
    assert!(v["category"]["confidence"].as_f64().unwrap() > 0.9);
    assert_eq!(v["category"]["auto_category"], Value::Null);
    // The rest of the details is as before.
    assert!(v["item"]["id"].is_string() && v.get("exif").is_some() && v.get("people").is_some());
    // An undecided photo says null.
    let fresh = add(&w.env, &spec("fresh", GRAY, "2024-10-01T10:00:00Z"));
    let (_, v) = get(&w.env.app, &format!("/v1/media/items/{fresh}/details")).await;
    assert_eq!(v["category"], Value::Null);
}

#[tokio::test]
async fn an_owner_correction_is_never_overwritten() {
    let w = world();
    let app = &w.env.app;
    let uri = format!("/v1/media/items/{}/category", w.receipt);
    // "Not a receipt".
    let (s, v) = j(app, "PUT", &uri, Some(json!({"category": "photo"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["category"]["category"], "photo");
    assert_eq!(v["category"]["decided_by"], "owner");
    assert_eq!(v["category"]["auto_category"], "receipt");
    let (_, d) = get(app, &format!("/v1/media/items/{}/details", w.receipt)).await;
    assert_eq!(d["category"]["decided_by"], "owner");
    let (_, v) = get(app, "/v1/media/categories/receipt/items").await;
    assert_eq!(ids(&v), vec![w.receipt2.clone()]);
    let (_, v) = get(app, "/v1/media/categories/photo/items").await;
    assert!(ids(&v).contains(&w.receipt));
    // "Move to Documents" for another.
    let (s, v) = j(
        app,
        "PUT",
        &format!("/v1/media/items/{}/category", w.photo),
        Some(json!({"category": "document"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["category"]["category"], "document");
    assert_eq!(v["category"]["auto_category"], "photo");

    // The rules run again (a new decider id), a model-backed one decides; neither touches the owner.
    categories::install_decider(&w.env.lib, Arc::new(Fixed(Category::Meme)));
    let (_, st) = get(app, "/v1/media/categories/status").await;
    assert_eq!(st["decider"]["kind"], "jev");
    assert_eq!(st["decider"]["id"], "jev-test-meme");
    // Eight photos, two with the owner's choice: the pet cannot be decided by it.
    assert_eq!(st["pending"], 6);
    assert_eq!(st["decided"], 2);
    assert_eq!(st["stale"], 6);
    assert_eq!(decide_all(&w.env, &w.s), 6);
    assert_eq!(category_of(&w.env, &w.receipt), Some(Category::Photo));
    assert_eq!(category_of(&w.env, &w.photo), Some(Category::Document));
    assert_eq!(category_of(&w.env, &w.food), Some(Category::Meme));
    assert_eq!(category_of(&w.env, &w.receipt2), Some(Category::Meme));
    // The pet was skipped (the decider failed): it keeps the rules' answer, and stays pending.
    assert_eq!(category_of(&w.env, &w.pet), Some(Category::Pet));
    let (_, st) = get(app, "/v1/media/categories/status").await;
    assert_eq!(st["by_decider"], json!({"rules": 0, "jev": 5, "owner": 2}));
    assert_eq!(st["pending"], 1);
    assert_eq!(st["stale"], 1);

    // The corrections are labelled examples, with what the machine had said and the record.
    let (s, v) = get(app, "/v1/media/categories/examples").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ex = v["examples"].as_array().unwrap();
    assert_eq!(ex.len(), 2);
    let first = ex
        .iter()
        .find(|e| e["item_id"] == w.receipt.as_str())
        .unwrap();
    assert_eq!(first["category"], "photo");
    assert_eq!(first["auto_category"], "receipt");
    assert_eq!(first["features"]["top_category"], "receipt");
    assert_eq!(first["features"]["version"], 1);
    assert_eq!(first["model"], "look-1");
    let (_, one) = get(app, "/v1/media/categories/examples?limit=1").await;
    assert_eq!(one["examples"].as_array().unwrap().len(), 1);
    let cur = one["next_cursor"]
        .as_str()
        .unwrap()
        .replace('|', "%7C")
        .replace(':', "%3A");
    let (_, two) = get(
        app,
        &format!("/v1/media/categories/examples?limit=1&cursor={cur}"),
    )
    .await;
    assert_eq!(two["examples"].as_array().unwrap().len(), 1);
    assert_ne!(one["examples"][0]["item_id"], two["examples"][0]["item_id"]);
    assert_eq!(two["next_cursor"], Value::Null);
    let (s, _) = get(app, "/v1/media/categories/examples?cursor=garbage").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Back to automatic: the owner's row goes and the photo is decided again now.
    let (s, v) = j(app, "PUT", &uri, Some(json!({"category": null}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["category"]["decided_by"], "jev");
    assert_eq!(v["category"]["category"], "meme");
    let (_, v) = get(app, "/v1/media/categories/examples").await;
    assert_eq!(v["examples"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_label_on_an_undecided_photo_keeps_its_record() {
    let w = world();
    let app = &w.env.app;
    let fresh = add(&w.env, &spec("fresh", GREEN, "2024-10-01T10:00:00Z"));
    embed_all(&w.env, &w.s);
    let uri = format!("/v1/media/items/{fresh}/category");
    let (s, v) = j(app, "PUT", &uri, Some(json!({"category": "meme"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["category"]["auto_category"], Value::Null);
    let (_, ex) = get(app, "/v1/media/categories/examples").await;
    let e = &ex["examples"][0];
    assert_eq!(e["item_id"], fresh.as_str());
    assert_eq!(e["features"]["top_category"], "food");
    // The step leaves it alone.
    decide_all(&w.env, &w.s);
    assert_eq!(category_of(&w.env, &fresh), Some(Category::Meme));
    // A request must say what it wants.
    for body in [
        json!({}),
        json!({"category": 3}),
        json!({"category": "nope"}),
        json!({"category": "Receipt"}),
    ] {
        let (s, _) = j(app, "PUT", &uri, Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    assert_eq!(category_of(&w.env, &fresh), Some(Category::Meme));
}

#[tokio::test]
async fn a_new_model_decides_every_photo_again() {
    let w = world();
    let app = &w.env.app;
    j(
        app,
        "PUT",
        &format!("/v1/media/items/{}/category", w.receipt),
        Some(json!({"category": "photo"})),
    )
    .await;
    // look-2 sees red as a document.
    let s2 = start(&w.env, look("look-2", Category::Document));
    let (_, st) = get(app, "/v1/media/categories/status").await;
    assert_eq!(st["model"], "look-2");
    // Before it runs: every automatic decision is stale and every photo with a vector is pending.
    assert_eq!(st["pending"], 7);
    assert_eq!(st["stale"], 7);
    assert_eq!(st["decided"], 1);
    // They keep their old category until decided again.
    assert_eq!(category_of(&w.env, &w.receipt2), Some(Category::Receipt));
    assert_eq!(decide_all(&w.env, &s2), 7);
    assert_eq!(category_of(&w.env, &w.receipt2), Some(Category::Document));
    assert_eq!(category_of(&w.env, &w.food), Some(Category::Food));
    assert_eq!(
        category_of(&w.env, &w.receipt),
        Some(Category::Photo),
        "the owner's choice stays"
    );
    let (_, st) = get(app, "/v1/media/categories/status").await;
    assert_eq!(
        (st["pending"].as_i64(), st["stale"].as_i64()),
        (Some(0), Some(0))
    );
    let model: String = w
        .env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT model FROM ml_categories WHERE item_id = ?1",
                [&w.receipt2],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(model, "look-2");
}

#[tokio::test]
async fn a_photo_that_changed_is_decided_again() {
    let w = world();
    // The photo changed after its vector and its decision were made (an edit): the old vector no
    // longer counts, so it is embedded again first, and then decided again.
    std::thread::sleep(std::time::Duration::from_millis(10));
    w.env
        .lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET updated_at = ?2 WHERE id = ?1",
                params![w.food, lucina_media::db::now_str()],
            )
        })
        .unwrap();
    let (_, st) = get(&w.env.app, "/v1/media/categories/status").await;
    assert_eq!(st["pending"], 0, "no current vector yet");
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert_eq!(embed_all(&w.env, &w.s), 1);
    let (_, st) = get(&w.env.app, "/v1/media/categories/status").await;
    assert_eq!(st["pending"], 1);
    assert_eq!(decide_all(&w.env, &w.s), 1);
    let (_, st) = get(&w.env.app, "/v1/media/categories/status").await;
    assert_eq!(st["pending"], 0);
}

#[tokio::test]
async fn sensitive_categories_stay_out_of_memories_and_places() {
    let env = setup();
    let place = Some((55.68, 12.57));
    let mut rows = Vec::new();
    for (name, rgb, hour, png) in [
        ("m-receipt", RED, "08", false),
        ("m-pet", BLUE, "09", false),
        ("m-photo", GRAY, "10", false),
        ("m-doc", WHITE, "11", false),
        ("m-note", BLACK, "12", false),
        ("m-shot", GRAY, "13", true),
        ("m-food", GREEN, "14", false),
    ] {
        let taken: &'static str = Box::leak(format!("2020-10-28T{hour}:00:00Z").into_boxed_str());
        let mut s = spec(name, rgb, taken);
        s.gps = place;
        if png {
            s.size = (1170, 2532);
            s.png = true;
            s.camera = None;
        }
        rows.push((name, add(&env, &s)));
    }
    let id = |n: &str| rows.iter().find(|r| r.0 == n).unwrap().1.clone();
    // Before they are decided, they all show: nothing is known about them.
    let (_, v) = get(
        &env.app,
        "/v1/media/items/on-this-day?date=10-28&year=2026&per_year=20",
    )
    .await;
    assert_eq!(v["years"][0]["items"].as_array().unwrap().len(), 7);
    let s = start(&env, look("look-1", Category::Receipt));
    assert_eq!(decide_all(&env, &s), 7);

    // On this day keeps real photos only: a pet, food and a photo, not even notes.
    let (st, v) = get(
        &env.app,
        "/v1/media/items/on-this-day?date=10-28&year=2026&per_year=20",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut shown: Vec<String> = v["years"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    shown.sort();
    let mut want = vec![id("m-pet"), id("m-photo"), id("m-food")];
    want.sort();
    assert_eq!(shown, want);
    assert_eq!(v["years"][0]["total"], 3);

    // Places: the sensitive ones are not on the map, and not in a cluster's list either.
    let (st, v) = get(&env.app, "/v1/media/items/places?zoom=3&kind=photo").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 4, "pet, photo, note, food");
    let (_, v) = get(&env.app, "/v1/media/items?bbox=12,55,13,56&kind=photo").await;
    let mut listed = ids(&v);
    listed.sort();
    let mut want = vec![id("m-pet"), id("m-photo"), id("m-note"), id("m-food")];
    want.sort();
    assert_eq!(listed, want);
    // The main grid still has everything: the photo stays in the Gallery.
    let (_, v) = get(&env.app, "/v1/media/items?kind=photo").await;
    assert_eq!(v["total"], 7);

    // The recently added strip leaves the sensitive ones out too.
    let (st, v) = get(&env.app, "/v1/media/items/recent?kind=photo").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["total"], 4);
    assert!(!ids(&v).contains(&id("m-receipt")));

    // Taking a photo out of a sensitive category (the owner's word) brings it back.
    j(
        &env.app,
        "PUT",
        &format!("/v1/media/items/{}/category", id("m-receipt")),
        Some(json!({"category": "photo"})),
    )
    .await;
    let (_, v) = get(&env.app, "/v1/media/items/recent?kind=photo").await;
    assert_eq!(v["total"], 5);
    // ...and putting one in does the opposite.
    j(
        &env.app,
        "PUT",
        &format!("/v1/media/items/{}/category", id("m-pet")),
        Some(json!({"category": "ticket"})),
    )
    .await;
    let (_, v) = get(&env.app, "/v1/media/items/places?zoom=3&kind=photo").await;
    assert_eq!(
        v["total"], 4,
        "the receipt came back, the pet is a ticket now: 4"
    );
}

#[tokio::test]
async fn nothing_about_categories_reaches_a_share_link() {
    let w = world();
    let app = &w.env.app;
    let (s, v) = j(
        app,
        "POST",
        "/v1/media/shares",
        Some(json!({"item_ids": [w.receipt, w.food]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    for path in ["", "/items"] {
        let (s, body) = call(
            app,
            "GET",
            &format!("/v1/media/a/{token}{path}"),
            None,
            false,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{path}");
        let low = String::from_utf8(body).unwrap().to_lowercase();
        for word in [
            "categor",
            "receipt",
            "decided_by",
            "screenshot",
            "confidence",
        ] {
            assert!(!low.contains(word), "{word} in a share response: {low}");
        }
    }
    for path in ["categories", "category", "items/x/category", "i/x/category"] {
        let (s, _) = call(
            app,
            "GET",
            &format!("/v1/media/a/{token}/{path}"),
            None,
            false,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }
    // Every category route needs the upstream key.
    for (method, uri) in [
        ("GET", "/v1/media/categories".to_string()),
        ("GET", "/v1/media/categories/status".to_string()),
        ("GET", "/v1/media/categories/examples".to_string()),
        ("GET", "/v1/media/categories/receipt/items".to_string()),
        ("PUT", format!("/v1/media/items/{}/category", w.receipt)),
    ] {
        let body = (method == "PUT").then(|| json!({"category": "photo"}));
        let (s, _) = call(app, method, &uri, body, false).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    assert_eq!(category_of(&w.env, &w.receipt), Some(Category::Receipt));
}

#[tokio::test]
async fn a_purge_forgets_the_decision() {
    let w = world();
    let app = &w.env.app;
    for op in ["trash", "purge"] {
        let (s, v) = j(
            app,
            "POST",
            "/v1/media/items/batch",
            Some(json!({"op": op, "ids": [w.receipt]})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{op} {v}");
    }
    assert_eq!(category_of(&w.env, &w.receipt), None);
    assert!(category_of(&w.env, &w.receipt2).is_some());
}

#[tokio::test]
async fn the_rules_decider_is_the_default_and_a_decider_can_be_chosen() {
    let env = setup();
    assert_eq!(
        categories::decider(&env.lib).id(),
        RulesDecider::default().id()
    );
    categories::install_decider(&env.lib, Arc::new(Fixed(Category::Map)));
    assert_eq!(categories::decider(&env.lib).kind(), DeciderKind::Jev);
    // Another catalog is not affected.
    let other = setup();
    assert_eq!(categories::decider(&other.lib).kind(), DeciderKind::Rules);
}

/// A decider that can be out (a JEV outage): while `up` is false it is not ready and every call
/// is an outage; `flaky` makes it answer an outage even while ready (a timeout).
struct Toggle {
    up: std::sync::atomic::AtomicBool,
    flaky: std::sync::atomic::AtomicBool,
}

impl Decider for Toggle {
    fn kind(&self) -> DeciderKind {
        DeciderKind::Jev
    }
    fn id(&self) -> String {
        "jev-toggle".into()
    }
    fn ready(&self) -> bool {
        self.up.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn decide(&self, _f: &Features) -> Result<Decision, MlError> {
        if !self.ready() || self.flaky.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(MlError::Unavailable("jev is out".into()));
        }
        Ok(Decision {
            category: Category::Map,
            confidence: 0.8,
            reason: "toggle".into(),
        })
    }
}

fn decided_by(env: &Env, id: &str) -> Option<cs::DecidedBy> {
    env.lib
        .db
        .with(|c| cs::get(c, id))
        .unwrap()
        .map(|r| r.decided_by)
}

#[tokio::test]
async fn during_an_outage_the_rules_only_fill_gaps() {
    use std::sync::atomic::Ordering::SeqCst;
    let env = setup();
    let a = add(&env, &spec("a1", GREEN, "2024-01-01T10:00:00Z"));
    let b = add(&env, &spec("b1", BLUE, "2024-02-01T10:00:00Z"));
    let s = start(&env, look("look-1", Category::Receipt));
    let toggle = Arc::new(Toggle {
        up: false.into(),
        flaky: false.into(),
    });
    categories::install_decider(&env.lib, toggle.clone());

    // JEV out from the start: the rules decide, so nothing waits undecided.
    assert_eq!(decide_all(&env, &s), 2);
    assert_eq!(decided_by(&env, &a), Some(cs::DecidedBy::Rules));
    assert_eq!(category_of(&env, &a), Some(Category::Food));
    let (_, st) = get(&env.app, "/v1/media/categories/status").await;
    assert_eq!(st["decider"]["ready"], false, "{st}");

    // JEV back: it decides every photo again.
    toggle.up.store(true, SeqCst);
    assert_eq!(decide_all(&env, &s), 2);
    assert_eq!(decided_by(&env, &a), Some(cs::DecidedBy::Jev));
    assert_eq!(category_of(&env, &b), Some(Category::Map));

    // Out again: its answers stay; a new photo is decided by the rules.
    toggle.up.store(false, SeqCst);
    let c = add(&env, &spec("c1", GREEN, "2024-03-01T10:00:00Z"));
    embed_all(&env, &s);
    assert_eq!(decide_all(&env, &s), 1);
    assert_eq!(category_of(&env, &a), Some(Category::Map));
    assert_eq!(decided_by(&env, &c), Some(cs::DecidedBy::Rules));

    // Ready but timing out: nothing is decided, nothing is skipped for good.
    toggle.up.store(true, SeqCst);
    toggle.flaky.store(true, SeqCst);
    let mut skip = HashSet::new();
    assert_eq!(categories::step(&env.lib, &s, &mut skip).unwrap(), 0);
    assert!(skip.is_empty());
    assert_eq!(decided_by(&env, &c), Some(cs::DecidedBy::Rules));
    toggle.flaky.store(false, SeqCst);
    assert_eq!(categories::step(&env.lib, &s, &mut skip).unwrap(), 1);
    assert_eq!(decided_by(&env, &c), Some(cs::DecidedBy::Jev));
}
