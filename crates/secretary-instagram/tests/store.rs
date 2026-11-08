//! Pages from the store, refreshes behind them, the background download (docs/instagram.md, "Local first").

mod common;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::*;
use secretary_instagram::service::MirrorOutcome;
use secretary_instagram::store::FeedKind;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

const REELS: &str = "/api/v1/users/natgeo/reels";

/// Everything an account's first visit asks, all there.
async fn mount_full_account(e: &Env) {
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(vec![item("s1", "image", "s1.jpg", &[])], None),
    )
    .await;
    mount_json(
        &e.server,
        REELS,
        page(vec![item("DeRRRRRRRR1", "video", "r1.mp4", &[])], None),
    )
    .await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/highlights",
        json!([{"id": "highlight:1", "title": "A", "cover_url": worker(&cdn("h.jpg")), "media_count": 1}]),
    )
    .await;
    mount_json(
        &e.server,
        "/api/v1/highlights/items",
        page(vec![item("h1", "image", "h1.jpg", &[])], None),
    )
    .await;
}

/// As if every answer were old: nothing fresh in the store or the JSON cache.
fn age_everything(e: &Env) {
    let s = &e.st.service.store;
    for kind in [FeedKind::Posts, FeedKind::Reels, FeedKind::Stories] {
        s.update_feed(kind, "natgeo", |f| f.fetched_at = 1);
    }
    s.update_profile("natgeo", |d| {
        d.fetched_at = 1;
        d.highlights_at = 1;
    });
    let json_dir = e.dir.path().join("cache/json");
    for f in std::fs::read_dir(json_dir).unwrap().flatten() {
        std::fs::remove_file(f.path()).unwrap();
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

async fn mirror_until_idle(e: &Env) {
    for _ in 0..200 {
        match e.st.service.mirror_tick().await {
            MirrorOutcome::Idle => return,
            MirrorOutcome::Busy => tokio::time::sleep(Duration::from_millis(5)).await,
            _ => {}
        }
    }
    panic!("the download did not finish");
}

#[tokio::test]
async fn a_repeat_visit_answers_from_the_store_while_the_backend_is_down() {
    let e = env().await;
    mount_full_account(&e).await;
    let first = settled(&e.app, "natgeo").await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(
        get(&e.app, "/v1/instagram/stories/natgeo").await.status,
        StatusCode::OK
    );
    // The bundle had a tray: no second call for it.
    assert_eq!(first.json["data"]["highlights"][0]["title"], "BOTW");

    e.server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_delay(Duration::from_secs(3)))
        .mount(&e.server)
        .await;
    age_everything(&e);
    let t = Instant::now();
    for route in ["profile", "posts", "reels", "stories", "highlights"] {
        let r = get(&e.app, &format!("/v1/instagram/{route}/natgeo")).await;
        assert_eq!(r.status, StatusCode::OK, "{route}");
    }
    let tiles = get(&e.app, "/v1/instagram/reels/natgeo").await;
    assert_eq!(ids(&tiles.json["data"]), ["DeRRRRRRRR1"]);
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn a_visit_within_the_ttl_asks_the_backend_nothing_more() {
    let e = env().await;
    mount_full_account(&e).await;
    settled(&e.app, "natgeo").await;
    let before = e.server.received_requests().await.unwrap().len();
    for _ in 0..3 {
        settled(&e.app, "natgeo").await;
        get(&e.app, "/v1/instagram/reels/natgeo").await;
        get(&e.app, "/v1/instagram/stories/natgeo").await;
    }
    assert_eq!(e.server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn a_refresh_stops_at_the_first_known_reel() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    mount_json(
        &e.server,
        REELS,
        page(
            vec![
                item("r2", "video", "r2.mp4", &[]),
                item("r1", "video", "r1.mp4", &[]),
            ],
            Some("TAIL"),
        ),
    )
    .await;
    settled(&e.app, "natgeo").await;

    // Two pages are new, the second reaches a known reel: the third is never asked.
    e.server.reset().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    Mock::given(method("GET"))
        .and(path(REELS))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                item("r5", "video", "r5.mp4", &[]),
                item("r4", "video", "r4.mp4", &[]),
            ],
            Some("P2"),
        )))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path(REELS))
        .and(query_param("cursor", "P2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                item("r3", "video", "r3.mp4", &[]),
                item("r2", "video", "r2.mp4", &[]),
            ],
            Some("P3"),
        )))
        .mount(&e.server)
        .await;
    age_everything(&e);
    e.st.service.refresh_account("natgeo").await.unwrap();

    let feed = e.st.service.store.feed(FeedKind::Reels, "natgeo");
    let got: Vec<&str> = feed.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(got, ["r5", "r4", "r3", "r2", "r1"]);
    assert_eq!(
        feed.next_cursor.as_deref(),
        Some("TAIL"),
        "older items still start there"
    );
    let asked: Vec<Option<String>> = e
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == REELS)
        .map(|r| r.url.query().map(str::to_string))
        .collect();
    assert_eq!(asked, [None, Some("cursor=P2".to_string())]);
}

#[tokio::test]
async fn long_feeds_page_through_the_store_then_the_backend() {
    let e = env().await;
    let items: Vec<Value> = (0..60)
        .map(|i| item(&format!("r{i}"), "video", "r.mp4", &[]))
        .collect();
    Mock::given(method("GET"))
        .and(path(REELS))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(items, Some("TAIL"))))
        .mount(&e.server)
        .await;
    get(&e.app, "/v1/instagram/reels/natgeo").await;
    let r = get(&e.app, "/v1/instagram/reels/natgeo").await;
    assert_eq!(ids(&r.json["data"]).len(), 48);
    assert_eq!(r.json["data"]["nextCursor"], "d48");
    let before = count(&e.server, REELS).await;
    let r = get(&e.app, "/v1/instagram/reels/natgeo?cursor=d48").await;
    assert_eq!(ids(&r.json["data"]).len(), 12);
    assert_eq!(r.json["data"]["nextCursor"], "TAIL");
    assert_eq!(
        count(&e.server, REELS).await,
        before,
        "a page of the store asks nobody"
    );
}

#[tokio::test]
async fn an_expired_story_keeps_playing_from_its_local_copy() {
    let e = env().await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(
            vec![
                item("kept", "image", "kept.jpg", &[]),
                item("lost", "image", "lost.jpg", &[]),
            ],
            None,
        ),
    )
    .await;
    mount_media(&e.server, "kept.jpg", JPEG, "image/jpeg").await;
    let r = get(&e.app, "/v1/instagram/stories/natgeo").await;
    assert_eq!(ids(&r.json["data"]), ["kept", "lost"]);
    assert_eq!(r.json["data"]["items"][0].get("expired"), None);
    // Only one of them was downloaded.
    let url = r.json["data"]["items"][0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(&e.app, "GET", &url, None, None, &[]).await.status,
        StatusCode::OK
    );

    // Both are gone upstream, and the CDN links with them.
    e.server.reset().await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(vec![item("today", "image", "today.jpg", &[])], None),
    )
    .await;
    age_everything(&e);
    e.st.service.store.update_profile("natgeo", |d| {
        d.profile = Some(serde_json::from_value(bundle()["profile"].clone()).unwrap());
        d.fetched_at = chrono::Utc::now().timestamp();
    });
    e.st.service.refresh_account("natgeo").await.unwrap();
    let r = get(&e.app, "/v1/instagram/stories/natgeo").await;
    assert_eq!(
        ids(&r.json["data"]),
        ["today", "kept"],
        "no copy, nothing to show"
    );
    assert_eq!(r.json["data"]["items"][1]["expired"], true);
    assert_eq!(r.json["data"]["items"][0].get("expired"), None);
    let url = r.json["data"]["items"][1]["url"]
        .as_str()
        .unwrap()
        .to_string();
    let played = send(&e.app, "GET", &url, None, None, &[]).await;
    assert_eq!(played.status, StatusCode::OK);
    assert_eq!(played.bytes, JPEG);
    assert_eq!(count(&e.server, "/cdn/").await, 0);
}

#[tokio::test]
async fn watched_media_is_downloaded_in_the_background_and_outlives_its_cdn_link() {
    let e = env().await;
    mount_full_account(&e).await;
    e.st.service.watch_put("natgeo", Some(false)).unwrap();
    for f in [
        "pfp.jpg",
        "h.jpg",
        "p1.jpg",
        "p1.jpg_thumb.jpg",
        "s1.jpg",
        "s1.jpg_thumb.jpg",
    ] {
        mount_media(&e.server, f, JPEG, "image/jpeg").await;
    }
    for f in ["p2.mp4", "r1.mp4"] {
        mount_media(&e.server, f, JPEG, "video/mp4").await;
    }
    let r = settled(&e.app, "natgeo").await;
    mirror_until_idle(&e).await;
    assert!(count(&e.server, "/cdn/").await >= 6);

    // The CDN refuses everything from now on; what was downloaded is still served.
    e.server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&e.server)
        .await;
    let pic = r.json["data"]["profile"]["picUrl"].as_str().unwrap();
    assert_eq!(send(&e.app, "GET", pic, None, None, &[]).await.bytes, JPEG);
    let post = r.json["data"]["posts"]["items"][0]["url"].as_str().unwrap();
    assert_eq!(
        send(&e.app, "GET", post, None, None, &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(count(&e.server, "/cdn/").await, 0);
}

#[tokio::test]
async fn a_link_the_cdn_refuses_is_left_alone_for_a_while() {
    let e = env().await;
    e.st.service.watch_put("natgeo", Some(false)).unwrap();
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(vec![item("s", "image", "s.jpg", &[])], None),
    )
    .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&e.server)
        .await;
    get(&e.app, "/v1/instagram/stories/natgeo").await;
    mirror_until_idle(&e).await;
    let asked = count(&e.server, "/cdn/").await;
    assert!(asked >= 1);
    get(&e.app, "/v1/instagram/stories/natgeo").await;
    e.st.service.mirror_account("natgeo").await;
    mirror_until_idle(&e).await;
    assert_eq!(count(&e.server, "/cdn/").await, asked);
}

#[tokio::test]
async fn only_links_this_service_signed_are_signed_again() {
    let e = env().await;
    let signer = &e.st.service.signer;
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    let now = chrono::Utc::now().timestamp();
    let old = signer.url_with_exp(&cdn("p1.jpg"), now - 86_400);
    assert_eq!(
        send(&e.app, "GET", &old, None, None, &[]).await.status,
        StatusCode::FORBIDDEN
    );

    let body = json!({"urls": [old, "/v1/instagram/s/1/ffff/abcd", cdn("p1.jpg")]});
    let unauth = send(
        &e.app,
        "POST",
        "/v1/instagram/resign",
        None,
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(unauth.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &e.app,
        "POST",
        "/v1/instagram/resign",
        Some(SECRET),
        Some(body),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let map = r.json["data"]["urls"].as_object().unwrap();
    assert_eq!(map.len(), 1, "{map:?}");
    let fresh = map[&old].as_str().unwrap();
    assert_eq!(
        send(&e.app, "GET", fresh, None, None, &[]).await.bytes,
        JPEG
    );

    let many = json!({"urls": vec!["x"; 501]});
    let r = send(
        &e.app,
        "POST",
        "/v1/instagram/resign",
        Some(SECRET),
        Some(many),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_watched_account_stays_through_eviction() {
    let e = env_with(Opts {
        cache_max: 2_500,
        ..Default::default()
    })
    .await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(vec![item("s", "image", "kept.jpg", &[])], None),
    )
    .await;
    mount_media(&e.server, "kept.jpg", &vec![9u8; 500], "image/jpeg").await;
    let svc = &e.st.service;
    svc.watch_put("natgeo", Some(false)).unwrap();
    let r = get(&e.app, "/v1/instagram/stories/natgeo").await;
    let url = r.json["data"]["items"][0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(&e.app, "GET", &url, None, None, &[]).await.status,
        StatusCode::OK
    );
    svc.pin_kept_media();
    for n in 1..=4 {
        let file = format!("other{n}.jpg");
        mount_media(&e.server, &file, &vec![n as u8; 1000], "image/jpeg").await;
        let signed = svc.signer.url_for(&cdn(&file));
        assert_eq!(
            send(&e.app, "GET", &signed, None, None, &[]).await.status,
            StatusCode::OK
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let before = count(&e.server, "/cdn/").await;
    assert_eq!(
        send(&e.app, "GET", &url, None, None, &[]).await.status,
        StatusCode::OK
    );
    assert_eq!(
        count(&e.server, "/cdn/").await,
        before,
        "served from disk, not fetched again"
    );
}

#[tokio::test]
async fn an_opened_account_that_is_not_watched_is_only_cached() {
    let e = env().await;
    mount_full_account(&e).await;
    let r = settled(&e.app, "natgeo").await;
    get(&e.app, "/v1/instagram/stories/natgeo").await;
    get(&e.app, "/v1/instagram/highlights/items?id=highlight%3A1").await;
    let svc = &e.st.service;
    assert_eq!(svc.mirror_tick().await, MirrorOutcome::Idle);
    assert!(!svc.backfill_tick().await);
    assert_eq!(count(&e.server, "/cdn/").await, 0);
    // A tile the owner looks at is fetched and cached as ever.
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    let post = r.json["data"]["posts"]["items"][0]["url"].as_str().unwrap();
    assert_eq!(send(&e.app, "GET", post, None, None, &[]).await.bytes, JPEG);
    assert_eq!(count(&e.server, "/cdn/").await, 1);
    // A repeat visit still renders from the store.
    e.server.reset().await;
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(ids(&r.json["data"]["posts"]).len(), 3);
}

#[tokio::test]
async fn unwatching_stops_the_download_of_what_is_left() {
    let e = env().await;
    mount_full_account(&e).await;
    let svc = &e.st.service;
    svc.watch_put("natgeo", Some(false)).unwrap();
    settled(&e.app, "natgeo").await;
    svc.mirror_account("natgeo").await;
    svc.watch_delete("natgeo").unwrap();
    for _ in 0..200 {
        if svc.mirror_tick().await == MirrorOutcome::Idle {
            break;
        }
    }
    assert_eq!(svc.mirror_tick().await, MirrorOutcome::Idle);
    assert_eq!(count(&e.server, "/cdn/").await, 0);
}

#[tokio::test]
async fn an_account_watched_before_the_store_existed_is_filled_by_the_backfill() {
    let e = env().await;
    mount_full_account(&e).await;
    let svc = &e.st.service;
    svc.watch_put("natgeo", Some(false)).unwrap();
    // A name that no longer answers is tried once, and does not hold the others up.
    svc.watch_put("aaa_dead", Some(false)).unwrap();
    assert!(svc.store.profile("natgeo").profile.is_none());
    assert!(svc.backfill_tick().await, "the dead name");
    assert!(svc.backfill_tick().await, "the account");
    assert!(svc.store.profile("natgeo").profile.is_some());
    assert_eq!(svc.store.feed(FeedKind::Reels, "natgeo").items.len(), 1);
    assert_eq!(count(&e.server, "/api/v1/users/aaa_dead/bundle").await, 1);
    assert!(
        matches!(
            svc.mirror_tick().await,
            MirrorOutcome::Fetched | MirrorOutcome::Failed
        ),
        "its media is on the queue"
    );
    // The tray's highlight is the next call, then nothing is left, and the dead name stays alone.
    assert!(svc.backfill_tick().await);
    assert!(!svc.backfill_tick().await);
    assert_eq!(count(&e.server, "/api/v1/users/aaa_dead/bundle").await, 1);
}

#[tokio::test]
async fn the_backfill_walks_older_pages_and_stops_when_nothing_is_new() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", {
        let mut b = bundle();
        b["highlights"] = json!([]);
        b
    })
    .await;
    Mock::given(method("GET"))
        .and(path(REELS))
        .and(query_param_is_missing("cursor"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![item("r3", "video", "r3.mp4", &[])], Some("C1"))),
        )
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path(REELS))
        .and(query_param("cursor", "C1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![item("r2", "video", "r2.mp4", &[])], Some("C2"))),
        )
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path(REELS))
        .and(query_param("cursor", "C2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![item("r2", "video", "r2.mp4", &[])], Some("C3"))),
        )
        .mount(&e.server)
        .await;
    e.st.service.watch_put("natgeo", Some(false)).unwrap();
    settled(&e.app, "natgeo").await;
    let svc = &e.st.service;
    assert!(svc.backfill_tick().await, "C1");
    assert!(svc.backfill_tick().await, "C2");
    assert!(
        !svc.backfill_tick().await,
        "the cursor goes on, the items do not"
    );
    let feed = svc.store.feed(FeedKind::Reels, "natgeo");
    let got: Vec<&str> = feed.items.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(got, ["r3", "r2"]);
    assert_eq!(feed.next_cursor, None);
}

#[tokio::test]
async fn a_known_account_that_went_away_keeps_what_is_there_and_says_so() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    settled(&e.app, "natgeo").await;
    e.server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&e.server)
        .await;
    age_everything(&e);
    e.st.service.refresh_account("natgeo").await.unwrap();
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["gone"], true);
    assert_eq!(r.json["data"]["profile"]["username"], "natgeo");
    assert_eq!(ids(&r.json["data"]["posts"]).len(), 3);
    // One never seen is still a plain 404.
    assert_eq!(
        get(&e.app, "/v1/instagram/profile/ghost").await.status,
        StatusCode::NOT_FOUND
    );
}
