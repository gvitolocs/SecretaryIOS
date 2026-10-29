//! The plugin's logic: validation, the adult gate, the JSON cache, upstream calls through the gate,
//! image fetching through the cache, and saving originals into lucina-media.
//!
//! The adult gate lives here, not in the HTTP layer: R-18 ranking modes and searches, R-18
//! works (detail, bookmark, save) answer `AdultOff` unless the caller passed `adult = true`, and
//! R-18 items are filtered out of every list in that case (one `Content` filter for all lists).
//!
//! R-18 needs a logged-in session whose account shows R-18: without one, an R-18 list answers
//! empty with a `notice` and makes no upstream request.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::account::{parse_account, Account};
use crate::backend::Backend;
use crate::cache::DiskCache;
use crate::error::SvcError;
use crate::images::{allowed, ImageConfig, ImageFetcher};
use crate::model::{
    empty_listing, filter_content, parse_illust, parse_ranking, parse_search, Content, Illust,
    Listing, Sort, Summary,
};
use crate::upstream::{ApiGate, GateConfig, Op};

pub const MAX_DISPLAY_IMAGE: usize = 32 * 1024 * 1024;
pub const MAX_ORIGINAL: usize = 64 * 1024 * 1024;
const MAX_SAVE_PAGES: usize = 200;
/// How long pixiv's answer about the session's account is reused.
pub const ACCOUNT_TTL: Duration = Duration::from_secs(30 * 60);
/// The bookmark-tier fallback of `order=popular` reads this many upstream pages at most.
pub const TIER_PAGES: u64 = 3;
/// Local page size of the bookmark-tier fallback (pixiv's own page size).
pub const TIER_PAGE_SIZE: usize = 60;
/// The bookmark-tier tags, largest first, OR-ed after the user's words.
pub const TIERS: &[u64] = &[
    100_000, 50_000, 30_000, 20_000, 10_000, 5_000, 1_000, 500, 300, 100,
];

pub const R18_NEEDS_SESSION: &str = "r18_needs_session";
pub const R18_DISABLED_IN_ACCOUNT: &str = "r18_disabled_in_account";
pub const ALL_AGES_ONLY: &str = "all_ages_only";

pub const RANKING_MODES: &[&str] = &[
    "day",
    "week",
    "month",
    "day_male",
    "day_female",
    "original",
    "rookie",
    "day_r18",
    "week_r18",
    "day_male_r18",
    "day_female_r18",
];

#[derive(Debug, Clone)]
pub struct Config {
    pub gate: GateConfig,
    pub images: ImageConfig,
    /// Ranking and search answers are reused for this long.
    pub list_ttl: Duration,
    /// Work details.
    pub illust_ttl: Duration,
    /// lucina-media's loopback base (`/internal/v1/ingest`).
    pub media_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gate: GateConfig::default(),
            images: ImageConfig::default(),
            list_ttl: Duration::from_secs(300),
            illust_ttl: Duration::from_secs(1800),
            media_url: "http://127.0.0.1:8830".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    pub q: String,
    pub order: String,
    pub mode: String,
    pub kind: String,
    pub page: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedItem {
    pub page: usize,
    pub item_id: String,
    pub duplicate: bool,
}

#[derive(Debug, Serialize)]
pub struct FailedItem {
    pub page: usize,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct SaveOutcome {
    pub saved: Vec<SavedItem>,
    pub failed: Vec<FailedItem>,
}

/// The bookmark-tier fallback's merged, sorted list (cached as one).
#[derive(Debug, Serialize, serde::Deserialize)]
struct TierList {
    items: Vec<Summary>,
    total: Option<u64>,
}

pub struct Service {
    backend: Arc<dyn Backend>,
    gate: ApiGate,
    account: tokio::sync::Mutex<Option<(Instant, Account)>>,
    images: ImageFetcher,
    pub cache: DiskCache,
    spool: PathBuf,
    cfg: Config,
    http: reqwest::Client,
}

pub fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 12 && id.bytes().all(|b| b.is_ascii_digit())
}

fn ext_of(url: &str, ctype: &str) -> &'static str {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    match path.rsplit('.').next() {
        Some("png") => "png",
        Some("gif") => "gif",
        Some("webp") => "webp",
        Some("jpg" | "jpeg") => "jpg",
        _ => match ctype {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        },
    }
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

impl Service {
    pub fn new(
        backend: Arc<dyn Backend>,
        cache: DiskCache,
        spool: PathBuf,
        cfg: Config,
    ) -> anyhow::Result<Service> {
        std::fs::create_dir_all(&spool)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Service {
            gate: ApiGate::new(cfg.gate.clone()),
            account: tokio::sync::Mutex::new(None),
            images: ImageFetcher::new(cfg.images.clone())?,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(120))
                .build()?,
            backend,
            cache,
            spool,
            cfg,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    // ---- account -------------------------------------------------------------------------

    /// The session's account: anonymous without a session (no request), else pixiv's answer,
    /// reused for `ACCOUNT_TTL`. A failed check is not kept and reads as "ok, flags unknown".
    pub fn connection(&self) -> Option<&Arc<crate::connection::Connection>> {
        self.backend.connection()
    }

    pub async fn reset_account(&self) {
        *self.account.lock().await = None;
    }

    pub async fn account(&self) -> Account {
        if let Some(c) = self.connection() {
            self.reset_account().await;
            return c.account();
        }
        if !self.backend.has_session() {
            return Account::anonymous();
        }
        let mut slot = self.account.lock().await;
        if let Some((at, a)) = slot.as_ref() {
            if at.elapsed() < ACCOUNT_TTL {
                return a.clone();
            }
        }
        match self.gate.call(&self.backend, &Op::SelfStatus).await {
            Ok(v) => {
                let a = parse_account(&v);
                *slot = Some((Instant::now(), a.clone()));
                a
            }
            Err(_) => {
                tracing::warn!("pixiv account status unavailable");
                Account::unknown()
            }
        }
    }

    // ---- lists -----------------------------------------------------------------------------

    pub async fn ranking(&self, mode: &str, page: u32, adult: bool) -> Result<Listing, SvcError> {
        if !RANKING_MODES.contains(&mode) {
            return Err(SvcError::BadRequest("mode"));
        }
        let r18 = mode.ends_with("_r18");
        if r18 && !adult {
            return Err(SvcError::AdultOff);
        }
        let content = match (r18, adult) {
            (true, _) => Content::R18,
            (false, true) => Content::All,
            (false, false) => Content::Safe,
        };
        let page = page.clamp(1, 100);
        let account = self.account().await;
        if r18 {
            if let Some(notice) = r18_refusal(&account) {
                return Ok(notice_listing(page, notice));
            }
        }
        let key = digest(&format!("ranking/{}/{mode}/{page}", account.fingerprint()));
        let listing = match self.cache.get_json::<Listing>(&key, self.cfg.list_ttl) {
            Some(l) => l,
            None => {
                let op = Op::Ranking {
                    mode: mode.to_string(),
                    page,
                };
                let l = match self.gate.call(&self.backend, &op).await {
                    Ok(v) => parse_ranking(&v, page, r18),
                    Err(SvcError::NotFound) => empty_listing(page),
                    Err(e) => return Err(e),
                };
                self.cache.put_json(&key, &l);
                l
            }
        };
        Ok(filtered(listing, content, &account))
    }

    pub async fn search(&self, p: &SearchParams, adult: bool) -> Result<Listing, SvcError> {
        let q = p.q.trim();
        if q.is_empty() || q.chars().count() > 200 {
            return Err(SvcError::BadRequest("q"));
        }
        if !matches!(p.order.as_str(), "date_d" | "date" | "popular") {
            return Err(SvcError::BadRequest("order"));
        }
        let content = Content::parse(&p.mode).ok_or(SvcError::BadRequest("mode"))?;
        if !matches!(p.kind.as_str(), "all" | "illust" | "manga" | "ugoira") {
            return Err(SvcError::BadRequest("type"));
        }
        // Without adult, "all" asks pixiv for the all-ages results only.
        let content = content.gated(adult).ok_or(SvcError::AdultOff)?;
        let words = query_words(q);
        if words.is_empty() {
            return Err(SvcError::BadRequest("q"));
        }
        let page = p.page.clamp(1, 1000);
        let account = self.account().await;
        if content == Content::R18 {
            if let Some(notice) = r18_refusal(&account) {
                return Ok(notice_listing(page, notice));
            }
        }
        let fp = account.fingerprint();
        let mode = content.wire();
        let (mut listing, used) = match (p.order.as_str(), account.premium) {
            ("popular", true) => (
                self.search_page(&fp, &words, "popular_d", mode, &p.kind, page)
                    .await?,
                "pixiv_popular",
            ),
            ("popular", false) if account.usable() && self.connection().is_some() => {
                let mut preview = self
                    .search_page(&fp, &words, "popular_preview", mode, &p.kind, 1)
                    .await?;
                let mut plain = self
                    .search_page(
                        &fp,
                        &words,
                        "date_d",
                        mode,
                        &p.kind,
                        page.saturating_sub(1).max(1),
                    )
                    .await?;
                if page == 1 {
                    preview.has_more = !plain.items.is_empty();
                    (preview, "pixiv_popular_preview")
                } else {
                    let shown: HashSet<_> = preview.items.iter().map(|s| s.id.as_str()).collect();
                    plain.items.retain(|s| !shown.contains(s.id.as_str()));
                    plain.page = page;
                    (plain, "pixiv_popular_preview")
                }
            }
            ("popular", false) => self.search_tiers(&fp, &words, mode, &p.kind, page).await?,
            (order, _) => (
                self.search_page(&fp, &words, order, mode, &p.kind, page)
                    .await?,
                order,
            ),
        };
        listing.sort = Some(Sort {
            requested: p.order.clone(),
            used: used.to_string(),
        });
        let mut listing = filtered(listing, content, &account);
        if content == Content::All && !account.usable() {
            listing.notice = Some(ALL_AGES_ONLY.into());
        }
        Ok(listing)
    }

    /// One upstream search page (cached per account), unfiltered.
    async fn search_page(
        &self,
        fp: &str,
        words: &[String],
        order: &str,
        mode: &str,
        kind: &str,
        page: u32,
    ) -> Result<Listing, SvcError> {
        let key = search_key(fp, words, order, mode, kind, page);
        if let Some(l) = self.cache.get_json::<Listing>(&key, self.cfg.list_ttl) {
            return Ok(l);
        }
        let args = search_args(words, order, mode, kind, page);
        let l = match self.gate.call(&self.backend, &Op::Search { args }).await? {
            Value::Null => empty_listing(page),
            v => parse_search(&v, page),
        };
        self.cache.put_json(&key, &l);
        Ok(l)
    }

    /// `order=popular` without Premium: first the works carrying a bookmark-tier tag (the words
    /// AND any tier tag, newest first, up to `TIER_PAGES` upstream pages merged and sorted by tier,
    /// then id, both descending) in local pages 1..=k, then the plain `date_d` search from page
    /// k + 1 on, without the works already shown. `date_d_fallback` when no work has a tier tag
    /// (k = 0: the plain search alone).
    async fn search_tiers(
        &self,
        fp: &str,
        words: &[String],
        mode: &str,
        kind: &str,
        page: u32,
    ) -> Result<(Listing, &'static str), SvcError> {
        let key = digest(&format!(
            "search-tiers/{fp}/{}/{mode}/{kind}",
            words.join(" ")
        ));
        let list = match self.cache.get_json::<TierList>(&key, self.cfg.list_ttl) {
            Some(l) => l,
            None => {
                let (list, complete) = self.fetch_tiers(words, mode, kind).await?;
                if complete {
                    self.cache.put_json(&key, &list);
                }
                list
            }
        };
        let used = if list.items.is_empty() {
            "date_d_fallback"
        } else {
            "users_tags"
        };
        let k = list.items.len().div_ceil(TIER_PAGE_SIZE) as u32;
        if page > k {
            let mut l = self
                .search_page(fp, words, "date_d", mode, kind, page - k)
                .await?;
            let shown: HashSet<&str> = list.items.iter().map(|s| s.id.as_str()).collect();
            l.items.retain(|s| !shown.contains(s.id.as_str()));
            l.page = page;
            l.total = l.total.or(list.total);
            l.num_pages = l.num_pages.map(|n| n + k as u64);
            return Ok((l, used));
        }
        // The plain query's first page: fetched for the last tier page (does anything follow?),
        // otherwise only read from the cache for the totals.
        let plain = if page == k {
            Some(self.search_page(fp, words, "date_d", mode, kind, 1).await?)
        } else {
            self.cache.get_json::<Listing>(
                &search_key(fp, words, "date_d", mode, kind, 1),
                self.cfg.list_ttl,
            )
        };
        let mut l = tier_page(&list, page);
        if let Some(plain) = plain {
            if page == k {
                l.has_more = !plain.items.is_empty() || plain.num_pages.unwrap_or(0) > 0;
            }
            l.total = plain.total.or(list.total);
            l.num_pages = plain.num_pages.map(|n| n + k as u64).or(l.num_pages);
        }
        Ok((l, used))
    }

    /// The upstream pages of the tier query. A failure after the first page keeps what was read
    /// (`complete == false`: not cached).
    async fn fetch_tiers(
        &self,
        words: &[String],
        mode: &str,
        kind: &str,
    ) -> Result<(TierList, bool), SvcError> {
        let mut tier_words = words.to_vec();
        tier_words.extend(tier_group());
        let mut items: Vec<Summary> = Vec::new();
        let mut total = None;
        let mut last = 1;
        let mut complete = true;
        let mut n = 1u64;
        while n <= last {
            let args = search_args(&tier_words, "date_d", mode, kind, n as u32);
            let l = match self.gate.call(&self.backend, &Op::Search { args }).await {
                Ok(Value::Null) => empty_listing(n as u32),
                Ok(v) => parse_search(&v, n as u32),
                Err(e) if n == 1 => return Err(e),
                Err(_) => {
                    complete = false;
                    break;
                }
            };
            if n == 1 {
                total = l.total;
                last = l.num_pages.unwrap_or(1).min(TIER_PAGES);
            }
            items.extend(l.items);
            n += 1;
        }
        Ok((
            TierList {
                items: by_tier(items),
                total,
            },
            complete,
        ))
    }

    // ---- one work --------------------------------------------------------------------------

    pub async fn illust(&self, id: &str, adult: bool) -> Result<Illust, SvcError> {
        if !valid_id(id) {
            return Err(SvcError::BadRequest("id"));
        }
        let key = digest(&format!("illust/{id}"));
        let illust = match self.cache.get_json::<Illust>(&key, self.cfg.illust_ttl) {
            Some(i) => i,
            None => {
                let v = self
                    .gate
                    .call(&self.backend, &Op::Illust { id: id.to_string() })
                    .await?;
                let i = parse_illust(&v).ok_or(SvcError::Upstream)?;
                self.cache.put_json(&key, &i);
                i
            }
        };
        if illust.r18 && !adult {
            return Err(SvcError::AdultOff);
        }
        Ok(illust)
    }

    // ---- images ----------------------------------------------------------------------------

    /// A display image by its upstream URL (the signed route has already vouched for the URL).
    pub async fn image(&self, url: &str) -> Result<(Vec<u8>, String), SvcError> {
        if allowed(url).is_none() {
            return Err(SvcError::BadRequest("host not allowed"));
        }
        let key = digest(url);
        if let Some(hit) = self.cache.get_image(&key) {
            return Ok(hit);
        }
        let (bytes, ctype) = self.images.fetch(url, MAX_DISPLAY_IMAGE).await?;
        self.cache.put_image(&key, &bytes, &ctype);
        Ok((bytes, ctype))
    }

    // ---- save to Lucina --------------------------------------------------------------------

    /// Downloads the original pages of a work (all, or the given zero-based indexes) into the
    /// spool and hands each to lucina-media's loopback ingest (`move: true`). Per-page failures
    /// are reported, not fatal.
    pub async fn save(
        &self,
        id: &str,
        pages: Option<Vec<usize>>,
        adult: bool,
    ) -> Result<SaveOutcome, SvcError> {
        let illust = self.illust(id, adult).await?;
        let total = illust.pages.len();
        if total == 0 {
            return Err(SvcError::NotFound);
        }
        let wanted: Vec<usize> = match pages {
            None => (0..total).collect(),
            Some(mut p) => {
                p.sort_unstable();
                p.dedup();
                if p.is_empty() || p.iter().any(|i| *i >= total) {
                    return Err(SvcError::BadRequest("pages"));
                }
                p
            }
        };
        if wanted.len() > MAX_SAVE_PAGES {
            return Err(SvcError::BadRequest("pages"));
        }
        let caption = format!(
            "{} — {} (pixiv #{})",
            illust.title.trim(),
            illust.author.trim(),
            illust.id
        );
        let taken_at = chrono::DateTime::parse_from_rfc3339(&illust.created)
            .ok()
            .map(|d| d.to_rfc3339());
        let mut out = SaveOutcome {
            saved: vec![],
            failed: vec![],
        };
        for i in wanted {
            let page = &illust.pages[i];
            let url = if page.orig.is_empty() {
                &page.regular
            } else {
                &page.orig
            };
            let (bytes, ctype) = match self.images.fetch(url, MAX_ORIGINAL).await {
                Ok(x) => x,
                Err(SvcError::RateLimited(s)) => {
                    out.failed.push(FailedItem {
                        page: i,
                        error: format!("rate_limited:{s}"),
                    });
                    continue;
                }
                Err(_) => {
                    out.failed.push(FailedItem {
                        page: i,
                        error: "download_failed".into(),
                    });
                    continue;
                }
            };
            let ext = ext_of(url, &ctype);
            let name = format!("pixiv_{}_p{}.{}", illust.id, i, ext);
            let path = self.spool.join(&name);
            if write_private(&path, &bytes).is_err() {
                out.failed.push(FailedItem {
                    page: i,
                    error: "spool_failed".into(),
                });
                continue;
            }
            let body = json!({
                "path": path,
                "name": name,
                "mime": mime_of(ext),
                "source": "pixiv",
                "album": "Pixiv",
                "caption": caption,
                "taken_at": taken_at,
                "move": true,
            });
            let resp = self
                .http
                .post(format!(
                    "{}/internal/v1/ingest",
                    self.cfg.media_url.trim_end_matches('/')
                ))
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    let v: Value = r.json().await.unwrap_or(Value::Null);
                    match v["item_id"].as_str() {
                        Some(item) => out.saved.push(SavedItem {
                            page: i,
                            item_id: item.to_string(),
                            duplicate: v["duplicate"].as_bool().unwrap_or(false),
                        }),
                        None => out.failed.push(FailedItem {
                            page: i,
                            error: "ingest_failed".into(),
                        }),
                    }
                }
                _ => out.failed.push(FailedItem {
                    page: i,
                    error: "ingest_failed".into(),
                }),
            }
            // `move: true` takes the file; anything left in the spool is ours to remove.
            let _ = std::fs::remove_file(&path);
        }
        Ok(out)
    }
}

/// The user's words as the core's word arguments (a word never starts with "--", so it cannot
/// become a flag).
fn query_words(q: &str) -> Vec<String> {
    q.split_whitespace()
        .map(|w| {
            if w.starts_with("--") {
                w.trim_start_matches('-')
            } else {
                w
            }
        })
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn search_key(
    fp: &str,
    words: &[String],
    order: &str,
    mode: &str,
    kind: &str,
    page: u32,
) -> String {
    digest(&format!(
        "search/{fp}/{}/{order}/{mode}/{kind}/{page}",
        words.join(" ")
    ))
}

/// The core takes CLI-style arguments: words first, then the flags.
fn search_args(words: &[String], order: &str, mode: &str, kind: &str, page: u32) -> Vec<String> {
    let mut args = words.to_vec();
    args.extend([
        "--order".into(),
        order.to_string(),
        "--mode".into(),
        mode.to_string(),
        "--type".into(),
        kind.to_string(),
        "--page".into(),
        page.to_string(),
    ]);
    args
}

/// `(100000users入り OR ... OR 100users入り)` as separate words (the core joins them with spaces).
fn tier_group() -> Vec<String> {
    let mut out = Vec::with_capacity(TIERS.len() * 2);
    for (i, t) in TIERS.iter().enumerate() {
        if i > 0 {
            out.push("OR".to_string());
        }
        let open = if i == 0 { "(" } else { "" };
        let close = if i + 1 == TIERS.len() { ")" } else { "" };
        out.push(format!("{open}{t}users入り{close}"));
    }
    out
}

/// Deduplicated by id, sorted by bookmark tier, then id (numeric), both descending.
fn by_tier(items: Vec<Summary>) -> Vec<Summary> {
    let mut seen = HashSet::new();
    let mut items: Vec<Summary> = items
        .into_iter()
        .filter(|s| seen.insert(s.id.clone()))
        .collect();
    let id_num = |s: &Summary| s.id.parse::<u64>().unwrap_or(0);
    items.sort_by(|a, b| {
        b.popular_tier()
            .cmp(&a.popular_tier())
            .then_with(|| id_num(b).cmp(&id_num(a)))
    });
    items
}

/// One local page of the merged tier list.
fn tier_page(list: &TierList, page: u32) -> Listing {
    let n = list.items.len();
    let pages = n.div_ceil(TIER_PAGE_SIZE);
    let from = ((page as usize - 1) * TIER_PAGE_SIZE).min(n);
    let to = (from + TIER_PAGE_SIZE).min(n);
    Listing {
        items: list.items[from..to].to_vec(),
        page,
        has_more: (page as usize) < pages,
        total: list.total,
        num_pages: Some(pages as u64),
        notice: None,
        sort: None,
    }
}

/// Why an R-18 list cannot be had from this account, if it cannot.
fn r18_refusal(a: &Account) -> Option<&'static str> {
    if !a.usable() {
        Some(R18_NEEDS_SESSION)
    } else if a.r18 == Some(false) {
        Some(R18_DISABLED_IN_ACCOUNT)
    } else {
        None
    }
}

fn notice_listing(page: u32, notice: &str) -> Listing {
    Listing {
        notice: Some(notice.to_string()),
        ..empty_listing(page)
    }
}

/// The content filter, plus the after-the-fact notice of an account whose R-18 display is
/// unknown: R-18 asked, an upstream page with items, none of them R-18.
fn filtered(l: Listing, content: Content, account: &Account) -> Listing {
    let had_items = !l.items.is_empty();
    let mut l = filter_content(l, content);
    if content == Content::R18 && account.r18.is_none() && had_items && l.items.is_empty() {
        l.notice = Some(R18_DISABLED_IN_ACCOUNT.into());
    }
    l
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_follow_the_url_then_the_type() {
        assert_eq!(ext_of("https://i.pximg.net/a_p0.PNG", "image/jpeg"), "png");
        assert_eq!(ext_of("https://i.pximg.net/a_p0", "image/webp"), "webp");
        assert_eq!(ext_of("https://i.pximg.net/a_p0.jpeg?x=1", ""), "jpg");
    }
}
