//! A fake of secretary-server's `/v1/lucina/reach/*`: the server forwards to Lucina's
//! `/api/secretary/reach/*` and unwraps `data`, so the shapes below are the thread views
//! of lucina-rs. It answers the device token only.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::device_ok;

pub const OPEN: &str = "rt-0123456789ab";
pub const ANSWERED: &str = "rt-abcdef012345";
pub const MISSING: &str = "rt-ffffffffffff";

fn thread(id: &str, status: &str, replies: Value) -> Value {
    json!({"id": id, "to": "claude", "kind": "issue", "subject": "Share link opens without sign-in",
        "text": "The share link in the iOS app\nopens without a session.", "channel": "ios",
        "created": "2026-10-30T13:05:00Z", "status": status, "route": "lucina-mailbox",
        "detail": null, "replies": replies})
}

pub fn route(method: &str, rest: &[&str], query: &str, headers: &HeaderMap) -> Response {
    if !device_ok(headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"code": "unauthorized", "message": "device token required"}})),
        )
            .into_response();
    }
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"code": "thread_not_found", "message": "no such thread"}})),
        )
            .into_response()
    };
    match (method, rest) {
        ("GET", ["threads"]) => {
            let mut all = vec![
                thread(OPEN, "sent", json!([])),
                thread(ANSWERED, "answered", json!([
                    {"from": "coordinator (mcp:claude)", "text": "Fixed.", "at": "2026-10-30T14:00:00Z"}])),
            ];
            if query.contains("status=open") {
                all.truncate(1);
            }
            if query.contains("to=grok") {
                all.clear();
            }
            Json(json!({"threads": all})).into_response()
        }
        ("GET", ["threads", id]) if *id == OPEN => {
            let mut t = thread(OPEN, "sent", json!([]));
            t["envelope"] = json!("[Lucina → Claude] thread");
            Json(t).into_response()
        }
        ("GET", ["threads", id]) if *id == ANSWERED => Json(thread(
            ANSWERED,
            "answered",
            json!([{"from": "coordinator (mcp:claude)", "text": "Fixed.", "at": "2026-10-30T14:00:00Z"}]),
        ))
        .into_response(),
        ("POST", ["threads", id, "reply"]) if *id == OPEN || *id == ANSWERED => {
            Json(json!({"thread": id, "status": "answered", "delivered": true})).into_response()
        }
        _ => not_found(),
    }
}
