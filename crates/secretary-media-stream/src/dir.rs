//! A read-only [`MediaSource`] over folders on disk, with a minimal listing
//! (`GET /v1/media/items`, `GET /v1/media/items/{id}`).
//!
//! This is the provisional library until `lucina-media` (catalog, uploads,
//! albums, search) replaces it; the routes and JSON match its contract.
use crate::{probe, MediaSource, Signer, SourceItem, StreamConfig};
use axum::{
    extract::{Path as UrlPath, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct Root {
    /// Shown as the album name.
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
struct Entry {
    item: SourceItem,
    album: String,
    size: u64,
    modified: u64,
}

pub struct DirSource {
    roots: Vec<Root>,
    scan: Mutex<Option<(Instant, Arc<Vec<Entry>>)>>,
}

const RESCAN_AFTER: Duration = Duration::from_secs(60);

pub fn kind_and_mime(ext: &str) -> Option<(&'static str, &'static str)> {
    Some(match ext {
        "mp4" | "m4v" => ("video", "video/mp4"),
        "mov" => ("video", "video/quicktime"),
        "mkv" => ("video", "video/x-matroska"),
        "webm" => ("video", "video/webm"),
        "avi" => ("video", "video/x-msvideo"),
        "mpg" | "mpeg" | "vob" => ("video", "video/mpeg"),
        "ts" | "m2ts" | "mts" => ("video", "video/mp2t"),
        "wmv" => ("video", "video/x-ms-wmv"),
        "mp3" => ("audio", "audio/mpeg"),
        "m4a" | "m4b" => ("audio", "audio/mp4"),
        "aac" => ("audio", "audio/aac"),
        "flac" => ("audio", "audio/flac"),
        "wav" => ("audio", "audio/wav"),
        "aif" | "aiff" => ("audio", "audio/aiff"),
        "ogg" | "oga" | "opus" => ("audio", "audio/ogg"),
        "jpg" | "jpeg" => ("photo", "image/jpeg"),
        "png" => ("photo", "image/png"),
        "heic" => ("photo", "image/heic"),
        "gif" => ("photo", "image/gif"),
        "webp" => ("photo", "image/webp"),
        _ => return None,
    })
}

fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn item_id(label: &str, rel: &Path) -> String {
    let mut h = Sha256::new();
    h.update(label.as_bytes());
    h.update(b"/");
    h.update(rel.as_os_str().as_encoded_bytes());
    probe::hex(&h.finalize()[..8])
}

impl DirSource {
    pub fn new(roots: Vec<Root>) -> Self {
        Self { roots, scan: Mutex::new(None) }
    }

    fn entries(&self) -> Arc<Vec<Entry>> {
        let mut guard = self.scan.lock().expect("scan lock");
        if let Some((at, entries)) = guard.as_ref() {
            if at.elapsed() < RESCAN_AFTER {
                return Arc::clone(entries);
            }
        }
        let entries = Arc::new(self.scan_now());
        *guard = Some((Instant::now(), Arc::clone(&entries)));
        entries
    }

    fn scan_now(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for root in &self.roots {
            let mut stack = vec![root.path.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(read) = std::fs::read_dir(&dir) else { continue };
                let mut files = Vec::new();
                for e in read.flatten() {
                    let p = e.path();
                    if e.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    // metadata() follows symlinks, so a folder of links works as a library.
                    match std::fs::metadata(&p) {
                        Ok(m) if m.is_dir() => stack.push(p),
                        Ok(m) if m.is_file() => files.push(p),
                        _ => {}
                    }
                }
                for p in &files {
                    let Some((kind, mime)) = kind_and_mime(&ext_of(p)) else { continue };
                    let Ok(meta) = std::fs::metadata(p) else { continue };
                    let rel = p.strip_prefix(&root.path).unwrap_or(p);
                    let title = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    out.push(Entry {
                        item: SourceItem {
                            id: item_id(&root.label, rel),
                            kind: kind.into(),
                            title,
                            path: p.clone(),
                            mime: mime.into(),
                            poster: None,
                            thumb: None,
                            // The router finds subtitle files next to the video itself.
                            subtitles: vec![],
                        },
                        album: root.label.clone(),
                        size: meta.len(),
                        modified: meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_millis() as u64),
                    });
                }
            }
        }
        out.sort_by(|a, b| (&a.album, &a.item.title).cmp(&(&b.album, &b.item.title)));
        out
    }
}

impl MediaSource for DirSource {
    async fn item(&self, id: &str) -> Option<SourceItem> {
        self.entries().iter().find(|e| e.item.id == id).map(|e| e.item.clone())
    }
}

#[derive(Serialize)]
struct ItemOut {
    id: String,
    kind: String,
    title: String,
    mime: String,
    size: u64,
    album: String,
    modified_at: u64,
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    poster_url: String,
    thumb_url: String,
}

#[derive(Deserialize)]
struct ListQuery {
    kind: Option<String>,
    q: Option<String>,
    album: Option<String>,
    #[serde(default)]
    cursor: usize,
    limit: Option<usize>,
}

struct ListState {
    src: Arc<DirSource>,
    signer: Signer,
    cfg: StreamConfig,
}

/// The listing routes. Durations come from the probe cache; files not probed
/// yet are probed in the background and show their duration next time.
pub fn items_router(src: Arc<DirSource>, cfg: StreamConfig) -> Router {
    let state = Arc::new(ListState { src, signer: Signer::new(&cfg.secret, cfg.token_ttl), cfg });
    Router::new().route("/v1/media/items", get(list)).route("/v1/media/items/{id}", get(one)).with_state(state)
}

async fn describe(state: &ListState, e: &Entry) -> ItemOut {
    let cached = probe_cached(&state.cfg, &e.item.path).await;
    let (base, _) = state.signer.base(&e.item.id);
    ItemOut {
        id: e.item.id.clone(),
        kind: e.item.kind.clone(),
        title: e.item.title.clone(),
        mime: e.item.mime.clone(),
        size: e.size,
        album: e.album.clone(),
        modified_at: e.modified,
        duration_ms: cached.as_ref().and_then(|i| i.duration_ms),
        width: cached.as_ref().and_then(|i| i.width),
        height: cached.as_ref().and_then(|i| i.height),
        poster_url: format!("{base}/poster.jpg"),
        thumb_url: format!("{base}/thumb.jpg"),
    }
}

async fn probe_cached(cfg: &StreamConfig, path: &Path) -> Option<probe::Info> {
    let key = probe::file_key(path).ok()?;
    let bytes = tokio::fs::read(cfg.cache_dir.join("probe").join(format!("{key}.json"))).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn list(State(state): State<Arc<ListState>>, Query(q): Query<ListQuery>) -> Json<serde_json::Value> {
    let entries = state.src.entries();
    let needle = q.q.as_deref().map(str::to_lowercase);
    let matching: Vec<&Entry> = entries
        .iter()
        .filter(|e| q.kind.as_deref().is_none_or(|k| e.item.kind == k))
        .filter(|e| q.album.as_deref().is_none_or(|a| e.album == a))
        .filter(|e| needle.as_deref().is_none_or(|n| e.item.title.to_lowercase().contains(n)))
        .collect();
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let page: Vec<&Entry> = matching.iter().skip(q.cursor).take(limit).copied().collect();
    let mut items = Vec::with_capacity(page.len());
    let mut unprobed = Vec::new();
    for e in &page {
        let out = describe(&state, e).await;
        if out.duration_ms.is_none() && e.item.kind != "photo" {
            unprobed.push(e.item.path.clone());
        }
        items.push(out);
    }
    if !unprobed.is_empty() {
        let (ffprobe, cache) = (state.cfg.ffprobe.clone(), state.cfg.cache_dir.clone());
        tokio::spawn(async move {
            for p in unprobed {
                let _ = probe::probe(&ffprobe, &cache, &p).await;
            }
        });
    }
    let next = q.cursor + page.len();
    Json(serde_json::json!({
        "ok": true,
        "items": items,
        "next_cursor": if next < matching.len() { Some(next.to_string()) } else { None },
        "albums": state.src.roots.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
    }))
}

async fn one(State(state): State<Arc<ListState>>, UrlPath(id): UrlPath<String>) -> Result<Json<ItemOut>, StatusCode> {
    let entries = state.src.entries();
    let e = entries.iter().find(|e| e.item.id == id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(describe(&state, e).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        assert_eq!(item_id("Movies", Path::new("a.mkv")), item_id("Movies", Path::new("a.mkv")));
        assert_ne!(item_id("Movies", Path::new("a.mkv")), item_id("Films", Path::new("a.mkv")));
        assert_eq!(item_id("Movies", Path::new("a.mkv")).len(), 16);
    }
}
