//! LucinaDesk on p1 (`lucinadesk.v1`; contract: `docs/CONTRACT.md` in the lucinadesk
//! repo). Lucina (lucina-server `desk.rs`) owns the tickets, the takeover lease, the
//! watch-mode input filter and the audit; this side authenticates the owner and splices.
//!
//! - `POST /v1/computer/desk` `{sandbox?, mode?, codecs?}` (owner device: device token or web
//!   session) → `{ticket, protocol, wsPath, expiresInSecs, sandbox, mode, lan}`. The audit
//!   `device` is the authenticated device's name, never a body value. `lan` is the
//!   home-network door (`{url}`), offered like the live view's (`computer::lan_offer`): to
//!   the owner's app, from the house's public IP only; else null. Its socket is the same
//!   `wsPath` on that origin.
//! - `GET /v1/computer/desk/ws` (WebSocket) **without** device auth, like the live view: the
//!   single-use ticket in the `ticket.<t>` subprotocol is the capability, and Lucina checks
//!   it. The socket is spliced byte for byte to Lucina's `/api/secretary/computer/desk/ws`
//!   with the server's bearer; cookies are never forwarded. Caddy drops
//!   `Sec-WebSocket-Protocol` from its log.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use secretary_core::Call;
use serde_json::{json, Value};

use crate::auth::Identity;
use crate::computer::{caller, device_label, lan_door, lan_offer};
use crate::error::{ApiError, JsonBody};
use crate::AppState;

/// Where the apps open the socket.
pub const WS_PATH: &str = "/v1/computer/desk/ws";
const LUCINA_WS_PATH: &str = "/api/secretary/computer/desk/ws";

fn unavailable(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "desk_unavailable", message)
}

fn valid_ticket(t: &str) -> bool {
    t.len() == 43 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn mint(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: axum::http::HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let sandbox = match body.get("sandbox").and_then(Value::as_str).unwrap_or("workspace") {
        "workspace" => "workspace",
        "android" => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "desk_unsupported",
                "LucinaDesk runs on the workspace only",
            ))
        }
        _ => return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "sandbox: must be workspace")),
    };
    let mode = match body.get("mode").and_then(Value::as_str).unwrap_or("watch") {
        m @ ("watch" | "control") => m,
        _ => return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "mode: must be watch or control")),
    };
    let who = caller(&headers);
    // What the browser decodes, best first; Lucina keeps the known names and the gateway asks
    // the host for them. Anything but a short list of short strings is dropped.
    let codecs: Vec<&str> = body
        .get("codecs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter(|c| c.len() <= 8).take(8).collect())
        .unwrap_or_default();
    let call = Call::computer_desk(json!({
        "sandbox": sandbox,
        "mode": mode,
        "device": device_label(&ident, &who),
        "codecs": codecs,
    }))
    .with_caller(&who);
    let lucina = st.lucina().map_err(|_| unavailable("Lucina is not configured"))?;
    let data: Value = match lucina.call(&call).await {
        Ok(d) => d,
        Err(e) => {
            let e = ApiError::from(e);
            // 4xx (desk_unsupported, too_many_tickets, bad_request) pass on; an outage is 503.
            return Err(if e.status.is_server_error() { unavailable(e.message) } else { e });
        }
    };
    let ticket = data.get("ticket").and_then(Value::as_str).unwrap_or("");
    if !valid_ticket(ticket) {
        return Err(unavailable("Lucina sent no usable desk ticket"));
    }
    // The home-network door, like the live view's: the owner's app, from the house only.
    let client_ip = crate::auth::client_ip(&st, &headers, peer.map(|Extension(c)| c.0));
    let lan = if who == "app" { lan_offer(&who, &client_ip, lan_door(&st).await.as_ref()) } else { None };
    Ok((
        StatusCode::OK,
        Json(json!({
            "ticket": ticket,
            "protocol": data.get("protocol").cloned().unwrap_or(json!("lucinadesk.v1")),
            "wsPath": WS_PATH,
            "expiresInSecs": data.get("expiresInSecs").cloned().unwrap_or(json!(30)),
            "sandbox": sandbox,
            "mode": mode,
            "lan": lan,
        })),
    ))
}

async fn ws(State(st): State<AppState>, req: Request) -> Response {
    let upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if !upgrade {
        return ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "a WebSocket upgrade is required")
            .into_response();
    }
    let Some((addr, auth)) = st.lucina.as_ref().and_then(|l| l.passthrough()) else {
        return unavailable("Lucina is not configured").into_response();
    };
    let mut resp = crate::computer::socket(&addr, &auth, LUCINA_WS_PATH, req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    resp
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/computer/desk", post(mint))
}

/// The socket (capability: the ticket; see the module doc).
pub fn ws_routes() -> Router<AppState> {
    Router::new().route(WS_PATH, get(ws))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_are_43_base64url_chars() {
        assert!(valid_ticket(&"A-_z".repeat(11)[..43]));
        assert!(!valid_ticket(&"a".repeat(42)));
        assert!(!valid_ticket(&"a".repeat(44)));
        assert!(!valid_ticket(&format!("{}+", "a".repeat(42))));
        assert!(!valid_ticket(&format!("{}=", "a".repeat(42))));
    }
}
