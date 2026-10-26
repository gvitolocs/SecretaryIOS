use axum::{
    body::Body,
    extract::Query,
    http::{header, Request, StatusCode},
    routing::get,
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

fn config(dir: &Path, photon: &str) -> Config {
    Config {
        dir: dir.to_path_buf(),
        tiles: "region".into(),
        home: Some(Home {
            lat_e7: 561_963_300,
            lng_e7: 102_011_000,
            label: "Home".into(),
        }),
        // Port 1: nothing listens, connections are refused at once.
        photon_url: photon.into(),
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
            "home":{"lat":56.19633,"lng":10.2011,"label":"Home"},"geocoder":false,
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
            json!({"error":"geocoder_unavailable"})
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

type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;

async fn fake_photon(seen: Seen, hits: Arc<AtomicUsize>) -> String {
    let route = get(move |Query(q): Query<HashMap<String, String>>| {
        let seen = Arc::clone(&seen);
        let hits = Arc::clone(&hits);
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(q);
            Json(json!({"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[10.2006,56.1611]},
                 "properties":{"name":"Folkehuset","street":"Møllestien","postcode":"8000","city":"Aarhus","country":"Denmark"}},
                {"geometry":{"coordinates":[0,0]},"properties":{"name":"broken"}}
            ]}))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/api", route))
            .await
            .unwrap()
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn geocode_through_photon() {
    let tmp = tempfile::tempdir().unwrap();
    let seen: Seen = Arc::default();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = fake_photon(Arc::clone(&seen), Arc::clone(&hits)).await;
    let a = app(AppState::new(config(tmp.path(), &url)));

    let (s, _, b) = send(
        &a,
        get_req("/v1/maps/geocode?q=%20Folkehuset%20%20Aarhus%20"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        json_of(&b),
        json!({"results":[{"name":"Folkehuset","address":"Møllestien, 8000 Aarhus, Denmark","lat":56.1611,"lng":10.2006}]})
    );
    {
        let seen = seen.lock().unwrap();
        let q = &seen[0];
        assert_eq!(q["q"], "Folkehuset  Aarhus");
        assert_eq!((q["limit"].as_str(), q["lang"].as_str()), ("3", "en"));
        assert_eq!(
            (q["lat"].as_str(), q["lon"].as_str()),
            ("56.19633", "10.2011")
        ); // home
    }
    // Same normalized query and bias: served from the cache.
    let (s, _, _) = send(&a, get_req("/v1/maps/geocode?q=folkehuset%20aarhus")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // A valid bias from the query wins; an invalid one falls back to home.
    send(
        &a,
        get_req("/v1/maps/geocode?q=folkehuset&lat=55.6761&lng=12.5683&limit=5"),
    )
    .await;
    send(&a, get_req("/v1/maps/geocode?q=elsewhere&lat=0&lng=0")).await;
    send(&a, get_req("/v1/maps/geocode?q=nowhere&lat=91&lng=10")).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 4);
    assert_eq!(
        (
            seen[1]["lat"].as_str(),
            seen[1]["lon"].as_str(),
            seen[1]["limit"].as_str()
        ),
        ("55.6761", "12.5683", "5")
    );
    assert_eq!(seen[2]["lat"], "56.19633");
    assert_eq!(seen[3]["lat"], "56.19633");

    // The probe now reports the geocoder up.
    let (_, _, b) = send(&a, get_req("/v1/maps/health")).await;
    assert_eq!(json_of(&b)["geocoder"], json!(true));
}

#[test]
fn config_from_env() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("key");
    std::fs::write(&key, "  0123456789abcdef-secret\n").unwrap();
    let env = |pairs: Vec<(&'static str, String)>| {
        let map: HashMap<&str, String> = pairs.into_iter().collect();
        move |k: &str| map.get(k).cloned()
    };
    let c = Config::from_env(env(vec![("HOME", "/home/x".into())])).unwrap();
    assert_eq!(c.dir, Path::new("/home/x/data/maps"));
    assert_eq!(
        (c.tiles.as_str(), c.photon_url.as_str()),
        ("region", "http://127.0.0.1:2322")
    );
    assert_eq!((c.home, c.upstream_key), (None, None));

    let c = Config::from_env(env(vec![
        ("MAPS_DIR", "/srv/maps".into()),
        ("MAPS_TILES", "dk".into()),
        ("MAPS_HOME_LAT", "56.19633".into()),
        ("MAPS_HOME_LNG", " 10.2011 ".into()),
        ("MAPS_PHOTON_URL", "http://10.0.0.2:2322/".into()),
        ("MAPS_UPSTREAM_KEY_FILE", key.display().to_string()),
    ]))
    .unwrap();
    assert_eq!(
        c.home,
        Some(Home {
            lat_e7: 561_963_300,
            lng_e7: 102_011_000,
            label: "Home".into()
        })
    );
    assert_eq!(c.photon_url, "http://10.0.0.2:2322");
    assert_eq!(c.upstream_key.as_deref(), Some("0123456789abcdef-secret"));

    for (lat, lng) in [
        ("56.2", ""),
        ("x", "10"),
        ("0", "0"),
        ("91", "10"),
        ("NaN", "10"),
    ] {
        let c = Config::from_env(env(vec![
            ("HOME", "/h".into()),
            ("MAPS_HOME_LAT", lat.into()),
            ("MAPS_HOME_LNG", lng.into()),
        ]));
        assert_eq!(c.unwrap().home, None, "{lat},{lng}");
    }
    let c = Config::from_env(env(vec![
        ("HOME", "/h".into()),
        ("MAPS_HOME_LAT", "56".into()),
        ("MAPS_HOME_LNG", "10".into()),
        ("MAPS_HOME_LABEL", "Skejby".into()),
    ]));
    assert_eq!(c.unwrap().home.unwrap().label, "Skejby");
    assert!(Config::from_env(env(vec![
        ("HOME", "/h".into()),
        ("MAPS_TILES", "../x".into())
    ]))
    .is_err());
    std::fs::write(&key, "short").unwrap();
    assert!(Config::from_env(env(vec![
        ("HOME", "/h".into()),
        ("MAPS_UPSTREAM_KEY_FILE", key.display().to_string())
    ]))
    .is_err());
}
