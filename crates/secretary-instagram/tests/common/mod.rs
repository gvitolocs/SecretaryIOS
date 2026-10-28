//! Shared harness for the integration tests: a mock backend, a mock Instagram CDN and a mock
//! lucina-media (all one wiremock server), behind the real router.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use secretary_instagram::cache::DiskCache;
use secretary_instagram::http::{router, Http};
use secretary_instagram::media::Signer;
use secretary_instagram::recent::Recent;
use secretary_instagram::service::{Service, ServiceConfig, WatchConfig};
use secretary_instagram::upstream::{Upstream, UpstreamConfig};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const SECRET: &str = "gateway-secret-for-tests";
pub const KEY: [u8; 32] = [7; 32];
pub const JPEG: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg-bytes-0123456789";

pub struct Env {
    pub app: axum::Router,
    pub st: Arc<Http>,
    pub server: MockServer,
    pub dir: tempfile::TempDir,
}

pub struct Opts {
    pub cache_max: u64,
    pub api_interval: Duration,
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub default_block: Duration,
    /// Tests of the warmer: a cap, intervals, a notify token.
    pub watch: WatchConfig,
    pub notify: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            cache_max: 64 << 20,
            api_interval: Duration::from_millis(1),
            max_retries: 2,
            backoff_base: Duration::from_millis(10),
            default_block: Duration::from_secs(2),
            watch: WatchConfig {
                enabled: true,
                interval_min: Duration::from_secs(1200),
                interval_max: Duration::from_secs(2400),
                pause: Duration::from_secs(1800),
                daily_cap_bytes: 500 << 20,
            },
            notify: true,
        }
    }
}

pub async fn env() -> Env {
    env_with(Opts::default()).await
}

pub async fn env_with(o: Opts) -> Env {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let up = Upstream::new(UpstreamConfig {
        base: server.uri(),
        media_base_override: Some(format!("{}/cdn", server.uri())),
        timeout: Duration::from_secs(5),
        api_interval: o.api_interval,
        media_interval: Duration::from_millis(1),
        media_concurrency: 2,
        max_retries: o.max_retries,
        backoff_base: o.backoff_base,
        backoff_cap: Duration::from_secs(5),
        default_block: o.default_block,
        ..Default::default()
    })
    .unwrap();
    let service = Service::new(
        up,
        DiskCache::new(dir.path().join("cache"), o.cache_max).unwrap(),
        Recent::open(dir.path().join("recent.json")),
        Signer::new(KEY.to_vec()),
        {
            let mut c = ServiceConfig::new(
                dir.path().join("spool"),
                server.uri(),
                dir.path().join("state"),
            );
            if o.notify {
                std::fs::write(dir.path().join("notify.token"), "svc_test_token\n").unwrap();
                c.notify_url = Some(server.uri());
                c.notify_token_file = Some(dir.path().join("notify.token"));
            }
            c.watch = o.watch.clone();
            c
        },
    )
    .unwrap();
    let st = Arc::new(Http {
        service: Arc::new(service),
        gateway_secret: SECRET.as_bytes().to_vec(),
    });
    Env {
        app: router(st.clone()),
        st,
        server,
        dir,
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub json: Value,
    pub bytes: Vec<u8>,
    pub headers: HeaderMap,
}

pub async fn send(
    app: &axum::Router,
    m: &str,
    uri: &str,
    gateway: Option<&str>,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Resp {
    let mut req = Request::builder().method(m).uri(uri);
    if let Some(g) = gateway {
        req = req.header("x-lucina-gateway", g);
    }
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Resp {
        status,
        json,
        bytes,
        headers,
    }
}

pub async fn get(app: &axum::Router, uri: &str) -> Resp {
    send(app, "GET", uri, Some(SECRET), None, &[]).await
}

// ---- fixtures -------------------------------------------------------------------------------

pub fn cdn(file: &str) -> String {
    format!("https://scontent-gmp1-1.cdninstagram.com/v/t51/{file}?stp=dst-jpg_e35&oh=1&oe=2")
}

pub fn worker(cdn_url: &str) -> String {
    let enc: String = url::form_urlencoded::byte_serialize(cdn_url.as_bytes()).collect();
    format!("https://storyview-ig-proxy.yodams.workers.dev/cdn?url={enc}&exp=1793049000&sig=abcd")
}

pub fn item(id: &str, media_type: &str, file: &str, children: &[&str]) -> Value {
    let video = media_type == "video";
    json!({
        "id": id,
        "media_type": media_type,
        "thumbnail_url": worker(&cdn(&format!("{file}_thumb.jpg"))),
        "media_url": worker(&cdn(file)),
        "caption": format!("caption of {id}"),
        "taken_at": 1_700_000_000,
        "like_count": 5,
        "comment_count": 2,
        "is_video": video,
        "carousel_urls": children.iter().map(|c| worker(&cdn(c))).collect::<Vec<_>>(),
    })
}

pub fn page(items: Vec<Value>, next: Option<&str>) -> Value {
    json!({"items": items, "next_cursor": next})
}

pub fn posts_items() -> Vec<Value> {
    vec![
        item("DeAAAAAAAA1", "image", "p1.jpg", &[]),
        item("DeAAAAAAAA2", "video", "p2.mp4", &[]),
        item(
            "DeAAAAAAAA3",
            "carousel",
            "c0.jpg",
            &["c0.jpg", "c1.mp4", "c2.jpg"],
        ),
    ]
}

pub fn bundle() -> Value {
    json!({
        "profile": {
            "username": "natgeo", "full_name": "National Geographic", "biography": "Step into wonder",
            "profile_pic_url": worker(&cdn("pfp.jpg")), "profile_url": "https://instagram.com/natgeo/",
            "media_count": 10, "follower_count": 1000, "following_count": 5, "is_private": false,
            "has_stories": true, "is_verified": true, "user_id": "787132", "external_url": null
        },
        "posts": page(posts_items(), Some("CUR1")),
        "highlights": [{"id": "highlight:18023864549864045", "title": "BOTW", "cover_url": worker(&cdn("hl.jpg")), "media_count": 3}]
    })
}

pub async fn mount_json(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

pub async fn mount_media(server: &MockServer, file: &str, bytes: &[u8], ctype: &str) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/cdn/scontent-gmp1-1.cdninstagram.com/v/t51/{file}"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", ctype)
                .set_body_bytes(bytes.to_vec()),
        )
        .mount(server)
        .await;
}

pub async fn count(server: &MockServer, prefix: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with(prefix))
        .count()
}

pub fn all_urls(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if matches!(k.as_str(), "url" | "thumbUrl" | "picUrl" | "coverUrl") {
                    if let Some(s) = x.as_str() {
                        out.push(s.to_string());
                    }
                }
                all_urls(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| all_urls(x, out)),
        _ => {}
    }
}
