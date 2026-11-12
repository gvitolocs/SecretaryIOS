//! `parcel`: Lucina follows the owner's parcels herself (crate `lucina_parcels`, through the local
//! Universal Parcel Scraper). Owner's turns only.

use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use lucina_parcels::{text, Cadence, Parcel, Service};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Start following a parcel by its tracking number.
    Add,
    /// Every parcel being followed, with the last status.
    List,
    /// Where one parcel is now (looked up again when the last check is old).
    Status,
    /// Stop following a parcel and forget it.
    Remove,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Action,
    /// add: the tracking number, as he gave it.
    #[serde(default)]
    number: Option<String>,
    /// add: the carrier id when known (e.g. postnord, gls, dhl, poste-italiane); leave out to detect it.
    #[serde(default)]
    carrier: Option<String>,
    /// add: a name for the parcel ("Red shoes").
    #[serde(default)]
    label: Option<String>,
    /// status, remove: the parcel's id, tracking number or label.
    #[serde(default)]
    id: Option<String>,
}

pub struct ParcelTool;

fn need<'a>(v: &'a Option<String>, what: &str) -> Result<&'a str, ToolError> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new(format!("this action needs {what}")))
}

fn cadence_name(c: Cadence) -> &'static str {
    match c {
        Cadence::Hourly => "checked every hour",
        Cadence::Daily => "checked once a day, in the morning digest",
        Cadence::Stopped => "no longer followed",
    }
}

/// One parcel as the model sees it.
fn view(svc: &Service, p: &Parcel) -> Value {
    let now = Utc::now();
    let mut v = json!({
        "id": p.id,
        "name": text::name(p),
        "number": p.number,
        "state": p.state,
        "cadence": cadence_name(svc.cadence_of(p, now)),
    });
    if let Some(s) = &p.status {
        v["stage"] = json!(s.stage.as_str());
        v["status"] = json!(text::status_line(s, svc.cfg.tz));
    }
    if let Some(e) = &p.last_error {
        v["last_error"] = json!(e);
    }
    v
}

#[async_trait::async_trait]
impl Tool for ParcelTool {
    fn name(&self) -> &'static str {
        "parcel"
    }
    fn description(&self) -> &'static str {
        "Follows Giuseppe's parcels by tracking number, by herself (no carrier name needed). add: start following \
one (she looks it up once and tells you the status; she then checks it daily and hourly once it is close, and tells \
him of changes). list: the parcels she follows. status: where one is now. remove: stop following one. Never ask \
him for the carrier first."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "parcel {} {}",
            args["action"].as_str().unwrap_or(""),
            args["number"]
                .as_str()
                .or(args["id"].as_str())
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>()
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let svc = ctx
            .env
            .parcels
            .clone()
            .ok_or_else(|| ToolError::new("parcel tracking is off on this server"))?;
        let now = Utc::now();
        match a.action {
            Action::Add => {
                let added = svc
                    .add(
                        need(&a.number, "number")?,
                        a.carrier.as_deref(),
                        a.label.as_deref(),
                        now,
                        true,
                    )
                    .await;
                let mut v = view(&svc, &added.parcel);
                v["already_tracked"] = json!(!added.created);
                if let Err(e) = &added.lookup {
                    let kept = if added.saved {
                        "saved, but the first lookup failed"
                    } else {
                        "not saved, the lookup failed"
                    };
                    v["lookup"] = json!(format!("{kept}: {}", text::failure_line(e)));
                }
                Ok(v)
            }
            Action::List => {
                let items: Vec<Value> = svc
                    .store
                    .list()
                    .iter()
                    .filter(|p| p.state == lucina_parcels::ParcelState::Active)
                    .map(|p| view(&svc, p))
                    .collect();
                Ok(json!({ "parcels": items }))
            }
            Action::Status => {
                let key = need(&a.id, "id (or number or label)")?;
                let p = svc
                    .store
                    .get(key)
                    .ok_or_else(|| ToolError::new(format!("no parcel matches {key}")))?;
                let (p, failed) = svc
                    .refresh_unless_fresh(&p.id, now)
                    .await
                    .ok_or_else(|| ToolError::new(format!("no parcel matches {key}")))?;
                let mut v = view(&svc, &p);
                if let Some(e) = failed {
                    v["lookup"] = json!(format!("couldn't refresh: {}", text::failure_line(&e)));
                }
                Ok(v)
            }
            Action::Remove => {
                let key = need(&a.id, "id (or number or label)")?;
                let p = svc
                    .store
                    .remove(key)
                    .ok_or_else(|| ToolError::new(format!("no parcel matches {key}")))?;
                Ok(json!({ "removed": text::name(&p) }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use lucina_parcels::{Home, ScheduleCfg, Scraper, Store};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn ctx(server: &MockServer) -> (Ctx, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut env = crate::ToolEnv::new(dir.path(), chrono_tz::Europe::Copenhagen);
        let store = Arc::new(Store::open(dir.path().join("parcels.json")));
        let cfg = ScheduleCfg::new(chrono_tz::Europe::Copenhagen, 9, Home::default());
        env.parcels = Some(Arc::new(Service::new(
            store,
            Scraper::new(&server.uri()),
            cfg,
        )));
        (Ctx::new(Arc::new(env), "web", "s1"), dir)
    }

    async fn scraper_stub() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/detect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "trackingNumber": "3UW1S8IAAKA23", "source": "number", "carrier": "poste-italiane",
                "confidence": "high", "candidates": ["poste-italiane"], "preferred": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/track"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "carrier": "poste-italiane", "source": "poste-italiane", "attempts": [],
                "result": {"current_stage": "in_transit", "last_status_text": "In transit", "last_update": "2026-09-28T08:00:00Z",
                           "events": [{"time": "2026-09-28T08:00:00Z", "description": "In transit", "location": "Milano", "stage": "in_transit", "instant": "2026-09-28T08:00:00Z"}]}
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn add_list_status_remove() {
        let server = scraper_stub().await;
        let (c, _dir) = ctx(&server).await;
        let r = ParcelTool
            .call(
                json!({"action": "add", "number": "3uw1s8iaaka23", "label": "Shoes"}),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(r["name"], "Shoes");
        assert_eq!(r["stage"], "in_transit");
        assert_eq!(
            r["status"].as_str().unwrap().split(" · ").next(),
            Some("In transit")
        );
        assert_eq!(r["cadence"], "checked once a day, in the morning digest");
        assert_eq!(r["already_tracked"], false);

        let again = ParcelTool
            .call(json!({"action": "add", "number": "3UW1S8IAAKA23"}), &c)
            .await
            .unwrap();
        assert_eq!(
            (again["already_tracked"].clone(), again["id"].clone()),
            (json!(true), r["id"].clone())
        );

        let l = ParcelTool
            .call(json!({"action": "list"}), &c)
            .await
            .unwrap();
        assert_eq!(l["parcels"].as_array().unwrap().len(), 1);

        // Freshly checked: status does not call the carrier again.
        let before = server.received_requests().await.unwrap().len();
        let s = ParcelTool
            .call(json!({"action": "status", "id": "shoes"}), &c)
            .await
            .unwrap();
        assert_eq!(s["number"], "3UW1S8IAAKA23");
        assert_eq!(server.received_requests().await.unwrap().len(), before);

        let gone = ParcelTool
            .call(json!({"action": "remove", "id": "3UW1S8IAAKA23"}), &c)
            .await
            .unwrap();
        assert_eq!(gone["removed"], "Shoes");
        assert!(ParcelTool
            .call(json!({"action": "status", "id": "shoes"}), &c)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_failed_first_lookup_keeps_the_parcel() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/detect"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"source": "none", "confidence": "none", "candidates": []}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/track"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"error": "nope", "hint": {"kind": "not_found"}})),
            )
            .mount(&server)
            .await;
        let (c, _dir) = ctx(&server).await;
        let r = ParcelTool
            .call(json!({"action": "add", "number": "ZZ9"}), &c)
            .await
            .unwrap();
        assert!(
            r["lookup"]
                .as_str()
                .unwrap()
                .contains("the carrier has no record of it yet"),
            "{r}"
        );
        let l = ParcelTool
            .call(json!({"action": "list"}), &c)
            .await
            .unwrap();
        assert_eq!(l["parcels"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn off_without_a_service() {
        let env = crate::ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        let c = Ctx::new(Arc::new(env), "web", "s1");
        assert!(ParcelTool
            .call(json!({"action": "list"}), &c)
            .await
            .is_err());
    }
}
