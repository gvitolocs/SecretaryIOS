//! `GET /v1/assets/{name}`: private files (e.g. the 3D Lucina avatar, whose
//! licence forbids redistribution) from `SECRETARY_PRIVATE_ASSETS_DIR`, only
//! to authenticated callers (the route sits behind `auth::require_device`).
//!
//! - Names: `[a-z0-9][a-z0-9._-]*`, at most 128 bytes, no `..`, one of the
//!   whitelisted extensions; anything else is a plain 404.
//! - `Cache-Control: private, max-age=86400`, a strong `ETag` from size and
//!   mtime (`If-None-Match` → 304), `Last-Modified`, and byte `Range`
//!   requests (via tower-http's `ServeFile`).

use std::path::Path;
use std::time::UNIX_EPOCH;

use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::error::ApiError;
use crate::AppState;

pub const CACHE_CONTROL: &str = "private, max-age=86400";

/// Content type for an allowed asset name, `None` if the name is refused.
pub fn content_type(name: &str) -> Option<&'static str> {
    let first_ok = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let chars_ok = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !first_ok || !chars_ok || name.len() > 128 || name.contains("..") {
        return None;
    }
    match name.rsplit_once('.')?.1 {
        "glb" => Some("model/gltf-binary"),
        "json" => Some("application/json"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "mp3" => Some("audio/mpeg"),
        "vmd" => Some("application/octet-stream"),
        _ => None,
    }
}

fn etag(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("\"{:x}-{:x}\"", meta.len(), mtime))
}

fn cache_headers(resp: &mut Response, tag: &str) {
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(v) = HeaderValue::from_str(tag) {
        h.insert(header::ETAG, v);
    }
}

async fn asset(
    State(st): State<AppState>,
    UrlPath(name): UrlPath<String>,
    req: Request,
) -> Response {
    let Some(mime) = content_type(&name) else {
        return ApiError::not_found().into_response();
    };
    let path = st.private_assets_dir.join(&name);
    let Some(tag) = etag(&path) else {
        return ApiError::not_found().into_response();
    };
    let not_modified = req
        .headers()
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == tag || t.trim() == "*");
    if not_modified {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        cache_headers(&mut resp, &tag);
        return resp;
    }
    let mime: mime::Mime = mime.parse().expect("static mime");
    let mut resp = match ServeFile::new_with_mime(&path, &mime).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(never) => match never {},
    };
    if resp.status() == StatusCode::NOT_FOUND {
        return ApiError::not_found().into_response();
    }
    cache_headers(&mut resp, &tag);
    resp
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/assets/{name}", get(asset))
}

#[cfg(test)]
mod tests {
    use super::content_type;

    #[test]
    fn names_are_whitelisted() {
        assert_eq!(content_type("lucina.glb"), Some("model/gltf-binary"));
        assert_eq!(
            content_type("idle-01.vmd"),
            Some("application/octet-stream")
        );
        assert_eq!(content_type("face_2.webp"), Some("image/webp"));
        assert_eq!(content_type("v1.0.json"), Some("application/json"));
        for bad in [
            "", ".glb", "..glb", "a..glb", "../x.glb", "a/b.glb", "A.glb", "x.exe", "x", "x.GLB",
            "-x.png", "_x.png", "x.png ", "x%2f.png",
        ] {
            assert_eq!(content_type(bad), None, "{bad:?}");
        }
        assert_eq!(content_type(&format!("{}.png", "a".repeat(125))), None);
    }
}
