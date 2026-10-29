//! Caller screening for the phones and secretary-server: `/api/secretary/callers/*`,
//! on top of `lucina_tools::tools::callers` (Jev, the caller store).
//!
//! - `POST /callers/screen`   `{numbers, lang?, context?, call?}` → `{lang, results, table}`
//!   (`docs`: the caller-screen contract; `call` records an incoming call)
//! - `GET  /callers/directory` → `{generatedAt, entries: [{number, label}]}` for the
//!   iOS Call Directory: numbers that called or were looked up, ascending.
//!
//! 404 `callers_disabled` when `LUCINA_CALLERS=off`. Jev down still answers
//! (the rules, `source: "rules"`). Nothing here touches a call.

use axum::body::Body;
use axum::http::{Method, Uri};
use axum::response::Response;
use chrono::Utc;
use lucina_phone_core::caller::Number;
use lucina_tools::tools::callers::{self as tc, ScreenRequest};
use serde_json::{json, Value};

use crate::api::{ok, read_json, ApiError};
use crate::AppState;

/// Bytes of a screen request.
const MAX_BODY: usize = 8 * 1024;
/// Mentions per number, and characters per mention.
const MAX_MENTIONS: usize = 3;
const MENTION_CHARS: usize = 160;

/// How the owner (or a phone) may write `n`.
fn spellings(n: &Number) -> Vec<String> {
    let mut v = vec![
        n.e164.clone(),
        n.display(),
        n.e164.trim_start_matches('+').to_owned(),
    ];
    if n.country == "DK" {
        v.push(n.national.clone());
        v.push(n.display().trim_start_matches("+45 ").to_owned());
    }
    v.retain(|s| s.len() >= 8);
    v
}

/// The owner's own messages that mention `n` and say something about it: not
/// lookups (no other number, no caller-screen request), newest last.
pub fn mentions_in(texts: &[String], n: &Number) -> Vec<String> {
    let ways = spellings(n);
    let mut out: Vec<String> = texts
        .iter()
        .filter(|t| ways.iter().any(|w| t.contains(w.as_str())))
        .filter(|t| lucina_phone_core::caller::find_numbers(t).len() == 1)
        .filter(|t| !crate::chat::callers_turn::lookup_cue(t))
        .map(|t| {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.chars().count() > MENTION_CHARS {
                t.chars().take(MENTION_CHARS - 1).collect::<String>() + "…"
            } else {
                t
            }
        })
        .collect();
    let skip = out.len().saturating_sub(MAX_MENTIONS);
    out.drain(..skip);
    out
}

/// The owner's messages in the timeline (text only).
pub fn owner_texts(st: &AppState) -> Vec<String> {
    let Ok(items) = st.timeline.read_all() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|i| serde_json::from_str::<Value>(&i.raw).ok())
        .filter(|v| v["role"] == "user")
        .filter_map(|v| v["text"].as_str().map(str::to_owned))
        .collect()
}

/// Screens `req` with everything Lucina knows.
pub async fn screen(st: &AppState, req: &ScreenRequest) -> Result<tc::ScreenResponse, String> {
    let texts = owner_texts(st);
    let public = st
        .cfg
        .callers_public_url
        .as_ref()
        .map(|base| tc::PublicLookup { base: base.clone() });
    tc::screen(
        lucina_jev::shared(),
        st.tool_env.callers.as_deref(),
        public.as_ref(),
        req,
        Utc::now(),
        &|n| mentions_in(&texts, n),
    )
    .await
}

pub async fn route(
    st: &AppState,
    method: &Method,
    _uri: &Uri,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    if !st.cfg.callers_enabled {
        return Err(ApiError::new(
            404,
            "callers_disabled",
            "Caller screening is off (LUCINA_CALLERS)",
        ));
    }
    match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["screen"] => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let req: ScreenRequest = serde_json::from_value(read_json(body, MAX_BODY).await?)
                .map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))?;
            req.validate().map_err(ApiError::bad_request)?;
            let r = screen(st, &req).await.map_err(ApiError::bad_request)?;
            tracing::info!(
                numbers = r.results.len(),
                call = req.call.is_some(),
                sources = ?r.results.iter().map(|v| v.source.as_str()).collect::<Vec<_>>(),
                "callers screen"
            );
            Ok(ok(serde_json::to_value(&r).unwrap_or(Value::Null)))
        }
        ["directory"] => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let entries: Vec<Value> = st
                .tool_env
                .callers
                .as_ref()
                .map(|s| s.directory())
                .unwrap_or_default()
                .into_iter()
                .map(|(number, label)| json!({"number": number, "label": label}))
                .collect();
            Ok(ok(json!({
                "generatedAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "entries": entries,
            })))
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_phone_core::caller::parse;

    #[test]
    fn mentions_skip_lookups_and_lists() {
        let n = parse("+45 20 68 27 58");
        let texts: Vec<String> = [
            "Jerry's Pizza calls me from 20 68 27 58 when an order is ready",
            "who called me from +45 20 68 27 58?",
            "+45 20 68 27 58 and +45 70 40 52 05",
            "nothing about it",
            "the guy at 4520682758 is my landlord",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            mentions_in(&texts, &n),
            vec![
                "Jerry's Pizza calls me from 20 68 27 58 when an order is ready".to_owned(),
                "the guy at 4520682758 is my landlord".to_owned()
            ]
        );
        assert!(mentions_in(&texts, &parse("+4570405205")).is_empty());
    }
}
