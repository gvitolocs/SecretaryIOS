//! Static web app (`SECRETARY_WEB_DIR`), same origin as the API.
//!
//! - Files are served from the directory at `/` (`/` → `index.html`).
//! - SPA fallback: a GET/HEAD for a missing path outside `/v1` and
//!   `/assets/` whose last segment has no `.` gets `index.html`, so client
//!   routes like `/agenda` work on reload. Missing files (`/assets/x.js`,
//!   `/logo.png`) stay 404.
//! - `/assets/*` (content-hashed build output) is
//!   `Cache-Control: public, max-age=31536000, immutable`; everything else,
//!   `index.html` included, is `no-cache`.

use std::path::Path;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::error::ApiError;
use crate::AppState;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const NO_CACHE: &str = "no-cache";

fn is_api(path: &str) -> bool {
    path == "/v1" || path.starts_with("/v1/")
}

fn spa_route(path: &str) -> bool {
    !path.starts_with("/assets/") && !path.rsplit('/').next().unwrap_or("").contains('.')
}

async fn serve_index(dir: &Path, method: Method) -> Response {
    let req = Request::builder()
        .method(method)
        .uri("/index.html")
        .body(Body::empty())
        .expect("static request");
    match ServeFile::new(dir.join("index.html")).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(never) => match never {},
    }
}

/// Router fallback: static files / SPA when configured, JSON 404 otherwise.
pub async fn fallback(State(st): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let web = st
        .web_dir
        .as_deref()
        .filter(|_| !is_api(&path) && matches!(method, Method::GET | Method::HEAD));
    let Some(dir) = web else {
        tracing::info!(method = %method, route = "<unmatched>", status = 404, "request");
        return ApiError::not_found().into_response();
    };

    let mut resp = match ServeDir::new(dir).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(never) => match never {},
    };
    if resp.status() == StatusCode::NOT_FOUND {
        resp = if spa_route(&path) {
            serve_index(dir, method.clone()).await
        } else {
            ApiError::not_found().into_response()
        };
    }
    let ok = resp.status().is_success() || resp.status() == StatusCode::NOT_MODIFIED;
    if ok {
        let cc = if path.starts_with("/assets/") {
            IMMUTABLE
        } else {
            NO_CACHE
        };
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cc));
    }
    tracing::info!(method = %method, route = "<web>", status = resp.status().as_u16(), "request");
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_rules() {
        assert!(is_api("/v1") && is_api("/v1/chat") && !is_api("/v10"));
        assert!(spa_route("/agenda") && spa_route("/reminders/r1") && spa_route("/"));
        assert!(!spa_route("/assets/app.js") && !spa_route("/favicon.ico"));
    }
}
