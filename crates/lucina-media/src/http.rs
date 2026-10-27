//! HTTP API. `/v1/media/*` is served on the tailnet listener behind [`upstream_guard`] (the p1
//! proxy authenticates the device and adds the upstream key). `/internal/*` is loopback only.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::db::{self, now_str, Item, ListQuery};
use crate::doctype::{self, DocType};
use crate::drive::{self, DriveInfo};
use crate::ingest::{self, ExternalReq, Library};
use crate::meta::ClientMeta;
use crate::protocol::{self, ChunkOutcome, UploadState};
use crate::thumbs::{self, ThumbSize};

/// Sent by p1's Caddy (`header_up X-Media-Upstream-Key {file./etc/caddy/media-upstream.key}`).
pub const UPSTREAM_KEY_HEADER: &str = "x-media-upstream-key";
pub const MAX_PREVIEW_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(e: E) -> Self {
        tracing::warn!(error = %e, "request failed");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

pub type ApiResult<T> = Result<T, ApiError>;

/// The public (tailnet) API. `extra` is the player's streaming router (play, `/s/…`, progress);
/// it sits behind the same guard.
pub fn router(lib: Library, extra: Option<Router>) -> Router {
    let uploads = Router::new()
        .route("/v1/media/uploads/{id}", put(put_chunk).get(get_upload))
        .route("/v1/media/uploads/{id}/preview", put(put_preview))
        .layer(DefaultBodyLimit::disable());
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/media/items", get(list_items))
        .route(
            "/v1/media/items/{id}",
            get(get_item).patch(patch_item).delete(trash_item),
        )
        .route("/v1/media/items/{id}/restore", post(restore_item))
        .route("/v1/media/items/{id}/original", get(original))
        .route("/v1/media/items/{id}/thumb", get(thumb))
        .route("/v1/media/items/{id}/poster", get(poster))
        .route("/v1/media/items/{id}/r/{rendition}", get(rendition))
        .route("/v1/media/items/{id}/text", get(item_text))
        .route("/v1/media/items/{id}/pdf", get(item_pdf))
        .route("/v1/media/items/{id}/archive", get(item_archive))
        .route("/v1/media/drive", get(drive_roots))
        .route("/v1/media/drive/browse", get(drive_browse))
        // The same renditions behind a play token: p1 passes /v1/media/s/* straight through, so a
        // gallery page costs no device-rate-limit tokens (60 a minute) and needs no header.
        .route(
            "/v1/media/s/{token}/{id}/r/{rendition}",
            get(signed_rendition),
        )
        .route("/v1/media/summary", get(summary))
        .route("/v1/media/albums", get(list_albums).post(create_album))
        .route(
            "/v1/media/albums/{id}",
            get(get_album).patch(patch_album).delete(delete_album),
        )
        .route(
            "/v1/media/albums/{id}/items",
            post(album_add).put(album_replace).delete(album_remove),
        )
        .route("/v1/media/uploads", post(create_upload))
        .route("/v1/media/uploads/{id}/finish", post(finish_upload))
        .route("/v1/media/devices", get(list_devices))
        .route("/v1/media/devices/{device}", put(register_device))
        .route("/v1/media/devices/{device}/assets", get(device_assets))
        .route("/v1/media/events", get(events))
        .merge(uploads)
        .merge(crate::share_http::owner_routes())
        .merge(crate::share_http::public_routes())
        .with_state(lib.clone())
        .merge(extra.unwrap_or_default())
        .layer(middleware::from_fn_with_state(lib, upstream_guard))
}

/// Loopback-only routes for services on nezopt (voice memos, imports).
pub fn internal_router(lib: Library) -> Router {
    Router::new()
        .route(
            "/internal/v1/external/{source}/{ext_id}",
            put(put_external).delete(delete_external),
        )
        .route("/internal/v1/import", post(start_import))
        .route("/internal/v1/ingest", post(ingest_local))
        .route("/internal/v1/items/{id}/path", get(item_path))
        .with_state(lib)
}

/// Requests must carry the p1 proxy's key. The player's tokenised `/v1/media/s/…` URLs and the
/// health check are exempt (the token is their auth).
pub async fn upstream_guard(State(lib): State<Library>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // Play tokens and album share links carry their own credential (`share_http::gate`).
    if path == "/healthz" || path.starts_with("/v1/media/s/") || path.starts_with("/v1/media/a/") {
        return next.run(req).await;
    }
    if let Some(key) = lib.cfg.upstream_key.as_deref() {
        let given = req
            .headers()
            .get(UPSTREAM_KEY_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or(b"");
        if !bool::from(given.ct_eq(key.as_bytes())) {
            return ApiError(
                StatusCode::UNAUTHORIZED,
                "missing or wrong upstream key".into(),
            )
            .into_response();
        }
    }
    next.run(req).await
}

#[derive(Serialize)]
pub struct ItemOut {
    #[serde(flatten)]
    pub item: Item,
    pub urls: Value,
    /// Tokenised URLs (no header needed), as the player apps expect.
    pub poster_url: Option<String>,
    pub thumb_url: Option<String>,
    /// First album title, for the player's grouping.
    pub album: Option<String>,
    /// The file's place in a Drive folder; `null` for other items.
    pub drive: Option<DriveInfo>,
    /// Text around the search words, when a search matched the file's content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

pub fn item_out(lib: &Library, it: Item, album: Option<String>) -> ItemOut {
    item_out_full(lib, it, album, None, None)
}

/// The document type of an item: its Drive type, or what its name says.
fn doc_type_of(it: &Item, drive: Option<&DriveInfo>) -> DocType {
    drive
        .and_then(|d| DocType::parse(&d.doc_type))
        .unwrap_or_else(|| doctype::classify(&it.title, &it.kind))
}

pub fn item_out_full(
    lib: &Library,
    it: Item,
    album: Option<String>,
    drive: Option<DriveInfo>,
    snippet: Option<String>,
) -> ItemOut {
    let (poster_url, thumb_url) = match &lib.signer {
        Some(s) => {
            let (base, _) = s.base(&it.id);
            let has_visual =
                it.thumb_state == "ready" || matches!(it.kind.as_str(), "video" | "audio");
            (
                has_visual.then(|| format!("{base}/poster.jpg")),
                has_visual.then(|| format!("{base}/thumb.jpg")),
            )
        }
        None => (None, None),
    };
    let id = &it.id;
    let v = it.version;
    let has_thumb = it.thumb_state == "ready";
    let urls = json!({
        "original": format!("/v1/media/items/{id}/original"),
        "thumb": has_thumb.then(|| format!("/v1/media/items/{id}/thumb?size=256&v={v}")),
        "large": has_thumb.then(|| format!("/v1/media/items/{id}/thumb?size=1024&v={v}")),
        "poster": (has_thumb && matches!(it.kind.as_str(), "video" | "audio")).then(|| format!("/v1/media/items/{id}/poster?v={v}")),
        // JPEG XL renditions for the Rust viewer: 256 px grid tile, 1440 px progressive preview.
        // Tokenised when the play secret is configured (no device rate limit, no header).
        "grid": has_thumb.then(|| rendition_url(lib, id, "grid", v)),
        "preview": has_thumb.then(|| rendition_url(lib, id, "preview", v)),
        "play": matches!(it.kind.as_str(), "video" | "audio").then(|| format!("/v1/media/items/{id}/play")),
    });
    let mut urls = urls;
    let doc = doc_type_of(&it, drive.as_ref());
    let name = drive
        .as_ref()
        .map_or(it.title.as_str(), |d| d.name.as_str());
    let office = matches!(
        doc,
        DocType::Document | DocType::Spreadsheet | DocType::Presentation
    );
    let texty = doc.has_text() || drive.as_ref().is_some_and(|d| d.preview == "text");
    urls["download"] = json!(format!("/v1/media/items/{id}/original?download=1"));
    urls["pdf"] = json!(
        (doc == DocType::Pdf || (office && lib.cfg.soffice.is_some()))
            .then(|| format!("/v1/media/items/{id}/pdf"))
    );
    urls["text"] = json!(texty.then(|| format!("/v1/media/items/{id}/text")));
    urls["archive"] = json!(
        (doc == DocType::Archive && crate::archive::format_of(name).is_some())
            .then(|| format!("/v1/media/items/{id}/archive"))
    );
    ItemOut {
        item: it,
        urls,
        poster_url,
        thumb_url,
        album,
        drive,
        snippet,
    }
}

fn rendition_url(lib: &Library, id: &str, which: &str, v: i64) -> String {
    match &lib.signer {
        Some(s) => format!("{}/r/{which}?v={v}", s.base(id).0),
        None => format!("/v1/media/items/{id}/r/{which}?v={v}"),
    }
}

/// Album titles for items, user albums first, for `ItemOut.album`.
fn first_album_titles(
    c: &rusqlite::Connection,
    items: &[Item],
) -> rusqlite::Result<Vec<Option<String>>> {
    let mut st = c.prepare_cached(
        "SELECT a.title FROM album_items ai JOIN albums a ON a.id = ai.album_id WHERE ai.item_id = ?1
         ORDER BY a.system_key IS NOT NULL, ai.added_at LIMIT 1",
    )?;
    items
        .iter()
        .map(|it| st.query_row([&it.id], |r| r.get(0)).optional())
        .collect()
}

async fn list_items(
    State(lib): State<Library>,
    Query(mut q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    q.film_folders = lib.cfg.film_folders.clone();
    if q.doc_type
        .as_deref()
        .is_some_and(|t| DocType::parse(t).is_none())
    {
        return Err(bad("unknown type"));
    }
    let words = search_words(q.q.as_deref());
    let (page, titles, albums, infos) = lib
        .db
        .call(move |c| {
            let page = db::list_items(c, &q)?;
            let titles = first_album_titles(c, &page.items)?;
            let albums: Vec<String> = db::list_albums(c)?
                .into_iter()
                .filter(|a| a.count > 0)
                .map(|a| a.title)
                .collect();
            let infos = page
                .items
                .iter()
                .map(|it| drive::info_for(c, &it.id))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((page, titles, albums, infos))
        })
        .await?;
    let out = with_snippets(&lib, page.items.into_iter().zip(infos).collect(), words).await;
    Ok(Json(json!({
        "ok": true,
        "items": out.into_iter().zip(titles).map(|((it, d, sn), a)| item_out_full(&lib, it, a, d, sn)).collect::<Vec<_>>(),
        "next_cursor": page.next_cursor,
        "total": page.total,
        "albums": albums,
    })))
}

async fn get_item(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<ItemOut>> {
    let (it, album, info) = lib
        .db
        .call(move |c| {
            let Some(it) = db::get_item(c, &id)? else {
                return Ok(None);
            };
            let a = first_album_titles(c, std::slice::from_ref(&it))?
                .pop()
                .flatten();
            let info = drive::info_for(c, &id)?;
            Ok(Some((it, a, info)))
        })
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(item_out_full(&lib, it, album, info, None)))
}

/// The words of a search, for snippets.
fn search_words(q: Option<&str>) -> Vec<String> {
    q.map(|s| {
        s.split(|ch: char| !ch.is_alphanumeric())
            .filter(|w| w.chars().count() >= 2)
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Snippets for the first results of a search that are Drive files with text (read from disk).
async fn with_snippets(
    lib: &Library,
    items: Vec<(Item, Option<DriveInfo>)>,
    words: Vec<String>,
) -> Vec<(Item, Option<DriveInfo>, Option<String>)> {
    if words.is_empty() {
        return items.into_iter().map(|(i, d)| (i, d, None)).collect();
    }
    let l = lib.clone();
    tokio::task::spawn_blocking(move || {
        items
            .into_iter()
            .enumerate()
            .map(|(n, (it, d))| {
                let sn = d
                    .as_ref()
                    .filter(|_| n < 50)
                    .and_then(|info| drive::snippet_for(&l, &it.id, info, &words));
                (it, d, sn)
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[derive(Deserialize)]
struct ItemPatch {
    title: Option<String>,
    caption: Option<String>,
    favorite: Option<bool>,
    taken_at: Option<String>,
}

async fn patch_item(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(p): Json<ItemPatch>,
) -> ApiResult<Json<ItemOut>> {
    let taken = match p.taken_at.as_deref() {
        Some(t) => Some(
            crate::meta::parse_loose_datetime(t).ok_or_else(|| bad("taken_at must be RFC 3339"))?,
        ),
        None => None,
    }
    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let iid = id.clone();
    let it = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let n = tx.execute(
                "UPDATE items SET title = COALESCE(?2, title), caption = COALESCE(?3, caption),
                    favorite = COALESCE(?4, favorite), taken_at = COALESCE(?5, taken_at), sort_at = COALESCE(?5, sort_at),
                    updated_at = ?6 WHERE id = ?1",
                params![iid, p.title, p.caption, p.favorite.map(|f| f as i64), taken, now_str()],
            )?;
            if n == 0 {
                return Ok(None);
            }
            match p.favorite {
                Some(true) => {
                    db::add_to_album(&tx, "sys-favorites", &iid)?;
                }
                Some(false) => {
                    tx.execute("DELETE FROM album_items WHERE album_id = 'sys-favorites' AND item_id = ?1", [&iid])?;
                }
                None => {}
            }
            db::reindex(&tx, &iid)?;
            let it = db::get_item(&tx, &iid)?;
            tx.commit()?;
            Ok(it)
        })
        .await?
        .ok_or_else(not_found)?;
    lib.emit("item.updated", &id);
    Ok(Json(item_out(&lib, it, None)))
}

/// Soft delete: the original stays on disk; the item leaves the timeline. Never touches phones.
async fn trash_item(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let iid = id.clone();
    let n = lib
        .db
        .call(move |c| {
            c.execute(
                "UPDATE items SET trashed_at = ?2, updated_at = ?2 WHERE id = ?1 AND trashed_at IS NULL",
                params![iid, now_str()],
            )
        })
        .await?;
    if n == 0 {
        return Err(not_found());
    }
    lib.emit("item.trashed", &id);
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_item(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> ApiResult<Json<ItemOut>> {
    let iid = id.clone();
    let it = lib
        .db
        .call(move |c| {
            c.execute(
                "UPDATE items SET trashed_at = NULL, updated_at = ?2 WHERE id = ?1",
                params![iid, now_str()],
            )?;
            db::get_item(c, &iid)
        })
        .await?
        .ok_or_else(not_found)?;
    lib.emit("item.updated", &id);
    Ok(Json(item_out(&lib, it, None)))
}

pub async fn serve_file(
    req_headers: HeaderMap,
    path: PathBuf,
    mime: &str,
    etag: &str,
    cache: &str,
) -> ApiResult<Response> {
    let etag = format!("\"{etag}\"");
    if req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .unwrap());
    }
    let mut req = Request::builder().uri("/");
    for (k, v) in req_headers.iter() {
        if matches!(
            k.as_str(),
            "range" | "if-range" | "if-modified-since" | "if-unmodified-since"
        ) {
            req = req.header(k, v);
        }
    }
    let svc = ServeFile::new_with_mime(
        &path,
        &mime
            .parse()
            .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM),
    );
    let mut res = svc
        .oneshot(req.body(Body::empty()).unwrap())
        .await?
        .map(Body::new);
    let h = res.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&etag)?);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_str(cache)?);
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(res)
}

#[derive(Deserialize)]
struct OriginalQuery {
    download: Option<String>,
}

/// `inline` or `attachment` with the file's name: ASCII for old clients, UTF-8 (RFC 5987) for the rest.
fn content_disposition(kind: &str, name: &str) -> Option<HeaderValue> {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\' && *c != '/')
        .collect();
    let ascii = clean.replace(|c: char| !c.is_ascii(), "_");
    let mut encoded = String::new();
    for b in clean.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .ok()
}

async fn original(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(oq): Query<OriginalQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let l = lib.clone();
    let iid = id.clone();
    let (path, item) = tokio::task::spawn_blocking(move || ingest::original_path(&l, &iid))
        .await??
        .ok_or_else(not_found)?;
    let info = lib.db.call(move |c| drive::info_for(c, &id)).await?;
    if !path.is_file() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "original is not reachable right now".into(),
        ));
    }
    let mut res = serve_file(
        headers,
        path,
        &item.mime,
        &item.hash,
        "private, max-age=31536000, immutable",
    )
    .await?;
    let kind = if matches!(oq.download.as_deref(), Some("1" | "true")) {
        "attachment"
    } else {
        "inline"
    };
    let name = info
        .as_ref()
        .map_or(item.title.as_str(), |d| d.name.as_str());
    if let Some(v) = content_disposition(kind, name) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

fn valid_item_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// An item, its Drive info, its document type, its name and its original's path.
async fn doc_item(
    lib: &Library,
    id: &str,
) -> ApiResult<(Item, Option<DriveInfo>, DocType, String, PathBuf)> {
    if !valid_item_id(id) {
        return Err(not_found());
    }
    let l = lib.clone();
    let iid = id.to_string();
    let (path, item) = tokio::task::spawn_blocking(move || ingest::original_path(&l, &iid))
        .await??
        .ok_or_else(not_found)?;
    if item.trashed_at.is_some() {
        return Err(not_found());
    }
    let iid = id.to_string();
    let info = lib.db.call(move |c| drive::info_for(c, &iid)).await?;
    let doc = doc_type_of(&item, info.as_ref());
    let name = info
        .as_ref()
        .map_or_else(|| item.title.clone(), |d| d.name.clone());
    if !path.is_file() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "original is not reachable right now".into(),
        ));
    }
    Ok((item, info, doc, name, path))
}

#[derive(Deserialize)]
struct TextQuery {
    highlight: Option<String>,
    max_bytes: Option<usize>,
}

/// The text of a text or code file (as it is, optionally highlighted), or the extracted text of a
/// PDF or Office file.
async fn item_text(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<TextQuery>,
) -> ApiResult<Response> {
    let (item, info, doc, name, path) = doc_item(&lib, &id).await?;
    let max = q
        .max_bytes
        .unwrap_or(1 << 20)
        .clamp(1024, crate::extract::MAX_TEXT_BYTES);
    let highlight = matches!(q.highlight.as_deref(), Some("1" | "true"));
    let extracted = matches!(
        doc,
        DocType::Pdf | DocType::Document | DocType::Spreadsheet | DocType::Presentation
    );
    if extracted {
        let out = thumbs::dir_for(&lib.cfg, &item.id).join("text.txt");
        if !out.is_file() {
            // Not read yet: read it now (one heavy job at a time; a busy worker means "later").
            let permit = match tokio::time::timeout(
                Duration::from_secs(30),
                lib.jobs.clone().acquire_owned(),
            )
            .await
            {
                Ok(p) => p.map_err(|e| ApiError::from(e.to_string()))?,
                Err(_) => {
                    return Ok(
                        (StatusCode::ACCEPTED, Json(json!({ "state": "pending" }))).into_response()
                    )
                }
            };
            let (cfg, o, n) = (lib.cfg.clone(), out.clone(), name.clone());
            let res = tokio::task::spawn_blocking(move || {
                std::fs::create_dir_all(o.parent().unwrap_or(std::path::Path::new(".")))?;
                crate::extract::extract_text(&cfg, &path, &n, doc, &o)
            })
            .await?;
            drop(permit);
            match res {
                Ok(true) => {}
                Ok(false) => {
                    return Err(ApiError(
                        StatusCode::NOT_FOUND,
                        "no text in this file".into(),
                    ))
                }
                Err(e) => {
                    tracing::warn!(item = %item.id, error = %e, "text extraction failed");
                    return Err(ApiError(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "the text can't be read".into(),
                    ));
                }
            }
        }
        let read = tokio::task::spawn_blocking(move || crate::extract::read_text_prefix(&out, max))
            .await??;
        let (text, truncated) =
            read.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no text".into()))?;
        return Ok(Json(json!({
            "language": null, "source": "extracted", "truncated": truncated,
            "bytes": text.len(), "text": text,
        }))
        .into_response());
    }
    let texty = doc.has_text()
        || info.as_ref().is_some_and(|d| d.preview == "text")
        || doc == DocType::Other;
    if !texty {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "no text in this file".into(),
        ));
    }
    let language = doctype::language_for(&name);
    let body = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Value>> {
        let Some((text, truncated)) = crate::extract::read_text_prefix(&path, max)? else {
            return Ok(None);
        };
        let lines = if highlight {
            language.and_then(|l| crate::highlight::highlight(&text, l))
        } else {
            None
        };
        let mut v = json!({
            "language": language, "source": "file", "truncated": truncated,
            "bytes": text.len(), "text": text,
        });
        if let Some(lines) = lines {
            v["lines"] = json!(lines
                .into_iter()
                .map(|l| l
                    .into_iter()
                    .map(|(c, t)| json!([c, t]))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>());
        }
        Ok(Some(v))
    })
    .await??;
    match body {
        Some(v) => Ok(Json(v).into_response()),
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            "no text in this file".into(),
        )),
    }
}

/// A PDF to show inline: the original of a PDF, a converted copy of an Office file.
async fn item_pdf(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (item, _info, doc, name, path) = doc_item(&lib, &id).await?;
    let etag = format!("{}-pdf", item.hash);
    let cache = "private, max-age=604800";
    match doc {
        DocType::Pdf => serve_file(headers, path, "application/pdf", &etag, cache).await,
        DocType::Document | DocType::Spreadsheet | DocType::Presentation => {
            let out = thumbs::dir_for(&lib.cfg, &item.id).join("doc.pdf");
            if !out.is_file() {
                if lib.cfg.soffice.is_none() {
                    return Err(ApiError(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "no converter".into(),
                    ));
                }
                let permit = lib
                    .jobs
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| ApiError::from(e.to_string()))?;
                let (cfg, o) = (lib.cfg.clone(), out.clone());
                let made = tokio::task::spawn_blocking(move || {
                    std::fs::create_dir_all(o.parent().unwrap_or(std::path::Path::new(".")))?;
                    crate::extract::office_pdf(&cfg, &path, &name, &o)
                })
                .await?;
                drop(permit);
                if !matches!(made, Ok(true)) {
                    return Err(ApiError(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "the file can't be converted".into(),
                    ));
                }
            }
            serve_file(headers, out, "application/pdf", &etag, cache).await
        }
        _ => Err(not_found()),
    }
}

async fn item_archive(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Response> {
    let (_item, _info, doc, name, path) = doc_item(&lib, &id).await?;
    if doc != DocType::Archive {
        return Err(not_found());
    }
    let res = tokio::task::spawn_blocking(move || crate::archive::list(&path, &name)).await?;
    match res {
        Ok(l) => Ok(Json(l).into_response()),
        Err(crate::archive::ArchiveError::Unsupported) => Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "this archive format can't be listed".into(),
        )),
        Err(crate::archive::ArchiveError::Corrupt(e)) => Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("the archive can't be read: {e}"),
        )),
        Err(e) => Err(e.into()),
    }
}

/// The Drive's top level: its folders, the other Drive collections, and which converters exist.
async fn drive_roots(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let folders: Vec<String> = drive::drive_folders(&lib)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    let films = lib.cfg.film_folders.clone();
    let f2 = folders.clone();
    let (roots, albums) = lib
        .db
        .call(move |c| {
            let roots = drive::roots(c, &f2)?;
            let albums: Vec<Value> = db::list_albums_in(c, Some("drive"), &films)?
                .into_iter()
                .filter(|a| {
                    !f2.contains(&a.title)
                        && a.system_key.as_deref().is_none_or(|k| k == "voice-memos")
                })
                .map(|a| json!({ "id": a.id, "title": a.title, "count": a.count }))
                .collect();
            Ok((roots, albums))
        })
        .await?;
    Ok(Json(json!({
        "folders": roots,
        "albums": albums,
        "converters": {
            "pdf_thumbs": lib.cfg.pdftoppm.is_some(),
            "office_pdf": lib.cfg.soffice.is_some(),
            "pdf_text": true,
        },
    })))
}

async fn drive_browse(
    State(lib): State<Library>,
    Query(q): Query<drive::BrowseQuery>,
) -> ApiResult<Json<Value>> {
    if !drive::drive_folders(&lib)
        .iter()
        .any(|(l, _)| *l == q.folder)
    {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "no such Drive folder".into(),
        ));
    }
    let (folder, path) = (q.folder.clone(), q.path.clone());
    let b = lib
        .db
        .call(move |c| Ok(drive::browse(c, &q)))
        .await?
        .map_err(|e| match e {
            drive::BrowseError::BadPath => bad("unsafe path"),
            drive::BrowseError::BadQuery => bad("unknown sort, order or type"),
            drive::BrowseError::Sql(e) => ApiError::from(e),
        })?;
    let words = b.words.clone();
    let files = with_snippets(
        &lib,
        b.files.into_iter().map(|(i, d)| (i, Some(d))).collect(),
        words,
    )
    .await;
    Ok(Json(json!({
        "folder": folder,
        "path": path,
        "breadcrumbs": b.breadcrumbs.iter().map(|(n, p)| json!({ "name": n, "path": p })).collect::<Vec<_>>(),
        "folders": b.folders,
        "files": files.into_iter().map(|(it, d, sn)| item_out_full(&lib, it, Some(folder.clone()), d, sn)).collect::<Vec<_>>(),
        "total": b.total,
        "next_offset": b.next_offset,
    })))
}

#[derive(Deserialize)]
struct ThumbQuery {
    size: Option<u32>,
}

async fn thumb(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<ThumbQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let file = ThumbSize::from_px(q.size).file();
    thumb_file(lib, id, file, headers).await
}

async fn signed_rendition(
    State(lib): State<Library>,
    Path((token, id, which)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let Some(secret) = lib.cfg.play_secret.as_deref() else {
        return Err(not_found());
    };
    if !secretary_media_stream::token::verify(
        secret,
        &id,
        &token,
        secretary_media_stream::now_secs(),
    ) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "bad or expired token".into(),
        ));
    }
    rendition(State(lib), Path((id, which)), headers).await
}

/// `grid` (g.jxl) or `preview` (p.jxl), `image/jxl`, cached for a year (the URL carries the version).
async fn rendition(
    State(lib): State<Library>,
    Path((id, rendition)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let file = match rendition.as_str() {
        "grid" => "g.jxl",
        "preview" => "p.jxl",
        _ => return Err(not_found()),
    };
    if id.contains('/') || id.contains("..") {
        return Err(not_found());
    }
    let iid = id.clone();
    let it = lib
        .db
        .call(move |c| db::get_item(c, &iid))
        .await?
        .ok_or_else(not_found)?;
    let path = thumbs::dir_for(&lib.cfg, &id).join(file);
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(
        headers,
        path,
        "image/jxl",
        &format!("{}-{}-{file}", it.hash, it.version),
        "private, max-age=31536000, immutable",
    )
    .await
}

async fn poster(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    thumb_file(lib, id, "poster.jpg", headers).await
}

async fn thumb_file(
    lib: Library,
    id: String,
    file: &str,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if id.contains('/') || id.contains("..") {
        return Err(not_found());
    }
    let iid = id.clone();
    let it = lib
        .db
        .call(move |c| db::get_item(c, &iid))
        .await?
        .ok_or_else(not_found)?;
    let path = thumbs::dir_for(&lib.cfg, &id).join(file);
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(
        headers,
        path,
        "image/jpeg",
        &format!("{}-{}-{file}", it.hash, it.version),
        "private, max-age=604800",
    )
    .await
}

async fn summary(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let v = lib
        .db
        .call(|c| {
            let by_kind: Vec<(String, i64, i64)> = {
                let mut st = c.prepare(
                    "SELECT kind, COUNT(*), COALESCE(SUM(bytes), 0) FROM items WHERE trashed_at IS NULL AND hidden = 0 GROUP BY kind",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            let years: Vec<(String, i64)> = {
                let mut st = c.prepare(
                    "SELECT substr(sort_at, 1, 4) y, COUNT(*) FROM items WHERE trashed_at IS NULL AND hidden = 0 GROUP BY y ORDER BY y DESC",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            let places: Vec<(String, Option<String>, i64)> = {
                let mut st = c.prepare(
                    "SELECT place_name, cc, COUNT(*) n FROM items WHERE place_name IS NOT NULL AND trashed_at IS NULL AND hidden = 0
                     GROUP BY place_name, cc ORDER BY n DESC LIMIT 30",
                )?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            let spool: i64 = c.query_row("SELECT COUNT(*) FROM blobs WHERE location = 'spool'", [], |r| r.get(0))?;
            let pending: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE thumb_state = 'pending'", [], |r| r.get(0))?;
            let latest: Option<String> =
                c.query_row("SELECT MAX(added_at) FROM items", [], |r| r.get(0)).optional()?.flatten();
            let trashed: i64 =
                c.query_row("SELECT COUNT(*) FROM items WHERE trashed_at IS NOT NULL", [], |r| r.get(0))?;
            Ok(json!({
                "kinds": by_kind.iter().map(|(k, n, b)| json!({"kind": k, "count": n, "bytes": b})).collect::<Vec<_>>(),
                "years": years.iter().map(|(y, n)| json!({"year": y, "count": n})).collect::<Vec<_>>(),
                "places": places.iter().map(|(p, cc, n)| json!({"name": p, "country": cc, "count": n})).collect::<Vec<_>>(),
                "spooled_originals": spool,
                "thumbs_pending": pending,
                "last_added_at": latest,
                "trashed": trashed,
            }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AlbumsQuery {
    /// `gallery`, `films` or `drive`: only albums with items there, counted there.
    scope: Option<String>,
}

async fn list_albums(
    State(lib): State<Library>,
    Query(q): Query<AlbumsQuery>,
) -> ApiResult<Json<Value>> {
    let albums = lib
        .db
        .call({
            let films = lib.cfg.film_folders.clone();
            move |c| db::list_albums_in(c, q.scope.as_deref(), &films)
        })
        .await?;
    Ok(Json(json!({ "albums": albums })))
}

#[derive(Deserialize)]
struct AlbumReq {
    title: String,
    #[serde(default)]
    item_ids: Vec<String>,
}

async fn create_album(
    State(lib): State<Library>,
    Json(r): Json<AlbumReq>,
) -> ApiResult<(StatusCode, Json<db::Album>)> {
    let title = r.title.trim().to_string();
    if title.is_empty() || title.len() > 200 {
        return Err(bad("title must be 1-200 characters"));
    }
    let album = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let id = db::new_id("alb");
            let now = now_str();
            tx.execute(
                "INSERT INTO albums (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params![id, title, now],
            )?;
            for item in &r.item_ids {
                if tx
                    .query_row("SELECT 1 FROM items WHERE id = ?1", [item], |_| Ok(()))
                    .optional()?
                    .is_some()
                {
                    db::add_to_album(&tx, &id, item)?;
                    db::reindex(&tx, item)?;
                }
            }
            let a = db::get_album(&tx, &id)?;
            tx.commit()?;
            Ok(a)
        })
        .await?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, Json(album)))
}

async fn get_album(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> ApiResult<Json<db::Album>> {
    Ok(Json(
        lib.db
            .call(move |c| db::get_album(c, &id))
            .await?
            .ok_or_else(not_found)?,
    ))
}

#[derive(Deserialize)]
struct AlbumPatch {
    title: Option<String>,
    cover_item: Option<String>,
}

async fn patch_album(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(p): Json<AlbumPatch>,
) -> ApiResult<Json<db::Album>> {
    let a = lib
        .db
        .call(move |c| {
            c.execute(
                "UPDATE albums SET title = COALESCE(?2, title), cover_item = COALESCE(?3, cover_item), updated_at = ?4
                 WHERE id = ?1",
                params![id, p.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()), p.cover_item, now_str()],
            )?;
            db::get_album(c, &id)
        })
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(a))
}

/// Deletes the album, never its items. System albums can't be deleted.
async fn delete_album(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let n = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let items: Vec<String> = {
                let mut st = tx.prepare("SELECT item_id FROM album_items WHERE album_id = ?1")?;
                let rows = st.query_map([&id], |r| r.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            let n = tx.execute(
                "DELETE FROM albums WHERE id = ?1 AND system_key IS NULL",
                [&id],
            )?;
            if n > 0 {
                tx.execute("DELETE FROM album_items WHERE album_id = ?1", [&id])?;
                for it in items {
                    db::reindex(&tx, &it)?;
                }
            }
            tx.commit()?;
            Ok(n)
        })
        .await?;
    if n == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such user album".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ItemIds {
    item_ids: Vec<String>,
}

async fn album_add(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(r): Json<ItemIds>,
) -> ApiResult<Json<Value>> {
    let added = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if db::get_album(&tx, &id)?.is_none() {
                return Ok(None);
            }
            let mut added = 0;
            for item in &r.item_ids {
                if tx
                    .query_row("SELECT 1 FROM items WHERE id = ?1", [item], |_| Ok(()))
                    .optional()?
                    .is_some()
                    && db::add_to_album(&tx, &id, item)?
                {
                    db::reindex(&tx, item)?;
                    added += 1;
                }
            }
            tx.commit()?;
            Ok(Some(added))
        })
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "added": added })))
}

/// Replace an album's items (Lucina's picks). Items themselves are untouched.
async fn album_replace(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(r): Json<ItemIds>,
) -> ApiResult<Json<Value>> {
    let n = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if db::get_album(&tx, &id)?.is_none() {
                return Ok(None);
            }
            let old: Vec<String> = {
                let mut st = tx.prepare("SELECT item_id FROM album_items WHERE album_id = ?1")?;
                let rows = st.query_map([&id], |r| r.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            tx.execute("DELETE FROM album_items WHERE album_id = ?1", [&id])?;
            tx.execute("UPDATE albums SET cover_item = NULL WHERE id = ?1", [&id])?;
            let mut n = 0;
            for item in r.item_ids.iter().take(500) {
                if tx
                    .query_row("SELECT 1 FROM items WHERE id = ?1", [item], |_| Ok(()))
                    .optional()?
                    .is_some()
                    && db::add_to_album(&tx, &id, item)?
                {
                    n += 1;
                }
            }
            for item in old.iter().chain(r.item_ids.iter()) {
                db::reindex(&tx, item)?;
            }
            tx.commit()?;
            Ok(Some(n))
        })
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({ "items": n })))
}

async fn album_remove(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(r): Json<ItemIds>,
) -> ApiResult<Json<Value>> {
    let removed = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let mut removed = 0;
            for item in &r.item_ids {
                removed += tx.execute(
                    "DELETE FROM album_items WHERE album_id = ?1 AND item_id = ?2",
                    params![id, item],
                )?;
                db::reindex(&tx, item)?;
            }
            tx.commit()?;
            Ok(removed)
        })
        .await?;
    Ok(Json(json!({ "removed": removed })))
}

// ---- uploads -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateUpload {
    filename: String,
    size: u64,
    /// blake3 hex of the whole file, if the client knows it (lets duplicates skip the upload).
    hash: Option<String>,
    #[serde(default)]
    meta: ClientMeta,
}

#[derive(Serialize)]
struct UploadOut {
    upload_id: Option<String>,
    offset: u64,
    size: u64,
    finished: bool,
    duplicate: bool,
    item_id: Option<String>,
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn create_upload(
    State(lib): State<Library>,
    Json(r): Json<CreateUpload>,
) -> ApiResult<Json<UploadOut>> {
    if r.filename.is_empty() || r.filename.len() > 255 || r.filename.contains('/') {
        return Err(bad("bad filename"));
    }
    if let Some(d) = r.meta.device.clone() {
        if !device_id_ok(&d) {
            return Err(bad("bad device id"));
        }
        lib.db.call(move |c| db::touch_device(c, &d)).await?;
    }
    if let Some(h) = &r.hash {
        if !valid_hash(h) {
            return Err(bad("hash must be 64 hex characters (blake3)"));
        }
        let h = h.to_ascii_lowercase();
        let device = r.meta.device.clone();
        let asset = r.meta.asset_id.clone();
        let existing: Option<String> = lib
            .db
            .call(move |c| {
                if let (Some(d), Some(a)) = (&device, &asset) {
                    let by_asset: Option<(String, String)> = c
                        .query_row(
                            "SELECT id, hash FROM items WHERE device = ?1 AND asset_id = ?2",
                            params![d, a],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    if let Some((id, cur)) = by_asset {
                        // Same asset, same bytes: done. Edited: the upload must go through.
                        return Ok(if cur == h { Some(id) } else { None });
                    }
                }
                c.query_row(
                    "SELECT id FROM items WHERE hash = ?1 ORDER BY added_at LIMIT 1",
                    [&h],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?;
        if let Some(item_id) = existing {
            return Ok(Json(UploadOut {
                upload_id: None,
                offset: r.size,
                size: r.size,
                finished: true,
                duplicate: true,
                item_id: Some(item_id),
            }));
        }
    }
    let id = db::new_id("upl");
    tokio::fs::File::create(lib.cfg.uploads_dir().join(&id)).await?;
    let mut meta = r.meta;
    meta.filename = meta.filename.or(Some(r.filename.clone()));
    let meta_json = serde_json::to_string(&meta)?;
    let (uid, size, hash) = (
        id.clone(),
        r.size as i64,
        r.hash.map(|h| h.to_ascii_lowercase()),
    );
    lib.db
        .call(move |c| {
            c.execute(
                "INSERT INTO uploads (id, filename, declared, expected_hash, meta, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![uid, r.filename, size, hash, meta_json, now_str()],
            )
        })
        .await?;
    Ok(Json(UploadOut {
        upload_id: Some(id),
        offset: 0,
        size: r.size,
        finished: false,
        duplicate: false,
        item_id: None,
    }))
}

struct UploadRow {
    declared: u64,
    received: u64,
    finished: bool,
    expected_hash: Option<String>,
    meta: String,
    item_id: Option<String>,
}

async fn load_upload(lib: &Library, id: &str) -> ApiResult<UploadRow> {
    if !id.starts_with("upl_") || id.contains('/') {
        return Err(not_found());
    }
    let id = id.to_string();
    lib.db
        .call(move |c| {
            c.query_row(
                "SELECT declared, received, finished, expected_hash, meta, item_id FROM uploads WHERE id = ?1",
                [&id],
                |r| {
                    Ok(UploadRow {
                        declared: r.get::<_, i64>(0)? as u64,
                        received: r.get::<_, i64>(1)? as u64,
                        finished: r.get::<_, i64>(2)? != 0,
                        expected_hash: r.get(3)?,
                        meta: r.get(4)?,
                        item_id: r.get(5)?,
                    })
                },
            )
            .optional()
        })
        .await?
        .ok_or_else(not_found)
}

async fn get_upload(
    State(lib): State<Library>,
    Path(id): Path<String>,
) -> ApiResult<Json<UploadOut>> {
    let u = load_upload(&lib, &id).await?;
    Ok(Json(UploadOut {
        upload_id: Some(id),
        offset: u.received,
        size: u.declared,
        finished: u.finished,
        duplicate: false,
        item_id: u.item_id,
    }))
}

#[derive(Deserialize)]
struct ChunkQuery {
    offset: Option<u64>,
}

/// Append a chunk. The whole file in one PUT at offset 0 is fine (that's what iOS background
/// uploads do); a broken transfer resumes from `GET /uploads/{id}`'s offset.
async fn put_chunk(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Query(q): Query<ChunkQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<UploadOut>> {
    let u = load_upload(&lib, &id).await?;
    let offset = q.offset.unwrap_or(0);
    let len = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(u.declared.saturating_sub(offset));
    let state = UploadState {
        declared: u.declared,
        received: u.received,
        finished: u.finished,
    };
    let out = |received: u64| UploadOut {
        upload_id: Some(id.clone()),
        offset: received,
        size: u.declared,
        finished: u.finished,
        duplicate: false,
        item_id: u.item_id.clone(),
    };
    let mut skip = match protocol::accept_chunk(state, offset, len) {
        Ok(ChunkOutcome::AlreadyHave { offset }) => return Ok(Json(out(offset))),
        Ok(ChunkOutcome::Append { skip, .. }) => skip,
        Err(e @ protocol::ChunkError::OffsetMismatch { .. }) => {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!("{e}; resume at {}", u.received),
            ))
        }
        Err(e) => return Err(ApiError(StatusCode::CONFLICT, e.to_string())),
    };
    let path = lib.cfg.uploads_dir().join(&id);
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await?;
    // The file can be longer than `received` if an earlier chunk was cut off mid-write.
    f.set_len(u.received).await?;
    let mut f = tokio::io::BufWriter::with_capacity(1 << 20, {
        use tokio::io::AsyncSeekExt;
        f.seek(std::io::SeekFrom::Start(u.received)).await?;
        f
    });
    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    let mut cut = false;
    while let Some(chunk) = stream.next().await {
        let Ok(mut chunk) = chunk else {
            cut = true;
            break;
        };
        // Bytes the server already has (a retry from an earlier offset).
        if skip > 0 {
            let n = skip.min(chunk.len() as u64);
            skip -= n;
            let _ = chunk.split_to(n as usize);
            if chunk.is_empty() {
                continue;
            }
        }
        if u.received + written + chunk.len() as u64 > u.declared {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "chunk longer than declared".into(),
            ));
        }
        f.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    f.flush().await?;
    f.get_mut().sync_data().await?;
    let received = u.received + written;
    let uid = id.clone();
    lib.db
        .call(move |c| {
            c.execute(
                "UPDATE uploads SET received = ?2, updated_at = ?3 WHERE id = ?1",
                params![uid, received as i64, now_str()],
            )
        })
        .await?;
    if cut {
        tracing::info!(upload = %id, received, "chunk cut off; client can resume");
    }
    Ok(Json(out(received)))
}

async fn put_preview(
    State(lib): State<Library>,
    Path(id): Path<String>,
    body: Body,
) -> ApiResult<StatusCode> {
    let u = load_upload(&lib, &id).await?;
    if u.finished {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "upload already finished".into(),
        ));
    }
    let bytes = axum::body::to_bytes(body, MAX_PREVIEW_BYTES)
        .await
        .map_err(|_| bad("preview too large"))?;
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(bad("preview must be a JPEG"));
    }
    tokio::fs::write(lib.cfg.uploads_dir().join(format!("{id}.preview")), &bytes).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct FinishReq {
    hash: Option<String>,
}

async fn finish_upload(
    State(lib): State<Library>,
    Path(id): Path<String>,
    body: Option<Json<FinishReq>>,
) -> ApiResult<Json<ingest::IngestResult>> {
    let r = body.map(|b| b.0).unwrap_or_default();
    let u = load_upload(&lib, &id).await?;
    if u.finished {
        // A retried finish (the first response was lost): report the same item.
        let item_id = u.item_id.ok_or_else(not_found)?;
        let iid = item_id.clone();
        let version = lib
            .db
            .call(move |c| {
                c.query_row("SELECT version FROM items WHERE id = ?1", [&iid], |r| {
                    r.get(0)
                })
            })
            .await?;
        return Ok(Json(ingest::IngestResult {
            item_id,
            duplicate: false,
            version,
        }));
    }
    let path = lib.cfg.uploads_dir().join(&id);
    if !path.is_file() {
        // The bytes are gone (an earlier finish ingested them, then the client hung up before the
        // answer): the client starts this upload again; dedupe makes that cheap.
        return Err(ApiError(
            StatusCode::GONE,
            "upload lost; start again".into(),
        ));
    }
    let p2 = path.clone();
    let computed = tokio::task::spawn_blocking(move || crate::store::hash_file(&p2)).await??;
    let expected = r.hash.or(u.expected_hash).map(|h| h.to_ascii_lowercase());
    let state = UploadState {
        declared: u.declared,
        received: u.received,
        finished: false,
    };
    protocol::check_finish(state, &computed, expected.as_deref()).map_err(|e| match e {
        protocol::FinishError::HashMismatch => ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "content hash does not match; upload again".into(),
        ),
        e => ApiError(StatusCode::CONFLICT, e.to_string()),
    })?;
    let meta: ClientMeta = serde_json::from_str(&u.meta).unwrap_or_default();
    let preview = lib.cfg.uploads_dir().join(format!("{id}.preview"));
    // Ingest and the "finished" mark in one blocking task: a client that hangs up drops this
    // handler at its next await, which must not fall between the two.
    let l = lib.clone();
    let uid = id.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<ingest::IngestResult> {
        let preview = preview.is_file().then_some(preview);
        let res = ingest::ingest_file(&l, &path, Some(computed), &meta, preview.as_deref())?;
        l.db.with(|c| {
            c.execute(
                "UPDATE uploads SET finished = 1, item_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![uid, res.item_id, now_str()],
            )
        })?;
        Ok(res)
    })
    .await??;
    let _ = tokio::fs::remove_file(lib.cfg.uploads_dir().join(format!("{id}.preview"))).await;
    Ok(Json(res))
}

/// What the library holds for one phone, so a reinstalled app can skip assets already backed up.
/// `[A-Za-z0-9._-]{1,64}`: what the apps send (`ios-<uuid>`, `mac-<uuid>`).
fn device_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[derive(Deserialize)]
struct DeviceReq {
    name: String,
    kind: Option<String>,
}

const DEVICE_KINDS: &[&str] = &[
    "iphone", "ipad", "android", "mac", "pc", "backup", "camera", "other",
];

async fn register_device(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(r): Json<DeviceReq>,
) -> ApiResult<Json<Value>> {
    let name = r.name.trim().to_string();
    if !device_id_ok(&id) || name.is_empty() || name.chars().count() > 80 {
        return Err(bad("bad device id or name"));
    }
    let kind = match r.kind.as_deref() {
        Some(k) if DEVICE_KINDS.contains(&k) => k.to_string(),
        Some(_) => {
            return Err(bad(
                "kind must be one of iphone, ipad, android, mac, pc, backup, camera, other",
            ))
        }
        None => db::guess_device_kind(&id).to_string(),
    };
    let i = id.clone();
    lib.db
        .call(move |c| db::upsert_device(c, &i, &name, &kind))
        .await?;
    let all = lib.db.call(|c| db::list_devices(c)).await?;
    Ok(Json(json!(all.into_iter().find(|d| d.id == id))))
}

async fn list_devices(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let all = lib.db.call(|c| db::list_devices(c)).await?;
    Ok(Json(json!({ "devices": all })))
}

async fn device_assets(
    State(lib): State<Library>,
    Path(device): Path<String>,
) -> ApiResult<Json<Value>> {
    let rows = lib
        .db
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT asset_id, hash, id, version, trashed_at IS NOT NULL FROM items WHERE device = ?1 AND asset_id IS NOT NULL",
            )?;
            let rows = st.query_map([&device], |r| {
                Ok(json!({"asset_id": r.get::<_, String>(0)?, "hash": r.get::<_, String>(1)?,
                          "item_id": r.get::<_, String>(2)?, "version": r.get::<_, i64>(3)?, "trashed": r.get::<_, bool>(4)?}))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!({ "assets": rows })))
}

async fn events(
    State(lib): State<Library>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = lib.events.subscribe();
    let stream = tokio_stream_from(rx).map(|e| {
        Ok(SseEvent::default()
            .event(e.kind)
            .data(serde_json::to_string(&e).unwrap_or_default()))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))
}

fn tokio_stream_from(
    rx: tokio::sync::broadcast::Receiver<ingest::Event>,
) -> impl Stream<Item = ingest::Event> {
    futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(e) => return Some((e, rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    })
}

// ---- internal ------------------------------------------------------------------------------

fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

async fn put_external(
    State(lib): State<Library>,
    Path((source, ext_id)): Path<(String, String)>,
    Json(req): Json<ExternalReq>,
) -> ApiResult<Json<ingest::IngestResult>> {
    if !valid_key(&source) || !valid_key(&ext_id) {
        return Err(bad("source and id must be [A-Za-z0-9._-]"));
    }
    let l = lib.clone();
    let res =
        tokio::task::spawn_blocking(move || ingest::register_external(&l, &source, &ext_id, &req))
            .await?
            .map_err(|e| bad(e.to_string()))?;
    Ok(Json(res))
}

async fn delete_external(
    State(lib): State<Library>,
    Path((source, ext_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let l = lib.clone();
    match tokio::task::spawn_blocking(move || ingest::unregister_external(&l, &source, &ext_id))
        .await??
    {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(not_found()),
    }
}

/// A file another service on nezopt hands over (shares to Lucina): moved or copied into the
/// library, deduped, indexed; thumbnails follow in the background.
#[derive(Deserialize)]
struct IngestReq {
    path: String,
    mime: Option<String>,
    name: Option<String>,
    source: Option<String>,
    album: Option<String>,
    taken_at: Option<String>,
    caption: Option<String>,
    #[serde(default)]
    r#move: bool,
}

async fn ingest_local(
    State(lib): State<Library>,
    Json(r): Json<IngestReq>,
) -> ApiResult<Json<Value>> {
    let src = PathBuf::from(&r.path);
    if !src.is_absolute() || !src.is_file() {
        return Err(bad("path must be an existing absolute file"));
    }
    let name = r
        .name
        .clone()
        .filter(|n| !n.is_empty() && !n.contains('/'))
        .or_else(|| src.file_name().and_then(|n| n.to_str()).map(str::to_string));
    let meta = ClientMeta {
        filename: name,
        mime: r.mime.clone(),
        taken_at: r.taken_at.clone(),
        source: Some(r.source.clone().unwrap_or_else(|| "share".into())),
        album: r.album.clone(),
        caption: r.caption.clone(),
        ..Default::default()
    };
    let l = lib.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<ingest::IngestResult> {
        // The library consumes its own copy in the uploads area (same disk as the spool).
        let tmp = l.cfg.uploads_dir().join(db::new_id("ing"));
        if r.r#move {
            crate::store::durable_move(&src, &tmp)?;
        } else {
            std::fs::copy(&src, &tmp)?;
        }
        ingest::ingest_file(&l, &tmp, None, &meta, None)
    })
    .await?
    .map_err(|e| bad(e.to_string()))?;
    let id = res.item_id.clone();
    let it = lib
        .db
        .call(move |c| db::get_item(c, &id))
        .await?
        .ok_or_else(not_found)?;
    let out = item_out(&lib, it, None);
    Ok(Json(json!({
        "item_id": res.item_id,
        "duplicate": res.duplicate,
        "version": res.version,
        "kind": out.item.kind,
        "mime": out.item.mime,
        "thumb_state": out.item.thumb_state,
        "urls": out.urls,
    })))
}

#[derive(Deserialize)]
struct ImportReq {
    path: String,
    source: Option<String>,
    album: Option<String>,
}

async fn start_import(
    State(lib): State<Library>,
    Json(r): Json<ImportReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let path = PathBuf::from(&r.path);
    if !path.is_absolute() || !path.exists() {
        return Err(bad("path must exist and be absolute"));
    }
    let l = lib.clone();
    tokio::task::spawn_blocking(move || {
        let opts = crate::import::ImportOpts {
            source: r.source.unwrap_or_else(|| "import".into()),
            album: r.album,
        };
        match crate::import::import_path(&l, &path, &opts) {
            Ok(s) => tracing::info!(?s, "import finished"),
            Err(e) => tracing::warn!(error = %e, "import failed"),
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "started": true }))))
}

async fn item_path(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let l = lib.clone();
    let (path, item) = tokio::task::spawn_blocking(move || ingest::original_path(&l, &id))
        .await??
        .ok_or_else(not_found)?;
    Ok(Json(
        json!({ "path": path, "mime": item.mime, "hash": item.hash, "kind": item.kind }),
    ))
}
