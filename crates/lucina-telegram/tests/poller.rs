//! The poller against a fake Bot API (wiremock): lease rendezvous with a
//! Node record, at-most-once claims across a restart, webhook handling, 409
//! back-off and a revoked token.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_telegram::claim::ClaimFile;
use lucina_telegram::lease_decision::LockRecord;
use lucina_telegram::poller::{Handler, Poller, PollerConfig, SharedHealth};
use lucina_telegram::BotApi;
use serde_json::{json, Value};
use tokio::sync::watch;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "777:TEST_SECRET";

struct Run {
    _dir: tempfile::TempDir,
    lock_dir: std::path::PathBuf,
    claim: ClaimFile,
    seen: Arc<Mutex<Vec<u64>>>,
    health: SharedHealth,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

fn ok(result: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": result}))
}

async fn base_mocks(server: &MockServer, webhook_url: &str) {
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getWebhookInfo")))
        .respond_with(ok(json!({"url": webhook_url, "pending_update_count": 0})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getMe")))
        .respond_with(ok(
            json!({"id": 777, "is_bot": true, "username": "Lucina00_bot"}),
        ))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/deleteWebhook")))
        .respond_with(ok(json!(true)))
        .mount(server)
        .await;
}

/// Long-poll stand-in: an empty batch after a short delay.
async fn idle_updates(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(ok(json!([])).set_delay(Duration::from_millis(100)))
        .with_priority(10)
        .mount(server)
        .await;
}

fn cfg(lock_dir: &std::path::Path, claim: ClaimFile) -> PollerConfig {
    let mut c = PollerConfig::new("Hermes", lock_dir.to_owned(), "lucina-rs", claim);
    c.lease_retry = Duration::from_millis(100);
    c.lease_heartbeat = Duration::from_millis(100);
    c.poll_retry = Duration::from_millis(50);
    c.conflict_retry = Duration::from_millis(1500);
    c.long_poll = 1;
    c.drain = Duration::from_secs(2);
    c
}

impl Run {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lock_dir = dir.path().join("locks");
        let claim = ClaimFile::for_bot(dir.path(), "777");
        let (stop, _) = watch::channel(false);
        Self {
            lock_dir,
            claim,
            _dir: dir,
            seen: Arc::default(),
            health: SharedHealth::default(),
            stop,
            task: None,
        }
    }

    fn start(&mut self, server: &MockServer) {
        let seen = self.seen.clone();
        let handler: Handler = Arc::new(move |u: Value| {
            let seen = seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(u["update_id"].as_u64().unwrap());
            })
        });
        let (stop, rx) = watch::channel(false);
        self.stop = stop;
        let poller = Poller::new(
            BotApi::with_base_url(TOKEN.into(), &server.uri()),
            cfg(&self.lock_dir, self.claim.clone()),
            handler,
            self.health.clone(),
        );
        self.task = Some(tokio::spawn(poller.run(rx)));
    }

    async fn stop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(5), t)
                .await
                .expect("poller stops")
                .unwrap();
        }
    }

    async fn until(&self, what: &str, f: impl Fn(&Run) -> bool) {
        for _ in 0..200 {
            if f(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "timed out waiting for {what}: {:?}",
            self.health.lock().unwrap()
        );
    }

    fn write_lock(&self, record: &LockRecord) {
        std::fs::create_dir_all(&self.lock_dir).unwrap();
        std::fs::write(
            self.lock_dir.join("telegram-777.lock"),
            serde_json::to_vec_pretty(record).unwrap(),
        )
        .unwrap();
    }
}

async fn calls(server: &MockServer, name: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with(&format!("/{name}")))
        .map(|r| r.body_json::<Value>().unwrap_or(Value::Null))
        .collect()
}

fn node_record(heartbeat_at: String) -> LockRecord {
    LockRecord {
        holder: "nezopt:4242:node".into(),
        name: "Hermes".into(),
        bot_key: "777".into(),
        instance: "lucina-nez".into(),
        heartbeat_at,
        ..Default::default()
    }
}

#[tokio::test]
async fn defers_to_a_live_node_record_then_takes_over_without_dropping_updates() {
    let server = MockServer::start().await;
    base_mocks(&server, "").await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(ok(json!([
            {"update_id": 900, "message": {"chat": {"id": 42}, "text": "sent while Node stopped"}}
        ])))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    idle_updates(&server).await;
    let mut run = Run::new();
    run.write_lock(&node_record(chrono::Utc::now().to_rfc3339()));
    run.start(&server);
    run.until("deferral", |r| r.health.lock().unwrap().state == "deferred")
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        calls(&server, "getUpdates").await.is_empty(),
        "no polling while deferred"
    );
    {
        let h = run.health.lock().unwrap();
        assert!(!h.lease_held);
        assert_eq!(h.holder.as_ref().unwrap().instance, "lucina-nez");
    }
    // Node stopped without releasing: its heartbeat goes stale.
    run.write_lock(&node_record("2000-01-01T00:00:00.000Z".into()));
    run.until("the update", |r| r.seen.lock().unwrap().as_slice() == [900])
        .await;
    {
        let h = run.health.lock().unwrap();
        assert!(h.lease_held);
        assert_eq!(h.state, "polling");
        assert_eq!(h.bot_username.as_deref(), Some("Lucina00_bot"));
        assert_eq!(h.claimed_update_id, Some(900));
    }
    let gets = calls(&server, "getUpdates").await;
    assert_eq!(gets[0]["offset"], 0);
    assert_eq!(
        gets[0]["allowed_updates"],
        json!(["message", "edited_message", "callback_query"])
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let gets = calls(&server, "getUpdates").await;
    assert!(gets.len() >= 2);
    assert_eq!(gets[1]["offset"], 901);
    for body in &gets {
        assert!(body.get("drop_pending_updates").is_none());
    }
    assert!(
        calls(&server, "deleteWebhook").await.is_empty(),
        "no webhook: nothing deleted"
    );
    let lock = run.lock_dir.join("telegram-777.lock");
    let held: LockRecord = serde_json::from_slice(&std::fs::read(&lock).unwrap()).unwrap();
    assert_eq!(held.instance, "lucina-rs");
    assert!(!std::fs::read_to_string(&lock)
        .unwrap()
        .contains("TEST_SECRET"));
    run.stop().await;
    assert!(!lock.exists(), "shutdown releases the lease");
    assert_eq!(run.health.lock().unwrap().state, "stopped");
}

#[tokio::test]
async fn a_foreign_heartbeat_stops_polling() {
    let server = MockServer::start().await;
    base_mocks(&server, "").await;
    idle_updates(&server).await;
    let mut run = Run::new();
    run.start(&server);
    run.until("polling", |r| {
        r.health.lock().unwrap().last_poll_ok_at.is_some()
    })
    .await;
    // Node starts and writes its record (again until the next heartbeat sees it).
    for _ in 0..100 {
        run.write_lock(&node_record(chrono::Utc::now().to_rfc3339()));
        if !run.health.lock().unwrap().lease_held {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    run.until("loss", |r| !r.health.lock().unwrap().lease_held)
        .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let before = calls(&server, "getUpdates").await.len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        calls(&server, "getUpdates").await.len(),
        before,
        "no polling after loss"
    );
    assert_eq!(run.health.lock().unwrap().state, "deferred");
    run.stop().await;
    let lock = run.lock_dir.join("telegram-777.lock");
    let record: LockRecord = serde_json::from_slice(&std::fs::read(lock).unwrap()).unwrap();
    assert_eq!(
        record.instance, "lucina-nez",
        "the other holder's record is kept"
    );
}

#[tokio::test]
async fn a_restart_after_claiming_never_handles_an_update_twice() {
    let server = MockServer::start().await;
    base_mocks(&server, "https://example.invalid/hook").await;
    // Telegram re-sends 5 and 6 to every poller (no confirmation reached it).
    let resend = || {
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getUpdates")))
            .respond_with(ok(json!([
                {"update_id": 5, "message": {"chat": {"id": 1}}},
                {"update_id": 6, "message": {"chat": {"id": 2}}}
            ])))
            .up_to_n_times(1)
            .with_priority(1)
    };
    resend().mount(&server).await;
    idle_updates(&server).await;
    let mut run = Run::new();
    // The previous process claimed 5 and crashed before handling it.
    run.claim.store(5).unwrap();
    run.start(&server);
    run.until("update 6", |r| r.seen.lock().unwrap().as_slice() == [6])
        .await;
    assert_eq!(run.claim.load().unwrap(), Some(6));
    run.stop().await;
    assert_eq!(run.health.lock().unwrap().skipped_replayed, 1);
    // Restarted: Telegram sends both again.
    resend().mount(&server).await;
    run.start(&server);
    run.until("the replay", |r| {
        r.health.lock().unwrap().skipped_replayed >= 3
    })
    .await;
    run.stop().await;
    assert_eq!(run.seen.lock().unwrap().as_slice(), [6]);
    // NR4: each start polls once without an offset, so a batch cut off by a crash is
    // re-delivered and logged as skipped instead of being confirmed unseen.
    let polls = calls(&server, "getUpdates").await;
    assert_eq!(polls[0]["offset"], json!(0));
    assert!(polls.iter().filter(|p| p["offset"] == json!(0)).count() >= 2);
    assert!(polls.iter().any(|p| p["offset"] == json!(7)));
    let deletes = calls(&server, "deleteWebhook").await;
    assert!(!deletes.is_empty());
    for d in deletes {
        assert_eq!(d, json!({"drop_pending_updates": false}));
    }
}

#[tokio::test]
async fn a_conflict_backs_off_and_a_revoked_token_stops() {
    let server = MockServer::start().await;
    base_mocks(&server, "").await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false, "error_code": 409,
            "description": "Conflict: terminated by other getUpdates request"
        })))
        .mount(&server)
        .await;
    let mut run = Run::new();
    run.start(&server);
    run.until("the conflict", |r| r.health.lock().unwrap().conflicts == 1)
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        calls(&server, "getUpdates").await.len(),
        1,
        "30 s-style back-off, not a hot loop"
    );
    {
        let h = run.health.lock().unwrap();
        assert_eq!(h.state, "backoff");
        assert!(h.last_error.as_deref().unwrap().starts_with("conflict"));
        assert!(!h.last_error.as_deref().unwrap().contains("TEST_SECRET"));
    }
    run.stop().await;

    let server = MockServer::start().await;
    base_mocks(&server, "").await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getUpdates")))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "ok": false, "error_code": 401, "description": "Unauthorized"
        })))
        .mount(&server)
        .await;
    let mut run = Run::new();
    run.start(&server);
    let task = run.task.take().unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("a rejected token stops the poller")
        .unwrap();
    let h = run.health.lock().unwrap().clone();
    assert_eq!(h.state, "unauthorized");
    assert!(!h.lease_held);
    assert!(!run.lock_dir.join("telegram-777.lock").exists());
}
