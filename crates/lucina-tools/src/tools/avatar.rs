//! `avatar_outfit`: Lucina changes her own 3D outfit and hairstyle in the Secretary apps.
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
    /// Wardrobe outfit to put on (e.g. "uniform", "gym", "chef", "maid", "casual"),
    /// or "auto" to change outfit and hair by app section again.
    outfit: Option<String>,
    /// Hairstyle (e.g. "ponytail", "bun", "twintails", "braid"), or "natural" for her own hair.
    hair: Option<String>,
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
         or when she decides to dress for the moment. Also her hair ('fatti la coda', 'put your hair in a bun'): \
         hair = a hairstyle id, or 'natural' for her own hair. outfit: a wardrobe id (uniform, gym, chef, maid, casual, ...), \
         or 'auto' to change outfit and hair by app section again (gym clothes and a ponytail in the Gym, ...). Without \
         outfit and hair it lists the wardrobe (outfits, hairstyles, colours) and what she wears. A choice stays until \
         changed or set back to auto."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        match (args["outfit"].as_str(), args["hair"].as_str()) {
            (Some(o), Some(h)) => format!("avatar_outfit {o}, hair {h}"),
            (Some(o), None) => format!("avatar_outfit {o}"),
            (None, Some(h)) => format!("avatar_outfit hair {h}"),
            (None, None) => "avatar_outfit (list)".into(),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (base, token) = server(ctx)?;
        let norm = |v: Option<String>| {
            v.map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
        };
        let (outfit, hair) = (norm(a.outfit), norm(a.hair));
        if outfit.is_none() && hair.is_none() {
            let wardrobe = get_json(ctx, &format!("{base}/v1/avatar/wardrobe"), &token).await?;
            let settings = get_json(ctx, &format!("{base}/v1/avatar"), &token).await?;
            // `{outfits, hairstyles, colors}`, or the older bare array of outfits.
            let list = |key: &str| -> Vec<Value> {
                let items = if key == "outfits" && wardrobe.is_array() {
                    &wardrobe
                } else {
                    &wardrobe[key]
                };
                items
                    .as_array()
                    .map(|l| {
                        l.iter()
                            .map(|e| json!({"id": e["id"], "name": e["name"], "tags": e["tags"], "hex": e["hex"]}))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            return Ok(json!({
                "outfits": list("outfits"),
                "hairstyles": list("hairstyles"),
                "colors": list("colors"),
                "current": settings,
            }));
        }
        let auto = outfit.as_deref() == Some(AUTO);
        for id in [&outfit, &hair].into_iter().flatten() {
            if !(valid_outfit_id(id) || (auto && id == AUTO)) {
                return Err(ToolError::new(
                    "outfit and hair must be wardrobe ids ([a-z0-9][a-z0-9_-]*); outfit may be \"auto\"",
                ));
            }
        }
        let body = if auto {
            json!({"outfit": null, "hair": null, "auto": true})
        } else {
            let mut b = serde_json::Map::new();
            if let Some(o) = &outfit {
                b.insert("outfit".into(), json!(o));
            }
            if let Some(h) = &hair {
                b.insert("hair".into(), json!(h));
            }
            Value::Object(b)
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
        let (outfit, hair) = if auto { (None, None) } else { (outfit, hair) };
        ctx.push_effect(Effect::AvatarOutfit {
            outfit: outfit.clone(),
            hair: hair.clone(),
        });
        Ok(json!({"ok": true, "outfit": outfit, "hair": hair, "auto": auto, "settings": settings}))
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
