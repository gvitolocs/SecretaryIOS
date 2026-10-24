//! The deal watch's state and one run (`watch.js`, without its scheduler
//! and channel transports): a morning digest of the shopping list's best
//! active offers and incremental alerts for new or cheaper ones.
//!
//! The state file is Node's `data/grocery/deal-watch.json`, read and written
//! byte-compatibly (key order kept, `JSON.stringify(state, null, 2)`), so a
//! switch-over between runtimes does not notify again:
//!
//! ```json
//! { "notified": { "<ruleId>|<dealKey>": { "price": 6, "validTo": "…" } },
//!   "lastDigestDate": "2026-10-24", "digestFingerprint": "[[\"key\",6]]",
//!   "alertCounts": { "2026-10-24": 1 } }
//! ```
//!
//! [`DealWatch::run`] is Node's `run(kind, { dryRun })`; delivery is a
//! [`Deliver`] (app notification, Telegram or both), and the scheduler
//! (digest time, alert window and interval) calls it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono_tz::Tz;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::engine::{DealsEngine, MatchItem, MatchResult};
use crate::format::{day_label, format_deal, format_matches, local_date, FormatOptions};
use crate::js::{self, locale_compare, parse_date, utf16_len, JsValue};
use crate::sources::Deal;
use crate::validity::Status;
use crate::{Clock, DealsError};

/// The app notification fields every deal notification uses.
pub const NOTIFY_CATEGORY: &str = "shopping";
pub const NOTIFY_ROUTE: &str = "/shopping/map";
pub const NOTIFY_CHANNEL: &str = "lucina";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    Digest,
    Alerts,
}

/// `deal-watch.json`, kept as the ordered JSON Node wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchState {
    root: Vec<(String, JsValue)>,
}

impl Default for WatchState {
    fn default() -> Self {
        Self::empty()
    }
}

fn object_mut<'a>(root: &'a mut [(String, JsValue)], key: &str) -> &'a mut Vec<(String, JsValue)> {
    let slot = root
        .iter_mut()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .expect("state key");
    if !matches!(slot, JsValue::Object(_)) {
        *slot = JsValue::Object(Vec::new());
    }
    match slot {
        JsValue::Object(entries) => entries,
        _ => unreachable!(),
    }
}

fn set_entry(entries: &mut Vec<(String, JsValue)>, key: &str, value: JsValue) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => entries.push((key.to_owned(), value)),
    }
}

impl WatchState {
    pub fn empty() -> Self {
        Self {
            root: vec![
                ("notified".into(), JsValue::Object(Vec::new())),
                ("lastDigestDate".into(), JsValue::Null),
                ("digestFingerprint".into(), JsValue::Null),
                ("alertCounts".into(), JsValue::Object(Vec::new())),
            ],
        }
    }

    /// `{ ...empty(), ...JSON.parse(text) }`.
    pub fn parse(text: &str) -> Result<Self, DealsError> {
        let value = JsValue::parse(text).map_err(|e| DealsError::State(e.to_string()))?;
        let mut state = Self::empty();
        if let JsValue::Object(entries) = value {
            for (k, v) in entries {
                set_entry(&mut state.root, &k, v);
            }
        }
        Ok(state)
    }

    /// The state at `path`; a missing file is the empty state.
    pub fn load(path: &Path) -> Result<Self, DealsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(DealsError::State(e.to_string())),
        }
    }

    /// `JSON.stringify(state, null, 2)`.
    pub fn to_json(&self) -> String {
        JsValue::Object(self.root.clone()).stringify(2)
    }

    /// Atomic write (`<file>.<pid>.tmp`, mode 0600, rename), like Node.
    pub fn save(&self, path: &Path) -> Result<(), DealsError> {
        let err = |e: std::io::Error| DealsError::State(e.to_string());
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(err)?;
            file.write_all(self.to_json().as_bytes()).map_err(err)?;
        }
        std::fs::rename(&tmp, path).map_err(err)
    }

    fn get(&self, key: &str) -> Option<&JsValue> {
        self.root.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn last_digest_date(&self) -> Option<&str> {
        self.get("lastDigestDate").and_then(JsValue::as_str)
    }

    pub fn digest_fingerprint(&self) -> Option<&str> {
        self.get("digestFingerprint").and_then(JsValue::as_str)
    }

    /// `alertCounts[date] || 0`.
    pub fn alert_count(&self, date: &str) -> f64 {
        let n = self
            .get("alertCounts")
            .and_then(|c| c.get(date))
            .map_or(0.0, JsValue::to_number);
        if n.is_nan() {
            0.0
        } else {
            n
        }
    }

    /// The price an offer was last notified at (`NaN` for an entry
    /// without one), `None` when it was never notified.
    pub fn notified_price(&self, id: &str) -> Option<f64> {
        let entry = self.get("notified").and_then(|n| n.get(id))?;
        let truthy = match entry {
            JsValue::Null => false,
            JsValue::Bool(b) => *b,
            JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
            JsValue::String(s) => !s.is_empty(),
            _ => true,
        };
        truthy.then(|| entry.get("price").map_or(f64::NAN, JsValue::to_number))
    }

    /// Forget notifications a day after their offer ended.
    pub fn prune(&mut self, now_ms: i64) {
        object_mut(&mut self.root, "notified").retain(|(_, entry)| {
            let end = entry
                .get("validTo")
                .and_then(JsValue::as_str)
                .and_then(parse_date);
            !end.is_some_and(|end| end + 86_400_000 < now_ms)
        });
    }

    pub fn set_notified(&mut self, id: &str, price: f64, valid_to: Option<&Value>) {
        let mut entry = vec![("price".to_owned(), JsValue::Number(price))];
        if let Some(v) = valid_to {
            entry.push(("validTo".into(), to_js(v)));
        }
        set_entry(
            object_mut(&mut self.root, "notified"),
            id,
            JsValue::Object(entry),
        );
    }

    pub fn set_last_digest_date(&mut self, date: &str) {
        set_entry(
            &mut self.root,
            "lastDigestDate",
            JsValue::String(date.into()),
        );
    }

    fn set_digest_fingerprint(&mut self, fingerprint: &str) {
        set_entry(
            &mut self.root,
            "digestFingerprint",
            JsValue::String(fingerprint.into()),
        );
    }

    fn count_alert(&mut self, date: &str) {
        let n = self.alert_count(date) + 1.0;
        set_entry(
            object_mut(&mut self.root, "alertCounts"),
            date,
            JsValue::Number(n),
        );
    }

    fn keep_alert_counts_from(&mut self, date: &str) {
        object_mut(&mut self.root, "alertCounts").retain(|(d, _)| d.as_str() >= date);
    }
}

fn to_js(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(b) => JsValue::Bool(*b),
        Value::Number(n) => JsValue::Number(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => JsValue::String(s.clone()),
        Value::Array(a) => JsValue::Array(a.iter().map(to_js).collect()),
        Value::Object(m) => JsValue::Object(m.iter().map(|(k, v)| (k.clone(), to_js(v))).collect()),
    }
}

/// Include every branch identity when tracking a grouped clearance.
pub fn deal_keys(deal: &Deal) -> Vec<String> {
    let mut keys = vec![deal.key.clone()];
    for k in deal.related_keys.iter().flatten() {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }
    keys
}

/// `JSON.stringify` of `[key, price]` pairs, deduplicated (last price wins,
/// first position kept) and sorted by `localeCompare`.
fn fingerprint(pairs: impl Iterator<Item = (String, f64)>) -> String {
    let mut entries: Vec<(String, f64)> = Vec::new();
    for (k, price) in pairs {
        match entries.iter_mut().find(|(old, _)| *old == k) {
            Some(slot) => slot.1 = price,
            None => entries.push((k, price)),
        }
    }
    entries.sort_by(|a, b| locale_compare(&a.0, &b.0));
    JsValue::Array(
        entries
            .into_iter()
            .map(|(k, p)| JsValue::Array(vec![JsValue::String(k), JsValue::Number(p)]))
            .collect(),
    )
    .stringify(0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatchSettings {
    pub state_path: PathBuf,
    /// The owner's zone (`LUCINA_REMINDER_TIMEZONE`, else Europe/Copenhagen):
    /// digest dates, alert caps and the digest header.
    pub zone: Tz,
    pub home_label: Option<String>,
    pub alert_max_per_day: f64,
}

impl Default for WatchSettings {
    fn default() -> Self {
        Self {
            state_path: PathBuf::from("data/grocery/deal-watch.json"),
            zone: chrono_tz::Europe::Copenhagen,
            home_label: None,
            alert_max_per_day: 4.0,
        }
    }
}

/// A composed notification, ready for delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub kind: WatchKind,
    pub title: String,
    pub body: String,
    /// `title\nbody` (Telegram text, chat context).
    pub text: String,
    /// `grocery-deals:<digest|alert>:<date>:<hash>` (app notification dedup).
    pub dedup_key: String,
}

/// What [`compose`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Composed {
    /// Nothing to send; a real digest run still records the day.
    Skip(&'static str),
    /// Every best offer is too long for one message (nothing recorded).
    TooLong,
    Ready(Box<Prepared>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub notification: Notification,
    active: Vec<MatchItem>,
    omitted: Vec<MatchItem>,
    fingerprint: String,
    date: String,
}

fn header(kind: WatchKind, count: usize, time: i64, settings: &WatchSettings) -> String {
    match kind {
        WatchKind::Digest => format!(
            "🛒 Deals on your shopping list near {} — {}",
            settings
                .home_label
                .as_deref()
                .filter(|l| !l.is_empty())
                .unwrap_or("home"),
            day_label(time, settings.zone)
        ),
        WatchKind::Alerts => format!(
            "🛒 New deal{} on your shopping list",
            if count > 1 { "s" } else { "" }
        ),
    }
}

/// The notification for `result` given what `state` already notified:
/// each item's best active offer, at most five items within 1490 characters.
pub fn compose(
    kind: WatchKind,
    state: &WatchState,
    result: &MatchResult,
    time: i64,
    settings: &WatchSettings,
) -> Composed {
    let date = local_date(time, settings.zone);
    let now = if result.now.is_empty() {
        js::iso_string(time)
    } else {
        result.now.clone()
    };
    let active: Vec<MatchItem> = result
        .items
        .iter()
        .map(|i| MatchItem {
            deals: i
                .deals
                .iter()
                .filter(|d| d.status == Status::Active)
                .cloned()
                .collect(),
            ..i.clone()
        })
        .collect();
    let digest_fingerprint = fingerprint(
        active
            .iter()
            .flat_map(|i| i.deals.iter().map(|d| (d.key.clone(), d.price_dkk))),
    );
    if !active.iter().any(|i| !i.deals.is_empty()) {
        return Composed::Skip("no active matches");
    }
    if kind == WatchKind::Digest && state.digest_fingerprint() == Some(digest_fingerprint.as_str())
    {
        return Composed::Skip("unchanged digest");
    }
    // Notifications show only each item's best active offer, in engine order.
    let selected: Vec<MatchItem> = active
        .iter()
        .map(|item| MatchItem {
            deals: item.deals.iter().take(1).cloned().collect(),
            ..item.clone()
        })
        .filter(|item| {
            let Some(best) = item.deals.first() else {
                return false;
            };
            kind == WatchKind::Digest
                || !deal_keys(best).iter().any(|key| {
                    state
                        .notified_price(&format!("{}|{key}", item.rule_id))
                        .is_some_and(|old| old <= best.price_dkk)
                })
        })
        .collect();
    if selected.is_empty() {
        return Composed::Skip("no new or cheaper matches");
    }
    let header_len = utf16_len(&header(kind, 1, time, settings))
        .max(utf16_len(&header(kind, 2, time, settings))) as i64;
    let body_limit = 1490 - header_len - 1;
    let now_ms = parse_date(&now).unwrap_or(time);
    // Bound alert batches by their rendered size; omitted offers remain eligible.
    let (mut remaining, mut chars) = (5, 0i64);
    let mut delivered = Vec::new();
    let mut omitted = Vec::new();
    for item in selected {
        let length = utf16_len(&format!(
            "{}: {}",
            item.name,
            format_deal(&item.deals[0], now_ms)
        )) as i64
            + 1;
        if remaining <= 0 || chars + length > body_limit - 180 {
            omitted.push(item);
            continue;
        }
        remaining -= 1;
        chars += length;
        delivered.push(item);
    }
    if delivered.is_empty() {
        return Composed::TooLong;
    }
    let count: usize = delivered.iter().map(|i| i.deals.len()).sum();
    let misses = match kind {
        WatchKind::Digest => active
            .iter()
            .filter(|i| i.deals.is_empty())
            .map(|i| i.name.clone())
            .collect(),
        WatchKind::Alerts => Vec::new(),
    };
    let title = header(kind, count, time, settings);
    let delivered_fingerprint = fingerprint(delivered.iter().flat_map(|item| {
        let best = &item.deals[0];
        deal_keys(best)
            .into_iter()
            .map(move |key| (format!("{}|{key}", item.rule_id), best.price_dkk))
    }));
    let body = format_matches(
        &MatchResult {
            items: delivered,
            failures: result.failures.clone(),
            now,
        },
        &FormatOptions {
            active_only: true,
            max_length: body_limit.max(0) as usize,
            misses: Some(misses),
            omitted: if kind == WatchKind::Digest {
                omitted.len()
            } else {
                0
            },
        },
    );
    let hash: String = Sha256::digest(delivered_fingerprint.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..24]
        .to_owned();
    let dedup_key = format!(
        "grocery-deals:{}:{date}:{hash}",
        if kind == WatchKind::Digest {
            "digest"
        } else {
            "alert"
        }
    );
    Composed::Ready(Box::new(Prepared {
        notification: Notification {
            kind,
            text: format!("{title}\n{body}"),
            title,
            body,
            dedup_key,
        },
        active,
        omitted,
        fingerprint: digest_fingerprint,
        date,
    }))
}

/// Record a delivered notification: every active offer is known (omitted
/// items' best offers stay eligible), the digest day and fingerprint or
/// the alert count, and alert counts of past days are dropped.
pub fn acknowledge(state: &mut WatchState, prepared: &Prepared) {
    let pending: HashSet<String> = prepared
        .omitted
        .iter()
        .flat_map(|item| {
            deal_keys(&item.deals[0])
                .into_iter()
                .map(move |key| format!("{}|{key}", item.rule_id))
        })
        .collect();
    for item in &prepared.active {
        for deal in &item.deals {
            for key in deal_keys(deal) {
                let id = format!("{}|{key}", item.rule_id);
                if !pending.contains(&id) {
                    state.set_notified(&id, deal.price_dkk, deal.valid_to.as_ref());
                }
            }
        }
    }
    match prepared.notification.kind {
        WatchKind::Digest => {
            state.set_last_digest_date(&prepared.date);
            state.set_digest_fingerprint(&prepared.fingerprint);
        }
        WatchKind::Alerts => state.count_alert(&prepared.date),
    }
    state.keep_alert_counts_from(&prepared.date);
}

/// The outcome of a delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// Not delivered; `reason` is `send failed` or `owner chat unavailable`.
    Failed {
        reason: String,
    },
    /// App notifications are disabled or not configured.
    NotConfigured,
}

#[async_trait]
pub trait Deliver: Send + Sync {
    async fn deliver(&self, notification: &Notification) -> Delivery;
}

/// Where a run reads the shopping-list matches.
#[async_trait]
pub trait MatchSource: Send + Sync {
    async fn list_matches(&self, user_id: &str) -> Result<MatchResult, DealsError>;
}

#[async_trait]
impl MatchSource for DealsEngine {
    async fn list_matches(&self, user_id: &str) -> Result<MatchResult, DealsError> {
        DealsEngine::list_matches(self, user_id).await
    }
}

/// `{ sent, text, reason }` of one run (`text` is empty when nothing was composed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub sent: bool,
    pub text: String,
    pub reason: String,
}

impl RunOutcome {
    fn no(reason: &str) -> Self {
        Self {
            sent: false,
            text: String::new(),
            reason: reason.into(),
        }
    }

    /// What the Node CLI prints: the text, else the reason.
    pub fn display(&self) -> &str {
        if self.text.is_empty() {
            &self.reason
        } else {
            &self.text
        }
    }
}

/// Serialized watcher runs over one state file.
pub struct DealWatch {
    settings: WatchSettings,
    source: Arc<dyn MatchSource>,
    deliver: Arc<dyn Deliver>,
    user_id: String,
    clock: Clock,
    /// Runs are serialized; holds the last real digest attempt (10 min cooldown).
    queue: tokio::sync::Mutex<Option<i64>>,
}

impl std::fmt::Debug for DealWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DealWatch")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl DealWatch {
    pub fn new(
        settings: WatchSettings,
        source: Arc<dyn MatchSource>,
        deliver: Arc<dyn Deliver>,
        user_id: &str,
    ) -> Self {
        Self {
            settings,
            source,
            deliver,
            user_id: user_id.into(),
            clock: crate::system_clock(),
            queue: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn settings(&self) -> &WatchSettings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut WatchSettings {
        &mut self.settings
    }

    /// Persist completed digest checks or successful deliveries.
    pub async fn run(&self, kind: WatchKind, dry_run: bool) -> Result<RunOutcome, DealsError> {
        let mut last_digest_attempt = self.queue.lock().await;
        let s = &self.settings;
        let time = (self.clock)();
        let date = local_date(time, s.zone);
        let mut state = WatchState::load(&s.state_path)?;
        state.prune(time);
        if kind == WatchKind::Digest && state.last_digest_date() == Some(date.as_str()) {
            return Ok(RunOutcome::no("already digested today"));
        }
        if kind == WatchKind::Alerts && state.alert_count(&date) >= s.alert_max_per_day {
            return Ok(RunOutcome::no("daily alert cap"));
        }
        if kind == WatchKind::Digest && !dry_run {
            if last_digest_attempt.is_some_and(|at| time - at < 600_000) {
                return Ok(RunOutcome::no("digest retry cooldown"));
            }
            *last_digest_attempt = Some(time);
        }
        let result = self.source.list_matches(&self.user_id).await?;
        let prepared = match compose(kind, &state, &result, time, s) {
            Composed::Skip(reason) => {
                if kind == WatchKind::Digest && !dry_run {
                    state.set_last_digest_date(&date);
                    state.save(&s.state_path)?;
                }
                return Ok(RunOutcome::no(reason));
            }
            Composed::TooLong => return Ok(RunOutcome::no("offers exceed message limit")),
            Composed::Ready(prepared) => prepared,
        };
        let text = prepared.notification.text.clone();
        if dry_run {
            return Ok(RunOutcome {
                sent: false,
                text,
                reason: "dry run".into(),
            });
        }
        match self.deliver.deliver(&prepared.notification).await {
            Delivery::Delivered => {
                acknowledge(&mut state, &prepared);
                state.save(&s.state_path)?;
                Ok(RunOutcome {
                    sent: true,
                    text,
                    reason: "sent".into(),
                })
            }
            Delivery::Failed { reason } => Ok(RunOutcome {
                sent: false,
                text,
                reason,
            }),
            Delivery::NotConfigured => {
                tracing::warn!("Grocery deal watch: app notifications not configured");
                Ok(RunOutcome::no("app notifications not configured"))
            }
        }
    }

    pub async fn run_digest(&self, dry_run: bool) -> Result<RunOutcome, DealsError> {
        self.run(WatchKind::Digest, dry_run).await
    }

    pub async fn run_alerts(&self, dry_run: bool) -> Result<RunOutcome, DealsError> {
        self.run(WatchKind::Alerts, dry_run).await
    }
}
