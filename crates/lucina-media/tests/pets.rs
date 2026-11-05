//! Pets end to end through the HTTP router, with a fake pet model and synthetic pictures: an "animal" is a
//! coloured square on a dark background and its identity is the red of the square (A: 10, B: 250, C: 128). Covers:
//! on by default (no opt-in, unlike faces), only photos categorised as pets or photos are looked at, grouping,
//! renaming and hiding, merging, a pet's photos, turning it off, and nothing without the upstream key.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use image::{Rgb, RgbImage};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, pets, store, thumbs};
use lucina_media_ml::pets::{PetDetection, PetModel, Species};
use lucina_media_ml::MlError;
use rusqlite::params;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const A: u8 = 10;
const B: u8 = 250;
const C: u8 = 128;

/// Finds the coloured squares of a 4×3 grid of cells, like the faces test.
struct Blocks;

impl PetModel for Blocks {
    fn model(&self) -> &str {
        "blocks-pets-test"
    }
    fn dim(&self) -> usize {
        3
    }
    fn detect(&self, img: &RgbImage) -> Result<Vec<PetDetection>, MlError> {
        let (w, h) = img.dimensions();
        let (cw, ch) = (w / 4, h / 3);
        let mut out = Vec::new();
        for cy in 0..3 {
            for cx in 0..4 {
                let (mut x0, mut y0, mut x1, mut y1, mut n) = (u32::MAX, u32::MAX, 0, 0, 0);
                for y in cy * ch..(cy + 1) * ch {
                    for x in cx * cw..(cx + 1) * cw {
                        let p = img.get_pixel(x, y);
                        if p[1] > 150 && p[2] < 120 {
                            n += 1;
                            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                        }
                    }
                }
                if n < cw * ch / 4 {
                    continue;
                }
                out.push(PetDetection {
                    x: x0 as f32 / w as f32,
                    y: y0 as f32 / h as f32,
                    w: (x1 + 1 - x0) as f32 / w as f32,
                    h: (y1 + 1 - y0) as f32 / h as f32,
                    score: 0.9,
                    species: Species::Cat,
                });
            }
        }
        Ok(out)
    }
    fn embed(&self, crops: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(crops
            .iter()
            .map(|c| {
                let r = c.get_pixel(c.width() / 2, c.height() / 2)[0] as f32;
                if r < 80.0 {
                    vec![1.0, 0.0, 0.0]
                } else if r > 180.0 {
                    vec![0.0, 1.0, 0.0]
                } else {
                    vec![0.0, 0.0, 1.0]
                }
            })
            .collect())
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

/// A 400×300 picture: dark background, a 60-px square at the centre of each listed cell (0..12).
fn picture(faces: &[(u32, u8)]) -> Vec<u8> {
    let mut img = RgbImage::from_pixel(400, 300, Rgb([30, 30, 60]));
    for (cell, red) in faces {
        let (cx, cy) = ((cell % 4) * 100 + 20, (cell / 4) * 100 + 20);
        for y in cy..cy + 60 {
            for x in cx..cx + 60 {
                img.put_pixel(x, y, Rgb([*red, 220, 40]));
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

/// A photo with the given animals, its renditions and a category (`None`: not categorised yet).
fn add(env: &Env, name: &str, pets: &[(u32, u8)], taken: &str, category: Option<&str>) -> String {
    let src = env.tmp.path().join(format!("src-{name}.jpg"));
    std::fs::write(&src, picture(pets)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{name}.jpg")),
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
            c.execute("UPDATE items SET thumb_state = ?2 WHERE id = ?1", params![id, state.as_str()])?;
            if let Some(cat) = category {
                c.execute(
                    "INSERT INTO ml_categories (item_id, category, confidence, decided_by, decider_id, model, features_version, decided_at) \
                     VALUES (?1, ?2, 0.9, 'rules', 'test', 'test', 1, '2024-01-01T00:00:00Z')",
                    params![id, cat],
                )?;
            }
            Ok(())
        })
        .unwrap();
    id
}

fn scan_all(env: &Env) -> usize {
    let mut total = 0;
    loop {
        let n = pets::step(&env.lib).unwrap();
        if n == 0 {
            return total;
        }
        total += n;
    }
}

async fn pets_list(app: &Router, query: &str) -> Vec<Value> {
    let (s, v) = get(app, &format!("/v1/media/pets{query}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["pets"].as_array().unwrap().clone()
}

#[tokio::test]
async fn the_same_animal_is_grouped_and_the_owner_names_hides_and_merges() {
    let env = setup();
    let t = |m: u32| format!("2024-{m:02}-01T10:00:00Z");
    let p: Vec<String> = [
        (vec![(0, A)], Some("pet")),
        (vec![(5, A)], Some("pet")),
        (vec![(10, A)], Some("photo")),
        (vec![(0, A), (11, B)], Some("pet")),
        (vec![(3, B)], Some("pet")),
        (vec![(6, B)], Some("pet")),
        (vec![(7, C)], Some("pet")),
        (vec![(2, A)], Some("screenshot")),
        (vec![(4, A)], None),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (pets, cat))| add(&env, &format!("p{i}"), &pets, &t(i as u32 + 1), cat))
    .collect();
    pets::install(&env.lib, Arc::new(Blocks)).unwrap();
    // On by default: nothing to turn on.
    let (s, raw) = call(&env.app, "GET", "/v1/media/pet-crops/settings", None, true).await;
    let v: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    assert_eq!(v["enabled"], true, "{s} {}", String::from_utf8_lossy(&raw));
    // A screenshot and a photo not categorised yet are not looked at.
    assert_eq!(scan_all(&env), 7);

    let list = pets_list(&env.app, "").await;
    assert_eq!(
        list.len(),
        2,
        "A (4 crops) and B (3); the lone C is no pet: {list:?}"
    );
    assert_eq!(
        (list[0]["crop_count"].clone(), list[1]["crop_count"].clone()),
        (json!(4), json!(3))
    );
    let (a, b) = (
        list[0]["id"].as_str().unwrap().to_owned(),
        list[1]["id"].as_str().unwrap().to_owned(),
    );

    let (s, v) = get(&env.app, &format!("/v1/media/pets/{a}/items")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4);
    assert!(ids.contains(&p[2].as_str()) && !ids.contains(&p[7].as_str()));

    let (s, v) = j(
        &env.app,
        "PATCH",
        &format!("/v1/media/pets/{b}"),
        Some(json!({"name": "Micio"})),
    )
    .await;
    assert_eq!((s, v["name"].clone()), (StatusCode::OK, json!("Micio")));
    let (s, _) = j(
        &env.app,
        "PATCH",
        &format!("/v1/media/pets/{a}"),
        Some(json!({"hidden": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        pets_list(&env.app, "").await.len(),
        1,
        "hidden pets are left out"
    );
    assert_eq!(pets_list(&env.app, "?include_hidden=true").await.len(), 2);

    let (s, v) = j(
        &env.app,
        "POST",
        &format!("/v1/media/pets/{a}/merge"),
        Some(json!({"from": [b]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["crop_count"].clone(), v["name"].clone()),
        (json!(7), json!("Micio"))
    );
    let (s, _) = get(&env.app, &format!("/v1/media/pets/{b}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, v) = get(&env.app, &format!("/v1/media/items/{}/pets", p[3])).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = call(
        &env.app,
        "GET",
        &format!("/v1/media/pets/{a}/thumb"),
        None,
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&env.app, "GET", "/v1/media/pets", None, false).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "never without the upstream key"
    );

    // Turned off: nothing more is looked at.
    let (s, _) = j(
        &env.app,
        "PUT",
        "/v1/media/pet-crops/settings",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    add(&env, "late", &[(1, A)], &t(12), Some("pet"));
    assert_eq!(scan_all(&env), 0);
}
