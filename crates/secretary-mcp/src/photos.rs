//! `photo_search`, `photo_get`: the owner's photos themselves, from the media library behind p1's Caddy
//! (`/v1/media`, whose forward_auth takes this service's device token). Scope `photos`, which only the owner
//! grants (consent page or `secretary-mcp token|oauth add-scope`): a photo shows far more than a record, so
//! holding `read` or `chat` is not enough. Search touches no album; only photos come back, as a JPEG
//! rendition, never the original with its location data.

use base64::Engine;
use rmcp::model::{ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::tools::{ToolError, Tools};

const MAX_LIST: u32 = 30;
/// A 1024 px JPEG is far below this.
const MAX_PICTURE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Words: a place, an album, a file name.
    #[serde(default)]
    pub query: Option<String>,
    /// From this date: YYYY, YYYY-MM or YYYY-MM-DD.
    #[serde(default)]
    pub from: Option<String>,
    /// Up to this date, same forms.
    #[serde(default)]
    pub to: Option<String>,
    /// How many (1-30, default 10).
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetArgs {
    /// The id photo_search gave (`itm_…`).
    pub item_id: String,
    /// Long side in pixels: 256 or 1024 (default).
    #[serde(default)]
    pub size: Option<u32>,
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| ToolError(format!("Invalid arguments: {e}")))
}

fn valid_id(id: &str) -> bool {
    id.starts_with("itm_")
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// One search hit as a line: no original links, no coordinates.
fn line(item: &Value) -> String {
    let s = |k: &str| item[k].as_str().unwrap_or("");
    let when = item["taken_at"].as_str().or(item["created_at"].as_str()).unwrap_or("unknown date");
    let place = item["place"]["name"].as_str().unwrap_or("");
    let at = if place.is_empty() { String::new() } else { format!(" at {place}") };
    format!("{} · {when}{at} · {}", s("id"), s("title"))
}

impl Tools<'_> {
    fn media_origin(&self) -> &str {
        let cfg = &self.app.cfg;
        cfg.media_url.as_deref().unwrap_or(cfg.public_url.as_str()).trim_end_matches('/')
    }

    async fn media_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
        let (bytes, _) = self
            .app
            .backend
            .media(self.media_origin(), path, query, MAX_PICTURE_BYTES)
            .await
            .map_err(|e| ToolError(e.message))?;
        serde_json::from_slice(&bytes).map_err(|_| ToolError("The media library gave a bad answer.".into()))
    }

    /// `photo_search` and `photo_get`.
    pub async fn photos(&self, name: &str, args: &JsonObject) -> Result<Vec<ContentBlock>, ToolError> {
        if name == "photo_search" {
            let a: SearchArgs = parse(args)?;
            let mut q: Vec<(&str, String)> = vec![
                ("scope", "gallery".to_owned()),
                ("kind", "photo".to_owned()),
                ("limit", a.limit.unwrap_or(10).clamp(1, MAX_LIST).to_string()),
            ];
            for (k, v) in [("q", a.query), ("from", a.from), ("to", a.to)] {
                if let Some(v) = v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
                    q.push((k, v));
                }
            }
            let page = self.media_json("/v1/media/items", &q).await?;
            let lines: Vec<String> = page["items"].as_array().into_iter().flatten().map(line).collect();
            let text = if lines.is_empty() { "No photos match.".to_owned() } else { lines.join("\n") };
            return Ok(vec![ContentBlock::text(text)]);
        }
        let a: GetArgs = parse(args)?;
        let id = a.item_id.trim();
        if !valid_id(id) {
            return Err(ToolError("item_id must be an id from photo_search (itm_…).".into()));
        }
        let item = self.media_json(&format!("/v1/media/items/{id}"), &[]).await?;
        if item["kind"] != "photo" {
            return Err(ToolError("That item is not a photo.".into()));
        }
        let size = if a.size == Some(256) { "256" } else { "1024" };
        let (bytes, mime) = self
            .app
            .backend
            .media(self.media_origin(), &format!("/v1/media/items/{id}/thumb"), &[("size", size.to_owned())], MAX_PICTURE_BYTES)
            .await
            .map_err(|e| ToolError(e.message))?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mime = if mime.starts_with("image/") { mime } else { "image/jpeg".to_owned() };
        tracing::info!(caller = %self.caller.name, item = %id, "photo fetched");
        Ok(vec![ContentBlock::image(data, mime), ContentBlock::text(line(&item))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_and_lines() {
        assert!(valid_id("itm_d452374e2a9c4ad3a48ced0865187bb0"));
        for bad in ["", "itm_", "../../etc/passwd", "itm_a/b", "itm_a?x=1", "abc"] {
            assert!(!valid_id(bad) || bad == "itm_", "{bad}");
        }
        let l = line(&json!({"id": "itm_1", "title": "IMG_1.HEIC", "taken_at": "2026-11-02T10:00:00Z",
            "place": {"name": "Aarhus", "lat": 56.1, "lon": 10.2}, "urls": {"original": "/x"}}));
        assert_eq!(l, "itm_1 · 2026-11-02T10:00:00Z at Aarhus · IMG_1.HEIC");
        assert!(!l.contains("56.1") && !l.contains("/x"), "no coordinates, no links");
    }
}
