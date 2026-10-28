//! The plugin's logic: ask the StoryView backend (through `instarust-core`'s URL builders and
//! parsers), turn what comes back into the plugin's JSON with signed media URLs, serve media
//! through the disk cache, remember opened profiles, and save items into lucina-media.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use instarust_core::media::{parse_highlights, parse_media_page};
use instarust_core::profile::parse_profile_bundle;
use instarust_core::url as core_url;
use instarust_core::{HighlightSummary, MediaItem, MediaPage, UserProfile, BASE_URL};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::cache::{ext_for_ctype, DiskCache};
use crate::media::{cache_key, inner_cdn_url, is_video_url, SignError, Signer};
use crate::recent::{Recent, RecentEntry};
use crate::upstream::{Upstream, UpstreamError};

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
}

pub struct Service {
    pub up: Upstream,
    pub cache: DiskCache,
    pub recent: Recent,
    pub signer: Signer,
    cfg: ServiceConfig,
    http: reqwest::Client,
    inflight: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
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
    core.strip_prefix(BASE_URL).map(str::to_string).unwrap_or(core)
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
    !id.is_empty() && !id.bytes().all(|b| b.is_ascii_digit()) && id.len() <= 20
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
            let _ = std::fs::set_permissions(&cfg.spool_dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Service {
            up,
            cache,
            recent,
            signer,
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            inflight: std::sync::Mutex::new(HashMap::new()),
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
            let path = if video && kind != "carousel" { "reel" } else { "p" };
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
        let username = normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
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
    pub async fn posts(&self, raw_username: &str, cursor: &Option<String>) -> Result<Value, ServiceError> {
        let username = normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&users_path(&username, "/posts", cursor), TTL_POSTS)
            .await?;
        match cursor {
            None => self.remember_first_page(&username, &page),
            Some(_) => {
                let first: Option<Vec<String>> =
                    self.cache.get(&Self::first_page_key(&username), TTL_FIRST_PAGE);
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
    pub async fn reels(&self, raw_username: &str, cursor: &Option<String>) -> Result<Value, ServiceError> {
        let username = normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&users_path(&username, "/reels", cursor), TTL_POSTS)
            .await?;
        Ok(self.page_json(&page))
    }

    pub async fn stories(&self, raw_username: &str, cursor: &Option<String>) -> Result<Value, ServiceError> {
        let username = normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&rel(core_url::user_stories_url(&username, cursor, None)), TTL_STORIES)
            .await?;
        Ok(self.page_json(&page))
    }

    pub async fn highlights(&self, raw_username: &str) -> Result<Value, ServiceError> {
        let username = normalize_username(raw_username).ok_or(ServiceError::BadRequest("username"))?;
        let text = self
            .fetch_text(&rel(core_url::user_highlights_url(&username)), TTL_HIGHLIGHTS)
            .await?;
        let hs = parse_highlights(&text).ok_or(ServiceError::Upstream)?;
        Ok(json!({"items": self.highlights_json(&hs)}))
    }

    pub async fn highlight_items(&self, id: &str, cursor: &Option<String>) -> Result<Value, ServiceError> {
        let ok = id.len() <= 80
            && id.starts_with("highlight:")
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_');
        if !ok {
            return Err(ServiceError::BadRequest("highlight id"));
        }
        let cursor = valid_cursor(cursor)?;
        let page = self
            .fetch_page(&rel(core_url::highlight_items_url(id, cursor)), TTL_HIGHLIGHTS)
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
        let (ctype, _) = self.up.media_to_file(cdn, &tmp).await.map_err(media_error)?;
        let ext = ext_for_ctype(ctype).ok_or(ServiceError::Upstream)?;
        let path = self.cache.commit_media(&tmp, key, ext).map_err(|_| ServiceError::Upstream)?;
        let len = std::fs::metadata(&path).map_err(|_| ServiceError::Upstream)?.len();
        Ok(MediaFile { path, ctype, len })
    }

    /// Verify a signed URL's parts and return its media file.
    pub async fn signed_media(&self, exp: i64, sig: &str, token: &str) -> Result<MediaFile, SignedError> {
        let cdn = self.signer.verify(exp, sig, token).map_err(SignedError::Sign)?;
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
        let ext = ext_for_ctype(file.ctype).unwrap_or("bin");
        let key = cache_key(&cdn);
        let name = format!("instagram-{username}-{}.{ext}", &key[..10]);
        let mut rnd = [0u8; 6];
        getrandom::fill(&mut rnd).map_err(|_| "spool_failed")?;
        let spool = self.cfg.spool_dir.join(format!("{}-{name}", hex::encode(rnd)));
        tokio::fs::copy(&file.path, &spool).await.map_err(|_| "spool_failed")?;
        let result = self.ingest(&spool, &name, file.ctype, &username, item).await;
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
        username: &str,
        item: &SaveItem,
    ) -> Result<(String, bool), &'static str> {
        let caption = {
            let c = item.caption.as_deref().unwrap_or("").trim();
            let c: String = c.chars().take(2000).collect();
            if c.is_empty() {
                format!("@{username}")
            } else {
                format!("@{username} {c}")
            }
        };
        let taken_at = match &item.taken_at {
            Some(Value::Number(n)) => n.as_i64().and_then(|t| {
                let secs = if t > 100_000_000_000 { t / 1000 } else { t };
                chrono::DateTime::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
            }),
            Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.to_rfc3339()),
            _ => None,
        };
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
                "album": "Instagram",
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
        assert!(valid_post_url("https://instagram.com/reel/DeXzbFEl5DG/?igsh=1"));
        assert!(valid_post_url("https://www.instagram.com/natgeo/p/DeXzbFEl5DG/"));
        assert!(!valid_post_url("https://www.instagram.com/natgeo/"));
        assert!(!valid_post_url("https://evil.test/p/DeXzbFEl5DG/"));
        assert!(!valid_post_url("https://www.instagram.com.evil.test/p/DeXzbFEl5DG/"));
        assert!(!valid_post_url("file:///etc/passwd"));
        assert!(!valid_post_url("nonsense"));
    }

    #[test]
    fn cursors_are_checked() {
        assert!(valid_cursor(&None).unwrap().is_none());
        assert!(valid_cursor(&Some("".into())).unwrap().is_none());
        assert_eq!(valid_cursor(&Some("AQHT-_x".into())).unwrap(), Some("AQHT-_x"));
        assert!(valid_cursor(&Some("a b".into())).is_err());
        assert!(valid_cursor(&Some("x".repeat(2000))).is_err());
    }
}
