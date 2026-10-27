//! Album share links over HTTP (`docs/media-library.md`, "Share links").
//!
//! - The owner's routes sit behind the upstream key with the rest of `/v1/media`.
//! - Everything under `/v1/media/a/{token}` is public: `upstream_guard` lets it through, like the
//!   play tokens. Every public handler goes through [`gate`] (a well-formed token, the rate limit,
//!   then the verified [`share::decide`]) before it reads anything of the album.
//! - The public side serves only the JPEG renditions with their metadata stripped (no EXIF, no
//!   GPS), and the original only when the link allows it. The item list has no place, camera,
//!   caption or file name.

use std::sync::OnceLock;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, now_str};
use crate::http::{serve_file, ApiError, ApiResult};
use crate::ingest::{self, Library};
use crate::share::{self, Access, ShareRow};
use crate::thumbs;

/// Items per page of a share's item list.
const PAGE: usize = 60;

fn limiter() -> &'static share::RateLimit {
    static L: OnceLock<share::RateLimit> = OnceLock::new();
    L.get_or_init(share::RateLimit::default)
}

/// The owner's routes (behind the upstream key).
pub fn owner_routes() -> Router<Library> {
    Router::new()
        .route(
            "/v1/media/albums/{id}/shares",
            get(album_shares).post(create_share),
        )
        .route("/v1/media/shares", get(all_shares))
        .route("/v1/media/shares/{token}", delete(revoke))
}

/// The public routes (no key; the guard lets `/v1/media/a/` through).
pub fn public_routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/a/{token}", get(page))
        .route("/v1/media/a/{token}/items", get(items))
        .route("/v1/media/a/{token}/i/{item}/{which}", get(item_file))
}

#[derive(Deserialize, Default)]
struct CreateReq {
    expires_in_days: Option<u32>,
    allow_original: Option<bool>,
}

fn share_out(s: &ShareRow) -> Value {
    let now = now_str();
    let expired = s.expires_at.as_deref().is_some_and(|e| e <= now.as_str());
    json!({
        "token": s.token,
        "url_path": format!("/v1/media/a/{}", s.token),
        "album_id": s.album_id,
        "created_at": s.created_at,
        "expires_at": s.expires_at,
        "revoked_at": s.revoked_at,
        "allow_original": s.allow_original,
        "views": s.views,
        "last_view_at": s.last_view_at,
        "live": s.revoked_at.is_none() && !expired,
    })
}

async fn create_share(
    State(lib): State<Library>,
    Path(album): Path<String>,
    body: Option<Json<CreateReq>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let r = body.map(|b| b.0).unwrap_or_default();
    let token = share::new_token()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let now = chrono::Utc::now();
    let expires_at = r.expires_in_days.map(|d| {
        (now + chrono::Duration::days(i64::from(d.max(1))))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    });
    let row = ShareRow {
        token,
        album_id: album.clone(),
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        expires_at,
        revoked_at: None,
        allow_original: r.allow_original.unwrap_or(false),
        views: 0,
        last_view_at: None,
    };
    let saved = row.clone();
    let made = lib
        .db
        .call(move |c| {
            if db::get_album(c, &album)?.is_none() {
                return Ok(false);
            }
            db::insert_share(c, &saved)?;
            Ok(true)
        })
        .await?;
    if !made {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such album".into()));
    }
    Ok((StatusCode::CREATED, Json(share_out(&row))))
}

async fn album_shares(
    State(lib): State<Library>,
    Path(album): Path<String>,
) -> ApiResult<Json<Value>> {
    let rows = lib
        .db
        .call(move |c| db::list_shares(c, Some(&album)))
        .await?;
    Ok(Json(
        json!({ "shares": rows.iter().map(share_out).collect::<Vec<_>>() }),
    ))
}

async fn all_shares(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let rows = lib.db.call(|c| db::list_shares(c, None)).await?;
    Ok(Json(
        json!({ "shares": rows.iter().map(share_out).collect::<Vec<_>>() }),
    ))
}

async fn revoke(State(lib): State<Library>, Path(token): Path<String>) -> ApiResult<StatusCode> {
    let found = lib.db.call(move |c| db::revoke_share(c, &token)).await?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "no such share".into()))
    }
}

fn status_of(a: Access) -> Option<ApiError> {
    match a {
        Access::Page | Access::Item | Access::Original => None,
        Access::NotFound => Some(ApiError(StatusCode::NOT_FOUND, "not found".into())),
        Access::Gone => Some(ApiError(
            StatusCode::GONE,
            "this link was revoked or has expired".into(),
        )),
        Access::Forbidden => Some(ApiError(
            StatusCode::FORBIDDEN,
            "this link does not allow originals".into(),
        )),
    }
}

/// Every public request: a token `new_token` could have made, under the rate limit, then the rule.
async fn gate(
    lib: &Library,
    token: &str,
    item: Option<&str>,
    want_original: bool,
) -> ApiResult<ShareRow> {
    if !share::token_well_formed(token) {
        return Err(ApiError(StatusCode::NOT_FOUND, "not found".into()));
    }
    if !limiter().allow(token, Instant::now()) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests".into(),
        ));
    }
    let (t, i) = (token.to_string(), item.map(str::to_string));
    let (row, in_album) = lib
        .db
        .call(move |c| {
            let row = db::get_share(c, &t)?;
            let in_album = match (&row, &i) {
                (Some(r), Some(id)) => Some(db::item_visible_in_album(c, &r.album_id, id)?),
                (None, Some(_)) => Some(false),
                _ => None,
            };
            Ok((row, in_album))
        })
        .await?;
    let access = share::decide(row.as_ref(), &now_str(), in_album, want_original);
    match (status_of(access), row) {
        (None, Some(r)) => Ok(r),
        (Some(e), _) => Err(e),
        (None, None) => Err(ApiError(StatusCode::NOT_FOUND, "not found".into())),
    }
}

/// Headers on everything public: never indexed, no referrer, nothing framed or cached by others.
fn public_headers(res: &mut Response, cache: &'static str) {
    let h = res.headers_mut();
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
}

fn escape_html(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            c => c.to_string(),
        })
        .collect()
}

async fn page(State(lib): State<Library>, Path(token): Path<String>) -> ApiResult<Response> {
    let row = gate(&lib, &token, None, false).await?;
    let (album_id, t) = (row.album_id.clone(), token.clone());
    let (title, count) = lib
        .db
        .call(move |c| {
            db::count_share_view(c, &t)?;
            let album = db::get_album(c, &album_id)?;
            Ok(album.map(|a| (a.title, a.count)).unwrap_or_default())
        })
        .await?;
    let nonce = share::new_token()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let html = SHARE_PAGE
        .replace("{{TITLE}}", &escape_html(&title))
        .replace("{{COUNT}}", &count.to_string())
        .replace("{{NONCE}}", &nonce)
        .replace(
            "{{ORIGINALS}}",
            if row.allow_original { "true" } else { "false" },
        );
    let mut res = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    let csp = format!(
        "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; \
         script-src 'nonce-{nonce}'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    public_headers(&mut res, "private, no-store");
    Ok(res)
}

#[derive(Deserialize)]
struct ItemsQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn items(
    State(lib): State<Library>,
    Path(token): Path<String>,
    Query(q): Query<ItemsQuery>,
) -> ApiResult<Response> {
    let row = gate(&lib, &token, None, false).await?;
    let offset: usize = q
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let limit = q.limit.unwrap_or(PAGE).clamp(1, 200);
    let album_id = row.album_id.clone();
    let (title, list, total) = lib
        .db
        .call(move |c| {
            let title = db::get_album(c, &album_id)?
                .map(|a| a.title)
                .unwrap_or_default();
            let (list, total) = db::album_visible_items(c, &album_id, offset, limit)?;
            Ok((title, list, total))
        })
        .await?;
    let out: Vec<Value> = list
        .iter()
        .map(|it| {
            let id = &it.id;
            let video = it.kind == "video";
            // Videos play from the stream router's play-token file route (/v1/media/s/…), which
            // p1 already passes through; the player needs the file, so a shared video is playable
            // whether or not the link allows downloading originals.
            let play = video
                .then(|| {
                    lib.signer
                        .as_ref()
                        .map(|s| format!("{}/file", s.base(id).0))
                })
                .flatten();
            json!({
                "id": id,
                "kind": it.kind,
                "width": it.width,
                "height": it.height,
                "taken_at": it.taken_at.as_deref().and_then(share::date_only),
                "duration_ms": it.duration_ms,
                "thumb": format!("i/{id}/thumb"),
                "large": format!("i/{id}/{}", if video { "poster" } else { "large" }),
                "original": row.allow_original.then(|| format!("i/{id}/original")),
                "play": play,
            })
        })
        .collect();
    let next = (offset + list.len() < total as usize).then(|| (offset + list.len()).to_string());
    let mut res =
        Json(json!({ "title": title, "total": total, "items": out, "next_cursor": next }))
            .into_response();
    public_headers(&mut res, "private, no-store");
    Ok(res)
}

async fn item_file(
    State(lib): State<Library>,
    Path((token, item, which)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let file = match which.as_str() {
        "thumb" => "s.jpg",
        "large" => "l.jpg",
        "poster" => "poster.jpg",
        "original" => "",
        _ => return Err(ApiError(StatusCode::NOT_FOUND, "not found".into())),
    };
    let want_original = file.is_empty();
    gate(&lib, &token, Some(&item), want_original).await?;
    if want_original {
        let l = lib.clone();
        let iid = item.clone();
        let (path, it) = tokio::task::spawn_blocking(move || ingest::original_path(&l, &iid))
            .await??
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not found".into()))?;
        if !path.is_file() {
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "original is not reachable right now".into(),
            ));
        }
        let mut res = serve_file(headers, path, &it.mime, &it.hash, "private, no-store").await?;
        res.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
        public_headers(&mut res, "private, no-store");
        return Ok(res);
    }
    if item.contains('/') || item.contains("..") {
        return Err(ApiError(StatusCode::NOT_FOUND, "not found".into()));
    }
    let path = thumbs::dir_for(&lib.cfg, &item).join(file);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "not found".into()))?;
    // Never serve a picture whose metadata could not be stripped.
    let clean = tokio::task::spawn_blocking(move || share::strip_jpeg_metadata(&bytes))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not found".into()))?;
    let mut res = ([(header::CONTENT_TYPE, "image/jpeg")], clean).into_response();
    public_headers(&mut res, "private, max-age=3600");
    Ok(res)
}

/// The share page: one file, no external resources; the script carries the CSP nonce.
const SHARE_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<meta name="referrer" content="no-referrer">
<title>{{TITLE}}</title>
<style>
:root{color-scheme:light dark;--bg:#fff;--fg:#1d1d1f;--muted:#6e6e73;--tile:#e9e9ee}
@media (prefers-color-scheme:dark){:root{--bg:#000;--fg:#f5f5f7;--muted:#a1a1a6;--tile:#1c1c1e}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.4 -apple-system,system-ui,sans-serif}
header{padding:20px 16px 12px}h1{margin:0;font-size:22px}p{margin:4px 0 0;color:var(--muted);font-size:14px}
#grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(110px,1fr));gap:2px}
#grid button{position:relative;border:0;padding:0;aspect-ratio:1;background:var(--tile);cursor:pointer;overflow:hidden}
#grid img{width:100%;height:100%;object-fit:cover;display:block}
#grid span{position:absolute;right:4px;bottom:4px;color:#fff;font-size:12px;text-shadow:0 1px 2px #000}
#more{display:block;margin:16px auto;padding:10px 16px;border:0;border-radius:8px;background:var(--tile);color:var(--fg)}
#view{position:fixed;inset:0;background:#000;display:none;align-items:center;justify-content:center;z-index:1}
#view.on{display:flex}#view img,#view video{max-width:100%;max-height:100%}
#view nav{position:absolute;top:0;left:0;right:0;display:flex;justify-content:space-between;padding:12px;
  background:linear-gradient(#000b,#0000)}#view nav button,#view nav a{color:#fff;background:#0006;border:0;border-radius:16px;padding:6px 12px;text-decoration:none;font-size:14px}
</style></head><body>
<header><h1>{{TITLE}}</h1><p>{{COUNT}} items</p></header>
<main id="grid"></main><button id="more" hidden>More</button>
<div id="view"><nav><button id="close">Close</button><a id="dl" hidden download>Download</a></nav><div id="stage"></div></div>
<script nonce="{{NONCE}}">
(() => {
  const base = location.pathname.replace(/\/$/, "") + "/";
  const originals = {{ORIGINALS}};
  const grid = document.getElementById("grid"), more = document.getElementById("more");
  const view = document.getElementById("view"), stage = document.getElementById("stage"), dl = document.getElementById("dl");
  let items = [], cursor = null, at = -1;
  async function load() {
    const r = await fetch(base + "items" + (cursor ? "?cursor=" + encodeURIComponent(cursor) : ""));
    if (!r.ok) { grid.textContent = r.status === 410 ? "This link has been turned off." : "This album is not available."; return; }
    const page = await r.json();
    for (const it of page.items) {
      const i = items.push(it) - 1;
      const b = document.createElement("button");
      const img = document.createElement("img");
      img.loading = "lazy"; img.alt = ""; img.src = base + it.thumb;
      b.append(img);
      if (it.kind === "video") { const s = document.createElement("span"); s.textContent = "▶"; b.append(s); }
      b.addEventListener("click", () => show(i));
      grid.append(b);
    }
    cursor = page.next_cursor; more.hidden = !cursor;
  }
  function show(i) {
    at = i; const it = items[i]; stage.replaceChildren();
    if (it.kind === "video" && it.play) {
      const v = document.createElement("video"); v.controls = true; v.playsInline = true; v.poster = base + it.large; v.src = it.play; stage.append(v);
    } else { const img = document.createElement("img"); img.alt = ""; img.src = base + it.large; stage.append(img); }
    dl.hidden = !(originals && it.original); if (!dl.hidden) dl.href = base + it.original;
    view.classList.add("on");
  }
  function close() { view.classList.remove("on"); stage.replaceChildren(); at = -1; }
  document.getElementById("close").addEventListener("click", close);
  more.addEventListener("click", load);
  document.addEventListener("keydown", e => {
    if (at < 0) return;
    if (e.key === "Escape") close();
    if (e.key === "ArrowRight" && at + 1 < items.length) show(at + 1);
    if (e.key === "ArrowLeft" && at > 0) show(at - 1);
  });
  let x0 = null;
  view.addEventListener("touchstart", e => { x0 = e.touches[0].clientX; }, { passive: true });
  view.addEventListener("touchend", e => {
    if (x0 === null) return; const dx = e.changedTouches[0].clientX - x0; x0 = null;
    if (dx < -50 && at + 1 < items.length) show(at + 1); else if (dx > 50 && at > 0) show(at - 1);
  });
  load();
})();
</script></body></html>
"#;
