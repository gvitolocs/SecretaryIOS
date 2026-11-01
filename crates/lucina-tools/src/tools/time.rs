//! `time`: the current date and time in a time zone.

use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// IANA time zone, e.g. "Europe/Copenhagen". Default: the owner's.
    timezone: Option<String>,
}

pub struct TimeTool;

#[async_trait::async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &'static str {
        "time"
    }
    fn description(&self) -> &'static str {
        "Current date, time and weekday in a time zone (default: the owner's)."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let tz: Tz = match a
            .timezone
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(name) => name
                .parse()
                .map_err(|_| ToolError::new(format!("unknown time zone {name:?}")))?,
            None => ctx.env.time_zone,
        };
        let local = ctx.now.with_timezone(&tz);
        Ok(json!({
            "timezone": tz.name(),
            "local": local.format("%Y-%m-%d %H:%M:%S").to_string(),
            "weekday": local.format("%A").to_string(),
            "utcOffset": local.format("%:z").to_string(),
            "iso": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        }))
    }
}
