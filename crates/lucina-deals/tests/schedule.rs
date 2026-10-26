//! The deal watch scheduler and its app delivery (`startDealWatch` `tick` and
//! `notifyApp` on Node), against a mock secretary-server `/v1/notify` and
//! the recorded fixtures.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use lucina_deals::js::parse_date;
use lucina_deals::{
    AppNotify, DealWatch, DealsConfig, FixedList, Scheduler, WatchConfig, WatchState,
};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Copenhagen is UTC+2 on 2026-10-24.
fn local(hhmm: &str) -> i64 {
    parse_date(&format!("2026-10-24T{hhmm}:00+02:00")).unwrap()
}

struct Rig {
    scheduler: Scheduler,
    clock: Arc<AtomicI64>,
    dir: tempfile::TempDir,
}

impl Rig {
    fn state_path(&self) -> std::path::PathBuf {
        self.dir.path().join("grocery/deal-watch.json")
    }

    async fn at(&self, hhmm: &str) -> lucina_deals::schedule::TickReport {
        self.clock.store(local(hhmm), Ordering::SeqCst);
        self.scheduler.tick().await.unwrap()
    }
}

fn rig_with(server: Option<&MockServer>, enabled: bool, state: Option<&str>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("service.token");
    std::fs::write(&token, "svc-token\n").unwrap();
    let config = WatchConfig::from_lookup(&|_: &str| None, dir.path());
    if let Some(text) = state {
        std::fs::create_dir_all(dir.path().join("grocery")).unwrap();
        std::fs::write(&config.state_path, text).unwrap();
    }
    let engine = engine_with(
        DealsConfig {
            home_label: Some("home".into()),
            ..common::config()
        },
        Arc::new(FixedList::needed(&ITEMS)),
        fixture_http(),
    );
    let mut notify = AppNotify::new(enabled, server.map(MockServer::uri), Some(token));
    notify.retry_delay = Duration::from_millis(10);
    let clock = Arc::new(AtomicI64::new(local("07:45")));
    let c = clock.clone();
    let watch = DealWatch::new(
        config.watch_settings(),
        Arc::new(engine),
        Arc::new(notify),
        "owner",
    )
    .with_clock(Arc::new(move || c.load(Ordering::SeqCst)));
    Rig {
        scheduler: Scheduler::new(Arc::new(watch), config),
        clock,
        dir,
    }
}

async fn notify_mock(server: &MockServer, status: u16) {
    Mock::given(method("POST"))
        .and(path("/v1/notify"))
        .and(header("authorization", "Bearer svc-token"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"status": "created"})))
        .mount(server)
        .await;
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn state(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn assert_dedup(key: &str, kind: &str) {
    let prefix = format!("grocery-deals:{kind}:2026-10-24:");
    let hash = key.strip_prefix(&prefix).unwrap_or_else(|| panic!("{key}"));
    assert_eq!(hash.len(), 24, "{key}");
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{key}");
}

#[tokio::test]
async fn the_digest_is_posted_once_per_local_day() {
    let server = MockServer::start().await;
    notify_mock(&server, 200).await;
    let rig = rig_with(Some(&server), true, None);

    let r = rig.at("07:29").await;
    assert_eq!((r.digest, r.alerts), (None, None));

    let r = rig.at("07:45").await;
    let digest = r.digest.unwrap();
    assert!(digest.sent, "{digest:?}");
    assert_eq!(r.alerts, None);
    // Node's exact text (the golden digest) and notification fields.
    let golden = golden("digest.txt");
    assert_eq!(format!("{}\n", digest.text), golden);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let (title, body) = golden.trim_end_matches('\n').split_once('\n').unwrap();
    let key = sent[0]["dedupKey"].as_str().unwrap().to_owned();
    assert_dedup(&key, "digest");
    assert_eq!(
        sent[0],
        json!({"category": "shopping", "title": title, "body": body, "route": "/shopping/map", "channel": "lucina", "dedupKey": key})
    );
    let saved = state(&rig.state_path());
    assert_eq!(saved["lastDigestDate"], "2026-10-24");
    assert!(saved["digestFingerprint"].is_string());
    assert!(!saved["notified"].as_object().unwrap().is_empty());

    let r = rig.at("08:00").await;
    assert_eq!(r.digest.unwrap().reason, "already digested today");
    // The first alert check of the day: everything is already notified.
    let r = rig.at("09:30").await;
    assert_eq!(r.digest.unwrap().reason, "already digested today");
    assert_eq!(r.alerts.unwrap().reason, "no new or cheaper matches");
    // From 11:00 there is no digest check; 90 minutes on, the next alert check.
    let r = rig.at("10:59").await;
    assert_eq!(r.alerts, None);
    let r = rig.at("11:00").await;
    assert_eq!(r.digest, None);
    assert_eq!(r.alerts.unwrap().reason, "no new or cheaper matches");
    assert_eq!(bodies(&server).await.len(), 1);
}

#[tokio::test]
async fn alerts_keep_the_interval_the_window_and_the_daily_cap() {
    let server = MockServer::start().await;
    notify_mock(&server, 200).await;
    let rig = rig_with(Some(&server), true, None);

    let r = rig.at("12:00").await;
    assert_eq!(r.digest, None);
    let alerts = r.alerts.unwrap();
    assert!(alerts.sent, "{alerts:?}");
    assert_eq!(format!("{}\n", alerts.text), golden("alerts.txt"));
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    assert_dedup(sent[0]["dedupKey"].as_str().unwrap(), "alert");
    assert_eq!(sent[0]["route"], "/shopping/map");
    assert_eq!(
        state(&rig.state_path())["alertCounts"],
        json!({"2026-10-24": 1})
    );

    // 90 minutes between alert runs.
    assert_eq!(rig.at("12:10").await.alerts, None);
    assert_eq!(rig.at("13:29").await.alerts, None);
    let r = rig.at("13:30").await;
    assert_eq!(r.alerts.unwrap().reason, "no new or cheaper matches");
    // Outside the window.
    assert_eq!(rig.at("20:00").await.alerts, None);
    assert_eq!(bodies(&server).await.len(), 1);

    // The daily cap (state written by Node).
    let capped = rig_with(
        Some(&server),
        true,
        Some(
            r#"{"notified":{},"lastDigestDate":null,"digestFingerprint":null,"alertCounts":{"2026-10-24":4}}"#,
        ),
    );
    assert_eq!(
        capped.at("12:00").await.alerts.unwrap().reason,
        "daily alert cap"
    );
    assert_eq!(bodies(&server).await.len(), 1);
}

#[tokio::test]
async fn a_failed_digest_is_retried_after_the_cooldown() {
    let server = MockServer::start().await;
    notify_mock(&server, 503).await;
    let rig = rig_with(Some(&server), true, None);

    let r = rig.at("07:45").await;
    let digest = r.digest.unwrap();
    assert_eq!(
        (digest.sent, digest.reason.as_str()),
        (false, "send failed")
    );
    // One retry after a 5xx; nothing acknowledged, the digest is pending (F1).
    assert_eq!(bodies(&server).await.len(), 2);
    let saved = state(&rig.state_path());
    assert_eq!(saved["lastDigestDate"], Value::Null);
    assert_eq!(
        saved["pending"]["dedupKey"],
        bodies(&server).await[0]["dedupKey"]
    );

    assert_eq!(
        rig.at("07:50").await.digest.unwrap().reason,
        "digest retry cooldown"
    );
    assert_eq!(bodies(&server).await.len(), 2);
    assert_eq!(rig.at("07:56").await.digest.unwrap().reason, "send failed");
    // The retry re-sends the pending digest unchanged.
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[2], sent[0]);
}

#[tokio::test]
async fn app_delivery_needs_the_flag_the_url_and_the_token() {
    let server = MockServer::start().await;
    notify_mock(&server, 400).await;
    // A 4xx is not retried.
    let rig = rig_with(Some(&server), true, None);
    assert_eq!(rig.at("12:00").await.alerts.unwrap().reason, "send failed");
    assert_eq!(bodies(&server).await.len(), 1);
    // F1: a failed send stays pending (re-sent by the next run); nothing acknowledged.
    let saved = state(&rig.state_path());
    assert_eq!(saved["pending"]["kind"], "alert");
    assert_eq!(saved["alertCounts"], json!({}));

    // LUCINA_APP_NOTIFY off, or no server URL: never posted, nothing recorded.
    let off = rig_with(Some(&server), false, None);
    let d = off.at("07:45").await.digest.unwrap();
    assert_eq!(
        (d.sent, d.text.as_str(), d.reason.as_str()),
        (false, "", "app notifications not configured")
    );
    let no_url = rig_with(None, true, None);
    assert_eq!(
        no_url.at("12:00").await.alerts.unwrap().reason,
        "app notifications not configured"
    );
    assert_eq!(bodies(&server).await.len(), 1);
    assert!(!off.state_path().exists());
}

#[tokio::test]
async fn a_node_state_file_is_kept_and_continued() {
    // Node already digested today: the switch-over neither re-notifies nor skips alerts.
    let server = MockServer::start().await;
    notify_mock(&server, 200).await;
    let node_state = "{\n  \"notified\": {},\n  \"lastDigestDate\": \"2026-10-24\",\n  \"digestFingerprint\": null,\n  \"alertCounts\": {\n    \"2026-10-24\": 1\n  }\n}";
    let rig = rig_with(Some(&server), true, Some(node_state));
    let r = rig.at("09:15").await;
    assert_eq!(r.digest.unwrap().reason, "already digested today");
    assert!(r.alerts.unwrap().sent);
    let saved = WatchState::load(&rig.state_path()).unwrap();
    assert_eq!(saved.alert_count("2026-10-24"), 2.0);
    assert_eq!(saved.last_digest_date(), Some("2026-10-24"));
}

#[test]
fn settings_read_nodes_variables_and_defaults() {
    let dir = Path::new("/srv/data");
    let none = |_: &str| None::<String>;
    let c = WatchConfig::from_lookup(&none, dir);
    assert!(!c.enabled);
    assert_eq!(c.channel, "app");
    assert_eq!(c.state_path, dir.join("grocery/deal-watch.json"));
    assert_eq!(c.zone, chrono_tz::Europe::Copenhagen);
    assert_eq!(
        c.describe(),
        "Grocery deal watch: digest 07:30 Europe/Copenhagen; alerts every 90 min, 09:00-20:00"
    );
    let env = |k: &str| {
        match k {
            "GROCERY_OFFER_WATCH_ENABLED" => Some("true"),
            "GROCERY_DEALS_DIGEST_TIME" => Some(" 08:00 "),
            "GROCERY_DEALS_ALERT_INTERVAL_MIN" => Some("60"),
            "GROCERY_DEALS_ALERT_WINDOW" => Some("10:00-18:00"),
            "GROCERY_DEALS_ALERT_MAX_PER_DAY" => Some("2"),
            "GROCERY_DEALS_STATE_PATH" => Some("/x/watch.json"),
            "LUCINA_REMINDER_TIMEZONE" => Some("Europe/Rome"),
            "GROCERY_HOME_LAT" => Some("56.19633"),
            "GROCERY_HOME_LNG" => Some("0"),
            "GROCERY_NEARBY_RADIUS_M" => Some("2500"),
            "SALLING_API_BASE_URL" => Some("https://salling.test//"),
            _ => None,
        }
        .map(str::to_owned)
    };
    let c = WatchConfig::from_lookup(&env, dir);
    assert!(c.enabled);
    assert_eq!(
        (
            c.digest_time.as_str(),
            c.alert_interval_min,
            c.alert_max_per_day
        ),
        ("08:00", 60.0, 2.0)
    );
    assert_eq!(c.state_path, Path::new("/x/watch.json"));
    assert_eq!(c.zone, chrono_tz::Europe::Rome);
    let d = DealsConfig::from_lookup(&env, dir);
    assert_eq!((d.home_lat, d.home_lng), (Some(56.19633), None));
    assert!(!d.has_home());
    assert_eq!(d.radius_m, 2500.0);
    assert_eq!(d.salling_base_url, "https://salling.test");
    assert_eq!(d.home_label.as_deref(), Some("home"));
    assert_eq!(d.synonyms_path, dir.join("grocery/deal-synonyms.json"));
    let d = DealsConfig::from_lookup(&none, dir);
    assert_eq!(
        (d.radius_m, d.lunch_budget_dkk, d.request_timeout_ms),
        (3000.0, 50.0, 8000)
    );
    assert_eq!(d.salling_api_key, None);
}
