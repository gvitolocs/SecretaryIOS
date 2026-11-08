//! Regression tests for the deal watch bugs DW1-DW3 found by the model
//! (SecretaryIOS `docs/verification.md` §14, `verification/models/src/deals.rs`)
//! and their fixes F1-F3. Each test runs the real watcher against a fake
//! secretary-server `/v1/notify` that keeps every dedup key, as the model's server
//! does, and checks what reached the owner (the server's inbox), not the state file.

mod common;

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use lucina_deals::watch::{DealWatch, Deliver, Delivery, Notification, WatchSettings};
use lucina_deals::{DealSource, MatchItem, MatchResult, WatchState};
use serde_json::Value;

const DAY: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The server stores the notification, the response is lost (`Delivery::Failed`).
    Lost,
    /// The server is down: nothing stored (`Delivery::Failed`).
    Down,
}

/// `POST /v1/notify` with the server's dedup: a known key is answered `duplicate`
/// (200, `Delivered`) without a new notification.
#[derive(Default)]
struct Server {
    inbox: Mutex<Vec<Notification>>,
    keys: Mutex<HashSet<String>>,
    script: Mutex<VecDeque<Fault>>,
    attempts: Mutex<Vec<Notification>>,
}

impl Server {
    fn fail_next(&self, fault: Fault) {
        self.script.lock().unwrap().push_back(fault);
    }

    fn inbox(&self) -> Vec<Notification> {
        self.inbox.lock().unwrap().clone()
    }

    fn attempts(&self) -> Vec<Notification> {
        self.attempts.lock().unwrap().clone()
    }

    /// How many notifications the owner received that show `text`.
    fn received(&self, text: &str) -> usize {
        self.inbox()
            .iter()
            .filter(|n| n.body.contains(text))
            .count()
    }

    fn digests(&self) -> usize {
        self.inbox()
            .iter()
            .filter(|n| n.dedup_key.starts_with("grocery-deals:digest:"))
            .count()
    }
}

#[async_trait]
impl Deliver for Server {
    async fn deliver(&self, n: &Notification) -> Delivery {
        self.attempts.lock().unwrap().push(n.clone());
        let fault = self.script.lock().unwrap().pop_front();
        if fault == Some(Fault::Down) {
            return Delivery::Failed {
                reason: "send failed".into(),
            };
        }
        if self.keys.lock().unwrap().insert(n.dedup_key.clone()) {
            self.inbox.lock().unwrap().push(n.clone());
        }
        match fault {
            Some(Fault::Lost) => Delivery::Failed {
                reason: "send failed".into(),
            },
            Some(Fault::Down) => unreachable!(),
            None => Delivery::Delivered,
        }
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    settings: WatchSettings,
    clock: TestClock,
    source: Arc<StubSource>,
    server: Arc<Server>,
    watch: DealWatch,
}

impl Rig {
    fn new(result: MatchResult) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let settings = WatchSettings {
            state_path: dir.path().join("deal-watch.json"),
            home_label: Some("home".into()),
            ..WatchSettings::default()
        };
        let clock = TestClock::new(fixed());
        let source = Arc::new(StubSource {
            result: Mutex::new(result),
            calls: AtomicUsize::new(0),
        });
        let server = Arc::new(Server::default());
        let watch = DealWatch::new(settings.clone(), source.clone(), server.clone(), "owner")
            .with_clock(clock.clock());
        Self {
            _dir: dir,
            settings,
            clock,
            source,
            server,
            watch,
        }
    }

    /// The process restarted (also what a crash before the state file's rename leaves).
    fn restart(&mut self) {
        self.watch = DealWatch::new(
            self.settings.clone(),
            self.source.clone(),
            self.server.clone(),
            "owner",
        )
        .with_clock(self.clock.clock());
    }

    fn set_result(&self, result: MatchResult) {
        *self.source.result.lock().unwrap() = result;
    }

    fn file(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&self.settings.state_path).unwrap()).unwrap()
    }
}

fn ham(deals: Vec<lucina_deals::Deal>) -> MatchItem {
    MatchItem {
        name: "Ham".into(),
        rule_id: "ham".into(),
        deals,
    }
}

fn bread(deals: Vec<lucina_deals::Deal>) -> MatchItem {
    MatchItem {
        name: "Bread".into(),
        rule_id: "bread".into(),
        deals,
    }
}

fn clearance(price: f64) -> lucina_deals::Deal {
    deal(
        "fw:store:ean",
        DealSource::FoodWaste,
        "Skinke clearance",
        price,
    )
}

fn flyer(key: &str, name: &str, price: f64) -> lucina_deals::Deal {
    deal(key, DealSource::Flyer, name, price)
}

/// DW1 (model G1 `no_repeat`, 9 actions): the digest reaches the owner but its response
/// is lost, so the state file never records it; the next alert run must not send the
/// same offer again. Fix F1: the run re-sends the pending digest with its dedup key
/// (the server answers `duplicate`), acknowledges it, then has nothing new.
#[tokio::test]
async fn dw1_an_alert_after_a_lost_digest_response_does_not_repeat_the_offer() {
    let rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    rig.server.fail_next(Fault::Lost);
    let digest = rig.watch.run_digest(false).await.unwrap();
    assert_eq!(
        (digest.sent, digest.reason.as_str()),
        (false, "send failed")
    );
    assert_eq!(rig.server.received("Skinke clearance"), 1);

    let alert = rig.watch.run_alerts(false).await.unwrap();
    assert_eq!(
        rig.server.received("Skinke clearance"),
        1,
        "the owner got the offer again: {:#?}",
        rig.server.inbox()
    );
    assert!(alert.sent, "the pending digest was re-sent: {alert:?}");
    let attempts = rig.server.attempts();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1].dedup_key, attempts[0].dedup_key);
    assert_eq!(
        rig.watch.run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
}

/// DW1 (model G1 `one_digest_per_day`, 9 actions): the digest is admitted, the process
/// dies before the save, a source changes and the restarted watcher digests again with
/// a different batch (a new dedup key). Fix F1: the pending digest is re-sent first.
#[tokio::test]
async fn dw1_a_restart_after_an_unsaved_digest_sends_no_second_digest() {
    let mut rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    rig.server.fail_next(Fault::Lost);
    assert!(!rig.watch.run_digest(false).await.unwrap().sent);
    rig.set_result(result_of(vec![
        ham(vec![clearance(10.0)]),
        bread(vec![flyer("tjek:bread", "Rugbrød", 15.0)]),
    ]));
    rig.restart();
    rig.watch.run_digest(false).await.unwrap();
    assert_eq!(
        rig.server.digests(),
        1,
        "two digests on one date: {:#?}",
        rig.server.inbox()
    );
    assert_eq!(
        rig.watch.run_digest(false).await.unwrap().reason,
        "already digested today"
    );
    // The new offer is not lost: the next alert carries it.
    let alert = rig.watch.run_alerts(false).await.unwrap();
    assert!(alert.sent && alert.text.contains("Rugbrød"), "{alert:?}");
    assert_eq!(rig.server.received("Skinke clearance"), 1);
}

/// F1's state file: the composed message is written before sending and cleared once it
/// is acknowledged; a file without it stays in the old format.
#[tokio::test]
async fn f1_the_pending_message_is_saved_before_sending_and_cleared_after() {
    let rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    rig.server.fail_next(Fault::Lost);
    rig.watch.run_alerts(false).await.unwrap();
    let file = rig.file();
    let pending = &file["pending"];
    assert_eq!(pending["kind"], "alert");
    assert_eq!(pending["date"], "2026-10-24");
    assert_eq!(
        pending["dedupKey"].as_str(),
        Some(rig.server.attempts()[0].dedup_key.as_str())
    );
    assert_eq!(pending["notified"]["ham|fw:store:ean"]["price"], 10);
    assert_eq!(pending["attempts"], 1);
    assert!(file["notified"].as_object().unwrap().is_empty());
    assert_eq!(file["alertCounts"], serde_json::json!({}));

    rig.watch.run_alerts(false).await.unwrap();
    let file = rig.file();
    assert!(file.get("pending").is_none(), "{file}");
    assert_eq!(file["notified"]["ham|fw:store:ean"]["price"], 10);
    assert_eq!(file["alertCounts"]["2026-10-24"], 1);
    let text = std::fs::read_to_string(&rig.settings.state_path).unwrap();
    assert!(text.starts_with("{\n  \"notified\": {"), "{text}");
}

/// DW2 (model G0 `no_repeat`, 20 actions): alerted at 10, the next day's digest shows the
/// offer at 12 (acknowledged at 12), the price drops back to 10 and the same offer at a
/// price the owner already received is alerted again. Fix F3: the notified price never
/// goes up.
#[tokio::test]
async fn dw2_a_price_that_rises_and_falls_back_is_not_alerted_again() {
    let rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    assert!(rig.watch.run_alerts(false).await.unwrap().sent);
    rig.clock.set(fixed() + DAY);
    rig.set_result(result_of(vec![ham(vec![clearance(12.0)])]));
    assert!(rig.watch.run_digest(false).await.unwrap().sent);
    rig.set_result(result_of(vec![ham(vec![clearance(10.0)])]));
    let again = rig.watch.run_alerts(false).await.unwrap();
    assert_eq!(
        again.reason, "no new or cheaper matches",
        "alerted a received price again: {again:?}"
    );
    assert_eq!(rig.server.inbox().len(), 2);
    assert_eq!(rig.file()["notified"]["ham|fw:store:ean"]["price"], 10);
}

/// DW2 at the state level: `set_notified` keeps the lower of the two prices.
#[test]
fn dw2_set_notified_keeps_the_lower_price() {
    let mut state = WatchState::empty();
    let to = Value::String("2026-10-30T22:00:00Z".into());
    state.set_notified("ham|fw:a", 10.0, Some(&to));
    state.set_notified("ham|fw:a", 12.0, Some(&to));
    assert_eq!(state.notified_price("ham|fw:a"), Some(10.0));
    state.set_notified("ham|fw:a", 8.0, None);
    assert_eq!(state.notified_price("ham|fw:a"), Some(8.0));
}

/// DW3 (model G0 `no_skip`, 11 actions): the digest shows only each item's best offer
/// but acknowledged the dearer alternative too; when the best offer goes away the
/// alternative is the best offer and was never shown, yet every alert skipped it. Fix F2:
/// only the delivered best offers are acknowledged.
#[tokio::test]
async fn dw3_an_alternative_that_becomes_the_best_offer_is_alerted() {
    let rig = Rig::new(result_of(vec![ham(vec![
        clearance(10.0),
        flyer("tjek:ham", "Skinke flyer", 12.0),
    ])]));
    let digest = rig.watch.run_digest(false).await.unwrap();
    assert!(digest.sent);
    assert!(!digest.text.contains("Skinke flyer"));
    rig.set_result(result_of(vec![ham(vec![flyer(
        "tjek:ham",
        "Skinke flyer",
        12.0,
    )])]));
    let alert = rig.watch.run_alerts(false).await.unwrap();
    assert!(
        alert.sent && alert.text.contains("Skinke flyer"),
        "the alternative was never shown: {alert:?}"
    );
    assert_eq!(rig.server.received("Skinke flyer"), 1);
}

/// F1 drops (a): a message still pending on a later local date is dropped without being
/// sent (a stale digest or alert is worse than none). It is recorded as handled, so a copy
/// the server may already have is never repeated.
#[tokio::test]
async fn f1_a_pending_message_from_an_earlier_day_is_dropped_not_sent() {
    let rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    rig.server.fail_next(Fault::Lost);
    assert!(!rig.watch.run_alerts(false).await.unwrap().sent);
    assert_eq!(rig.file()["pending"]["date"], "2026-10-24");
    rig.clock.set(fixed() + DAY);
    let next = rig.watch.run_alerts(false).await.unwrap();
    assert_eq!(
        rig.server.attempts().len(),
        1,
        "yesterday's alert was sent again: {:#?}",
        rig.server.attempts()
    );
    assert!(!next.sent, "{next:?}");
    let file = rig.file();
    assert!(file.get("pending").is_none(), "{file}");
    assert_eq!(rig.server.received("Skinke clearance"), 1);
    // Today's digest still lists the offer.
    assert!(rig.watch.run_digest(false).await.unwrap().sent);
    assert_eq!(rig.server.received("Skinke clearance"), 2);
}

/// F1 drops (c): after five failed attempts on the same pending message it is dropped
/// instead of being retried by every run.
#[tokio::test]
async fn f1_a_pending_message_is_dropped_after_five_failed_attempts() {
    let rig = Rig::new(result_of(vec![ham(vec![clearance(10.0)])]));
    for _ in 0..5 {
        rig.server.fail_next(Fault::Down);
    }
    for attempt in 1..=5 {
        let run = rig.watch.run_alerts(false).await.unwrap();
        assert_eq!(run.reason, "send failed", "attempt {attempt}");
    }
    let sixth = rig.watch.run_alerts(false).await.unwrap();
    assert_eq!(
        rig.server.attempts().len(),
        5,
        "a sixth attempt: {:#?}",
        rig.server.attempts()
    );
    assert!(!sixth.sent, "{sixth:?}");
    assert!(rig.file().get("pending").is_none());
    let keys: HashSet<String> = rig
        .server
        .attempts()
        .iter()
        .map(|n| n.dedup_key.clone())
        .collect();
    assert_eq!(keys.len(), 1, "every attempt is the same message");
}
