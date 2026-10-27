//! The `/v1/notes*` HTTP routes (docs/server-api.md, "Notes").

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use secretary_core::models::CreateReminder;
use secretary_core::Call;
use serde::Deserialize;
use serde_json::{json, Value};

use super::attachments::{mime_for_ext, split_file_name};
use super::model::*;
use super::store::{same_instant, Caller};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

fn client_label(headers: &HeaderMap) -> &'static str {
    match headers
        .get(crate::routes::CLIENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        Some("web") => "web",
        Some("ios") => "ios",
        _ => "device",
    }
}

/// A device (Bearer, or cookie + CSRF for writes) or the service token.
async fn device_or_service(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if crate::service::service_bearer(req.headers()).is_some() {
        req.extensions_mut().insert(Caller("lucina"));
        return crate::service::require_service(State(st), req, next).await;
    }
    let label = client_label(req.headers());
    req.extensions_mut().insert(Caller(label));
    crate::auth::require_device(State(st), req, next).await
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("notes task failed"))
}

fn log_write(what: &str, n: &Note, caller: Caller, source: Option<&str>) {
    tracing::info!(
        id = %n.id,
        slug = %n.slug,
        bytes = n.content.len(),
        source = %source.unwrap_or(&n.source),
        by = caller.0,
        "note {what}"
    );
}

// ---------------------------------------------------------------- Lucina reminders

/// Make a Lucina reminder (the same upstream call as `POST /v1/reminders`).
async fn lucina_create(st: &AppState, text: String, at: &str) -> Result<String, ApiError> {
    let req = CreateReminder::Explicit {
        text,
        due_at: at.to_owned(),
    };
    let v: Value = st.lucina()?.call(&Call::create_reminder(&req)?).await?;
    v.pointer("/reminder/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "lucina_error",
                "Lucina answered a reminder without an id",
            )
        })
}

/// Cancel a Lucina reminder. One that is gone or already sent is fine.
async fn lucina_cancel(st: &AppState, id: &str) -> Result<(), ApiError> {
    match st
        .lucina()?
        .call::<Value>(&Call::cancel_reminder(id)?)
        .await
    {
        Ok(_) => Ok(()),
        Err(secretary_core::Error::Api { status, code, .. })
            if (status == 404 && code == "reminder_not_found")
                || (status == 409 && code == "already_sent") =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Cancel reminders of notes that go to the trash; a Lucina that cannot be
/// reached must not keep the owner from deleting a note.
async fn cancel_best_effort(st: &AppState, ids: Vec<String>) {
    for id in ids {
        if let Err(e) = lucina_cancel(st, &id).await {
            tracing::warn!(reminder = %id, error = %e.code, "could not cancel the Lucina reminder of a trashed note");
        }
    }
}

// ---------------------------------------------------------------- handlers

async fn list(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    if q.q.as_deref().is_some_and(|s| s.chars().count() > 200) {
        return Err(bad_request("q: at most 200 characters"));
    }
    let filter = Filter::from_query(&q)?;
    let store = st.notes.clone();
    let notes = blocking(move || store.list_filtered(&filter, limit)).await?;
    Ok(Json(json!({ "notes": notes })))
}

async fn create(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(mut req): JsonBody<CreateNote>,
) -> Result<(StatusCode, Json<Note>), ApiError> {
    let mut armed: Option<String> = None;
    if req.reminder.is_some() {
        // Validate everything first, so Lucina is only asked for a note that
        // will be saved.
        let (store, probe) = (st.notes.clone(), req.clone());
        let cand = blocking(move || store.create_dry(probe, caller)).await??;
        if let Some(r) = &cand.reminder {
            let id = lucina_create(&st, reminder_text(&cand), &r.at).await?;
            req.lucina_id = Some(id.clone());
            armed = Some(id);
        }
    }
    let store = st.notes.clone();
    match blocking(move || store.create(req, caller)).await? {
        Ok(n) => {
            log_write("created", &n, caller, None);
            Ok((StatusCode::CREATED, Json(n)))
        }
        Err(e) => {
            if let Some(id) = armed {
                cancel_best_effort(&st, vec![id]).await;
            }
            Err(e)
        }
    }
}

async fn get_note(
    State(st): State<AppState>,
    UrlPath(key): UrlPath<String>,
) -> Result<Json<Note>, ApiError> {
    let store = st.notes.clone();
    Ok(Json(blocking(move || store.get(&key)).await??))
}

async fn patch_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
    JsonBody(mut req): JsonBody<UpdateNote>,
) -> Result<Json<Note>, ApiError> {
    let source = req.source.clone();
    if let Some(s) = source.as_deref() {
        check_source(Some(s), "owner")?;
    }
    let mut armed: Option<String> = None;
    if let Some(change) = req.reminder.clone() {
        // Validate first (conflict, bad fields) so Lucina is only asked for a
        // change that will be saved; the note stays unchanged when she fails.
        let (store, k, probe) = (st.notes.clone(), key.clone(), req.clone());
        let (cand, current) = blocking(move || {
            let cand = store.update_dry(&k, probe)?;
            store.get(&k).map(|cur| (cand, cur))
        })
        .await??;
        let old = current.reminder.as_ref();
        let old_id = old.and_then(|o| o.lucina_id.clone());
        let mut cancel: Option<String> = None;
        match change {
            None => cancel = old_id,
            Some(r) => {
                let armed_same =
                    old.is_some_and(|o| same_instant(&o.at, &r.at) && o.lucina_id.is_some());
                if !armed_same {
                    let id = lucina_create(&st, reminder_text(&cand), r.at.trim()).await?;
                    req.lucina_id = Some(id.clone());
                    armed = Some(id);
                    cancel = old_id;
                }
            }
        }
        if let Some(old_id) = cancel {
            if let Err(e) = lucina_cancel(&st, &old_id).await {
                if let Some(id) = armed {
                    cancel_best_effort(&st, vec![id]).await;
                }
                return Err(e);
            }
        }
    }
    let store = st.notes.clone();
    match blocking(move || store.update(&key, req)).await? {
        Ok(n) => {
            log_write("updated", &n, caller, source.as_deref());
            Ok(Json(n))
        }
        Err(e) => {
            if let Some(id) = armed {
                cancel_best_effort(&st, vec![id]).await;
            }
            Err(e)
        }
    }
}

async fn append_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
    JsonBody(req): JsonBody<AppendNote>,
) -> Result<Json<Note>, ApiError> {
    let source = req.source.clone();
    if let Some(s) = source.as_deref() {
        check_source(Some(s), "owner")?;
    }
    let store = st.notes.clone();
    let n = blocking(move || store.append(&key, req)).await??;
    log_write("appended", &n, caller, source.as_deref());
    Ok(Json(n))
}

async fn delete_note(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let k = key.clone();
    let ids = blocking(move || store.lucina_ids(&[k])).await?;
    cancel_best_effort(&st, ids).await;
    let store = st.notes.clone();
    let n = blocking(move || store.delete(&key)).await??;
    log_write("moved to the trash", &n, caller, None);
    Ok(Json(json!({
        "deleted": true,
        "id": n.id,
        "slug": n.slug,
        "deletedAt": n.deleted_at,
        "purgeAfter": n.deleted_at.as_deref().and_then(purge_after),
    })))
}

async fn trash(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let notes = blocking(move || store.trash()).await?;
    Ok(Json(json!({ "notes": notes, "days": TRASH_DAYS })))
}

async fn restore(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Note>, ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.restore(&id)).await??;
    log_write("restored", &n, caller, None);
    Ok(Json(n))
}

async fn delete_forever(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let i = id.clone();
    blocking(move || store.delete_forever(&i)).await??;
    tracing::info!(id = %id, by = caller.0, "note deleted forever");
    Ok(Json(json!({"ok": true, "removed": 1})))
}

async fn empty_trash(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.empty_trash()).await?;
    tracing::info!(removed = n, by = caller.0, "trash emptied");
    Ok(Json(json!({"ok": true, "removed": n})))
}

// ---------------------------------------------------------------- batch, sync

async fn batch(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(req): JsonBody<BatchReq>,
) -> Result<Json<Value>, ApiError> {
    if req.trash {
        let (store, ids) = (st.notes.clone(), req.ids.clone());
        let ids = blocking(move || store.lucina_ids(&ids)).await?;
        cancel_best_effort(&st, ids).await;
    }
    let store = st.notes.clone();
    let asked = req.ids.len();
    let out = blocking(move || {
        let r = store.batch(&req);
        (r, req)
    })
    .await?;
    let (out, req) = (out.0?, out.1);
    tracing::info!(
        asked,
        changed = out.notes.len(),
        removed = out.removed.len(),
        trash = req.trash,
        restore = req.restore,
        delete_forever = req.delete_forever,
        by = caller.0,
        "notes batch"
    );
    let mut body = json!({"notes": out.notes});
    if !out.removed.is_empty() {
        body["removed"] = json!(out.removed);
    }
    Ok(Json(body))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesQuery {
    since: Option<String>,
    limit: Option<usize>,
}

async fn changes(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<ChangesQuery>,
) -> Result<Json<Value>, ApiError> {
    let since = match q.since.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => 0,
        Some(s) => s
            .parse::<u64>()
            .map_err(|_| bad_request("since: a cursor from a previous answer, or 0"))?,
    };
    let limit = q
        .limit
        .unwrap_or(DEFAULT_CHANGES_LIMIT)
        .clamp(1, MAX_CHANGES_LIMIT);
    let store = st.notes.clone();
    let c = blocking(move || store.changes(since, limit)).await??;
    Ok(Json(json!({
        "notes": c.notes,
        "removed": c.removed,
        "cursor": c.cursor,
        "hasMore": c.has_more,
    })))
}

// ---------------------------------------------------------------- labels

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelBody {
    name: String,
}

async fn labels(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let labels = blocking(move || store.labels()).await?;
    Ok(Json(json!({ "labels": labels })))
}

async fn create_label(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(b): JsonBody<LabelBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let store = st.notes.clone();
    let l = blocking(move || store.create_label(&b.name)).await??;
    tracing::info!(tag = %l.tag, by = caller.0, "label created");
    Ok((StatusCode::CREATED, Json(json!(l))))
}

async fn rename_label(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(tag): UrlPath<String>,
    JsonBody(b): JsonBody<LabelBody>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let l = blocking(move || store.rename_label(&tag, &b.name)).await??;
    tracing::info!(tag = %l.tag, by = caller.0, "label renamed");
    Ok(Json(json!(l)))
}

async fn delete_label(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(tag): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.notes.clone();
    let t = tag.clone();
    let n = blocking(move || store.delete_label(&t)).await??;
    tracing::info!(tag = %tag, notes = n, by = caller.0, "label deleted");
    Ok(Json(json!({"ok": true, "removed": n})))
}

// ---------------------------------------------------------------- attachments

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    name: Option<String>,
}

async fn upload_attachment(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(key): UrlPath<String>,
    QueryArgs(q): QueryArgs<UploadQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Note>), ApiError> {
    let body = body?;
    let mimetype = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let size = body.len();
    let store = st.notes.clone();
    let n =
        blocking(move || store.add_attachment(&key, &body, &mimetype, q.name.as_deref())).await??;
    tracing::info!(id = %n.id, slug = %n.slug, bytes = size, by = caller.0, "note attachment added");
    Ok((StatusCode::CREATED, Json(n)))
}

async fn remove_attachment(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath((key, att)): UrlPath<(String, String)>,
) -> Result<Json<Note>, ApiError> {
    let store = st.notes.clone();
    let n = blocking(move || store.remove_attachment(&key, &att)).await??;
    tracing::info!(id = %n.id, slug = %n.slug, by = caller.0, "note attachment removed");
    Ok(Json(n))
}

fn attachment_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "attachment_not_found",
        "no such attachment",
    )
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` over `len` bytes → `(first, last)`.
/// `Err` = not satisfiable. A malformed header or several ranges: the whole file.
fn parse_range(h: &str, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Ok(None);
    };
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        let Ok(n) = b.parse::<u64>() else {
            return Ok(None);
        };
        if n == 0 || len == 0 {
            return Err(());
        }
        return Ok(Some((len.saturating_sub(n), len - 1)));
    }
    let Ok(first) = a.parse::<u64>() else {
        return Ok(None);
    };
    let last = if b.is_empty() {
        len.saturating_sub(1)
    } else {
        match b.parse::<u64>() {
            Ok(l) => l.min(len.saturating_sub(1)),
            Err(_) => return Ok(None),
        }
    };
    if first >= len || first > last {
        return Err(());
    }
    Ok(Some((first, last)))
}

async fn get_attachment(
    State(st): State<AppState>,
    UrlPath(file): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Some((sha, ext)) = split_file_name(&file) else {
        return Err(attachment_not_found());
    };
    let (sha, ext) = (sha.to_owned(), ext.to_owned());
    let store = st.notes.clone();
    let f = file.clone();
    let path = blocking(move || store.attachment_path(&f))
        .await?
        .ok_or_else(attachment_not_found)?;
    let etag = format!("\"{sha}\"");
    let cache = "private, max-age=31536000, immutable";
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == etag || t == "*")
        });
    let base = |status: StatusCode| {
        let mut r = Response::builder()
            .status(status)
            .header(header::ETAG, etag.as_str())
            .header(header::CACHE_CONTROL, cache)
            .header(header::ACCEPT_RANGES, "bytes")
            .header("x-content-type-options", "nosniff");
        if let Some(m) = mime_for_ext(&ext) {
            r = r.header(header::CONTENT_TYPE, m);
        }
        r
    };
    if not_modified {
        return base(StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .map_err(|_| ApiError::internal("response"));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| attachment_not_found())?;
    let len = bytes.len() as u64;
    let range = match headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|h| parse_range(h, len))
    {
        None | Some(Ok(None)) => None,
        Some(Ok(Some(r))) => Some(r),
        Some(Err(())) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(axum::body::Body::empty())
                .map_err(|_| ApiError::internal("response"));
        }
    };
    let resp = match range {
        Some((a, b)) => base(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{len}"))
            .body(axum::body::Body::from(
                bytes[a as usize..=b as usize].to_vec(),
            )),
        None => base(StatusCode::OK).body(axum::body::Body::from(bytes)),
    };
    resp.map_err(|_| ApiError::internal("response"))
}

// ---------------------------------------------------------------- router

/// The notes routes with their own auth (device or service token), a body
/// limit for JSON and a bigger one for attachment uploads.
pub fn routes(state: AppState) -> Router<AppState> {
    let json = Router::new()
        .route("/v1/notes", get(list).post(create))
        .route("/v1/notes/trash", get(trash).delete(empty_trash))
        .route("/v1/notes/trash/{id}", delete(delete_forever))
        .route("/v1/notes/trash/{id}/restore", post(restore))
        .route("/v1/notes/labels", get(labels).post(create_label))
        .route(
            "/v1/notes/labels/{tag}",
            patch(rename_label).delete(delete_label),
        )
        .route("/v1/notes/changes", get(changes))
        .route("/v1/notes/batch", post(batch))
        .route("/v1/notes/attachments/{file}", get(get_attachment))
        .route(
            "/v1/notes/{key}",
            get(get_note).patch(patch_note).delete(delete_note),
        )
        .route("/v1/notes/{key}/append", post(append_note))
        .route(
            "/v1/notes/{key}/attachments/{att}",
            delete(remove_attachment),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT));
    let upload = Router::new()
        .route("/v1/notes/{key}/attachments", post(upload_attachment))
        .layer(DefaultBodyLimit::max(ATTACHMENT_BODY_LIMIT));
    // The Google Keep import streams a zip of up to 2 GB to disk.
    let import = Router::new()
        .route(
            "/v1/notes/import/google-keep",
            post(crate::keep_import::endpoint::import_google_keep),
        )
        .layer(DefaultBodyLimit::max(
            crate::keep_import::endpoint::MAX_ZIP_BYTES as usize,
        ));
    json.merge(upload)
        .merge(import)
        .route_layer(middleware::from_fn_with_state(state, device_or_service))
}
