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
//!
//! Three changes from Node, found by the deal watch model (SecretaryIOS
//! `docs/verification.md` §14, bugs DW1-DW3):
//!
//! - F1: the composed message is written to the file as `pending` (with its
//!   dedup key, kind, date and what it acknowledges) before it is sent. A run
//!   that finds one re-sends exactly that message first (same dedup key, so
//!   secretary-server answers `duplicate` if it already has it), acknowledges
//!   it, then continues. The field is absent when nothing is pending, so a
//!   file without it is the old format. A pending message never blocks the
//!   watch: it is dropped (recorded as handled without sending, one log line)
//!   when its local date is not today, when the server refuses it with a
//!   non-retryable status ([`Delivery::Rejected`]), or after
//!   [`MAX_SEND_ATTEMPTS`] attempts. Recording it keeps a copy the server may
//!   already have from being repeated; its offers are skipped by that decision.
//! - F2: only the delivered best offers (and their `relatedKeys`) are
//!   acknowledged, not every active offer.
//! - F3: a notified price never goes up ([`WatchState::set_notified`] keeps
//!   the lower price).

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

/// F1: send attempts on one pending message before it is dropped.
pub const MAX_SEND_ATTEMPTS: u32 = 5;

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
        write_atomic(path, self.to_json().as_bytes())
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

    /// Record that `id` reached the owner at `price` (F3: keeping a lower
    /// price already recorded, so a price that rises and falls back is not
    /// new again). `validTo` is the latest offer's.
    pub fn set_notified(&mut self, id: &str, price: f64, valid_to: Option<&Value>) {
        self.set_notified_entry(id, &notified_entry(price, valid_to.map(to_js)));
    }

    /// `set_notified` with a `{price, validTo}` entry as `notified_entry` built it.
    fn set_notified_entry(&mut self, id: &str, entry: &JsValue) {
        let mut entry = entry.clone();
        let price = entry.get("price").map_or(f64::NAN, JsValue::to_number);
        if let (Some(old), JsValue::Object(fields)) = (self.notified_price(id), &mut entry) {
            if old < price {
                set_entry(fields, "price", JsValue::Number(old));
            }
        }
        set_entry(object_mut(&mut self.root, "notified"), id, entry);
    }

    /// F1: the message being sent, if a run wrote one and did not acknowledge it.
    pub fn pending(&self) -> Option<&JsValue> {
        self.get("pending").filter(|p| !matches!(p, JsValue::Null))
    }

    /// F1: set or clear `pending`. Cleared, the key is removed, so the file is
    /// the format Node wrote.
    pub fn set_pending(&mut self, pending: Option<JsValue>) {
        match pending {
            Some(p) => set_entry(&mut self.root, "pending", p),
            None => self.root.retain(|(k, _)| k != "pending"),
        }
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

/// Atomic write (`<file>.<pid>.tmp`, mode 0600, rename), like Node.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DealsError> {
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
        file.write_all(bytes).map_err(err)?;
    }
    std::fs::rename(&tmp, path).map_err(err)
}

/// A `notified` entry: `{price, validTo?}`.
fn notified_entry(price: f64, valid_to: Option<JsValue>) -> JsValue {
    let mut entry = vec![("price".to_owned(), JsValue::Number(price))];
    if let Some(v) = valid_to {
        entry.push(("validTo".into(), v));
    }
    JsValue::Object(entry)
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
    /// F2: `<ruleId>|<key>` → `{price, validTo}` of each delivered best offer
    /// and its related keys (what [`acknowledge`] records).
    acks: Vec<(String, JsValue)>,
    fingerprint: String,
    date: String,
}

impl Prepared {
    /// F1: the `pending` field of the state file.
    fn to_pending(&self) -> JsValue {
        let n = &self.notification;
        JsValue::Object(vec![
            ("kind".into(), JsValue::String(kind_name(n.kind).into())),
            ("date".into(), JsValue::String(self.date.clone())),
            ("dedupKey".into(), JsValue::String(n.dedup_key.clone())),
            ("title".into(), JsValue::String(n.title.clone())),
            ("body".into(), JsValue::String(n.body.clone())),
            (
                "fingerprint".into(),
                JsValue::String(self.fingerprint.clone()),
            ),
            ("notified".into(), JsValue::Object(self.acks.clone())),
            ("attempts".into(), JsValue::Number(1.0)),
        ])
    }

    /// The message a `pending` field holds (`None` when it is malformed).
    fn from_pending(p: &JsValue) -> Option<Self> {
        let text = |key: &str| p.get(key).and_then(JsValue::as_str).map(str::to_owned);
        let kind = match p.get("kind").and_then(JsValue::as_str)? {
            "digest" => WatchKind::Digest,
            "alert" => WatchKind::Alerts,
            _ => return None,
        };
        let (title, body) = (text("title")?, text("body")?);
        let acks = match p.get("notified")? {
            JsValue::Object(entries) => entries.clone(),
            _ => return None,
        };
        Some(Self {
            notification: Notification {
                kind,
                text: format!("{title}\n{body}"),
                title,
                body,
                dedup_key: text("dedupKey")?,
            },
            acks,
            fingerprint: text("fingerprint")?,
            date: text("date")?,
        })
    }
}

/// The send attempts a `pending` field records (1 when it has none).
fn pending_attempts(p: &JsValue) -> u32 {
    let n = p.get("attempts").map_or(1.0, JsValue::to_number);
    if n.is_finite() && n >= 0.0 {
        n as u32
    } else {
        1
    }
}

fn with_attempts(p: &JsValue, attempts: u32) -> JsValue {
    let mut p = p.clone();
    if let JsValue::Object(fields) = &mut p {
        set_entry(fields, "attempts", JsValue::Number(f64::from(attempts)));
    }
    p
}

/// Drop a pending (or just refused) message: record it as handled without
/// sending it, so a copy the server may have is never repeated.
fn drop_pending(state: &mut WatchState, pending: &Prepared, why: &str) {
    acknowledge(state, pending);
    tracing::warn!(
        kind = kind_name(pending.notification.kind),
        date = %pending.date,
        dedup_key = %pending.notification.dedup_key,
        why,
        "grocery deal watch: dropped a pending notification without sending it"
    );
}

fn kind_name(kind: WatchKind) -> &'static str {
    match kind {
        WatchKind::Digest => "digest",
        WatchKind::Alerts => "alert",
    }
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
    // F2: acknowledge what the message shows, each best offer under all its keys.
    let mut acks: Vec<(String, JsValue)> = Vec::new();
    for item in &delivered {
        let best = &item.deals[0];
        for key in deal_keys(best) {
            let id = format!("{}|{key}", item.rule_id);
            let entry = notified_entry(best.price_dkk, best.valid_to.as_ref().map(to_js));
            match acks.iter_mut().find(|(k, _)| *k == id) {
                Some(slot) => {
                    let old = slot.1.get("price").map_or(f64::NAN, JsValue::to_number);
                    if old.is_nan() || old >= best.price_dkk {
                        slot.1 = entry;
                    }
                }
                None => acks.push((id, entry)),
            }
        }
    }
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
    let dedup_key = format!("grocery-deals:{}:{date}:{hash}", kind_name(kind));
    Composed::Ready(Box::new(Prepared {
        notification: Notification {
            kind,
            text: format!("{title}\n{body}"),
            title,
            body,
            dedup_key,
        },
        acks,
        fingerprint: digest_fingerprint,
        date,
    }))
}

/// Record a delivered notification: its best offers are known (F2: not the
/// alternatives it did not show; omitted items stay eligible), the digest day
/// and fingerprint or the alert count, alert counts of past days are dropped,
/// and nothing is pending any more (F1).
pub fn acknowledge(state: &mut WatchState, prepared: &Prepared) {
    for (id, entry) in &prepared.acks {
        state.set_notified_entry(id, entry);
    }
    match prepared.notification.kind {
        WatchKind::Digest => {
            state.set_last_digest_date(&prepared.date);
            state.set_digest_fingerprint(&prepared.fingerprint);
        }
        WatchKind::Alerts => state.count_alert(&prepared.date),
    }
    state.keep_alert_counts_from(&prepared.date);
    state.set_pending(None);
}

/// The outcome of a delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// Not delivered, may be retried (network error, timeout, 5xx, 429); the
    /// server may have it. `reason` is `send failed` or `owner chat unavailable`.
    Failed {
        reason: String,
    },
    /// Refused for good (400, 401, 403, 404, 413, 422): retrying the same
    /// message cannot succeed.
    Rejected {
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

    /// The watch clock (epoch milliseconds).
    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    pub fn settings_mut(&mut self) -> &mut WatchSettings {
        &mut self.settings
    }

    /// Persist completed digest checks or successful deliveries.
    ///
    /// F1: a composed message is saved as `pending` before it is sent, and a
    /// run that finds one re-sends it (same dedup key) before anything else:
    /// delivered, it is acknowledged and the run continues; not delivered, it
    /// stays pending and the run ends.
    pub async fn run(&self, kind: WatchKind, dry_run: bool) -> Result<RunOutcome, DealsError> {
        let mut last_digest_attempt = self.queue.lock().await;
        let s = &self.settings;
        let time = (self.clock)();
        let date = local_date(time, s.zone);
        let mut state = WatchState::load(&s.state_path)?;
        state.prune(time);
        let cooling = |at: Option<i64>| at.is_some_and(|at| time - at < 600_000);
        // The digest retry cooldown counts this run once, also when it re-sent.
        let mut attempted = false;
        let mut resent = None;
        if let (false, Some(p)) = (dry_run, state.pending().cloned()) {
            match Prepared::from_pending(&p) {
                Some(pending) => {
                    let attempts = pending_attempts(&p);
                    let stale = pending.date != date;
                    if stale || attempts >= MAX_SEND_ATTEMPTS {
                        let why = if stale {
                            "stale: composed on an earlier day"
                        } else {
                            "too many failed attempts"
                        };
                        drop_pending(&mut state, &pending, why);
                        state.save(&s.state_path)?;
                    } else {
                        if kind == WatchKind::Digest {
                            if cooling(*last_digest_attempt) {
                                return Ok(RunOutcome::no("digest retry cooldown"));
                            }
                            *last_digest_attempt = Some(time);
                            attempted = true;
                        }
                        state.set_pending(Some(with_attempts(&p, attempts + 1)));
                        state.save(&s.state_path)?;
                        let text = pending.notification.text.clone();
                        match self.deliver.deliver(&pending.notification).await {
                            Delivery::Delivered => {
                                acknowledge(&mut state, &pending);
                                state.save(&s.state_path)?;
                                tracing::info!(
                                    kind = kind_name(pending.notification.kind),
                                    "grocery deal watch: pending notification re-sent"
                                );
                                resent = Some(RunOutcome {
                                    sent: true,
                                    text,
                                    reason: "sent".into(),
                                });
                            }
                            // It may have reached the server: keep it for the next run.
                            Delivery::Failed { reason } => {
                                return Ok(RunOutcome {
                                    sent: false,
                                    text,
                                    reason,
                                })
                            }
                            Delivery::Rejected { reason } => {
                                drop_pending(&mut state, &pending, &reason);
                                state.save(&s.state_path)?;
                            }
                            // Never posted: not an attempt.
                            Delivery::NotConfigured => {
                                state.set_pending(Some(p.clone()));
                                state.save(&s.state_path)?;
                                tracing::warn!(
                                    "Grocery deal watch: app notifications not configured"
                                );
                                return Ok(RunOutcome::no("app notifications not configured"));
                            }
                        }
                    }
                }
                None => {
                    tracing::warn!("grocery deal watch: dropping a malformed pending notification");
                    state.set_pending(None);
                    state.save(&s.state_path)?;
                }
            }
        }
        // After a re-send, a run with nothing more to send reports the re-send.
        let ended = |outcome: RunOutcome| Ok(resent.clone().unwrap_or(outcome));
        if kind == WatchKind::Digest && state.last_digest_date() == Some(date.as_str()) {
            return ended(RunOutcome::no("already digested today"));
        }
        if kind == WatchKind::Alerts && state.alert_count(&date) >= s.alert_max_per_day {
            return ended(RunOutcome::no("daily alert cap"));
        }
        if kind == WatchKind::Digest && !dry_run && !attempted {
            if cooling(*last_digest_attempt) {
                return ended(RunOutcome::no("digest retry cooldown"));
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
                return ended(RunOutcome::no(reason));
            }
            Composed::TooLong => return ended(RunOutcome::no("offers exceed message limit")),
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
        // F1: the file knows the message before the server can have it.
        let before = match std::fs::read(&s.state_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(DealsError::State(e.to_string())),
        };
        state.set_pending(Some(prepared.to_pending()));
        state.save(&s.state_path)?;
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
            // It may have reached the server: the next run re-sends it.
            Delivery::Failed { reason } => Ok(RunOutcome {
                sent: false,
                text,
                reason,
            }),
            // Refused for good: dropped, not retried.
            Delivery::Rejected { reason } => {
                drop_pending(&mut state, &prepared, &reason);
                state.save(&s.state_path)?;
                Ok(RunOutcome {
                    sent: false,
                    text,
                    reason,
                })
            }
            // Never posted: put the file back as it was.
            Delivery::NotConfigured => {
                match before {
                    Some(bytes) => write_atomic(&s.state_path, &bytes)?,
                    None => std::fs::remove_file(&s.state_path)
                        .map_err(|e| DealsError::State(e.to_string()))?,
                }
                tracing::warn!("Grocery deal watch: app notifications not configured");
                ended(RunOutcome::no("app notifications not configured"))
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
