//! The long-polling loop of one bot (Node `launchManagedTelegramPolling`).
//!
//! - The lease ([`crate::lease`]) is held before the first `getUpdates`.
//!   Deferred: the holder is logged (instance and heartbeat, never the token)
//!   and acquisition is retried every 5 s. Lost: polling stops at once and
//!   acquisition is retried. Shutdown: polling stops, the lease is released,
//!   then in-flight handlers get a bounded drain.
//! - Pending updates are never dropped: no `drop_pending_updates`. A webhook
//!   is only deleted when `getWebhookInfo` reports one, with
//!   `drop_pending_updates: false`.
//! - At most once: a batch's highest new `update_id` is persisted
//!   ([`crate::claim`]) before any of its updates reaches a handler; updates
//!   at or below the claim are skipped (`skippedReplayed`).
//! - Errors: 409 backs off 30 s (another consumer: the lease should make it
//!   impossible, so it is logged as an error); 401/404 stop the poller (the
//!   token is revoked); anything else backs off 5 s; `retry_after` is honoured.
//! - Updates of different chats run concurrently; one chat's run in order
//!   (one bounded queue per chat).
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::claim::{self, ClaimFile};
use crate::lease::{self, Lease};
use crate::{ApiError, BotApi};

pub type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// Handles one update (the whole `Update` object).
pub type Handler = Arc<dyn Fn(Value) -> HandlerFuture + Send + Sync>;

/// What Node's Telegraf effectively consumed is `message` updates; edited
/// messages and callback queries are accepted (and claimed) but ignored by
/// the Lucina handler, as in Node.
pub const ALLOWED_UPDATES: [&str; 3] = ["message", "edited_message", "callback_query"];
/// Node `TELEGRAM_POLL_RETRY_MS`.
pub const POLL_RETRY: Duration = Duration::from_secs(5);
/// Node `TELEGRAM_CONFLICT_RETRY_MS`.
pub const CONFLICT_RETRY: Duration = Duration::from_secs(30);
/// Node `config.gatewayLock.retryMs`.
pub const LEASE_RETRY: Duration = Duration::from_secs(5);
const WORKER_IDLE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HolderInfo {
    pub instance: String,
    pub heartbeat_at: String,
}

/// `/health` → `telegram`. Never holds a token or message text.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PollerHealth {
    pub enabled: bool,
    /// `starting` | `deferred` | `polling` | `backoff` | `unauthorized` | `stopped`.
    pub state: String,
    pub lease_held: bool,
    /// The other holder while the lease is deferred.
    pub holder: Option<HolderInfo>,
    pub bot_username: Option<String>,
    pub last_poll_ok_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: u64,
    pub updates_handled: u64,
    pub skipped_replayed: u64,
    pub conflicts: u64,
    pub claimed_update_id: Option<u64>,
    /// How many times the lease was acquired (handlers reload shared files
    /// another holder may have written in between).
    pub lease_acquisitions: u64,
}

pub type SharedHealth = Arc<Mutex<PollerHealth>>;

fn with_health<R>(h: &SharedHealth, f: impl FnOnce(&mut PollerHealth) -> R) -> R {
    f(&mut h.lock().unwrap_or_else(|e| e.into_inner()))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// The lease record's `name` (Node: `Hermes` for the Lucina bot).
    pub name: String,
    pub lock_dir: PathBuf,
    pub instance: String,
    pub claim: ClaimFile,
    pub lease_retry: Duration,
    pub lease_heartbeat: Duration,
    pub poll_retry: Duration,
    pub conflict_retry: Duration,
    /// `getUpdates` long-poll seconds.
    pub long_poll: u64,
    pub allowed_updates: Vec<String>,
    /// Bounded queue per chat.
    pub chat_queue: usize,
    /// How long in-flight handlers may finish after shutdown.
    pub drain: Duration,
}

impl PollerConfig {
    pub fn new(name: &str, lock_dir: PathBuf, instance: &str, claim: ClaimFile) -> Self {
        Self {
            name: name.into(),
            lock_dir,
            instance: instance.into(),
            claim,
            lease_retry: LEASE_RETRY,
            lease_heartbeat: lease::HEARTBEAT,
            poll_retry: POLL_RETRY,
            conflict_retry: CONFLICT_RETRY,
            long_poll: 30,
            allowed_updates: ALLOWED_UPDATES.iter().map(|s| s.to_string()).collect(),
            chat_queue: 64,
            drain: Duration::from_secs(20),
        }
    }
}

/// The chat whose queue an update joins (`_` when it has none).
pub fn chat_key(update: &Value) -> String {
    let chat = update["message"]["chat"]["id"]
        .as_i64()
        .or_else(|| update["edited_message"]["chat"]["id"].as_i64())
        .or_else(|| update["callback_query"]["message"]["chat"]["id"].as_i64())
        .or_else(|| update["callback_query"]["from"]["id"].as_i64());
    chat.map_or_else(|| "_".into(), |c| c.to_string())
}

/// A batch split into the updates to hand out (in order) and the new claim.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub handle: Vec<Value>,
    pub claimed: Option<u64>,
    pub skipped: u64,
    pub malformed: u64,
}

/// Apply the claim kernel to one `getUpdates` result.
pub fn plan_batch(result: &Value, mut claimed: Option<u64>) -> Batch {
    let mut b = Batch::default();
    for u in result.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(id) = u["update_id"].as_u64() else {
            b.malformed += 1;
            continue;
        };
        if claim::should_handle_opt(id, claimed) {
            claimed = Some(claim::claim_opt(id, claimed));
            b.handle.push(u.clone());
        } else {
            b.skipped += 1;
        }
    }
    b.claimed = claimed;
    b
}

fn is_fatal(e: &ApiError) -> bool {
    matches!(e.error_code, Some(401) | Some(404)) || matches!(e.http_status, Some(401) | Some(404))
}

fn is_conflict(e: &ApiError) -> bool {
    e.error_code == Some(409) || e.http_status == Some(409)
}

/// Per-chat ordered queues.
struct Dispatcher {
    handler: Handler,
    cap: usize,
    queues: Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>,
    workers: JoinSet<()>,
    health: SharedHealth,
}

impl Dispatcher {
    fn new(handler: Handler, cap: usize, health: SharedHealth) -> Self {
        Self {
            handler,
            cap: cap.max(1),
            queues: Arc::default(),
            workers: JoinSet::new(),
            health,
        }
    }

    async fn dispatch(&mut self, update: Value) {
        while self.workers.try_join_next().is_some() {}
        let key = chat_key(&update);
        let mut update = update;
        loop {
            let sender = {
                let mut q = self.queues.lock().unwrap_or_else(|e| e.into_inner());
                let tx = match q.get(&key) {
                    Some(tx) if !tx.is_closed() => tx.clone(),
                    _ => {
                        let (tx, rx) = mpsc::channel(self.cap);
                        q.insert(key.clone(), tx.clone());
                        self.workers.spawn(worker(
                            key.clone(),
                            rx,
                            self.handler.clone(),
                            self.queues.clone(),
                            self.health.clone(),
                        ));
                        tx
                    }
                };
                match tx.try_send(update) {
                    Ok(()) => return,
                    Err(mpsc::error::TrySendError::Full(u)) => {
                        update = u;
                        tx
                    }
                    Err(mpsc::error::TrySendError::Closed(u)) => {
                        update = u;
                        q.remove(&key);
                        continue;
                    }
                }
            };
            // Bounded: a full chat queue holds the poller back.
            match sender.send(update).await {
                Ok(()) => return,
                Err(mpsc::error::SendError(u)) => update = u,
            }
        }
    }

    /// Close every queue and let the workers finish, for at most `limit`.
    async fn drain(mut self, limit: Duration) {
        self.queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let all = async { while self.workers.join_next().await.is_some() {} };
        if tokio::time::timeout(limit, all).await.is_err() {
            tracing::warn!("telegram handlers still running at shutdown; abandoned");
            self.workers.abort_all();
        }
    }
}

async fn run_one(handler: &Handler, update: Value, health: &SharedHealth) {
    let id = update["update_id"].as_u64();
    // Its own task: a panicking handler does not stop the chat's queue.
    if let Err(e) = tokio::spawn(handler(update)).await {
        tracing::error!(
            update_id = id,
            panicked = e.is_panic(),
            "telegram update handler failed"
        );
    }
    with_health(health, |h| h.updates_handled += 1);
}

async fn worker(
    key: String,
    mut rx: mpsc::Receiver<Value>,
    handler: Handler,
    queues: Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>,
    health: SharedHealth,
) {
    loop {
        match tokio::time::timeout(WORKER_IDLE, rx.recv()).await {
            Ok(Some(update)) => run_one(&handler, update, &health).await,
            Ok(None) => break,
            Err(_) => {
                let idle = {
                    let mut q = queues.lock().unwrap_or_else(|e| e.into_inner());
                    let idle = rx.is_empty();
                    if idle {
                        q.remove(&key);
                    }
                    idle
                };
                if idle {
                    rx.close();
                    while let Ok(update) = rx.try_recv() {
                        run_one(&handler, update, &health).await;
                    }
                    break;
                }
            }
        }
    }
}

enum Stop {
    Shutdown,
    Lost,
    Fatal,
}

/// Sleep `d` unless shutdown comes first (`true`: shut down).
async fn pause(d: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(d) => *shutdown.borrow(),
        r = shutdown.changed() => r.is_err() || *shutdown.borrow(),
    }
}

/// Sleep `d` while polling: shutdown or lease loss interrupt it.
async fn pause_polling(
    d: Duration,
    shutdown: &mut watch::Receiver<bool>,
    lost: &mut watch::Receiver<bool>,
) -> Option<Stop> {
    tokio::select! {
        _ = tokio::time::sleep(d) => None,
        r = shutdown.changed() => (r.is_err() || *shutdown.borrow()).then_some(Stop::Shutdown),
        _ = lost.changed() => Some(Stop::Lost),
    }
}

pub struct Poller {
    bot: BotApi,
    cfg: PollerConfig,
    handler: Handler,
    health: SharedHealth,
}

impl Poller {
    pub fn new(bot: BotApi, cfg: PollerConfig, handler: Handler, health: SharedHealth) -> Self {
        Self {
            bot,
            cfg,
            handler,
            health,
        }
    }

    fn fail(&self, state: &str, error: &str) {
        let error = error.replace(self.bot.token(), "[REDACTED]");
        with_health(&self.health, |h| {
            h.state = state.into();
            h.last_error = Some(error);
            h.consecutive_failures += 1;
        });
    }

    /// Poll until `shutdown` turns true (or its sender goes away), the token
    /// is rejected, or the claim file cannot be read.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        with_health(&self.health, |h| {
            h.enabled = true;
            h.state = "starting".into();
        });
        let mut claimed = match self.cfg.claim.load() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(path = %self.cfg.claim.path().display(), error = %e, "telegram claim file unreadable; not polling");
                self.fail("stopped", "claim file unreadable");
                return;
            }
        };
        with_health(&self.health, |h| h.claimed_update_id = claimed);
        let mut dispatcher = Dispatcher::new(
            self.handler.clone(),
            self.cfg.chat_queue,
            self.health.clone(),
        );
        let token = self.bot.token().to_owned();
        let mut deferrals = 0u64;
        'outer: loop {
            if *shutdown.borrow() {
                break;
            }
            let lease = match Lease::acquire_with(
                &self.cfg.lock_dir,
                &self.cfg.name,
                &token,
                &self.cfg.instance,
                self.cfg.lease_heartbeat,
            )
            .await
            {
                Ok(Some(l)) => l,
                Ok(None) => {
                    let holder =
                        lease::read_holder(&self.cfg.lock_dir, &token).map(|r| HolderInfo {
                            instance: r.instance,
                            heartbeat_at: r.heartbeat_at,
                        });
                    if deferrals.is_multiple_of(12) {
                        tracing::warn!(
                            bot = %self.bot.bot_key(),
                            holder_instance = holder.as_ref().map(|h| h.instance.as_str()),
                            holder_heartbeat = holder.as_ref().map(|h| h.heartbeat_at.as_str()),
                            retry_ms = self.cfg.lease_retry.as_millis() as u64,
                            "telegram polling deferred: another gateway holds the bot"
                        );
                    }
                    deferrals += 1;
                    with_health(&self.health, |h| {
                        h.state = "deferred".into();
                        h.lease_held = false;
                        h.holder = holder;
                    });
                    if pause(self.cfg.lease_retry, &mut shutdown).await {
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    tracing::error!(error = %e, "telegram gateway lease unusable");
                    self.fail("deferred", &format!("lease: {e}"));
                    if pause(self.cfg.lease_retry, &mut shutdown).await {
                        break;
                    }
                    continue;
                }
            };
            deferrals = 0;
            with_health(&self.health, |h| {
                h.state = "polling".into();
                h.lease_held = true;
                h.holder = None;
                h.lease_acquisitions += 1;
            });
            tracing::info!(bot = %self.bot.bot_key(), instance = %self.cfg.instance, "telegram polling lease acquired");
            let stop = self
                .poll(&mut shutdown, &lease, &mut claimed, &mut dispatcher)
                .await;
            // Release first: a successor may poll while handlers finish.
            drop(lease);
            with_health(&self.health, |h| h.lease_held = false);
            match stop {
                Stop::Shutdown => break 'outer,
                Stop::Fatal => {
                    tracing::error!("telegram rejected the bot token; polling stopped");
                    with_health(&self.health, |h| h.state = "unauthorized".into());
                    dispatcher.drain(self.cfg.drain).await;
                    return;
                }
                Stop::Lost => {
                    tracing::warn!("telegram gateway lease lost; polling stopped, reacquiring");
                    with_health(&self.health, |h| h.state = "deferred".into());
                    if pause(self.cfg.lease_retry, &mut shutdown).await {
                        break 'outer;
                    }
                }
            }
        }
        dispatcher.drain(self.cfg.drain).await;
        with_health(&self.health, |h| {
            h.state = "stopped".into();
            h.lease_held = false;
        });
        tracing::info!("telegram poller stopped; lease released");
    }

    async fn poll(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        lease: &Lease,
        claimed: &mut Option<u64>,
        dispatcher: &mut Dispatcher,
    ) -> Stop {
        let mut lost = lease.lost();
        if *lost.borrow() {
            return Stop::Lost;
        }
        // Keep pending updates: only a configured webhook is removed, without dropping.
        match self.bot.get_webhook_info().await {
            Ok(info) => {
                if info["url"].as_str().is_some_and(|u| !u.is_empty()) {
                    tracing::warn!("telegram webhook set; deleting it (pending updates kept)");
                    if let Err(e) = self.bot.delete_webhook(false).await {
                        if is_fatal(&e) {
                            return Stop::Fatal;
                        }
                        tracing::warn!(error = %e, "telegram deleteWebhook failed");
                    }
                }
            }
            Err(e) if is_fatal(&e) => {
                self.fail("unauthorized", &e.to_string());
                return Stop::Fatal;
            }
            Err(e) => tracing::warn!(error = %e, "telegram getWebhookInfo failed"),
        }
        match self.bot.get_me().await {
            Ok(me) => {
                let username = me["username"].as_str().map(str::to_owned);
                tracing::info!(
                    username = username.as_deref().unwrap_or("unknown"),
                    "telegram polling started"
                );
                with_health(&self.health, |h| h.bot_username = username);
            }
            Err(e) if is_fatal(&e) => {
                self.fail("unauthorized", &e.to_string());
                return Stop::Fatal;
            }
            Err(e) => tracing::warn!(error = %e, "telegram getMe failed"),
        }
        let allowed: Vec<&str> = self
            .cfg
            .allowed_updates
            .iter()
            .map(String::as_str)
            .collect();
        loop {
            if *lost.borrow() {
                return Stop::Lost;
            }
            let offset = claim::next_offset(*claimed);
            let result = tokio::select! {
                biased;
                r = shutdown.changed() => {
                    if r.is_err() || *shutdown.borrow() { return Stop::Shutdown; }
                    continue;
                }
                _ = lost.changed() => return Stop::Lost,
                r = self.bot.get_updates(offset, self.cfg.long_poll, &allowed) => r,
            };
            let wait = match result {
                Ok(updates) => {
                    with_health(&self.health, |h| {
                        h.state = "polling".into();
                        h.last_poll_ok_at = Some(now_iso());
                        h.consecutive_failures = 0;
                    });
                    let batch = plan_batch(&updates, *claimed);
                    if batch.skipped > 0 {
                        tracing::warn!(
                            skipped = batch.skipped,
                            "telegram updates at or below the claim skipped (already claimed)"
                        );
                    }
                    if batch.malformed > 0 {
                        tracing::warn!(
                            malformed = batch.malformed,
                            "telegram updates without update_id ignored"
                        );
                    }
                    with_health(&self.health, |h| h.skipped_replayed += batch.skipped);
                    if batch.claimed != *claimed {
                        let new = batch.claimed.unwrap_or_default();
                        let file = self.cfg.claim.clone();
                        let stored = tokio::task::spawn_blocking(move || file.store(new))
                            .await
                            .map_err(std::io::Error::other)
                            .and_then(|r| r);
                        if let Err(e) = stored {
                            // Nothing is handed out unclaimed; the same batch comes back.
                            tracing::error!(error = %e, "telegram claim not persisted; batch not handled");
                            self.fail("backoff", "claim file not writable");
                            if let Some(stop) =
                                pause_polling(self.cfg.poll_retry, shutdown, &mut lost).await
                            {
                                return stop;
                            }
                            continue;
                        }
                        *claimed = batch.claimed;
                        with_health(&self.health, |h| h.claimed_update_id = *claimed);
                    }
                    for update in batch.handle {
                        dispatcher.dispatch(update).await;
                    }
                    continue;
                }
                Err(e) if is_fatal(&e) => {
                    self.fail("unauthorized", &e.to_string());
                    return Stop::Fatal;
                }
                Err(e) if is_conflict(&e) => {
                    tracing::error!(error = %e, "telegram getUpdates conflict: another consumer is polling this bot");
                    self.fail("backoff", &format!("conflict: {e}"));
                    with_health(&self.health, |h| h.conflicts += 1);
                    self.cfg
                        .conflict_retry
                        .max(Duration::from_secs(e.retry_after.unwrap_or(0)))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "telegram getUpdates failed");
                    self.fail("backoff", &e.to_string());
                    e.retry_after
                        .map(Duration::from_secs)
                        .unwrap_or(self.cfg.poll_retry)
                }
            };
            if let Some(stop) = pause_polling(wait, shutdown, &mut lost).await {
                return stop;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batches_claim_once_and_skip_replays() {
        let r = json!([{"update_id": 5}, {"update_id": 6}, {"x": 1}, {"update_id": 6}]);
        let b = plan_batch(&r, Some(4));
        assert_eq!(b.handle.len(), 2);
        assert_eq!(b.claimed, Some(6));
        assert_eq!((b.skipped, b.malformed), (1, 1));
        let again = plan_batch(&r, b.claimed);
        assert!(again.handle.is_empty());
        assert_eq!(again.skipped, 3);
        assert_eq!(plan_batch(&json!([]), None), Batch::default());
        assert_eq!(
            plan_batch(&json!([{"update_id": 0}]), None).claimed,
            Some(0)
        );
    }

    #[test]
    fn chat_keys() {
        assert_eq!(
            chat_key(&json!({"message": {"chat": {"id": -100}}})),
            "-100"
        );
        assert_eq!(
            chat_key(&json!({"callback_query": {"from": {"id": 7}}})),
            "7"
        );
        assert_eq!(chat_key(&json!({"poll": {}})), "_");
    }

    #[tokio::test]
    async fn one_chat_in_order_chats_concurrently() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l = log.clone();
        let handler: Handler = Arc::new(move |u: Value| {
            let l = l.clone();
            Box::pin(async move {
                let id = u["update_id"].as_u64().unwrap();
                let chat = chat_key(&u);
                // Chat 1's first update is slow: chat 2 must not wait for it.
                if id == 1 {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                l.lock().unwrap().push(format!("{chat}:{id}"));
            })
        });
        let health = SharedHealth::default();
        let mut d = Dispatcher::new(handler, 2, health.clone());
        for (id, chat) in [(1, 1), (2, 1), (3, 2), (4, 1)] {
            d.dispatch(json!({"update_id": id, "message": {"chat": {"id": chat}}}))
                .await;
        }
        d.drain(Duration::from_secs(5)).await;
        let log = log.lock().unwrap().clone();
        assert_eq!(log[0], "2:3", "{log:?}");
        let chat1: Vec<_> = log.iter().filter(|e| e.starts_with("1:")).collect();
        assert_eq!(chat1, ["1:1", "1:2", "1:4"]);
        assert_eq!(health.lock().unwrap().updates_handled, 4);
    }
}
