//! The plugin's logic: ask the StoryView backend (through `instarust-core`'s URL builders and
//! parsers), turn what comes back into the plugin's JSON with signed media URLs, serve media
//! through the disk cache, remember opened profiles, and save items into lucina-media.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

use instarust_core::media::{parse_highlights, parse_media_page};
use instarust_core::profile::parse_profile_bundle;
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
use crate::upstream::{Upstream, UpstreamError};
use crate::watch::{apply_not_found, apply_observation, Account, Change, Observation, WatchStore};

const TTL_PROFILE: Duration = Duration::from_secs(300);
const TTL_POSTS: Duration = Duration::from_secs(300);
const TTL_STORIES: Duration = Duration::from_secs(120);
const TTL_HIGHLIGHTS: Duration = Duration::from_secs(600);
const TTL_RESOLVE: Duration = Duration::from_secs(600);
const TTL_FIRST_PAGE: Duration = Duration::from_secs(24 * 3600);
const MAX_SAVE_ITEMS: usize = 50;

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
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// The warmer refreshes watched accounts (and auto-saves their new stories).
    pub enabled: bool,
    /// Each account is refreshed every random time in this range (20-40 min by default).
    pub interval_min: Duration,
    pub interval_max: Duration,
    /// How long the warmer stays quiet after the backend said 429 (at least).
    pub pause: Duration,
    /// Auto-saved bytes per account per UTC day.
    pub daily_cap_bytes: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            enabled: true,
            interval_min: Duration::from_secs(20 * 60),
            interval_max: Duration::from_secs(40 * 60),
            pause: Duration::from_secs(30 * 60),
            daily_cap_bytes: 500 * 1024 * 1024,
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
    pub up: Upstream,
    pub cache: DiskCache,
    pub recent: Recent,
    pub signer: Signer,
    pub watch: WatchStore,
    pub archive: Archive,
    cfg: ServiceConfig,
    http: reqwest::Client,
    notifier: Notifier,
    inflight: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The warmer does nothing until then (a 429 sets it).
    warmer_paused_until: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Wakes the warmer (a newly watched account).
    pub wake: Notify,
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
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Service {
            up,
            cache,
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
            warmer_paused_until: std::sync::Mutex::new(None),
            wake: Notify::new(),
        })
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

    fn highlights_json(&self, hs: &[HighlightSummary]) -> Vec<Value> {
        hs.iter()
            .map(|h| {
                json!({
                    "id": h.id,
                    "title": h.title,
                    "coverUrl": self.sign_opt(&h.cover_url),
                    "mediaCount": h.media_count,
                })
            })
            .collect()
    }

    // ---- upstream JSON, cached ----

    async fn fetch_text(&self, path: &str, ttl: Duration) -> Result<String, ServiceError> {
        let key = hash_key(path);
        if let Some(s) = self.cache.get::<String>(&key, ttl) {
            return Ok(s);
        }
        let body = self.up.api_get(path).await?;
        self.cache.put(&key, &body);
        Ok(body)
    }

    async fn fetch_page(&self, path: &str, ttl: Duration) -> Result<MediaPage, ServiceError> {
        let text = self.fetch_text(path, ttl).await?;
        parse_media_page(&text).ok_or(ServiceError::Upstream)
    }

    // ---- endpoints ----

    /// Profile, the first page of posts and the highlights, in one backend call.
    pub async fn profile(&self, raw_username: &str) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let text = self
            .fetch_text(&rel(core_url::user_bundle_url(&username)), TTL_PROFILE)
            .await?;
        let bundle = parse_profile_bundle(&text).ok_or(ServiceError::Upstream)?;
        self.remember_first_page(&username, &bundle.posts);
        self.recent.add(RecentEntry {
            username: username.clone(),
            full_name: bundle.profile.full_name.clone(),
            pic_cdn: inner_cdn_url(&bundle.profile.profile_pic_url),
            verified: bundle.profile.is_verified,
            opened_at: chrono::Utc::now().timestamp(),
        });
        Ok(json!({
            "profile": self.profile_json(&bundle.profile),
            "posts": self.page_json(&bundle.posts),
            "highlights": self.highlights_json(&bundle.highlights),
        }))
    }

    fn first_page_key(username: &str) -> String {
        format!("first-{}", hash_key(username))
    }

    fn remember_first_page(&self, username: &str, page: &MediaPage) {
        let ids: Vec<&str> = page.items.iter().map(|m| m.id.as_str()).collect();
        self.cache.put(&Self::first_page_key(username), &ids);
    }

    /// Posts. The backend's `/users/{u}/posts` ignores `cursor` (observed 2026-10-28: the same
    /// twelve posts come back whatever the cursor, though `next_cursor` keeps changing). A
    /// cursor page that is just the first page again is reported as the end of the list.
    pub async fn posts(
        &self,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&users_path(&username, "/posts", cursor), TTL_POSTS)
            .await?;
        match cursor {
            None => self.remember_first_page(&username, &page),
            Some(_) => {
                let first: Option<Vec<String>> = self
                    .cache
                    .get(&Self::first_page_key(&username), TTL_FIRST_PAGE);
                if let Some(first) = first {
                    let first: HashSet<&str> = first.iter().map(String::as_str).collect();
                    let again = !page.items.is_empty()
                        && page.items.iter().all(|m| first.contains(m.id.as_str()));
                    if again {
                        return Ok(json!({"items": [], "nextCursor": null, "exhausted": true}));
                    }
                }
            }
        }
        Ok(self.page_json(&page))
    }

    /// Reels: the backend has a separate `/users/{u}/reels` (video posts only, cursor works).
    pub async fn reels(
        &self,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&users_path(&username, "/reels", cursor), TTL_POSTS)
            .await?;
        Ok(self.page_json(&page))
    }

    pub async fn stories(
        &self,
        raw_username: &str,
        cursor: &Option<String>,
    ) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(
                &rel(core_url::user_stories_url(&username, cursor, None)),
                TTL_STORIES,
            )
            .await?;
        Ok(self.page_json(&page))
    }

    pub async fn highlights(&self, raw_username: &str) -> Result<Value, ServiceError> {
        let username =
            normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let text = self
            .fetch_text(
                &rel(core_url::user_highlights_url(&username)),
                TTL_HIGHLIGHTS,
            )
            .await?;
        let hs = parse_highlights(&text).ok_or(ServiceError::Upstream)?;
        Ok(json!({"items": self.highlights_json(&hs)}))
    }

    pub async fn highlight_items(
        &self,
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
        let page = self
            .fetch_page(
                &rel(core_url::highlight_items_url(id, cursor)),
                TTL_HIGHLIGHTS,
            )
            .await?;
        Ok(self.page_json(&page))
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
        let (ctype, _) = self
            .up
            .media_to_file(cdn, &tmp)
            .await
            .map_err(media_error)?;
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
        self.media_file(&cdn).await.map_err(SignedError::Service)
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
        let cdn = self.signer.verify_url(&item.url).map_err(|e| match e {
            SignError::Expired => "expired",
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
        let first = now + 2 + (jitter(8) as i64);
        let acc = self.watch.put(&user, auto_save, now, first);
        self.wake.notify_one();
        Ok(self.watch_json(&acc))
    }

    pub fn watch_delete(&self, raw: &str) -> Result<(), ServiceError> {
        let user = normalize_username(raw).ok_or(ServiceError::BadRequest("username"))?;
        if self.watch.remove(&user) {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
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
        let lo = self.cfg.watch.interval_min.as_millis() as u64;
        let hi = self.cfg.watch.interval_max.as_millis().max(lo as u128) as u64;
        ((lo + jitter(hi - lo + 1)) / 1000).max(1) as i64
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

        // Stories: not for a private account (the backend has none to give).
        let mut stories: Option<MediaPage> = None;
        if !bundle.profile.is_private {
            let path = rel(core_url::user_stories_url(user, None, None));
            match self.fetch_fresh(&path).await {
                Ok(t) => {
                    if let Some(p) = parse_media_page(&t) {
                        self.cache.put(&Self::latest_key("stories", user), &t);
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
        if let Some(page) = &stories {
            return self.autosave_stories(user, &page.items).await;
        }
        Ok(())
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

    /// The warmer's loop: a pass, then a short rest, until `stop` says so.
    pub async fn run_warmer(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        loop {
            let rest = match self.watch_tick().await {
                TickOutcome::Disabled => return,
                TickOutcome::Checked(_) => Duration::from_secs(3),
                TickOutcome::Idle => Duration::from_secs(30),
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
