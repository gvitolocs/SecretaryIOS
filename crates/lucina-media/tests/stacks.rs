//! Stacks through the HTTP router with synthetic pictures: a stack shows as its primary with a count,
//! its other members stay out of listings, counts and months, a member is still readable, the primary
//! can change, unstacking restores everything, trashing keeps stacks whole, and bursts and RAW+JPEG
//! pairs are suggested but never applied by themselves.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, store};
use rusqlite::params;
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

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

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
    let img = image::RgbImage::from_pixel(16, 16, image::Rgb([shade, 90, 160]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// A photo of device `device` taken at `taken`, with its own bytes (`shade`).
fn shot(env: &Env, shade: u8, device: &str, taken: &str, filename: &str) -> String {
    let src = env.tmp.path().join(format!("src-{shade}-{filename}"));
    std::fs::write(&src, png(shade)).unwrap();
    let client = ClientMeta {
        filename: Some(filename.into()),
        source: Some("phone-ios".into()),
        device: Some(device.into()),
        asset_id: Some(format!("{device}/{shade}")),
        taken_at: Some(taken.into()),
        ..Default::default()
    };
    ingest::ingest_file(&env.lib, &src, None, &client, None)
        .unwrap()
        .item_id
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// Three frames of one burst (a few hundred milliseconds apart) and a photo ten seconds later.
fn burst(env: &Env) -> (Vec<String>, String) {
    let frames = vec![
        shot(env, 1, "iphone", "2026-05-01T10:00:00.000Z", "IMG_1.JPG"),
        shot(env, 2, "iphone", "2026-05-01T10:00:00.400Z", "IMG_2.JPG"),
        shot(env, 3, "iphone", "2026-05-01T10:00:00.900Z", "IMG_3.JPG"),
    ];
    let later = shot(env, 4, "iphone", "2026-05-01T10:00:10.000Z", "IMG_4.JPG");
    (frames, later)
}

async fn stack(env: &Env, ids: &[&String], primary: Option<&String>) -> Value {
    let mut body = json!({"ids": ids});
    if let Some(p) = primary {
        body["primary"] = json!(p);
    }
    let (s, v) = call(&env.app, "POST", "/v1/media/stacks", Some(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["stack"].clone()
}

#[tokio::test]
async fn a_stack_is_one_tile_with_a_count_and_hides_the_rest_everywhere() {
    let env = setup();
    let (f, later) = burst(&env);
    let s = stack(&env, &[&f[0], &f[1], &f[2]], Some(&f[1])).await;
    assert_eq!(s["primary"], f[1].as_str());
    assert_eq!(s["count"], 3);
    assert_eq!(s["kind"], "manual");
    let items = s["items"].as_array().unwrap();
    assert_eq!(items[0]["id"], f[1].as_str(), "the primary first");
    assert_eq!(items[1]["id"], f[0].as_str(), "then by capture time");
    assert_eq!(items[2]["id"], f[2].as_str());

    // Listings: the primary with its count, the other two nowhere.
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    let got = ids_of(&list);
    assert_eq!(got.len(), 2);
    assert!(got.contains(&f[1]) && got.contains(&later));
    assert_eq!(list["total"], 2);
    let tile = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == f[1].as_str())
        .unwrap();
    assert_eq!(tile["stack"]["count"], 3);
    assert_eq!(tile["stack"]["primary"], true);
    assert_eq!(tile["stack"]["id"], s["id"]);
    let plain = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == later.as_str())
        .unwrap();
    assert!(plain["stack"].is_null());

    // Counts, months, sources, the summary, the origin filter and the gallery scope.
    let (_, src) = call(&env.app, "GET", "/v1/media/sources", None).await;
    assert_eq!(src["all"]["photos"], 2);
    let (_, months) = call(&env.app, "GET", "/v1/media/items/months", None).await;
    assert_eq!(months["months"][0]["count"], 2);
    let (_, sum) = call(&env.app, "GET", "/v1/media/summary", None).await;
    assert_eq!(sum["kinds"][0]["count"], 2, "{sum}");
    for uri in [
        "/v1/media/items?origin=device:iphone",
        "/v1/media/items?scope=gallery",
    ] {
        let (_, l) = call(&env.app, "GET", uri, None).await;
        assert_eq!(ids_of(&l).len(), 2, "{uri}");
    }

    // A member is still an item of its own: readable, with its stack, not the primary.
    let (st, m) = call(&env.app, "GET", &format!("/v1/media/items/{}", f[0]), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(m["id"], f[0].as_str());
    assert_eq!(m["stack"]["primary"], false);
    assert_eq!(m["stack"]["count"], 3);

    // GET stack.
    let (st, g) = call(
        &env.app,
        "GET",
        &format!("/v1/media/stacks/{}", s["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(g["stack"]["count"], 3);
    let (st, _) = call(&env.app, "GET", "/v1/media/stacks/stk_nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_primary_defaults_to_the_first_id_and_can_change() {
    let env = setup();
    let (f, _) = burst(&env);
    let s = stack(&env, &[&f[2], &f[0]], None).await;
    assert_eq!(s["primary"], f[2].as_str());
    let sid = s["id"].as_str().unwrap();

    let (st, v) = call(
        &env.app,
        "PUT",
        &format!("/v1/media/stacks/{sid}"),
        Some(json!({"primary": f[0]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["stack"]["primary"], f[0].as_str());
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert!(ids_of(&list).contains(&f[0]) && !ids_of(&list).contains(&f[2]));

    // A photo that is not a member cannot be the primary; an unknown stack is a 404.
    let (st, _) = call(
        &env.app,
        "PUT",
        &format!("/v1/media/stacks/{sid}"),
        Some(json!({"primary": f[1]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &env.app,
        "PUT",
        &format!("/v1/media/stacks/{sid}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &env.app,
        "PUT",
        "/v1/media/stacks/stk_nope",
        Some(json!({"primary": f[0]})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unstacking_puts_every_photo_back() {
    let env = setup();
    let (f, later) = burst(&env);
    let s = stack(&env, &[&f[0], &f[1], &f[2]], None).await;
    let sid = s["id"].as_str().unwrap();
    let (st, v) = call(&env.app, "DELETE", &format!("/v1/media/stacks/{sid}"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["unstacked"].as_array().unwrap().len(), 3);
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids_of(&list).len(), 4);
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["stack"].is_null()));
    let (st, _) = call(&env.app, "DELETE", &format!("/v1/media/stacks/{sid}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // They can be stacked again.
    stack(&env, &[&f[0], &later], None).await;
}

#[tokio::test]
async fn photos_that_cannot_join_are_named_and_nothing_changes() {
    let env = setup();
    let (f, later) = burst(&env);
    stack(&env, &[&f[0], &f[1]], None).await;
    let (st, v) = call(
        &env.app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "trash", "ids": [later]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let (st, v) = call(
        &env.app,
        "POST",
        "/v1/media/stacks",
        Some(json!({"ids": [f[2], f[0], later, "itm_missing"]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["ok"], false);
    let failed: std::collections::BTreeMap<String, String> = v["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["id"].as_str().unwrap().into(),
                x["reason"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(failed[&f[0]], "already_stacked");
    assert_eq!(failed[&later], "trashed");
    assert_eq!(failed["itm_missing"], "missing");
    assert!(!failed.contains_key(&f[2]));
    let (_, g) = call(&env.app, "GET", &format!("/v1/media/items/{}", f[2]), None).await;
    assert!(
        g["stack"].is_null(),
        "the valid photo was not stacked either"
    );

    for body in [
        json!({"ids": [f[2]]}),
        json!({"ids": [f[2], f[2]]}),
        json!({"ids": [f[2], f[1]], "primary": "x_other"}),
        json!({"ids": [f[2], f[1]], "kind": "pile"}),
        json!({"ids": [f[2], "a/b"]}),
    ] {
        let (st, _) = call(&env.app, "POST", "/v1/media/stacks", Some(body.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    }
    let many: Vec<String> = (0..101).map(|i| format!("id{i}")).collect();
    let (st, _) = call(
        &env.app,
        "POST",
        "/v1/media/stacks",
        Some(json!({"ids": many})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trashing_keeps_stacks_whole_by_whatever_route() {
    let env = setup();
    let (f, later) = burst(&env);
    let s = stack(&env, &[&f[0], &f[1], &f[2]], Some(&f[0])).await;
    let sid = s["id"].as_str().unwrap().to_string();

    // The primary goes to the Trash with the batch op: the next member takes over.
    let (st, v) = call(
        &env.app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "trash", "ids": [f[0]]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, g) = call(&env.app, "GET", &format!("/v1/media/stacks/{sid}"), None).await;
    assert_eq!(g["stack"]["count"], 2);
    assert_eq!(g["stack"]["primary"], f[1].as_str());
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(
        sorted(ids_of(&list)),
        sorted(vec![f[1].clone(), later.clone()])
    );
    let (_, tr) = call(&env.app, "GET", "/v1/media/items?trashed=true", None).await;
    assert_eq!(ids_of(&tr), vec![f[0].clone()]);
    let (_, gone) = call(&env.app, "GET", &format!("/v1/media/items/{}", f[0]), None).await;
    assert!(
        gone["stack"].is_null(),
        "a trashed photo has left the stack"
    );

    // A member goes with DELETE items/{id}: a stack of one is no stack.
    let (st, _) = call(
        &env.app,
        "DELETE",
        &format!("/v1/media/items/{}", f[2]),
        None,
    )
    .await;
    assert!(st.is_success());
    let (st, _) = call(&env.app, "GET", &format!("/v1/media/stacks/{sid}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(
        sorted(ids_of(&list)),
        sorted(vec![f[1].clone(), later.clone()])
    );
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["stack"].is_null()));
    // The trashed member shows up in the Trash, where it can be restored.
    let (_, tr) = call(&env.app, "GET", "/v1/media/items?trashed=true", None).await;
    assert_eq!(ids_of(&tr).len(), 2);
    let (_, r) = call(
        &env.app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "restore", "ids": [f[0]]})),
    )
    .await;
    assert_eq!(r["done"], json!([f[0]]));
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids_of(&list).len(), 3, "a restored photo is a plain photo");
    let rows: i64 = env
        .lib
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM stacks", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn bursts_are_suggested_by_time_and_by_look_and_never_applied() {
    let env = setup();
    let (f, later) = burst(&env);
    // Another phone shooting at the same moment is another burst, not part of this one.
    let other = shot(&env, 9, "ipad", "2026-05-01T10:00:00.500Z", "IMG_9.JPG");

    let (st, v) = call(&env.app, "GET", "/v1/media/stacks/suggestions", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 1, "{v}");
    let s = &v["suggestions"][0];
    assert_eq!(s["kind"], "burst");
    assert_eq!(s["evidence"], "time", "no hashes stored yet");
    assert_eq!(v["hashes_pending"], 5);
    let mut got = ids_of(&json!({"items": s["items"]}));
    got.sort();
    let mut want = f.clone();
    want.sort();
    assert_eq!(got, want);
    assert_eq!(s["span_ms"], 900);
    assert!(f.contains(&s["primary"].as_str().unwrap().to_string()));
    assert!(s["id"].as_str().unwrap().starts_with("sg_"));
    assert!(!got.contains(&later) && !got.contains(&other));

    // Nothing was stacked by asking.
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids_of(&list).len(), 5);

    // With hashes that agree, the evidence is time and look; one frame that looks nothing like the
    // others (a different scene) leaves the burst.
    for (id, ph) in [(&f[0], 0u64), (&f[1], 1), (&f[2], u64::MAX)] {
        env.lib
            .db
            .with(|c| {
                let content: String = c.query_row("SELECT hash FROM items WHERE id = ?1", [id], |r| r.get(0))?;
                c.execute(
                    "INSERT INTO item_hashes (item_id, content_hash, phash, dhash, thumb_w, thumb_h, flat, computed_at) VALUES (?1, ?2, ?3, ?3, 32, 32, 0, 'now')",
                    params![id, content, ph as i64],
                )
            })
            .unwrap();
    }
    let (_, v) = call(&env.app, "GET", "/v1/media/stacks/suggestions", None).await;
    let s = &v["suggestions"][0];
    assert_eq!(v["total"], 1);
    assert_eq!(s["evidence"], "time_and_look");
    assert_eq!(
        s["items"].as_array().unwrap().len(),
        2,
        "the other scene left: {v}"
    );

    // The count shows on the duplicates page for its banner.
    let (_, d) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(d["stack_suggestions"], 1);

    // Turning one down hides that exact set; stacking another removes the rest.
    let ids: Vec<String> = s["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().into())
        .collect();
    let (st, _) = call(
        &env.app,
        "POST",
        "/v1/media/stacks/suggestions/dismiss",
        Some(json!({"ids": ids})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = call(&env.app, "GET", "/v1/media/stacks/suggestions", None).await;
    assert_eq!(v["total"], 0);
    let (st, _) = call(
        &env.app,
        "POST",
        "/v1/media/stacks/suggestions/dismiss",
        Some(json!({"ids": [f[0]]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_suggestion_disappears_once_it_is_stacked_and_a_raw_pairs_with_its_picture() {
    let env = setup();
    let (f, _) = burst(&env);
    let (_, v) = call(
        &env.app,
        "GET",
        "/v1/media/stacks/suggestions?limit=1",
        None,
    )
    .await;
    let ids: Vec<String> = v["suggestions"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().into())
        .collect();
    assert_eq!(ids.len(), 3);
    let (st, s) = call(
        &env.app,
        "POST",
        "/v1/media/stacks",
        Some(json!({"ids": ids, "primary": v["suggestions"][0]["primary"], "kind": "burst"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{s}");
    assert_eq!(s["stack"]["kind"], "burst");
    let (_, v) = call(&env.app, "GET", "/v1/media/stacks/suggestions", None).await;
    assert_eq!(v["total"], 0);
    let _ = f;

    // A RAW and its JPEG: one name, two extensions, the same moment (not a burst of two shots).
    let jpg = shot(
        &env,
        20,
        "camera",
        "2026-06-01T08:00:00.000Z",
        "DSC_0100.JPG",
    );
    let raw = shot(
        &env,
        21,
        "camera",
        "2026-06-01T08:00:00.000Z",
        "DSC_0100.DNG",
    );
    let (_, v) = call(&env.app, "GET", "/v1/media/stacks/suggestions", None).await;
    assert_eq!(v["total"], 1, "{v}");
    let s = &v["suggestions"][0];
    assert_eq!(s["kind"], "raw_jpeg");
    assert_eq!(s["evidence"], "name");
    assert_eq!(
        s["primary"],
        jpg.as_str(),
        "the picture is the tile, not the RAW"
    );
    assert_eq!(s["items"].as_array().unwrap().len(), 2);
    let _ = raw;
}
