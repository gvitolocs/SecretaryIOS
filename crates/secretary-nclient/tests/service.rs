//! End to end against a fake site (wiremock): what reaches a client and what reaches the cache.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use secretary_nclient::cache::DiskCache;
use secretary_nclient::http::{router, Http};
use secretary_nclient::saves::Saves;
use secretary_nclient::service::Service;
use secretary_nclient::store::Store;
use secretary_nclient::tags::TagCache;
use secretary_nclient::upstream::{Upstream, UpstreamConfig};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "gateway-secret-for-tests";

fn tag(id: i32, name: &str) -> Value {
    json!({"id": id, "type": "tag", "name": name, "slug": name, "count": 100 - id})
}

fn detail(id: i32, title: &str, tags: Vec<Value>) -> Value {
    json!({
        "id": id,
        "media_id": format!("{}", id * 10),
        "title": {"english": title, "japanese": "", "pretty": title},
        "upload_date": 1_700_000_000,
        "num_favorites": 3,
        "num_pages": 2,
        "cover": {"path": format!("galleries/{}/cover.jpg", id * 10), "width": 350, "height": 500},
        "thumbnail": {"path": format!("galleries/{}/thumb.jpg", id * 10), "width": 250, "height": 350},
        "pages": [
            {"path": format!("galleries/{}/1.jpg", id * 10), "width": 1280, "height": 1800},
            {"path": format!("galleries/{}/2.jpg", id * 10), "width": 1280, "height": 1800}
        ],
        "tags": tags,
        "related": []
    })
}

fn list_item(id: i32, title: &str, tag_ids: &[i32]) -> Value {
    json!({
        "id": id,
        "media_id": format!("{}", id * 10),
        "english_title": title,
        "japanese_title": "",
        "thumbnail": format!("galleries/{}/thumb.jpg", id * 10),
        "num_pages": 2,
        "tag_ids": tag_ids,
    })
}

struct Env {
    app: axum::Router,
    server: MockServer,
    /// Stands in for lucina-media's loopback ingest.
    media: MockServer,
    dir: tempfile::TempDir,
}

async fn env() -> Env {
    env_with(|_| {}).await
}

async fn env_with(tweak: impl FnOnce(&mut UpstreamConfig)) -> Env {
    let server = MockServer::start().await;
    let media = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = UpstreamConfig {
        host: "site.test".into(),
        base_override: Some(server.uri()),
        api_interval: Duration::from_millis(1),
        image_interval: Duration::from_millis(1),
        timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(5),
        ..Default::default()
    };
    tweak(&mut cfg);
    let up = Upstream::new(cfg).unwrap();
    let service = Service::new(
        up,
        TagCache::open(Some(dir.path().join("tags.json"))),
        DiskCache::new(dir.path().join("cache")).unwrap(),
    );
    let st = Arc::new(Http {
        service,
        store: Store::open(&dir.path().join("owners"), "owner").unwrap(),
        gateway_secret: SECRET.as_bytes().to_vec(),
        signing_key: vec![7; 32],
        saves: Saves::new(dir.path().join("spool"), media.uri()).unwrap(),
    });
    Env {
        app: router(st),
        server,
        media,
        dir,
    }
}

async fn call(app: &axum::Router, m: &str, uri: &str, auth: bool) -> (StatusCode, Value, Vec<u8>) {
    let mut req = Request::builder().method(m).uri(uri);
    if auth {
        req = req.header("x-lucina-gateway", SECRET);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, bytes)
}

async fn mount_detail(server: &MockServer, d: Value) {
    let id = d["id"].as_i64().unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/galleries/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(d))
        .mount(server)
        .await;
}

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(std::fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    out
}

#[tokio::test]
async fn the_gateway_header_is_required() {
    let e = env().await;
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/status", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/status", true).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn blocked_galleries_never_reach_a_client_or_the_cache() {
    let e = env().await;
    // Teach the tag cache through two details: one clean, one blocked.
    mount_detail(
        &e.server,
        detail(1, "Office Story", vec![tag(10, "big breasts")]),
    )
    .await;
    mount_detail(
        &e.server,
        detail(
            2,
            "Bad One",
            vec![tag(10, "big breasts"), tag(11, "lolicon")],
        ),
    )
    .await;
    mount_detail(
        &e.server,
        detail(
            4,
            "Uniform",
            vec![tag(12, "schoolgirl uniform"), tag(13, "flat chest")],
        ),
    )
    .await;
    // 3: unknown tag id 99 → resolved through its detail (clean).
    mount_detail(
        &e.server,
        detail(3, "Resolved Later", vec![tag(99, "milf")]),
    )
    .await;
    // 5: no tag data and its detail has no tags → dropped.
    mount_detail(&e.server, detail(5, "No Tags", vec![])).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [
                list_item(1, "Office Story", &[10]),
                list_item(2, "Bad One", &[10, 11]),
                list_item(3, "Resolved Later", &[99]),
                list_item(4, "Uniform", &[12, 13]),
                {"id": 5, "media_id": "50", "english_title": "No Tags", "thumbnail": "galleries/50/thumb.jpg"},
                list_item(6, "My loli title", &[10]),
            ],
            "num_pages": 1
        })))
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/t1/galleries/.*"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(b"JPEGDATA".to_vec()),
        )
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/i1/galleries/.*"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(b"PAGEDATA".to_vec()),
        )
        .mount(&e.server)
        .await;

    // Learn the tags of 1, 2 and 4.
    for id in [1, 2, 4] {
        let _ = call(&e.app, "GET", &format!("/v1/nclient/galleries/{id}"), true).await;
    }
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/galleries/2", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/galleries/4", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body, _) = call(&e.app, "GET", "/v1/nclient/search?q=office", true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<i64> = body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 3]);

    // Every search carries the upstream exclusions.
    let reqs = e.server.received_requests().await.unwrap();
    let search = reqs
        .iter()
        .find(|r| r.url.path() == "/api/v2/search")
        .unwrap();
    let q = search.url.query().unwrap();
    assert!(q.contains("-tag%3A%22lolicon%22"), "{q}");

    // A blocked search term never leaves the server.
    let before = e.server.received_requests().await.unwrap().len();
    let (_, body, _) = call(&e.app, "GET", "/v1/nclient/search?q=lolicon", true).await;
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0);
    assert_eq!(e.server.received_requests().await.unwrap().len(), before);

    // Images: the clean gallery's page through the proxy; the URL is signed.
    let (_, d, _) = call(&e.app, "GET", "/v1/nclient/galleries/1", true).await;
    let url = d["data"]["pages"][0]["url"].as_str().unwrap().to_string();
    let (status, _, bytes) = call(&e.app, "GET", &url, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"PAGEDATA");
    let forged = url.replace("/1/page/0/", "/2/page/0/");
    let (status, _, _) = call(&e.app, "GET", &forged, false).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Nothing about 2, 4, 5 or 6 is stored anywhere in the cache.
    let stored = files_under(&e.dir.path().join("cache")).join("\n");
    for bad in [
        "Bad One",
        "Uniform",
        "No Tags",
        "My loli title",
        "galleries/20/",
        "galleries/40/",
    ] {
        assert!(!stored.contains(bad), "{bad} reached the cache");
    }
    assert!(stored.contains("Office Story"));
}

#[tokio::test]
async fn favorites_and_progress_round_trip() {
    let e = env().await;
    mount_detail(
        &e.server,
        detail(1, "Office Story", vec![tag(10, "big breasts")]),
    )
    .await;
    let (status, _, _) = call(&e.app, "PUT", "/v1/nclient/favorites/1", true).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body, _) = call(&e.app, "GET", "/v1/nclient/favorites", true).await;
    assert_eq!(body["data"]["items"][0]["id"], 1);

    let req = Request::builder()
        .method("PUT")
        .uri("/v1/nclient/progress/1")
        .header("x-lucina-gateway", SECRET)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"page":1,"pages":2}"#))
        .unwrap();
    assert_eq!(
        e.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let (_, body, _) = call(&e.app, "GET", "/v1/nclient/galleries/1", true).await;
    assert_eq!(body["data"]["progress"]["page"], 1);
    assert_eq!(body["data"]["favorite"], true);

    let (_, body, _) = call(&e.app, "DELETE", "/v1/nclient/favorites/1", true).await;
    assert_eq!(body["data"]["favorite"], false);
}

#[tokio::test]
async fn random_skips_blocked_galleries() {
    let e = env().await;
    mount_detail(&e.server, detail(2, "Bad", vec![tag(11, "shotacon")])).await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/galleries/random"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 2})))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/galleries/random"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
        .mount(&e.server)
        .await;
    let (status, body, _) = call(&e.app, "GET", "/v1/nclient/random", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["id"], 1);
}

#[tokio::test]
async fn the_tag_picker_hides_blocked_tags() {
    let e = env().await;
    mount_detail(
        &e.server,
        detail(2, "Bad", vec![tag(11, "lolicon"), tag(10, "big breasts")]),
    )
    .await;
    let _ = call(&e.app, "GET", "/v1/nclient/galleries/2", true).await;
    let (_, body, _) = call(&e.app, "GET", "/v1/nclient/tags", true).await;
    let names: Vec<&str> = body["data"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["big breasts"]);
}

// ---- Save to Lucina ------------------------------------------------------------------------

type Ingested = Arc<Mutex<Vec<Value>>>;

/// A fake lucina-media: reads the file it is told about, then removes it (`move: true`) and
/// records the request next to the file's bytes.
async fn mount_ingest(e: &Env) -> Ingested {
    let seen: Ingested = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    Mock::given(method("POST"))
        .and(path("/internal/v1/ingest"))
        .respond_with(move |req: &wiremock::Request| {
            let mut v: Value = serde_json::from_slice(&req.body).unwrap();
            let p = v["path"].as_str().unwrap().to_string();
            let bytes = std::fs::read(&p).unwrap_or_default();
            if v["move"] == true {
                let _ = std::fs::remove_file(&p);
            }
            v["bytes"] = json!(String::from_utf8_lossy(&bytes));
            let mut g = sink.lock().unwrap();
            g.push(v);
            ResponseTemplate::new(200)
                .set_body_json(json!({"item_id": format!("item-{}", g.len()), "duplicate": false}))
        })
        .mount(&e.media)
        .await;
    seen
}

async fn mount_pages(server: &MockServer, media_id: i32) {
    for (n, body) in [(1, "PAGE-ONE"), (2, "PAGE-TWO")] {
        Mock::given(method("GET"))
            .and(path(format!("/i1/galleries/{media_id}/{n}.jpg")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(body.as_bytes().to_vec()),
            )
            .mount(server)
            .await;
    }
}

async fn post_json(app: &axum::Router, uri: &str, body: &str, auth: bool) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if auth {
        req = req.header("x-lucina-gateway", SECRET);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn finished(app: &axum::Router, job: &str) -> Value {
    for _ in 0..200 {
        let (status, body, _) = call(app, "GET", &format!("/v1/nclient/saves/{job}"), true).await;
        assert_eq!(status, StatusCode::OK);
        let state = body["data"]["state"].as_str().unwrap().to_string();
        if !matches!(state.as_str(), "queued" | "running") {
            return body["data"].clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("save job did not finish");
}

#[tokio::test]
async fn saving_a_gallery_ingests_every_page_with_source_and_album() {
    let e = env().await;
    let seen = mount_ingest(&e).await;
    mount_detail(
        &e.server,
        detail(1, "Office Story", vec![tag(10, "big breasts")]),
    )
    .await;
    mount_pages(&e.server, 10).await;

    let (status, body) = post_json(
        &e.app,
        "/v1/nclient/galleries/1/save",
        r#"{"target":"gallery"}"#,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["data"]["total"], 2);
    let job = body["data"]["job"].as_str().unwrap().to_string();
    assert!(job.len() >= 16, "the job id is random");

    let done = finished(&e.app, &job).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["done"], 2);
    assert_eq!(done["saved"].as_array().unwrap().len(), 2);
    assert_eq!(done["saved"][0]["itemId"], "item-1");
    assert_eq!(done["failed"].as_array().unwrap().len(), 0);

    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 2);
    for (i, (name, bytes)) in [("001.jpg", "PAGE-ONE"), ("002.jpg", "PAGE-TWO")]
        .iter()
        .enumerate()
    {
        assert_eq!(got[i]["source"], "nclient");
        assert_eq!(got[i]["album"], "Office Story");
        assert_eq!(got[i]["move"], true);
        assert_eq!(got[i]["name"], *name);
        assert_eq!(got[i]["mime"], "image/jpeg");
        assert_eq!(got[i]["bytes"], *bytes);
        let p = got[i]["path"].as_str().unwrap();
        assert!(
            p.starts_with(e.dir.path().join("spool").to_str().unwrap()),
            "{p}"
        );
    }
    // The spool is empty again: the pages moved into the library.
    let left: Vec<_> = std::fs::read_dir(e.dir.path().join("spool"))
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty(), "spool not cleaned");
}

#[tokio::test]
async fn saving_a_blocked_gallery_is_a_404_and_nothing_is_fetched_or_ingested() {
    let e = env().await;
    let seen = mount_ingest(&e).await;
    mount_detail(
        &e.server,
        detail(
            2,
            "Bad One",
            vec![tag(11, "lolicon"), tag(10, "big breasts")],
        ),
    )
    .await;
    mount_pages(&e.server, 20).await;
    mount_detail(
        &e.server,
        detail(
            4,
            "Uniform",
            vec![tag(12, "schoolgirl uniform"), tag(13, "flat chest")],
        ),
    )
    .await;
    mount_pages(&e.server, 40).await;
    for id in [2, 4, 777] {
        let (status, body) = post_json(
            &e.app,
            &format!("/v1/nclient/galleries/{id}/save"),
            r#"{"target":"gallery"}"#,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}: {body}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(seen.lock().unwrap().is_empty());
    assert!(e.media.received_requests().await.unwrap().is_empty());
    for r in e.server.received_requests().await.unwrap() {
        assert!(
            !r.url.path().starts_with("/i1/") && !r.url.path().starts_with("/t1/"),
            "an image of a blocked gallery was requested"
        );
    }
    let stored = files_under(&e.dir.path().join("cache")).join("\n");
    assert!(!stored.contains("Bad One") && !stored.contains("Uniform"));
}

#[tokio::test]
async fn save_routes_need_the_gateway_header() {
    let e = env().await;
    let seen = mount_ingest(&e).await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    mount_pages(&e.server, 10).await;
    let (status, _) = post_json(&e.app, "/v1/nclient/galleries/1/save", "{}", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/saves/abc", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(&e.app, "DELETE", "/v1/nclient/saves/abc", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(seen.lock().unwrap().is_empty());
    assert!(e.server.received_requests().await.unwrap().is_empty());
    // An unknown job is a 404 with the header.
    let (status, _, _) = call(&e.app, "GET", "/v1/nclient/saves/abc", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_library_target_is_not_supported() {
    let e = env().await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    let (status, body) = post_json(
        &e.app,
        "/v1/nclient/galleries/1/save",
        r#"{"target":"library"}"#,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "library_not_supported");
    let (_, st, _) = call(&e.app, "GET", "/v1/nclient/status", true).await;
    assert_eq!(st["data"]["save"]["library"], false);
}

#[tokio::test]
async fn saving_twice_returns_the_running_job() {
    let e = env().await;
    let _seen = mount_ingest(&e).await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    mount_pages(&e.server, 10).await;
    let (_, a) = post_json(&e.app, "/v1/nclient/galleries/1/save", "", true).await;
    let (_, b) = post_json(&e.app, "/v1/nclient/galleries/1/save", "", true).await;
    // Empty body means target gallery. While the first runs the second is the same job
    // (or, if it already finished, a new one): both end done.
    let ja = a["data"]["job"].as_str().unwrap().to_string();
    let jb = b["data"]["job"].as_str().unwrap().to_string();
    assert_eq!(finished(&e.app, &ja).await["state"], "done");
    assert_eq!(finished(&e.app, &jb).await["state"], "done");
}

#[tokio::test]
async fn a_429_with_retry_after_is_retried_with_backoff_and_then_succeeds() {
    let e = env().await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    Mock::given(method("GET"))
        .and(path("/i1/galleries/10/1.jpg"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/i1/galleries/10/1.jpg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(b"AFTER".to_vec()),
        )
        .mount(&e.server)
        .await;
    let (_, d, _) = call(&e.app, "GET", "/v1/nclient/galleries/1", true).await;
    let url = d["data"]["pages"][0]["url"].as_str().unwrap().to_string();
    let t = std::time::Instant::now();
    let (status, _, bytes) = call(&e.app, "GET", &url, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"AFTER");
    // Retry-After: 1 was honoured (the backoff step alone is 5 ms).
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn a_long_retry_after_is_passed_to_the_client_without_waiting() {
    let e = env().await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    Mock::given(method("GET"))
        .and(path("/i1/galleries/10/1.jpg"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .mount(&e.server)
        .await;
    let (_, d, _) = call(&e.app, "GET", "/v1/nclient/galleries/1", true).await;
    let url = d["data"]["pages"][0]["url"].as_str().unwrap().to_string();
    let t = std::time::Instant::now();
    let req = Request::builder()
        .method("GET")
        .uri(&url)
        .body(Body::empty())
        .unwrap();
    let resp = e.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.headers()["retry-after"], "120");
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "rate_limited");
    assert_eq!(body["retryAfter"], 120);
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn upstream_5xx_is_retried_then_reported_as_unavailable() {
    let e = env_with(|c| c.max_retries = 1).await;
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    Mock::given(method("GET"))
        .and(path("/i1/galleries/10/1.jpg"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&e.server)
        .await;
    let (_, d, _) = call(&e.app, "GET", "/v1/nclient/galleries/1", true).await;
    let url = d["data"]["pages"][0]["url"].as_str().unwrap().to_string();
    let (status, _, _) = call(&e.app, "GET", &url, false).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let tries = e
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/i1/galleries/10/1.jpg")
        .count();
    assert_eq!(tries, 2, "one try and one retry");
}

#[tokio::test]
async fn a_save_refused_by_lucina_media_fails_without_leaving_files() {
    let e = env().await; // no ingest mock mounted: wiremock answers 404 to everything
    mount_detail(&e.server, detail(1, "Fine", vec![tag(10, "big breasts")])).await;
    mount_pages(&e.server, 10).await;
    let (_, body) = post_json(&e.app, "/v1/nclient/galleries/1/save", "{}", true).await;
    let job = body["data"]["job"].as_str().unwrap().to_string();
    let done = finished(&e.app, &job).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(done["saved"].as_array().unwrap().len(), 0);
    let left: Vec<_> = std::fs::read_dir(e.dir.path().join("spool"))
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty());
}

#[tokio::test]
async fn a_save_with_lucina_media_unreachable_stops_after_three_pages() {
    let mut e = env().await;
    // Rebuild the app with an ingest URL nothing listens on.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let up = Upstream::new(UpstreamConfig {
        host: "site.test".into(),
        base_override: Some(e.server.uri()),
        api_interval: Duration::from_millis(1),
        image_interval: Duration::from_millis(1),
        ..Default::default()
    })
    .unwrap();
    let st = Arc::new(Http {
        service: Service::new(
            up,
            TagCache::open(Some(e.dir.path().join("tags2.json"))),
            DiskCache::new(e.dir.path().join("cache2")).unwrap(),
        ),
        store: Store::open(&e.dir.path().join("owners2"), "owner").unwrap(),
        gateway_secret: SECRET.as_bytes().to_vec(),
        signing_key: vec![7; 32],
        saves: Saves::new(e.dir.path().join("spool2"), dead)
            .unwrap()
            .with_retry_delay(Duration::from_millis(5)),
    });
    e.app = router(st);
    // Five pages, so "stops after three" is visible.
    let mut d = detail(1, "Fine", vec![tag(10, "big breasts")]);
    let page = d["pages"][0].clone();
    d["pages"] = json!([page, page, page, page, page]);
    mount_detail(&e.server, d).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/i1/galleries/10/.*"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(b"X".to_vec()),
        )
        .mount(&e.server)
        .await;
    let (_, body) = post_json(&e.app, "/v1/nclient/galleries/1/save", "{}", true).await;
    let job = body["data"]["job"].as_str().unwrap().to_string();
    let done = finished(&e.app, &job).await;
    assert_eq!(done["state"], "failed", "{done}");
    assert_eq!(done["error"], "media_unavailable");
    assert_eq!(done["failed"].as_array().unwrap().len(), 3);
    let left: Vec<_> = std::fs::read_dir(e.dir.path().join("spool2"))
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty());
}
