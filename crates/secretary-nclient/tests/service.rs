//! End to end against a fake site (wiremock): what reaches a client and what reaches the cache.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use secretary_nclient::cache::DiskCache;
use secretary_nclient::http::{router, Http};
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
    dir: tempfile::TempDir,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let up = Upstream::new(UpstreamConfig {
        host: "site.test".into(),
        base_override: Some(server.uri()),
        api_interval: Duration::from_millis(1),
        image_interval: Duration::from_millis(1),
        timeout: Duration::from_secs(5),
        ..Default::default()
    })
    .unwrap();
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
    });
    Env {
        app: router(st),
        server,
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
