//! Watched accounts: what the warmer remembers about each one (docs/instagram.md, "Watching").
//!
//! The state is one JSON file (0600) in the data dir. The diff logic here is pure: it takes what
//! the backend showed on a check and says what is new and what the owner should be told.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const MAX_SAVED_IDS: usize = 5000;
const MAX_KNOWN_POSTS: usize = 60;
const NOTICE_MAX_AGE_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    pub username: String,
    /// Instagram's numeric id, from the profile. A different id under the same name means the
    /// name now belongs to another account.
    pub user_id: Option<String>,
    pub full_name: String,
    /// Instagram CDN URL of the profile picture (signed again on every read).
    pub pic_cdn: Option<String>,
    pub verified: bool,
    pub auto_save: bool,
    pub added_at: i64,
    pub last_check: Option<i64>,
    /// Unix seconds of the next refresh.
    pub next_due: i64,
    pub is_private: Option<bool>,
    /// The username answered 404 after it had answered before.
    pub gone: bool,
    /// The last check's problem, if any (`not_found`, `rate_limited`, `error`).
    pub status: Option<String>,
    /// The first check only records what is there; later ones report what is new.
    pub baselined: bool,
    pub known_posts: Vec<String>,
    pub known_stories: Vec<String>,
    pub unseen_posts: Vec<String>,
    pub unseen_stories: Vec<String>,
    /// Instagram ids of the stories already saved to the Gallery (the dedupe key).
    pub saved_stories: Vec<String>,
    /// UTC day (`YYYY-MM-DD`) and the bytes auto-saved on it.
    pub day: String,
    pub day_bytes: u64,
    pub last_opened: Option<i64>,
}

impl Account {
    pub fn new(username: &str, now: i64) -> Account {
        Account {
            username: username.to_string(),
            auto_save: true,
            added_at: now,
            next_due: now,
            ..Default::default()
        }
    }

    /// Stories and posts the owner has not opened yet.
    pub fn badge(&self) -> usize {
        self.unseen_stories.len() + self.unseen_posts.len()
    }

    pub fn is_saved(&self, ig_id: &str) -> bool {
        self.saved_stories.iter().any(|s| s == ig_id)
    }

    pub fn mark_saved(&mut self, ig_id: &str) {
        if !self.is_saved(ig_id) {
            self.saved_stories.push(ig_id.to_string());
            let excess = self.saved_stories.len().saturating_sub(MAX_SAVED_IDS);
            self.saved_stories.drain(..excess);
        }
    }

    /// Bytes still allowed today.
    pub fn room_today(&self, cap: u64, today: &str) -> u64 {
        if self.day == today {
            cap.saturating_sub(self.day_bytes)
        } else {
            cap
        }
    }

    pub fn add_bytes(&mut self, today: &str, n: u64) {
        if self.day != today {
            self.day = today.to_string();
            self.day_bytes = 0;
        }
        self.day_bytes += n;
    }
}

/// What a check saw.
#[derive(Debug, Clone)]
pub struct Observation {
    pub user_id: Option<String>,
    pub is_private: bool,
    pub full_name: String,
    pub pic_cdn: Option<String>,
    pub verified: bool,
    pub post_ids: Vec<String>,
    /// `None` when stories were not asked for (a private account).
    pub story_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    BecamePrivate,
    BecamePublic,
    DifferentAccount,
    Gone,
    Back,
}

impl Change {
    pub fn text(&self, user: &str) -> String {
        match self {
            Change::BecamePrivate => format!("@{user} is now private"),
            Change::BecamePublic => format!("@{user} is now public"),
            Change::DifferentAccount => format!(
                "@{user} now belongs to a different account (the old one was renamed or deleted)"
            ),
            Change::Gone => format!("@{user} is gone: the account was renamed, deleted or blocked"),
            Change::Back => format!("@{user} is back"),
        }
    }

    fn key(&self) -> &'static str {
        match self {
            Change::BecamePrivate => "private",
            Change::BecamePublic => "public",
            Change::DifferentAccount => "other",
            Change::Gone => "gone",
            Change::Back => "back",
        }
    }
}

/// Fold a check into the account and return what changed about the account itself.
pub fn apply_observation(acc: &mut Account, obs: &Observation, now: i64) -> Vec<Change> {
    let mut changes = Vec::new();
    if acc.gone {
        acc.gone = false;
        changes.push(Change::Back);
    }
    if let (Some(old), Some(new)) = (&acc.user_id, &obs.user_id) {
        if old != new {
            changes.push(Change::DifferentAccount);
            // A different account has different posts and stories: start over.
            acc.baselined = false;
            acc.known_posts.clear();
            acc.known_stories.clear();
            acc.unseen_posts.clear();
            acc.unseen_stories.clear();
        }
    }
    if obs.user_id.is_some() {
        acc.user_id = obs.user_id.clone();
    }
    match acc.is_private {
        Some(old) if old != obs.is_private => changes.push(if obs.is_private {
            Change::BecamePrivate
        } else {
            Change::BecamePublic
        }),
        _ => {}
    }
    acc.is_private = Some(obs.is_private);
    acc.full_name = obs.full_name.clone();
    acc.pic_cdn = obs.pic_cdn.clone();
    acc.verified = obs.verified;
    acc.status = None;
    acc.last_check = Some(now);

    // Posts: the first check is the baseline; after it, an id not seen before is new.
    let mut known_posts = std::mem::take(&mut acc.known_posts);
    if acc.baselined {
        for id in &obs.post_ids {
            if !known_posts.contains(id) {
                if !acc.unseen_posts.contains(id) {
                    acc.unseen_posts.push(id.clone());
                }
                known_posts.insert(0, id.clone());
            }
        }
    } else {
        known_posts = obs.post_ids.clone();
    }
    known_posts.truncate(MAX_KNOWN_POSTS);
    acc.known_posts = known_posts;

    // Stories: same rule; stories that expired are no longer unseen.
    if let Some(current) = &obs.story_ids {
        if acc.baselined {
            for id in current {
                if !acc.known_stories.contains(id) && !acc.unseen_stories.contains(id) {
                    acc.unseen_stories.push(id.clone());
                }
            }
        }
        acc.unseen_stories.retain(|id| current.contains(id));
        acc.known_stories = current.clone();
    }
    acc.baselined = true;
    changes
}

/// When an account should be checked next, given the interval and jitter in force (seconds) and
/// a random `offset` in `-jitter..=jitter`. A `next_due` inside `last_check + interval ± jitter` is
/// kept; one outside it (an older schedule, or a changed setting) becomes
/// `last_check + interval + offset`, so a restart or a deploy never adds a check. Kept as well:
/// an account never checked, and a `rate_limited` one pushed past the window's start by a 429.
pub fn rescheduled_due(acc: &Account, interval: i64, jitter: i64, offset: i64) -> i64 {
    let Some(last) = acc.last_check else {
        return acc.next_due;
    };
    let (start, end) = (last + interval - jitter, last + interval + jitter);
    if (start..=end).contains(&acc.next_due) {
        return acc.next_due;
    }
    if acc.status.as_deref() == Some("rate_limited") && acc.next_due > start {
        return acc.next_due;
    }
    last + interval + offset.clamp(-jitter, jitter)
}

/// The username answered 404. `Some(Gone)` the first time after it had answered before.
pub fn apply_not_found(acc: &mut Account, now: i64) -> Option<Change> {
    acc.last_check = Some(now);
    acc.status = Some("not_found".into());
    if acc.user_id.is_some() && !acc.gone {
        acc.gone = true;
        return Some(Change::Gone);
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notice {
    /// Sent as `dedupKey`: one per detected change, so a retry cannot notify twice.
    pub key: String,
    pub text: String,
    pub route: String,
    pub at: i64,
}

/// The owner's choices from the app (`/v1/instagram/watch-settings`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Hours between two checks of an account; `None`: the service's default.
    pub interval_hours: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    accounts: BTreeMap<String, Account>,
    #[serde(default)]
    notices: Vec<Notice>,
    #[serde(default)]
    settings: Settings,
}

pub struct WatchStore {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl WatchStore {
    pub fn open(path: Option<PathBuf>) -> WatchStore {
        let state = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        WatchStore {
            path,
            state: Mutex::new(state),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut g);
        self.save(&g);
        r
    }

    fn save(&self, state: &State) {
        let Some(path) = &self.path else { return };
        let Ok(bytes) = serde_json::to_vec(state) else {
            return;
        };
        crate::cache::write_private_atomic(path, &bytes);
    }

    pub fn list(&self) -> Vec<Account> {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.accounts.values().cloned().collect()
    }

    pub fn get(&self, user: &str) -> Option<Account> {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.accounts.get(user).cloned()
    }

    /// Add an account (or change its auto-save switch). New accounts are due at once.
    pub fn put(&self, user: &str, auto_save: Option<bool>, now: i64, first_due: i64) -> Account {
        self.with(|s| {
            let acc = s.accounts.entry(user.to_string()).or_insert_with(|| {
                let mut a = Account::new(user, now);
                a.next_due = first_due;
                a
            });
            if let Some(v) = auto_save {
                acc.auto_save = v;
            }
            acc.clone()
        })
    }

    pub fn remove(&self, user: &str) -> bool {
        self.with(|s| s.accounts.remove(user).is_some())
    }

    /// Change one account under the lock; `None` when it is not watched (any more).
    pub fn update<R>(&self, user: &str, f: impl FnOnce(&mut Account) -> R) -> Option<R> {
        self.with(|s| s.accounts.get_mut(user).map(f))
    }

    /// The watched account that is due first, if one is due.
    pub fn next_due(&self, now: i64) -> Option<String> {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.accounts
            .values()
            .filter(|a| a.next_due <= now)
            .min_by_key(|a| a.next_due)
            .map(|a| a.username.clone())
    }

    pub fn settings(&self) -> Settings {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.settings.clone()
    }

    pub fn set_interval_hours(&self, hours: Option<u32>) {
        self.with(|s| s.settings.interval_hours = hours);
    }

    /// [`rescheduled_due`] for every account, saved once; `offset` gives each one its own.
    pub fn reschedule(&self, interval: i64, jitter: i64, mut offset: impl FnMut() -> i64) {
        self.with(|s| {
            for a in s.accounts.values_mut() {
                a.next_due = rescheduled_due(a, interval, jitter, offset());
            }
        });
    }

    pub fn earliest_due(&self) -> Option<i64> {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.accounts.values().map(|a| a.next_due).min()
    }

    pub fn queue_notice(&self, user: &str, change: &Change, now: i64) {
        self.with(|s| {
            s.notices.push(Notice {
                key: format!("instagram:{user}:{}:{now}", change.key()),
                text: change.text(user),
                route: format!("/instagram/{user}"),
                at: now,
            })
        });
    }

    pub fn notices(&self) -> Vec<Notice> {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.notices.clone()
    }

    /// Drop a notice that was delivered, and any that are too old to matter.
    pub fn settle_notice(&self, key: &str, now: i64) {
        self.with(|s| {
            s.notices
                .retain(|n| n.key != key && now - n.at < NOTICE_MAX_AGE_SECS)
        });
    }

    pub fn drop_old_notices(&self, now: i64) {
        self.with(|s| s.notices.retain(|n| now - n.at < NOTICE_MAX_AGE_SECS));
    }

    pub fn badges(&self) -> (usize, BTreeMap<String, usize>) {
        let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let accounts: BTreeMap<String, usize> = g
            .accounts
            .values()
            .filter(|a| a.badge() > 0)
            .map(|a| (a.username.clone(), a.badge()))
            .collect();
        (accounts.values().sum(), accounts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(posts: &[&str], stories: Option<&[&str]>, private: bool, id: &str) -> Observation {
        Observation {
            user_id: Some(id.into()),
            is_private: private,
            full_name: "N".into(),
            pic_cdn: None,
            verified: false,
            post_ids: posts.iter().map(|s| s.to_string()).collect(),
            story_ids: stories.map(|s| s.iter().map(|x| x.to_string()).collect()),
        }
    }

    #[test]
    fn the_first_check_is_a_baseline_then_new_ids_are_unseen() {
        let mut a = Account::new("x", 0);
        assert!(
            apply_observation(&mut a, &obs(&["p1", "p2"], Some(&["s1"]), false, "1"), 10)
                .is_empty()
        );
        assert_eq!(a.badge(), 0, "what was already there is not new");
        apply_observation(
            &mut a,
            &obs(&["p3", "p1", "p2"], Some(&["s1", "s2"]), false, "1"),
            20,
        );
        assert_eq!(a.unseen_posts, vec!["p3"]);
        assert_eq!(a.unseen_stories, vec!["s2"]);
        assert_eq!(a.badge(), 2);
        // Seeing it again does not count it twice.
        apply_observation(
            &mut a,
            &obs(&["p3", "p1", "p2"], Some(&["s1", "s2"]), false, "1"),
            30,
        );
        assert_eq!(a.badge(), 2);
        // A story that expired is no longer unseen.
        apply_observation(
            &mut a,
            &obs(&["p3", "p1", "p2"], Some(&["s1"]), false, "1"),
            40,
        );
        assert_eq!(a.unseen_stories, Vec::<String>::new());
        assert_eq!(a.badge(), 1);
    }

    #[test]
    fn a_private_flip_is_reported_once_per_flip() {
        let mut a = Account::new("x", 0);
        assert!(apply_observation(&mut a, &obs(&[], None, false, "1"), 1).is_empty());
        assert_eq!(
            apply_observation(&mut a, &obs(&[], None, true, "1"), 2),
            vec![Change::BecamePrivate]
        );
        assert!(apply_observation(&mut a, &obs(&[], None, true, "1"), 3).is_empty());
        assert_eq!(
            apply_observation(&mut a, &obs(&[], None, false, "1"), 4),
            vec![Change::BecamePublic]
        );
    }

    #[test]
    fn gone_then_back_and_a_taken_over_name() {
        let mut a = Account::new("x", 0);
        assert_eq!(
            apply_not_found(&mut a, 1),
            None,
            "never seen: nothing to report"
        );
        apply_observation(&mut a, &obs(&["p"], None, false, "1"), 2);
        assert_eq!(apply_not_found(&mut a, 3), Some(Change::Gone));
        assert_eq!(apply_not_found(&mut a, 4), None, "reported once");
        assert_eq!(
            apply_observation(&mut a, &obs(&["p"], None, false, "1"), 5),
            vec![Change::Back]
        );
        assert_eq!(
            apply_observation(&mut a, &obs(&["q"], None, false, "2"), 6),
            vec![Change::DifferentAccount]
        );
        assert_eq!(a.user_id.as_deref(), Some("2"));
        assert_eq!(
            a.badge(),
            0,
            "a different account starts from a new baseline"
        );
    }

    #[test]
    fn the_saved_ids_and_the_daily_cap() {
        let mut a = Account::new("x", 0);
        a.mark_saved("s1");
        a.mark_saved("s1");
        assert_eq!(a.saved_stories, vec!["s1"]);
        assert_eq!(a.room_today(100, "2026-10-28"), 100);
        a.add_bytes("2026-10-28", 70);
        assert_eq!(a.room_today(100, "2026-10-28"), 30);
        assert_eq!(a.room_today(100, "2026-10-29"), 100, "a new day resets");
        a.add_bytes("2026-10-29", 5);
        assert_eq!(a.day_bytes, 5);
        for i in 0..(MAX_SAVED_IDS + 10) {
            a.mark_saved(&format!("id{i}"));
        }
        assert_eq!(a.saved_stories.len(), MAX_SAVED_IDS);
    }

    const H: i64 = 3600;
    const DAY: i64 = 23 * H;
    const J: i64 = 30 * 60;

    fn checked(last: i64, next_due: i64) -> Account {
        Account {
            last_check: Some(last),
            next_due,
            ..Account::new("x", 0)
        }
    }

    #[test]
    fn a_next_check_inside_the_window_is_kept() {
        let t = 1_000_000;
        for due in [t + DAY - J, t + DAY, t + DAY + J, t + DAY + 7] {
            assert_eq!(rescheduled_due(&checked(t, due), DAY, J, 0), due);
            assert_eq!(rescheduled_due(&checked(t, due), DAY, J, -J), due);
        }
    }

    #[test]
    fn the_old_twenty_to_forty_minute_schedule_moves_to_a_day_after_the_last_check() {
        let t = 1_000_000;
        for due in [t + 20 * 60, t + 40 * 60, t - 5] {
            assert_eq!(rescheduled_due(&checked(t, due), DAY, J, 0), t + DAY);
            assert_eq!(rescheduled_due(&checked(t, due), DAY, J, 99), t + DAY + 99);
        }
        // A shorter setting brings a far-off check closer.
        assert_eq!(
            rescheduled_due(&checked(t, t + DAY), 6 * H, J, 0),
            t + 6 * H
        );
    }

    #[test]
    fn the_random_offset_stays_inside_the_window_at_both_extremes() {
        let t = 1_000_000;
        let a = checked(t, t + 60);
        assert_eq!(rescheduled_due(&a, DAY, J, -J), t + DAY - J);
        assert_eq!(rescheduled_due(&a, DAY, J, J), t + DAY + J);
        // An offset beyond the jitter is held at its edge.
        assert_eq!(rescheduled_due(&a, DAY, J, -10 * J), t + DAY - J);
        assert_eq!(rescheduled_due(&a, DAY, J, 10 * J), t + DAY + J);
    }

    #[test]
    fn an_account_never_checked_keeps_its_first_check() {
        let a = Account {
            next_due: 1234,
            ..Account::new("x", 1000)
        };
        assert_eq!(rescheduled_due(&a, DAY, J, J), 1234);
    }

    #[test]
    fn a_rate_limited_account_pushed_out_by_a_429_keeps_its_next_check() {
        let t = 1_000_000;
        let mut a = checked(t, t + DAY + 2 * H);
        a.status = Some("rate_limited".into());
        assert_eq!(rescheduled_due(&a, DAY, J, 0), t + DAY + 2 * H);
        // One due before the window is moved like any other.
        a.next_due = t + 30 * 60;
        assert_eq!(rescheduled_due(&a, DAY, J, 0), t + DAY);
    }

    #[test]
    fn the_settings_persist_and_an_old_file_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watch.json");
        std::fs::write(
            &p,
            r#"{"accounts":{"a":{"username":"a","nextDue":5}},"notices":[]}"#,
        )
        .unwrap();
        let s = WatchStore::open(Some(p.clone()));
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.settings().interval_hours, None);
        s.set_interval_hours(Some(12));
        assert_eq!(
            WatchStore::open(Some(p)).settings().interval_hours,
            Some(12)
        );
    }

    #[test]
    fn the_store_persists_privately_and_counts_badges() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("watch.json");
        let s = WatchStore::open(Some(p.clone()));
        s.put("a", None, 100, 100);
        s.put("b", Some(false), 100, 200);
        assert!(s.get("a").unwrap().auto_save);
        assert!(!s.get("b").unwrap().auto_save);
        assert_eq!(s.next_due(150).as_deref(), Some("a"));
        s.update("a", |a| a.unseen_posts.push("p".into()));
        assert_eq!(s.badges(), (1, BTreeMap::from([("a".to_string(), 1)])));
        s.queue_notice("a", &Change::BecamePublic, 5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let again = WatchStore::open(Some(p));
        assert_eq!(again.list().len(), 2);
        assert_eq!(again.notices().len(), 1);
        assert_eq!(again.notices()[0].text, "@a is now public");
        again.settle_notice(&again.notices()[0].key.clone(), 6);
        assert!(again.notices().is_empty());
        assert!(again.remove("a"));
        assert!(!again.remove("a"));
    }
}
