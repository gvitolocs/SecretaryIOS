//! `places`: OpenStreetMap Nominatim search, with a polite User-Agent and
//! at most one request per second (Nominatim usage policy).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// What to find: an address, a place or a kind of place ("pharmacy", "Tivoli").
    query: String,
    /// Optional area to search in ("Copenhagen", "Nørrebro, København").
    near: Option<String>,
    /// Results, 1-10 (default 5).
    #[schemars(range(min = 1, max = 10))]
    limit: Option<u8>,
}

pub struct PlacesTool;

#[async_trait::async_trait]
impl Tool for PlacesTool {
    fn name(&self) -> &'static str {
        "places"
    }
    fn description(&self) -> &'static str {
        "Find places and addresses with OpenStreetMap (Nominatim): name, address, coordinates and type."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let q = match a.near.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(near) => format!("{}, {near}", a.query.trim()),
            None => a.query.trim().to_owned(),
        };
        if q.is_empty() {
            return Err(ToolError::new("query is empty"));
        }
        let limit = a.limit.unwrap_or(5).clamp(1, 10).to_string();
        let mut query: Vec<(&str, String)> = vec![
            ("q", q.clone()),
            ("format", "jsonv2".into()),
            ("limit", limit),
            ("accept-language", "it,en".into()),
        ];
        if let Some(email) = &ctx.env.nominatim_email {
            query.push(("email", email.clone()));
        }
        ctx.env.nominatim_gate.wait().await;
        let url = format!(
            "{}/search",
            ctx.env.endpoints.nominatim.trim_end_matches('/')
        );
        let resp = ctx
            .env
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, &ctx.env.user_agent)
            .query(&query)
            .send()
            .await
            .map_err(|e| ToolError::new(format!("nominatim unreachable: {}", e.without_url())))?;
        if !resp.status().is_success() {
            return Err(ToolError::new(format!(
                "nominatim HTTP {}",
                resp.status().as_u16()
            )));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::new(format!("nominatim response: {e}")))?;
        let results: Vec<Value> = v
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                json!({
                    "name": r["name"],
                    "address": r["display_name"],
                    "latitude": r["lat"].as_str().and_then(|s| s.parse::<f64>().ok()),
                    "longitude": r["lon"].as_str().and_then(|s| s.parse::<f64>().ok()),
                    "category": r["category"],
                    "type": r["type"],
                })
            })
            .collect();
        Ok(
            json!({"query": q, "results": results, "source": "OpenStreetMap Nominatim (© OpenStreetMap contributors)"}),
        )
    }
}
