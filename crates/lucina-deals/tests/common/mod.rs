//! Shared fixture setup: the Node test's config, items and fixed `now`.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use lucina_deals::js::parse_date;
use lucina_deals::{
    fixed_clock, DealsConfig, DealsEngine, DealsHttp, FixedList, FixtureHttp, ShoppingSource,
};
use serde_json::Value;

pub const ITEMS: [&str; 5] = [
    "Prosciutto crudo",
    "Latte",
    "Prosciutto cotto",
    "Pane di segale",
    "Yogurt",
];

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn fixture_dir() -> PathBuf {
    crate_dir().join("fixtures/grocery-deals")
}

pub fn golden(name: &str) -> String {
    std::fs::read_to_string(crate_dir().join("fixtures/golden").join(name)).expect(name)
}

pub fn golden_json(name: &str) -> Value {
    serde_json::from_str(&golden(name)).expect(name)
}

pub fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(name)).expect(name))
        .expect(name)
}

/// `2026-10-24T09:30:00Z`, the fixtures' recording time.
pub fn fixed() -> i64 {
    parse_date("2026-10-24T09:30:00Z").unwrap()
}

/// The Node test's `config` (no home label; key and base are placeholders).
pub fn config() -> DealsConfig {
    DealsConfig {
        home_lat: Some(56.19633),
        home_lng: Some(10.20110),
        salling_api_key: Some("fixture".into()),
        salling_base_url: "https://example.test".into(),
        radius_m: 3000.0,
        lunch_budget_dkk: 50.0,
        synonyms_path: PathBuf::from("/nonexistent/deal-synonyms.json"),
        ..DealsConfig::default()
    }
}

pub fn fixture_http() -> Arc<dyn DealsHttp> {
    Arc::new(FixtureHttp::new(fixture_dir()))
}

pub fn engine_with(
    config: DealsConfig,
    shopping: Arc<dyn ShoppingSource>,
    http: Arc<dyn DealsHttp>,
) -> DealsEngine {
    DealsEngine::new(config, shopping, http).with_clock(fixed_clock(fixed()))
}

/// `engineFor()`: the five items, fixtures, fixed clock.
pub fn engine() -> DealsEngine {
    engine_with(
        config(),
        Arc::new(FixedList::needed(&ITEMS)),
        fixture_http(),
    )
}

/// Numbers compared as f64 (`20` and `20.0` are the same JS number).
pub fn normalized(v: &Value) -> Value {
    match v {
        Value::Number(n) => serde_json::Number::from_f64(n.as_f64().unwrap())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Value::Array(a) => Value::Array(a.iter().map(normalized).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), normalized(v))).collect())
        }
        other => other.clone(),
    }
}

/// The first path where two JSON values differ.
pub fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => first_difference(p, q, &format!("{path}.{k}")),
                (p, q) => Some(format!("{path}.{k}: {p:?} vs {q:?}")),
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: length {} vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_difference(p, q, &format!("{path}[{i}]")))
        }
        _ if normalized(a) == normalized(b) => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

#[track_caller]
pub fn assert_json_eq(actual: &Value, expected: &Value, what: &str) {
    if let Some(diff) = first_difference(actual, expected, what) {
        panic!("{what} differs from Node (actual vs node): {diff}");
    }
}

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Mutex;

use async_trait::async_trait;
use lucina_deals::sources::DealStore;
use lucina_deals::watch::{DealWatch, Deliver, Delivery, MatchSource, Notification, WatchSettings};
use lucina_deals::{Deal, DealSource, DealsError, MatchItem, MatchResult, Status};

/// A settable clock (`clock += …` in the Node tests).
#[derive(Clone)]
pub struct TestClock(pub Arc<AtomicI64>);

impl TestClock {
    pub fn new(ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(ms)))
    }
    pub fn set(&self, ms: i64) {
        self.0.store(ms, AtomicOrdering::SeqCst);
    }
    pub fn get(&self) -> i64 {
        self.0.load(AtomicOrdering::SeqCst)
    }
    pub fn clock(&self) -> lucina_deals::Clock {
        let t = self.0.clone();
        Arc::new(move || t.load(AtomicOrdering::SeqCst))
    }
}

/// A deal with the Node tests' object-literal defaults.
pub fn deal(key: &str, source: DealSource, name: &str, price: f64) -> Deal {
    Deal {
        key: key.into(),
        source,
        name: Some(Value::String(name.into())),
        chain: Some(Value::String("Netto".into())),
        chain_key: "netto".into(),
        dealer_id: None,
        store: DealStore {
            id: None,
            name: Some(Value::String("Netto Branch".into())),
            street: Some(Value::String("Street 1".into())),
            lat: None,
            lng: None,
            distance_m: 100.0,
        },
        price_dkk: price,
        normal_price_dkk: None,
        discount_pct: None,
        stock: None,
        stock_unit: None,
        valid_from: Some(Value::String("2026-10-23T00:00:00Z".into())),
        valid_to: Some(Value::String("2026-10-26T22:00:00Z".into())),
        ean: None,
        category: None,
        unit_price: None,
        status: Status::Active,
        score: 0,
        related_keys: None,
        more_stores: None,
    }
}

/// `notificationItem(index)`: a best offer and a dearer alternative.
pub fn notification_item(index: usize) -> MatchItem {
    let best = deal(
        &format!("tjek:{index}:best"),
        DealSource::Flyer,
        &format!("Offer {index}"),
        5.0,
    );
    let second = Deal {
        key: format!("tjek:{index}:second"),
        name: Some(Value::String(format!("Alternative {index}"))),
        price_dkk: 7.0,
        ..best.clone()
    };
    MatchItem {
        name: format!("Item {index}"),
        rule_id: format!("item_{index}"),
        deals: vec![best, second],
    }
}

pub fn result_of(items: Vec<MatchItem>) -> MatchResult {
    MatchResult {
        items,
        failures: Vec::new(),
        now: lucina_deals::js::iso_string(fixed()),
    }
}

/// `engine.listMatches` returning a settable result, counting calls.
pub struct StubSource {
    pub result: Mutex<MatchResult>,
    pub calls: AtomicUsize,
}

#[async_trait]
impl MatchSource for StubSource {
    async fn list_matches(&self, _user_id: &str) -> Result<MatchResult, DealsError> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        Ok(self.result.lock().unwrap().clone())
    }
}

/// A delivery that records every attempt and can fail or be unconfigured.
#[derive(Default)]
pub struct StubDeliver {
    pub fail: AtomicBool,
    pub unconfigured: AtomicBool,
    pub attempts: Mutex<Vec<Notification>>,
    pub delivered: Mutex<Vec<Notification>>,
}

#[async_trait]
impl Deliver for StubDeliver {
    async fn deliver(&self, n: &Notification) -> Delivery {
        self.attempts.lock().unwrap().push(n.clone());
        if self.unconfigured.load(AtomicOrdering::SeqCst) {
            return Delivery::NotConfigured;
        }
        if self.fail.load(AtomicOrdering::SeqCst) {
            return Delivery::Failed {
                reason: "send failed".into(),
            };
        }
        self.delivered.lock().unwrap().push(n.clone());
        Delivery::Delivered
    }
}

/// `watchHarness`: a private state file, a stub engine and delivery.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub file: PathBuf,
    pub clock: TestClock,
    pub source: Arc<StubSource>,
    pub deliver: Arc<StubDeliver>,
    pub settings: WatchSettings,
    pub watch: DealWatch,
}

impl Harness {
    pub fn new(result: MatchResult) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let settings = WatchSettings {
            state_path: file.clone(),
            home_label: Some("home".into()),
            ..WatchSettings::default()
        };
        let clock = TestClock::new(fixed());
        let source = Arc::new(StubSource {
            result: Mutex::new(result),
            calls: AtomicUsize::new(0),
        });
        let deliver = Arc::new(StubDeliver::default());
        let watch = DealWatch::new(settings.clone(), source.clone(), deliver.clone(), "owner")
            .with_clock(clock.clock());
        Self {
            dir,
            file,
            clock,
            source,
            deliver,
            settings,
            watch,
        }
    }

    /// With the fixture engine's list matches.
    pub async fn with_fixture_result() -> Self {
        Self::new(engine().list_matches("owner").await.unwrap())
    }

    /// A fresh watcher on the same state (a restart).
    pub fn restart(&self) -> DealWatch {
        DealWatch::new(
            self.settings.clone(),
            self.source.clone(),
            self.deliver.clone(),
            "owner",
        )
        .with_clock(self.clock.clock())
    }

    pub fn set_result(&self, result: MatchResult) {
        *self.source.result.lock().unwrap() = result;
    }

    pub fn calls(&self) -> usize {
        self.source.calls.load(AtomicOrdering::SeqCst)
    }

    pub fn set_fail(&self, fail: bool) {
        self.deliver.fail.store(fail, AtomicOrdering::SeqCst);
    }

    pub fn state(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.file).unwrap()).unwrap()
    }

    pub fn file_text(&self) -> String {
        std::fs::read_to_string(&self.file).unwrap()
    }
}
