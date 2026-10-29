//! Browsing the Gallery, end to end through the HTTP router with synthetic rows only: the Places
//! clusters, "on this day" and "recently added", and the archive (batch ops, the `archived` filter,
//! counts, the upgrade of an older catalog).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::Library;
use lucina_media::{http, store};
use rusqlite::params;
use serde_json::{json, Value};
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

async fn json_of(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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

/// A catalog row of our choosing: no file behind it.
#[derive(Clone)]
struct Row {
    id: &'static str,
    kind: &'static str,
    source: &'static str,
    device: Option<&'static str>,
    taken: Option<&'static str>,
    added: &'static str,
    pos: Option<(f64, f64)>,
    favorite: bool,
    size: (i64, i64),
}

impl Row {
    fn new(id: &'static str) -> Self {
        Row {
            id,
            kind: "photo",
            source: "phone-ios",
            device: Some("ios-1"),
            taken: Some("2020-06-15T10:00:00.000Z"),
            added: "2024-01-01T00:00:00.000Z",
            pos: None,
            favorite: false,
            size: (4000, 3000),
        }
    }
}

fn put(lib: &Library, r: &Row) {
    lib.db
        .with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO blobs (hash, bytes, ext, location, created_at) VALUES (?1, 1, 'bin', 'store', ?2)",
                params![format!("h-{}", r.id), r.added],
            )?;
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, taken_at, sort_at, source, device, added_at, updated_at,
                                    lat, lon, favorite, width, height)
                 VALUES (?1, ?2, ?1, ?3, 'x/y', 1, ?4, COALESCE(?4, ?5), ?6, ?7, ?5, ?5, ?8, ?9, ?10, ?11, ?12)",
                params![
                    r.id,
                    r.kind,
                    format!("h-{}", r.id),
                    r.taken,
                    r.added,
                    r.source,
                    r.device,
                    r.pos.map(|p| p.0),
                    r.pos.map(|p| p.1),
                    r.favorite,
                    r.size.0,
                    r.size.1
                ],
            )
        })
        .unwrap();
}

fn ids_in(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

async fn batch(app: &Router, op: &str, ids: &[&str]) -> (StatusCode, Value) {
    json_of(
        app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({ "op": op, "ids": ids })),
    )
    .await
}

fn failed_of(v: &Value, id: &str) -> Option<String> {
    v["failed"]
        .as_array()?
        .iter()
        .find(|f| f["id"] == id)
        .map(|f| f["error"].as_str().unwrap().to_string())
}

// ---- archive -----------------------------------------------------------------------------------

#[tokio::test]
async fn archiving_hides_from_the_grid_and_the_counts_but_deletes_nothing() {
    let (_t, lib, app) = setup();
    for id in ["a", "b", "c"] {
        put(&lib, &Row::new(id));
    }
    let (s, r) = batch(&app, "archive", &["b"]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["done"], json!(["b"]));

    let (_, all) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all",
        None,
    )
    .await;
    assert_eq!(sorted(ids_in(&all, "items")), strs(&["a", "c"]));
    assert_eq!(all["total"], 2);
    let (_, archived) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&archived=true",
        None,
    )
    .await;
    assert_eq!(ids_in(&archived, "items"), strs(&["b"]));
    // It is still there: a plain item read, the device list and months.
    let (s, _) = json_of(&app, "GET", "/v1/media/items/b", None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, months) = json_of(&app, "GET", "/v1/media/items/months?origin=all", None).await;
    assert_eq!(months["months"][0]["count"], 2);
    let (_, sources) = json_of(&app, "GET", "/v1/media/sources", None).await;
    assert_eq!(sources["all"]["photos"], 2);
    assert_eq!(sources["sources"][0]["photos"], 2);
    // The archive's own months.
    let (_, m) = json_of(
        &app,
        "GET",
        "/v1/media/items/months?origin=all&archived=true",
        None,
    )
    .await;
    assert_eq!(m["months"][0]["count"], 1);

    let (_, r) = batch(&app, "unarchive", &["b"]).await;
    assert_eq!(r["done"], json!(["b"]));
    let (_, all) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all",
        None,
    )
    .await;
    assert_eq!(sorted(ids_in(&all, "items")), strs(&["a", "b", "c"]));
    let (_, none) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&archived=true",
        None,
    )
    .await;
    assert!(ids_in(&none, "items").is_empty());
}

#[tokio::test]
async fn archive_reports_each_id_and_refuses_what_it_cannot_do() {
    let (_t, lib, app) = setup();
    put(&lib, &Row::new("photo"));
    put(&lib, &Row::new("again"));
    put(
        &lib,
        &Row {
            kind: "file",
            source: "folder",
            device: None,
            ..Row::new("doc")
        },
    );
    put(&lib, &Row::new("bin"));
    put(&lib, &Row::new("plain"));
    batch(&app, "trash", &["bin"]).await;

    let (s, r) = batch(
        &app,
        "archive",
        &["photo", "again", "doc", "bin", "ghost", "../x"],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        sorted(
            r["done"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        ),
        strs(&["again", "photo"])
    );
    assert_eq!(failed_of(&r, "doc").as_deref(), Some("not_media"));
    assert_eq!(failed_of(&r, "bin").as_deref(), Some("trashed"));
    assert_eq!(failed_of(&r, "ghost").as_deref(), Some("not_found"));
    assert_eq!(failed_of(&r, "../x").as_deref(), Some("invalid_id"));

    let (_, r) = batch(&app, "archive", &["photo"]).await;
    assert_eq!(failed_of(&r, "photo").as_deref(), Some("already_archived"));
    let (_, r) = batch(&app, "unarchive", &["again", "plain", "doc"]).await;
    assert_eq!(r["done"], json!(["again"]));
    assert_eq!(failed_of(&r, "plain").as_deref(), Some("not_archived"));
    assert_eq!(failed_of(&r, "doc").as_deref(), Some("not_media"));

    // `also_from_device` and a missing id list are still refused for the new ops.
    let (s, _) = json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "archive", "ids": ["photo"], "also_from_device": true})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = json_of(
        &app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "archive", "ids": []})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_archived_item_can_still_be_trashed_and_the_trash_shows_it_once() {
    let (_t, lib, app) = setup();
    put(&lib, &Row::new("x"));
    batch(&app, "archive", &["x"]).await;
    let (_, r) = batch(&app, "trash", &["x"]).await;
    assert_eq!(r["done"], json!(["x"]));
    let (_, trash) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&trashed=true",
        None,
    )
    .await;
    assert_eq!(ids_in(&trash, "items"), strs(&["x"]));
    // Trashed items are not "archived" listings.
    let (_, arch) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&archived=true",
        None,
    )
    .await;
    assert!(ids_in(&arch, "items").is_empty());
    // Restored, it is still archived.
    batch(&app, "restore", &["x"]).await;
    let (_, arch) = json_of(
        &app,
        "GET",
        "/v1/media/items?scope=gallery&origin=all&archived=true",
        None,
    )
    .await;
    assert_eq!(ids_in(&arch, "items"), strs(&["x"]));
}

#[tokio::test]
async fn an_older_catalog_gets_the_archive_column_and_keeps_its_items() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("catalog.db");
    {
        let db = lucina_media::Db::open(&path).unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h', 1, 'bin', 'store', 'now')",
                [],
            )?;
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 VALUES ('old', 'photo', 'IMG_9.JPG', 'h', 'image/jpeg', 1, '2020-01-01T00:00:00.000Z', 'takeout', 'now', 'now')",
                [],
            )?;
            c.execute("DROP INDEX IF EXISTS items_archived", [])?;
            c.execute("ALTER TABLE items DROP COLUMN archived", [])
        })
        .unwrap();
    }
    let db = lucina_media::Db::open(&path).unwrap();
    let archived: i64 = db
        .with(|c| {
            c.query_row("SELECT archived FROM items WHERE id = 'old'", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(archived, 0);
    drop(db);
    // A second open changes nothing.
    lucina_media::Db::open(&path).unwrap();
}

// ---- places ------------------------------------------------------------------------------------

fn seed_places(lib: &Library) {
    // Three pictures in one city, one a favorite; two in another country; one with no position.
    let city = [
        ("c1", 55.6761, 12.5683, false),
        ("c2", 55.6770, 12.5700, true),
        ("c3", 55.6800, 12.5750, false),
    ];
    for (id, lat, lon, fav) in city {
        put(
            lib,
            &Row {
                pos: Some((lat, lon)),
                favorite: fav,
                ..Row::new(id)
            },
        );
    }
    put(
        lib,
        &Row {
            pos: Some((-33.87, 151.21)),
            device: Some("mac-1"),
            source: "mac",
            ..Row::new("s1")
        },
    );
    put(
        lib,
        &Row {
            pos: Some((-33.88, 151.20)),
            device: Some("mac-1"),
            source: "mac",
            kind: "video",
            ..Row::new("s2")
        },
    );
    put(lib, &Row::new("nowhere"));
}

#[tokio::test]
async fn places_group_pictures_into_clusters_with_a_cover() {
    let (_t, lib, app) = setup();
    seed_places(&lib);
    let (s, r) = json_of(&app, "GET", "/v1/media/items/places?zoom=4", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        r["total"], 5,
        "the picture without a position is not on the map"
    );
    let clusters = r["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 2);
    assert_eq!(clusters[0]["count"], 3);
    // The favorite stands for the city; the cover is a full item with its thumbnail URLs.
    assert_eq!(clusters[0]["cover"]["id"], "c2");
    assert!(clusters[0]["cover"]["urls"]["original"]
        .as_str()
        .unwrap()
        .contains("/items/c2/"));
    assert_eq!(clusters[1]["count"], 2);
    let bbox = clusters[0]["bbox"].as_array().unwrap();
    assert_eq!(bbox.len(), 4);
    assert_eq!(bbox[0], 12.5683);
    assert_eq!(bbox[3], 55.68);
    assert!((clusters[0]["lat"].as_f64().unwrap() - 55.6777).abs() < 0.01);

    // Zoomed all the way in, every picture is alone.
    let (_, z) = json_of(&app, "GET", "/v1/media/items/places?zoom=22", None).await;
    assert_eq!(z["clusters"].as_array().unwrap().len(), 5);
    // Zoomed out, the two places are still apart or one: counts always add up.
    let (_, w) = json_of(&app, "GET", "/v1/media/items/places?zoom=0", None).await;
    let sum: i64 = w["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["count"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, 5);
}

#[tokio::test]
async fn places_follow_the_map_window_and_the_gallery_filters() {
    let (_t, lib, app) = setup();
    seed_places(&lib);
    // Only Copenhagen's window.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/places?zoom=10&bbox=12,55,13,56",
        None,
    )
    .await;
    assert_eq!(r["total"], 3);
    // A window across the date line holds Sydney's pictures only when it spans 151 degrees.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/places?zoom=10&bbox=150,-35,-170,-30",
        None,
    )
    .await;
    assert_eq!(r["total"], 2);
    // Filters: a device, the kind, favorites, the archive and the trash.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/places?zoom=22&origin=device:mac-1",
        None,
    )
    .await;
    assert_eq!(r["total"], 2);
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/places?zoom=22&kind=video",
        None,
    )
    .await;
    assert_eq!(r["total"], 1);
    assert_eq!(r["clusters"][0]["cover"]["id"], "s2");
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/places?zoom=22&favorite=true",
        None,
    )
    .await;
    assert_eq!(r["total"], 1);
    batch(&app, "archive", &["c1"]).await;
    batch(&app, "trash", &["c3"]).await;
    let (_, r) = json_of(&app, "GET", "/v1/media/items/places?zoom=22", None).await;
    assert_eq!(r["total"], 3);
    let (_, r) = json_of(&app, "GET", "/v1/media/items/places?zoom=22&limit=2", None).await;
    assert_eq!(r["clusters"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn places_refuse_bad_parameters() {
    let (_t, _lib, app) = setup();
    for uri in [
        "/v1/media/items/places?bbox=1,2,3",
        "/v1/media/items/places?bbox=0,50,10,40",
        "/v1/media/items/places?zoom=-1",
        "/v1/media/items/places?zoom=25",
        "/v1/media/items/places?origin=nope",
    ] {
        let (s, _) = json_of(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}");
    }
    let (s, r) = json_of(&app, "GET", "/v1/media/items/places", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["clusters"], json!([]));
}

#[tokio::test]
async fn the_listing_filters_by_a_box_so_a_cluster_opens_its_pictures() {
    let (_t, lib, app) = setup();
    seed_places(&lib);
    let (_, places) = json_of(&app, "GET", "/v1/media/items/places?zoom=4", None).await;
    let city = &places["clusters"][0];
    let bbox: Vec<String> = city["bbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.to_string())
        .collect();
    let uri = format!(
        "/v1/media/items?scope=gallery&origin=all&bbox={}",
        bbox.join(",")
    );
    let (s, list) = json_of(&app, "GET", &uri, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sorted(ids_in(&list, "items")), strs(&["c1", "c2", "c3"]));
    assert_eq!(list["total"], 3);
    let (s, _) = json_of(&app, "GET", "/v1/media/items?bbox=nope", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---- memories ----------------------------------------------------------------------------------

#[tokio::test]
async fn on_this_day_groups_previous_years_and_skips_bursts() {
    let (_t, lib, app) = setup();
    let on = |id: &'static str, taken: &'static str| Row {
        taken: Some(taken),
        ..Row::new(id)
    };
    // 2023: a burst of three, one favorite elsewhere in the day, one in the evening.
    put(&lib, &on("b1", "2023-10-28T09:00:00.000Z"));
    put(
        &lib,
        &Row {
            size: (6000, 4000),
            ..on("b2", "2023-10-28T09:00:10.000Z")
        },
    );
    put(&lib, &on("b3", "2023-10-28T09:00:20.000Z"));
    put(
        &lib,
        &Row {
            favorite: true,
            size: (10, 10),
            ..on("fav", "2023-10-28T13:00:00.000Z")
        },
    );
    put(&lib, &on("eve", "2023-10-28T20:00:00.000Z"));
    // 2021: one video. 2026 (this year) and other days do not count.
    put(
        &lib,
        &Row {
            kind: "video",
            ..on("clip", "2021-10-28T12:00:00.000Z")
        },
    );
    put(&lib, &on("now", "2026-10-28T08:00:00.000Z"));
    put(&lib, &on("other", "2023-10-29T09:00:00.000Z"));
    // No taken date: never a memory, even if it was added today.
    put(
        &lib,
        &Row {
            taken: None,
            added: "2022-10-28T09:00:00.000Z",
            ..Row::new("undated")
        },
    );

    let (s, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=10-28&year=2026",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["date"], "10-28");
    let years = r["years"].as_array().unwrap();
    assert_eq!(years.len(), 2);
    assert_eq!(years[0]["year"], 2023);
    assert_eq!(years[0]["years_ago"], 3);
    assert_eq!(years[0]["total"], 5);
    // Time order; the burst counts once (its biggest picture); the favorite and the evening one stay.
    assert_eq!(ids_in(&years[0], "items"), strs(&["b2", "fav", "eve"]));
    assert_eq!(years[1]["year"], 2021);
    assert_eq!(years[1]["total"], 1);
    assert_eq!(years[1]["items"][0]["id"], "clip");
    assert_eq!(years[1]["items"][0]["kind"], "video");

    // Fewer per year: the favorite first, then the biggest.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=10-28&year=2026&per_year=1",
        None,
    )
    .await;
    assert_eq!(ids_in(&r["years"][0], "items"), strs(&["fav"]));
    // Photos only; a device that has none.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=10-28&year=2026&kind=photo",
        None,
    )
    .await;
    assert_eq!(r["years"].as_array().unwrap().len(), 1);
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=10-28&year=2026&origin=chat",
        None,
    )
    .await;
    assert_eq!(r["years"], json!([]));
    // Looking from 2023, only older years are previous.
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=10-28&year=2023",
        None,
    )
    .await;
    assert_eq!(r["years"].as_array().unwrap().len(), 1);
    assert_eq!(r["years"][0]["year"], 2021);
}

#[tokio::test]
async fn memories_leave_out_archived_and_trashed_pictures() {
    let (_t, lib, app) = setup();
    for (id, taken) in [
        ("keep", "2022-03-01T09:00:00.000Z"),
        ("arch", "2022-03-01T11:00:00.000Z"),
        ("bin", "2022-03-01T15:00:00.000Z"),
    ] {
        put(
            &lib,
            &Row {
                taken: Some(taken),
                ..Row::new(id)
            },
        );
    }
    batch(&app, "archive", &["arch"]).await;
    batch(&app, "trash", &["bin"]).await;
    let (_, r) = json_of(
        &app,
        "GET",
        "/v1/media/items/on-this-day?date=03-01&year=2026",
        None,
    )
    .await;
    assert_eq!(ids_in(&r["years"][0], "items"), strs(&["keep"]));
    assert_eq!(r["years"][0]["total"], 1);
}

#[tokio::test]
async fn on_this_day_defaults_to_today_and_refuses_bad_dates() {
    let (_t, lib, app) = setup();
    put(&lib, &Row::new("x"));
    let (s, r) = json_of(&app, "GET", "/v1/media/items/on-this-day", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(r["date"].as_str().unwrap().len() == 5 && r["year"].as_i64().unwrap() >= 2026);
    for uri in [
        "/v1/media/items/on-this-day?date=13-01",
        "/v1/media/items/on-this-day?date=04-31",
        "/v1/media/items/on-this-day?date=2026-10-28",
        "/v1/media/items/on-this-day?date=10-28&year=0",
        "/v1/media/items/on-this-day?date=10-28&origin=nope",
    ] {
        let (s, _) = json_of(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}");
    }
    // Feb 29 is a day.
    let (s, _) = json_of(&app, "GET", "/v1/media/items/on-this-day?date=02-29", None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn recently_added_lists_the_newest_arrivals_first() {
    let (_t, lib, app) = setup();
    let now = chrono::Utc::now();
    let ago = |d: i64| {
        (now - chrono::Duration::days(d)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let (a, b, c, d) = (ago(0), ago(2), ago(40), ago(400));
    // Taken long ago, added recently: it is "recent" by arrival.
    for (id, added) in [("new", &a), ("mid", &b), ("old", &c), ("older", &d)] {
        let added: &'static str = Box::leak(added.clone().into_boxed_str());
        put(
            &lib,
            &Row {
                added,
                taken: Some("2010-01-01T00:00:00.000Z"),
                ..Row::new(id)
            },
        );
    }
    let (s, r) = json_of(&app, "GET", "/v1/media/items/recent", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ids_in(&r, "items"), strs(&["new", "mid", "old", "older"]));
    assert_eq!(r["total"], 4);
    let (_, r) = json_of(&app, "GET", "/v1/media/items/recent?days=7", None).await;
    assert_eq!(ids_in(&r, "items"), strs(&["new", "mid"]));
    assert_eq!(r["total"], 2);
    let (_, r) = json_of(&app, "GET", "/v1/media/items/recent?limit=1", None).await;
    assert_eq!(ids_in(&r, "items"), strs(&["new"]));
    assert_eq!(r["total"], 4);
    batch(&app, "archive", &["new"]).await;
    let (_, r) = json_of(&app, "GET", "/v1/media/items/recent?days=7", None).await;
    assert_eq!(ids_in(&r, "items"), strs(&["mid"]));
    for uri in [
        "/v1/media/items/recent?days=0",
        "/v1/media/items/recent?days=99999",
        "/v1/media/items/recent?origin=x",
    ] {
        let (s, _) = json_of(&app, "GET", uri, None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn the_new_routes_do_not_shadow_an_item_called_like_them() {
    let (_t, lib, app) = setup();
    put(&lib, &Row::new("places-not-an-id"));
    let (s, r) = json_of(&app, "GET", "/v1/media/items/places-not-an-id", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["id"], "places-not-an-id");
}
