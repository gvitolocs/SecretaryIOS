//! `/v1/vault/{manifest,file,trash}`: the owner's vault for devices that sync it themselves (Juno; later the phones).
//! Lucina holds the vault on nezopt and serves it at `/api/secretary/vault/*` (lucina-server `vault_sync.rs`); this
//! server forwards to it over the Link, as for `/v1/notebooks`, with this server's Lucina token.
//!
//! Who: owner devices only, with their device token or web session (cookie + CSRF on writes), checked by
//! [`crate::notebooks::authenticate`]. The MCP and service tokens are refused: nothing but a device syncs a vault copy.
//! Requests count against the media limiter under `vault:<device>` (a first sync fetches every file once).
//! Bodies are bytes up to [`BODY_LIMIT`]; the query (`path`, `expect`) passes unchanged, and so do the status and the
//! `X-Vault-Revision` header back.
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use std::time::Duration;

use crate::agents::{Caller, ORIGIN_HEADER};
use crate::error::ApiError;
use crate::{AppState, DeviceId};

/// One vault file (Lucina's limit is the same).
pub const BODY_LIMIT: usize = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const REVISION: HeaderName = HeaderName::from_static("x-vault-revision");

/// The part after `/api/secretary/vault` for one of the three routes.
pub fn upstream_path(path: &str) -> Option<&'static str> {
    match path {
        "/v1/vault/manifest" => Some("/manifest"),
        "/v1/vault/file" => Some("/file"),
        "/v1/vault/trash" => Some("/trash"),
        _ => None,
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "lucina_not_configured",
        "SECRETARY_LUCINA_TOKEN_FILE is not set on the server",
    )
}

async fn handle(
    State(st): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let Some(nb) = st.notebooks.as_ref() else {
        return Err(unavailable());
    };
    let caller = crate::notebooks::authenticate(&st, &nb.mcp_devices, &method, &headers)?;
    let Caller::Device(_) = &caller else {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "devices_only", "only an owner device syncs the vault"));
    };
    let rest = upstream_path(uri.path()).ok_or_else(ApiError::not_found)?;
    st.media_limiter
        .check(&format!("vault:{}", caller.key()))
        .map_err(ApiError::rate_limited)?;
    let sends_body = matches!(method, Method::PUT | Method::POST);
    let bytes: Bytes = if sends_body {
        let declared = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|n| n > BODY_LIMIT as u64) {
            return Err(too_large());
        }
        axum::body::to_bytes(body, BODY_LIMIT).await.map_err(|_| too_large())?
    } else {
        Bytes::new()
    };

    let mut url = format!("{}/api/secretary/vault{rest}", nb.base);
    if let Some(q) = uri.query().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    let mut req = nb
        .http
        .request(method.clone(), &url)
        .header(header::AUTHORIZATION, &nb.auth)
        .header(ORIGIN_HEADER, caller.origin())
        .timeout(TIMEOUT);
    if sends_body {
        req = req
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes);
    }
    let up = match req.send().await {
        Ok(up) => up,
        Err(e) => {
            tracing::warn!(timeout = e.is_timeout(), "vault: Lucina unreachable");
            return Err(ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "Lucina is unreachable"));
        }
    };
    let status = up.status();
    if status == StatusCode::UNAUTHORIZED {
        tracing::error!("vault: Lucina refused the server's token");
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "lucina_auth", "Lucina refused the server's credentials"));
    }
    let content_type = up.headers().get(header::CONTENT_TYPE).cloned();
    let revision = up.headers().get(&REVISION).cloned();
    let mut resp = Body::from_stream(up.bytes_stream()).into_response();
    *resp.status_mut() = status;
    if let Some(ct) = content_type {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    if let Some(r) = revision {
        resp.headers_mut().insert(REVISION, r);
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.extensions_mut().insert(DeviceId(caller.key()));
    Ok(resp)
}

fn too_large() -> ApiError {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", "a vault file is limited to 64 MB")
}

/// The routes, with their own authentication (module docs).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/vault/{*rest}", any(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_routes() {
        assert_eq!(upstream_path("/v1/vault/manifest"), Some("/manifest"));
        assert_eq!(upstream_path("/v1/vault/file"), Some("/file"));
        assert_eq!(upstream_path("/v1/vault/trash"), Some("/trash"));
        for bad in ["/v1/vault", "/v1/vault/", "/v1/vault/files", "/v1/vault/file/x", "/v1/vault/../agentd", "/v1/vault/manifest/"] {
            assert_eq!(upstream_path(bad), None, "{bad}");
        }
    }
}
