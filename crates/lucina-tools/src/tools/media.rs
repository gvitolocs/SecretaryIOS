//! `media`: the owner's photos, videos and audio (lucina-media on nezopt, SecretaryIOS
//! `docs/media-library.md`; the Gallery tab in the app).
//!
//! The library listens on nezopt's tailnet address and wants the upstream key that p1's Caddy
//! sends (`LUCINA_MEDIA_URL`, default `http://100.107.164.33:8830`; `LUCINA_MEDIA_KEY_FILE`,
//! default `~/.config/secretary-media/upstream.key`). A search the owner should see fills the
//! system album "Lucina's picks", and the reply gets a button that opens it in the Gallery.

use std::path::PathBuf;

use chrono::DateTime;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

const PICKS_ALBUM: &str = "sys-lucina";
const MAX_LIST: usize = 30;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Search,
    Albums,
    AddToAlbum,
    CreateAlbum,
    Summary,
}

#[derive(Deserialize, JsonSchema)]
struct Near {
    lat: f64,
    lon: f64,
    /// Radius in km (default 25).
    km: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// Words to match: place names, country, month ("July 2025"), album, title, caption.
    query: Option<String>,
    /// Start date: YYYY, YYYY-MM, YYYY-MM-DD or RFC 3339 (search).
    from: Option<String>,
    /// End date, inclusive: YYYY, YYYY-MM, YYYY-MM-DD (search).
    to: Option<String>,
    /// Around a place (search). Give coordinates you know, e.g. Copenhagen 55.676,12.568.
    near: Option<Near>,
    /// photo, video, audio or file (search).
    kind: Option<String>,
    /// Album id or title (search: only this album; add_to_album: the album).
    album: Option<String>,
    /// Item ids (add_to_album, create_album).
    item_ids: Option<Vec<String>>,
    /// New album title (create_album).
    title: Option<String>,
    /// search: put the results in "Lucina's picks" and add a Gallery button to the reply (default true).
    show: Option<bool>,
    /// Max results (search, default 30).
    limit: Option<u32>,
}

fn base() -> String {
    std::env::var("LUCINA_MEDIA_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "http://100.107.164.33:8830".into())
        .trim_end_matches('/')
        .to_owned()
}

fn key() -> Result<String, ToolError> {
    let path = std::env::var("LUCINA_MEDIA_KEY_FILE")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into()))
                .join(".config/secretary-media/upstream.key")
        });
    std::fs::read_to_string(&path)
        .map(|k| k.trim().to_owned())
        .ok()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            ToolError::new("the media library is not configured (LUCINA_MEDIA_KEY_FILE)")
        })
}

async fn send(req: reqwest::RequestBuilder) -> Result<Value, ToolError> {
    let resp = req
        .header("X-Media-Upstream-Key", key()?)
        .send()
        .await
        .map_err(|e| {
            ToolError::new(format!(
                "the media library is unreachable: {}",
                e.without_url()
            ))
        })?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body["error"].as_str().unwrap_or("request failed");
        return Err(ToolError::new(format!(
            "media library: {msg} (HTTP {})",
            status.as_u16()
        )));
    }
    Ok(body)
}

/// Compact item for the model: no URLs, local dates.
fn brief(item: &Value, tz: chrono_tz::Tz) -> Value {
    let when = item["taken_at"]
        .as_str()
        .or(item["created_at"].as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string());
    let place = item["place"].as_object().map(|p| {
        [p.get("name"), p.get("admin1"), p.get("country")]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let mut out = json!({ "id": item["id"], "kind": item["kind"], "title": item["title"] });
    if let Some(w) = when {
        out["when"] = json!(w);
    }
    if let Some(p) = place.filter(|p| !p.is_empty()) {
        out["place"] = json!(p);
    }
    if let Some(a) = item["album"].as_str() {
        out["album"] = json!(a);
    }
    if let Some(d) = item["duration_ms"].as_i64() {
        out["duration_s"] = json!(d / 1000);
    }
    if item["favorite"] == true {
        out["favorite"] = json!(true);
    }
    out
}

pub struct MediaTool;

#[async_trait::async_trait]
impl Tool for MediaTool {
    fn name(&self) -> &'static str {
        "media"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's own photo, video and audio library (his server; the app's Gallery tab, where his iPhone backs up). \
         search: by date (from/to: YYYY, YYYY-MM, YYYY-MM-DD), place (near: coordinates + km; you know the coordinates of \
         cities), words (query: place names, country, \"July 2025\", album, title) and kind (photo/video/audio). \
         \"Photos from Copenhagen last summer\" → near {lat:55.676, lon:12.568, km:30}, from 2026-06, to 2026-08, kind photo. \
         Results go into the album \"Lucina's picks\" and the reply gets a button that shows them; say what you found \
         (how many, when, where) rather than listing ids. \
         albums: list albums. create_album / add_to_album: put item ids from a search into an album (by id or title). \
         summary: counts by kind and year, top places."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
        match s("action") {
            "search" => format!("media search {} {}-{}", s("query"), s("from"), s("to")),
            "add_to_album" => format!("media: add to album {}", s("album")),
            "create_album" => format!("media: new album \"{}\"", s("title")),
            other => format!("media {other}"),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let http = &ctx.env.http;
        let b = base();
        match a.action {
            Action::Search => {
                let limit = a.limit.unwrap_or(MAX_LIST as u32).clamp(1, 200);
                let mut q: Vec<(&str, String)> = vec![("limit", limit.to_string())];
                if let Some(v) = a.query.as_deref().filter(|v| !v.trim().is_empty()) {
                    q.push(("q", v.to_owned()));
                }
                if let Some(v) = &a.from {
                    q.push(("from", v.clone()));
                }
                if let Some(v) = &a.to {
                    q.push(("to", v.clone()));
                }
                if let Some(n) = &a.near {
                    q.push((
                        "near",
                        format!("{},{},{}", n.lat, n.lon, n.km.unwrap_or(25.0)),
                    ));
                }
                if let Some(v) = &a.kind {
                    q.push(("kind", v.clone()));
                }
                if let Some(v) = &a.album {
                    q.push(("album", v.clone()));
                }
                let page = send(http.get(format!("{b}/v1/media/items")).query(&q)).await?;
                let items = page["items"].as_array().cloned().unwrap_or_default();
                let ids: Vec<Value> = items.iter().map(|i| i["id"].clone()).collect();
                let shown = a.show.unwrap_or(true) && !items.is_empty();
                if shown {
                    send(
                        http.put(format!("{b}/v1/media/albums/{PICKS_ALBUM}/items"))
                            .json(&json!({ "item_ids": ids })),
                    )
                    .await?;
                    ctx.push_effect(Effect::Gallery {
                        path: format!("/gallery/album/{PICKS_ALBUM}"),
                        count: items.len(),
                    });
                }
                let tz = ctx.env.time_zone;
                Ok(json!({
                    "count": items.len(),
                    "more": page["next_cursor"].is_string(),
                    "items": items.iter().take(MAX_LIST).map(|i| brief(i, tz)).collect::<Vec<_>>(),
                    "shown_in_gallery": shown,
                }))
            }
            Action::Albums => {
                let v = send(http.get(format!("{b}/v1/media/albums"))).await?;
                let albums: Vec<Value> = v["albums"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|a| a["count"].as_i64().unwrap_or(0) > 0 || a["system_key"].is_null())
                    .map(|a| json!({ "id": a["id"], "title": a["title"], "count": a["count"] }))
                    .collect();
                Ok(json!({ "albums": albums }))
            }
            Action::CreateAlbum => {
                let title = a
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| ToolError::new("title is required"))?;
                let album =
                    send(http.post(format!("{b}/v1/media/albums")).json(
                        &json!({ "title": title, "item_ids": a.item_ids.unwrap_or_default() }),
                    ))
                    .await?;
                ctx.push_effect(Effect::Gallery {
                    path: format!("/gallery/album/{}", album["id"].as_str().unwrap_or("")),
                    count: album["count"].as_u64().unwrap_or(0) as usize,
                });
                Ok(
                    json!({ "album": { "id": album["id"], "title": album["title"], "count": album["count"] } }),
                )
            }
            Action::AddToAlbum => {
                let wanted = a
                    .album
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| ToolError::new("album is required"))?;
                let ids = a.item_ids.unwrap_or_default();
                if ids.is_empty() {
                    return Err(ToolError::new("item_ids is required (ids from a search)"));
                }
                let list = send(http.get(format!("{b}/v1/media/albums"))).await?;
                let found = list["albums"].as_array().into_iter().flatten().find(|x| {
                    x["id"].as_str() == Some(wanted)
                        || x["title"]
                            .as_str()
                            .is_some_and(|t| t.eq_ignore_ascii_case(wanted))
                });
                let id = match found {
                    Some(x) => x["id"].as_str().unwrap_or_default().to_owned(),
                    None => {
                        let created = send(
                            http.post(format!("{b}/v1/media/albums"))
                                .json(&json!({ "title": wanted, "item_ids": [] })),
                        )
                        .await?;
                        created["id"].as_str().unwrap_or_default().to_owned()
                    }
                };
                let r = send(
                    http.post(format!("{b}/v1/media/albums/{id}/items"))
                        .json(&json!({ "item_ids": ids })),
                )
                .await?;
                ctx.push_effect(Effect::Gallery {
                    path: format!("/gallery/album/{id}"),
                    count: r["added"].as_u64().unwrap_or(0) as usize,
                });
                Ok(json!({ "album_id": id, "added": r["added"] }))
            }
            Action::Summary => {
                let v = send(http.get(format!("{b}/v1/media/summary"))).await?;
                Ok(
                    json!({ "kinds": v["kinds"], "years": v["years"], "places": v["places"], "last_added_at": v["last_added_at"] }),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_drops_urls_and_formats_local_time() {
        let item = json!({"id": "itm_1", "kind": "photo", "title": "IMG_1.HEIC", "taken_at": "2025-07-12T12:03:22.000Z",
            "place": {"name": "Copenhagen", "admin1": "Capital Region", "country": "DK", "lat": 55.6, "lon": 12.5},
            "urls": {"original": "/v1/media/items/itm_1/original"}, "favorite": true});
        let b = brief(&item, chrono_tz::Europe::Copenhagen);
        assert_eq!(b["when"], "2025-07-12 14:03");
        assert_eq!(b["place"], "Copenhagen, Capital Region, DK");
        assert!(b.get("urls").is_none());
        assert_eq!(b["favorite"], true);
    }

    #[test]
    fn schema_is_an_object() {
        assert_eq!(MediaTool.parameters()["type"], "object");
        assert!(!crate::is_denied_name(MediaTool.name()));
    }
}
