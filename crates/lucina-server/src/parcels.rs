//! Parcel tracking on the server (crate `lucina_parcels`): the settings, the tracker loop that
//! checks the owner's parcels and tells him of changes, and the `lucina-rs parcels` CLI helpers.
//!
//! The loop ([`tick`], every 5 minutes) delivers what `lucina_parcels::tick` returns the way the
//! caller digest does: a timeline record and, with `LUCINA_APP_NOTIFY`, an app notification.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Method;
use axum::response::Response;
use chrono::Utc;
use lucina_parcels::{Home, OwnerMessage, ScheduleCfg, Scraper, Service, Store};
use lucina_store::timeline::{Meta, NewItem};
use serde_json::json;

use crate::api::{ok, read_json, ApiError};
use crate::AppState;

const BODY_BYTES: usize = 8 * 1024;
const LOOP_EVERY: Duration = Duration::from_secs(300);

/// `LUCINA_PARCELS*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParcelSettings {
    /// `LUCINA_PARCELS=off`: no tool, no chat turn, no loop.
    pub enabled: bool,
    /// `LUCINA_PARCEL_SCRAPER_URL` (default `http://127.0.0.1:8796`).
    pub scraper_url: String,
    /// `LUCINA_PARCELS_DIGEST_HOUR` (default 9, 0-23): the local hour of the daily pass.
    pub digest_hour: u32,
    /// `LUCINA_PARCEL_HOME_CITY`: a parcel whose newest scan is in this city is checked hourly.
    pub home_city: Option<String>,
    /// `<data>/parcels/parcels.json`.
    pub store: PathBuf,
}

impl ParcelSettings {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let val = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Self {
            enabled: !matches!(
                val("LUCINA_PARCELS")
                    .map(|v| v.to_ascii_lowercase())
                    .as_deref(),
                Some("off" | "false" | "0" | "no")
            ),
            scraper_url: val("LUCINA_PARCEL_SCRAPER_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8796".into()),
            digest_hour: val("LUCINA_PARCELS_DIGEST_HOUR")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|h| *h <= 23)
                .unwrap_or(9),
            home_city: val("LUCINA_PARCEL_HOME_CITY"),
            store: data_dir.join("parcels").join("parcels.json"),
        }
    }

    pub fn schedule(&self, tz: chrono_tz::Tz) -> ScheduleCfg {
        ScheduleCfg::new(
            tz,
            self.digest_hour,
            Home {
                city: self.home_city.clone(),
            },
        )
    }
}

/// The service the tool, the chat turn and the loop share; `None`: `LUCINA_PARCELS=off`.
pub fn open(s: &ParcelSettings, tz: chrono_tz::Tz) -> Option<Arc<Service>> {
    if !s.enabled {
        return None;
    }
    tracing::info!(store = %s.store.display(), scraper = %s.scraper_url, "parcel tracking: on");
    Some(Arc::new(Service::new(
        Arc::new(Store::open(&s.store)),
        Scraper::new(&s.scraper_url),
        s.schedule(tz),
    )))
}

/// Timeline record and app notification for each message, like `callers::digest_tick`.
pub async fn deliver(st: &AppState, messages: &[OwnerMessage]) {
    for m in messages {
        let item = st.timeline.record(NewItem {
            channel: "web".into(),
            role: "lucina".into(),
            text: m.text.clone(),
            meta: Some(Meta::kind("parcel_update")),
            ..Default::default()
        });
        if st.cfg.app_notify {
            let mut body = json!({"category": "reminder", "body": m.text, "route": "/", "dedupKey": m.dedup_key});
            if let Some(item) = item {
                body["timelineId"] = json!(item.id);
            }
            let r = crate::bridges::notify(&st.secretary, &body).await;
            if !r.ok {
                tracing::warn!(status = r.status, "parcel notification failed");
            }
        }
    }
}

/// A tracking number the API accepts: 4 to 80 characters.
const NUMBER_CHARS: std::ops::RangeInclusive<usize> = 4..=80;
const LABEL_MAX: usize = 80;
const CARRIER_MAX: usize = 40;

fn object(v: &serde_json::Value, allowed: &[&str]) -> Result<(), ApiError> {
    let Some(o) = v.as_object() else {
        return Err(ApiError::bad_request("the body must be a JSON object"));
    };
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ApiError::bad_request(format!("unknown field {k}"))),
        None => Ok(()),
    }
}

/// An optional text field: absent or null is `None`, blank is `None`, else trimmed and at most `max` characters.
fn text_field(v: &serde_json::Value, key: &str, max: usize) -> Result<Option<String>, ApiError> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let s = s.trim();
            if s.chars().count() > max {
                return Err(ApiError::bad_request(format!(
                    "{key} must be at most {max} characters"
                )));
            }
            Ok((!s.is_empty()).then(|| s.to_owned()))
        }
        Some(_) => Err(ApiError::bad_request(format!("{key} must be text"))),
    }
}

fn unknown_parcel() -> ApiError {
    ApiError::new(404, "not_found", "no such parcel")
}

/// `/api/secretary/parcels...` (`parts` after `parcels`): the Parcels API of the apps.
///
/// - `GET  /parcels`               `{parcels, digestHour}`, active first, newest change first
/// - `POST /parcels`               `{number, carrier?, label?}`: detect, save, look up once; `{parcel}`
/// - `GET  /parcels/{id}`          `{parcel}`
/// - `POST /parcels/{id}/refresh`  `{parcel}`, looked up now unless checked under 10 minutes ago
/// - `PATCH /parcels/{id}`         `{label: text|null}`: `{parcel}`
/// - `DELETE /parcels/{id}`        `{ok: true}`
///
/// Off (`LUCINA_PARCELS=off`): 404 `parcels_disabled`; an unknown id: 404 `not_found`.
pub async fn route(
    st: &AppState,
    method: &Method,
    parts: &[String],
    body: Body,
) -> Result<Response, ApiError> {
    let svc = st.tool_env.parcels.clone().ok_or_else(|| {
        ApiError::new(
            404,
            "parcels_disabled",
            "Parcel tracking is off (LUCINA_PARCELS)",
        )
    })?;
    let only = |allowed: &[&str]| -> Result<(), ApiError> {
        if allowed.contains(&method.as_str()) {
            Ok(())
        } else {
            Err(ApiError::method_not_allowed(allowed))
        }
    };
    let now = Utc::now();
    let one = |p: &lucina_parcels::Parcel| ok(json!({ "parcel": svc.view(p, now) }));
    match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            only(&["GET", "POST"])?;
            if *method == Method::GET {
                let parcels: Vec<_> = lucina_parcels::ordered(svc.store.list())
                    .iter()
                    .map(|p| svc.view(p, now))
                    .collect();
                return Ok(ok(
                    json!({ "parcels": parcels, "digestHour": svc.cfg.digest_hour }),
                ));
            }
            let b = read_json(body, BODY_BYTES).await?;
            object(&b, &["number", "carrier", "label"])?;
            let number = text_field(&b, "number", usize::MAX)?.unwrap_or_default();
            if !NUMBER_CHARS.contains(&number.chars().count()) {
                return Err(ApiError::bad_request("number must be 4 to 80 characters"));
            }
            let carrier = text_field(&b, "carrier", CARRIER_MAX)?;
            let label = text_field(&b, "label", LABEL_MAX)?;
            let added = svc
                .add(&number, carrier.as_deref(), label.as_deref(), now, true)
                .await;
            if !added.saved {
                return Err(ApiError::bad_request(
                    "the carrier does not accept this number",
                ));
            }
            Ok(one(&added.parcel))
        }
        [id] => {
            only(&["GET", "PATCH", "DELETE"])?;
            if !lucina_parcels::valid_id(id) {
                return Err(unknown_parcel());
            }
            match *method {
                Method::GET => Ok(one(&svc.store.get_id(id).ok_or_else(unknown_parcel)?)),
                Method::DELETE => {
                    svc.store.remove_id(id).ok_or_else(unknown_parcel)?;
                    Ok(ok(json!({ "ok": true })))
                }
                _ => {
                    let b = read_json(body, BODY_BYTES).await?;
                    object(&b, &["label"])?;
                    if b.get("label").is_none() {
                        return Err(ApiError::bad_request("label is required (text or null)"));
                    }
                    let label = text_field(&b, "label", LABEL_MAX)?;
                    let p = svc
                        .store
                        .update(id, |p| p.label = label)
                        .ok_or_else(unknown_parcel)?;
                    Ok(one(&p))
                }
            }
        }
        [id, "refresh"] => {
            only(&["POST"])?;
            if !lucina_parcels::valid_id(id) {
                return Err(unknown_parcel());
            }
            let (p, _) = svc
                .refresh_unless_fresh(id, now)
                .await
                .ok_or_else(unknown_parcel)?;
            Ok(one(&p))
        }
        _ => Err(ApiError::not_found()),
    }
}

/// One pass of the loop.
pub async fn tick(st: &AppState) {
    let Some(svc) = st.tool_env.parcels.clone() else {
        return;
    };
    let messages = lucina_parcels::tick(&svc.store, &svc.scraper, Utc::now(), &svc.cfg).await;
    deliver(st, &messages).await;
}

/// The tracker loop; `None`: parcels are off.
pub fn spawn(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    st.tool_env.parcels.as_ref()?;
    let st = st.clone();
    Some(tokio::spawn(async move {
        loop {
            tick(&st).await;
            tokio::time::sleep(LOOP_EVERY).await;
        }
    }))
}

/// A service for the CLI: the same settings as the server, whatever `LUCINA_PARCELS` says.
pub fn open_cli() -> anyhow::Result<Arc<Service>> {
    let cfg = crate::Config::from_env()?;
    let mut s = cfg.parcels.clone();
    s.enabled = true;
    open(&s, cfg.time_zone).ok_or_else(|| anyhow::anyhow!("parcel tracking is off"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> ParcelSettings {
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        };
        ParcelSettings::from_lookup(get, Path::new("/data"))
    }

    #[test]
    fn defaults_and_overrides() {
        let d = settings(&[]);
        assert!(d.enabled);
        assert_eq!(
            (d.scraper_url.as_str(), d.digest_hour, d.home_city.clone()),
            ("http://127.0.0.1:8796", 9, None)
        );
        assert_eq!(d.store, Path::new("/data/parcels/parcels.json"));
        let s = settings(&[
            ("LUCINA_PARCELS", "off"),
            ("LUCINA_PARCEL_SCRAPER_URL", "http://x:1"),
            ("LUCINA_PARCELS_DIGEST_HOUR", "7"),
            ("LUCINA_PARCEL_HOME_CITY", " Aarhus "),
        ]);
        assert_eq!(
            (
                s.enabled,
                s.scraper_url.as_str(),
                s.digest_hour,
                s.home_city.as_deref()
            ),
            (false, "http://x:1", 7, Some("Aarhus"))
        );
        assert_eq!(
            settings(&[("LUCINA_PARCELS_DIGEST_HOUR", "24")]).digest_hour,
            9
        );
        assert_eq!(
            settings(&[("LUCINA_PARCELS_DIGEST_HOUR", "x")]).digest_hour,
            9
        );
    }
}
