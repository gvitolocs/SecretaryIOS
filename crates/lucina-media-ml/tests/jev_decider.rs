//! The JEV decider against a mock JEV server (wiremock): the state it sends (labels and booleans,
//! no names or places), the category and confidence it reads back, outages that leave the photo
//! pending and close `ready()`, and malformed answers that fail only that photo.

use std::collections::BTreeMap;
use std::time::Duration;

use lucina_jev::{Config, Jev};
use lucina_media_ml::categories::{
    Category, Decider, DeciderKind, Features, ImageFormat, MediaKind, NameHint, SourceKind,
    FEATURES_VERSION,
};
use lucina_media_ml::categories_jev::{state_of, JevDecider, QUESTION_VERSION};
use lucina_media_ml::MlError;
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn client(url: &str) -> &'static Jev {
    Box::leak(Box::new(Jev::new(Config {
        url: format!("{url}/v1/systemone"),
        model: "jev-test".into(),
        key: Some("test-key".into()),
        timeout: Duration::from_secs(2),
        enabled: true,
    })))
}

/// A receipt-looking PNG from a chat, no camera data, with a position.
fn features() -> Features {
    let mut zero_shot = BTreeMap::new();
    for c in Category::ALL {
        zero_shot.insert(c, 0.01);
    }
    zero_shot.insert(Category::Receipt, 0.62);
    zero_shot.insert(Category::Document, 0.25);
    zero_shot.insert(Category::Photo, 0.05);
    Features {
        version: FEATURES_VERSION,
        kind: MediaKind::Photo,
        zero_shot,
        top_category: Category::Receipt,
        top_cosine: 0.131,
        width: 900,
        height: 1600,
        aspect: 0.5625,
        screen_sized: false,
        camera_exif: false,
        format: ImageFormat::Png,
        source_kind: SourceKind::Chat,
        name_hint: NameHint::Chat,
        has_gps: true,
    }
}

fn answer(choice: &str, confidence: f64) -> Value {
    let mut probabilities = serde_json::Map::new();
    let rest = (1.0 - confidence) / (Category::ALL.len() - 1) as f64;
    for c in Category::ALL {
        let p = if c.as_str() == choice {
            confidence
        } else {
            rest
        };
        probabilities.insert(c.as_str().into(), json!(p));
    }
    json!({"model": "jev-test", "answers": {"category": {
        "type": "choice", "choice": choice, "confidence": confidence, "probabilities": probabilities}},
        "usage": {"input_tokens": 420, "output_tokens": 12}})
}

#[test]
fn the_state_is_labels_and_booleans_only() {
    let s = state_of(&features());
    assert_eq!(
        s["looks_most_like"],
        json!(["receipt", "document", "photo"])
    );
    assert_eq!(s["likeness"]["receipt"], "strong");
    assert_eq!(s["likeness"]["document"], "some");
    assert_eq!(s["likeness"]["photo"], "weak");
    assert_eq!(s["file_format"], "png");
    assert_eq!(s["came_from"], "received in a chat or shared");
    assert_eq!(s["file_name_suggests"], "chat app");
    assert_eq!(s["has_location"], true);
    assert_eq!(s["portrait"], true);
    // No raw numbers, no names, no coordinates.
    let text = s.to_string();
    for banned in ["0.62", "0.131", "900", "1600", "lat", "lon"] {
        assert!(!text.contains(banned), "{banned} in {text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn decides_from_the_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer("receipt", 0.93)))
        .expect(1)
        .mount(&server)
        .await;
    let d = JevDecider::new(client(&server.uri()));
    assert_eq!(d.kind(), DeciderKind::Jev);
    assert_eq!(d.id(), format!("jev:jev-test:q{QUESTION_VERSION}"));
    assert!(d.ready());
    let f = features();
    // From a blocking task, as the HTTP route and the embedding thread call it.
    let out = tokio::task::spawn_blocking(move || d.decide(&f))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.category, Category::Receipt);
    assert!((out.confidence - 0.93).abs() < 1e-6);
    let sent: Vec<Request> = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["model"], "jev-test");
    assert_eq!(body["state"]["looks_most_like"][0], "receipt");
    let options = body["questions"]["category"].to_string();
    for c in Category::ALL {
        assert!(
            options.contains(c.as_str()),
            "{} missing in {options}",
            c.as_str()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quota_answer_is_an_outage_and_closes_ready() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
        .expect(1)
        .mount(&server)
        .await;
    let d = std::sync::Arc::new(JevDecider::new(client(&server.uri())));
    let d2 = d.clone();
    let err = tokio::task::spawn_blocking(move || d2.decide(&features()))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, MlError::Unavailable(_)), "{err:?}");
    assert!(!d.ready());
    // Cooling down: no second request reaches the server.
    let d3 = d.clone();
    let err = tokio::task::spawn_blocking(move || d3.decide(&features()))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, MlError::Unavailable(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_answer_fails_only_this_photo() {
    let server = MockServer::start().await;
    let mut bad = answer("receipt", 0.9);
    bad["answers"]["category"]["choice"] = json!("selfie");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bad))
        .mount(&server)
        .await;
    let d = std::sync::Arc::new(JevDecider::new(client(&server.uri())));
    let d2 = d.clone();
    let err = tokio::task::spawn_blocking(move || d2.decide(&features()))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, MlError::Failed(_)), "{err:?}");
    assert!(d.ready());
}

#[test]
fn no_key_means_no_jev_decider() {
    // The tests never set JEV_API_KEY; `from_env` reads the process environment once.
    if std::env::var("JEV_API_KEY").is_err() {
        assert!(JevDecider::from_env().is_none());
    }
}

/// The real JEV API on synthetic records (no photo, no owner data). Needs `JEV_API_KEY`:
/// `cargo test -p lucina-media-ml --test jev_decider -- --ignored --nocapture`.
#[test]
#[ignore]
fn real_jev_picks_plausible_categories() {
    let d = JevDecider::from_env().expect("JEV_API_KEY is not set");
    let mut receipt = features();
    receipt.has_gps = false;
    let mut shot = features();
    shot.zero_shot.insert(Category::Screenshot, 0.7);
    shot.zero_shot.insert(Category::Receipt, 0.1);
    shot.screen_sized = true;
    shot.source_kind = SourceKind::PhoneBackup;
    shot.name_hint = NameHint::Screenshot;
    let mut food = features();
    food.zero_shot.insert(Category::Food, 0.8);
    food.zero_shot.insert(Category::Receipt, 0.02);
    food.format = ImageFormat::Heic;
    food.camera_exif = true;
    food.source_kind = SourceKind::PhoneBackup;
    food.name_hint = NameHint::Camera;
    for (f, want) in [
        (receipt, Category::Receipt),
        (shot, Category::Screenshot),
        (food, Category::Food),
    ] {
        let t0 = std::time::Instant::now();
        let out = d.decide(&f).unwrap();
        eprintln!(
            "{:?} -> {:?} ({:.2}) in {:?}",
            want,
            out.category,
            out.confidence,
            t0.elapsed()
        );
        assert_eq!(out.category, want);
    }
}
