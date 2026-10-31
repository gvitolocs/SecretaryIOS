//! The duplicate review screen's API with synthetic pictures: groups from stored hashes (never hashed
//! by a request), the suggested keeper and its reason, resolve with the metadata merge and its undo,
//! "not duplicates" that stays dismissed, and apply-all that leaves the weak groups alone.

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

fn png(shade: u8, side: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(side, side, image::Rgb([shade, 90, 160]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

/// A photo with its own bytes; `meta` fills in what the test cares about.
fn add(env: &Env, shade: u8, side: u32, source: &str, meta: ClientMeta) -> String {
    let src = env.tmp.path().join(format!("src-{shade}-{side}-{source}"));
    std::fs::write(&src, png(shade, side)).unwrap();
    let client = ClientMeta {
        filename: Some(format!("{source}-{shade}.png")),
        source: Some(source.into()),
        width: Some(side as i64),
        height: Some(side as i64),
        ..meta
    };
    ingest::ingest_file(&env.lib, &src, None, &client, None)
        .unwrap()
        .item_id
}

fn phone(asset: &str, taken: Option<&str>) -> ClientMeta {
    ClientMeta {
        device: Some("iphone".into()),
        asset_id: Some(asset.into()),
        taken_at: taken.map(Into::into),
        ..Default::default()
    }
}

/// Store a perceptual hash for a photo, as `dedup-report`'s hasher would.
fn hash(env: &Env, id: &str, phash: u64, dhash: u64) {
    env.lib
        .db
        .with(|c| {
            let content: String =
                c.query_row("SELECT hash FROM items WHERE id = ?1", [id], |r| r.get(0))?;
            c.execute(
                "INSERT OR REPLACE INTO item_hashes (item_id, content_hash, phash, dhash, thumb_w, thumb_h, flat, computed_at)
                 VALUES (?1, ?2, ?3, ?4, 32, 32, 0, 'now')",
                params![id, content, phash as i64, dhash as i64],
            )
        })
        .unwrap();
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

const WHEN: &str = "2026-05-01T10:00:00.000Z";

#[tokio::test]
async fn a_group_has_its_members_a_keeper_and_a_reason() {
    let env = setup();
    let big = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let small = add(
        &env,
        11,
        16,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    let lone = add(&env, 12, 20, "folder", ClientMeta::default());
    hash(&env, &big, 0x0F0F_0F0F_0F0F_0F0F, 0x1234_5678_9ABC_DEF0);
    hash(&env, &small, 0x0F0F_0F0F_0F0F_0F0F, 0x1234_5678_9ABC_DEF0);
    hash(&env, &lone, 0xF0F0_F0F0_F0F0_F0F0, 0xEDCB_A987_6543_210F);

    let (s, v) = call(&env.app, "GET", "/v1/media/duplicates?status=open", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 1);
    assert_eq!(v["applicable"], 1);
    assert_eq!(v["hashes"], json!({"photos": 3, "hashed": 3, "pending": 0}));
    assert_eq!(v["min_score"].as_f64().unwrap() as f32, 0.7);
    assert_eq!(v["stack_suggestions"], 0);
    let g = &v["groups"][0];
    assert_eq!(g["class"], "resized_copy");
    assert_eq!(g["applicable"], true);
    assert_eq!(g["keep"], big.as_str());
    assert_eq!(g["reason"], "highest_resolution");
    assert!(g["id"].as_str().unwrap().starts_with("dg_"));
    let members = g["members"].as_array().unwrap();
    let order: Vec<&str> = members.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(order, vec![big.as_str(), small.as_str()], "keeper first");
    assert_eq!(members[0]["review"]["keep"], true);
    assert_eq!(members[1]["review"]["keep"], false);
    assert!(members[0]["review"]["metadata"].as_u64().unwrap() >= 1);
    assert!(members[0]["urls"]["original"].is_string(), "full item JSON");
    assert!(members[0]["stack"].is_null());
    assert!(v["reclaimable_bytes"].as_i64().unwrap() > 0);

    // Paging.
    let (_, v) = call(
        &env.app,
        "GET",
        "/v1/media/duplicates?limit=1&offset=1",
        None,
    )
    .await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["groups"].as_array().unwrap().len(), 0);
    let (s, _) = call(
        &env.app,
        "GET",
        "/v1/media/duplicates?status=resolved",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_request_never_hashes_the_library() {
    let env = setup();
    add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    add(&env, 11, 32, "archive-nespc", ClientMeta::default());
    let (s, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total"], 0);
    assert_eq!(v["groups"].as_array().unwrap().len(), 0);
    assert_eq!(v["hashes"], json!({"photos": 2, "hashed": 0, "pending": 2}));
    let rows: i64 = env
        .lib
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM item_hashes", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn keeping_one_copies_what_the_merge_does_not_and_undo_gives_it_back() {
    let env = setup();
    // The keeper is the bigger picture and knows little; the copy has a caption, a place, a date,
    // a favourite flag and an album.
    let keeper = add(&env, 10, 32, "archive-nespc", ClientMeta::default());
    let copy = add(
        &env,
        11,
        16,
        "phone-ios",
        ClientMeta {
            caption: Some("Dinner by the lake".into()),
            lat: Some(45.0),
            lon: Some(9.0),
            favorite: Some(true),
            album: Some("Holidays".into()),
            taken_at: Some(WHEN.into()),
            ..phone("A7", None)
        },
    );
    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [keeper], "drop": [copy]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["merged"], json!([{"id": copy, "into": keeper}]));
    assert_eq!(v["skipped"], json!([]));
    assert_eq!(
        v["metadata"],
        json!({"captions": 1, "location": true, "taken_at": true})
    );

    let (_, k) = call(&env.app, "GET", &format!("/v1/media/items/{keeper}"), None).await;
    assert_eq!(k["caption"], "Dinner by the lake");
    assert_eq!(k["place"]["lat"], 45.0);
    assert_eq!(k["taken_at"], WHEN);
    assert_eq!(k["favorite"], true);
    assert!(
        k["albums"].as_array().unwrap().len() >= 2,
        "Holidays and Favourites moved over: {k}"
    );
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids_of(&list), vec![keeper.clone()]);
    // The caption is searchable on the keeper.
    let (_, found) = call(&env.app, "GET", "/v1/media/items?q=lake", None).await;
    assert_eq!(ids_of(&found), vec![keeper.clone()]);
    // The link says it was a manual decision (no hashes stored).
    let (_, d) = call(
        &env.app,
        "GET",
        &format!("/v1/media/items/{keeper}/duplicates"),
        None,
    )
    .await;
    assert_eq!(d["duplicates"][0]["kind"], "manual");

    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/undo",
        Some(json!({"ids": [copy, "itm_other"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["undone"], json!([copy]));
    assert_eq!(v["not_merged"], json!(["itm_other"]));
    let (_, k) = call(&env.app, "GET", &format!("/v1/media/items/{keeper}"), None).await;
    assert!(k["caption"].is_null(), "{k}");
    assert!(k["place"].is_null());
    assert!(k["taken_at"].is_null());
    assert_eq!(k["favorite"], false);
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    let mut got = ids_of(&list);
    got.sort();
    let mut want = vec![keeper.clone(), copy.clone()];
    want.sort();
    assert_eq!(got, want);
    let (_, found) = call(&env.app, "GET", "/v1/media/items?q=lake", None).await;
    assert_eq!(ids_of(&found), vec![copy.clone()]);
    let left: i64 = env
        .lib
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM dedup_meta_undo", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn captions_join_and_a_place_needs_every_copy_to_agree() {
    let env = setup();
    let keeper = add(
        &env,
        10,
        32,
        "archive-nespc",
        ClientMeta {
            caption: Some("Original note".into()),
            ..Default::default()
        },
    );
    let near = add(
        &env,
        11,
        16,
        "web-chat",
        ClientMeta {
            caption: Some("From the chat".into()),
            lat: Some(45.0),
            lon: Some(9.0),
            ..Default::default()
        },
    );
    let far = add(
        &env,
        12,
        20,
        "folder",
        ClientMeta {
            caption: Some("Original note".into()), // already in the keeper's caption
            lat: Some(10.0),
            lon: Some(20.0),
            ..Default::default()
        },
    );
    let (_, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [keeper], "drop": [near, far]})),
    )
    .await;
    assert_eq!(v["merged"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(
        v["metadata"],
        json!({"captions": 1, "location": false, "taken_at": false})
    );
    let (_, k) = call(&env.app, "GET", &format!("/v1/media/items/{keeper}"), None).await;
    assert_eq!(k["caption"], "Original note\n\nFrom the chat");
    assert!(k["place"].is_null(), "the copies disagree on the place");

    // Undoing one copy takes only its own text away; the keeper's stays.
    let (_, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/undo",
        Some(json!({"ids": [near]})),
    )
    .await;
    assert_eq!(v["undone"], json!([near]));
    let (_, k) = call(&env.app, "GET", &format!("/v1/media/items/{keeper}"), None).await;
    assert_eq!(k["caption"], "Original note");
}

#[tokio::test]
async fn an_edited_caption_survives_the_undo_of_a_merge() {
    let env = setup();
    let keeper = add(&env, 10, 32, "archive-nespc", ClientMeta::default());
    let copy = add(
        &env,
        11,
        16,
        "web-chat",
        ClientMeta {
            caption: Some("copy text".into()),
            ..Default::default()
        },
    );
    call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [keeper], "drop": [copy]})),
    )
    .await;
    let (s, _) = call(
        &env.app,
        "PATCH",
        &format!("/v1/media/items/{keeper}"),
        Some(json!({"caption": "my own words"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    call(
        &env.app,
        "POST",
        "/v1/media/duplicates/undo",
        Some(json!({"ids": [copy]})),
    )
    .await;
    let (_, k) = call(&env.app, "GET", &format!("/v1/media/items/{keeper}"), None).await;
    assert_eq!(k["caption"], "my own words");
}

#[tokio::test]
async fn resolve_checks_its_input_and_reports_what_it_skipped() {
    let env = setup();
    let a = add(&env, 10, 32, "archive-nespc", ClientMeta::default());
    let b = add(&env, 11, 16, "web-chat", ClientMeta::default());
    let bad = |body: Value| {
        let app = env.app.clone();
        async move {
            call(&app, "POST", "/v1/media/duplicates/resolve", Some(body))
                .await
                .0
        }
    };
    assert_eq!(
        bad(json!({"keep": [], "drop": [b]})).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        bad(json!({"keep": [a], "drop": [a]})).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        bad(json!({"keep": ["../x"], "drop": [b]})).await,
        StatusCode::BAD_REQUEST
    );
    let many: Vec<String> = (0..201).map(|i| format!("id{i}")).collect();
    assert_eq!(
        bad(json!({"keep": [a], "drop": many})).await,
        StatusCode::BAD_REQUEST
    );

    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [a], "drop": ["itm_missing", b, a.clone() + "x"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["merged"], json!([{"id": b, "into": a}]));
    let reasons: Vec<&str> = v["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, vec!["missing", "missing"]);

    // A keeper that is not there skips everything, and says so.
    let (_, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": ["itm_gone"], "drop": [a]})),
    )
    .await;
    assert_eq!(v["merged"], json!([]));
    assert_eq!(v["skipped"], json!([{"id": a, "reason": "keep_missing"}]));
}

#[tokio::test]
async fn keeping_two_side_by_side_records_them_as_not_duplicates() {
    let env = setup();
    let a = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let b = add(
        &env,
        11,
        32,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    hash(&env, &a, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    hash(&env, &b, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 1);
    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [a, b], "drop": []})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["merged"], json!([]));
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 0, "the group does not come back");
}

#[tokio::test]
async fn not_duplicates_is_remembered_and_can_be_taken_back() {
    let env = setup();
    let a = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let b = add(
        &env,
        11,
        32,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    for id in [&a, &b] {
        hash(&env, id, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    }
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 1);

    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/dismiss",
        Some(json!({"ids": [b, a]})),
    )
    .await;
    assert_eq!((s, v["pairs"].clone()), (StatusCode::OK, json!(1)), "{v}");
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 0);
    // Still two photos in the Gallery.
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(ids_of(&list).len(), 2);

    let (_, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/dismiss",
        Some(json!({"ids": [a, b], "undo": true})),
    )
    .await;
    assert_eq!(v["pairs"], 1);
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 1);

    for body in [
        json!({"ids": [a]}),
        json!({"ids": [a, a]}),
        json!({"ids": ["a b", b]}),
    ] {
        let (s, _) = call(&env.app, "POST", "/v1/media/duplicates/dismiss", Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn a_merge_the_owner_undid_does_not_come_back_as_a_group() {
    let env = setup();
    let a = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let b = add(
        &env,
        11,
        32,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    for id in [&a, &b] {
        hash(&env, id, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    }
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    let g = &v["groups"][0];
    let keep = g["keep"].as_str().unwrap().to_string();
    let other = if keep == a { b.clone() } else { a.clone() };
    call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [keep], "drop": [other]})),
    )
    .await;
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 0, "merged: one photo left");
    call(
        &env.app,
        "POST",
        "/v1/media/duplicates/undo",
        Some(json!({"ids": [other]})),
    )
    .await;
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 0, "the owner split them: not proposed again");
}

#[tokio::test]
async fn apply_all_resolves_the_confident_groups_and_leaves_the_weak_ones() {
    let env = setup();
    // Group 1: the same moment, identical hashes (score 0.9 or more).
    let a1 = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let a2 = add(
        &env,
        11,
        32,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    hash(&env, &a1, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    hash(&env, &a2, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    // Group 2: no capture time and a few bits apart: a weak candidate (below 0.7).
    let b1 = add(&env, 20, 32, "web-chat", ClientMeta::default());
    let b2 = add(&env, 21, 32, "folder", ClientMeta::default());
    hash(&env, &b1, 0xF0F0_F0F0_F0F0_F0F0, 0x2222_2222_2222_2222);
    hash(&env, &b2, 0xF0F0_F0F0_F0F0_F0F7, 0x2222_2222_2222_2222);

    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 2, "{v}");
    assert_eq!(v["applicable"], 1);
    assert_eq!(
        v["groups"][0]["applicable"], true,
        "applicable groups come first"
    );
    assert_eq!(v["groups"][1]["applicable"], false);
    assert!(v["groups"][1]["score"].as_f64().unwrap() < 0.7);

    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve-all",
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["groups"], 1);
    assert_eq!(v["merged"], 1);
    assert_eq!(v["merged_ids"].as_array().unwrap().len(), 1);
    let (_, list) = call(&env.app, "GET", "/v1/media/items", None).await;
    assert_eq!(
        ids_of(&list).len(),
        3,
        "one copy hidden, the weak pair untouched"
    );
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(
        (v["total"].clone(), v["applicable"].clone()),
        (json!(1), json!(0))
    );

    // A body-less call works, and a lower bar than the apply threshold is raised to it.
    let (s, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve-all",
        Some(json!({"min_score": 0.1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["merged"], 0, "{v}");

    // The merge is undoable like any other, and recorded as an apply-all decision.
    let merged = v["merged_ids"].clone();
    assert_eq!(merged, json!([]));
    let by: String = env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT decided_by FROM item_links WHERE undone_at IS NULL",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(by, "review:all");
}

#[tokio::test]
async fn a_stacked_photo_is_not_a_duplicate_to_review_or_to_drop() {
    let env = setup();
    let a = add(&env, 10, 32, "phone-ios", phone("A1", Some(WHEN)));
    let b = add(
        &env,
        11,
        32,
        "archive-nespc",
        ClientMeta {
            taken_at: Some(WHEN.into()),
            ..Default::default()
        },
    );
    let c = add(&env, 12, 32, "folder", ClientMeta::default());
    for id in [&a, &b] {
        hash(&env, id, 0x0F0F_0F0F_0F0F_0F0F, 0x1111_1111_1111_1111);
    }
    let (s, _) = call(
        &env.app,
        "POST",
        "/v1/media/stacks",
        Some(json!({"ids": [b, c]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, v) = call(&env.app, "GET", "/v1/media/duplicates", None).await;
    assert_eq!(v["total"], 0, "the stacked photo is left out of the review");
    // Even by hand: hiding a stack's tile under another photo would lose the stack.
    let (_, v) = call(
        &env.app,
        "POST",
        "/v1/media/duplicates/resolve",
        Some(json!({"keep": [a], "drop": [b, c]})),
    )
    .await;
    assert_eq!(v["merged"], json!([]));
    let reasons: Vec<&str> = v["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, vec!["stacked", "stacked"]);
}
