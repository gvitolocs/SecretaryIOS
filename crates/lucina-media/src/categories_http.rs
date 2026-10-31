//! The category routes (behind the upstream key; the share routes know nothing of them):
//!
//! - `GET categories?origin=&kind=…`: the automatic albums, `{ ok, categories: [{ category, count,
//!   cover }] }` in the apps' order, only those with photos, `photo` not counted; `cover` is the
//!   item JSON of the newest photo of the category.
//! - `GET categories/{cat}/items?<listing filters>&cursor&limit`: the Gallery's listing limited to
//!   one category, in the same page shape as `GET items`. `photo` lists what no other category
//!   claims.
//! - `PUT items/{id}/category {category: "receipt" | null}`: the owner's label (never overwritten
//!   by a decider); `null` takes it back and the photo is decided automatically again. Answers
//!   `{ ok, category: {category, confidence, decided_by, auto_category} | null }`.
//! - `GET categories/status`: `{ enabled, model, features_version, decider: {kind, id}, decided,
//!   pending, stale, by_decider: {rules, jev, owner}, running, paused_hot, paused_busy }`.
//! - `GET categories/examples?cursor&limit`: the owner's corrections as labelled examples for a
//!   better decider, oldest first: `{ examples: [{ item_id, category, auto_category, features,
//!   model, labelled_at }], next_cursor }`.
//! - `category` (`{category, confidence, decided_by, auto_category}` or `null`) is also part of
//!   `GET items/{id}/details`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use lucina_media_ml::categories::Category;
use lucina_media_ml::categories_store as cs;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::categories;
use crate::db::{self, ListQuery};
use crate::http::{bad, item_out, ApiError, ApiResult};
use crate::ingest::Library;

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/categories", get(list_categories))
        .route("/v1/media/categories/status", get(status))
        .route("/v1/media/categories/examples", get(list_examples))
        .route("/v1/media/categories/{cat}/items", get(category_items))
        .route("/v1/media/items/{id}/category", put(put_category))
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("{what} not found"))
}

fn parse_category(s: &str) -> ApiResult<Category> {
    Category::parse(s).ok_or_else(|| not_found("category"))
}

async fn list_categories(
    State(lib): State<Library>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let q = crate::browse::gallery_query(&lib, q)?;
    let (rows, mut covers) = lib
        .db
        .call(move |c| {
            let rows = categories::counts(c, &q)?;
            let mut covers = std::collections::HashMap::new();
            for (cat, _, cover) in &rows {
                if let Some(id) = cover {
                    if let Some(it) = db::get_item(c, id)? {
                        covers.insert(*cat, it);
                    }
                }
            }
            Ok((rows, covers))
        })
        .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(cat, count, _)| {
            json!({
                "category": cat.as_str(),
                "count": count,
                "cover": covers.remove(&cat).map(|it| item_out(&lib, it, None)),
            })
        })
        .collect();
    Ok(Json(json!({ "ok": true, "categories": out })))
}

async fn category_items(
    State(lib): State<Library>,
    Path(cat): Path<String>,
    Query(mut q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    q.category = Some(parse_category(&cat)?);
    crate::http::list_items(State(lib), Query(q)).await
}

async fn status(State(lib): State<Library>) -> ApiResult<Json<Value>> {
    let l = lib.clone();
    let v = tokio::task::spawn_blocking(move || categories::status_json(&l)).await??;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ExamplesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn list_examples(
    State(lib): State<Library>,
    Query(q): Query<ExamplesQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let after = match q.cursor.as_deref() {
        None => None,
        Some(c) => Some(
            c.split_once('|')
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .ok_or_else(|| bad("bad cursor"))?,
        ),
    };
    let (rows, next) = lib
        .db
        .call(move |c| {
            let a = after.as_ref().map(|(a, b)| (a.as_str(), b.as_str()));
            let mut rows = categories::examples(c, a, limit + 1)?;
            let more = rows.len() > limit;
            rows.truncate(limit);
            let next = more
                .then(|| {
                    rows.last().map(|r| {
                        format!(
                            "{}|{}",
                            r["labelled_at"].as_str().unwrap_or(""),
                            r["item_id"].as_str().unwrap_or("")
                        )
                    })
                })
                .flatten();
            Ok((rows, next))
        })
        .await?;
    Ok(Json(
        json!({ "ok": true, "examples": rows, "next_cursor": next }),
    ))
}

async fn put_category(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    if !crate::http::valid_item_id(&id) {
        return Err(not_found("item"));
    }
    let Some(asked) = body.get("category") else {
        return Err(bad(
            "category is required (a name, or null to go back to automatic)",
        ));
    };
    let wanted = match asked {
        Value::Null => None,
        Value::String(s) => {
            Some(Category::parse(s).ok_or_else(|| bad(format!("unknown category {s:?}")))?)
        }
        _ => return Err(bad("category must be a string or null")),
    };
    // A copy merged into another photo is the other photo's business.
    let iid = id.clone();
    let (id, kind) = lib
        .db
        .call(move |c| {
            let id = crate::dedup_merge::resolve(c, &iid)?;
            let kind: Option<String> = c
                .query_row("SELECT kind FROM items WHERE id = ?1", [&id], |r| r.get(0))
                .ok();
            Ok((id, kind))
        })
        .await?;
    match kind.as_deref() {
        None => return Err(not_found("item")),
        Some("photo") => {}
        Some(_) => return Err(bad("only photos have a category")),
    }
    let l = lib.clone();
    let row = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<cs::Row>> {
        match wanted {
            Some(cat) => {
                // Keep the record the machine decided from; build one if it had not decided yet.
                let has_record =
                    l.db.with(|c| cs::get(c, &id))?
                        .is_some_and(|r| r.features.is_some());
                let features = if has_record {
                    None
                } else {
                    categories::features_now(&l, &id)?
                };
                let now = db::now_str();
                let row =
                    l.db.with(|c| cs::set_owner(c, &id, cat, features.as_ref(), &now))?;
                Ok(Some(row))
            }
            None => {
                l.db.with(|c| cs::clear_owner(c, &id))?;
                categories::decide_now(&l, &id)
            }
        }
    })
    .await??;
    Ok(Json(json!({
        "ok": true,
        "category": row.as_ref().map(categories::row_json),
    })))
}
