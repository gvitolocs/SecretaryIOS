//! `avatar_outfit`: Lucina changes her own 3D outfit in the Secretary apps.
//!
//! The choice is saved on secretary-server (`PUT /v1/avatar` with the service
//! token, see SecretaryIOS `docs/server-api.md`, "Avatar wardrobe"); the chat
//! reply then carries the timeline meta `{kind: "avatar_outfit", outfit}`
//! ([`crate::Effect::AvatarOutfit`]) so open apps change her live.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

/// `[a-z0-9][a-z0-9_-]*`, at most 40 bytes (the server's rule).
pub fn valid_outfit_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// The id that means "back to changing by app section".
pub const AUTO: &str = "auto";

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// Wardrobe id to put on (e.g. "uniform", "gym", "chef", "maid", "casual"),
    /// or "auto" to change by app section again. Omit to list the wardrobe.
    outfit: Option<String>,
}

pub struct AvatarOutfitTool;

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    let env = &ctx.env;
    let url = env
        .secretary_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned())
        .filter(|u| !u.is_empty());
    let token = env
        .secretary_service_token_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| t.starts_with("svc_"));
    match (url, token) {
        (Some(u), Some(t)) => Ok((u, t)),
        _ => Err(ToolError::new(
            "the wardrobe is not configured (LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE)",
        )),
    }
}

/// The server's `{error:{message}}`, else the status.
async fn server_error(resp: reqwest::Response) -> ToolError {
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    if code == "unknown_outfit" {
        return ToolError::new(message.to_owned());
    }
    ToolError::new(format!("secretary-server answered {status} {code}"))
}

async fn get_json(ctx: &Ctx, url: &str, token: &str) -> Result<Value, ToolError> {
    let resp = ctx
        .env
        .http
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| ToolError::new(format!("secretary-server unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(server_error(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| ToolError::new(format!("secretary-server: bad JSON: {e}")))
}

#[async_trait::async_trait]
impl Tool for AvatarOutfitTool {
    fn name(&self) -> &'static str {
        "avatar_outfit"
    }
    fn description(&self) -> &'static str {
        "Change Lucina's own 3D outfit in the Secretary apps (web and iPhone). Use it when Giuseppe asks her to \
         put on or change clothes ('mettiti il vestito da cameriera', 'put on your gym clothes', 'tag træningstøjet på') \
         or when she decides to dress for the moment. outfit: a wardrobe id (uniform, gym, chef, maid, casual, ...), or \
         'auto' to change by app section again (gym clothes in the Gym, ...). Without outfit it lists the wardrobe and \
         what she wears. A chosen outfit stays on until changed or set back to auto."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        match args["outfit"].as_str() {
            Some(o) => format!("avatar_outfit {o}"),
            None => "avatar_outfit (list)".into(),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (base, token) = server(ctx)?;
        let Some(outfit) = a.outfit.map(|o| o.trim().to_ascii_lowercase()) else {
            let wardrobe = get_json(ctx, &format!("{base}/v1/avatar/wardrobe"), &token).await?;
            let settings = get_json(ctx, &format!("{base}/v1/avatar"), &token).await?;
            let outfits: Vec<Value> = wardrobe
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|e| json!({"id": e["id"], "name": e["name"], "tags": e["tags"]}))
                        .collect()
                })
                .unwrap_or_default();
            return Ok(json!({"outfits": outfits, "current": settings}));
        };
        let body = if outfit == AUTO {
            json!({"outfit": null, "auto": true})
        } else if valid_outfit_id(&outfit) {
            json!({"outfit": outfit})
        } else {
            return Err(ToolError::new(
                "outfit must be a wardrobe id ([a-z0-9][a-z0-9_-]*) or \"auto\"",
            ));
        };
        let resp = ctx
            .env
            .http
            .put(format!("{base}/v1/avatar"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::new(format!("secretary-server unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(server_error(resp).await);
        }
        let settings: Value = resp.json().await.unwrap_or(Value::Null);
        let worn = (outfit != AUTO).then_some(outfit);
        ctx.push_effect(Effect::AvatarOutfit {
            outfit: worn.clone(),
        });
        Ok(json!({"ok": true, "outfit": worn, "auto": worn.is_none(), "settings": settings}))
    }
}

#[cfg(test)]
mod tests {
    use super::valid_outfit_id;

    #[test]
    fn ids() {
        assert!(valid_outfit_id("maid"));
        assert!(valid_outfit_id("gym-2"));
        for bad in ["", "Maid", "-x", "a b", "../x", &"a".repeat(41)] {
            assert!(!valid_outfit_id(bad), "{bad:?}");
        }
    }
}
