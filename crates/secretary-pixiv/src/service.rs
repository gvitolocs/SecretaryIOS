//! The plugin's logic: validation, the adult gate, the JSON cache, upstream calls through the gate,
//! image fetching through the cache, and saving originals into lucina-media.
//!
//! The adult gate lives here, not in the HTTP layer: R-18 ranking modes and searches, R-18
//! works (detail, bookmark, save) answer `AdultOff` unless the caller passed `adult = true`, and
//! R-18 items are filtered out of every list in that case.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend::Backend;
use crate::cache::DiskCache;
use crate::error::SvcError;
use crate::images::{allowed, ImageConfig, ImageFetcher};
use crate::model::{
    empty_listing, parse_illust, parse_ranking, parse_search, Illust, Listing, Summary,
};
use crate::upstream::{ApiGate, GateConfig, Op};

pub const MAX_DISPLAY_IMAGE: usize = 32 * 1024 * 1024;
pub const MAX_ORIGINAL: usize = 64 * 1024 * 1024;
const MAX_SAVE_PAGES: usize = 200;

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

pub struct Service {
    backend: Arc<dyn Backend>,
    gate: ApiGate,
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
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
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

    // ---- lists -----------------------------------------------------------------------------

    pub async fn ranking(&self, mode: &str, page: u32, adult: bool) -> Result<Listing, SvcError> {
        if !RANKING_MODES.contains(&mode) {
            return Err(SvcError::BadRequest("mode"));
        }
        let r18 = mode.ends_with("_r18");
        if r18 && !adult {
            return Err(SvcError::AdultOff);
        }
        let page = page.clamp(1, 100);
        let key = digest(&format!("ranking/{mode}/{page}"));
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
        Ok(gate_list(listing, adult))
    }

    pub async fn search(&self, p: &SearchParams, adult: bool) -> Result<Listing, SvcError> {
        let q = p.q.trim();
        if q.is_empty() || q.chars().count() > 200 {
            return Err(SvcError::BadRequest("q"));
        }
        if !matches!(p.order.as_str(), "date_d" | "date") {
            return Err(SvcError::BadRequest("order"));
        }
        if !matches!(p.mode.as_str(), "safe" | "all" | "r18") {
            return Err(SvcError::BadRequest("mode"));
        }
        if !matches!(p.kind.as_str(), "all" | "illust" | "manga" | "ugoira") {
            return Err(SvcError::BadRequest("type"));
        }
        if p.mode == "r18" && !adult {
            return Err(SvcError::AdultOff);
        }
        let page = p.page.clamp(1, 1000);
        // Without adult, "all" asks pixiv for the all-ages results only.
        let mode = if !adult && p.mode == "all" { "safe" } else { p.mode.as_str() };
        let key = digest(&format!("search/{q}/{}/{mode}/{}/{page}", p.order, p.kind));
        let listing = match self.cache.get_json::<Listing>(&key, self.cfg.list_ttl) {
            Some(l) => l,
            None => {
                // The core takes CLI-style arguments: words first (a word never starts with
                // "--"), then the flags.
                let mut args: Vec<String> = q
                    .split_whitespace()
                    .map(|w| {
                        if w.starts_with("--") {
                            w.trim_start_matches('-').to_string()
                        } else {
                            w.to_string()
                        }
                    })
                    .filter(|w| !w.is_empty())
                    .collect();
                if args.is_empty() {
                    return Err(SvcError::BadRequest("q"));
                }
                args.extend([
                    "--order".into(),
                    p.order.clone(),
                    "--mode".into(),
                    mode.to_string(),
                    "--type".into(),
                    p.kind.clone(),
                    "--page".into(),
                    page.to_string(),
                ]);
                let l = match self.gate.call(&self.backend, &Op::Search { args }).await {
                    Ok(Value::Null) => empty_listing(page),
                    Ok(v) => parse_search(&v, page),
                    Err(e) => return Err(e),
                };
                self.cache.put_json(&key, &l);
                l
            }
        };
        Ok(gate_list(listing, adult))
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
            let url = if page.orig.is_empty() { &page.regular } else { &page.orig };
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

fn gate_list(mut l: Listing, adult: bool) -> Listing {
    if !adult {
        l.items.retain(|s: &Summary| !s.r18);
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
