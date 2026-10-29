//! Caller screening for the phones and secretary-server: `/api/secretary/callers/*`,
//! on top of `lucina_tools::tools::callers` (Jev, the caller store).
//!
//! - `POST /callers/screen`   `{numbers, lang?, context?, call?}` → `{lang, results, table}`
//!   (`docs`: the caller-screen contract; `call` records an incoming call)
//! - `GET  /callers/directory[?block=1&spam=&pickup=]` → `{generatedAt, entries: [{number, label}], block}`
//!   for the iOS Call Directory: labels for numbers that called or were looked up,
//!   and (with `block=1`) the numbers above the owner's auto-reject thresholds.
//! - `POST /callers/rejected` `{number, line, spam?, pickUp?, sources, source, at?}`: a phone rejected a call.
//! - `POST /callers/allow` `{number}`: the owner's "Allow" (never rejected or blocked again).
//!
//! Background ([`spawn`]): tellows' Scorelist refresh (daily, with partner keys)
//! and the rejected-calls digest in chat (`LUCINA_CALLERS_DIGEST_HOUR`, default 21).
//!
//! 404 `callers_disabled` when `LUCINA_CALLERS=off`. Jev down still answers
//! (the rules, `source: "rules"`). Nothing here touches a call.

use axum::body::Body;
use axum::http::{Method, Uri};
use axum::response::Response;
use chrono::Utc;
use lucina_phone_core::caller::Number;
use lucina_phone_core::caller::RejectSettings;
use lucina_tools::tools::callers::{self as tc, Rejection, ScreenRequest};
use serde_json::{json, Value};

use crate::api::{ok, query_param, read_json, ApiError};
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

/// tellows' Scorelist on disk.
pub fn tellows_path(st: &AppState) -> std::path::PathBuf {
    st.cfg.data_dir.join("callers").join("tellows-dk.csv")
}

/// The report sources configured on this runtime.
pub fn lookup(st: &AppState) -> tc::PublicLookup {
    tc::PublicLookup {
        base: st.cfg.callers_public_url.clone(),
        spamcalls_base: st.cfg.callers_it_url.clone(),
        ipqs: st.cfg.callers_ipqs_key.as_ref().map(|key| tc::Ipqs {
            http: st.tool_env.http.clone(),
            key: key.clone(),
            base: tc::IPQS_BASE.into(),
        }),
        tellows_list: st.cfg.callers_tellows.is_some().then(|| tellows_path(st)),
    }
}

/// Screens `req` with everything Lucina knows.
pub async fn screen(st: &AppState, req: &ScreenRequest) -> Result<tc::ScreenResponse, String> {
    let texts = owner_texts(st);
    let public = lookup(st);
    tc::screen(
        lucina_jev::shared(),
        st.tool_env.callers.as_deref(),
        Some(&public),
        req,
        Utc::now(),
        &|n| mentions_in(&texts, n),
    )
    .await
}

pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
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
            let block = match query_param(uri, "block").as_deref() {
                Some("1" | "true") => Some(thresholds(uri)?),
                None | Some("" | "0" | "false") => None,
                Some(_) => return Err(ApiError::bad_request("block must be 0 or 1")),
            };
            let d = st
                .tool_env
                .callers
                .as_ref()
                .map(|s| s.directory(block.as_ref()))
                .unwrap_or_default();
            let entries: Vec<Value> = d
                .entries
                .into_iter()
                .map(|(number, label)| json!({"number": number, "label": label}))
                .collect();
            Ok(ok(json!({
                "generatedAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "entries": entries,
                "block": d.block,
            })))
        }
        ["rejected"] => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let b: RejectedBody = serde_json::from_value(read_json(body, MAX_BODY).await?)
                .map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))?;
            let r = b.validate()?;
            if let Some(s) = st.tool_env.callers.as_ref() {
                s.record_rejection(r);
            }
            tracing::info!("callers: a phone rejected a call");
            Ok(ok(json!({"recorded": true})))
        }
        ["allow"] => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let b: AllowBody = serde_json::from_value(read_json(body, MAX_BODY).await?)
                .map_err(|e| ApiError::bad_request(format!("invalid body: {e}")))?;
            let n = lucina_phone_core::caller::parse(&b.number);
            if !n.valid() {
                return Err(ApiError::bad_request("number is not a phone number"));
            }
            if let Some(s) = st.tool_env.callers.as_ref() {
                s.allow(&n.e164, &Utc::now().to_rfc3339());
            }
            Ok(ok(json!({"allowed": true, "number": n.e164})))
        }
        _ => Err(ApiError::not_found()),
    }
}

/// `spam` and `pickup` (0..1) on top of the owner's defaults.
fn thresholds(uri: &Uri) -> Result<RejectSettings, ApiError> {
    let mut s = RejectSettings::default();
    for (key, slot) in [("spam", &mut s.spam_min), ("pickup", &mut s.pick_up_max)] {
        if let Some(v) = query_param(uri, key) {
            *slot = v
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| ApiError::bad_request(format!("{key} must be 0..1")))?;
        }
    }
    Ok(s)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RejectedBody {
    number: String,
    line: String,
    spam: Option<f64>,
    pick_up: Option<f64>,
    #[serde(default)]
    sources: String,
    source: String,
    at: Option<chrono::DateTime<Utc>>,
}

impl RejectedBody {
    fn validate(self) -> Result<Rejection, ApiError> {
        let n = lucina_phone_core::caller::parse(&self.number);
        let p = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v));
        if !n.valid()
            || !matches!(self.source.as_str(), "android" | "ios")
            || self.line.trim().is_empty()
            || self.line.chars().count() > 300
            || self.sources.chars().count() > 300
            || !p(self.spam)
            || !p(self.pick_up)
        {
            return Err(ApiError::bad_request(
                "number, line (1-300), source android|ios, spam/pickUp 0..1, sources (≤300)",
            ));
        }
        Ok(Rejection {
            at: self.at.unwrap_or_else(Utc::now).to_rfc3339(),
            number: n.e164,
            line: self.line.trim().to_owned(),
            spam: self.spam,
            pick_up: self.pick_up,
            sources: self.sources,
            source: self.source,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowBody {
    number: String,
}

/// The digest text for the last day's rejections (local times), or `None` when there were none.
pub fn digest_text(rejections: &[Rejection], tz: chrono_tz::Tz) -> Option<String> {
    if rejections.is_empty() {
        return None;
    }
    let mut out = format!(
        "📵 Calls I rejected in the last 24 hours ({}):\n",
        rejections.len()
    );
    for r in rejections
        .iter()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let t =
            r.at.parse::<chrono::DateTime<Utc>>()
                .map(|t| t.with_timezone(&tz).format("%H:%M").to_string())
                .unwrap_or_default();
        out.push_str(&format!("• {t} {}\n", r.line));
    }
    if rejections.len() > 20 {
        out.push_str(&format!("…and {} more.\n", rejections.len() - 20));
    }
    out.push_str("\nIf one was wrong, tell me \"allow <number>\" and I won't reject it again.");
    Some(out)
}

/// Posts the digest once a day at the configured hour (timeline + app notification).
async fn digest_tick(st: &AppState, now: chrono::DateTime<Utc>) {
    let (Some(store), Some(hour)) = (st.tool_env.callers.as_ref(), st.cfg.callers_digest_hour)
    else {
        return;
    };
    let local = now.with_timezone(&st.cfg.time_zone);
    let today = local.format("%Y-%m-%d").to_string();
    if chrono::Timelike::hour(&local) < hour
        || store.digest_sent().as_deref() == Some(today.as_str())
    {
        return;
    }
    store.set_digest_sent(&today);
    let Some(text) = digest_text(
        &store.rejections_since(now - chrono::Duration::hours(24)),
        st.cfg.time_zone,
    ) else {
        return;
    };
    let item = st.timeline.record(lucina_store::timeline::NewItem {
        channel: "web".into(),
        role: "lucina".into(),
        text: text.clone(),
        meta: Some(lucina_store::timeline::Meta::kind("caller_digest")),
        ..Default::default()
    });
    if st.cfg.app_notify {
        let mut body = json!({"category": "reminder", "body": text, "route": "/",
            "dedupKey": format!("caller-digest:{today}")});
        if let Some(item) = item {
            body["timelineId"] = json!(item.id);
        }
        let r = crate::bridges::notify(&st.secretary, &body).await;
        if !r.ok {
            tracing::warn!(status = r.status, "caller digest notification failed");
        }
    }
}

/// tellows' Scorelist refresh (daily) and the digest (checked every 10 minutes).
pub fn spawn(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    if !st.cfg.callers_enabled {
        return None;
    }
    let st = st.clone();
    Some(tokio::spawn(async move {
        let mut last_tellows: Option<std::time::Instant> = None;
        loop {
            if let Some((partner, key)) = st.cfg.callers_tellows.clone() {
                let due = last_tellows
                    .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(24 * 3600));
                if due {
                    last_tellows = Some(std::time::Instant::now());
                    match tc::refresh_tellows(
                        &st.tool_env.http,
                        "https://www.tellows.de",
                        &partner,
                        &key,
                        &tellows_path(&st),
                    )
                    .await
                    {
                        Ok(n) => tracing::info!(rows = n, "tellows Scorelist refreshed"),
                        Err(e) => tracing::warn!(error = %e, "tellows Scorelist refresh failed"),
                    }
                }
            }
            digest_tick(&st, Utc::now()).await;
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_phone_core::caller::parse;

    #[test]
    fn the_digest_lists_rejections_in_local_time() {
        assert!(digest_text(&[], chrono_tz::Europe::Copenhagen).is_none());
        let r = Rejection {
            at: "2026-10-29T13:02:00Z".into(),
            number: "+4570405205".into(),
            line: "Rejected likely telemarketer +45 70 40 52 05 · 180.dk 23 reports".into(),
            spam: Some(0.75),
            pick_up: Some(0.16),
            sources: "180.dk 23 reports".into(),
            source: "android".into(),
        };
        let t = digest_text(&[r], chrono_tz::Europe::Copenhagen).unwrap();
        assert!(t.contains("(1)"));
        assert!(
            t.contains("• 14:02 Rejected likely telemarketer +45 70 40 52 05 · 180.dk 23 reports")
        );
        assert!(t.contains("allow <number>"));
    }

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
