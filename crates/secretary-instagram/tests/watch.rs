//! Watching: the warmer's checks, auto-saving new stories, badges, change notices, the 429 pause,
//! and the Archive. Same harness as tests/service.rs (one wiremock server plays the StoryView
//! backend, the Instagram CDN, lucina-media and secretary-server).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::*;
use secretary_instagram::service::{TickOutcome, WatchConfig};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, Request as MockRequest, Respond, ResponseTemplate};

/// lucina-media's ingest: numbers the items it creates.
struct Ingest(AtomicUsize);
impl Respond for Ingest {
    fn respond(&self, _: &MockRequest) -> ResponseTemplate {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        ResponseTemplate::new(200)
            .set_body_json(json!({"item_id": format!("itm_{n}"), "duplicate": false, "version": 1}))
    }
}

async fn mount_ingest(e: &Env) {
    Mock::given(method("POST"))
        .and(path("/internal/v1/ingest"))
        .respond_with(Ingest(AtomicUsize::new(0)))
        .mount(&e.server)
        .await;
}

async fn mount_notify(e: &Env) {
    Mock::given(method("POST"))
        .and(path("/v1/notify"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"status": "created"})))
        .mount(&e.server)
        .await;
}

/// The backend: a profile bundle and a stories page for `natgeo`, and the story media on the CDN.
async fn mount_account(e: &Env, private: bool, user_id: &str, posts: &[&str], stories: &[&str]) {
    let mut b = bundle();
    b["profile"]["user_id"] = json!(user_id);
    b["profile"]["is_private"] = json!(private);
    b["posts"] = page(
        posts
            .iter()
            .map(|i| item(i, "image", &format!("{i}.jpg"), &[]))
            .collect(),
        None,
    );
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", b).await;
    let items: Vec<Value> = stories
        .iter()
        .map(|s| {
            let mut it = item(s, "image", &format!("{s}.jpg"), &[]);
            it["taken_at"] = json!(1_700_000_000);
            it["caption"] = Value::Null;
            it
        })
        .collect();
    mount_json(&e.server, "/api/v1/users/natgeo/stories", page(items, None)).await;
    for s in stories {
        mount_media(
            &e.server,
            &format!("{s}.jpg"),
            &vec![1u8; 1000],
            "image/jpeg",
        )
        .await;
    }
}

/// A fresh backend without the notify mock (a test mounts its own).
async fn reset_backend(e: &Env, private: bool, user_id: &str, posts: &[&str], stories: &[&str]) {
    e.server.reset().await;
    mount_ingest(e).await;
    mount_account(e, private, user_id, posts, stories).await;
}

/// A fresh backend (the previous mocks and request log are dropped), keeping the notify/ingest mocks.
async fn remount(e: &Env, private: bool, user_id: &str, posts: &[&str], stories: &[&str]) {
    e.server.reset().await;
    mount_ingest(e).await;
    mount_notify(e).await;
    mount_account(e, private, user_id, posts, stories).await;
}

async fn watch(e: &Env, user: &str, body: Value) -> Resp {
    send(
        &e.app,
        "PUT",
        &format!("/v1/instagram/watch/{user}"),
        Some(SECRET),
        Some(body),
        &[],
    )
    .await
}

async fn requests_to(e: &Env, prefix: &str) -> Vec<wiremock::Request> {
    e.server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().starts_with(prefix))
        .collect()
}

fn body_of(r: &wiremock::Request) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn every_watch_route_needs_the_gateway_header() {
    let e = env().await;
    for (m, uri) in [
        ("GET", "/v1/instagram/watch"),
        ("PUT", "/v1/instagram/watch/natgeo"),
        ("DELETE", "/v1/instagram/watch/natgeo"),
        ("GET", "/v1/instagram/watch/natgeo/new"),
        ("POST", "/v1/instagram/watch/natgeo/opened"),
        ("GET", "/v1/instagram/badges"),
        ("GET", "/v1/instagram/archive/natgeo"),
    ] {
        for gw in [None, Some("wrong")] {
            let r = send(&e.app, m, uri, gw, None, &[]).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {uri}");
        }
    }
    assert_eq!(e.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn the_watch_list_toggle_and_removal() {
    let e = env().await;
    let r = watch(&e, "@NatGeo", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["username"], "natgeo");
    assert_eq!(r.json["data"]["autoSave"], true, "on by default");
    let r = watch(&e, "natgeo", json!({"autoSave": false})).await;
    assert_eq!(r.json["data"]["autoSave"], false);
    // No body at all keeps the setting.
    let r = send(
        &e.app,
        "PUT",
        "/v1/instagram/watch/natgeo",
        Some(SECRET),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["autoSave"], false);
    let r = send(
        &e.app,
        "PUT",
        "/v1/instagram/watch/natgeo",
        Some(SECRET),
        Some(json!({"autoSave": "yes"})),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        watch(&e, "a%20b", json!({})).await.status,
        StatusCode::BAD_REQUEST
    );

    let r = get(&e.app, "/v1/instagram/watch").await;
    let list = r.json["data"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    for key in [
        "username",
        "autoSave",
        "lastCheck",
        "newStories",
        "newPosts",
        "private",
    ] {
        assert!(list[0].get(key).is_some(), "{key}");
    }
    assert_eq!(list[0]["lastCheck"], Value::Null);
    assert_eq!(list[0]["newStories"], 0);

    let r = send(
        &e.app,
        "DELETE",
        "/v1/instagram/watch/natgeo",
        Some(SECRET),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.json["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let r = send(
        &e.app,
        "DELETE",
        "/v1/instagram/watch/natgeo",
        Some(SECRET),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&e.app, "/v1/instagram/watch/natgeo/new").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &e.app,
            "POST",
            "/v1/instagram/watch/natgeo/opened",
            Some(SECRET),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn new_stories_are_saved_once_and_the_badge_clears_on_open() {
    let e = env().await;
    mount_ingest(&e).await;
    mount_account(&e, false, "787132", &["p1", "p2"], &["s1", "s2"]).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;

    // First check: the stories that are up are saved, with the album per account; nothing is "new".
    svc.check_account("natgeo").await.unwrap();
    let ingests = requests_to(&e, "/internal/v1/ingest").await;
    assert_eq!(ingests.len(), 2);
    let a = body_of(&ingests[0]);
    assert_eq!(a["source"], "instagram");
    assert_eq!(a["album"], "Instagram \u{b7} @natgeo");
    assert_eq!(a["caption"], "@natgeo");
    assert_eq!(a["move"], true);
    assert_eq!(a["mime"], "image/jpeg");
    assert_eq!(a["taken_at"], "2023-11-14T22:13:20+00:00");
    assert!(
        std::path::Path::new(a["path"].as_str().unwrap()).starts_with(e.dir.path().join("spool"))
    );
    assert_eq!(
        get(&e.app, "/v1/instagram/badges").await.json["data"],
        json!({"total": 0, "accounts": {}})
    );

    // The same stories again: nothing is saved twice.
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(requests_to(&e, "/internal/v1/ingest").await.len(), 2);

    // A new story and a new post.
    remount(
        &e,
        false,
        "787132",
        &["p0", "p1", "p2"],
        &["s1", "s2", "s3"],
    )
    .await;
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(
        requests_to(&e, "/internal/v1/ingest").await.len(),
        1,
        "only s3"
    );
    assert_eq!(
        svc.watch.get("natgeo").unwrap().saved_stories,
        vec!["s1", "s2", "s3"]
    );

    let r = get(&e.app, "/v1/instagram/watch").await;
    assert_eq!(r.json["data"][0]["newStories"], 1);
    assert_eq!(r.json["data"][0]["newPosts"], 1);
    assert_eq!(r.json["data"][0]["private"], false);
    assert!(r.json["data"][0]["lastCheck"].as_i64().unwrap() > 0);
    assert_eq!(
        get(&e.app, "/v1/instagram/badges").await.json["data"],
        json!({"total": 2, "accounts": {"natgeo": 2}})
    );

    let r = get(&e.app, "/v1/instagram/watch/natgeo/new").await;
    assert_eq!(r.json["data"]["stories"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["data"]["stories"][0]["id"], "s3");
    assert_eq!(r.json["data"]["posts"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["data"]["posts"][0]["id"], "p0");
    assert!(r.json["data"]["posts"][0]["url"]
        .as_str()
        .unwrap()
        .starts_with("/v1/instagram/s/"));

    // Opening clears the badge; the list of new things is then empty.
    let r = send(
        &e.app,
        "POST",
        "/v1/instagram/watch/natgeo/opened",
        Some(SECRET),
        None,
        &[],
    )
    .await;
    assert_eq!(r.json["data"], json!({"total": 0, "accounts": {}}));
    assert_eq!(
        get(&e.app, "/v1/instagram/badges").await.json["data"]["total"],
        0
    );
    let r = get(&e.app, "/v1/instagram/watch/natgeo/new").await;
    assert_eq!(r.json["data"]["stories"].as_array().unwrap().len(), 0);
    assert_eq!(r.json["data"]["posts"].as_array().unwrap().len(), 0);
    assert!(r.json["data"]["since"].as_i64().unwrap() > 0);

    // Read-only: only GETs went to the backend and the CDN, and nothing that says "seen".
    for r in e.server.received_requests().await.unwrap() {
        if r.url.path().starts_with("/api/") || r.url.path().starts_with("/cdn/") {
            assert_eq!(r.method.to_string(), "GET", "{}", r.url.path());
            assert!(!r.url.path().contains("seen") && !r.url.path().contains("view"));
        }
    }
}

#[tokio::test]
async fn the_saved_ids_survive_a_restart() {
    let e = env().await;
    mount_ingest(&e).await;
    mount_account(&e, false, "1", &["p1"], &["s1"]).await;
    watch(&e, "natgeo", json!({})).await;
    e.st.service.check_account("natgeo").await.unwrap();
    assert_eq!(requests_to(&e, "/internal/v1/ingest").await.len(), 1);
    let reopened =
        secretary_instagram::watch::WatchStore::open(Some(e.dir.path().join("state/watch.json")));
    assert_eq!(reopened.get("natgeo").unwrap().saved_stories, vec!["s1"]);
}

#[tokio::test]
async fn auto_save_off_still_counts_new_stories_but_saves_nothing() {
    let e = env().await;
    mount_ingest(&e).await;
    mount_account(&e, false, "1", &["p1"], &["s1"]).await;
    watch(&e, "natgeo", json!({"autoSave": false})).await;
    e.st.service.check_account("natgeo").await.unwrap();
    remount(&e, false, "1", &["p1"], &["s1", "s2"]).await;
    e.st.service.check_account("natgeo").await.unwrap();
    assert_eq!(requests_to(&e, "/internal/v1/ingest").await.len(), 0);
    assert_eq!(
        requests_to(&e, "/cdn/").await.len(),
        0,
        "not even downloaded"
    );
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.json["data"][0]["newStories"],
        1
    );

    // Turning it on saves what is up now, once.
    watch(&e, "natgeo", json!({"autoSave": true})).await;
    e.st.service.check_account("natgeo").await.unwrap();
    assert_eq!(requests_to(&e, "/internal/v1/ingest").await.len(), 2);
}

#[tokio::test]
async fn the_daily_cap_limits_what_is_saved_per_account() {
    let e = env_with(Opts {
        watch: WatchConfig {
            daily_cap_bytes: 1500,
            ..Opts::default().watch
        },
        ..Default::default()
    })
    .await;
    mount_ingest(&e).await;
    mount_account(&e, false, "1", &["p1"], &["s1", "s2", "s3"]).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(
        requests_to(&e, "/internal/v1/ingest").await.len(),
        1,
        "1000 of 1500 bytes used; the next ones don't fit"
    );
    let acc = svc.watch.get("natgeo").unwrap();
    assert_eq!(acc.saved_stories, vec!["s1"]);
    assert_eq!(acc.day_bytes, 1000);
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(
        requests_to(&e, "/internal/v1/ingest").await.len(),
        1,
        "still today"
    );
    // The next day the allowance is back.
    svc.watch.update("natgeo", |a| a.day = "2000-01-01".into());
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(requests_to(&e, "/internal/v1/ingest").await.len(), 2);
    assert_eq!(
        svc.watch.get("natgeo").unwrap().saved_stories,
        vec!["s1", "s2"]
    );
}

#[tokio::test]
async fn a_private_flip_notifies_the_owner_once_per_flip() {
    let e = env().await;
    mount_notify(&e).await;
    mount_account(&e, false, "787132", &["p1"], &["s1"]).await;
    mount_ingest(&e).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(
        requests_to(&e, "/v1/notify").await.len(),
        0,
        "the first look is a baseline"
    );

    remount(&e, true, "787132", &["p1"], &[]).await;
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await; // flushes the queue
    let sent = requests_to(&e, "/v1/notify").await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer svc_test_token"
    );
    let b = body_of(&sent[0]);
    assert_eq!(b["category"], "lucina_message");
    assert_eq!(b["body"], "@natgeo is now private");
    assert_eq!(b["route"], "/instagram/natgeo");
    assert!(b["dedupKey"]
        .as_str()
        .unwrap()
        .starts_with("instagram:natgeo:private:"));
    assert_eq!(
        requests_to(&e, "/api/v1/users/natgeo/stories").await.len(),
        0,
        "no stories are asked of a private account"
    );
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.json["data"][0]["private"],
        true
    );

    // Checked again, still private: nothing more.
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await;
    assert_eq!(requests_to(&e, "/v1/notify").await.len(), 1);

    remount(&e, false, "787132", &["p1"], &["s1"]).await;
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await;
    let sent = requests_to(&e, "/v1/notify").await;
    assert_eq!(sent.len(), 1, "only the new request is in this log");
    assert_eq!(body_of(&sent[0])["body"], "@natgeo is now public");
}

#[tokio::test]
async fn a_notice_that_could_not_be_delivered_is_retried_not_repeated() {
    let e = env().await;
    mount_account(&e, false, "1", &["p1"], &[]).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;
    svc.check_account("natgeo").await.unwrap();
    reset_backend(&e, true, "1", &["p1"], &[]).await;
    Mock::given(method("POST"))
        .and(path("/v1/notify"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    mount_notify(&e).await;
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await; // 503: stays queued
    assert_eq!(svc.watch.notices().len(), 1);
    svc.watch_tick().await; // delivered
    assert_eq!(svc.watch.notices().len(), 0);
    svc.watch_tick().await;
    svc.watch_tick().await;
    let sent = requests_to(&e, "/v1/notify").await;
    assert_eq!(sent.len(), 2, "one refused, one delivered, then silence");
    assert_eq!(
        body_of(&sent[0])["dedupKey"],
        body_of(&sent[1])["dedupKey"],
        "the server can dedupe a retry"
    );
}

#[tokio::test]
async fn a_deleted_or_taken_over_username_is_reported_once() {
    let e = env().await;
    mount_notify(&e).await;
    mount_account(&e, false, "787132", &["p1"], &[]).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;
    svc.check_account("natgeo").await.unwrap();

    // 404 after it answered before: gone, once.
    e.server.reset().await;
    mount_notify(&e).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "User not found"})))
        .mount(&e.server)
        .await;
    svc.check_account("natgeo").await.unwrap();
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await;
    let sent = requests_to(&e, "/v1/notify").await;
    assert_eq!(sent.len(), 1);
    assert!(body_of(&sent[0])["body"]
        .as_str()
        .unwrap()
        .starts_with("@natgeo is gone"));
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.json["data"][0]["gone"],
        true
    );

    // The name now belongs to someone else (a different user id).
    remount(&e, false, "999", &["q1"], &[]).await;
    svc.check_account("natgeo").await.unwrap();
    svc.watch_tick().await;
    let sent = requests_to(&e, "/v1/notify").await;
    let texts: Vec<String> = sent
        .iter()
        .map(|r| body_of(r)["body"].as_str().unwrap().to_string())
        .collect();
    assert!(texts.iter().any(|t| t == "@natgeo is back"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t.contains("different account")),
        "{texts:?}"
    );
    assert_eq!(texts.len(), 2);
    assert_eq!(
        svc.watch.get("natgeo").unwrap().user_id.as_deref(),
        Some("999")
    );

    // An account that never answered is not "gone".
    watch(&e, "ghost", json!({})).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/ghost/bundle"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&e.server)
        .await;
    svc.check_account("ghost").await.unwrap();
    svc.watch_tick().await;
    assert_eq!(requests_to(&e, "/v1/notify").await.len(), 2);
}

#[tokio::test]
async fn without_a_notify_configuration_changes_are_dropped_not_queued_forever() {
    let e = env_with(Opts {
        notify: false,
        ..Default::default()
    })
    .await;
    mount_account(&e, false, "1", &["p1"], &[]).await;
    watch(&e, "natgeo", json!({})).await;
    let svc = &e.st.service;
    svc.check_account("natgeo").await.unwrap();
    remount(&e, true, "1", &["p1"], &[]).await;
    svc.check_account("natgeo").await.unwrap();
    assert_eq!(svc.watch.notices().len(), 1);
    svc.watch_tick().await;
    assert_eq!(svc.watch.notices().len(), 0);
    assert_eq!(requests_to(&e, "/v1/notify").await.len(), 0);
}

#[tokio::test]
async fn a_429_pauses_the_warmer() {
    let e = env_with(Opts {
        watch: WatchConfig {
            pause: Duration::from_secs(60),
            ..Opts::default().watch
        },
        ..Default::default()
    })
    .await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/users/.*/bundle$"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({"detail": "Too many requests"})),
        )
        .mount(&e.server)
        .await;
    for u in ["natgeo", "nasa"] {
        watch(&e, u, json!({})).await;
        e.st.service.watch.update(u, |a| a.next_due = 0);
    }
    let svc = &e.st.service;
    assert_eq!(svc.watch_tick().await, TickOutcome::Paused);
    let after_first = requests_to(&e, "/api/").await.len();
    assert_eq!(
        after_first, 3,
        "one try and two backed-off retries, then it gave up"
    );

    // The warmer stays down, even once the backend's own short block has passed.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(svc.watch_tick().await, TickOutcome::Paused);
    assert_eq!(requests_to(&e, "/api/").await.len(), after_first);
    // (nasa and natgeo were both due; one of them was tried.)
    let tried: Vec<_> = svc
        .watch
        .list()
        .into_iter()
        .filter(|a| a.status.as_deref() == Some("rate_limited"))
        .collect();
    assert_eq!(tried.len(), 1);
    assert!(
        tried[0].next_due > chrono::Utc::now().timestamp() + 50,
        "the account is rescheduled well after the pause"
    );
    // The owner's own requests are not blocked by the warmer's pause (only by the backend's 429).
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_429_the_owner_hit_also_holds_the_warmer_back() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .mount(&e.server)
        .await;
    assert_eq!(
        get(&e.app, "/v1/instagram/profile/natgeo").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    watch(&e, "nasa", json!({})).await;
    e.st.service.watch.update("nasa", |a| a.next_due = 0);
    assert_eq!(e.st.service.watch_tick().await, TickOutcome::Paused);
    assert_eq!(requests_to(&e, "/api/").await.len(), 1);
}

#[tokio::test]
async fn the_warmer_spaces_accounts_with_jitter_and_does_nothing_when_none_is_due() {
    let e = env_with(Opts {
        watch: WatchConfig {
            interval: Duration::from_secs(60),
            jitter: Duration::from_secs(10),
            ..Opts::default().watch
        },
        ..Default::default()
    })
    .await;
    assert_eq!(e.st.service.watch_tick().await, TickOutcome::Idle);
    mount_account(&e, false, "1", &["p1"], &[]).await;
    watch(&e, "natgeo", json!({})).await;
    // A new account is checked within seconds, not at once and not after a full interval.
    let due = e.st.service.watch.get("natgeo").unwrap().next_due - chrono::Utc::now().timestamp();
    assert!((1..=11).contains(&due), "{due}");
    e.st.service.watch.update("natgeo", |a| a.next_due = 0);
    assert_eq!(
        e.st.service.watch_tick().await,
        TickOutcome::Checked("natgeo".into())
    );
    let next = e.st.service.watch.get("natgeo").unwrap().next_due - chrono::Utc::now().timestamp();
    assert!((49..=71).contains(&next), "{next}");
    assert_eq!(
        e.st.service.watch_tick().await,
        TickOutcome::Idle,
        "nothing else is due"
    );
    // And the warmer's checks warm the cache for the owner's next visit.
    let before = requests_to(&e, "/api/v1/users/natgeo/bundle").await.len();
    assert_eq!(
        get(&e.app, "/v1/instagram/profile/natgeo").await.status,
        StatusCode::OK
    );
    assert_eq!(
        requests_to(&e, "/api/v1/users/natgeo/bundle").await.len(),
        before
    );
}

#[tokio::test]
async fn by_default_an_account_is_checked_again_after_about_23_hours() {
    let e = env().await;
    mount_account(&e, false, "1", &["p1"], &[]).await;
    watch(&e, "natgeo", json!({})).await;
    e.st.service.watch.update("natgeo", |a| a.next_due = 0);
    assert_eq!(
        e.st.service.watch_tick().await,
        TickOutcome::Checked("natgeo".into())
    );
    let next = e.st.service.watch.get("natgeo").unwrap().next_due - chrono::Utc::now().timestamp();
    assert!(
        (22 * 3600 + 1800 - 2..=23 * 3600 + 1800).contains(&next),
        "{next}"
    );
    let w = get(&e.app, "/v1/instagram/watch").await;
    assert_eq!(
        w.json["data"][0]["nextCheck"],
        e.st.service.watch.get("natgeo").unwrap().next_due
    );
}

async fn put_settings(e: &Env, body: Value) -> Resp {
    send(
        &e.app,
        "PUT",
        "/v1/instagram/watch-settings",
        Some(SECRET),
        Some(body),
        &[],
    )
    .await
}

#[tokio::test]
async fn the_owner_chooses_the_interval_and_it_applies_to_the_next_check() {
    let e = env().await;
    for gw in [None, Some("wrong")] {
        for m in ["GET", "PUT"] {
            let r = send(
                &e.app,
                m,
                "/v1/instagram/watch-settings",
                gw,
                Some(json!({"intervalHours": 12})),
                &[],
            )
            .await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m}");
        }
    }
    let r = get(&e.app, "/v1/instagram/watch-settings").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json["data"],
        json!({"intervalHours": 23, "defaultHours": 23, "jitterMinutes": 30, "choices": [6, 12, 23]})
    );

    // An account checked an hour ago, due again in 22 hours.
    watch(&e, "natgeo", json!({})).await;
    let now = chrono::Utc::now().timestamp();
    e.st.service.watch.update("natgeo", |a| {
        a.last_check = Some(now - 3600);
        a.next_due = now + 22 * 3600;
    });

    for bad in [
        json!({"intervalHours": 0}),
        json!({"intervalHours": 24}),
        json!({"intervalHours": "x"}),
        json!({}),
    ] {
        let r = put_settings(&e, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(r.json["error"], "bad_request");
    }
    assert_eq!(
        e.st.service.watch.get("natgeo").unwrap().next_due,
        now + 22 * 3600,
        "a refused change moves nothing"
    );

    let r = put_settings(&e, json!({"intervalHours": 12})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["intervalHours"], 12);
    assert_eq!(r.json["data"]["defaultHours"], 23);
    let due = e.st.service.watch.get("natgeo").unwrap().next_due;
    let want = now - 3600 + 12 * 3600;
    assert!(
        (want - 1800..=want + 1800).contains(&due),
        "{due} vs {want}"
    );
    assert_eq!(
        get(&e.app, "/v1/instagram/watch-settings").await.json["data"]["intervalHours"],
        12
    );

    let reopened =
        secretary_instagram::watch::WatchStore::open(Some(e.dir.path().join("state/watch.json")));
    assert_eq!(reopened.settings().interval_hours, Some(12));
    assert_eq!(reopened.get("natgeo").unwrap().next_due, due);
}

#[tokio::test]
async fn a_restart_does_not_add_checks() {
    let e = env().await;
    // Scheduled by the old 20-40 minute warmer, five minutes after a check.
    watch(&e, "natgeo", json!({})).await;
    watch(&e, "nasa", json!({})).await;
    let now = chrono::Utc::now().timestamp();
    e.st.service.watch.update("natgeo", |a| {
        a.last_check = Some(now - 300);
        a.next_due = now - 300 + 30 * 60;
    });
    let nasa_due = e.st.service.watch.get("nasa").unwrap().next_due;
    let state = e.dir.path().join("state");
    let svc = secretary_instagram::service::Service::new(
        secretary_instagram::upstream::Upstream::new(Default::default()).unwrap(),
        secretary_instagram::cache::DiskCache::new(e.dir.path().join("cache2"), 1 << 20).unwrap(),
        secretary_instagram::recent::Recent::open(e.dir.path().join("recent2.json")),
        secretary_instagram::media::Signer::new(KEY.to_vec()),
        secretary_instagram::service::ServiceConfig::new(
            e.dir.path().join("spool2"),
            e.server.uri(),
            state,
        ),
    )
    .unwrap();
    let due = svc.watch.get("natgeo").unwrap().next_due;
    let want = now - 300 + 23 * 3600;
    assert!(
        (want - 1800..=want + 1800).contains(&due),
        "{due} vs {want}"
    );
    assert_eq!(
        svc.watch.get("nasa").unwrap().next_due,
        nasa_due,
        "never checked: still checked within seconds"
    );
    assert_eq!(svc.watch_tick().await, TickOutcome::Idle);
}

#[tokio::test]
async fn the_warmer_loop_checks_a_new_account_by_itself_and_stops_when_told() {
    let e = env().await;
    mount_ingest(&e).await;
    mount_account(&e, false, "1", &["p1"], &["s1"]).await;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let svc = e.st.service.clone();
    let looped = tokio::spawn(async move { svc.run_warmer(stop_rx).await });
    watch(&e, "natgeo", json!({})).await; // wakes the loop; due within 2 s
    let started = Instant::now();
    loop {
        if e.st
            .service
            .watch
            .get("natgeo")
            .unwrap()
            .last_check
            .is_some()
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the warmer never checked it"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // last_check is set before the story is saved: wait for the save.
    while requests_to(&e, "/internal/v1/ingest").await.is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "its story was not saved"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), looped)
        .await
        .expect("the loop stops on request")
        .unwrap();
}

#[tokio::test]
async fn a_stop_request_ends_a_pass_that_is_still_waiting_on_the_backend() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(20))
                .set_body_json(bundle()),
        )
        .mount(&e.server)
        .await;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let svc = e.st.service.clone();
    let looped = tokio::spawn(async move { svc.run_warmer(stop_rx).await });
    watch(&e, "natgeo", json!({})).await;
    while requests_to(&e, "/api/").await.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let t = Instant::now();
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), looped)
        .await
        .expect("the loop stops while the request is in flight")
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn the_warmer_can_be_turned_off() {
    let e = env_with(Opts {
        watch: WatchConfig {
            enabled: false,
            ..Opts::default().watch
        },
        ..Default::default()
    })
    .await;
    watch(&e, "natgeo", json!({})).await;
    e.st.service.watch.update("natgeo", |a| a.next_due = 0);
    assert_eq!(e.st.service.watch_tick().await, TickOutcome::Disabled);
    assert_eq!(e.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn owner_requests_go_before_the_warmers() {
    let e = env().await;
    let slow = |p: &str| {
        Mock::given(method("GET"))
            .and(path(p.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(300))
                    .set_body_json(page(vec![], None)),
            )
    };
    slow("/api/v1/users/one/stories").mount(&e.server).await;
    mount_json(&e.server, "/api/v1/users/two/stories", page(vec![], None)).await;
    mount_json(&e.server, "/api/v1/users/low/stories", page(vec![], None)).await;
    let up = &e.st.service.up;
    let app = e.app.clone();
    let first = tokio::spawn(async move { get(&app, "/v1/instagram/stories/one").await });
    tokio::time::sleep(Duration::from_millis(60)).await;
    let low = async { up.api_get_low("/api/v1/users/low/stories").await.unwrap() };
    let app = e.app.clone();
    let second = async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        get(&app, "/v1/instagram/stories/two").await
    };
    let t = Instant::now();
    let (_, r2) = tokio::join!(low, second);
    assert_eq!(r2.status, StatusCode::OK);
    first.await.unwrap();
    // Stories only: a refresh behind the owner's answers asks for the rest of each account.
    let order: Vec<String> = requests_to(&e, "/api/")
        .await
        .iter()
        .map(|r| r.url.path().to_string())
        .filter(|p| p.ends_with("/stories"))
        .collect();
    assert_eq!(
        order,
        [
            "/api/v1/users/one/stories",
            "/api/v1/users/two/stories",
            "/api/v1/users/low/stories"
        ],
        "the warmer waited for both owner requests ({:?})",
        t.elapsed()
    );
}

// ---- the Archive ----------------------------------------------------------------------------

#[tokio::test]
async fn the_archive_lists_saved_items_and_serves_them_from_the_gallery() {
    let e = env().await;
    mount_ingest(&e).await;
    mount_account(&e, false, "1", &["p1"], &["s1", "s2"]).await;
    // lucina-media's loopback: where an item's file is.
    for n in 1..=3 {
        let file = e.dir.path().join(format!("blob-itm_{n}"));
        std::fs::write(&file, format!("GALLERY-FILE-{n}-0123456789")).unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/internal/v1/items/itm_{n}/path")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"path": file})))
            .mount(&e.server)
            .await;
    }
    watch(&e, "natgeo", json!({})).await;
    e.st.service.check_account("natgeo").await.unwrap();
    // A manual save (the viewer's button) lands in the archive too.
    let signed = e.st.service.signer.url_for(&cdn("p1.jpg"));
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    let r = send(&e.app, "POST", "/v1/instagram/save", Some(SECRET), Some(json!({"items": [{"url": signed, "kind": "image", "username": "natgeo", "caption": "mine"}]})), &[]).await;
    assert_eq!(r.json["data"]["saved"][0]["itemId"], "itm_3");

    let r = get(&e.app, "/v1/instagram/archive/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    let items = r.json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(r.json["data"]["nextCursor"], Value::Null);
    assert_eq!(items[0]["mediaId"], "itm_3", "newest first");
    assert_eq!(items[0]["auto"], false);
    assert_eq!(items[0]["caption"], "mine");
    assert_eq!(items[1]["auto"], true);
    assert_eq!(items[1]["takenAt"], 1_700_000_000);
    assert_eq!(items[1]["kind"], "image");
    assert_eq!(items[1]["thumbUrl"], "/v1/media/items/itm_2/thumb?size=256");
    let url = items[1]["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("/v1/instagram/a/"), "{url}");

    // The signed URL needs no gateway header, serves the Gallery's file, and takes Range.
    let r = send(&e.app, "GET", &url, None, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.bytes, b"GALLERY-FILE-2-0123456789");
    assert_eq!(r.headers["content-type"], "image/jpeg");
    let r = send(&e.app, "GET", &url, None, None, &[("range", "bytes=13-13")]).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes, b"2");
    // Bad signature, another item under the same signature, expired: refused.
    let mut parts: Vec<String> = url.split('/').map(String::from).collect();
    parts[6] = "itm_1".into();
    assert_eq!(
        send(&e.app, "GET", &parts.join("/"), None, None, &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let mut parts: Vec<String> = url.split('/').map(String::from).collect();
    parts[5] = "0".repeat(32);
    assert_eq!(
        send(&e.app, "GET", &parts.join("/"), None, None, &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &e.app,
            "GET",
            "/v1/instagram/a/1/abc/itm_1",
            None,
            None,
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // A media signature is not an archive signature.
    let media_url = e.st.service.signer.url_for(&cdn("p1.jpg"));
    assert_eq!(
        send(
            &e.app,
            "GET",
            &media_url.replace("/s/", "/a/"),
            None,
            None,
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    // Pagination is an offset; a bad cursor is a 400; another account has an empty archive.
    assert_eq!(
        get(&e.app, "/v1/instagram/archive/natgeo?cursor=abc")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let r = get(&e.app, "/v1/instagram/archive/natgeo?cursor=2").await;
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        get(&e.app, "/v1/instagram/archive/nasa").await.json["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        get(&e.app, "/v1/instagram/watch").await.json["data"][0]["archived"],
        3
    );
}
