//! The plugin's logic: ask the StoryView backend (through `instarust-core`'s URL builders and
//! parsers), keep what comes back in the profile store, answer pages from the store with signed
//! media URLs while a refresh behind them adds what is new, download media to the disk cache in the
//! background, remember opened profiles, and save items into lucina-media.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

use instarust_core::media::{parse_highlights, parse_media_page};
use instarust_core::profile::{parse_profile_bundle, ProfileBundle};
use instarust_core::url as core_url;
use instarust_core::{HighlightSummary, MediaItem, MediaPage, UserProfile, BASE_URL};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::archive::{Archive, Saved};
use crate::cache::{ctype_for_ext, ext_for_ctype, DiskCache};
use crate::media::{cache_key, inner_cdn_url, is_video_url, SignError, Signer};
use crate::recent::{Recent, RecentEntry};
use crate::store::{Feed, FeedKind, ProfileDoc, Store};
use crate::upstream::{Upstream, UpstreamError};
use crate::watch::{apply_not_found, apply_observation, Account, Change, Observation, WatchStore};

/// The intervals the app offers, in hours.
pub const INTERVAL_CHOICES: [u32; 3] = [6, 12, 23];

const TTL_PROFILE: Duration = Duration::from_secs(300);
const TTL_POSTS: Duration = Duration::from_secs(300);
const TTL_STORIES: Duration = Duration::from_secs(120);
const TTL_HIGHLIGHTS: Duration = Duration::from_secs(600);
const TTL_EMPTY_TRAY: Duration = Duration::from_secs(60);
const TTL_RESOLVE: Duration = Duration::from_secs(600);
const TTL_FIRST_PAGE: Duration = Duration::from_secs(24 * 3600);
/// How old a cached answer may be and still be shown while a fresh one is fetched behind it.
/// More than the warmer's 23 h, so a watched account always opens at once. (Instagram's CDN URLs
/// in it stay valid for days; media already fetched is served from the disk cache anyway.)
const STALE_MAX: Duration = Duration::from_secs(2 * 24 * 3600);
/// Items in one answer from the store; the next ones are `d<offset>` cursors.
const DISK_PAGE: usize = 48;
/// A refresh that failed is not tried again sooner.
const REFRESH_RETRY_SECS: i64 = 60;
/// A refresh stops at a known item, at the latest after this many pages (the rest is a gap the
/// owner's "more" or the backfill's next pass does not fill: the feed is newest first anyway).
const MAX_REFRESH_PAGES: usize = 5;
/// A download that failed for good (its CDN link has expired) is left alone this long; a refresh
/// meanwhile brings a fresh link only for an item still up.
const MIRROR_RETRY_SECS: i64 = 6 * 3600;
/// What a first fill costs the backfill's daily budget: bundle, stories, reels, tray.
const SEED_CALLS: usize = 4;
const STORY_LIFE_SECS: i64 = 24 * 3600;
/// `fetched_at` of a first answer that may have come from an older JSON copy: asked, and out of
/// date, so the refresh behind it runs (it costs no backend call if a fresh answer is in the cache).
const ASK_AGAIN: i64 = 1;

/// Per cache key, the lock a backend fetch holds ([`Service::fetch_shared`]).
type Flights = Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

/// Whose fetch it is: the owner waits for it; a refresh behind a cached copy yields to the owner.
#[derive(Clone, Copy)]
enum Priority {
    Owner,
    Low,
}

fn keep_any(_: &str) -> bool {
    true
}

/// Empty trays are unknown (the backend answers `[]` for accounts that have highlights): never
/// cached, so the next request asks again.
fn nonempty_tray(text: &str) -> bool {
    parse_highlights(text).is_some_and(|hs| !hs.is_empty())
}

const MAX_SAVE_ITEMS: usize = 50;
const MAX_RESIGN: usize = 500;

/// The offset in a `d<offset>` cursor (a page of the store, not of the backend).
fn disk_offset(cursor: &str) -> Option<usize> {
    cursor.strip_prefix('d')?.parse().ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceError {
    NotFound,
    BadRequest(&'static str),
    /// Seconds until the backend may be asked again.
    RateLimited(u64),
    /// The Instagram link behind a media URL has expired; the client should reload the page.
    Gone,
    Forbidden,
    Upstream,
}

impl From<UpstreamError> for ServiceError {
    fn from(e: UpstreamError) -> ServiceError {
        match e {
            UpstreamError::RateLimited(s) => ServiceError::RateLimited(s),
            UpstreamError::Status(404) => ServiceError::NotFound,
            UpstreamError::Status(400 | 422) => ServiceError::BadRequest("rejected_by_upstream"),
            UpstreamError::Forbidden => ServiceError::Forbidden,
            _ => ServiceError::Upstream,
        }
    }
}

/// An error from fetching media: expired CDN links are `Gone`.
fn media_error(e: UpstreamError) -> ServiceError {
    match e {
        UpstreamError::Status(403 | 404 | 410) => ServiceError::Gone,
        other => other.into(),
    }
}

pub struct ServiceConfig {
    pub spool_dir: PathBuf,
    /// lucina-media's loopback base, e.g. `http://127.0.0.1:8830`.
    pub ingest_url: String,
    /// Where the watch list and the archive index live.
    pub state_dir: PathBuf,
    /// secretary-server's base URL and the file holding Lucina's `svc_…` token, for the owner's
    /// notifications (`POST /v1/notify`). Both unset: changes are logged, not pushed.
    pub notify_url: Option<String>,
    pub notify_token_file: Option<PathBuf>,
    pub watch: WatchConfig,
    pub mirror: MirrorConfig,
}

/// The slow background download of what opened and watched profiles show.
#[derive(Debug, Clone)]
pub struct MirrorConfig {
    /// Bytes downloaded per UTC day.
    pub daily_max_bytes: u64,
    /// Rest between two downloads (on top of the CDN spacing).
    pub interval: Duration,
    /// Rest between two older-pages calls to the backend (on top of its spacing).
    pub backfill_interval: Duration,
    /// Older-pages calls to the backend per UTC day, over all accounts.
    pub backfill_daily_calls: u32,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        MirrorConfig {
            daily_max_bytes: 20 * 1024 * 1024 * 1024,
            interval: Duration::from_secs(1),
            backfill_interval: Duration::from_secs(20),
            backfill_daily_calls: 300,
        }
    }
}

impl ServiceConfig {
    pub fn new(spool_dir: PathBuf, ingest_url: String, state_dir: PathBuf) -> ServiceConfig {
        ServiceConfig {
            spool_dir,
            ingest_url,
            state_dir,
            notify_url: None,
            notify_token_file: None,
            watch: WatchConfig::default(),
            mirror: MirrorConfig::default(),
        }
    }
}

/// What the background download has to do and has done today.
#[derive(Default)]
struct MirrorQueue {
    /// CDN URL and the watched account it is for.
    images: VecDeque<(String, String)>,
    videos: VecDeque<(String, String)>,
    queued: HashSet<String>,
    /// Keys not to try again before this time.
    skip_until: HashMap<String, i64>,
    day: String,
    day_bytes: u64,
    backfill_day: String,
    backfill_calls: u32,
    /// Files fetched and bytes since the last log line.
    done: u64,
    done_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// One file is on disk.
    Fetched,
    /// One file could not be fetched.
    Failed,
    /// A queued file needed no fetch.
    Skipped,
    /// Nothing to do.
    Idle,
    /// The owner is waiting for media: the background gives way.
    Busy,
    /// The day's budget is spent, the cache is full, or the CDN said 429.
    Paused,
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// The warmer refreshes watched accounts (and auto-saves their new stories).
    pub enabled: bool,
    /// Each account is refreshed every `interval` ± a random `jitter` (23 h ± 30 min by default:
    /// stories last 24 h). The owner's choice in the app replaces `interval`.
    pub interval: Duration,
    pub jitter: Duration,
    /// How long the warmer stays quiet after the backend said 429 (at least).
    pub pause: Duration,
    /// Auto-saved bytes per account per UTC day.
    pub daily_cap_bytes: u64,
    /// A newly watched account is first checked after 2 s plus up to this long.
    pub first_check_jitter: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            enabled: true,
            interval: Duration::from_secs(23 * 3600),
            jitter: Duration::from_secs(30 * 60),
            pause: Duration::from_secs(30 * 60),
            daily_cap_bytes: 500 * 1024 * 1024,
            first_check_jitter: Duration::from_secs(8),
        }
    }
}

/// Pushes the owner's notifications through secretary-server (the way lucina-meshy does):
/// `POST /v1/notify` with Lucina's service token, read from a file on every send.
struct Notifier {
    url: Option<String>,
    token_file: Option<PathBuf>,
    http: reqwest::Client,
}

enum Sent {
    Yes,
    NotConfigured,
    Failed,
}

impl Notifier {
    async fn send(&self, n: &crate::watch::Notice) -> Sent {
        let (Some(url), Some(file)) = (&self.url, &self.token_file) else {
            return Sent::NotConfigured;
        };
        let Some(token) = std::fs::read_to_string(file)
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| t.starts_with("svc_"))
        else {
            return Sent::NotConfigured;
        };
        let r = self
            .http
            .post(format!("{}/v1/notify", url.trim_end_matches('/')))
            .bearer_auth(token)
            .json(&json!({
                "category": "lucina_message",
                "title": "Instagram",
                "body": n.text,
                "route": n.route,
                "dedupKey": n.key,
            }))
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => Sent::Yes,
            Ok(resp) => {
                tracing::warn!(status = resp.status().as_u16(), "instagram notify refused");
                Sent::Failed
            }
            Err(_) => {
                tracing::warn!("instagram notify: secretary-server unreachable");
                Sent::Failed
            }
        }
    }
}

pub struct Service {
    pub up: Arc<Upstream>,
    pub cache: Arc<DiskCache>,
    pub recent: Recent,
    pub signer: Signer,
    pub watch: WatchStore,
    pub archive: Archive,
    /// Profiles and feeds as last seen: what pages are answered from.
    pub store: Store,
    cfg: ServiceConfig,
    http: reqwest::Client,
    notifier: Notifier,
    inflight: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The same for backend JSON: one fetch per cache key at a time ([`Service::fetch_shared`]).
    json_flights: Flights,
    /// The warmer does nothing until then (a 429 sets it).
    warmer_paused_until: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Wakes the warmer (a newly watched account).
    pub wake: Notify,
    /// Accounts and highlights being refreshed, and when each was last tried.
    refreshes: std::sync::Mutex<(HashSet<String>, HashMap<String, i64>)>,
    mirror: std::sync::Mutex<MirrorQueue>,
    /// Wakes the background download (something was queued).
    mirror_wake: Notify,
    /// Owner requests waiting for a file that is not on disk yet.
    owner_media: AtomicUsize,
}

/// Counts an owner request while it waits for a download.
struct OwnerMedia<'a>(&'a AtomicUsize);

impl<'a> OwnerMedia<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::SeqCst);
        OwnerMedia(n)
    }
}

impl Drop for OwnerMedia<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct MediaFile {
    pub path: PathBuf,
    pub ctype: &'static str,
    pub len: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveItem {
    /// A signed `/v1/instagram/s/…` URL from one of this service's own responses.
    pub url: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub username: String,
    #[serde(default)]
    pub taken_at: Option<Value>,
    #[serde(default)]
    pub caption: Option<String>,
}

fn hash_key(s: &str) -> String {
    hex::encode(&Sha256::digest(s.as_bytes())[..16])
}

/// Lower-case, without the `@`; letters, digits, `.` and `_` only (Instagram's alphabet).
pub fn normalize_username(raw: &str) -> Option<String> {
    let u = raw.trim().trim_start_matches('@').to_ascii_lowercase();
    let ok = !u.is_empty()
        && u.len() <= 30
        && u.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_');
    ok.then_some(u)
}

fn valid_cursor(c: &Option<String>) -> Result<Option<&str>, ServiceError> {
    match c.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(c) if c.len() <= 1024 && c.bytes().all(|b| (0x21..0x7f).contains(&b)) => Ok(Some(c)),
        Some(_) => Err(ServiceError::BadRequest("cursor")),
    }
}

fn rel(core: String) -> String {
    core.strip_prefix(BASE_URL)
        .map(str::to_string)
        .unwrap_or(core)
}

fn users_path(username: &str, suffix: &str, cursor: Option<&str>) -> String {
    let mut p = format!(
        "{}{}{suffix}",
        instarust_core::paths::USERS,
        core_url::dart_query_encode(username)
    );
    if let Some(c) = cursor {
        p.push_str("?cursor=");
        p.push_str(&core_url::dart_query_encode(c));
    }
    p
}

/// `https://www.instagram.com/<user>/p/<code>/`, `/reel/<code>/`, `/reels/<code>/`, `/tv/<code>/`.
fn valid_post_url(s: &str) -> bool {
    let Ok(u) = Url::parse(s.trim()) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host_ok = matches!(
        u.host_str().map(str::to_ascii_lowercase).as_deref(),
        Some("instagram.com" | "www.instagram.com" | "m.instagram.com" | "instagr.am")
    );
    if !host_ok {
        return false;
    }
    let segs: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
    segs.windows(2).any(|w| {
        matches!(w[0], "p" | "reel" | "reels" | "tv")
            && !w[1].is_empty()
            && w[1].len() <= 40
            && w[1]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn shortcode_of(id: &str) -> bool {
    !id.is_empty()
        && !id.bytes().all(|b| b.is_ascii_digit())
        && id.len() <= 20
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Service {
    pub fn new(
        up: Upstream,
        cache: DiskCache,
        recent: Recent,
        signer: Signer,
        cfg: ServiceConfig,
    ) -> anyhow::Result<Service> {
        std::fs::create_dir_all(&cfg.spool_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&cfg.spool_dir, std::fs::Permissions::from_mode(0o700));
        }
        std::fs::create_dir_all(&cfg.state_dir)?;
        // A save cut short by a stop leaves its spool copy behind; nothing in the spool is
        // needed after a restart.
        for e in std::fs::read_dir(&cfg.spool_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            if e.metadata().is_ok_and(|m| m.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let svc = Service {
            up: Arc::new(up),
            store: Store::open(cache.root().join("profiles"))?,
            cache: Arc::new(cache),
            recent,
            signer,
            watch: WatchStore::open(Some(cfg.state_dir.join("watch.json"))),
            archive: Archive::open(Some(cfg.state_dir.join("archive.json"))),
            notifier: Notifier {
                url: cfg.notify_url.clone(),
                token_file: cfg.notify_token_file.clone(),
                http: http.clone(),
            },
            cfg,
            http,
            inflight: std::sync::Mutex::new(HashMap::new()),
            json_flights: Flights::default(),
            warmer_paused_until: std::sync::Mutex::new(None),
            wake: Notify::new(),
            refreshes: Default::default(),
            mirror: Default::default(),
            mirror_wake: Notify::new(),
            owner_media: AtomicUsize::new(0),
        };
        // Before the warmer's first pass: what was checked just before a restart is not checked
        // again now.
        svc.reschedule_watched();
        svc.pin_kept_media();
        Ok(svc)
    }

    // ---- JSON building ----

    fn sign_opt(&self, backend_url: &str) -> Value {
        match inner_cdn_url(backend_url) {
            Some(cdn) => Value::String(self.signer.url_for(&cdn)),
            None => Value::Null,
        }
    }

    fn profile_json(&self, p: &UserProfile) -> Value {
        json!({
            "username": p.username,
            "fullName": p.full_name,
            "biography": p.biography,
            "picUrl": self.sign_opt(&p.profile_pic_url),
            "profileUrl": p.profile_url,
            "mediaCount": p.media_count,
            "followerCount": p.follower_count,
            "followingCount": p.following_count,
            "isPrivate": p.is_private,
            "hasStories": p.has_stories,
            "isVerified": p.is_verified,
            "userId": p.user_id,
            "externalUrl": p.external_url,
        })
    }

    /// A media item, or `None` when its media URL is not on the allowlist.
    fn item_json(&self, m: &MediaItem) -> Option<Value> {
        let media = inner_cdn_url(&m.media_url)?;
        let video = m.is_video || m.media_type == "video" || is_video_url(&media);
        let kind = if m.media_type == "carousel" {
            "carousel"
        } else if video {
            "video"
        } else {
            "image"
        };
        let mut v = json!({
            "id": m.id,
            "kind": kind,
            "url": self.signer.url_for(&media),
            "thumbUrl": self.sign_opt(&m.thumbnail_url),
            "takenAt": m.taken_at,
            "caption": m.caption.as_deref().filter(|c| !c.trim().is_empty()),
            "likeCount": m.like_count,
            "commentCount": m.comment_count,
        });
        if shortcode_of(&m.id) {
            let path = if video && kind != "carousel" {
                "reel"
            } else {
                "p"
            };
            v["permalink"] = json!(format!("https://www.instagram.com/{path}/{}/", m.id));
        }
        if kind == "carousel" {
            let thumb = v["thumbUrl"].clone();
            let children: Vec<Value> = m
                .carousel_urls
                .iter()
                .enumerate()
                .filter_map(|(i, u)| {
                    let cdn = inner_cdn_url(u)?;
                    let is_video = is_video_url(&cdn);
                    let signed = self.signer.url_for(&cdn);
                    Some(json!({
                        "id": format!("{}_{i}", m.id),
                        "kind": if is_video { "video" } else { "image" },
                        "url": signed,
                        "thumbUrl": if is_video { if i == 0 { thumb.clone() } else { Value::Null } } else { json!(signed) },
                    }))
                })
                .collect();
            v["children"] = Value::Array(children);
        }
        Some(v)
    }

    fn page_json(&self, page: &MediaPage) -> Value {
        json!({
            "items": page.items.iter().filter_map(|m| self.item_json(m)).collect::<Vec<_>>(),
            "nextCursor": page.next_cursor.as_deref().filter(|c| !c.is_empty()),
        })
    }

    /// One page of a feed from the store: `DISK_PAGE` items from `offset`. Past the end of what is
    /// known the cursor is the backend's own, where its older items start.
    fn feed_json(&self, feed: &Feed, offset: usize, refreshing: bool) -> Value {
        let end = (offset + DISK_PAGE).min(feed.items.len());
        let slice = feed.items.get(offset..end).unwrap_or(&[]);
        let next = if end < feed.items.len() {
            Some(format!("d{end}"))
        } else {
            feed.next_cursor.clone().filter(|c| !c.is_empty())
        };
        json!({
            "items": slice.iter().filter_map(|m| self.item_json(m)).collect::<Vec<_>>(),
            "nextCursor": next,
            "updatedAt": feed.fetched_at,
            "refreshing": refreshing,
        })
    }

    /// Stories from the store: the ones up now first, then the expired ones that still have their
    /// local copy (`expired: true`).
    fn stories_json(&self, feed: &Feed, offset: usize, refreshing: bool) -> Value {
        let now = now_secs();
        let items: Vec<Value> = feed
            .items
            .iter()
            .filter_map(|m| {
                // Not in the last answer; or a day old, with no answer since to say it is still up.
                let expired = !feed.is_live(&m.id)
                    || m.taken_at.is_some_and(|t| {
                        now - t > STORY_LIFE_SECS && feed.fetched_at - t <= STORY_LIFE_SECS
                    });
                if expired && !self.has_local(&m.media_url) {
                    return None;
                }
                let mut v = self.item_json(m)?;
                if expired {
                    v["expired"] = json!(true);
                }
                Some(v)
            })
            .collect();
        let end = (offset + DISK_PAGE).min(items.len());
        let next = (end < items.len()).then(|| format!("d{end}"));
        json!({
            "items": items.get(offset..end).unwrap_or(&[]),
            "nextCursor": next,
            "updatedAt": feed.fetched_at,
            "refreshing": refreshing,
        })
    }

    /// Whether the file behind a backend media URL is on disk.
    fn has_local(&self, backend_url: &str) -> bool {
        inner_cdn_url(backend_url).is_some_and(|c| self.cache.has_media(&cache_key(&c)))
    }

    fn highlights_json(&self, hs: &[HighlightSummary]) -> Vec<Value> {
        hs.iter()
            .map(|h| {
                json!({
                    "id": h.id,
                    "title": h.title,
                    "coverUrl": self.sign_opt(&h.cover_url),
                    // The backend has said 0 for highlights with items since 2026-10-29: 0 is
                    // "not known", not "empty".
                    "mediaCount": Some(h.media_count).filter(|n| *n > 0),
                })
            })
            .collect()
    }

    // ---- upstream JSON, cached ----

    /// Fresh from the cache; else an older copy (up to `STALE_MAX`) at once while a fresh one is
    /// fetched behind it; else the backend. A cold bundle takes the backend 10 s or more
    /// (2026-10-30), longer than the web waits, and a watched account's copy is up to a day old.
    async fn fetch_text(&self, path: &str, ttl: Duration) -> Result<String, ServiceError> {
        self.fetch_cached(path, ttl, STALE_MAX, keep_any).await
    }

    /// [`Service::fetch_text`] with its own limit for an older copy (zero: none) and `keep`, which
    /// says which answers may be cached at all.
    async fn fetch_cached(
        &self,
        path: &str,
        ttl: Duration,
        stale: Duration,
        keep: fn(&str) -> bool,
    ) -> Result<String, ServiceError> {
        let key = hash_key(path);
        if let Some(s) = self.cache.get::<String>(&key, ttl).filter(|s| keep(s)) {
            return Ok(s);
        }
        let old = (stale > ttl)
            .then(|| self.cache.get::<String>(&key, stale))
            .flatten()
            .filter(|s| keep(s));
        if let Some(s) = old {
            // Detached: the copy answers now, the fresh one lands in the cache.
            drop(self.fetch_shared(path, key, ttl, keep, Priority::Low));
            return Ok(s);
        }
        match self
            .fetch_shared(path, key, ttl, keep, Priority::Owner)
            .await
        {
            Ok(r) => Ok(r?),
            Err(_) => Err(ServiceError::Upstream),
        }
    }

    /// One backend fetch per cache key at a time, in a task of its own: a request that arrives
    /// while one is under way waits for it and takes its answer from the cache, and a client that
    /// gives up (the web's timeout, a closed tab) does not throw the answer away.
    fn fetch_shared(
        &self,
        path: &str,
        key: String,
        ttl: Duration,
        keep: fn(&str) -> bool,
        priority: Priority,
    ) -> tokio::task::JoinHandle<Result<String, UpstreamError>> {
        let up = self.up.clone();
        let cache = self.cache.clone();
        let flights = self.json_flights.clone();
        let path = path.to_string();
        tokio::spawn(async move {
            let lock = {
                let mut m = flights.lock().unwrap_or_else(|e| e.into_inner());
                m.entry(key.clone()).or_default().clone()
            };
            let guard = lock.lock().await;
            let result = match cache.get::<String>(&key, ttl).filter(|s| keep(s)) {
                Some(s) => Ok(s),
                None => {
                    let r = match priority {
                        Priority::Owner => up.api_get(&path).await,
                        Priority::Low => up.api_get_low(&path).await,
                    };
                    match &r {
                        Ok(body) if keep(body) => cache.put(&key, body),
                        Ok(_) => {}
                        Err(e) if matches!(priority, Priority::Low) => {
                            tracing::warn!(error = %e, "instagram: refreshing a cached answer failed; the copy stays");
                        }
                        Err(_) => {}
                    }
                    r
                }
            };
            {
                let mut m = flights.lock().unwrap_or_else(|e| e.into_inner());
                // The map's copy and ours: nobody else is waiting on this key.
                if Arc::strong_count(&lock) <= 2 {
                    m.remove(&key);
                }
            }
            drop(guard);
            result
        })
    }

    async fn fetch_page(&self, path: &str, ttl: Duration) -> Result<MediaPage, ServiceError> {
        let text = self.fetch_text(path, ttl).await?;
        parse_media_page(&text).ok_or(ServiceError::Upstream)
    }

    // ---- the store ----

    /// Fold a bundle into the store: the profile, its tray when it has one, the posts.
    fn absorb_bundle(&self, user: &str, bundle: &ProfileBundle) {
        let now = now_secs();
        self.store.update_profile(user, |d| {
            d.profile = Some(bundle.profile.clone());
            d.fetched_at = now;
            d.gone = false;
            if !bundle.highlights.is_empty() {
                d.highlights = bundle.highlights.clone();
                d.highlights_at = now;
            }
        });
        let local = |u: &str| self.has_local(u);
        self.store.update_feed(FeedKind::Posts, user, |f| {
            if f.fetched_at == 0 {
                f.next_cursor = bundle.posts.next_cursor.clone();
            }
            f.merge_front(&bundle.posts.items, &local);
            f.fetched_at = now;
        });
        self.remember_first_page(user, &bundle.posts);
    }

    fn absorb_stories(&self, user: &str, page: &MediaPage) {
        let local = |u: &str| self.has_local(u);
        self.store.update_feed(FeedKind::Stories, user, |f| {
            f.set_stories(&page.items, &local);
            f.fetched_at = now_secs();
        });
    }

    /// Fold the first answer of a feed in (or a refresh's), with where its older items start.
    fn absorb_first(&self, kind: FeedKind, owner: &str, items: &[MediaItem], next: Option<String>) {
        let local = |u: &str| self.has_local(u);
        self.store.update_feed(kind, owner, |f| {
            if f.fetched_at == 0 {
                f.next_cursor = next;
            }
            f.merge_front(items, &local);
            f.fetched_at = now_secs();
        });
    }

    /// Fold an older page in. The page that continues from the feed's end moves the end on; one
    /// that brought nothing new ends the feed, however the backend's cursor goes on.
    fn absorb_older(&self, kind: FeedKind, owner: &str, cursor: &str, page: &MediaPage) {
        let local = |u: &str| self.has_local(u);
        self.store.update_feed(kind, owner, |f| {
            let added = f.append_older(&page.items, &local);
            if f.next_cursor.as_deref() == Some(cursor) {
                f.next_cursor = if added == 0 {
                    None
                } else {
                    page.next_cursor.clone().filter(|c| !c.is_empty())
                };
            }
        });
    }

    // ---- the refresh behind an answer ----

    fn stale(at: i64, ttl: Duration) -> bool {
        now_secs() - at > ttl.as_secs() as i64
    }

    fn account_needs_refresh(&self, user: &str) -> bool {
        let doc = self.store.profile(user);
        if Self::stale(doc.fetched_at, TTL_PROFILE) {
            return true;
        }
        if doc.gone || doc.profile.as_ref().is_some_and(|p| p.is_private) {
            return false;
        }
        Self::stale(
            self.store.feed(FeedKind::Stories, user).fetched_at,
            TTL_STORIES,
        ) || Self::stale(self.store.feed(FeedKind::Reels, user).fetched_at, TTL_POSTS)
            || Self::tray_stale(&doc)
    }

    /// An empty tray is not known to be empty (the backend answers `[]` for accounts that have
    /// highlights): it is asked again sooner, behind the answer.
    fn tray_stale(doc: &ProfileDoc) -> bool {
        let ttl = if doc.highlights.is_empty() {
            TTL_EMPTY_TRAY
        } else {
            TTL_HIGHLIGHTS
        };
        Self::stale(doc.highlights_at, ttl)
    }

    /// May a refresh of `key` start? Marks it running when it may.
    fn claim_refresh(&self, key: &str) -> bool {
        let now = now_secs();
        let mut g = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
        if g.0.contains(key) || g.1.get(key).is_some_and(|t| now - t < REFRESH_RETRY_SECS) {
            return false;
        }
        g.0.insert(key.to_string());
        g.1.insert(key.to_string(), now);
        true
    }

    fn refresh_running(&self, key: &str) -> bool {
        let g = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
        g.0.contains(key)
    }

    fn refresh_done(&self, key: &str) {
        let mut g = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
        g.0.remove(key);
    }

    /// Start what is out of date for this account, behind the answer. Says whether a refresh is
    /// running (the client asks the store again until it is not).
    fn refresh_behind(self: &Arc<Self>, user: &str) -> bool {
        if self.refresh_running(user) {
            return true;
        }
        if !self.account_needs_refresh(user) || !self.claim_refresh(user) {
            return false;
        }
        let svc = self.clone();
        let user = user.to_string();
        tokio::spawn(async move {
            let started = Instant::now();
            let r = svc.refresh_account(&user).await;
            svc.refresh_done(&user);
            let ms = started.elapsed().as_millis() as u64;
            match r {
                Ok(()) => tracing::info!(ms, "instagram refresh"),
                Err(e) => tracing::warn!(ms, error = %e, "instagram refresh"),
            }
        });
        true
    }

    /// A backend answer at the warmer's low priority, through the JSON cache and its one fetch
    /// per key: within `ttl` no call is made.
    async fn low_text(
        &self,
        path: &str,
        ttl: Duration,
        keep: fn(&str) -> bool,
    ) -> Result<String, UpstreamError> {
        match self
            .fetch_shared(path, hash_key(path), ttl, keep, Priority::Low)
            .await
        {
            Ok(r) => r,
            Err(_) => Err(UpstreamError::Network),
        }
    }

    /// What is out of date for one account: the bundle, the stories, the reels down to the first
    /// known item, the tray. A kind that fails costs that kind; a 429 ends the pass.
    pub async fn refresh_account(&self, user: &str) -> Result<(), UpstreamError> {
        fn soft(r: Result<(), UpstreamError>, what: &'static str) -> Result<(), UpstreamError> {
            match r {
                Err(UpstreamError::RateLimited(s)) => Err(UpstreamError::RateLimited(s)),
                Err(e) => {
                    tracing::warn!(what, error = %e, "instagram refresh: one kind failed");
                    Ok(())
                }
                Ok(()) => Ok(()),
            }
        }
        if Self::stale(self.store.profile(user).fetched_at, TTL_PROFILE) {
            let path = rel(core_url::user_bundle_url(user));
            match self.low_text(&path, TTL_PROFILE, keep_any).await {
                Ok(text) => match parse_profile_bundle(&text) {
                    Some(b) => self.absorb_bundle(user, &b),
                    None => tracing::warn!("instagram refresh: a bundle did not parse"),
                },
                Err(UpstreamError::Status(404)) => {
                    // Known before: what is here is what is left. Never known: there is no such
                    // account, and the profile route says so.
                    if self.store.profile(user).profile.is_some() {
                        self.store.update_profile(user, |d| {
                            d.gone = true;
                            d.fetched_at = now_secs();
                        });
                    }
                    return Ok(());
                }
                Err(e) => soft(Err(e), "bundle")?,
            }
        }
        let doc = self.store.profile(user);
        if doc.gone || doc.profile.as_ref().is_none_or(|p| p.is_private) {
            self.mirror_account(user).await;
            return Ok(());
        }
        if Self::stale(
            self.store.feed(FeedKind::Stories, user).fetched_at,
            TTL_STORIES,
        ) {
            let path = rel(core_url::user_stories_url(user, None, None));
            match self.low_text(&path, TTL_STORIES, keep_any).await {
                Ok(text) => {
                    if let Some(p) = parse_media_page(&text) {
                        self.absorb_stories(user, &p);
                    }
                }
                Err(e) => soft(Err(e), "stories")?,
            }
        }
        if Self::stale(self.store.feed(FeedKind::Reels, user).fetched_at, TTL_POSTS) {
            soft(self.refresh_reels(user).await, "reels")?;
        }
        if Self::tray_stale(&self.store.profile(user)) {
            let path = rel(core_url::user_highlights_url(user));
            match self.low_text(&path, TTL_HIGHLIGHTS, nonempty_tray).await {
                Ok(text) => {
                    let tray = parse_highlights(&text).unwrap_or_default();
                    self.store.update_profile(user, |d| {
                        // An empty tray is not known to be empty: the old one stays, and it is
                        // asked again (only a nonempty answer is cached).
                        if !tray.is_empty() {
                            d.highlights = tray;
                        }
                        d.highlights_at = now_secs();
                    });
                }
                Err(e) => soft(Err(e), "highlights")?,
            }
        }
        self.mirror_account(user).await;
        Ok(())
    }

    /// New reels: page by page until a known one comes up. The first time, one page; the older
    /// ones come through the backfill.
    async fn refresh_reels(&self, user: &str) -> Result<(), UpstreamError> {
        let feed = self.store.feed(FeedKind::Reels, user);
        let known: HashSet<&str> = feed.items.iter().map(|m| m.id.as_str()).collect();
        let mut fresh: Vec<MediaItem> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut tail = None;
        for _ in 0..MAX_REFRESH_PAGES {
            let path = users_path(user, "/reels", cursor.as_deref());
            let text = self.low_text(&path, TTL_POSTS, keep_any).await?;
            let page = parse_media_page(&text).ok_or(UpstreamError::Network)?;
            let met_known = page.items.iter().any(|m| known.contains(m.id.as_str()));
            tail = page.next_cursor.clone().filter(|c| !c.is_empty());
            fresh.extend(page.items);
            if feed.fetched_at == 0 || met_known || tail.is_none() {
                break;
            }
            cursor = tail.clone();
        }
        self.absorb_first(FeedKind::Reels, user, &fresh, tail);
        Ok(())
    }

    fn highlight_refresh_key(id: &str) -> String {
        format!("hl:{id}")
    }

    /// New items of a highlight, behind the answer.
    fn refresh_highlight_behind(self: &Arc<Self>, id: &str) -> bool {
        let key = Self::highlight_refresh_key(id);
        if self.refresh_running(&key) {
            return true;
        }
        let feed = self.store.feed(FeedKind::Highlight, id);
        if !Self::stale(feed.fetched_at, TTL_HIGHLIGHTS) || !self.claim_refresh(&key) {
            return false;
        }
        let svc = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            let path = rel(core_url::highlight_items_url(&id, None));
            match svc.low_text(&path, TTL_HIGHLIGHTS, keep_any).await {
                Ok(text) => {
                    if let Some(p) = parse_media_page(&text) {
                        let next = p.next_cursor.clone().filter(|c| !c.is_empty());
                        svc.absorb_first(FeedKind::Highlight, &id, &p.items, next);
                        svc.mirror_highlight(&id, &svc.store.feed(FeedKind::Highlight, &id).items);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "instagram refresh: a highlight failed"),
            }
            svc.refresh_done(&Self::highlight_refresh_key(&id));
        });
        true
    }

    // ---- endpoints ----

    fn page_log(kind: &'static str, disk: bool, started: Instant, refreshing: bool) {
        tracing::info!(
            kind,
            source = if disk { "disk" } else { "backend" },
            ms = started.elapsed().as_millis() as u64,
            refreshing,
            "instagram page"
        );
    }

    /// Profile, the newest posts and the highlights, from the store; only a profile never opened
    /// waits for the backend, and then only for its bundle. Everything else comes behind the
    /// answer (`refreshing`).
    pub async fn profile(self: &Arc<Self>, raw_username: &str) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let started = Instant::now();
        let on_disk = self.store.profile(&username).profile.is_some();
        if !on_disk {
            let asked = self
                .fetch_text(&rel(core_url::user_bundle_url(&username)), TTL_PROFILE)
                .await;
            let text = match asked {
                Ok(t) => t,
                Err(ServiceError::NotFound) => return Err(ServiceError::NotFound),
                Err(e) => {
                    // The backend timed out or refused: keep asking behind the error, so the
                    // owner's "Try again" finds it.
                    self.refresh_behind(&username);
                    return Err(e);
                }
            };
            let bundle = parse_profile_bundle(&text).ok_or(ServiceError::Upstream)?;
            self.absorb_bundle(&username, &bundle);
            self.store
                .update_profile(&username, |d| d.fetched_at = ASK_AGAIN);
            self.store
                .update_feed(FeedKind::Posts, &username, |f| f.fetched_at = ASK_AGAIN);
        }
        let doc = self.store.profile(&username);
        let profile = doc.profile.as_ref().ok_or(ServiceError::Upstream)?;
        self.recent.add(RecentEntry {
            username: username.clone(),
            full_name: profile.full_name.clone(),
            pic_cdn: inner_cdn_url(&profile.profile_pic_url),
            verified: profile.is_verified,
            private: profile.is_private,
            opened_at: now_secs(),
        });
        let refreshing = self.refresh_behind(&username);
        Self::page_log("profile", on_disk, started, refreshing);
        let posts = self.store.feed(FeedKind::Posts, &username);
        Ok(json!({
            "profile": self.profile_json(profile),
            "posts": self.feed_json(&posts, 0, refreshing),
            "highlights": self.highlights_json(&doc.highlights),
            "updatedAt": doc.fetched_at,
            "refreshing": refreshing,
            "gone": doc.gone,
        }))
    }

    /// Empty trays are unknown, including entries written by older versions. Retry them on
    /// the next request; only a parsed, nonempty tray is reusable for the highlights TTL.
    async fn fetch_highlights(
        &self,
        username: &str,
    ) -> Result<Vec<HighlightSummary>, ServiceError> {
        let path = rel(core_url::user_highlights_url(username));
        let text = self
            .fetch_cached(&path, TTL_HIGHLIGHTS, STALE_MAX, nonempty_tray)
            .await?;
        parse_highlights(&text).ok_or(ServiceError::Upstream)
    }

    fn first_page_key(username: &str) -> String {
        format!("first-{}", hash_key(username))
    }

    fn remember_first_page(&self, username: &str, page: &MediaPage) {
        let ids: Vec<&str> = page.items.iter().map(|m| m.id.as_str()).collect();
        self.cache.put(&Self::first_page_key(username), &ids);
    }

    /// Posts, from the store. The backend's `/users/{u}/posts` ignores `cursor` (observed
    /// 2026-10-28: the same twelve posts come back whatever the cursor, though `next_cursor` keeps
    /// changing). A cursor page that is just the first page again is reported as the end of the
    /// list, and the store stops offering the cursor.
    pub async fn posts(
        self: &Arc<Self>,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let started = Instant::now();
        let feed = self.store.feed(FeedKind::Posts, &username);
        match cursor {
            None if feed.fetched_at > 0 => {
                let refreshing = self.refresh_behind(&username);
                Self::page_log("posts", true, started, refreshing);
                Ok(self.feed_json(&feed, 0, refreshing))
            }
            None => {
                let page = self
                    .fetch_page(&users_path(&username, "/posts", None), TTL_POSTS)
                    .await?;
                self.remember_first_page(&username, &page);
                let next = page.next_cursor.clone().filter(|c| !c.is_empty());
                self.absorb_first(FeedKind::Posts, &username, &page.items, next);
                self.store
                    .update_feed(FeedKind::Posts, &username, |f| f.fetched_at = ASK_AGAIN);
                let refreshing = self.refresh_behind(&username);
                Self::page_log("posts", false, started, refreshing);
                Ok(self.feed_json(&self.store.feed(FeedKind::Posts, &username), 0, refreshing))
            }
            Some(c) if disk_offset(c).is_some() => {
                Ok(self.feed_json(&feed, disk_offset(c).unwrap_or(0), false))
            }
            Some(c) => {
                let page = self
                    .fetch_page(&users_path(&username, "/posts", Some(c)), TTL_POSTS)
                    .await?;
                let first: Option<Vec<String>> = self
                    .cache
                    .get(&Self::first_page_key(&username), TTL_FIRST_PAGE);
                if let Some(first) = first {
                    let first: HashSet<&str> = first.iter().map(String::as_str).collect();
                    let again = !page.items.is_empty()
                        && page.items.iter().all(|m| first.contains(m.id.as_str()));
                    if again {
                        self.store
                            .update_feed(FeedKind::Posts, &username, |f| f.next_cursor = None);
                        return Ok(json!({"items": [], "nextCursor": null, "exhausted": true}));
                    }
                }
                self.absorb_older(FeedKind::Posts, &username, c, &page);
                Ok(self.page_json(&page))
            }
        }
    }

    /// Reels, from the store: the backend has a separate `/users/{u}/reels` (video posts only,
    /// cursor works). A backend cursor comes after everything known.
    pub async fn reels(
        self: &Arc<Self>,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let started = Instant::now();
        let feed = self.store.feed(FeedKind::Reels, &username);
        match cursor {
            None if feed.fetched_at > 0 => {
                let refreshing = self.refresh_behind(&username);
                Self::page_log("reels", true, started, refreshing);
                Ok(self.feed_json(&feed, 0, refreshing))
            }
            None => {
                let page = self
                    .fetch_page(&users_path(&username, "/reels", None), TTL_POSTS)
                    .await?;
                let next = page.next_cursor.clone().filter(|c| !c.is_empty());
                self.absorb_first(FeedKind::Reels, &username, &page.items, next);
                self.store
                    .update_feed(FeedKind::Reels, &username, |f| f.fetched_at = ASK_AGAIN);
                let refreshing = self.refresh_behind(&username);
                Self::page_log("reels", false, started, refreshing);
                Ok(self.feed_json(&self.store.feed(FeedKind::Reels, &username), 0, refreshing))
            }
            Some(c) if disk_offset(c).is_some() => {
                Ok(self.feed_json(&feed, disk_offset(c).unwrap_or(0), false))
            }
            Some(c) => {
                let page = self
                    .fetch_page(&users_path(&username, "/reels", Some(c)), TTL_POSTS)
                    .await?;
                self.absorb_older(FeedKind::Reels, &username, c, &page);
                self.mirror_items(&username, &page.items);
                Ok(self.page_json(&page))
            }
        }
    }

    /// Stories, from the store: the ones up now, then the expired ones with a local copy. Only an
    /// account whose stories were never asked waits for the backend.
    pub async fn stories(
        self: &Arc<Self>,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let started = Instant::now();
        let feed = self.store.feed(FeedKind::Stories, &username);
        if let Some(offset) = cursor.and_then(disk_offset) {
            return Ok(self.stories_json(&feed, offset, false));
        }
        let on_disk = feed.fetched_at > 0;
        if !on_disk {
            let text = self
                .fetch_cached(
                    &rel(core_url::user_stories_url(&username, cursor, None)),
                    TTL_STORIES,
                    Duration::ZERO,
                    keep_any,
                )
                .await?;
            let page = parse_media_page(&text).ok_or(ServiceError::Upstream)?;
            self.absorb_stories(&username, &page);
            self.mirror_items(&username, &page.items);
        }
        let refreshing = self.refresh_behind(&username);
        Self::page_log("stories", on_disk, started, refreshing);
        Ok(self.stories_json(
            &self.store.feed(FeedKind::Stories, &username),
            0,
            refreshing,
        ))
    }

    /// The tray, from the store (kept when a later answer is empty); an account with none known
    /// asks the backend.
    pub async fn highlights(self: &Arc<Self>, raw_username: &str) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let started = Instant::now();
        let doc = self.store.profile(&username);
        if !doc.highlights.is_empty() || doc.highlights_at > 0 {
            let refreshing = self.refresh_behind(&username);
            Self::page_log("highlights", true, started, refreshing);
            return Ok(json!({
                "items": self.highlights_json(&doc.highlights),
                "refreshing": refreshing,
            }));
        }
        let hs = self.fetch_highlights(&username).await?;
        self.store.update_profile(&username, |d| {
            d.highlights.clone_from(&hs);
            d.highlights_at = now_secs();
        });
        Self::page_log("highlights", false, started, false);
        Ok(json!({"items": self.highlights_json(&hs)}))
    }

    pub async fn highlight_items(
        self: &Arc<Self>,
        id: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let ok = id.len() <= 80
            && id.starts_with("highlight:")
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_');
        if !ok {
            return Err(ServiceError::BadRequest("highlight id"));
        }
        let cursor = valid_cursor(cursor)?;
        let started = Instant::now();
        let feed = self.store.feed(FeedKind::Highlight, id);
        match cursor {
            None if feed.fetched_at > 0 => {
                let refreshing = self.refresh_highlight_behind(id);
                Self::page_log("highlight", true, started, refreshing);
                Ok(self.feed_json(&feed, 0, refreshing))
            }
            None => {
                let page = self
                    .fetch_page(
                        &rel(core_url::highlight_items_url(id, None)),
                        TTL_HIGHLIGHTS,
                    )
                    .await?;
                let next = page.next_cursor.clone().filter(|c| !c.is_empty());
                self.absorb_first(FeedKind::Highlight, id, &page.items, next);
                self.store
                    .update_feed(FeedKind::Highlight, id, |f| f.fetched_at = ASK_AGAIN);
                self.mirror_highlight(id, &page.items);
                let refreshing = self.refresh_highlight_behind(id);
                Self::page_log("highlight", false, started, refreshing);
                Ok(self.feed_json(&self.store.feed(FeedKind::Highlight, id), 0, refreshing))
            }
            Some(c) if disk_offset(c).is_some() => {
                Ok(self.feed_json(&feed, disk_offset(c).unwrap_or(0), false))
            }
            Some(c) => {
                let page = self
                    .fetch_page(
                        &rel(core_url::highlight_items_url(id, Some(c))),
                        TTL_HIGHLIGHTS,
                    )
                    .await?;
                self.absorb_older(FeedKind::Highlight, id, c, &page);
                self.mirror_highlight(id, &page.items);
                Ok(self.page_json(&page))
            }
        }
    }

    /// New links for ones this service signed; anything else is left out. Behind the owner's
    /// check, so a page kept for days never shows a dead link and never holds a link longer.
    pub fn resign(&self, urls: &[String]) -> Result<Value, ServiceError> {
        if urls.len() > MAX_RESIGN {
            return Err(ServiceError::BadRequest("urls"));
        }
        let map: serde_json::Map<String, Value> = urls
            .iter()
            .filter_map(|u| Some((u.clone(), Value::String(self.signer.resign(u)?))))
            .collect();
        Ok(json!({"urls": map}))
    }

    pub async fn resolve(&self, post_url: &str) -> Result<Value, ServiceError> {
        if !valid_post_url(post_url) {
            return Err(ServiceError::BadRequest("instagram post or reel url"));
        }
        let page = self
            .fetch_page(&rel(core_url::resolve_url(post_url.trim())), TTL_RESOLVE)
            .await?;
        Ok(self.page_json(&page))
    }

    pub fn recent_json(&self) -> Value {
        let items: Vec<Value> = self
            .recent
            .list()
            .iter()
            .map(|e| {
                json!({
                    "username": e.username,
                    "fullName": e.full_name,
                    "isVerified": e.verified,
                    "isPrivate": e.private,
                    "picUrl": e.pic_cdn.as_deref().map(|c| self.signer.url_for(c)),
                    "openedAt": e.opened_at,
                })
            })
            .collect();
        json!({"items": items})
    }

    // ---- media ----

    /// The file for a (verified, allowlisted) CDN URL: from the cache, or fetched once even if
    /// several requests ask for it at the same moment.
    pub async fn media_file(&self, cdn: &str) -> Result<MediaFile, ServiceError> {
        let key = cache_key(cdn);
        if let Some(f) = self.cached_media(&key) {
            return Ok(f);
        }
        let lock = {
            let mut m = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            m.entry(key.clone()).or_default().clone()
        };
        let guard = lock.lock().await;
        let result = self.fetch_media(cdn, &key).await;
        {
            let mut m = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            // The map's copy and ours: nobody else is waiting on this key.
            if Arc::strong_count(&lock) <= 2 {
                m.remove(&key);
            }
        }
        drop(guard);
        result
    }

    fn cached_media(&self, key: &str) -> Option<MediaFile> {
        let (path, ctype) = self.cache.find_media(key)?;
        let len = std::fs::metadata(&path).ok()?.len();
        Some(MediaFile { path, ctype, len })
    }

    async fn fetch_media(&self, cdn: &str, key: &str) -> Result<MediaFile, ServiceError> {
        if let Some(f) = self.cached_media(key) {
            return Ok(f);
        }
        let tmp = self.cache.tmp_media(key);
        let started = Instant::now();
        let fetched = self.up.media_to_file(cdn, &tmp).await;
        let ms = started.elapsed().as_millis() as u64;
        let ctype = match fetched {
            Ok((ctype, bytes)) => {
                tracing::info!(ctype, bytes, ms, "instagram cdn");
                ctype
            }
            Err(e) => {
                tracing::warn!(ms, error = %e, "instagram cdn");
                return Err(media_error(e));
            }
        };
        let ext = ext_for_ctype(ctype).ok_or(ServiceError::Upstream)?;
        let path = self
            .cache
            .commit_media(&tmp, key, ext)
            .map_err(|_| ServiceError::Upstream)?;
        let len = std::fs::metadata(&path)
            .map_err(|_| ServiceError::Upstream)?
            .len();
        Ok(MediaFile { path, ctype, len })
    }

    /// Verify a signed URL's parts and return its media file.
    pub async fn signed_media(
        &self,
        exp: i64,
        sig: &str,
        token: &str,
    ) -> Result<MediaFile, SignedError> {
        let cdn = self
            .signer
            .verify(exp, sig, token)
            .map_err(SignedError::Sign)?;
        if let Some(f) = self.cached_media(&cache_key(&cdn)) {
            return Ok(f);
        }
        let _waiting = OwnerMedia::new(&self.owner_media);
        self.media_file(&cdn).await.map_err(SignedError::Service)
    }

    // ---- the background download ----

    /// Every file of an item: picture or poster, then the media itself, then a carousel's frames.
    fn item_files(m: &MediaItem) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut add = |u: &str| {
            if let Some(cdn) = inner_cdn_url(u) {
                let video = is_video_url(&cdn);
                out.push((cdn, video));
            }
        };
        add(&m.thumbnail_url);
        add(&m.media_url);
        m.carousel_urls.iter().for_each(|u| add(u));
        out
    }

    fn mirror_files(&self, files: Vec<(String, bool)>, account: &str) {
        let now = now_secs();
        let mut missing: Vec<(String, String, bool)> = Vec::new();
        for (cdn, video) in files {
            let key = cache_key(&cdn);
            if !self.cache.has_media(&key) {
                missing.push((key, cdn, video));
            }
        }
        if missing.is_empty() {
            return;
        }
        let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
        // In front, in reverse: what came last comes first, newest item first.
        for (key, cdn, video) in missing.into_iter().rev() {
            if g.skip_until.get(&key).is_some_and(|t| *t > now) || !g.queued.insert(key) {
                continue;
            }
            if video {
                g.videos.push_front((cdn, account.to_string()));
            } else {
                g.images.push_front((cdn, account.to_string()));
            }
        }
        drop(g);
        self.mirror_wake.notify_one();
    }

    /// Queue what these items need on disk, when their account is watched.
    fn mirror_items(&self, account: &str, items: &[MediaItem]) {
        if self.watch.get(account).is_some() {
            self.mirror_files(items.iter().flat_map(Self::item_files).collect(), account);
        }
    }

    /// [`Service::mirror_items`] for a highlight, whose id does not name its account.
    fn mirror_highlight(&self, id: &str, items: &[MediaItem]) {
        let account = self
            .watched_accounts()
            .into_iter()
            .find(|u| self.store.profile(u).highlights.iter().any(|h| h.id == id));
        if let Some(account) = account {
            self.mirror_items(&account, items);
        }
    }

    /// Queue everything known about a watched account (off the async threads: this asks the disk
    /// once per file, and the disk is a slow one). Any other account: nothing.
    pub async fn mirror_account(&self, user: &str) {
        if self.watch.get(user).is_none() {
            return;
        }
        let mut files: Vec<(String, bool)> = Vec::new();
        let doc = self.store.profile(user);
        if let Some(p) = &doc.profile {
            files.extend(inner_cdn_url(&p.profile_pic_url).map(|c| (c, false)));
        }
        for h in &doc.highlights {
            files.extend(inner_cdn_url(&h.cover_url).map(|c| (c, false)));
            let feed = self.store.feed(FeedKind::Highlight, &h.id);
            files.extend(feed.items.iter().flat_map(Self::item_files));
        }
        for kind in [FeedKind::Stories, FeedKind::Posts, FeedKind::Reels] {
            let feed = self.store.feed(kind, user);
            files.extend(feed.items.iter().flat_map(Self::item_files));
        }
        // No spawn_blocking here: `self` is not 'static on this path, and block_in_place keeps the
        // runtime's other tasks moving while the disk is asked.
        if tokio::runtime::Handle::current().runtime_flavor()
            == tokio::runtime::RuntimeFlavor::MultiThread
        {
            tokio::task::block_in_place(|| self.mirror_files(files, user));
        } else {
            self.mirror_files(files, user);
        }
    }

    /// One step of the background download. Images go before videos; the owner's requests go
    /// before all of it, and one of the two CDN slots is always left to them.
    pub async fn mirror_tick(&self) -> MirrorOutcome {
        if self.cache.is_full() {
            return MirrorOutcome::Paused;
        }
        if self.owner_media.load(Ordering::SeqCst) > 0 {
            return MirrorOutcome::Busy;
        }
        let (cdn, account) = {
            let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
            let day = today();
            if g.day != day {
                g.day = day;
                g.day_bytes = 0;
            }
            if g.day_bytes >= self.cfg.mirror.daily_max_bytes {
                return MirrorOutcome::Paused;
            }
            let Some((cdn, account)) = g.images.pop_front().or_else(|| g.videos.pop_front()) else {
                return MirrorOutcome::Idle;
            };
            g.queued.remove(&cache_key(&cdn));
            (cdn, account)
        };
        let key = cache_key(&cdn);
        // Unwatched since it was queued: what is left of it is not fetched.
        if self.watch.get(&account).is_none() {
            return MirrorOutcome::Skipped;
        }
        // From before the file lands, so its own arrival cannot evict it.
        self.cache.pin(&key);
        if self.cache.has_media(&key) {
            return MirrorOutcome::Skipped;
        }
        match self.media_file(&cdn).await {
            Ok(file) => {
                let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
                g.day_bytes += file.len;
                g.done += 1;
                g.done_bytes += file.len;
                let left = g.images.len() + g.videos.len();
                if g.done >= 100 || left == 0 {
                    tracing::info!(
                        files = g.done,
                        bytes = g.done_bytes,
                        left,
                        today_bytes = g.day_bytes,
                        "instagram mirror"
                    );
                    g.done = 0;
                    g.done_bytes = 0;
                }
                MirrorOutcome::Fetched
            }
            Err(ServiceError::RateLimited(_)) => {
                let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
                if g.queued.insert(key) {
                    g.images.push_back((cdn, account));
                }
                MirrorOutcome::Paused
            }
            Err(_) => {
                let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
                g.skip_until.insert(key, now_secs() + MIRROR_RETRY_SECS);
                MirrorOutcome::Failed
            }
        }
    }

    /// The download's loop: slow, and quiet when there is nothing to do.
    pub async fn run_mirror(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut last_pin = Instant::now();
        for a in self.watched_accounts() {
            self.mirror_account(&a).await;
        }
        loop {
            let outcome = tokio::select! {
                o = self.mirror_tick() => o,
                _ = stop.changed() => return,
            };
            if last_pin.elapsed() > Duration::from_secs(600) {
                last_pin = Instant::now();
                self.pin_kept_media();
            }
            let rest = match outcome {
                MirrorOutcome::Fetched | MirrorOutcome::Failed => self.cfg.mirror.interval,
                MirrorOutcome::Skipped => Duration::ZERO,
                MirrorOutcome::Busy => Duration::from_millis(300),
                MirrorOutcome::Idle => Duration::from_secs(300),
                MirrorOutcome::Paused => Duration::from_secs(60),
            };
            tokio::select! {
                _ = tokio::time::sleep(rest) => {}
                _ = self.mirror_wake.notified(), if outcome == MirrorOutcome::Idle => {}
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
        }
    }

    /// What eviction must leave alone: everything of a watched account, and the local copies of
    /// saved items.
    pub fn pin_kept_media(&self) {
        let mut keys: HashSet<String> = self.archive.cache_keys().into_iter().collect();
        for a in self.watch.list() {
            let doc = self.store.profile(&a.username);
            let mut add = |items: &[MediaItem]| {
                for m in items {
                    keys.extend(Self::item_files(m).iter().map(|(c, _)| cache_key(c)));
                }
            };
            for h in &doc.highlights {
                add(&self.store.feed(FeedKind::Highlight, &h.id).items);
            }
            for kind in [FeedKind::Stories, FeedKind::Posts, FeedKind::Reels] {
                add(&self.store.feed(kind, &a.username).items);
            }
            let pics = doc.profile.iter().map(|p| &p.profile_pic_url);
            for u in pics.chain(doc.highlights.iter().map(|h| &h.cover_url)) {
                keys.extend(inner_cdn_url(u).map(|c| cache_key(&c)));
            }
        }
        self.cache.set_pinned(keys);
    }

    // ---- the backfill: older pages, slowly ----

    /// Only watched accounts are downloaded in full (owner, 2026-11-01: "at least for the watching
    /// creators"); others keep what was looked at, in the ordinary cache.
    fn watched_accounts(&self) -> Vec<String> {
        self.watch.list().into_iter().map(|a| a.username).collect()
    }

    /// An account with nothing in the store was not tried lately (a dead name must not eat the
    /// day's calls).
    fn seed_due(&self, user: &str) -> bool {
        let g = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
        g.1.get(user)
            .is_none_or(|t| now_secs() - t >= MIRROR_RETRY_SECS)
    }

    /// One older page of the first feed that has any left, or the first fill of an account. Says
    /// whether the backend was asked.
    pub async fn backfill_tick(&self) -> bool {
        if self.warmer_paused().is_some() || self.up.blocked_for().is_some() {
            return false;
        }
        {
            let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
            let day = today();
            if g.backfill_day != day {
                g.backfill_day = day;
                g.backfill_calls = 0;
            }
            if g.backfill_calls >= self.cfg.mirror.backfill_daily_calls {
                return false;
            }
        }
        for user in self.watched_accounts() {
            let doc = self.store.profile(&user);
            // Watched before the store existed: fill it now, not at the next check.
            if doc.profile.is_none() && !doc.gone {
                if !self.seed_due(&user) || !self.claim_refresh(&user) {
                    continue;
                }
                for _ in 0..SEED_CALLS {
                    self.count_backfill_call();
                }
                let r = self.refresh_account(&user).await;
                self.refresh_done(&user);
                if let Err(e) = r {
                    self.backfill_failed(e, FeedKind::Reels, &user, Some(""));
                }
                return true;
            }
            if doc.gone || doc.profile.as_ref().is_none_or(|p| p.is_private) {
                continue;
            }
            let reels = self.store.feed(FeedKind::Reels, &user);
            if let (true, Some(c)) = (reels.fetched_at > 0, reels.next_cursor.as_deref()) {
                let path = users_path(&user, "/reels", Some(c));
                self.backfill_page(FeedKind::Reels, &user, &user, c, &path)
                    .await;
                return true;
            }
            for h in &doc.highlights {
                let feed = self.store.feed(FeedKind::Highlight, &h.id);
                if feed.fetched_at == 0 {
                    let path = rel(core_url::highlight_items_url(&h.id, None));
                    self.count_backfill_call();
                    match self.low_text(&path, TTL_HIGHLIGHTS, keep_any).await {
                        Ok(text) => {
                            if let Some(p) = parse_media_page(&text) {
                                let next = p.next_cursor.clone().filter(|c| !c.is_empty());
                                self.absorb_first(FeedKind::Highlight, &h.id, &p.items, next);
                                self.mirror_items(&user, &p.items);
                            }
                        }
                        Err(e) => {
                            self.backfill_failed(e, FeedKind::Highlight, &h.id, None);
                        }
                    }
                    return true;
                }
                if let Some(c) = feed.next_cursor.as_deref() {
                    let path = rel(core_url::highlight_items_url(&h.id, Some(c)));
                    self.backfill_page(FeedKind::Highlight, &h.id, &user, c, &path)
                        .await;
                    return true;
                }
            }
        }
        false
    }

    fn count_backfill_call(&self) {
        let mut g = self.mirror.lock().unwrap_or_else(|e| e.into_inner());
        g.backfill_calls += 1;
    }

    async fn backfill_page(
        &self,
        kind: FeedKind,
        owner: &str,
        account: &str,
        cursor: &str,
        path: &str,
    ) {
        self.count_backfill_call();
        match self.low_text(path, TTL_POSTS, keep_any).await {
            Ok(text) => {
                if let Some(p) = parse_media_page(&text) {
                    self.absorb_older(kind, owner, cursor, &p);
                    self.mirror_items(account, &p.items);
                }
            }
            Err(e) => self.backfill_failed(e, kind, owner, Some(cursor)),
        }
    }

    /// A 429 rests the backfill with the warmer. A cursor the backend no longer takes ends that
    /// feed's backfill (the newest items keep coming through refreshes); a first page that fails
    /// is marked as asked, so one bad highlight does not hold up the rest.
    fn backfill_failed(&self, e: UpstreamError, kind: FeedKind, owner: &str, cursor: Option<&str>) {
        tracing::warn!(error = %e, "instagram backfill");
        match e {
            UpstreamError::RateLimited(s) => self.pause_warmer(Duration::from_secs(s)),
            UpstreamError::Status(400 | 404 | 410 | 422) if cursor != Some("") => {
                self.store.update_feed(kind, owner, |f| {
                    if cursor.is_some() {
                        f.next_cursor = None;
                    } else {
                        f.fetched_at = now_secs();
                    }
                });
            }
            _ => {}
        }
    }

    /// The backfill's loop.
    pub async fn run_backfill(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            let asked = tokio::select! {
                a = self.backfill_tick() => a,
                _ = stop.changed() => return,
            };
            let rest = if asked {
                self.cfg.mirror.backfill_interval
            } else {
                Duration::from_secs(300)
            };
            tokio::select! {
                _ = tokio::time::sleep(rest) => {}
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
        }
    }

    // ---- save to lucina-media ----

    pub async fn save(&self, items: Vec<SaveItem>) -> Result<Value, ServiceError> {
        if items.is_empty() || items.len() > MAX_SAVE_ITEMS {
            return Err(ServiceError::BadRequest("items"));
        }
        let mut saved = Vec::new();
        let mut failed = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match self.save_one(item).await {
                Ok((item_id, duplicate)) => {
                    saved.push(json!({"index": index, "itemId": item_id, "duplicate": duplicate}))
                }
                Err(code) => failed.push(json!({"index": index, "error": code})),
            }
        }
        Ok(json!({"saved": saved, "failed": failed}))
    }

    async fn save_one(&self, item: &SaveItem) -> Result<(String, bool), &'static str> {
        if !matches!(item.kind.as_deref(), None | Some("image") | Some("video")) {
            return Err("bad_request");
        }
        let username = normalize_username(&item.username).ok_or("bad_request")?;
        // Any age: this route is behind the owner's check, and a page may have been open for hours.
        let cdn = self
            .signer
            .verify_url_any_age(&item.url)
            .map_err(|e| match e {
                SignError::HostNotAllowed => "forbidden",
                _ => "bad_url",
            })?;
        let file = self.media_file(&cdn).await.map_err(|e| match e {
            ServiceError::RateLimited(_) => "rate_limited",
            ServiceError::Gone => "expired",
            _ => "download_failed",
        })?;
        let caption = {
            let c = item.caption.as_deref().unwrap_or("").trim();
            let c: String = c.chars().take(2000).collect();
            if c.is_empty() {
                format!("@{username}")
            } else {
                format!("@{username} {c}")
            }
        };
        let taken = match &item.taken_at {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.timestamp()),
            _ => None,
        };
        let (id, duplicate) = self
            .send_to_library(&cdn, &file, &username, "Instagram", &caption, taken)
            .await?;
        self.archive.add(
            &username,
            Saved {
                media_id: id.clone(),
                ig_id: None,
                kind: kind_of(file.ctype).into(),
                mime: file.ctype.into(),
                taken_at: taken,
                saved_at: chrono::Utc::now().timestamp(),
                caption: item.caption.clone().filter(|c| !c.trim().is_empty()),
                auto: false,
                cache_key: Some(cache_key(&cdn)),
            },
        );
        Ok((id, duplicate))
    }

    /// Copy a cached file into the spool and hand it to lucina-media. The spool copy is removed
    /// when the hand-over fails (on success lucina-media moved it).
    async fn send_to_library(
        &self,
        cdn: &str,
        file: &MediaFile,
        username: &str,
        album: &str,
        caption: &str,
        taken_at: Option<i64>,
    ) -> Result<(String, bool), &'static str> {
        let ext = ext_for_ctype(file.ctype).unwrap_or("bin");
        let key = cache_key(cdn);
        let name = format!("instagram-{username}-{}.{ext}", &key[..10]);
        let mut rnd = [0u8; 6];
        getrandom::fill(&mut rnd).map_err(|_| "spool_failed")?;
        let spool = self
            .cfg
            .spool_dir
            .join(format!("{}-{name}", hex::encode(rnd)));
        tokio::fs::copy(&file.path, &spool)
            .await
            .map_err(|_| "spool_failed")?;
        let result = self
            .ingest(&spool, &name, file.ctype, album, caption, taken_at)
            .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&spool).await;
        }
        result
    }

    async fn ingest(
        &self,
        spool: &std::path::Path,
        name: &str,
        mime: &str,
        album: &str,
        caption: &str,
        taken_at: Option<i64>,
    ) -> Result<(String, bool), &'static str> {
        let taken_at = taken_at.and_then(|t| {
            let secs = if t > 100_000_000_000 { t / 1000 } else { t };
            chrono::DateTime::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
        });
        let resp = self
            .http
            .post(format!(
                "{}/internal/v1/ingest",
                self.cfg.ingest_url.trim_end_matches('/')
            ))
            .json(&json!({
                "path": spool,
                "name": name,
                "mime": mime,
                "source": "instagram",
                "album": album,
                "caption": caption,
                "taken_at": taken_at,
                "move": true,
            }))
            .send()
            .await
            .map_err(|_| "ingest_unavailable")?;
        if !resp.status().is_success() {
            return Err("ingest_failed");
        }
        let v: Value = resp.json().await.map_err(|_| "ingest_failed")?;
        let id = v["item_id"].as_str().ok_or("ingest_failed")?.to_string();
        Ok((id, v["duplicate"].as_bool().unwrap_or(false)))
    }

    // ---- watching ----------------------------------------------------------------------------

    fn watch_json(&self, a: &Account) -> Value {
        json!({
            "username": a.username,
            "autoSave": a.auto_save,
            "lastCheck": a.last_check,
            "nextCheck": a.next_due,
            "newStories": a.unseen_stories.len(),
            "newPosts": a.unseen_posts.len(),
            "private": a.is_private,
            "fullName": a.full_name,
            "picUrl": a.pic_cdn.as_deref().map(|c| self.signer.url_for(c)),
            "isVerified": a.verified,
            "gone": a.gone,
            "status": a.status,
            "archived": self.archive.count(&a.username),
        })
    }

    pub fn watch_list(&self) -> Value {
        Value::Array(
            self.watch
                .list()
                .iter()
                .map(|a| self.watch_json(a))
                .collect(),
        )
    }

    /// Watch an account, or change its auto-save switch. A new account is checked within seconds.
    pub fn watch_put(&self, raw: &str, auto_save: Option<bool>) -> Result<Value, ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        let now = chrono::Utc::now().timestamp();
        let first = now + 2 + (jitter(self.cfg.watch.first_check_jitter.as_secs() + 1) as i64);
        let acc = self.watch.put(&user, auto_save, now, first);
        self.wake.notify_one();
        Ok(self.watch_json(&acc))
    }

    pub fn watch_delete(&self, raw: &str) -> Result<(), ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        if self.watch.remove(&user) {
            // Its media goes back to the ordinary cache; the queue drops what is left of it.
            self.pin_kept_media();
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// The interval in force: the owner's choice, else the configured one.
    fn interval(&self) -> Duration {
        match self.watch.settings().interval_hours {
            Some(h) => Duration::from_secs(u64::from(h) * 3600),
            None => self.cfg.watch.interval,
        }
    }

    /// Every account's next check moved into the window of the interval in force.
    fn reschedule_watched(&self) {
        let j = self.cfg.watch.jitter.as_secs() as i64;
        self.watch
            .reschedule(self.interval().as_secs() as i64, j, || random_offset(j));
    }

    pub fn watch_settings_json(&self) -> Value {
        json!({
            "intervalHours": self.interval().as_secs() / 3600,
            "defaultHours": self.cfg.watch.interval.as_secs() / 3600,
            "jitterMinutes": self.cfg.watch.jitter.as_secs() / 60,
            "choices": INTERVAL_CHOICES,
        })
    }

    /// The owner's interval for all watched accounts (1-23 h); it applies to the next check.
    pub fn set_watch_interval(&self, hours: u32) -> Result<Value, ServiceError> {
        if !(1..=23).contains(&hours) {
            return Err(ServiceError::BadRequest("intervalHours"));
        }
        self.watch.set_interval_hours(Some(hours));
        self.reschedule_watched();
        self.wake.notify_one();
        Ok(self.watch_settings_json())
    }

    pub fn badges_json(&self) -> Value {
        let (total, accounts) = self.watch.badges();
        json!({"total": total, "accounts": accounts})
    }

    /// The owner opened the account: its badge clears.
    pub fn watch_opened(&self, raw: &str) -> Result<Value, ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        let now = chrono::Utc::now().timestamp();
        self.watch
            .update(&user, |a| {
                a.unseen_posts.clear();
                a.unseen_stories.clear();
                a.last_opened = Some(now);
            })
            .ok_or(ServiceError::NotFound)?;
        Ok(self.badges_json())
    }

    fn latest_key(kind: &str, user: &str) -> String {
        format!("latest-{kind}-{}", hash_key(user))
    }

    /// The stories and posts that appeared since the owner last opened the account.
    pub fn watch_new(&self, raw: &str) -> Result<Value, ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        let acc = self.watch.get(&user).ok_or(ServiceError::NotFound)?;
        let day = Duration::from_secs(24 * 3600);
        let pick = |kind: &str, ids: &[String], from_bundle: bool| -> Vec<Value> {
            let Some(text) = self
                .cache
                .get::<String>(&Self::latest_key(kind, &user), day)
            else {
                return Vec::new();
            };
            let items = if from_bundle {
                parse_profile_bundle(&text).map(|b| b.posts.items)
            } else {
                parse_media_page(&text).map(|p| p.items)
            };
            items
                .unwrap_or_default()
                .iter()
                .filter(|m| ids.contains(&m.id))
                .filter_map(|m| self.item_json(m))
                .collect()
        };
        Ok(json!({
            "username": user,
            "since": acc.last_opened,
            "stories": pick("stories", &acc.unseen_stories, false),
            "posts": pick("bundle", &acc.unseen_posts, true),
        }))
    }

    /// The saved items of an account (newest saved first), `cursor` being an offset.
    pub fn archive_json(&self, raw: &str, cursor: &Option<String>) -> Result<Value, ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        let offset = match cursor.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            None => 0,
            Some(c) => c
                .parse::<usize>()
                .map_err(|_| ServiceError::BadRequest("cursor"))?,
        };
        let (items, next) = self.archive.page(&user, offset, 30);
        let items: Vec<Value> = items
            .iter()
            .map(|s| {
                json!({
                    "id": s.ig_id.clone().unwrap_or_else(|| s.media_id.clone()),
                    "mediaId": s.media_id,
                    "kind": s.kind,
                    "url": self.signer.archive_url(&s.media_id),
                    "thumbUrl": format!("/v1/media/items/{}/thumb?size=256", s.media_id),
                    "takenAt": s.taken_at,
                    "savedAt": s.saved_at,
                    "caption": s.caption,
                    "auto": s.auto,
                })
            })
            .collect();
        Ok(json!({"items": items, "nextCursor": next.map(|n| n.to_string())}))
    }

    /// The file of a saved item, read from where lucina-media keeps it (its loopback tells us).
    pub async fn archive_file(
        &self,
        exp: i64,
        sig: &str,
        media_id: &str,
    ) -> Result<MediaFile, SignedError> {
        self.signer
            .verify_archive(exp, sig, media_id)
            .map_err(SignedError::Sign)?;
        let saved = self
            .archive
            .find(media_id)
            .ok_or(SignedError::Service(ServiceError::NotFound))?;
        let resp = self
            .http
            .get(format!(
                "{}/internal/v1/items/{media_id}/path",
                self.cfg.ingest_url.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| SignedError::Service(ServiceError::Upstream))?;
        if !resp.status().is_success() {
            return Err(SignedError::Service(ServiceError::NotFound));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|_| SignedError::Service(ServiceError::Upstream))?;
        let path = PathBuf::from(
            v["path"]
                .as_str()
                .ok_or(SignedError::Service(ServiceError::Upstream))?,
        );
        let len = std::fs::metadata(&path)
            .map_err(|_| SignedError::Service(ServiceError::NotFound))?
            .len();
        let ctype = ctype_for_ext(ext_for_ctype(&saved.mime).unwrap_or(""))
            .unwrap_or("application/octet-stream");
        Ok(MediaFile { path, ctype, len })
    }

    // ---- the warmer --------------------------------------------------------------------------

    fn warmer_paused(&self) -> Option<Duration> {
        let g = self
            .warmer_paused_until
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let until = (*g)?;
        let now = tokio::time::Instant::now();
        (until > now).then(|| until - now)
    }

    fn pause_warmer(&self, at_least: Duration) {
        let mut g = self
            .warmer_paused_until
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *g = Some(tokio::time::Instant::now() + at_least.max(self.cfg.watch.pause));
    }

    fn next_interval(&self) -> i64 {
        let j = self.cfg.watch.jitter.as_secs() as i64;
        (self.interval().as_secs() as i64 + random_offset(j)).max(1)
    }

    /// Fetch through the warmer's low-priority queue and keep the answer in the cache, so the
    /// owner's next visit is instant.
    async fn fetch_fresh(&self, path: &str) -> Result<String, UpstreamError> {
        let body = self.up.api_get_low(path).await?;
        self.cache.put(&hash_key(path), &body);
        Ok(body)
    }

    /// One pass of the warmer: send queued notices, then refresh the account that is due.
    pub async fn watch_tick(&self) -> TickOutcome {
        if !self.cfg.watch.enabled {
            return TickOutcome::Disabled;
        }
        self.flush_notices().await;
        if self.warmer_paused().is_some() || self.up.blocked_for().is_some() {
            return TickOutcome::Paused;
        }
        let now = chrono::Utc::now().timestamp();
        let Some(user) = self.watch.next_due(now) else {
            return TickOutcome::Idle;
        };
        let result = self.check_account(&user).await;
        let now = chrono::Utc::now().timestamp();
        let wait = match &result {
            Err(UpstreamError::RateLimited(secs)) => {
                self.pause_warmer(Duration::from_secs(*secs));
                self.watch
                    .update(&user, |a| a.status = Some("rate_limited".into()));
                self.cfg.watch.pause.as_secs() as i64 + jitter(60) as i64
            }
            _ => self.next_interval(),
        };
        self.watch.update(&user, |a| a.next_due = now + wait);
        self.flush_notices().await;
        match result {
            Err(UpstreamError::RateLimited(_)) => TickOutcome::Paused,
            _ => TickOutcome::Checked(user),
        }
    }

    /// Refresh one watched account: profile, stories, the diff, auto-save, notices.
    pub async fn check_account(&self, user: &str) -> Result<(), UpstreamError> {
        let now = chrono::Utc::now().timestamp();
        let bundle_path = rel(core_url::user_bundle_url(user));
        let text = match self.fetch_fresh(&bundle_path).await {
            Ok(t) => t,
            Err(UpstreamError::Status(404)) => {
                let change = self
                    .watch
                    .update(user, |a| apply_not_found(a, now))
                    .flatten();
                if let Some(c) = change {
                    self.watch.queue_notice(user, &c, now);
                }
                return Ok(());
            }
            Err(e) => {
                if !matches!(e, UpstreamError::RateLimited(_)) {
                    self.watch.update(user, |a| a.status = Some("error".into()));
                }
                return Err(e);
            }
        };
        let Some(bundle) = parse_profile_bundle(&text) else {
            self.watch.update(user, |a| a.status = Some("error".into()));
            return Ok(());
        };
        self.cache.put(&Self::latest_key("bundle", user), &text);
        self.absorb_bundle(user, &bundle);

        // Stories: not for a private account (the backend has none to give).
        let mut stories: Option<MediaPage> = None;
        if !bundle.profile.is_private {
            let path = rel(core_url::user_stories_url(user, None, None));
            match self.fetch_fresh(&path).await {
                Ok(t) => {
                    if let Some(p) = parse_media_page(&t) {
                        self.cache.put(&Self::latest_key("stories", user), &t);
                        self.absorb_stories(user, &p);
                        stories = Some(p);
                    }
                }
                Err(UpstreamError::RateLimited(s)) => return Err(UpstreamError::RateLimited(s)),
                Err(_) => {}
            }
        }
        let obs = Observation {
            user_id: Some(bundle.profile.user_id.clone()).filter(|s| !s.is_empty()),
            is_private: bundle.profile.is_private,
            full_name: bundle.profile.full_name.clone(),
            pic_cdn: inner_cdn_url(&bundle.profile.profile_pic_url),
            verified: bundle.profile.is_verified,
            post_ids: bundle.posts.items.iter().map(|m| m.id.clone()).collect(),
            story_ids: stories
                .as_ref()
                .map(|p| p.items.iter().map(|m| m.id.clone()).collect()),
        };
        let changes: Vec<Change> = self
            .watch
            .update(user, |a| apply_observation(a, &obs, now))
            .unwrap_or_default();
        for c in &changes {
            self.watch.queue_notice(user, c, now);
        }
        let saved = match &stories {
            Some(page) => self.autosave_stories(user, &page.items).await,
            None => Ok(()),
        };
        // The rest of the account (reels, tray), and everything it shows on disk. A 429 here is
        // the block's business; the check itself is done.
        if saved.is_ok() {
            if let Err(e) = self.refresh_account(user).await {
                tracing::warn!(error = %e, "instagram watch: the rest of the account failed");
            }
        }
        self.pin_kept_media();
        saved
    }

    /// Save the stories not saved yet into the Gallery (album "Instagram · @user"). Reads only:
    /// the backend is never told a story was seen.
    async fn autosave_stories(&self, user: &str, items: &[MediaItem]) -> Result<(), UpstreamError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        for m in items {
            let Some(acc) = self.watch.get(user) else {
                return Ok(());
            };
            if !acc.auto_save || acc.is_private == Some(true) {
                return Ok(());
            }
            if acc.is_saved(&m.id) {
                continue;
            }
            let Some(cdn) = inner_cdn_url(&m.media_url) else {
                continue;
            };
            let room = acc.room_today(self.cfg.watch.daily_cap_bytes, &today);
            if room == 0 {
                tracing::info!("instagram watch: daily cap reached for an account");
                return Ok(());
            }
            let file = match self.media_file(&cdn).await {
                Ok(f) => f,
                Err(ServiceError::RateLimited(s)) => return Err(UpstreamError::RateLimited(s)),
                Err(_) => continue,
            };
            if file.len > room {
                tracing::info!("instagram watch: a story is over the daily cap; skipped");
                continue;
            }
            let caption = match m
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                Some(c) => format!("@{user} {}", c.chars().take(2000).collect::<String>()),
                None => format!("@{user}"),
            };
            let album = format!("Instagram \u{b7} @{user}");
            match self
                .send_to_library(&cdn, &file, user, &album, &caption, m.taken_at)
                .await
            {
                Ok((media_id, duplicate)) => {
                    self.watch.update(user, |a| {
                        a.mark_saved(&m.id);
                        if !duplicate {
                            a.add_bytes(&today, file.len);
                        }
                    });
                    self.archive.add(
                        user,
                        Saved {
                            media_id,
                            ig_id: Some(m.id.clone()),
                            kind: kind_of(file.ctype).into(),
                            mime: file.ctype.into(),
                            taken_at: m.taken_at,
                            saved_at: chrono::Utc::now().timestamp(),
                            caption: m.caption.clone().filter(|c| !c.trim().is_empty()),
                            auto: true,
                            cache_key: Some(cache_key(&cdn)),
                        },
                    );
                }
                Err(code) => tracing::warn!(
                    code,
                    "instagram watch: could not save a story; retried next time"
                ),
            }
        }
        Ok(())
    }

    async fn flush_notices(&self) {
        let now = chrono::Utc::now().timestamp();
        self.watch.drop_old_notices(now);
        for n in self.watch.notices() {
            match self.notifier.send(&n).await {
                Sent::Yes => self.watch.settle_notice(&n.key, now),
                Sent::NotConfigured => {
                    tracing::warn!(
                        "instagram watch: a change was found but notifications are not configured"
                    );
                    self.watch.settle_notice(&n.key, now);
                }
                Sent::Failed => {}
            }
        }
    }

    /// How long the warmer may sleep: until the next account is due (at most 30 s).
    fn until_next_due(&self) -> Duration {
        let now = chrono::Utc::now().timestamp();
        match self.watch.earliest_due() {
            Some(due) => Duration::from_secs((due - now + 1).clamp(1, 30) as u64),
            None => Duration::from_secs(30),
        }
    }

    /// The warmer's loop: a pass, then a short rest, until `stop` says so.
    pub async fn run_warmer(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            // A pass can take minutes (a video download, a backed-off request); a stop request
            // ends it at once. Everything it learned is already saved after each step.
            let outcome = tokio::select! {
                o = self.watch_tick() => o,
                _ = stop.changed() => return,
            };
            let rest = match outcome {
                TickOutcome::Disabled => return,
                TickOutcome::Checked(_) => Duration::from_secs(3),
                TickOutcome::Idle => self.until_next_due(),
                TickOutcome::Paused => Duration::from_secs(60),
            };
            tokio::select! {
                _ = tokio::time::sleep(rest) => {}
                _ = self.wake.notified() => {}
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TickOutcome {
    Disabled,
    Idle,
    Paused,
    Checked(String),
}

fn kind_of(ctype: &str) -> &'static str {
    if ctype.starts_with("video/") {
        "video"
    } else {
        "image"
    }
}

/// A random number in `0..max`.
fn jitter(max: u64) -> u64 {
    let mut b = [0u8; 8];
    if max == 0 || getrandom::fill(&mut b).is_err() {
        return 0;
    }
    u64::from_le_bytes(b) % max
}

/// A random number in `-max..=max`.
fn random_offset(max: i64) -> i64 {
    let max = max.max(0);
    jitter(2 * max as u64 + 1) as i64 - max
}

#[derive(Debug)]
pub enum SignedError {
    Sign(SignError),
    Service(ServiceError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_normalised_and_checked() {
        assert_eq!(normalize_username("@NatGeo ").as_deref(), Some("natgeo"));
        assert_eq!(normalize_username("a.b_c9").as_deref(), Some("a.b_c9"));
        assert!(normalize_username("").is_none());
        assert!(normalize_username("a/b").is_none());
        assert!(normalize_username("a b").is_none());
        assert!(normalize_username("../x").is_none());
        assert!(normalize_username(&"a".repeat(31)).is_none());
    }

    #[test]
    fn post_urls_are_checked() {
        assert!(valid_post_url("https://www.instagram.com/p/DeXzbFEl5DG/"));
        assert!(valid_post_url(
            "https://instagram.com/reel/DeXzbFEl5DG/?igsh=1"
        ));
        assert!(valid_post_url(
            "https://www.instagram.com/natgeo/p/DeXzbFEl5DG/"
        ));
        assert!(!valid_post_url("https://www.instagram.com/natgeo/"));
        assert!(!valid_post_url("https://evil.test/p/DeXzbFEl5DG/"));
        assert!(!valid_post_url(
            "https://www.instagram.com.evil.test/p/DeXzbFEl5DG/"
        ));
        assert!(!valid_post_url("file:///etc/passwd"));
        assert!(!valid_post_url("nonsense"));
    }

    #[test]
    fn cursors_are_checked() {
        assert!(valid_cursor(&None).unwrap().is_none());
        assert!(valid_cursor(&Some("".into())).unwrap().is_none());
        assert_eq!(
            valid_cursor(&Some("AQHT-_x".into())).unwrap(),
            Some("AQHT-_x")
        );
        assert!(valid_cursor(&Some("a b".into())).is_err());
        assert!(valid_cursor(&Some("x".repeat(2000))).is_err());
    }
}
