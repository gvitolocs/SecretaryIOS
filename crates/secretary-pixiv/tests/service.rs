//! End to end with a mock core (the `Backend` trait), a fake image CDN (wiremock) and a fake
//! lucina-media: what reaches a client, what reaches the cache, what reaches the gallery.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use secretary_pixiv::backend::Backend;
use secretary_pixiv::cache::DiskCache;
use secretary_pixiv::http::{router, Http};
use secretary_pixiv::images::ImageConfig;
use secretary_pixiv::service::{Config, Service};
use secretary_pixiv::store::Store;
use secretary_pixiv::upstream::GateConfig;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "gateway-secret-for-tests";
const CDN: &str = "https://i.pximg.net";

fn img(n: u32, page: u32) -> String {
    format!("{CDN}/img-master/img/2024/01/01/00/00/00/{n}_p{page}_master1200.jpg")
}

fn orig(n: u32, page: u32) -> String {
    format!("{CDN}/img-original/img/2024/01/01/00/00/00/{n}_p{page}.png")
}

// ---- the mock core ------------------------------------------------------------------------

type SearchFn = fn(&[String]) -> Result<Value, String>;

#[derive(Default)]
struct Mock_ {
    /// `Some(user_status)`: a session is configured and pixiv answers this about it.
    session: Mutex<Option<Value>>,
    /// Replaces the default search answer.
    search_fn: Mutex<Option<SearchFn>>,
    calls: Mutex<Vec<String>>,
    /// Errors handed out first, whatever the operation.
    failures: Mutex<VecDeque<String>>,
    /// Every call fails with this while set.
    always: Mutex<Option<String>>,
    count: AtomicUsize,
}

impl Mock_ {
    fn gate(&self, call: String) -> Result<(), String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().unwrap().push(call);
        if let Some(e) = self.always.lock().unwrap().clone() {
            return Err(e);
        }
        if let Some(e) = self.failures.lock().unwrap().pop_front() {
            return Err(e);
        }
        Ok(())
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn login(&self, status: Value) {
        *self.session.lock().unwrap() = Some(status);
    }
    fn on_search(&self, f: SearchFn) {
        *self.search_fn.lock().unwrap() = Some(f);
    }
}

impl Backend for Mock_ {
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String> {
        self.gate(format!("ranking {mode} {page}"))?;
        Ok(json!({
            "mode": mode, "page": page, "next": true,
            "items": [
                {"id": "1001", "title": "Sky", "author": "Ann", "author_id": "7",
                 "tags": ["sky", "風景"], "thumb": img(1001, 0), "pages": 1},
                {"id": "1002", "title": "Comic", "author": "Bob", "author_id": "8",
                 "tags": [], "thumb": img(1002, 0), "pages": 3},
                {"id": "1003", "title": "Late", "author": "Cat", "author_id": "9",
                 "tags": [], "thumb": img(1003, 0), "pages": 1, "r18": true},
            ],
        }))
    }

    fn search(&self, args: &[String]) -> Result<Value, String> {
        self.gate(format!("search {}", args.join(" ")))?;
        if let Some(f) = *self.search_fn.lock().unwrap() {
            return f(args);
        }
        Ok(json!({
            "word": "w", "page": 1, "total": 3, "num_pages": 2,
            "items": [
                {"id": "2001", "title": "Safe one", "author": "Cy", "author_id": "9",
                 "thumb": img(2001, 0), "pages": 1, "r18": false},
                {"id": "2002", "title": "Hidden one", "author": "Di", "author_id": "10",
                 "thumb": img(2002, 0), "pages": 1, "r18": true},
                {"id": "2003", "title": "Safe two", "author": "Ed", "author_id": "11",
                 "thumb": "https://evil.test/x.jpg", "pages": 2, "r18": false},
            ],
        }))
    }

    fn illust(&self, id: &str) -> Result<Value, String> {
        self.gate(format!("illust {id}"))?;
        let n: u32 = id.parse().unwrap();
        if n == 404 {
            return Err("pixiv: this work was deleted".into());
        }
        let pages = if n == 4004 || n == 3003 { 2 } else { 1 };
        Ok(json!({
            "id": id, "title": "Work Title", "author": "Artist", "author_id": "77",
            "created": "2024-05-06T07:08:09+00:00", "type": if n == 5005 { 2 } else { 1 },
            "r18": n == 3003, "description": "hello",
            "stats": {"views": 10, "likes": 5, "bookmarks": 3, "comments": 1},
            "page_count": pages, "cover": img(n, 0), "orig": orig(n, 0),
            "tags": [{"tag": "風景", "en": "landscape"}, {"tag": "sky", "en": ""}],
            "pages": (0..pages).map(|p| json!({
                "thumb": img(n, p).replace("master1200", "square1200"),
                "regular": img(n, p), "orig": orig(n, p),
            })).collect::<Vec<_>>(),
        }))
    }

    fn self_status(&self) -> Result<Value, String> {
        self.gate("self_status".into())?;
        Ok(self.session.lock().unwrap().clone().unwrap_or(json!({"is_logged_in": false})))
    }

    fn has_session(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }
}

// ---- a fake lucina-media ------------------------------------------------------------------

#[derive(Default)]
struct Ingested {
    bodies: Mutex<Vec<Value>>,
    /// Size of the file at `path` when the ingest was called.
    sizes: Mutex<Vec<usize>>,
}

async fn fake_media(seen: Arc<Ingested>, fail: bool) -> String {
    use axum::routing::post;
    async fn ingest(
        axum::extract::State((seen, fail)): axum::extract::State<(Arc<Ingested>, bool)>,
        axum::Json(v): axum::Json<Value>,
    ) -> (StatusCode, axum::Json<Value>) {
        if fail {
            return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({})));
        }
        let size = std::fs::read(v["path"].as_str().unwrap()).map(|b| b.len()).unwrap_or(0);
        let n = {
            let mut b = seen.bodies.lock().unwrap();
            b.push(v);
            b.len()
        };
        seen.sizes.lock().unwrap().push(size);
        (
            StatusCode::OK,
            axum::Json(json!({"item_id": format!("item-{n}"), "duplicate": n == 2})),
        )
    }
    let app = axum::Router::new()
        .route("/internal/v1/ingest", post(ingest))
        .with_state((seen, fail));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

// ---- harness ------------------------------------------------------------------------------

struct Env {
    app: axum::Router,
    http: Arc<Http>,
    mock: Arc<Mock_>,
    cdn: MockServer,
    dir: tempfile::TempDir,
    ingested: Arc<Ingested>,
}

struct Opts {
    cache_max: u64,
    media_fails: bool,
    image_bytes: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            cache_max: 64 * 1024 * 1024,
            media_fails: false,
            image_bytes: 300,
        }
    }
}

async fn env_with(o: Opts) -> Env {
    let cdn = MockServer::start().await;
    // The CDN insists on the Referer, like the real one.
    let body = vec![7u8; o.image_bytes];
    Mock::given(method("GET"))
        .and(path_regex(".*"))
        .and(header("referer", "https://www.pixiv.net/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "image/png"))
        .mount(&cdn)
        .await;
    let ingested = Arc::new(Ingested::default());
    let media_url = fake_media(ingested.clone(), o.media_fails).await;
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(Mock_::default());
    let cfg = Config {
        gate: GateConfig {
            interval: Duration::from_millis(1),
            backoff_base: Duration::from_millis(5),
            attempts: 3,
            cooldown: Duration::from_secs(30),
        },
        images: ImageConfig {
            base_override: Some(cdn.uri()),
            spacing: Duration::from_millis(1),
            concurrency: 2,
            backoff_base: Duration::from_millis(5),
            attempts: 3,
            cooldown: Duration::from_secs(30),
            timeout: Duration::from_secs(5),
        },
        list_ttl: Duration::from_secs(300),
        illust_ttl: Duration::from_secs(300),
        media_url,
    };
    let service = Service::new(
        mock.clone(),
        DiskCache::new(dir.path().join("cache"), o.cache_max).unwrap(),
        dir.path().join("spool"),
        cfg,
    )
    .unwrap();
    let http = Arc::new(Http {
        service,
        store: Store::open(&dir.path().join("owners"), "owner").unwrap(),
        gateway_secret: SECRET.as_bytes().to_vec(),
        signing_key: vec![7; 32],
    });
    Env {
        app: router(http.clone()),
        http,
        mock,
        cdn,
        dir,
        ingested,
    }
}

async fn env() -> Env {
    env_with(Opts::default()).await
}

struct Reply {
    status: StatusCode,
    json: Value,
    bytes: Vec<u8>,
    headers: axum::http::HeaderMap,
}

async fn send(app: &axum::Router, m: &str, uri: &str, auth: bool, body: Option<Value>) -> Reply {
    let mut req = Request::builder().method(m).uri(uri);
    if auth {
        req = req.header("x-lucina-gateway", SECRET);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        json,
        bytes,
        headers,
    }
}

async fn get(app: &axum::Router, uri: &str) -> Reply {
    send(app, "GET", uri, true, None).await
}

fn ids(r: &Reply) -> Vec<String> {
    r.json["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// A logged-in account with R-18 display on.
fn r18_account() -> Value {
    json!({"is_logged_in": true, "is_premium": false, "x_restrict": 1})
}

async fn cdn_hits(e: &Env) -> usize {
    e.cdn.received_requests().await.unwrap().len()
}

fn dir_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

// ---- gateway ------------------------------------------------------------------------------

#[tokio::test]
async fn every_api_route_needs_the_gateway_header() {
    let e = env().await;
    for (m, uri) in [
        ("GET", "/v1/pixiv/status"),
        ("GET", "/v1/pixiv/ranking?mode=day"),
        ("GET", "/v1/pixiv/search?q=cat"),
        ("GET", "/v1/pixiv/illust/1001"),
        ("GET", "/v1/pixiv/bookmarks"),
        ("PUT", "/v1/pixiv/bookmarks/1001"),
        ("DELETE", "/v1/pixiv/bookmarks/1001"),
        ("POST", "/v1/pixiv/save"),
    ] {
        let r = send(&e.app, m, uri, false, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {uri}");
        assert_eq!(r.json["error"], "unauthorized");
    }
    // a wrong secret of the right length, and one of another length
    for wrong in ["gateway-secret-for-testz", "x"] {
        let req = Request::builder()
            .uri("/v1/pixiv/status")
            .header("x-lucina-gateway", wrong)
            .body(Body::empty())
            .unwrap();
        let resp = e.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(e.mock.count.load(Ordering::SeqCst), 0, "nothing reached the core");
    assert_eq!(get(&e.app, "/v1/pixiv/status").await.status, StatusCode::OK);
}

// ---- ranking, search, detail --------------------------------------------------------------

#[tokio::test]
async fn ranking_returns_signed_urls_and_caches_the_answer() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day&page=2").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["ok"], true);
    let d = &r.json["data"];
    assert_eq!(d["page"], 2);
    assert_eq!(d["hasMore"], true);
    let items = d["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "1001");
    assert_eq!(items[0]["authorId"], "7");
    assert_eq!(items[1]["pages"], 3);
    assert_eq!(items[0]["bookmarked"], false);
    let thumb = items[0]["thumbUrl"].as_str().unwrap();
    assert!(thumb.starts_with("/v1/pixiv/s/"), "{thumb}");
    assert!(!r.json.to_string().contains("pximg.net"), "no upstream URL leaves the service");
    assert_eq!(e.mock.calls(), vec!["ranking day 2"]);
    // the same request again is served from the JSON cache
    assert_eq!(get(&e.app, "/v1/pixiv/ranking?mode=day&page=2").await.status, StatusCode::OK);
    assert_eq!(e.mock.calls().len(), 1);
    // bad input
    assert_eq!(get(&e.app, "/v1/pixiv/ranking?mode=bogus").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get(&e.app, "/v1/pixiv/ranking?mode=day&page=x").await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn r18_ranking_modes_need_adult_and_a_session() {
    let e = env().await;
    for mode in ["day_r18", "week_r18", "day_male_r18", "day_female_r18"] {
        let r = get(&e.app, &format!("/v1/pixiv/ranking?mode={mode}")).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{mode}");
        assert_eq!(r.json["error"], "adult_off");
        let r = get(&e.app, &format!("/v1/pixiv/ranking?mode={mode}&adult=0")).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{mode}");
    }
    assert!(e.mock.calls().is_empty(), "refused before any upstream call");
    // anonymous: pixiv would answer 403, so the service answers an empty list with a notice
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day_r18&adult=1").await;
    assert_eq!(r.status, StatusCode::OK);
    let d = &r.json["data"];
    assert_eq!(d["notice"], "r18_needs_session");
    assert_eq!((d["hasMore"].clone(), d["total"].clone(), d["numPages"].clone()), (json!(false), json!(0), json!(0)));
    assert!(d["items"].as_array().unwrap().is_empty());
    assert!(e.mock.calls().is_empty(), "no upstream request");
    // with a session that shows R-18
    e.mock.login(r18_account());
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day_r18&adult=1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["items"][0]["r18"], true);
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(r.json["data"]["notice"], Value::Null);
    assert_eq!(e.mock.calls(), vec!["self_status", "ranking day_r18 1"]);
    // the cached R-18 page is not handed to a request without adult
    assert_eq!(get(&e.app, "/v1/pixiv/ranking?mode=day_r18").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn search_filters_r18_items_unless_adult() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/search?q=blue%20sky&mode=all&order=date&type=manga&page=3").await;
    assert_eq!(r.status, StatusCode::OK);
    let ids: Vec<&str> = r.json["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["2001", "2003"], "the R-18 item is gone");
    assert_eq!(r.json["data"]["total"], 3);
    assert_eq!(r.json["data"]["numPages"], 2);
    // "all" without adult asks pixiv for the all-ages results; words are separate arguments
    assert_eq!(
        e.mock.calls(),
        vec!["search blue sky --order date --mode safe --type manga --page 3"]
    );
    // a thumbnail on another host never gets a URL
    assert_eq!(r.json["data"]["items"][1]["thumbUrl"], "");

    let r = get(&e.app, "/v1/pixiv/search?q=blue%20sky&mode=all&order=date&type=manga&page=3&adult=1").await;
    let ids: Vec<&str> = r.json["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["2001", "2002", "2003"]);
    assert!(e.mock.calls()[1].contains("--mode all"));
    // anonymous: pixiv only hands out all-ages works, and says so
    assert_eq!(r.json["data"]["notice"], "all_ages_only");
    assert_eq!(r.json["data"]["sort"], json!({"requested": "date", "used": "date"}));
}

#[tokio::test]
async fn r18_search_mode_is_refused_without_adult() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=r18").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"], "adult_off");
    assert!(e.mock.calls().is_empty());
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=r18&adult=1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["notice"], "r18_needs_session");
    assert!(r.json["data"]["items"].as_array().unwrap().is_empty());
    assert!(e.mock.calls().is_empty(), "anonymous R-18 makes no upstream request");
}

#[tokio::test]
async fn search_validates_its_input() {
    let e = env().await;
    for q in [
        "q=",
        "q=%20%20",
        "q=cat&order=popular_d",
        "q=cat&order=popularity",
        "q=cat&mode=weird",
        "q=cat&type=video",
        "q=cat&page=-1",
    ] {
        let r = get(&e.app, &format!("/v1/pixiv/search?{q}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
    }
    // flags typed as words cannot become flags
    let r = get(&e.app, "/v1/pixiv/search?q=%2D%2Dmode%20r18%20cat").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(e.mock.calls()[0].starts_with("search mode r18 cat --order date_d --mode safe"));
}

#[tokio::test]
async fn illust_detail_lists_every_page() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/illust/4004").await;
    assert_eq!(r.status, StatusCode::OK);
    let d = &r.json["data"];
    assert_eq!(d["title"], "Work Title");
    assert_eq!(d["kind"], "manga");
    assert_eq!(d["pageCount"], 2);
    assert_eq!(d["stats"]["bookmarks"], 3);
    assert_eq!(d["tags"][0]["tag"], "風景");
    assert_eq!(d["tags"][0]["en"], "landscape");
    let pages = d["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1]["index"], 1);
    for p in pages {
        for k in ["thumbUrl", "url", "originalUrl"] {
            assert!(p[k].as_str().unwrap().starts_with("/v1/pixiv/s/"), "{k}");
        }
    }
    assert!(d["coverUrl"].as_str().unwrap().starts_with("/v1/pixiv/s/"));
    assert!(!r.json.to_string().contains("pximg.net"));
    // ugoira: shown as its cover
    let r = get(&e.app, "/v1/pixiv/illust/5005").await;
    assert_eq!(r.json["data"]["ugoira"], true);
    assert_eq!(r.json["data"]["pages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn r18_works_are_refused_without_adult() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/illust/3003").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"], "adult_off");
    assert!(!r.json.to_string().contains("/s/"));
    assert_eq!(get(&e.app, "/v1/pixiv/illust/3003?adult=1").await.status, StatusCode::OK);
    // and the cached copy is still refused afterwards
    assert_eq!(get(&e.app, "/v1/pixiv/illust/3003").await.status, StatusCode::FORBIDDEN);
    assert_eq!(get(&e.app, "/v1/pixiv/illust/404").await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&e.app, "/v1/pixiv/illust/abc").await.status, StatusCode::BAD_REQUEST);
}

// ---- signed image URLs --------------------------------------------------------------------

#[tokio::test]
async fn signed_urls_serve_images_with_the_referer_and_cache_them() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day").await;
    let thumb = r.json["data"]["items"][0]["thumbUrl"].as_str().unwrap().to_string();
    // no gateway header on the signed route
    let img1 = send(&e.app, "GET", &thumb, false, None).await;
    assert_eq!(img1.status, StatusCode::OK);
    assert_eq!(img1.headers["content-type"], "image/png");
    assert_eq!(img1.headers["x-content-type-options"], "nosniff");
    assert_eq!(img1.bytes.len(), 300);
    assert_eq!(cdn_hits(&e).await, 1);
    let req = &e.cdn.received_requests().await.unwrap()[0];
    assert!(req.url.path().contains("1001_p0_master1200.jpg"));
    // again: from the disk cache, and HEAD works too
    assert_eq!(send(&e.app, "GET", &thumb, false, None).await.status, StatusCode::OK);
    let head = send(&e.app, "HEAD", &thumb, false, None).await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(cdn_hits(&e).await, 1);
}

#[tokio::test]
async fn signed_url_tokens_are_lowercase_hex() {
    // Lucina shows these links in chat; her secret redactor masks mixed-case random-looking
    // segments (base64), so the token must stay lowercase hex.
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day").await;
    let thumb = r.json["data"]["items"][0]["thumbUrl"].as_str().unwrap().to_string();
    let token = thumb.trim_start_matches("/v1/pixiv/s/").split('/').nth(1).unwrap();
    assert!(token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{token}");
    assert_eq!(String::from_utf8(hex::decode(token).unwrap()).unwrap(), img(1001, 0));
    assert_eq!(send(&e.app, "GET", &thumb, false, None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn bad_and_expired_signatures_are_refused() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/ranking?mode=day").await;
    let thumb = r.json["data"]["items"][0]["thumbUrl"].as_str().unwrap().to_string();
    let mut parts: Vec<String> = thumb.split('/').map(str::to_string).collect();
    // tampered signature
    let last = parts.len() - 1;
    let sig = parts[last].clone();
    parts[last] = format!("{}{}", &sig[..sig.len() - 1], if sig.ends_with('0') { '1' } else { '0' });
    let bad = parts.join("/");
    let r = send(&e.app, "GET", &bad, false, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // another token under the same signature
    let other = e.http.sign_unchecked(&img(9999, 0));
    let swapped = format!(
        "{}/{}",
        other.rsplit_once('/').unwrap().0,
        thumb.rsplit_once('/').unwrap().1
    );
    assert_eq!(send(&e.app, "GET", &swapped, false, None).await.status, StatusCode::FORBIDDEN);
    // expired, even with a correct signature
    let old = e.http.sign_at(&img(1001, 0), chrono::Utc::now().timestamp() - 10);
    assert_eq!(send(&e.app, "GET", &old, false, None).await.status, StatusCode::FORBIDDEN);
    // garbage
    for u in [
        "/v1/pixiv/s/1/2/3",
        "/v1/pixiv/s/99999999999/aGVsbG8/00000000000000000000000000000000",
    ] {
        assert_eq!(send(&e.app, "GET", u, false, None).await.status, StatusCode::FORBIDDEN, "{u}");
    }
    assert_eq!(cdn_hits(&e).await, 0, "nothing was fetched");
}

#[tokio::test]
async fn hosts_other_than_pximg_are_refused_even_when_signed() {
    let e = env().await;
    for evil in [
        "https://evil.test/a.jpg",
        "https://i.pximg.net.evil.test/a.jpg",
        "http://i.pximg.net/a.jpg",
        "https://user@i.pximg.net/a.jpg",
        "https://www.pixiv.net/ajax/illust/1",
        "http://127.0.0.1:8870/v1/nclient/status",
    ] {
        let url = e.http.sign_unchecked(evil);
        let r = send(&e.app, "GET", &url, false, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{evil}");
    }
    assert_eq!(e.http.signed_url("https://evil.test/a.jpg"), "");
    assert_eq!(cdn_hits(&e).await, 0);
}

#[tokio::test]
async fn the_disk_cache_stays_under_its_cap_and_drops_the_oldest() {
    // each cached image is 1000 bytes + the content type line
    let e = env_with(Opts {
        cache_max: 2500,
        image_bytes: 1000,
        ..Opts::default()
    })
    .await;
    let mut urls = Vec::new();
    for n in 1..=4 {
        urls.push(e.http.signed_url(&img(n, 0)));
    }
    for u in &urls {
        assert_eq!(send(&e.app, "GET", u, false, None).await.status, StatusCode::OK);
    }
    assert_eq!(cdn_hits(&e).await, 4);
    let cache = e.dir.path().join("cache");
    assert!(dir_bytes(&cache) <= 2500, "{} bytes on disk", dir_bytes(&cache));
    assert!(e.http.service.cache.total_bytes() <= 2500);
    // the newest is still cached, the oldest was evicted and is fetched again
    assert_eq!(send(&e.app, "GET", &urls[3], false, None).await.status, StatusCode::OK);
    assert_eq!(cdn_hits(&e).await, 4);
    assert_eq!(send(&e.app, "GET", &urls[0], false, None).await.status, StatusCode::OK);
    assert_eq!(cdn_hits(&e).await, 5);
    assert!(dir_bytes(&cache) <= 2500);
}

// ---- rate limits --------------------------------------------------------------------------

#[tokio::test]
async fn a_429_is_retried_with_backoff() {
    let e = env().await;
    e.mock.failures.lock().unwrap().extend([
        "HTTP 429 Too Many Requests: slow down".to_string(),
        "HTTP 503 Service Unavailable: busy".to_string(),
    ]);
    let started = std::time::Instant::now();
    let r = get(&e.app, "/v1/pixiv/ranking?mode=week").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(e.mock.calls().len(), 3, "two failures, then the answer");
    assert!(started.elapsed() >= Duration::from_millis(10), "it waited between attempts");
}

#[tokio::test]
async fn a_429_that_persists_answers_rate_limited_and_cools_down() {
    let e = env().await;
    *e.mock.always.lock().unwrap() = Some("HTTP 429 Too Many Requests: slow".into());
    let r = get(&e.app, "/v1/pixiv/ranking?mode=month").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["ok"], false);
    assert_eq!(r.json["error"], "rate_limited");
    assert!(r.json["retryAfter"].as_u64().unwrap() >= 1);
    assert_eq!(r.headers["retry-after"], r.json["retryAfter"].to_string());
    assert_eq!(e.mock.calls().len(), 3, "three attempts");
    // cooling down: further calls fail fast without touching pixiv
    let r = get(&e.app, "/v1/pixiv/search?q=cat").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(e.mock.calls().len(), 3);
    // cached answers and images keep working
    assert_eq!(get(&e.app, "/v1/pixiv/status").await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_dead_upstream_is_a_502_not_a_429() {
    let e = env().await;
    *e.mock.always.lock().unwrap() = Some("network: connection refused".into());
    let r = get(&e.app, "/v1/pixiv/illust/1001").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json["error"], "upstream_unavailable");
    // a 403 from the edge is not retried
    *e.mock.always.lock().unwrap() = Some("HTTP 403 Forbidden: blocked".into());
    let before = e.mock.calls().len();
    assert_eq!(get(&e.app, "/v1/pixiv/illust/1002").await.status, StatusCode::BAD_GATEWAY);
    assert_eq!(e.mock.calls().len(), before + 1);
}

#[tokio::test]
async fn the_cdn_429_is_retried_then_cools_down() {
    let cdn = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&cdn)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(".*"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![1u8; 10], "image/jpeg"))
        .with_priority(2)
        .mount(&cdn)
        .await;
    let mut e = env().await;
    // rebuild the service against this CDN
    let mock = e.mock.clone();
    let service = Service::new(
        mock,
        DiskCache::new(e.dir.path().join("cache2"), 1 << 20).unwrap(),
        e.dir.path().join("spool2"),
        Config {
            gate: GateConfig::default(),
            images: ImageConfig {
                base_override: Some(cdn.uri()),
                spacing: Duration::from_millis(1),
                backoff_base: Duration::from_millis(5),
                ..ImageConfig::default()
            },
            ..Config::default()
        },
    )
    .unwrap();
    e.http = Arc::new(Http {
        service,
        store: Store::open(&e.dir.path().join("owners2"), "owner").unwrap(),
        gateway_secret: SECRET.as_bytes().to_vec(),
        signing_key: vec![7; 32],
    });
    e.app = router(e.http.clone());
    let u = e.http.signed_url(&img(1, 0));
    let r = send(&e.app, "GET", &u, false, None).await;
    assert_eq!(r.status, StatusCode::OK, "retried after the 429");
    assert_eq!(cdn.received_requests().await.unwrap().len(), 2);

    // a CDN that only says 429: clean rate_limited, then fail fast
    let cdn2 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .mount(&cdn2)
        .await;
    let service = Service::new(
        e.mock.clone(),
        DiskCache::new(e.dir.path().join("cache3"), 1 << 20).unwrap(),
        e.dir.path().join("spool3"),
        Config {
            images: ImageConfig {
                base_override: Some(cdn2.uri()),
                spacing: Duration::from_millis(1),
                backoff_base: Duration::from_millis(5),
                ..ImageConfig::default()
            },
            ..Config::default()
        },
    )
    .unwrap();
    let http = Arc::new(Http {
        service,
        store: Store::open(&e.dir.path().join("owners3"), "owner").unwrap(),
        gateway_secret: SECRET.as_bytes().to_vec(),
        signing_key: vec![7; 32],
    });
    let app = router(http.clone());
    let u = http.signed_url(&img(2, 0));
    let r = send(&app, "GET", &u, false, None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["error"], "rate_limited");
    assert_eq!(r.json["retryAfter"], 2);
    let seen = cdn2.received_requests().await.unwrap().len();
    let r = send(&app, "GET", &http.signed_url(&img(3, 0)), false, None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(cdn2.received_requests().await.unwrap().len(), seen, "cooling down");
}

// ---- bookmarks ----------------------------------------------------------------------------

#[tokio::test]
async fn bookmarks_are_local_and_private() {
    let e = env().await;
    let r = send(&e.app, "PUT", "/v1/pixiv/bookmarks/1001", true, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["bookmarked"], true);
    let list = get(&e.app, "/v1/pixiv/bookmarks").await;
    let items = list.json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "1001");
    assert_eq!(items[0]["bookmarked"], true);
    assert!(items[0]["thumbUrl"].as_str().unwrap().starts_with("/v1/pixiv/s/"));
    assert!(items[0]["addedAt"].as_i64().unwrap() > 0);
    // it is reflected in the detail and in lists
    assert_eq!(get(&e.app, "/v1/pixiv/illust/1001").await.json["data"]["bookmarked"], true);
    assert_eq!(
        get(&e.app, "/v1/pixiv/ranking?mode=day").await.json["data"]["items"][0]["bookmarked"],
        true
    );
    // the file is 0600 and holds no signed URL
    let file = e.dir.path().join("owners/owner.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("1001") && !text.contains("/v1/pixiv/s/"));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // R-18 works: refused without adult, hidden from the list without adult
    assert_eq!(send(&e.app, "PUT", "/v1/pixiv/bookmarks/3003", true, None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(send(&e.app, "PUT", "/v1/pixiv/bookmarks/3003?adult=1", true, None).await.status, StatusCode::OK);
    assert_eq!(get(&e.app, "/v1/pixiv/bookmarks").await.json["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(get(&e.app, "/v1/pixiv/bookmarks?adult=1").await.json["data"]["items"].as_array().unwrap().len(), 2);
    // delete
    let r = send(&e.app, "DELETE", "/v1/pixiv/bookmarks/1001", true, None).await;
    assert_eq!(r.json["data"]["bookmarked"], false);
    assert_eq!(get(&e.app, "/v1/pixiv/bookmarks?adult=1").await.json["data"]["items"].as_array().unwrap().len(), 1);
}

// ---- save to Lucina -----------------------------------------------------------------------

#[tokio::test]
async fn save_ingests_every_original_page() {
    let e = env_with(Opts {
        image_bytes: 4321,
        ..Opts::default()
    })
    .await;
    let r = send(&e.app, "POST", "/v1/pixiv/save", true, Some(json!({"id": "4004"}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.bytes));
    let d = &r.json["data"];
    assert_eq!(d["failed"].as_array().unwrap().len(), 0);
    let saved = d["saved"].as_array().unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["itemId"], "item-1");
    assert_eq!(saved[0]["duplicate"], false);
    assert_eq!(saved[1]["itemId"], "item-2");
    assert_eq!(saved[1]["duplicate"], true);

    let bodies = e.ingested.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    for (i, b) in bodies.iter().enumerate() {
        assert_eq!(b["source"], "pixiv");
        assert_eq!(b["album"], "Pixiv");
        assert_eq!(b["move"], true);
        assert_eq!(b["mime"], "image/png");
        assert_eq!(b["name"], format!("pixiv_4004_p{i}.png"));
        let caption = b["caption"].as_str().unwrap();
        assert!(caption.contains("Work Title") && caption.contains("Artist") && caption.contains("4004"), "{caption}");
        assert_eq!(b["taken_at"], "2024-05-06T07:08:09+00:00");
        assert!(b["path"].as_str().unwrap().starts_with(e.dir.path().join("spool").to_str().unwrap()));
    }
    assert_eq!(*e.ingested.sizes.lock().unwrap(), vec![4321, 4321], "the full file was there");
    // the originals were fetched (not the 1200 px versions), with the Referer
    let urls: Vec<String> = e
        .cdn
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(urls.len(), 2);
    assert!(urls.iter().all(|u| u.contains("img-original")), "{urls:?}");
    // nothing is left in the spool
    assert_eq!(std::fs::read_dir(e.dir.path().join("spool")).unwrap().count(), 0);
}

#[tokio::test]
async fn save_one_page_only() {
    let e = env().await;
    let r = send(&e.app, "POST", "/v1/pixiv/save", true, Some(json!({"id": 4004, "pages": [1]}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["saved"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["data"]["saved"][0]["page"], 1);
    let b = e.ingested.bodies.lock().unwrap().clone();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0]["name"], "pixiv_4004_p1.png");
    // out-of-range and empty selections, junk bodies
    for body in [json!({"id": "4004", "pages": [2]}), json!({"id": "4004", "pages": []}), json!({"id": "x1"}), json!({"nope": 1})] {
        let r = send(&e.app, "POST", "/v1/pixiv/save", true, Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(e.ingested.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn save_refuses_r18_works_without_adult() {
    let e = env().await;
    let r = send(&e.app, "POST", "/v1/pixiv/save", true, Some(json!({"id": "3003"}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"], "adult_off");
    assert!(e.ingested.bodies.lock().unwrap().is_empty());
    assert_eq!(cdn_hits(&e).await, 0);
    let r = send(&e.app, "POST", "/v1/pixiv/save?adult=1", true, Some(json!({"id": "3003", "pages": [0]}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(e.ingested.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn save_reports_a_media_outage_per_page_and_cleans_the_spool() {
    let e = env_with(Opts {
        media_fails: true,
        ..Opts::default()
    })
    .await;
    let r = send(&e.app, "POST", "/v1/pixiv/save", true, Some(json!({"id": "4004"}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["saved"].as_array().unwrap().len(), 0);
    let failed = r.json["data"]["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[0]["error"], "ingest_failed");
    assert_eq!(std::fs::read_dir(e.dir.path().join("spool")).unwrap().count(), 0);
}

// ---- session, account, content, popular order ---------------------------------------------

/// One search item; `tags` drive `popularTier`.
fn item(id: u64, r18: bool, tags: &[&str]) -> Value {
    json!({"id": id.to_string(), "title": "t", "author": "a", "author_id": "1",
           "thumb": img(id as u32, 0), "pages": 1, "r18": r18, "tags": tags})
}

fn arg<'a>(args: &'a [String], flag: &str) -> &'a str {
    let i = args.iter().position(|a| a == flag).unwrap();
    &args[i + 1]
}

/// The tier query: 5 upstream pages of 2 works each (the service reads 3); page 3 repeats an id.
/// The plain query: 3 pages, newest first, partly the same works as the tier query.
fn tier_pages(args: &[String]) -> Result<Value, String> {
    let page: u64 = arg(args, "--page").parse().unwrap();
    if !args.join(" ").contains("users入り") {
        let items = match page {
            1 => vec![item(40, false, &[]), item(31, false, &["2024年"]), item(22, false, &["100users入り"]), item(12, false, &[])],
            2 => vec![item(30, false, &[]), item(21, false, &[]), item(20, false, &[])],
            _ => vec![item(10, false, &[])],
        };
        return Ok(json!({"word": "w", "page": page, "total": 368, "num_pages": 3, "items": items}));
    }
    let items = match page {
        1 => vec![item(11, false, &["100users入り"]), item(12, false, &["ミク10000users入り", "500users入り"])],
        2 => vec![item(21, false, &["VOCALOID1000users入り"]), item(22, false, &["100users入り"])],
        3 => vec![item(12, false, &["ミク10000users入り"]), item(31, false, &["2024年"])],
        _ => vec![item(99, false, &["100000users入り"])],
    };
    Ok(json!({"word": "w", "page": page, "total": 9, "num_pages": 5, "items": items}))
}

/// No work carries a tier tag; plain searches answer as usual.
fn no_tiers(args: &[String]) -> Result<Value, String> {
    if args.join(" ").contains("users入り") {
        return Err("search: no illustManga (word may have no results)".into());
    }
    Ok(json!({"word": "w", "page": 1, "total": 1, "num_pages": 1,
              "items": [item(7, false, &["猫"])]}))
}

#[tokio::test]
async fn popular_with_premium_asks_pixiv_for_popular_d() {
    let e = env().await;
    e.mock.login(json!({"is_logged_in": true, "is_premium": 1, "x_restrict": 0}));
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["sort"], json!({"requested": "popular", "used": "pixiv_popular"}));
    assert_eq!(
        e.mock.calls(),
        vec!["self_status", "search cat --order popular_d --mode safe --type all --page 1"]
    );
}

#[tokio::test]
async fn popular_without_premium_merges_bookmark_tier_pages_then_continues_newest_first() {
    let e = env().await;
    e.mock.on_search(tier_pages);
    let base = "/v1/pixiv/search?q=blue%20sky&order=popular&type=illust";
    let r = get(&e.app, base).await;
    assert_eq!(r.status, StatusCode::OK);
    let d = &r.json["data"];
    assert_eq!(d["sort"], json!({"requested": "popular", "used": "users_tags"}));
    // three tier pages at most, newest first, the tier group after the words; then the plain
    // query's first page, to know whether anything follows the last tier page
    let group = "(100000users入り OR 50000users入り OR 30000users入り OR 20000users入り OR \
                 10000users入り OR 5000users入り OR 1000users入り OR 500users入り OR 300users入り OR 100users入り)";
    let calls = e.mock.calls();
    assert_eq!(calls.len(), 4, "{calls:?}");
    for (n, c) in calls[..3].iter().enumerate() {
        assert_eq!(c, &format!("search blue sky {group} --order date_d --mode safe --type illust --page {}", n + 1));
    }
    assert_eq!(calls[3], "search blue sky --order date_d --mode safe --type illust --page 1");
    // deduplicated, by tier then id, both descending
    assert_eq!(ids(&r), vec!["12", "21", "22", "11", "31"]);
    let tiers: Vec<Value> = d["items"].as_array().unwrap().iter().map(|i| i["popularTier"].clone()).collect();
    assert_eq!(tiers, vec![json!(10000), json!(1000), json!(100), json!(100), Value::Null]);
    // the last tier page: more follows; totals are the plain query's (1 tier page + 3 plain)
    assert_eq!((d["page"].clone(), d["hasMore"].clone(), d["numPages"].clone(), d["total"].clone()),
               (json!(1), json!(true), json!(4), json!(368)));

    // page 2 = plain page 1 without the works already shown (cached: no new call)
    let r = get(&e.app, &format!("{base}&page=2")).await;
    assert_eq!(ids(&r), vec!["40"]);
    let d = &r.json["data"];
    assert_eq!((d["page"].clone(), d["hasMore"].clone(), d["numPages"].clone(), d["total"].clone()),
               (json!(2), json!(true), json!(4), json!(368)));
    assert_eq!(d["sort"]["used"], "users_tags");
    assert_eq!(e.mock.calls().len(), 4);
    // page 3 = plain page 2, page 4 = plain page 3 (the last)
    let r = get(&e.app, &format!("{base}&page=3")).await;
    assert_eq!(ids(&r), vec!["30", "20"]);
    assert_eq!(r.json["data"]["hasMore"], true);
    let r = get(&e.app, &format!("{base}&page=4")).await;
    assert_eq!(ids(&r), vec!["10"]);
    assert_eq!((r.json["data"]["page"].clone(), r.json["data"]["hasMore"].clone()), (json!(4), json!(false)));
    let calls = e.mock.calls();
    assert_eq!(calls.len(), 6);
    assert!(calls[4].ends_with("--type illust --page 2") && !calls[4].contains("users入り"));
    assert!(calls[5].ends_with("--type illust --page 3") && !calls[5].contains("users入り"));
    // no work twice across the pages (all cached now)
    let mut all = Vec::new();
    for page in 1..=4 {
        all.extend(ids(&get(&e.app, &format!("{base}&page={page}")).await));
    }
    assert_eq!(all.len(), 9, "{all:?}");
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 9, "a work appeared twice");
    assert_eq!(e.mock.calls().len(), 6);
}

#[tokio::test]
async fn popular_tier_pages_before_the_last_need_no_plain_query() {
    let e = env().await;
    // 70 tier works on one upstream page: two local tier pages
    e.mock.on_search(|args| {
        if !args.join(" ").contains("users入り") {
            return Ok(json!({"total": 500, "num_pages": 9, "items": [item(1, false, &[])]}));
        }
        let items: Vec<Value> = (100..170).map(|i| item(i, false, &["100users入り"])).collect();
        Ok(json!({"total": 70, "num_pages": 1, "items": items}))
    });
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular").await;
    let d = &r.json["data"];
    assert_eq!(d["items"].as_array().unwrap().len(), 60);
    assert_eq!(d["items"][0]["id"], "169");
    assert_eq!((d["hasMore"].clone(), d["numPages"].clone(), d["total"].clone()), (json!(true), json!(2), json!(70)));
    assert_eq!(e.mock.calls().len(), 1, "the tier query only");
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular&page=2").await;
    let d = &r.json["data"];
    assert_eq!(d["items"].as_array().unwrap().len(), 10);
    assert_eq!((d["hasMore"].clone(), d["numPages"].clone(), d["total"].clone()), (json!(true), json!(11), json!(500)));
    assert_eq!(e.mock.calls().len(), 2);
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular&page=3").await;
    assert_eq!(ids(&r), vec!["1"]);
    assert_eq!(r.json["data"]["page"], 3);
    assert_eq!(e.mock.calls().len(), 2, "plain page 1 was cached");
}

#[tokio::test]
async fn popular_falls_back_to_date_d_when_no_tier_matches() {
    let e = env().await;
    e.mock.on_search(no_tiers);
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["sort"]["used"], "date_d_fallback");
    assert_eq!(ids(&r), vec!["7"]);
    let calls = e.mock.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].contains("users入り"));
    assert_eq!(calls[1], "search cat --order date_d --mode safe --type all --page 1");
    assert_eq!((r.json["data"]["page"].clone(), r.json["data"]["hasMore"].clone()), (json!(1), json!(false)));
    // its later pages are the plain query's, still reported as the fallback
    let r = get(&e.app, "/v1/pixiv/search?q=cat&order=popular&page=2").await;
    assert_eq!(r.json["data"]["sort"]["used"], "date_d_fallback");
    assert_eq!(r.json["data"]["page"], 2);
    assert_eq!(e.mock.calls()[2], "search cat --order date_d --mode safe --type all --page 2");
}

#[tokio::test]
async fn date_orders_pass_through_and_carry_popular_tier() {
    let e = env().await;
    e.mock.on_search(|args| {
        Ok(json!({"total": 1, "num_pages": 1, "items": [item(5, false, &["VOCALOID500users入り"])],
                  "word": args[0]}))
    });
    for order in ["date_d", "date"] {
        let r = get(&e.app, &format!("/v1/pixiv/search?q=miku&order={order}")).await;
        assert_eq!(r.json["data"]["sort"], json!({"requested": order, "used": order}));
        assert_eq!(r.json["data"]["items"][0]["popularTier"], 500);
        assert_eq!(r.json["data"]["items"][0]["tags"][0], "VOCALOID500users入り");
    }
    assert_eq!(
        e.mock.calls(),
        vec![
            "search miku --order date_d --mode safe --type all --page 1",
            "search miku --order date --mode safe --type all --page 1",
        ]
    );
}

#[tokio::test]
async fn one_content_filter_for_search_ranking_and_bookmarks() {
    let e = env().await;
    e.mock.login(r18_account());
    // search
    assert_eq!(ids(&get(&e.app, "/v1/pixiv/search?q=cat&mode=safe&adult=1").await), vec!["2001", "2003"]);
    assert_eq!(ids(&get(&e.app, "/v1/pixiv/search?q=cat&mode=r18&adult=1").await), vec!["2002"]);
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=all&adult=1").await;
    assert_eq!(ids(&r), vec!["2001", "2002", "2003"]);
    assert_eq!(r.json["data"]["notice"], Value::Null, "a session gets R-18 from pixiv");
    // without adult: r18 refused, all is safe
    assert_eq!(get(&e.app, "/v1/pixiv/search?q=cat&mode=r18").await.json["error"], "adult_off");
    assert_eq!(ids(&get(&e.app, "/v1/pixiv/search?q=cat&mode=all").await), vec!["2001", "2003"]);
    // ranking: R-18 rows only with adult
    assert_eq!(ids(&get(&e.app, "/v1/pixiv/ranking?mode=day").await), vec!["1001", "1002"]);
    assert_eq!(ids(&get(&e.app, "/v1/pixiv/ranking?mode=day&adult=1").await), vec!["1001", "1002", "1003"]);
    // bookmarks
    send(&e.app, "PUT", "/v1/pixiv/bookmarks/1001", true, None).await;
    send(&e.app, "PUT", "/v1/pixiv/bookmarks/3003?adult=1", true, None).await;
    let b = |q: &'static str| {
        let app = e.app.clone();
        async move { get(&app, &format!("/v1/pixiv/bookmarks{q}")).await }
    };
    assert_eq!(ids(&b("?adult=1").await), vec!["1001", "3003"], "all by default with adult");
    assert_eq!(ids(&b("?adult=1&content=safe").await), vec!["1001"]);
    assert_eq!(ids(&b("?adult=1&content=r18").await), vec!["3003"]);
    assert_eq!(ids(&b("").await), vec!["1001"], "safe by default without adult");
    assert_eq!(ids(&b("?content=all").await), vec!["1001"]);
    let r = b("?content=r18").await;
    assert_eq!((r.status, r.json["error"].clone()), (StatusCode::FORBIDDEN, json!("adult_off")));
    assert_eq!(b("?content=weird&adult=1").await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn r18_disabled_in_the_account_answers_empty_without_a_list_call() {
    let e = env().await;
    e.mock.login(json!({"is_logged_in": true, "x_restrict": 0}));
    for uri in ["/v1/pixiv/search?q=cat&mode=r18&adult=1", "/v1/pixiv/ranking?mode=week_r18&adult=1"] {
        let r = get(&e.app, uri).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        assert_eq!(r.json["data"]["notice"], "r18_disabled_in_account", "{uri}");
        assert!(ids(&r).is_empty());
    }
    assert_eq!(e.mock.calls(), vec!["self_status"], "only the account check");
}

#[tokio::test]
async fn an_unknown_r18_setting_is_found_out_from_the_answer() {
    let e = env().await;
    e.mock.login(json!({"is_logged_in": true}));
    e.mock.on_search(|_| Ok(json!({"total": 1, "num_pages": 1, "items": [item(8, false, &[])]})));
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=r18&adult=1").await;
    assert!(ids(&r).is_empty());
    assert_eq!(r.json["data"]["notice"], "r18_disabled_in_account");
    assert_eq!(e.mock.calls().len(), 2, "the account check and the search");
}

#[tokio::test]
async fn the_account_shows_in_status() {
    let e = env().await;
    let r = get(&e.app, "/v1/pixiv/status").await;
    assert_eq!(r.json["data"]["account"], json!({"session": "none", "premium": false, "r18": false}));
    assert!(e.mock.calls().is_empty(), "no session, no request");

    for (status, want) in [
        (json!({"is_logged_in": false}), json!({"session": "expired", "premium": false, "r18": false})),
        (json!({"is_logged_in": true, "is_premium": true, "x_restrict": "1"}), json!({"session": "ok", "premium": true, "r18": true})),
        (json!({"is_logged_in": true, "is_premium": "0", "x_restrict": 1}), json!({"session": "ok", "premium": false, "r18": true})),
        (json!({"is_logged_in": true, "x_restrict": 2}), json!({"session": "ok", "premium": false, "r18": true})),
        (json!({"is_logged_in": true, "x_restrict": 0}), json!({"session": "ok", "premium": false, "r18": false})),
        (json!({"is_logged_in": true}), json!({"session": "ok", "premium": false, "r18": null})),
    ] {
        let e = env().await;
        e.mock.login(status.clone());
        assert_eq!(get(&e.app, "/v1/pixiv/status").await.json["data"]["account"], want, "{status}");
        // kept for half an hour
        assert_eq!(get(&e.app, "/v1/pixiv/status").await.json["data"]["account"], want);
        assert_eq!(e.mock.calls(), vec!["self_status"], "{status}");
    }
}

#[tokio::test]
async fn a_failed_account_check_is_not_kept() {
    let e = env().await;
    e.mock.login(r18_account());
    e.mock.failures.lock().unwrap().push_back("HTTP 403 Forbidden: no".into());
    let r = get(&e.app, "/v1/pixiv/status").await;
    assert_eq!(r.json["data"]["account"], json!({"session": "ok", "premium": false, "r18": null}));
    let r = get(&e.app, "/v1/pixiv/status").await;
    assert_eq!(r.json["data"]["account"]["r18"], true);
    assert_eq!(e.mock.calls(), vec!["self_status", "self_status"]);
}

#[tokio::test]
async fn an_expired_session_needs_a_new_one_for_r18() {
    let e = env().await;
    e.mock.login(json!({"is_logged_in": false}));
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=r18&adult=1").await;
    assert_eq!(r.json["data"]["notice"], "r18_needs_session");
    let r = get(&e.app, "/v1/pixiv/search?q=cat&mode=all&adult=1").await;
    assert_eq!(r.json["data"]["notice"], "all_ages_only");
    assert_eq!(e.mock.calls().len(), 2, "the account check, then the all-ages search");
}

#[tokio::test]
async fn anonymous_answers_are_never_served_to_a_session() {
    let e = env().await;
    let uri = "/v1/pixiv/search?q=cat&mode=all&adult=1";
    assert_eq!(get(&e.app, uri).await.json["data"]["notice"], "all_ages_only");
    assert_eq!(get(&e.app, "/v1/pixiv/ranking?mode=day&adult=1").await.status, StatusCode::OK);
    assert_eq!(e.mock.calls().len(), 2);
    e.mock.login(r18_account());
    let r = get(&e.app, uri).await;
    assert_eq!(r.json["data"]["notice"], Value::Null);
    get(&e.app, "/v1/pixiv/ranking?mode=day&adult=1").await;
    assert_eq!(
        e.mock.calls()[2..],
        ["self_status", "search cat --order date_d --mode all --type all --page 1", "ranking day 1"]
    );
    // and the session's own answers are cached under its key
    get(&e.app, uri).await;
    get(&e.app, "/v1/pixiv/ranking?mode=day&adult=1").await;
    assert_eq!(e.mock.calls().len(), 5);
}
