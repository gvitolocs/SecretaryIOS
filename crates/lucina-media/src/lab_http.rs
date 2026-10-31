//! The Lab over HTTP, under `/v1/media` behind the upstream key (`docs/media-library.md`, "AI Lab").
//!
//! - `POST items/{id}/lab {prompt, reference_item_id?, steps?, seed?}`: queue an edit (201, the job).
//! - `GET lab[?status=a,b&item_id=…&limit=n]`: jobs, running first, then the queue, then the newest.
//! - `GET lab/{job}`, `GET lab/{job}/result` (the finished picture, PNG), `GET lab/{job}/before` (what the model was given).
//! - `POST lab/{job}/accept`: the result becomes a new item linked to the original (200, job + item).
//! - `POST lab/{job}/discard`: delete the result. `POST lab/{job}/retry`: queue a failed job again.
//! - `DELETE lab/{job}`: cancel a queued job, or remove a failed or discarded one.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, Item};
use crate::http::{item_out, serve_file, ApiError, ApiResult};
use crate::ingest::Library;
use crate::lab::{self, AcceptError, Job};
use crate::lab_run;

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/items/{id}/lab", post(create))
        .route("/v1/media/lab", get(list))
        .route("/v1/media/lab/{job}", get(one).delete(cancel))
        .route("/v1/media/lab/{job}/result", get(result))
        .route("/v1/media/lab/{job}/before", get(before))
        .route("/v1/media/lab/{job}/accept", post(accept))
        .route("/v1/media/lab/{job}/discard", post(discard))
        .route("/v1/media/lab/{job}/retry", post(retry))
}

fn err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(status, msg.into())
}
fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}
fn not_found() -> ApiError {
    err(StatusCode::NOT_FOUND, "not found")
}

fn job_id(raw: &str) -> ApiResult<&str> {
    if lab::id_ok(raw) {
        Ok(raw)
    } else {
        Err(not_found())
    }
}

fn conflict(e: AcceptError) -> ApiError {
    match e {
        AcceptError::NotFound => not_found(),
        AcceptError::Conflict(status) => err(
            StatusCode::CONFLICT,
            format!("the job is {status}, not ready for that"),
        ),
        AcceptError::Failed(m) => {
            tracing::warn!(error = %m, "lab request failed");
            err(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// The picture shown as "before" / "after" and the job's own URLs. Paths are relative to the API root
/// like every other `urls` object.
fn job_json(lib: &Library, job: &Job, original: Option<&Item>, position: Option<i64>) -> Value {
    let thumb = |id: &str, v: i64| format!("/v1/media/items/{id}/thumb?size=1024&v={v}");
    // What the model was given (rotation applied, same size as the result) while its file is kept; else the
    // library's own large preview of the original.
    let input_kept = matches!(job.status.as_str(), "running" | "done" | "failed")
        && lab::job_dir(&lib.cfg, &job.id).join("input.png").is_file();
    let before = if input_kept {
        Some(format!("/v1/media/lab/{}/before", job.id))
    } else {
        original.map(|o| thumb(&o.id, o.version))
    };
    let after = match job.status.as_str() {
        "done" => Some(format!("/v1/media/lab/{}/result", job.id)),
        "accepted" => job.accepted_item_id.as_deref().map(|id| thumb(id, 1)),
        _ => None,
    };
    let reference = job
        .reference_item
        .as_deref()
        .map(|id| format!("/v1/media/items/{id}/thumb?size=256"));
    let original_json = original.map(|o| {
        json!({
            "id": o.id,
            "title": o.title,
            "width": o.width,
            "height": o.height,
            "rotation": o.rotation,
            "thumbhash": o.thumbhash,
            "trashed": o.trashed_at.is_some(),
        })
    });
    json!({
        "id": job.id,
        "item_id": job.item_id,
        "reference_item_id": job.reference_item,
        "prompt": job.prompt,
        "status": job.status,
        "phase": job.phase,
        "progress": job.progress,
        "error": job.error,
        "seed": job.seed,
        "steps": job.steps,
        "model": job.model,
        "attempts": job.attempts,
        "queue_position": position,
        "created_at": job.created_at,
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "accepted_item_id": job.accepted_item_id,
        "result": job.result_hash.as_ref().map(|h| json!({
            "hash": h, "bytes": job.result_bytes, "width": job.result_w, "height": job.result_h,
        })),
        "original": original_json,
        "urls": {
            "before": before,
            "after": after,
            "reference": reference,
            "result": (job.status == "done").then(|| format!("/v1/media/lab/{}/result", job.id)),
        },
    })
}

async fn describe(lib: &Library, jobs: Vec<Job>) -> ApiResult<Vec<Value>> {
    let l2 = lib.clone();
    let rows = lib
        .db
        .call(move |c| {
            jobs.into_iter()
                .map(|j| {
                    let it = db::get_item(c, &j.item_id)?;
                    let pos = lab::queue_position(c, &j)?;
                    Ok((j, it, pos))
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(rows
        .iter()
        .map(|(j, it, pos)| job_json(&l2, j, it.as_ref(), *pos))
        .collect())
}

async fn load(lib: &Library, id: &str) -> ApiResult<Job> {
    let id = job_id(id)?.to_owned();
    lib.db
        .call(move |c| lab::get_job(c, &id))
        .await?
        .ok_or_else(not_found)
}

#[derive(Deserialize)]
struct CreateReq {
    prompt: String,
    #[serde(default)]
    reference_item_id: Option<String>,
    #[serde(default)]
    steps: Option<i64>,
    #[serde(default)]
    seed: Option<i64>,
}

async fn create(
    State(lib): State<Library>,
    Path(id): Path<String>,
    Json(req): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() || prompt.chars().count() > lab::MAX_PROMPT_CHARS {
        return Err(bad(format!(
            "prompt must be 1 to {} characters",
            lab::MAX_PROMPT_CHARS
        )));
    }
    if req.steps.is_some_and(|s| !(1..=100).contains(&s)) {
        return Err(bad("steps must be 1 to 100"));
    }
    let reference = req.reference_item_id.filter(|r| !r.is_empty());
    if reference.as_deref() == Some(id.as_str()) {
        return Err(bad("the reference must be a different picture"));
    }
    let (item_id, reference_id) = (id.clone(), reference.clone());
    let (job, original) = lib
        .db
        .call(move |c| {
            let Some(item) = db::get_item(c, &item_id)? else {
                return Ok(Err((StatusCode::NOT_FOUND, "no such picture")));
            };
            if item.kind != "photo" {
                return Ok(Err((
                    StatusCode::BAD_REQUEST,
                    "only pictures can be edited",
                )));
            }
            if item.trashed_at.is_some() {
                return Ok(Err((
                    StatusCode::BAD_REQUEST,
                    "the picture is in the Trash",
                )));
            }
            if let Some(r) = &reference_id {
                match db::get_item(c, r)? {
                    Some(r) if r.kind == "photo" && r.trashed_at.is_none() => {}
                    _ => {
                        return Ok(Err((
                            StatusCode::BAD_REQUEST,
                            "the reference is not a picture in the library",
                        )))
                    }
                }
            }
            if lab::count_queued(c)? >= lab::MAX_QUEUED {
                return Ok(Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    "the Lab queue is full",
                )));
            }
            let job = lab::insert_job(
                c,
                &lab::NewJob {
                    item_id: item.id.clone(),
                    reference_item: reference_id,
                    prompt,
                    steps: req.steps,
                    seed: req.seed,
                },
            )?;
            Ok(Ok((job, item)))
        })
        .await?
        .map_err(|(s, m)| err(s, m))?;
    lib.emit("lab.queued", &job.id);
    lab_run::kick();
    let pos = lib
        .db
        .call({
            let j = job.clone();
            move |c| lab::queue_position(c, &j)
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(job_json(&lib, &job, Some(&original), pos)),
    ))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    status: Option<String>,
    item_id: Option<String>,
    limit: Option<i64>,
}

async fn list(State(lib): State<Library>, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let statuses: Vec<String> = q
        .status
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(s) = statuses.iter().find(|s| {
        !matches!(
            s.as_str(),
            "queued" | "running" | "done" | "failed" | "accepted" | "discarded"
        )
    }) {
        return Err(bad(format!("unknown status {s}")));
    }
    let (item_id, limit) = (q.item_id, q.limit.unwrap_or(100));
    let jobs = lib
        .db
        .call(move |c| lab::list_jobs(c, &statuses, item_id.as_deref(), limit))
        .await?;
    let jobs = describe(&lib, jobs).await?;
    // Whether anything will run the queue (the runner command is set).
    let enabled = lab_run::LabConfig::from_env().cmd.is_some();
    Ok(Json(json!({ "enabled": enabled, "jobs": jobs })))
}

async fn one(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let job = load(&lib, &id).await?;
    let mut v = describe(&lib, vec![job]).await?;
    Ok(Json(v.remove(0)))
}

async fn result(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let job = load(&lib, &id).await?;
    let (Some(hash), "done") = (job.result_hash.as_deref(), job.status.as_str()) else {
        return Err(not_found());
    };
    let path = lab::result_path(&lib.cfg, &job.id);
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(headers, path, "image/png", hash, "private, max-age=3600").await
}

/// The picture the model was given (PNG), while the job still holds it.
async fn before(
    State(lib): State<Library>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let job = load(&lib, &id).await?;
    let path = lab::job_dir(&lib.cfg, &job.id).join("input.png");
    if !path.is_file() {
        return Err(not_found());
    }
    serve_file(
        headers,
        path,
        "image/png",
        &format!("in-{}", job.id),
        "private, max-age=3600",
    )
    .await
}

async fn accept(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = job_id(&id)?.to_owned();
    let l2 = lib.clone();
    let done = tokio::task::spawn_blocking(move || lab::accept(&l2, &id))
        .await
        .map_err(ApiError::from)?
        .map_err(conflict)?;
    let item = lib
        .db
        .call({
            let id = done.item_id.clone();
            move |c| db::get_item(c, &id)
        })
        .await?
        .ok_or_else(not_found)?;
    let original = lib
        .db
        .call({
            let id = done.job.item_id.clone();
            move |c| db::get_item(c, &id)
        })
        .await?;
    Ok(Json(json!({
        "job": job_json(&lib, &done.job, original.as_ref(), None),
        "item": item_out(&lib, item, None),
        "duplicate": done.duplicate,
    })))
}

async fn discard(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = job_id(&id)?.to_owned();
    let l2 = lib.clone();
    let job = tokio::task::spawn_blocking(move || lab::discard(&l2, &id))
        .await
        .map_err(ApiError::from)?
        .map_err(conflict)?;
    let mut v = describe(&lib, vec![job]).await?;
    Ok(Json(v.remove(0)))
}

async fn retry(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let job = load(&lib, &id).await?;
    let jid = job.id.clone();
    if !lib.db.call(move |c| lab::retry(c, &jid)).await? {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "the job is {}, only a failed job can be retried",
                job.status
            ),
        ));
    }
    lib.emit("lab.queued", &job.id);
    lab_run::kick();
    let job = load(&lib, &job.id).await?;
    let mut v = describe(&lib, vec![job]).await?;
    Ok(Json(v.remove(0)))
}

async fn cancel(State(lib): State<Library>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let job = load(&lib, &id).await?;
    let jid = job.id.clone();
    if !lib.db.call(move |c| lab::delete_idle(c, &jid)).await? {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "the job is {}: running jobs finish first, finished ones are accepted or discarded",
                job.status
            ),
        ));
    }
    let _ = std::fs::remove_dir_all(lab::job_dir(&lib.cfg, &job.id));
    lib.emit("lab.cancelled", &job.id);
    Ok(StatusCode::NO_CONTENT)
}
