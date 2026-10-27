//! `/v1/meetings` HTTP API: Secretary Code's upload contract (`MEETINGS_API.md`) plus speaker
//! naming. Same auth as `/v1/memos` (p1 Caddy's `X-Memo-Gateway` after secretary-server
//! accepted the device, or the service bearer token).
//!
//! - `POST /v1/meetings` (`Idempotency-Key`) → `201 {id}` (`200` for a repeated key).
//! - `PUT /v1/meetings/{id}/tracks/{name}` with `Content-Range: bytes */N` (query) or
//!   `bytes a-b/N` → `308` + `Range: bytes=0-<last>` (partial), `200 {name, bytes, sha256}`
//!   (whole), `409`/`416` (another offset).
//! - `POST /v1/meetings/{id}/complete` → `202 {id, status: "processing"}`.
//! - `GET /v1/meetings/{id}` → `{id, status, brief, transcript, transcript_url, error, …}`.
//! - `GET /v1/meetings`, `GET …/{id}/transcript[?format=json]`, `POST …/{id}/speakers`,
//!   `POST …/{id}/reprocess`, `GET /v1/meetings/voices`, `POST /v1/meetings/voices/{voice}`.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::kernel::Range;
use super::model::{track_name, Brief, Meeting, MeetingTranscript, SourceApp, Speaker, Status, ROLES};
use super::store::{parse_content_range, valid_id, MeetingStore, NewMeeting, NewTrack, PutResult};
use super::worker::Labelled;
use crate::api::{ApiError, R};
use crate::store::{read_json, write_json, StoreError};
use crate::App;

pub fn routes(app: Arc<App>) -> Router<Arc<App>> {
    Router::new()
        .route("/v1/meetings", get(list).post(create))
        .route("/v1/meetings/voices", get(voices))
        .route("/v1/meetings/voices/{voice}", post(name_voice))
        .route("/v1/meetings/{id}", get(detail))
        .route("/v1/meetings/{id}/tracks/{name}", put(upload))
        .route("/v1/meetings/{id}/complete", post(complete))
        .route("/v1/meetings/{id}/transcript", get(transcript))
        .route("/v1/meetings/{id}/speakers", post(name_speaker))
        .route("/v1/meetings/{id}/reprocess", post(reprocess))
        .route_layer(middleware::from_fn_with_state(app, crate::api::auth))
}

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such meeting")
}

fn storage(app: &App) -> Result<Arc<MeetingStore>, ApiError> {
    app.meetings().map_err(|e| {
        tracing::error!(error = %e, "meetings archive unavailable");
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable", "the meetings archive is not available")
    })
}

fn meeting(store: &MeetingStore, id: &str) -> Result<Meeting, ApiError> {
    if !valid_id(id) {
        return Err(not_found());
    }
    store.get(id).ok_or_else(not_found)
}

#[derive(Deserialize)]
struct CreateTrack {
    name: String,
    bytes: u64,
    sha256: String,
    mime: String,
    #[serde(default)]
    start_offset_ms: Option<f64>,
}

#[derive(Deserialize)]
struct CreateBody {
    title: String,
    started_at: DateTime<Utc>,
    #[serde(default)]
    ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    source_app: Option<SourceApp>,
    #[serde(default)]
    calendar_event_id: Option<String>,
    tracks: Vec<CreateTrack>,
}

async fn create(State(app): State<Arc<App>>, headers: HeaderMap, Json(b): Json<CreateBody>) -> R<(StatusCode, Json<Value>)> {
    let store = storage(&app)?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if let Some(m) = key.as_deref().and_then(|k| store.list().into_iter().find(|m| m.idempotency_key.as_deref() == Some(k))) {
        return Ok((StatusCode::OK, Json(json!({"id": m.id, "status": m.status.public()}))));
    }
    let event_id = b.calendar_event_id.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty() && s.len() <= 300);
    // The folder is named now: the calendar event (by the Mac's id) and its company, looked up
    // briefly; when the calendar is slow the app's name stands in.
    let mut event = None;
    if let (Some(want), true) = (&event_id, crate::events::configured(&app)) {
        let from = b.started_at - chrono::Duration::hours(1);
        let to = b.ended_at.unwrap_or(b.started_at) + chrono::Duration::hours(1);
        match tokio::time::timeout(std::time::Duration::from_secs(4), crate::events::fetch(&app, from, to)).await {
            Ok(Ok(events)) => event = events.iter().find(|e| &e.id == want).map(|e| e.link()),
            Ok(Err(e)) => tracing::info!(error = %e, "calendar lookup for a new meeting failed"),
            Err(_) => tracing::info!("calendar lookup for a new meeting timed out"),
        }
    }
    let company = event.as_ref().and_then(|e| super::company(&e.attendees));
    let source_app = b.source_app.unwrap_or_default();
    let prefix = company
        .clone()
        .or_else(|| event.as_ref().map(|e| e.title.clone()))
        .or_else(|| source_app.name.clone());
    let linked = event.is_some();
    let (m, created) = store.create(NewMeeting {
        idempotency_key: key,
        title: b.title,
        started_at: b.started_at,
        ended_at: b.ended_at,
        source_app,
        calendar_event_id: event_id,
        tracks: b
            .tracks
            .into_iter()
            .map(|t| NewTrack {
                name: t.name,
                bytes: t.bytes,
                sha256: t.sha256,
                mime: t.mime,
                start_offset_ms: t.start_offset_ms.filter(|o| o.is_finite()).map(|o| o.round() as i64),
            })
            .collect(),
        folder_prefix: prefix,
        company,
        event,
    })?;
    let m = if linked && created {
        store.update(&m.id, |m| m.steps.linked_at = Some(Utc::now()))?
    } else {
        m
    };
    if created {
        tracing::info!(id = %m.id, folder = %m.folder, tracks = m.tracks.len(), "meeting created");
    }
    Ok((
        if created { StatusCode::CREATED } else { StatusCode::OK },
        Json(json!({"id": m.id, "status": m.status.public(), "folder": m.folder})),
    ))
}

async fn upload(
    State(app): State<Arc<App>>,
    Path((id, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> R<Response> {
    let store = storage(&app)?;
    let m = meeting(&store, &id)?;
    let name = track_name(&name).ok_or_else(not_found)?;
    let size = m.track(name).ok_or_else(not_found)?.bytes;
    let range: Range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range)
        .ok_or_else(|| ApiError::bad("Content-Range: bytes */<total> or bytes <start>-<end>/<total>"))?;
    let stored_range = |n: u64| (n > 0).then(|| HeaderValue::from_str(&format!("bytes=0-{}", n - 1)).ok()).flatten();
    let r = store.put(&id, name, range, body.into_data_stream()).await?;
    Ok(match r {
        PutResult::Partial(n) => {
            let mut r = (StatusCode::PERMANENT_REDIRECT, Json(json!({"name": name, "bytes": size, "received": n}))).into_response();
            if let Some(v) = stored_range(n) {
                r.headers_mut().insert(header::RANGE, v);
            }
            r
        }
        PutResult::Complete { bytes, sha256 } => Json(json!({"name": name, "bytes": bytes, "sha256": sha256})).into_response(),
        PutResult::DigestMismatch { bytes, sha256 } => Json(json!({
            "name": name, "bytes": bytes, "sha256": sha256, "verified": false,
            "error": "the stored bytes have another sha256; the track was reset, upload it again"
        }))
        .into_response(),
        PutResult::Mismatch(n) | PutResult::Unsatisfiable(n) => {
            let status = if matches!(r, PutResult::Mismatch(_)) { StatusCode::CONFLICT } else { StatusCode::RANGE_NOT_SATISFIABLE };
            let mut r = (
                status,
                Json(json!({"error": {"code": "offset_mismatch", "message": format!("the server has {n} bytes")}, "received": n})),
            )
                .into_response();
            if let Some(v) = stored_range(n) {
                r.headers_mut().insert(header::RANGE, v);
            }
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            r
        }
    })
}

async fn complete(State(app): State<Arc<App>>, Path(id): Path<String>) -> R<(StatusCode, Json<Value>)> {
    let store = storage(&app)?;
    meeting(&store, &id)?;
    let m = store.complete(&id)?;
    tracing::info!(%id, "meeting upload complete");
    app.meetings_wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(json!({"id": m.id, "status": m.status.public()}))))
}

fn analysis(store: &MeetingStore, m: &Meeting) -> Option<Labelled> {
    read_json(&store.work(m).join("analysis.json")).ok().flatten()
}

async fn detail(State(app): State<Arc<App>>, Path(id): Path<String>) -> R {
    let store = storage(&app)?;
    let m = meeting(&store, &id)?;
    let ready = m.status == Status::Done;
    let dir = store.dir(&m);
    let brief_md = std::fs::read_to_string(dir.join("brief.md")).ok().filter(|_| m.steps.written_at.is_some());
    let t: Option<MeetingTranscript> = read_json(&dir.join("transcript.json")).ok().flatten();
    let brief: Option<Brief> = read_json(&store.work(&m).join("brief.json")).ok().flatten();
    let speakers: Vec<Speaker> = analysis(&store, &m).map(|l| l.speakers).unwrap_or_default();
    Ok(Json(json!({
        "id": m.id,
        "status": m.status.public(),
        "stage": m.status,
        "title": m.title,
        "brief": brief_md,
        "transcript": t.as_ref().map(|t| super::brief::plain(&t.segments)),
        "transcript_url": ready.then(|| app.cfg.meeting_transcript_url(&id)),
        "error": (m.status == Status::Failed).then(|| m.error.clone()).flatten(),
        "speakers": speakers,
        "brief_json": brief,
        "meeting": m,
    })))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    before: Option<DateTime<Utc>>,
}

async fn list(State(app): State<Arc<App>>, Query(q): Query<ListQuery>) -> R {
    let store = storage(&app)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let mut all: Vec<Meeting> = store.list().into_iter().filter(|m| q.before.is_none_or(|b| m.started_at < b)).take(limit + 1).collect();
    let next = (all.len() > limit).then(|| all[limit - 1].started_at);
    all.truncate(limit);
    let items: Vec<Value> = all
        .iter()
        .map(|m| {
            json!({
                "id": m.id, "title": m.title, "status": m.status.public(), "stage": m.status,
                "started_at": m.started_at, "ended_at": m.ended_at, "duration_sec": m.duration_sec,
                "source_app": m.source_app, "folder": m.folder, "company": m.company,
                "event": m.event.as_ref().map(|e| &e.title), "error": m.error,
            })
        })
        .collect();
    Ok(Json(json!({"meetings": items, "next": next})))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    format: Option<String>,
}

async fn transcript(State(app): State<Arc<App>>, Path(id): Path<String>, Query(q): Query<TranscriptQuery>, headers: HeaderMap) -> R<Response> {
    let store = storage(&app)?;
    let m = meeting(&store, &id)?;
    if m.steps.written_at.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_ready", "no transcript yet"));
    }
    let (file, mime) = if q.format.as_deref() == Some("json") {
        ("transcript.json", "application/json")
    } else {
        ("transcript.md", "text/plain; charset=utf-8")
    };
    crate::api::serve_file(&store.dir(&m).join(file), mime, &headers, None).await
}

fn clean_name(s: &str) -> Result<String, ApiError> {
    let n = crate::store::clean_title(s);
    if n.is_empty() || n.chars().count() > 80 {
        return Err(ApiError::bad("name: 1 to 80 characters"));
    }
    Ok(n)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameBody {
    #[serde(alias = "id", alias = "cluster")]
    speaker: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// The owner names a speaker cluster (and/or fixes its role). The name goes to every output
/// of this meeting, the cluster's voice joins the person's profile (later recordings know
/// them), and other meetings and memos where the same unknown voice was heard get the name.
async fn name_speaker(State(app): State<Arc<App>>, Path(id): Path<String>, Json(b): Json<NameBody>) -> R {
    let store = storage(&app)?;
    let m = meeting(&store, &id)?;
    let name = b.name.as_deref().map(clean_name).transpose()?;
    if let Some(r) = &b.role {
        if !ROLES.contains(&r.as_str()) {
            return Err(ApiError::bad(format!("role: one of {}", ROLES.join(", "))));
        }
    }
    if name.is_none() && b.role.is_none() {
        return Err(ApiError::bad("name and/or role"));
    }
    if !matches!(m.status, Status::Done | Status::Failed) || m.steps.analyzed_at.is_none() {
        return Err(ApiError::new(StatusCode::CONFLICT, "busy", "the meeting is still being processed"));
    }
    let labelled = analysis(&store, &m).ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "busy", "no speakers yet"))?;
    let speaker = labelled
        .speakers
        .iter()
        .find(|s| s.id == b.speaker)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such speaker in this meeting"))?;
    if let Some(name) = &name {
        if speaker.id != "me" {
            match &speaker.voice_id {
                Some(voice) => {
                    if let Some((_, seen)) = app.voices.name_unknown(voice, name).map_err(StoreError::from)? {
                        relabel_sightings(&app, &store, &seen, &id, name).await;
                    }
                }
                None => {
                    let voices: super::model::Voices = read_json(&store.work(&m).join("voices.json")).map_err(StoreError::from)?.unwrap_or_default();
                    if let Some((_, c)) = voices.centroids.iter().find(|(s, _)| *s == speaker.id) {
                        app.voices.add_to_profile(name, false, c).map_err(StoreError::from)?;
                    }
                }
            }
        }
    }
    let m = rename_in_meeting(&store, &m, &b.speaker, name.as_deref(), b.role.as_deref())?;
    app.meetings_wake.notify_one();
    let speakers = analysis(&store, &m).map(|l| l.speakers).unwrap_or_default();
    Ok(Json(json!({"id": id, "status": m.status.public(), "speakers": speakers})))
}

/// Writes a speaker's name/role into the meeting's analysis and queues the outputs again
/// (files, note; no second notification).
fn rename_in_meeting(store: &MeetingStore, m: &Meeting, speaker: &str, name: Option<&str>, role: Option<&str>) -> Result<Meeting, StoreError> {
    let path = store.work(m).join("analysis.json");
    let Some(mut l) = read_json::<Labelled>(&path)? else {
        return Ok(m.clone());
    };
    for s in l.speakers.iter_mut().filter(|s| s.id == speaker) {
        if let Some(n) = name {
            s.name = Some(n.to_owned());
            s.name_source = Some("owner".into());
            s.voice_id = None;
        }
        if let Some(r) = role {
            s.role = Some(r.to_owned());
        }
    }
    super::worker::relabel(&mut l.utterances, &l.speakers);
    write_json(&path, &l)?;
    store.update(&m.id, |m| {
        m.revision += 1;
        m.steps.written_at = None;
        m.steps.noted_at = None;
        m.status = Status::Queued;
        m.attempts = 0;
        m.error = None;
        m.next_attempt_at = None;
    })
}

/// The named voice in its other recordings: finished meetings are re-rendered, memos get the
/// name in their transcript.
async fn relabel_sightings(app: &App, store: &MeetingStore, seen: &[crate::diarize::profiles::Sighting], except: &str, name: &str) {
    for s in seen.iter().filter(|s| s.source != except) {
        if let Some(other) = store.get(&s.source).filter(|o| matches!(o.status, Status::Done | Status::Failed)) {
            if let Err(e) = rename_in_meeting(store, &other, &s.speaker, Some(name), None) {
                tracing::warn!(meeting = %s.source, error = %e, "could not name the voice there");
            }
        } else if crate::store::valid_id(&s.source) {
            crate::diarize::memo::rename_speaker(app, &s.source, &s.speaker, name);
        }
    }
    app.wake.notify_one();
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceName {
    name: String,
}

async fn name_voice(State(app): State<Arc<App>>, Path(voice): Path<String>, Json(b): Json<VoiceName>) -> R {
    let store = storage(&app)?;
    let name = clean_name(&b.name)?;
    let (profile, seen) = app
        .voices
        .name_unknown(&voice, &name)
        .map_err(StoreError::from)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such voice"))?;
    relabel_sightings(&app, &store, &seen, "", &name).await;
    app.meetings_wake.notify_one();
    Ok(Json(json!({"profile": profile, "name": name, "recordings": seen.len()})))
}

async fn voices(State(app): State<Arc<App>>) -> R {
    let b = app.voices.snapshot();
    let after = app.cfg.diarize.as_ref().map_or(2, |c| c.ask_after);
    Ok(Json(json!({
        "profiles": b.profiles.iter().map(|p| json!({
            "id": p.id, "name": p.name, "owner": p.owner, "samples": p.embeddings.len(), "updated_at": p.updated_at,
        })).collect::<Vec<_>>(),
        "unknown": b.unknown.iter().filter(|u| u.named.is_none()).map(|u| json!({
            "id": u.id, "recordings": u.recordings(), "recurring": u.recordings() >= after.max(2),
            "asked_at": u.asked_at, "sightings": u.sightings,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ReprocessBody {
    /// `transcript` (diarization and everything after; Whisper's output is kept), `analysis`
    /// (roles, kinds, brief and after), `whisper` (from scratch).
    #[serde(default)]
    from: Option<String>,
}

async fn reprocess(State(app): State<Arc<App>>, Path(id): Path<String>, body: Option<Json<ReprocessBody>>) -> R {
    let store = storage(&app)?;
    let m = meeting(&store, &id)?;
    if !matches!(m.status, Status::Done | Status::Failed) {
        return Err(ApiError::new(StatusCode::CONFLICT, "busy", "the meeting is uploading or being processed"));
    }
    let from = body.and_then(|Json(b)| b.from).unwrap_or_else(|| "analysis".into());
    let work = store.work(&m);
    let remove: &[&str] = match from.as_str() {
        "whisper" => &["mic.json", "system.json", "turns.json", "timeline.json", "analysis.json", "brief.json"],
        "transcript" => &["turns.json", "timeline.json", "analysis.json", "brief.json"],
        "analysis" => &["analysis.json", "brief.json"],
        _ => return Err(ApiError::bad("from: whisper, transcript or analysis")),
    };
    for f in remove {
        let _ = std::fs::remove_file(work.join(f));
    }
    let m = store.update(&id, |m| {
        if from == "whisper" {
            m.steps.transcribed_at = None;
        }
        if from != "analysis" {
            m.steps.diarized_at = None;
        }
        m.steps.analyzed_at = None;
        m.steps.briefed_at = None;
        m.steps.written_at = None;
        m.steps.noted_at = None;
        m.steps.remembered_at = None;
        m.steps.notified_at = None;
        m.revision += 1;
        m.status = Status::Queued;
        m.attempts = 0;
        m.error = None;
        m.next_attempt_at = None;
    })?;
    app.meetings_wake.notify_one();
    Ok(Json(json!({"id": id, "status": m.status.public(), "revision": m.revision})))
}
