//! `presence`: is the owner online on Discord, what is he doing, and how long
//! has he been on today. Reads the presence watcher's files
//! (`<data>/presence/discord.json{,l}`); never contacts Discord.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolResult};
use lucina_discord::store::{today_events, PresenceReport};

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// Also return today's transitions (status changes, activities started
    /// and stopped), newest last.
    #[serde(default)]
    history: bool,
}

pub struct PresenceTool;

#[async_trait::async_trait]
impl Tool for PresenceTool {
    fn name(&self) -> &'static str {
        "presence"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's Discord presence: online/idle/dnd/offline, on desktop/mobile/web, what he is doing \
         (game, Spotify song and artist, custom status, with start times) and his online minutes today. \
         Use for 'is Giuseppe online', 'what is he doing / listening to', 'how long has he been on Discord today'. \
         Invisible looks exactly like offline; never claim to detect it. fresh=false means the watcher is down \
         and the state is unknown."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let env = &ctx.env;
        let report = PresenceReport::build(
            &env.presence_dir,
            env.time_zone,
            ctx.now,
            env.presence_max_age,
        );
        let mut out = serde_json::to_value(&report).unwrap_or_else(|_| json!({}));
        out["timeZone"] = json!(env.time_zone.name());
        if a.history {
            let events = today_events(&env.presence_dir, env.time_zone, ctx.now, 50);
            out["todayHistory"] = serde_json::to_value(events).unwrap_or_default();
        }
        Ok(out)
    }
}
