//! Bearer authentication of `/mcp`.
//!
//! `Authorization: Bearer mcp_…` (service token) or `Bearer mcpa_…` (OAuth
//! access token). A bare `mcp_…` without the `Bearer ` prefix is accepted
//! too, for clients that put the raw token in the header. Anything else is
//! `401` with an RFC 9728 challenge pointing at the protected resource
//! metadata, which is how ChatGPT discovers the OAuth flow.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::oauth::ACCESS_PREFIX;
use crate::scope::Scopes;
use crate::tokens::TOKEN_PREFIX;
use crate::AppState;

/// The client IP: the right-most `X-Forwarded-For` entry (the one Caddy
/// set) when the proxy is trusted, else the TCP peer.
pub fn client_ip(trust_proxy: bool, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if trust_proxy {
        if let Some(ip) = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .rfind(|s| !s.is_empty())
        {
            return ip.to_owned();
        }
    }
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let t = match v.split_once(' ') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
        Some(_) => return None,
        None => v,
    };
    (!t.is_empty()).then_some(t)
}

/// `WWW-Authenticate: Bearer resource_metadata="…", scope="…"[, error=…]`.
pub fn challenge(st: &AppState, error: Option<&str>) -> String {
    // The scopes a client should ask for by default; `agents` only when it wants it.
    let scopes = Scopes::default_grant().to_oauth();
    let mut c = format!(
        "Bearer resource_metadata=\"{}\", scope=\"{scopes}\"",
        st.cfg.resource_metadata_url()
    );
    if let Some(e) = error {
        c.push_str(&format!(", error=\"{e}\""));
    }
    c
}

fn unauthorized(st: &AppState, error: Option<&str>, msg: &str) -> Response {
    let mut r = (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": error.unwrap_or("unauthorized"), "error_description": msg})),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&challenge(st, error)) {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

pub async fn require_auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(token) = presented_token(req.headers()).map(str::to_owned) else {
        return unauthorized(&st, None, "a Bearer token is required");
    };
    let found = if token.starts_with(ACCESS_PREFIX) {
        st.oauth.authenticate(&token, &st.cfg.resource())
    } else if token.starts_with(TOKEN_PREFIX) {
        st.tokens.authenticate(&token)
    } else {
        Ok(None)
    };
    let caller = match found {
        Ok(Some(c)) => c,
        Ok(None) => {
            let peer = req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0);
            tracing::warn!(target: "secretary_mcp::audit", event = "auth_failed", ip = %client_ip(st.cfg.trust_proxy, req.headers(), peer));
            return unauthorized(
                &st,
                Some("invalid_token"),
                "unknown, expired or revoked token",
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "token store unreadable");
            return (StatusCode::INTERNAL_SERVER_ERROR, "token store error").into_response();
        }
    };
    req.extensions_mut().insert(caller);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_from_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(presented_token(&h), None);
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer mcp_abc"),
        );
        assert_eq!(presented_token(&h), Some("mcp_abc"));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer  mcpa_x "),
        );
        assert_eq!(presented_token(&h), Some("mcpa_x"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("mcp_raw"));
        assert_eq!(presented_token(&h), Some("mcp_raw"));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic Zm9v"),
        );
        assert_eq!(presented_token(&h), None);
    }

    #[test]
    fn ip_selection() {
        let mut h = HeaderMap::new();
        h.append(
            "x-forwarded-for",
            HeaderValue::from_static("1.1.1.1, 2.2.2.2"),
        );
        let peer: SocketAddr = "127.0.0.1:5".parse().unwrap();
        assert_eq!(client_ip(true, &h, Some(peer)), "2.2.2.2");
        assert_eq!(client_ip(false, &h, Some(peer)), "127.0.0.1");
        assert_eq!(client_ip(false, &HeaderMap::new(), None), "unknown");
    }
}
