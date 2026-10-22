//! `GET /internal/status`: the AI clients connected to the Lucina MCP, for
//! the Secretary app's Links tab (secretary-server reads it on localhost).
//!
//! - Needs `Authorization: Bearer <status token>` (the one line in
//!   `SECRETARY_MCP_STATUS_TOKEN_FILE`, the same file content secretary-server
//!   reads); `404` when no status token is configured, so the route does not
//!   exist by default. Caddy does not route `/internal/*` here either.
//! - Lists caller names, how they connect (`token` or `oauth`), scopes,
//!   creation and last-use times, and whether they are active. Never token
//!   ids, hashes or client ids.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::oauth::TokenKind;
use crate::store::unix_now;
use crate::AppState;

fn authorized(app: &AppState, headers: &HeaderMap) -> Option<bool> {
    let want = app.cfg.status_token.as_deref()?;
    let got = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    Some(bool::from(got.as_bytes().ct_eq(want.as_bytes())))
}

pub async fn internal_status(State(app): State<AppState>, headers: HeaderMap) -> Response {
    match authorized(&app, &headers) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(false) => return StatusCode::UNAUTHORIZED.into_response(),
        Some(true) => {}
    }
    let now = unix_now();
    let ms = |s: u64| s.saturating_mul(1000);
    let mut clients: Vec<Value> = Vec::new();
    match app.tokens.list() {
        Ok(list) => {
            for t in list {
                clients.push(json!({
                    "caller": t.name, "via": "token", "scopes": t.scopes,
                    "createdAt": ms(t.created_at), "lastUsedAt": t.last_used_at.map(ms),
                    "active": !t.revoked,
                }));
            }
        }
        Err(e) => tracing::warn!(error = %e, "tokens unreadable for status"),
    }
    let ttl = app.cfg.access_ttl.as_secs();
    match app.oauth.data() {
        Ok(d) => {
            for g in &d.grants {
                let mine = d.tokens.iter().filter(|t| t.grant_id == g.id);
                let alive = mine
                    .clone()
                    .any(|t| t.expires_at > now && !(t.kind == TokenKind::Refresh && t.used));
                // An access token's issue time approximates the last use.
                let last = mine
                    .filter(|t| t.kind == TokenKind::Access)
                    .map(|t| t.expires_at.saturating_sub(ttl))
                    .max();
                clients.push(json!({
                    "caller": g.caller, "via": "oauth", "name": g.client_name, "scopes": g.scopes,
                    "createdAt": ms(g.created_at), "lastUsedAt": last.map(ms),
                    "active": !g.revoked && alive,
                }));
            }
        }
        Err(e) => tracing::warn!(error = %e, "oauth data unreadable for status"),
    }
    let mut r = Json(json!({ "clients": clients })).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().expect("static"));
    r
}
