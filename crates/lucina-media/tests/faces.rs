//! Faces and people end to end through the HTTP router, with a fake face model and synthetic
//! pictures only: the "faces" are coloured squares on a dark background, and a face's identity is
//! the red of its square (A: 10, B: 250, C: 128 — three directions of the fake 3-value vectors).
//!
//! Covers the opt-in (off by default, nothing scanned), the scan and clustering, the People
//! routes, corrections by hand that survive a re-cluster and a re-scan, merging, purges, "delete
//! face data", smart search by person, and that nothing of it reaches a share link.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use image::{Rgb, RgbImage};
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{faces, http, smart, store, thumbs};
use lucina_media_ml::faces::{Detection, FaceModel};
use lucina_media_ml::{Embedder, MlError};
use rusqlite::params;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const A: u8 = 10;
const B: u8 = 250;
const C: u8 = 128;

/// Finds the coloured squares: the picture is a 4×3 grid of cells, a face is a square of pixels
/// with green above 150 and blue below 120 inside a cell.
struct Blocks;

impl FaceModel for Blocks {
    fn model(&self) -> &str {
        "blocks-test"
    }
    fn dim(&self) -> usize {
        3
    }
    fn detect(&self, img: &RgbImage) -> Result<Vec<Detection>, MlError> {
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
                let (bx, by) = (x0 as f32 / w as f32, y0 as f32 / h as f32);
                let (bw, bh) = (
                    (x1 + 1 - x0) as f32 / w as f32,
                    (y1 + 1 - y0) as f32 / h as f32,
                );
                let at = |fx: f32, fy: f32| [bx + fx * bw, by + fy * bh];
                out.push(Detection {
                    x: bx,
                    y: by,
                    w: bw,
                    h: bh,
                    score: 0.95,
                    landmarks: [
                        at(0.3, 0.35),
                        at(0.7, 0.35),
                        at(0.5, 0.55),
                        at(0.35, 0.78),
                        at(0.65, 0.78),
                    ],
                });
            }
        }
        Ok(out)
    }
    fn embed(&self, aligned: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(aligned
            .iter()
            .map(|a| {
                let r = a.get_pixel(56, 56)[0] as f32;
                if r < 80.0 {
                    vec![1.0, r / 255.0 * 0.3, 0.0]
                } else if r > 180.0 {
                    vec![0.0, 1.0, (255.0 - r) / 255.0 * 0.3]
                } else {
                    vec![0.0, 0.0, 1.0]
                }
            })
            .collect())
    }
}

/// Smart search's embedder: every picture and every word is the same vector.
struct Same;

impl Embedder for Same {
    fn model(&self) -> &str {
        "same-test"
    }
    fn dim(&self) -> usize {
        3
    }
    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(images.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }
    fn embed_text(&self, _: &str) -> Result<Vec<f32>, MlError> {
        Ok(vec![1.0, 0.0, 0.0])
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

/// A phone photo with the given faces and its renditions; `taken` orders the timeline.
fn add(env: &Env, name: &str, faces: &[(u32, u8)], taken: &str, album: Option<&str>) -> String {
    let src = env.tmp.path().join(format!("src-{name}.jpg"));
    std::fs::write(&src, picture(faces)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{name}.jpg")),
        source: Some("phone-ios".into()),
        device: Some("ios-1".into()),
        asset_id: Some(format!("ios-1/{name}")),
        taken_at: Some(taken.into()),
        album: album.map(str::to_string),
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

/// Scans everything pending, then the final pass, the way the worker does.
fn scan_all(env: &Env) -> usize {
    let mut total = 0;
    loop {
        let n = faces::step(&env.lib).unwrap();
        if n == 0 {
            return total;
        }
        total += n;
    }
}

struct World {
    env: Env,
    /// Photos in time order: A, A, A, A+B, B, B, C, none.
    p: Vec<String>,
}

const TAKEN: [&str; 8] = [
    "2024-01-01T10:00:00Z",
    "2024-02-01T10:00:00Z",
    "2024-03-01T10:00:00Z",
    "2024-04-01T10:00:00Z",
    "2024-05-01T10:00:00Z",
    "2024-06-01T10:00:00Z",
    "2024-07-01T10:00:00Z",
    "2024-08-01T10:00:00Z",
];

/// Eight photos scanned with face recognition on: A has four faces, B three, C one, the last
/// photo none.
async fn world() -> World {
    let env = setup();
    let layout: [&[(u32, u8)]; 8] = [
        &[(0, A)],
        &[(5, A)],
        &[(10, A)],
        &[(0, A), (11, B)],
        &[(3, B)],
        &[(6, B)],
        &[(7, C)],
        &[],
    ];
    let mut p = Vec::new();
    for (i, f) in layout.iter().enumerate() {
        let album = (i == 0).then_some("Trip");
        p.push(add(&env, &format!("p{i}"), f, TAKEN[i], album));
    }
    faces::install(&env.lib, Arc::new(Blocks)).unwrap();
    let (s, v) = j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(scan_all(&env), 8);
    World { env, p }
}

async fn people(app: &Router, query: &str) -> Vec<Value> {
    let (s, v) = get(app, &format!("/v1/media/people{query}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["people"].as_array().unwrap().clone()
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

async fn faces_of(app: &Router, item: &str) -> Vec<Value> {
    let (s, v) = get(app, &format!("/v1/media/items/{item}/faces")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["faces"].as_array().unwrap().clone()
}

/// The person with this many faces.
fn with_faces(list: &[Value], n: i64) -> Value {
    list.iter()
        .find(|p| p["face_count"] == n)
        .unwrap_or_else(|| panic!("no person with {n} faces in {list:?}"))
        .clone()
}

#[tokio::test]
async fn off_until_the_owner_turns_it_on() {
    let env = setup();
    // No model on this server: the setting reads off and unavailable, enabling is refused.
    let (s, v) = get(&env.app, "/v1/media/faces/settings").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (v["enabled"].clone(), v["available"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(v["people"], 0);
    let (s, _) = j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(people(&env.app, "").await.len(), 0);
    assert_eq!(faces::step(&env.lib).unwrap(), 0);

    // With a model it is available, and still off: a scan does nothing.
    let id = add(&env, "a", &[(0, A)], TAKEN[0], None);
    faces::install(&env.lib, Arc::new(Blocks)).unwrap();
    let (_, v) = get(&env.app, "/v1/media/faces/settings").await;
    assert_eq!(
        (v["enabled"].clone(), v["available"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(v["model"], "blocks-test");
    assert_eq!(faces::step(&env.lib).unwrap(), 0);
    let state = faces::state(&env.lib).unwrap();
    assert_eq!(faces::scan_step(&env.lib, &state).unwrap(), 0);
    let (_, v) = get(&env.app, "/v1/media/faces/settings").await;
    assert_eq!(
        (v["scanned"].clone(), v["faces"].clone()),
        (json!(0), json!(0))
    );
    assert!(faces_of(&env.app, &id).await.is_empty());

    // On: it scans; off again: it stops, and keeps what it found.
    let (_, v) = j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(v["enabled"], true);
    assert_eq!(v["pending"], 1);
    assert_eq!(scan_all(&env), 1);
    assert_eq!(faces_of(&env.app, &id).await.len(), 1);
    let (_, v) = j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(
        (
            v["enabled"].clone(),
            v["faces"].clone(),
            v["scanned"].clone()
        ),
        (json!(false), json!(1), json!(1))
    );
    add(&env, "b", &[(1, A)], TAKEN[1], None);
    assert_eq!(scan_all(&env), 0);
    let (s, _) = j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": "yes"})),
    )
    .await;
    assert!(s.is_client_error());
}

#[tokio::test]
async fn faces_are_found_and_grouped_into_people() {
    let w = world().await;
    let app = &w.env.app;
    let (_, v) = get(app, "/v1/media/faces/settings").await;
    assert_eq!(v["enabled"], true);
    assert_eq!(v["scanned"], 8);
    assert_eq!(v["pending"], 0);
    assert_eq!(v["faces"], 8);
    assert_eq!(v["people"], 2);
    assert_eq!(v["running"], false);

    // Two people, the bigger first; the lone face (C) is not a person.
    let list = people(app, "").await;
    assert_eq!(list.len(), 2);
    assert_eq!(
        (list[0]["face_count"].clone(), list[0]["item_count"].clone()),
        (json!(4), json!(4))
    );
    assert_eq!(
        (list[1]["face_count"].clone(), list[1]["item_count"].clone()),
        (json!(3), json!(3))
    );
    for p in &list {
        assert_eq!(p["name"], Value::Null);
        assert_eq!(p["hidden"], false);
        assert_eq!(p["birth_date"], Value::Null);
        let (id, cover) = (
            p["id"].as_str().unwrap(),
            p["cover_face_id"].as_str().unwrap(),
        );
        assert_eq!(
            p["thumb_url"],
            format!("/v1/media/people/{id}/thumb?v={cover}")
        );
    }
    assert_eq!(people(app, "?min_faces=4").await.len(), 1);
    assert_eq!(people(app, "?min_faces=1").await.len(), 2);
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap().to_string(),
        list[1]["id"].as_str().unwrap().to_string(),
    );
    let (s, one) = get(app, &format!("/v1/media/people/{a_id}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one, list[0]);

    // A photo's faces: boxes as fractions of the picture, the person of each.
    let both = faces_of(app, &w.p[3]).await;
    assert_eq!(both.len(), 2);
    let (first, second) = (&both[0], &both[1]);
    assert_eq!(first["person"]["id"], a_id.as_str());
    assert_eq!(second["person"]["id"], b_id.as_str());
    assert_eq!(first["source"], "ml");
    assert_eq!(first["item_id"], w.p[3].as_str());
    // Cell 0: the square spans x 20..80 of 400 and y 20..80 of 300.
    let bx = &first["box"];
    for (k, want) in [("x", 0.05), ("y", 20.0 / 300.0), ("w", 0.15), ("h", 0.2)] {
        assert!((bx[k].as_f64().unwrap() - want).abs() < 0.01, "{k}: {bx}");
    }
    assert!(first["score"].as_f64().unwrap() > 0.9);
    assert!(faces_of(app, &w.p[7]).await.is_empty());

    // The photo's details name its people (unnamed here).
    let (s, d) = get(app, &format!("/v1/media/items/{}/details", w.p[3])).await;
    assert_eq!(s, StatusCode::OK);
    let ppl = d["people"].as_array().unwrap();
    assert_eq!(ppl.len(), 2);
    assert!(ppl.iter().all(|p| p["name"].is_null()));
    assert!(ppl.iter().any(|p| p["id"] == a_id.as_str()));
    let (_, d) = get(app, &format!("/v1/media/items/{}/details", w.p[7])).await;
    assert_eq!(d["people"], json!([]));

    // The faces with nobody: the lone one.
    let (s, un) = get(app, "/v1/media/faces/unassigned").await;
    assert_eq!(s, StatusCode::OK);
    let un_faces = un["faces"].as_array().unwrap();
    assert_eq!(un_faces.len(), 1);
    assert_eq!(un_faces[0]["item_id"], w.p[6].as_str());
    let c_face = un_faces[0]["id"].as_str().unwrap();
    assert_eq!(
        un_faces[0]["thumb_url"],
        format!("/v1/media/faces/{c_face}/thumb")
    );
    assert_eq!(un["next_cursor"], Value::Null);
}

#[tokio::test]
async fn a_persons_items_follow_the_listing() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let a_id = list[0]["id"].as_str().unwrap();

    let (s, v) = get(app, &format!("/v1/media/people/{a_id}/items")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Date order, newest first: A is in the photos 3, 2, 1, 0.
    assert_eq!(
        ids_of(&v),
        vec![
            w.p[3].clone(),
            w.p[2].clone(),
            w.p[1].clone(),
            w.p[0].clone()
        ]
    );
    assert_eq!(v["total"], 4);
    assert_eq!(v["next_cursor"], Value::Null);
    assert_eq!(v["items"][0]["kind"], "photo");

    // The listing's filters and paging.
    let (_, v) = get(
        app,
        &format!("/v1/media/people/{a_id}/items?from=2024-02&to=2024-03"),
    )
    .await;
    assert_eq!(ids_of(&v), vec![w.p[2].clone(), w.p[1].clone()]);
    let (_, v) = get(app, &format!("/v1/media/people/{a_id}/items?limit=3")).await;
    assert_eq!(ids_of(&v).len(), 3);
    let cursor = v["next_cursor"].as_str().unwrap().to_string();
    let (_, v) = get(
        app,
        &format!("/v1/media/people/{a_id}/items?limit=3&cursor={cursor}"),
    )
    .await;
    assert_eq!(ids_of(&v), vec![w.p[0].clone()]);
    let (_, v) = get(app, &format!("/v1/media/people/{a_id}/items?kind=video")).await;
    assert_eq!(v["total"], 0);
    w.env
        .lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET trashed_at = '2024-09-01T00:00:00Z' WHERE id = ?1",
                [&w.p[3]],
            )
        })
        .unwrap();
    let (_, v) = get(app, &format!("/v1/media/people/{a_id}/items")).await;
    assert_eq!(v["total"], 3, "the trash is left out like in the listing");
    let (_, one) = get(app, &format!("/v1/media/people/{a_id}")).await;
    assert_eq!(
        (one["face_count"].clone(), one["item_count"].clone()),
        (json!(3), json!(3))
    );
    let (st, _) = get(
        app,
        &format!("/v1/media/people/{a_id}/items?origin=nowhere"),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn crops_are_square_jpegs_under_the_data_folder() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let a = &list[0];
    let url = a["thumb_url"].as_str().unwrap();
    let (s, bytes) = call(app, "GET", url, None, true).await;
    assert_eq!(s, StatusCode::OK);
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (256, 256));
    // The crop shows the face's square (the red of A) at its centre, with margin around it.
    let rgb = img.to_rgb8();
    assert!(
        rgb.get_pixel(128, 128)[1] > 180,
        "{:?}",
        rgb.get_pixel(128, 128)
    );
    assert!(rgb.get_pixel(2, 2)[1] < 100);

    let cover = a["cover_face_id"].as_str().unwrap();
    let file = faces::crop_path(&w.env.lib.cfg, cover);
    assert!(file.is_file(), "{file:?}");
    assert!(file.starts_with(w.env.tmp.path().join("data").join("faces")));

    // A face's own crop, including the lone one.
    let (_, un) = get(app, "/v1/media/faces/unassigned").await;
    let thumb = un["faces"][0]["thumb_url"].as_str().unwrap();
    let (s, bytes) = call(app, "GET", thumb, None, true).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 256);
}

#[tokio::test]
async fn naming_hiding_and_covers() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap().to_string(),
        list[1]["id"].as_str().unwrap().to_string(),
    );
    let uri = format!("/v1/media/people/{a_id}");

    let (s, v) = j(
        app,
        "PATCH",
        &uri,
        Some(json!({"name": "  Ada  ", "birth_date": "1990-02-03"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].clone(), v["birth_date"].clone()),
        (json!("Ada"), json!("1990-02-03"))
    );
    // Named people come first, the bigger first.
    let (_, v) = j(
        app,
        "PATCH",
        &format!("/v1/media/people/{b_id}"),
        Some(json!({"name": "Bea"})),
    )
    .await;
    assert_eq!(v["name"], "Bea");
    let list = people(app, "").await;
    assert_eq!(
        (list[0]["name"].clone(), list[1]["name"].clone()),
        (json!("Ada"), json!("Bea")),
        "{list:?}"
    );
    // And the photo's details carry the names.
    let (_, d) = get(app, &format!("/v1/media/items/{}/details", w.p[3])).await;
    assert_eq!(
        d["people"],
        json!([{"id": a_id, "name": "Ada"}, {"id": b_id, "name": "Bea"}])
    );

    // Bad input.
    for body in [
        json!({"name": "x".repeat(81)}),
        json!({"birth_date": "1990-02-30"}),
        json!({"birth_date": "2999-01-01"}),
        json!({"birth_date": "03/02/1990"}),
        json!({"cover_face_id": "fc_nope"}),
        json!({"hidden": "yes"}),
    ] {
        let (s, v) = j(app, "PATCH", &uri, Some(body.clone())).await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {s} {v}"
        );
    }
    // 80 characters are fine; an empty name clears it; null clears the birth date.
    let (s, v) = j(app, "PATCH", &uri, Some(json!({"name": "é".repeat(80)}))).await;
    assert_eq!(
        (s, v["name"].as_str().unwrap().chars().count()),
        (StatusCode::OK, 80)
    );
    let (_, v) = j(
        app,
        "PATCH",
        &uri,
        Some(json!({"name": "   ", "birth_date": null})),
    )
    .await;
    assert_eq!(
        (v["name"].clone(), v["birth_date"].clone()),
        (Value::Null, Value::Null)
    );
    let (_, v) = j(app, "PATCH", &uri, Some(json!({}))).await;
    assert_eq!(v["id"], a_id.as_str());

    // A cover must be one of the person's own faces.
    let a_faces: Vec<String> = faces_of(app, &w.p[2])
        .await
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect();
    let b_face = faces_of(app, &w.p[4]).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = j(app, "PATCH", &uri, Some(json!({"cover_face_id": b_face}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = j(
        app,
        "PATCH",
        &uri,
        Some(json!({"cover_face_id": a_faces[0]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["cover_face_id"], a_faces[0].as_str());
    assert_eq!(
        v["thumb_url"],
        format!("/v1/media/people/{a_id}/thumb?v={}", a_faces[0])
    );

    // Hidden people are left out unless asked for, and out of the photo's details.
    let (_, v) = j(app, "PATCH", &uri, Some(json!({"hidden": true}))).await;
    assert_eq!(v["hidden"], true);
    assert_eq!(people(app, "").await.len(), 1);
    assert_eq!(people(app, "?include_hidden=true").await.len(), 2);
    let (_, d) = get(app, &format!("/v1/media/items/{}/details", w.p[3])).await;
    assert_eq!(d["people"].as_array().unwrap().len(), 1);
    let (_, fs) = get(app, &format!("/v1/media/items/{}/faces", w.p[3])).await;
    assert_eq!(fs["faces"][0]["person"]["hidden"], true);
    let (_, st) = get(app, "/v1/media/faces/settings").await;
    assert_eq!(st["people"], 1);

    let (s, _) = j(
        app,
        "PATCH",
        "/v1/media/people/pe_nope",
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn merging_people() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap().to_string(),
        list[1]["id"].as_str().unwrap().to_string(),
    );
    j(
        app,
        "PATCH",
        &format!("/v1/media/people/{b_id}"),
        Some(json!({"name": "Bea"})),
    )
    .await;
    let uri = format!("/v1/media/people/{a_id}/merge");

    for body in [json!({"from": []}), json!({})] {
        let (s, _) = j(app, "POST", &uri, Some(body)).await;
        assert!(s.is_client_error());
    }
    let (s, _) = j(
        app,
        "POST",
        "/v1/media/people/pe_nope/merge",
        Some(json!({"from": [b_id]})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Unknown ids and the person itself are skipped.
    let (s, v) = j(app, "POST", &uri, Some(json!({"from": ["pe_nope", a_id]}))).await;
    assert_eq!((s, v["face_count"].clone()), (StatusCode::OK, json!(4)));

    let (s, v) = j(app, "POST", &uri, Some(json!({"from": [b_id]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Seven faces; the photo with both A and B counts once.
    assert_eq!(
        (v["face_count"].clone(), v["item_count"].clone()),
        (json!(7), json!(6))
    );
    assert_eq!(
        v["name"], "Bea",
        "the unnamed person takes the merged one's name"
    );
    assert_eq!(people(app, "").await.len(), 1);
    let (s, _) = get(app, &format!("/v1/media/people/{b_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for f in faces_of(app, &w.p[3]).await {
        assert_eq!(f["person"]["id"], a_id.as_str());
    }
    // The merged faces stay together when scanning goes on.
    let photo = add(&w.env, "late", &[(1, B)], "2024-09-01T10:00:00Z", None);
    scan_all(&w.env);
    assert_eq!(
        faces_of(&w.env.app, &photo).await[0]["person"]["id"],
        a_id.as_str()
    );
}

#[tokio::test]
async fn corrections_by_hand_survive_clustering_and_rescans() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap().to_string(),
        list[1]["id"].as_str().unwrap().to_string(),
    );
    let c_face = faces_of(app, &w.p[6]).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let a_face = faces_of(app, &w.p[1]).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The lone face goes to B by hand; one of A's faces is unassigned by hand.
    let (s, v) = j(
        app,
        "PUT",
        &format!("/v1/media/faces/{c_face}"),
        Some(json!({"person_id": b_id})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["source"].clone(), v["person"]["id"].clone()),
        (json!("manual"), json!(b_id))
    );
    let (s, v) = j(
        app,
        "PUT",
        &format!("/v1/media/faces/{a_face}"),
        Some(json!({"person_id": null})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (v["source"].clone(), v["person"].clone()),
        (json!("manual"), Value::Null)
    );
    assert_eq!(with_faces(&people(app, "").await, 4)["id"], b_id.as_str());
    let state = faces::state(&w.env.lib).unwrap();

    // Another pass over everything, then the photos change and are scanned again.
    faces::recluster(&w.env.lib, &state).unwrap();
    let later = lucina_media::db::now_str();
    for id in [&w.p[6], &w.p[1]] {
        std::thread::sleep(std::time::Duration::from_millis(5));
        w.env
            .lib
            .db
            .with(|c| {
                c.execute(
                    "UPDATE items SET updated_at = ?2 WHERE id = ?1",
                    params![id, later],
                )
            })
            .unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_, st) = get(app, "/v1/media/faces/settings").await;
    assert_eq!(st["pending"], 2);
    assert_eq!(scan_all(&w.env), 2);
    let again = faces_of(app, &w.p[6]).await;
    assert_eq!(
        again[0]["id"],
        c_face.as_str(),
        "the same face keeps its id"
    );
    assert_eq!(again[0]["person"]["id"], b_id.as_str());
    assert_eq!(again[0]["source"], "manual");
    let again = faces_of(app, &w.p[1]).await;
    assert_eq!(again[0]["id"], a_face.as_str());
    assert_eq!(
        again[0]["person"],
        Value::Null,
        "the owner's unassignment holds"
    );
    let (_, un) = get(app, "/v1/media/faces/unassigned").await;
    assert_eq!(un["faces"].as_array().unwrap().len(), 1);
    assert_eq!(un["faces"][0]["id"], a_face.as_str());

    // A new person by hand: named people are listed whatever their face count.
    let (s, v) = j(
        app,
        "PUT",
        &format!("/v1/media/faces/{a_face}"),
        Some(json!({"new_person": " Eve "})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let eve = v["person"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["person"]["name"], "Eve");
    let list = people(app, "").await;
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["id"], eve.as_str());
    assert_eq!(list[0]["face_count"], 1);
    // Moving its only face away drops an unnamed person, not a named one.
    let (_, v) = j(
        app,
        "PUT",
        &format!("/v1/media/faces/{a_face}"),
        Some(json!({"person_id": a_id})),
    )
    .await;
    assert_eq!(v["person"]["id"], a_id.as_str());
    let (s, p) = get(app, &format!("/v1/media/people/{eve}")).await;
    assert_eq!(
        (s, p["face_count"].clone(), p["cover_face_id"].clone()),
        (StatusCode::OK, json!(0), Value::Null)
    );
    assert_eq!(
        people(app, "").await.len(),
        2,
        "a person with no photos is not listed"
    );
}

#[tokio::test]
async fn bad_face_requests_and_missing_things() {
    let w = world().await;
    let app = &w.env.app;
    let face = faces_of(app, &w.p[0]).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let uri = format!("/v1/media/faces/{face}");
    for body in [
        json!({}),
        json!({"person_id": "x", "new_person": "y"}),
        json!({"new_person": "   "}),
        json!({"new_person": "x".repeat(81)}),
    ] {
        let (s, _) = j(app, "PUT", &uri, Some(body.clone())).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    }
    let (s, _) = j(app, "PUT", &uri, Some(json!({"person_id": "pe_nope"}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = j(
        app,
        "PUT",
        "/v1/media/faces/fc_nope",
        Some(json!({"person_id": null})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for uri in [
        "/v1/media/people/pe_nope",
        "/v1/media/people/pe_nope/items",
        "/v1/media/people/pe_nope/thumb",
        "/v1/media/people/a.b",
        "/v1/media/faces/fc_nope/thumb",
        "/v1/media/faces/a.b/thumb",
        "/v1/media/items/it_nope/faces",
    ] {
        let (s, _) = get(app, uri).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
    }
    let (s, _) = get(app, "/v1/media/faces/unassigned?cursor=zz").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Paging the unassigned: three of A's faces taken off A by hand, plus the lone one.
    for item in &w.p[..3] {
        let f = faces_of(app, item).await[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (s, _) = j(
            app,
            "PUT",
            &format!("/v1/media/faces/{f}"),
            Some(json!({"person_id": null})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let (_, page) = get(app, "/v1/media/faces/unassigned?limit=3").await;
    let first: Vec<&str> = page["faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["item_id"].as_str().unwrap())
        .collect();
    // Newest photo first: the lone C face (p6), then A's from p2 and p1.
    assert_eq!(
        first,
        vec![w.p[6].as_str(), w.p[2].as_str(), w.p[1].as_str()]
    );
    let cursor = page["next_cursor"].as_str().unwrap();
    let (_, rest) = get(
        app,
        &format!("/v1/media/faces/unassigned?limit=3&cursor={cursor}"),
    )
    .await;
    assert_eq!(rest["faces"].as_array().unwrap().len(), 1);
    assert_eq!(rest["faces"][0]["item_id"], w.p[0].as_str());
    assert_eq!(rest["next_cursor"], Value::Null);
}

#[tokio::test]
async fn a_changed_photo_is_scanned_again_and_keeps_its_faces() {
    let w = world().await;
    let app = &w.env.app;
    let before = faces_of(app, &w.p[3]).await;
    assert_eq!(before.len(), 2);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let later = lucina_media::db::now_str();
    w.env
        .lib
        .db
        .with(|c| {
            c.execute(
                "UPDATE items SET updated_at = ?2 WHERE id = ?1",
                params![w.p[3], later],
            )
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert_eq!(scan_all(&w.env), 1);
    assert_eq!(faces_of(app, &w.p[3]).await, before);
    assert_eq!(scan_all(&w.env), 0, "and it is current again");
}

#[tokio::test]
async fn purged_items_take_their_faces_with_them() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap().to_string(),
        list[1]["id"].as_str().unwrap().to_string(),
    );
    // Crops exist for both covers.
    for p in &list {
        let (s, _) = call(app, "GET", p["thumb_url"].as_str().unwrap(), None, true).await;
        assert_eq!(s, StatusCode::OK);
    }
    let b_cover = list[1]["cover_face_id"].as_str().unwrap().to_string();
    let crop = faces::crop_path(&w.env.lib.cfg, &b_cover);
    assert!(crop.is_file());

    // The purge calls forget_item: B's three photos go one by one.
    let forget = |id: &str| {
        w.env
            .lib
            .db
            .with(|c| lucina_media_ml::forget_item(c, id))
            .unwrap()
    };
    forget(&w.p[4]);
    let (_, one) = get(app, &format!("/v1/media/people/{b_id}")).await;
    assert_eq!(one["face_count"], 2);
    assert!(faces_of(app, &w.p[4]).await.is_empty());
    forget(&w.p[5]);
    forget(&w.p[3]);
    let (s, _) = get(app, &format!("/v1/media/people/{b_id}")).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "an unnamed person with no face is gone"
    );
    let (_, one) = get(app, &format!("/v1/media/people/{a_id}")).await;
    assert_eq!(one["face_count"], 3);
    assert_ne!(one["cover_face_id"], Value::Null);

    // A named person keeps existing without faces, but is not listed.
    j(
        app,
        "PATCH",
        &format!("/v1/media/people/{a_id}"),
        Some(json!({"name": "Ada"})),
    )
    .await;
    for id in [&w.p[0], &w.p[1], &w.p[2]] {
        forget(id);
    }
    let (s, one) = get(app, &format!("/v1/media/people/{a_id}")).await;
    assert_eq!(
        (s, one["face_count"].clone(), one["cover_face_id"].clone()),
        (StatusCode::OK, json!(0), Value::Null)
    );
    assert!(people(app, "").await.is_empty());

    // The hourly clean-up removes the crops of faces that are gone, and the faces of items that
    // no longer exist at all.
    assert!(crop.is_file());
    let removed = faces::prune(&w.env.lib).unwrap();
    assert_eq!(removed, 0);
    assert!(!crop.exists(), "the crop of a forgotten face is removed");
    w.env
        .lib
        .db
        .with(|c| {
            c.execute_batch("PRAGMA foreign_keys = OFF")?;
            c.execute("DELETE FROM items WHERE id = ?1", [&w.p[6]])?;
            c.execute_batch("PRAGMA foreign_keys = ON")
        })
        .unwrap();
    assert_eq!(faces::prune(&w.env.lib).unwrap(), 1);
    let (_, st) = get(app, "/v1/media/faces/settings").await;
    assert_eq!(st["faces"], 0);
}

#[tokio::test]
async fn delete_face_data_removes_everything_and_turns_it_off() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let (s, _) = call(
        app,
        "GET",
        list[0]["thumb_url"].as_str().unwrap(),
        None,
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let crops = w.env.tmp.path().join("data").join("faces");
    assert!(crops.is_dir());

    let (s, v) = j(app, "DELETE", "/v1/media/faces", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"ok": true})));
    assert!(!crops.exists());
    let (_, st) = get(app, "/v1/media/faces/settings").await;
    assert_eq!(st["enabled"], false);
    assert_eq!(
        (
            st["faces"].clone(),
            st["people"].clone(),
            st["scanned"].clone()
        ),
        (json!(0), json!(0), json!(0))
    );
    assert!(people(app, "?min_faces=1&include_hidden=true")
        .await
        .is_empty());
    for item in &w.p {
        assert!(faces_of(app, item).await.is_empty());
    }
    let (_, un) = get(app, "/v1/media/faces/unassigned").await;
    assert!(un["faces"].as_array().unwrap().is_empty());
    assert_eq!(scan_all(&w.env), 0, "it stays off");

    // Turned on again it starts from nothing and finds the same people.
    j(
        app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(scan_all(&w.env), 8);
    assert_eq!(people(app, "").await.len(), 2);
    // Deleting twice is fine.
    j(app, "DELETE", "/v1/media/faces", None).await;
    let (s, v) = j(app, "DELETE", "/v1/media/faces", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"ok": true})));
}

#[tokio::test]
async fn smart_search_can_be_restricted_to_a_person() {
    let w = world().await;
    let app = &w.env.app;
    let s = smart::install(&w.env.lib, Arc::new(Same), 0.5, 0.5).unwrap();
    while smart::embed_pending(&w.env.lib, &s).unwrap() > 0 {}
    let list = people(app, "").await;
    let (a_id, b_id) = (
        list[0]["id"].as_str().unwrap(),
        list[1]["id"].as_str().unwrap(),
    );

    let (st, v) = get(app, "/v1/media/items/search?q=anything").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 8);
    let (_, v) = get(
        app,
        &format!("/v1/media/items/search?q=anything&person={a_id}"),
    )
    .await;
    let mut got = ids_of(&v);
    got.sort();
    let mut want = vec![
        w.p[0].clone(),
        w.p[1].clone(),
        w.p[2].clone(),
        w.p[3].clone(),
    ];
    want.sort();
    assert_eq!(got, want);
    let (_, v) = get(
        app,
        &format!("/v1/media/items/search?q=anything&person={b_id}&from=2024-05"),
    )
    .await;
    assert_eq!(v["total"], 2);
    let (_, v) = get(app, "/v1/media/items/search?q=anything&person=pe_nope").await;
    assert_eq!(v["total"], 0);
    let (_, v) = get(
        app,
        &format!("/v1/media/items/{}/similar?person={b_id}", w.p[0]),
    )
    .await;
    assert_eq!(v["total"], 3);
}

#[tokio::test]
async fn nothing_about_faces_reaches_a_share_link_or_a_caller_without_the_key() {
    let w = world().await;
    let app = &w.env.app;
    let list = people(app, "").await;
    let a_id = list[0]["id"].as_str().unwrap();
    j(
        app,
        "PATCH",
        &format!("/v1/media/people/{a_id}"),
        Some(json!({"name": "Zelda Testname"})),
    )
    .await;

    // The album "Trip" holds the first photo (it has a face); share it.
    let (_, albums) = get(app, "/v1/media/albums").await;
    let trip = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["title"] == "Trip")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, v) = j(
        app,
        "POST",
        &format!("/v1/media/albums/{trip}/shares"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let token = v["token"].as_str().unwrap().to_string();

    let (s, page) = call(app, "GET", &format!("/v1/media/a/{token}"), None, false).await;
    assert_eq!(s, StatusCode::OK);
    let (s, items) = call(
        app,
        "GET",
        &format!("/v1/media/a/{token}/items"),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let items_json: Value = serde_json::from_slice(&items).unwrap();
    assert_eq!(items_json["items"].as_array().unwrap().len(), 1);
    for body in [
        String::from_utf8(page).unwrap(),
        String::from_utf8(items).unwrap(),
    ] {
        let low = body.to_lowercase();
        for word in ["zelda", "people", "person", "face", "fc_", "pe_"] {
            assert!(!low.contains(word), "{word} in a share response");
        }
    }
    // There are no faces routes under a share link.
    for path in ["faces", "people", "items/x/faces", "i/x/faces"] {
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

    // Every faces route needs the upstream key.
    let first_face = faces_of(app, &w.p[0]).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (method, uri) in [
        ("GET", "/v1/media/faces/settings".to_string()),
        ("PUT", "/v1/media/faces/settings".to_string()),
        ("DELETE", "/v1/media/faces".to_string()),
        ("GET", "/v1/media/people".to_string()),
        ("GET", format!("/v1/media/people/{a_id}")),
        ("PATCH", format!("/v1/media/people/{a_id}")),
        ("POST", format!("/v1/media/people/{a_id}/merge")),
        ("GET", format!("/v1/media/people/{a_id}/items")),
        ("GET", format!("/v1/media/people/{a_id}/thumb")),
        ("GET", format!("/v1/media/faces/{first_face}/thumb")),
        ("PUT", format!("/v1/media/faces/{first_face}")),
        ("GET", "/v1/media/faces/unassigned".to_string()),
        ("GET", format!("/v1/media/items/{}/faces", w.p[0])),
        ("GET", format!("/v1/media/items/{}/details", w.p[0])),
    ] {
        let (s, _) = call(app, method, &uri, Some(json!({})), false).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_rendition_that_cannot_be_read_is_recorded_once() {
    let env = setup();
    let id = add(&env, "lost", &[(0, A)], TAKEN[0], None);
    let dir = thumbs::dir_for(&env.lib.cfg, &id);
    for f in ["s.jpg", "l.jpg"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    faces::install(&env.lib, Arc::new(Blocks)).unwrap();
    j(
        &env.app,
        "PUT",
        "/v1/media/faces/settings",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(scan_all(&env), 1, "looked at once");
    let (_, st) = get(&env.app, "/v1/media/faces/settings").await;
    assert_eq!(
        (
            st["scanned"].clone(),
            st["pending"].clone(),
            st["faces"].clone()
        ),
        (json!(1), json!(0), json!(0))
    );
    let err: String = env
        .lib
        .db
        .with(|c| c.query_row("SELECT error FROM face_scans", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(err, "no rendition");
}
