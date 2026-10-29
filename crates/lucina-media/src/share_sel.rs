//! Share chosen photos (not only an album): `POST shares {item_ids, title?, expires_in_days?, allow_original?}`.
//!
//! The link is an ordinary album share (`share.rs`, `share_http.rs`, the verified `decide` rule, unchanged)
//! over a hidden system album `share:<token>` that holds the chosen items. That album has the
//! `system_key` `share:<token>`, so it never shows in an album list or an item's albums, is not counted,
//! is not found by title, and no client route can add to it, rename it or delete it
//! (`db::not_share_album`, `db::get_album`). The public page serves it like any album: an item that is
//! trashed later is no longer served, and revoking the link (`DELETE shares/{token}`, the same route as
//! for albums) ends access at once. `GET shares` lists both kinds, with `kind` telling them apart.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, now_str};
use crate::http::{ApiError, ApiResult};
use crate::ingest::Library;
use crate::share::{self, ShareRow};
use crate::share_http::{expiry_after, share_json};

/// Photos and videos in one link.
pub const MAX_ITEMS: usize = 500;
/// The title a link gets when the owner gives none.
pub const DEFAULT_TITLE: &str = "Shared photos";
const MAX_TITLE: usize = 200;

pub fn routes() -> Router<Library> {
    Router::new().route("/v1/media/shares", post(create))
}

#[derive(Deserialize)]
struct Req {
    #[serde(default)]
    item_ids: Vec<String>,
    title: Option<String>,
    expires_in_days: Option<u32>,
    allow_original: Option<bool>,
}

fn bad(msg: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

/// The ids to share, in the order given and without repeats; the request must name 1 to [`MAX_ITEMS`].
pub fn clean_ids(ids: &[String]) -> Result<Vec<String>, &'static str> {
    if ids.is_empty() {
        return Err("item_ids must name at least one item");
    }
    if ids.len() > MAX_ITEMS {
        return Err("at most 500 items in one link");
    }
    let mut seen = HashSet::new();
    Ok(ids
        .iter()
        .filter(|i| !i.is_empty() && i.len() <= 128 && seen.insert(i.as_str()))
        .cloned()
        .collect())
}

/// The album title for a link: the owner's, trimmed (1 to 200 characters), or the default.
pub fn clean_title(title: Option<&str>) -> Result<String, &'static str> {
    match title.map(str::trim) {
        None | Some("") => Ok(DEFAULT_TITLE.into()),
        Some(t) if t.chars().count() > MAX_TITLE => Err("title must be at most 200 characters"),
        Some(t) => Ok(t.into()),
    }
}

async fn create(
    State(lib): State<Library>,
    Json(r): Json<Req>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ids = clean_ids(&r.item_ids).map_err(bad)?;
    let title = clean_title(r.title.as_deref()).map_err(bad)?;
    let token = share::new_token()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let now = chrono::Utc::now();
    let row = ShareRow {
        token: token.clone(),
        album_id: format!("{}{token}", db::SHARE_ALBUM_PREFIX),
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        expires_at: expiry_after(now, r.expires_in_days),
        revoked_at: None,
        allow_original: r.allow_original.unwrap_or(false),
        views: 0,
        last_view_at: None,
    };
    let (saved, l) = (row.clone(), lib.clone());
    let made = lib
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            // Only photos and videos that exist and are in no trash; the rest are reported, not shared.
            let mut shared = Vec::new();
            let mut skipped = Vec::new();
            for id in &ids {
                let ok = tx
                    .query_row(
                        "SELECT 1 FROM items WHERE id = ?1 AND kind IN ('photo', 'video') \
                         AND trashed_at IS NULL AND hidden = 0",
                        [id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if ok {
                    shared.push(id.clone());
                } else {
                    skipped.push(id.clone());
                }
            }
            if shared.is_empty() {
                return Ok(Err(skipped));
            }
            let now = now_str();
            tx.execute(
                "INSERT INTO albums (id, title, system_key, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![saved.album_id, title, saved.album_id, now],
            )?;
            for id in &shared {
                db::add_to_album(&tx, &saved.album_id, id)?;
            }
            db::insert_share(&tx, &saved)?;
            let mut out = share_json(&l, &tx, &saved)?;
            tx.commit()?;
            out["skipped"] = json!(skipped);
            Ok(Ok(out))
        })
        .await?;
    match made {
        Ok(v) => Ok((StatusCode::CREATED, Json(v))),
        Err(skipped) => Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "none of the {} items can be shared (missing, in the Trash or not a photo or video)",
                skipped.len()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ids_are_counted_and_deduplicated_in_order() {
        assert!(clean_ids(&[]).is_err());
        assert_eq!(clean_ids(&v(&["b", "a", "b", "", "c"])).unwrap(), v(&["b", "a", "c"]));
        let many: Vec<String> = (0..501).map(|i| format!("itm_{i}")).collect();
        assert!(clean_ids(&many).is_err());
        assert_eq!(clean_ids(&many[..500]).unwrap().len(), 500);
    }

    #[test]
    fn titles_default_trim_and_are_bounded() {
        assert_eq!(clean_title(None).unwrap(), DEFAULT_TITLE);
        assert_eq!(clean_title(Some("   ")).unwrap(), DEFAULT_TITLE);
        assert_eq!(clean_title(Some("  Trip ")).unwrap(), "Trip");
        assert!(clean_title(Some(&"x".repeat(201))).is_err());
        assert!(clean_title(Some(&"x".repeat(200))).is_ok());
    }
}
