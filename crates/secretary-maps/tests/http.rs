use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    routing::post,
    Json, Router,
};
use http_body_util::BodyExt;
use secretary_maps::{app, AppState, Config, Home};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tower::ServiceExt;

const TILES: &[u8] = b"PMTiles\x03-0123456789abcdefghijklmnopqrstuvwxyz";

fn setup(dir: &Path) {
    std::fs::create_dir_all(dir.join("tiles")).unwrap();
    std::fs::write(dir.join("tiles/region.pmtiles"), TILES).unwrap();
    std::fs::create_dir_all(dir.join("assets/fonts/Noto Sans Regular")).unwrap();
    std::fs::write(
        dir.join("assets/fonts/Noto Sans Regular/0-255.pbf"),
        b"\x0a\x01x",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("assets/sprites/v4")).unwrap();
    std::fs::write(dir.join("assets/sprites/v4/light.json"), b"{}").unwrap();
    std::fs::write(dir.join("assets/sprites/v4/light@2x.png"), b"\x89PNG").unwrap();
    std::fs::write(dir.join("secret.txt"), b"outside").unwrap();
}

fn config(dir: &Path, google: &str) -> Config {
    Config {
        dir: dir.to_path_buf(),
        tiles: "region".into(),
        home: Some(Home {
            lat_e7: 561_963_300,
            lng_e7: 102_011_000,
            label: "Home".into(),
        }),
        // Port 1: nothing listens, connections are refused at once.
        google_key: None,
        places_url: google.into(),
        routes_url: google.into(),
        state_dir: dir.join("state"),
        upstream_key: None,
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = res.into_parts();
    (
        parts.status,
        parts.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn get_req(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn json_of(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap()
}

#[tokio::test]
async fn config_and_health() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let a = app(AppState::new(config(tmp.path(), "http://127.0.0.1:1")));
    let (s, _, b) = send(&a, get_req("/v1/maps/config")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        json_of(&b),
        json!({"tiles":"/v1/maps/tiles/region.pmtiles","assets":"/v1/maps/assets",
            "home":{"lat":56.19633,"lng":10.2011,"label":"Home"},"geocoder":false,"routes":false,
            "attribution":"© OpenStreetMap contributors · Protomaps"})
    );
    let (s, _, b) = send(&a, get_req("/v1/maps/health")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        json_of(&b),
        json!({"ok":true,"tiles":true,"geocoder":false})
    );

    std::fs::remove_file(tmp.path().join("tiles/region.pmtiles")).unwrap();
    let mut c = config(tmp.path(), "http://127.0.0.1:1");
    c.home = None;
    let a = app(AppState::new(c));
    let (_, _, b) = send(&a, get_req("/v1/maps/config")).await;
    let v = json_of(&b);
    assert_eq!(
        (v["tiles"].clone(), v["home"].clone()),
        (Value::Null, Value::Null)
    );
}

#[tokio::test]
async fn tiles_ranges_and_validators() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let a = app(AppState::new(config(tmp.path(), "http://127.0.0.1:1")));

    let (s, h, b) = send(&a, get_req("/v1/maps/tiles/region.pmtiles")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, TILES);
    assert_eq!(h[header::CACHE_CONTROL], "private, max-age=86400");
    assert_eq!(h[header::ACCEPT_RANGES], "bytes");
    assert!(h.contains_key(header::LAST_MODIFIED));
    let etag = h[header::ETAG].to_str().unwrap().to_string();

    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::RANGE, "bytes=0-9")
        .body(Body::empty());
    let (s, h, b) = send(&a, req.unwrap()).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    assert_eq!(b, &TILES[..10]);
    assert_eq!(
        h[header::CONTENT_RANGE],
        format!("bytes 0-9/{}", TILES.len())
    );
    assert_eq!(h[header::ETAG], etag.as_str());
    assert_eq!(h[header::CACHE_CONTROL], "private, max-age=86400");

    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::RANGE, "bytes=1000-2000")
        .body(Body::empty());
    let (s, _, _) = send(&a, req.unwrap()).await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);

    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty());
    let (s, h, b) = send(&a, req.unwrap()).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    assert!(b.is_empty());
    assert_eq!(h[header::ETAG], etag.as_str());

    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::IF_MATCH, "\"other\"")
        .body(Body::empty());
    assert_eq!(
        send(&a, req.unwrap()).await.0,
        StatusCode::PRECONDITION_FAILED
    );

    // If-Range: our tag keeps the range, a stale one gets the whole file.
    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::RANGE, "bytes=2-3")
        .header(header::IF_RANGE, &etag)
        .body(Body::empty());
    let (s, _, b) = send(&a, req.unwrap()).await;
    assert_eq!(
        (s, b.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &TILES[2..4])
    );
    let req = Request::get("/v1/maps/tiles/region.pmtiles")
        .header(header::RANGE, "bytes=2-3")
        .header(header::IF_RANGE, "\"stale\"")
        .body(Body::empty());
    let (s, _, b) = send(&a, req.unwrap()).await;
    assert_eq!((s, b.as_slice()), (StatusCode::OK, TILES));

    for bad in [
        "/v1/maps/tiles/missing.pmtiles",
        "/v1/maps/tiles/Region.pmtiles",
        "/v1/maps/tiles/region.mbtiles",
        "/v1/maps/tiles/..%2Fsecret.pmtiles",
        "/v1/maps/tiles/region",
    ] {
        let (s, _, b) = send(&a, get_req(bad)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{bad}");
        assert_eq!(json_of(&b), json!({"error":"not_found"}));
    }
}

#[tokio::test]
async fn assets_and_paths() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    std::os::unix::fs::symlink(
        tmp.path().join("secret.txt"),
        tmp.path().join("assets/link.json"),
    )
    .unwrap();
    let a = app(AppState::new(config(tmp.path(), "http://127.0.0.1:1")));

    let (s, h, b) = send(
        &a,
        get_req("/v1/maps/assets/fonts/Noto%20Sans%20Regular/0-255.pbf"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"\x0a\x01x");
    assert_eq!(h[header::CONTENT_TYPE], "application/x-protobuf");
    assert_eq!(h[header::CACHE_CONTROL], "private, max-age=604800");
    let (_, h, _) = send(&a, get_req("/v1/maps/assets/sprites/v4/light.json")).await;
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    let (_, h, _) = send(&a, get_req("/v1/maps/assets/sprites/v4/light@2x.png")).await;
    assert_eq!(h[header::CONTENT_TYPE], "image/png");

    for (uri, want) in [
        ("/v1/maps/assets/../secret.txt", StatusCode::BAD_REQUEST),
        (
            "/v1/maps/assets/fonts/..%2F..%2Fsecret.txt",
            StatusCode::BAD_REQUEST,
        ),
        ("/v1/maps/assets/%2Fetc%2Fpasswd", StatusCode::BAD_REQUEST),
        (
            "/v1/maps/assets/fonts%5C..%5Csecret.txt",
            StatusCode::BAD_REQUEST,
        ),
        ("/v1/maps/assets/fonts/a%00.pbf", StatusCode::BAD_REQUEST),
        ("/v1/maps/assets/fonts/%252e%252e", StatusCode::BAD_REQUEST),
        ("/v1/maps/assets/link.json", StatusCode::NOT_FOUND),
        ("/v1/maps/assets/fonts", StatusCode::NOT_FOUND),
        ("/v1/maps/assets/fonts/none.pbf", StatusCode::NOT_FOUND),
    ] {
        let (s, _, b) = send(&a, get_req(uri)).await;
        assert_eq!(s, want, "{uri}");
        assert!(json_of(&b)["error"].is_string(), "{uri}");
    }
}

#[tokio::test]
async fn upstream_key() {
    let tmp = tempfile::tempdir().unwrap();
    setup(tmp.path());
    let mut c = config(tmp.path(), "http://127.0.0.1:1");
    c.upstream_key = Some("0123456789abcdef-key".into());
    let a = app(AppState::new(c));
    assert_eq!(send(&a, get_req("/v1/maps/health")).await.0, StatusCode::OK);
    assert_eq!(
        send(&a, get_req("/v1/maps/config")).await.0,
        StatusCode::UNAUTHORIZED
    );
    let req = Request::get("/v1/maps/config")
        .header("x-maps-upstream-key", "0123456789abcdef-kez")
        .body(Body::empty());
    assert_eq!(send(&a, req.unwrap()).await.0, StatusCode::UNAUTHORIZED);
    let req = Request::get("/v1/maps/config")
        .header("x-maps-upstream-key", "0123456789abcdef-key")
        .body(Body::empty());
    assert_eq!(send(&a, req.unwrap()).await.0, StatusCode::OK);
    assert_eq!(
        send(&a, get_req("/v1/maps/nope")).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn not_found_json() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(AppState::new(config(tmp.path(), "http://127.0.0.1:1")));
    for uri in ["/", "/v1/maps", "/v1/maps/other", "/v1/other"] {
        let (s, _, b) = send(&a, get_req(uri)).await;
        assert_eq!(
            (s, json_of(&b)),
            (StatusCode::NOT_FOUND, json!({"error":"not_found"})),
            "{uri}"
        );
    }
    let (s, _, b) = send(
        &a,
        Request::post("/v1/maps/config")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, json_of(&b)),
        (
            StatusCode::METHOD_NOT_ALLOWED,
            json!({"error":"method_not_allowed"})
        )
    );
}

#[tokio::test]
async fn geocode_validation_and_down() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(AppState::new(config(tmp.path(), "http://127.0.0.1:1")));
    for uri in [
        "/v1/maps/geocode",
        "/v1/maps/geocode?q=a",
        "/v1/maps/geocode?q=%20%20a%20",
    ] {
        assert_eq!(
            send(&a, get_req(uri)).await.0,
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
    let long = "x".repeat(201);
    assert_eq!(
        send(&a, get_req(&format!("/v1/maps/geocode?q={long}")))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for l in ["0", "6", "x", "-1"] {
        assert_eq!(
            send(&a, get_req(&format!("/v1/maps/geocode?q=aarhus&limit={l}")))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (s, _, b) = send(&a, get_req("/v1/maps/geocode?q=aarhus")).await;
    assert_eq!(
        (s, json_of(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"provider_unavailable"})
        )
    );
    let ok = "x".repeat(200);
    assert_eq!(
        send(&a, get_req(&format!("/v1/maps/geocode?q={ok}")))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

type Seen = Arc<Mutex<Vec<(axum::http::HeaderMap, Value)>>>;

async fn fake_google(
    seen: Seen,
    hits: Arc<AtomicUsize>,
    fail: &'static str,
    slow: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let route = post(
        move |uri: axum::http::Uri, headers: axum::http::HeaderMap, Json(body): Json<Value>| {
            let seen = seen.clone();
            let hits = hits.clone();
            async move {
                assert!(uri.query().is_none());
                assert!(!body.to_string().contains("stub-google-secret"));
                hits.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["x-goog-api-key"], "stub-google-secret");
                seen.lock().unwrap().push((headers, body.clone()));
                if slow {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                if body["travelMode"] == fail || fail == "all" {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error":"stub-google-secret"})),
                    );
                }
                if body.get("textQuery").is_some() {
                    return (
                        StatusCode::OK,
                        Json(
                            json!({"places":[{"displayName":{"text":"Folkehuset"},"formattedAddress":"Møllestien, Aarhus","location":{"latitude":56.1611,"longitude":10.2006}},{"location":{"latitude":0,"longitude":0}}]}),
                        ),
                    );
                }
                (
                    StatusCode::OK,
                    Json(
                        json!({"routes":[{"duration":"540s","staticDuration":"480s","distanceMeters":4200,"polyline":{"encodedPolyline":"_p~iF~ps|U_ulLnnqC_mqNvxq`@"},"legs":[{"steps":[{"transitDetails":{"transitLine":{"nameShort":"2A","name":"Bus 2A","vehicle":{"type":"BUS"}},"stopDetails":{"departureStop":{"name":"Home stop"},"arrivalStop":{"name":"City stop"},"departureTime":"2099-10-27T16:40:00Z","arrivalTime":"2099-10-27T16:49:00Z"}}}]}]}]}),
                    ),
                )
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/", route))
            .await
            .unwrap();
    });
    (format!("http://{addr}/"), task)
}
fn keyed(dir: &Path, url: &str) -> Config {
    let mut c = config(dir, url);
    c.google_key = secretary_maps::provider::Secret::new("stub-google-secret");
    c
}
const ROUTE: &str = "/v1/maps/routes?from=56.19633,10.2011&to=56.1611,10.2006";

#[tokio::test]
async fn google_happy_cache_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let seen: Seen = Arc::default();
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, task) = fake_google(seen.clone(), hits.clone(), "none", false).await;
    let c = keyed(tmp.path(), &url);
    let a = app(AppState::new(c.clone()));
    let (_, _, b) = send(&a, get_req("/v1/maps/config")).await;
    assert_eq!(json_of(&b)["geocoder"], true);
    assert_eq!(json_of(&b)["routes"], true);
    let (s, _, b) = send(
        &a,
        get_req("/v1/maps/geocode?q=%20Folkehuset%20%20Aarhus%20"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        json_of(&b),
        json!({"results":[{"name":"Folkehuset","address":"Møllestien, Aarhus","lat":56.1611,"lng":10.2006}]})
    );
    send(&a, get_req("/v1/maps/geocode?q=folkehuset%20aarhus")).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let a2 = app(AppState::new(c));
    assert_eq!(
        send(&a2, get_req("/v1/maps/geocode?q=folkehuset%20aarhus"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    {
        let seen = seen.lock().unwrap();
        let (h, b) = &seen[0];
        assert_eq!(
            h["x-goog-fieldmask"],
            "places.displayName,places.formattedAddress,places.location"
        );
        assert_eq!(b["languageCode"], "da");
        assert_eq!(b["pageSize"], 3);
        assert_eq!(b["locationBias"]["circle"]["radius"], 50000.0);
        assert_eq!(b["locationBias"]["circle"]["center"]["latitude"], 56.19633);
    }
    let uri = format!("{ROUTE}&arrive=2099-10-27T17:00:00Z");
    let (s, _, b) = send(&a, get_req(&uri)).await;
    assert_eq!(s, StatusCode::OK);
    let v = json_of(&b);
    assert_eq!(v["routes"].as_array().unwrap().len(), 4);
    assert_eq!(v["failed"], json!([]));
    assert_eq!(v["routes"][0]["leaveAt"], "2099-10-27T16:46:00Z");
    assert_eq!(v["routes"][3]["transit"][0]["line"], "2A");
    assert_eq!(v["routes"][0]["polyline"][0], json!([38.5, -120.2]));
    assert!(!String::from_utf8(b).unwrap().contains("stub-google-secret"));
    assert_eq!(hits.load(Ordering::SeqCst), 6); // Places + four modes + drive second pass
    let (s, _, b) = send(&a, get_req(&format!("{ROUTE}&arrive=2099-10-27T17:00:01Z"))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    assert_eq!(json_of(&b)["routes"][0]["leaveAt"], "2099-10-27T16:46:01Z");
    let seen = seen.lock().unwrap();
    let drives: Vec<_> = seen
        .iter()
        .filter(|(_, b)| b["travelMode"] == "DRIVE")
        .collect();
    assert_eq!(drives.len(), 2);
    assert_eq!(drives[1].1["departureTime"], "2099-10-27T16:52:00Z");
    assert_eq!(drives[1].1["routingPreference"], "TRAFFIC_AWARE");
    let transit = seen
        .iter()
        .find(|(_, b)| b["travelMode"] == "TRANSIT")
        .unwrap();
    assert_eq!(transit.1["arrivalTime"], "2099-10-27T17:00:00Z");
    assert!(transit.1.get("departureTime").is_none());
    task.abort();
}

#[tokio::test]
async fn partial_failure_validation_and_no_key() {
    let tmp = tempfile::tempdir().unwrap();
    let seen: Seen = Arc::default();
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, task) = fake_google(seen, hits.clone(), "BICYCLE", false).await;
    let a = app(AppState::new(keyed(tmp.path(), &url)));
    let (s, _, b) = send(&a, get_req(ROUTE)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json_of(&b)["failed"], json!(["bike"]));
    assert_eq!(json_of(&b)["routes"].as_array().unwrap().len(), 3);
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    for uri in [
        "/v1/maps/routes",
        "/v1/maps/routes?from=0,0&to=1,2",
        "/v1/maps/routes?from=NaN,2&to=1,2",
    ] {
        assert_eq!(send(&a, get_req(uri)).await.0, StatusCode::BAD_REQUEST);
    }
    for suffix in [
        "&modes=",
        "&modes=car",
        "&arrive=tomorrow",
        "&modes=drive,,walk",
    ] {
        assert_eq!(
            send(&a, get_req(&format!("{ROUTE}{suffix}"))).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let a = app(AppState::new(config(tmp.path(), &url)));
    for uri in [ROUTE, "/v1/maps/geocode?q=aarhus"] {
        let (s, _, b) = send(&a, get_req(uri)).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json_of(&b), json!({"error":"provider_unavailable"}));
    }
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    task.abort();
}

#[derive(Clone)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogCapture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
#[tokio::test]
async fn errors_timeouts_and_secret_redaction() {
    let logs = LogCapture(Arc::default());
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    for slow in [false, true] {
        let (url, task) =
            fake_google(Arc::default(), Arc::new(AtomicUsize::new(0)), "all", slow).await;
        let c = keyed(tmp.path(), &url);
        assert!(!format!("{c:?}").contains("stub-google-secret"));
        let a = app(AppState::new(c));
        let start = std::time::Instant::now();
        // Route calls must run concurrently, including timeouts.
        let (s, _, b) = send(&a, get_req(ROUTE)).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert!(start.elapsed() < std::time::Duration::from_millis(5500));
        assert_eq!(json_of(&b), json!({"error":"provider_unavailable"}));
        let (s, _, b) = send(&a, get_req("/v1/maps/geocode?q=private-address")).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!String::from_utf8(b).unwrap().contains("stub-google-secret"));
        task.abort();
    }
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("stub-google-secret"));
    assert!(!text.contains("private-address"));
    assert!(!text.contains("56.19633"));
    assert!(text.contains("geocode unavailable"));
}

#[test]
fn config_from_env() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("key");
    std::fs::write(&key, "  stub-google-secret\n").unwrap();
    let env = HashMap::from([
        ("HOME", tmp.path().display().to_string()),
        ("MAPS_GOOGLE_KEY_FILE", key.display().to_string()),
    ]);
    let c = Config::from_env(|k| env.get(k).cloned()).unwrap();
    assert!(c.google_key.is_some());
    assert!(!format!("{c:?}").contains("stub-google-secret"));
    assert_eq!(
        c.places_url,
        "https://places.googleapis.com/v1/places:searchText"
    );
    assert_eq!(c.state_dir, tmp.path().join(".local/state/secretary-maps"));
    std::fs::remove_file(key).unwrap();
    let c = Config::from_env(|k| env.get(k).cloned()).unwrap();
    assert!(c.google_key.is_none());
}

#[test]
fn env_home_validation_and_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("upstream");
    std::fs::write(&key, " 0123456789abcdef-upstream ").unwrap();
    let mut env = HashMap::from([
        ("HOME", tmp.path().display().to_string()),
        ("MAPS_HOME_LAT", "56.19633".into()),
        ("MAPS_HOME_LNG", "10.2011".into()),
        ("MAPS_HOME_LABEL", "Skejby".into()),
        ("MAPS_TILES", "dk".into()),
        ("MAPS_GOOGLE_PLACES_URL", "http://localhost/places".into()),
        ("MAPS_GOOGLE_ROUTES_URL", "http://localhost/routes".into()),
        (
            "XDG_STATE_HOME",
            tmp.path().join("custom-state").display().to_string(),
        ),
        ("MAPS_UPSTREAM_KEY_FILE", key.display().to_string()),
    ]);
    let c = Config::from_env(|k| env.get(k).cloned()).unwrap();
    assert_eq!(c.home.unwrap().label, "Skejby");
    assert_eq!(c.tiles, "dk");
    assert_eq!(c.places_url, "http://localhost/places");
    assert_eq!(c.routes_url, "http://localhost/routes");
    assert_eq!(c.state_dir, tmp.path().join("custom-state/secretary-maps"));
    assert_eq!(c.upstream_key.as_deref(), Some("0123456789abcdef-upstream"));
    for (a, b) in [
        ("56", ""),
        ("x", "10"),
        ("0", "0"),
        ("91", "10"),
        ("NaN", "10"),
    ] {
        env.insert("MAPS_HOME_LAT", a.into());
        env.insert("MAPS_HOME_LNG", b.into());
        assert!(Config::from_env(|k| env.get(k).cloned())
            .unwrap()
            .home
            .is_none());
    }
    env.insert("MAPS_GOOGLE_PLACES_URL", "http://127.0.0.1:1234".into());
    env.insert("MAPS_GOOGLE_ROUTES_URL", "http://127.0.0.1:1235/".into());
    let c = Config::from_env(|k| env.get(k).cloned()).unwrap();
    assert_eq!(c.places_url, "http://127.0.0.1:1234/v1/places:searchText");
    assert_eq!(
        c.routes_url,
        "http://127.0.0.1:1235/directions/v2:computeRoutes"
    );
    env.insert("MAPS_TILES", "../x".into());
    assert!(Config::from_env(|k| env.get(k).cloned()).is_err());
}
