//! The HTTP API (`docs/design/notebooks-api.md`). lucina-server mounts it under
//! `/api/secretary/notebooks` after its own bearer check; secretary-server forwards
//! `/v1/notebooks/*` here unchanged.

use std::convert::Infallible;
use std::io::SeekFrom;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::ids::{new_id, slug, valid};
use crate::llm::Model;
use crate::search::excerpt;
use crate::{chat, podcast, Error, Notebooks};

type ApiResult = Result<Response, Error>;

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Error::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Error::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Error::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Error::Internal(m) => {
                tracing::error!(error = %m, "notebooks: request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR { "internal error".to_owned() } else { self.to_string() };
        (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
    }
}

fn ok(v: impl serde::Serialize) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created(v: impl serde::Serialize) -> ApiResult {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

fn accepted(v: impl serde::Serialize) -> ApiResult {
    Ok((StatusCode::ACCEPTED, Json(v)).into_response())
}

fn check(id: &str, prefix: &str, what: &'static str) -> Result<(), Error> {
    if valid(id, prefix) {
        Ok(())
    } else {
        Err(Error::NotFound(what))
    }
}

fn text_field(v: Option<String>, what: &str, max: usize) -> Result<Option<String>, Error> {
    match v {
        Some(s) if s.chars().count() > max => Err(Error::BadRequest(format!("{what} is too long (max {max} characters)"))),
        v => Ok(v.map(|s| s.trim().to_owned())),
    }
}

pub fn router(nb: Notebooks) -> Router {
    let max = nb.cfg().max_upload;
    Router::new()
        .route("/", get(list_notebooks).post(create_notebook))
        .route("/health", get(health))
        .route("/search", get(search_all))
        .route("/transformations", get(list_transformations).post(create_transformation))
        .route("/transformations/{t}", patch(update_transformation).delete(delete_transformation))
        .route("/podcast-profiles", get(podcast_profiles))
        .route("/sources/{src}", get(get_source))
        .route("/sources/{src}/chunks", get(source_chunks))
        .route("/sources/{src}/file", get(source_file))
        .route("/sources/{src}/retry", post(retry_source))
        .route("/sources/{src}/insights", get(list_insights).post(create_insight))
        .route("/insights/{i}", axum::routing::delete(delete_insight))
        .route("/insights/{i}/note", post(insight_note))
        .route("/chats/{c}", get(get_chat).patch(rename_chat).delete(delete_chat))
        .route("/chats/{c}/messages", post(send_message))
        .route("/chats/{c}/messages/{m}/note", post(message_note))
        .route("/podcasts/{p}", get(get_podcast).delete(delete_podcast))
        .route("/podcasts/{p}/audio", get(podcast_audio))
        .route("/{nb}", get(get_notebook).patch(update_notebook).delete(delete_notebook))
        .route("/{nb}/sources", post(add_source))
        .route("/{nb}/sources/upload", post(upload_source).layer(DefaultBodyLimit::max(max + 64 * 1024)))
        .route("/{nb}/sources/{src}", patch(set_context).delete(remove_source))
        .route("/{nb}/notes", post(add_note))
        .route("/{nb}/notes/{slug}", axum::routing::delete(remove_note))
        .route("/{nb}/search", get(search_notebook))
        .route("/{nb}/chats", get(list_chats).post(create_chat))
        .route("/{nb}/podcasts", get(list_podcasts).post(create_podcast))
        .with_state(nb)
}

// ------------------------------------------------------------------ notebooks

#[derive(Deserialize)]
struct ListQ {
    archived: Option<String>,
}

async fn list_notebooks(State(nb): State<Notebooks>, Query(q): Query<ListQ>) -> ApiResult {
    let archived = match q.archived.as_deref() {
        None | Some("exclude") => Some(false),
        Some("only") => Some(true),
        Some("include") => None,
        Some(_) => return Err(Error::BadRequest("archived is exclude, include or only".into())),
    };
    ok(json!({ "notebooks": nb.store().notebooks(archived)? }))
}

#[derive(Deserialize)]
struct NotebookBody {
    name: Option<String>,
    description: Option<String>,
    archived: Option<bool>,
}

async fn create_notebook(State(nb): State<Notebooks>, Json(b): Json<NotebookBody>) -> ApiResult {
    let name = text_field(b.name, "name", 200)?.filter(|n| !n.is_empty()).ok_or_else(|| Error::BadRequest("name is required".into()))?;
    let description = text_field(b.description, "description", 4000)?.unwrap_or_default();
    created(nb.store().create_notebook(&new_id("nb"), &name, &description)?)
}

async fn get_notebook(State(nb): State<Notebooks>, Path(id): Path<String>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let st = nb.store();
    let n = st.notebook(&id)?;
    let mut v = serde_json::to_value(&n).map_err(|e| Error::Internal(e.to_string()))?;
    v["sources"] = json!(st.notebook_sources(&id)?);
    v["notes"] = json!(st.notes(&id)?);
    v["chats"] = json!(st.chats(&id)?);
    v["podcasts"] = json!(st.podcasts(&id)?);
    ok(v)
}

async fn update_notebook(State(nb): State<Notebooks>, Path(id): Path<String>, Json(b): Json<NotebookBody>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let name = text_field(b.name, "name", 200)?;
    if name.as_deref() == Some("") {
        return Err(Error::BadRequest("name can't be empty".into()));
    }
    let description = text_field(b.description, "description", 4000)?;
    ok(nb.store().update_notebook(&id, name.as_deref(), description.as_deref(), b.archived)?)
}

#[derive(Deserialize)]
struct DeleteQ {
    sources: Option<String>,
}

async fn delete_notebook(State(nb): State<Notebooks>, Path(id): Path<String>, Query(q): Query<DeleteQ>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let st = nb.store();
    st.notebook(&id)?;
    let exclusive = match q.sources.as_deref() {
        None | Some("keep") => Vec::new(),
        Some("delete-exclusive") => st.exclusive_sources(&id)?,
        Some(_) => return Err(Error::BadRequest("sources is keep or delete-exclusive".into())),
    };
    let podcasts = st.podcasts(&id)?;
    st.delete_notebook(&id)?;
    for s in &exclusive {
        st.delete_source(s)?;
        let _ = tokio::fs::remove_dir_all(nb.files_dir(s)).await;
    }
    for p in podcasts {
        let _ = tokio::fs::remove_file(nb.podcast_path(&p.id)).await;
    }
    ok(json!({ "deleted": true, "sources_deleted": exclusive.len() }))
}

// ------------------------------------------------------------------ sources

#[derive(Deserialize)]
struct SourceBody {
    kind: String,
    url: Option<String>,
    text: Option<String>,
    title: Option<String>,
    path: Option<String>,
    slug: Option<String>,
    id: Option<String>,
    #[serde(default)]
    transformations: Vec<String>,
}

fn check_transformations(nb: &Notebooks, ts: &[String]) -> Result<(), Error> {
    for t in ts {
        nb.store().transformation(t)?;
    }
    Ok(())
}

async fn add_source(State(nb): State<Notebooks>, Path(id): Path<String>, Json(b): Json<SourceBody>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let st = nb.store();
    st.notebook(&id)?;
    check_transformations(&nb, &b.transformations)?;
    let title = text_field(b.title, "title", 300)?.unwrap_or_default();
    let sid = new_id("s");
    match b.kind.as_str() {
        "url" => {
            let url = b.url.as_deref().map(str::trim).unwrap_or("");
            let parsed = reqwest::Url::parse(url).map_err(|_| Error::BadRequest("url is not a valid link".into()))?;
            if !["http", "https"].contains(&parsed.scheme()) {
                return Err(Error::BadRequest("only http and https links".into()));
            }
            let kind = if crate::ingest::youtube_id(url).is_some() { "youtube" } else { "url" };
            st.create_source(&sid, kind, &title, url, "queued", "", None, &b.transformations)?;
        }
        "text" => {
            let text = text_field(b.text, "text", 2_000_000)?.filter(|t| !t.is_empty()).ok_or_else(|| Error::BadRequest("text is required".into()))?;
            let title = if title.is_empty() { text.lines().next().unwrap_or("Text").chars().take(80).collect() } else { title };
            st.create_source(&sid, "text", &title, "text", "queued", &text, None, &b.transformations)?;
        }
        "sandbox" => {
            let path = b.path.as_deref().map(str::trim).unwrap_or("");
            if !path.starts_with("/home/cua/") || path.split('/').any(|p| p == "..") || path.ends_with('/') {
                return Err(Error::BadRequest("path must be a file under /home/cua/ on Lucina's computer".into()));
            }
            st.create_source(&sid, "sandbox", &title, &format!("sandbox:{path}"), "queued", "", None, &b.transformations)?;
        }
        "note" => {
            let s = b.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| Error::BadRequest("slug is required".into()))?;
            // A note is linked, not copied: it stays in Keep.
            let link = nb.link_note(&id, s).await?;
            return created(json!({ "note": link }));
        }
        "source" => {
            let existing = b.id.as_deref().unwrap_or("");
            check(existing, "s", "source")?;
            st.source(existing, false)?;
            st.link_source(&id, existing)?;
            return ok(source_in(&nb, &id, existing)?);
        }
        _ => return Err(Error::BadRequest("kind is url, text, sandbox, note or source".into())),
    }
    st.link_source(&id, &sid)?;
    nb.wake();
    accepted(source_in(&nb, &id, &sid)?)
}

fn source_in(nb: &Notebooks, notebook_id: &str, source_id: &str) -> Result<Value, Error> {
    let s = nb
        .store()
        .notebook_sources(notebook_id)?
        .into_iter()
        .find(|s| s.id == source_id)
        .ok_or(Error::NotFound("source"))?;
    serde_json::to_value(s).map_err(|e| Error::Internal(e.to_string()))
}

fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c })
        .collect::<String>()
        .trim_start_matches('.')
        .chars()
        .take(160)
        .collect();
    if clean.trim().is_empty() {
        "file".into()
    } else {
        clean
    }
}

async fn upload_source(State(nb): State<Notebooks>, Path(id): Path<String>, mut mp: Multipart) -> ApiResult {
    check(&id, "nb", "notebook")?;
    nb.store().notebook(&id)?;
    let sid = new_id("s");
    let dir = nb.files_dir(&sid);
    let mut title = String::new();
    let mut transformations: Vec<String> = Vec::new();
    let mut saved: Option<String> = None;
    let fail = |e: &dyn std::fmt::Display| Error::BadRequest(format!("upload: {e}"));
    while let Some(mut field) = mp.next_field().await.map_err(|e| fail(&e))? {
        match field.name().unwrap_or("") {
            "file" => {
                let name = safe_name(field.file_name().unwrap_or("file"));
                tokio::fs::create_dir_all(&dir).await?;
                let path = dir.join(&name);
                let mut f = tokio::fs::File::create(&path).await?;
                let mut size = 0usize;
                while let Some(chunk) = field.chunk().await.map_err(|e| fail(&e))? {
                    size += chunk.len();
                    if size > nb.cfg().max_upload {
                        drop(f);
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        return Err(Error::BadRequest("the file is too large".into()));
                    }
                    f.write_all(&chunk).await?;
                }
                f.flush().await?;
                saved = Some(name);
            }
            "title" => title = field.text().await.map_err(|e| fail(&e))?.trim().chars().take(300).collect(),
            "transformations" => {
                let t = field.text().await.map_err(|e| fail(&e))?;
                transformations = serde_json::from_str(&t).map_err(|_| Error::BadRequest("transformations is a JSON list".into()))?;
            }
            _ => {}
        }
    }
    let Some(name) = saved else {
        return Err(Error::BadRequest("file is required".into()));
    };
    if let Err(e) = check_transformations(&nb, &transformations) {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(e);
    }
    let kind = if crate::ingest::extension(&name) == "pdf" { "pdf" } else { "file" };
    nb.store().create_source(&sid, kind, &title, &format!("upload:{name}"), "queued", "", Some(&name), &transformations)?;
    nb.store().link_source(&id, &sid)?;
    nb.wake();
    accepted(source_in(&nb, &id, &sid)?)
}

#[derive(Deserialize)]
struct ContextBody {
    context: String,
}

async fn set_context(State(nb): State<Notebooks>, Path((id, src)): Path<(String, String)>, Json(b): Json<ContextBody>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    check(&src, "s", "source")?;
    if !["full", "summary", "off"].contains(&b.context.as_str()) {
        return Err(Error::BadRequest("context is full, summary or off".into()));
    }
    nb.store().set_context(&id, &src, &b.context)?;
    ok(source_in(&nb, &id, &src)?)
}

async fn remove_source(State(nb): State<Notebooks>, Path((id, src)): Path<(String, String)>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    check(&src, "s", "source")?;
    let deleted = nb.store().unlink_source(&id, &src)?;
    if deleted {
        let _ = tokio::fs::remove_dir_all(nb.files_dir(&src)).await;
    }
    ok(json!({ "removed": true, "deleted": deleted }))
}

#[derive(Deserialize)]
struct SourceQ {
    text: Option<String>,
}

async fn get_source(State(nb): State<Notebooks>, Path(src): Path<String>, Query(q): Query<SourceQ>) -> ApiResult {
    check(&src, "s", "source")?;
    let with_text = matches!(q.text.as_deref(), Some("1" | "true"));
    ok(nb.store().source(&src, with_text)?)
}

#[derive(Deserialize)]
struct ChunksQ {
    from: Option<i64>,
    limit: Option<i64>,
    around: Option<i64>,
}

async fn source_chunks(State(nb): State<Notebooks>, Path(src): Path<String>, Query(q): Query<ChunksQ>) -> ApiResult {
    check(&src, "s", "source")?;
    nb.store().source(&src, false)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let from = match q.around {
        Some(a) => (a - limit / 2).max(0),
        None => q.from.unwrap_or(0).max(0),
    };
    ok(json!({ "chunks": nb.store().chunks_of(&src, from, limit)? }))
}

fn content_type(name: &str) -> &'static str {
    match crate::ingest::extension(name).as_str() {
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "txt" | "md" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

async fn source_file(State(nb): State<Notebooks>, Path(src): Path<String>, headers: HeaderMap) -> ApiResult {
    check(&src, "s", "source")?;
    let s = nb.store().source(&src, false)?;
    let name = s.file_name.ok_or(Error::NotFound("file"))?;
    let path = nb.files_dir(&src).join(&name);
    let disposition = format!("inline; filename*=UTF-8''{}", percent(&name));
    serve_file(&path, content_type(&name), &headers, Some(disposition)).await
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` → (start, end inclusive).
pub fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n), len - 1)
        }
        (a, "") => (a.parse().ok()?, len - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

async fn serve_file(path: &std::path::Path, ctype: &str, headers: &HeaderMap, disposition: Option<String>) -> ApiResult {
    let mut f = tokio::fs::File::open(path).await.map_err(|_| Error::NotFound("file"))?;
    let len = f.metadata().await?.len();
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(ctype).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    resp_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    if let Some(d) = disposition.and_then(|d| HeaderValue::from_str(&d).ok()) {
        resp_headers.insert(header::CONTENT_DISPOSITION, d);
    }
    match range {
        Some(r) => {
            let Some((start, end)) = parse_range(r, len) else {
                resp_headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{len}")).expect("ascii"));
                return Ok((StatusCode::RANGE_NOT_SATISFIABLE, resp_headers).into_response());
            };
            f.seek(SeekFrom::Start(start)).await?;
            let n = end - start + 1;
            resp_headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).expect("ascii"));
            resp_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
            let body = Body::from_stream(tokio_util::io::ReaderStream::new(f.take(n)));
            Ok((StatusCode::PARTIAL_CONTENT, resp_headers, body).into_response())
        }
        None => {
            resp_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            Ok((StatusCode::OK, resp_headers, Body::from_stream(tokio_util::io::ReaderStream::new(f))).into_response())
        }
    }
}

async fn retry_source(State(nb): State<Notebooks>, Path(src): Path<String>) -> ApiResult {
    check(&src, "s", "source")?;
    let s = nb.store().source(&src, false)?;
    if s.status != "failed" {
        return Err(Error::Conflict("only a failed source can be retried".into()));
    }
    nb.store().reset_source_attempts(&src)?;
    nb.store().set_source_status(&src, "queued", None)?;
    nb.wake();
    accepted(nb.store().source(&src, false)?)
}

// ------------------------------------------------------------------ insights

async fn list_insights(State(nb): State<Notebooks>, Path(src): Path<String>) -> ApiResult {
    check(&src, "s", "source")?;
    nb.store().source(&src, false)?;
    ok(json!({ "insights": nb.store().insights(&src)? }))
}

#[derive(Deserialize)]
struct InsightBody {
    transformation_id: String,
}

async fn create_insight(State(nb): State<Notebooks>, Path(src): Path<String>, Json(b): Json<InsightBody>) -> ApiResult {
    check(&src, "s", "source")?;
    accepted(nb.queue_insight(&src, &b.transformation_id)?)
}

async fn delete_insight(State(nb): State<Notebooks>, Path(i): Path<String>) -> ApiResult {
    check(&i, "i", "insight")?;
    nb.store().delete_insight(&i)?;
    ok(json!({ "deleted": true }))
}

async fn insight_note(State(nb): State<Notebooks>, Path(i): Path<String>) -> ApiResult {
    check(&i, "i", "insight")?;
    let ins = nb.store().insight(&i)?;
    if ins.status != "ready" {
        return Err(Error::Conflict("the insight is not ready yet".into()));
    }
    let src = nb.store().source(&ins.source_id, false)?;
    let notebook = src.notebooks.first().cloned().ok_or(Error::NotFound("notebook"))?;
    let title = format!("{} · {}", ins.title, src.title);
    created(nb.create_note(&notebook, &title, &ins.text).await?)
}

// ------------------------------------------------------------------ notes

#[derive(Deserialize)]
struct NoteBody {
    slug: Option<String>,
    title: Option<String>,
    text: Option<String>,
}

async fn add_note(State(nb): State<Notebooks>, Path(id): Path<String>, Json(b): Json<NoteBody>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    if let Some(s) = b.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return created(nb.link_note(&id, s).await?);
    }
    let text = text_field(b.text, "text", 200_000)?.filter(|t| !t.is_empty()).ok_or_else(|| Error::BadRequest("slug, or title and text, are required".into()))?;
    let title = text_field(b.title, "title", 300)?.filter(|t| !t.is_empty()).unwrap_or_else(|| text.lines().next().unwrap_or("Note").chars().take(80).collect());
    created(nb.create_note(&id, &title, &text).await?)
}

async fn remove_note(State(nb): State<Notebooks>, Path((id, s)): Path<(String, String)>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    nb.store().unlink_note(&id, &s)?;
    ok(json!({ "removed": true }))
}

// ------------------------------------------------------------------ search

#[derive(Deserialize)]
struct SearchQ {
    q: Option<String>,
    limit: Option<usize>,
}

async fn hits_json(nb: &Notebooks, hits: Vec<crate::search::Hit>, q: &str, with_notebooks: bool) -> Result<Value, Error> {
    let passages = chat::passages(nb.store(), &hits)?;
    let mut out = Vec::new();
    for (p, h) in passages.into_iter().zip(hits) {
        let (kind, rest) = p.reference.split_once(':').unwrap_or(("source", ""));
        let mut v = json!({
            "ref": p.reference,
            "kind": kind,
            "title": p.title,
            "excerpt": excerpt(&p.chunk.text, q, 240),
            "score": h.score,
            "page": p.chunk.page,
            "start_s": p.chunk.start_s,
        });
        match kind {
            "source" => {
                v["source_id"] = json!(p.chunk.owner);
                v["chunk"] = json!(p.chunk.ordinal);
            }
            "note" => v["slug"] = json!(rest),
            _ => v["insight_id"] = json!(rest),
        }
        if with_notebooks {
            v["notebook_ids"] = json!(nb.store().notebooks_of_owner(&p.chunk.owner)?);
        }
        out.push(v);
    }
    Ok(json!({ "hits": out }))
}

async fn search_notebook(State(nb): State<Notebooks>, Path(id): Path<String>, Query(q): Query<SearchQ>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let query = q.q.unwrap_or_default();
    let hits = nb.search(Some(&id), &query, q.limit.unwrap_or(20).clamp(1, 50)).await?;
    ok(hits_json(&nb, hits, &query, false).await?)
}

async fn search_all(State(nb): State<Notebooks>, Query(q): Query<SearchQ>) -> ApiResult {
    let query = q.q.unwrap_or_default();
    let hits = nb.search(None, &query, q.limit.unwrap_or(20).clamp(1, 50)).await?;
    ok(hits_json(&nb, hits, &query, true).await?)
}

// ------------------------------------------------------------------ chats

async fn list_chats(State(nb): State<Notebooks>, Path(id): Path<String>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    nb.store().notebook(&id)?;
    ok(json!({ "chats": nb.store().chats(&id)? }))
}

#[derive(Deserialize)]
struct ChatBody {
    title: Option<String>,
}

async fn create_chat(State(nb): State<Notebooks>, Path(id): Path<String>, body: Option<Json<ChatBody>>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    nb.store().notebook(&id)?;
    let title = text_field(body.and_then(|b| b.0.title), "title", 200)?.filter(|t| !t.is_empty()).unwrap_or_else(|| "New chat".into());
    created(nb.store().create_chat(&new_id("c"), &id, &title)?)
}

async fn get_chat(State(nb): State<Notebooks>, Path(c): Path<String>) -> ApiResult {
    check(&c, "c", "chat")?;
    let chat = nb.store().chat(&c)?;
    let mut v = serde_json::to_value(chat).map_err(|e| Error::Internal(e.to_string()))?;
    v["messages"] = json!(nb.store().messages(&c)?);
    ok(v)
}

async fn rename_chat(State(nb): State<Notebooks>, Path(c): Path<String>, Json(b): Json<ChatBody>) -> ApiResult {
    check(&c, "c", "chat")?;
    let title = text_field(b.title, "title", 200)?.filter(|t| !t.is_empty()).ok_or_else(|| Error::BadRequest("title is required".into()))?;
    ok(nb.store().rename_chat(&c, &title)?)
}

async fn delete_chat(State(nb): State<Notebooks>, Path(c): Path<String>) -> ApiResult {
    check(&c, "c", "chat")?;
    nb.store().delete_chat(&c)?;
    ok(json!({ "deleted": true }))
}

#[derive(Deserialize)]
struct MessageBody {
    text: String,
    model: Option<String>,
}

async fn send_message(State(nb): State<Notebooks>, Path(c): Path<String>, headers: HeaderMap, Json(b): Json<MessageBody>) -> ApiResult {
    check(&c, "c", "chat")?;
    let model = Model::parse(b.model.as_deref()).ok_or_else(|| Error::BadRequest("model is deepseek or qwen".into()))?;
    if b.text.chars().count() > 8000 {
        return Err(Error::BadRequest("the message is too long".into()));
    }
    nb.store().chat(&c)?;
    let streaming = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if !streaming {
        let (user, reply) = nb.ask(&c, &b.text, model, None).await?;
        return ok(json!({ "user": user, "reply": reply }));
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        let delta_tx = tx.clone();
        let on_delta = Box::new(move |d: &str| {
            let _ = delta_tx.send(Event::default().event("delta").data(json!({ "text": d }).to_string()));
        });
        // The answer is stored even if the client went away mid-stream.
        let ev = match nb.ask(&c, &b.text, model, Some(on_delta)).await {
            Ok((user, reply)) => Event::default().event("message").data(json!({ "user": user, "reply": reply }).to_string()),
            Err(e) => Event::default().event("error").data(json!({ "error": { "message": e.to_string() } }).to_string()),
        };
        let _ = tx.send(ev);
    });
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(Ok::<_, Infallible>);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()).into_response())
}

#[derive(Deserialize, Default)]
struct SaveBody {
    title: Option<String>,
}

async fn message_note(State(nb): State<Notebooks>, Path((c, m)): Path<(String, String)>, body: Option<Json<SaveBody>>) -> ApiResult {
    check(&c, "c", "chat")?;
    check(&m, "m", "message")?;
    let chat = nb.store().chat(&c)?;
    let msg = nb.store().message(&c, &m)?;
    if msg.role != "assistant" {
        return Err(Error::BadRequest("only answers can be saved as notes".into()));
    }
    let title = text_field(body.and_then(|b| b.0.title), "title", 300)?.filter(|t| !t.is_empty()).unwrap_or_else(|| chat.title.clone());
    // Passage ids mean nothing in Keep: the note lists its sources at the end instead.
    let mut text = msg.text.clone();
    if !msg.citations.is_empty() {
        text.push_str("\n\n**Sources**\n");
        for (i, cit) in msg.citations.iter().enumerate() {
            let at = match (cit.page, cit.start_s) {
                (Some(p), _) => format!(", p. {p}"),
                (_, Some(s)) => format!(", {}", chat::clock(s)),
                _ => String::new(),
            };
            text.push_str(&format!("{}. {}{} [{}]\n", i + 1, cit.title, at, cit.reference));
        }
    }
    created(nb.create_note(&chat.notebook_id, &title, &text).await?)
}

// ------------------------------------------------------------------ transformations

#[derive(Deserialize)]
struct TransformationBody {
    name: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    apply_default: Option<bool>,
}

async fn list_transformations(State(nb): State<Notebooks>) -> ApiResult {
    ok(json!({ "transformations": nb.store().transformations()? }))
}

async fn create_transformation(State(nb): State<Notebooks>, Json(b): Json<TransformationBody>) -> ApiResult {
    let name = text_field(b.name, "name", 100)?.filter(|n| !n.is_empty()).ok_or_else(|| Error::BadRequest("name is required".into()))?;
    let prompt = text_field(b.prompt, "prompt", 20_000)?.filter(|p| !p.is_empty()).ok_or_else(|| Error::BadRequest("prompt is required".into()))?;
    let id = format!("t-{}", slug(&name, 40));
    if id == "t-" {
        return Err(Error::BadRequest("name needs letters or digits".into()));
    }
    created(nb.store().create_transformation(&crate::store::Transformation {
        id,
        name,
        description: text_field(b.description, "description", 500)?.unwrap_or_default(),
        prompt,
        apply_default: b.apply_default.unwrap_or(false),
        builtin: false,
    })?)
}

async fn update_transformation(State(nb): State<Notebooks>, Path(t): Path<String>, Json(b): Json<TransformationBody>) -> ApiResult {
    let name = text_field(b.name, "name", 100)?;
    let description = text_field(b.description, "description", 500)?;
    let prompt = text_field(b.prompt, "prompt", 20_000)?;
    if name.as_deref() == Some("") || prompt.as_deref() == Some("") {
        return Err(Error::BadRequest("name and prompt can't be empty".into()));
    }
    ok(nb.store().update_transformation(&t, name.as_deref(), description.as_deref(), prompt.as_deref(), b.apply_default)?)
}

async fn delete_transformation(State(nb): State<Notebooks>, Path(t): Path<String>) -> ApiResult {
    nb.store().delete_transformation(&t)?;
    ok(json!({ "deleted": true }))
}

// ------------------------------------------------------------------ podcasts

async fn podcast_profiles() -> ApiResult {
    ok(json!({ "episode_profiles": podcast::episode_profiles(), "speaker_profiles": podcast::speaker_profiles() }))
}

async fn list_podcasts(State(nb): State<Notebooks>, Path(id): Path<String>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    nb.store().notebook(&id)?;
    ok(json!({ "podcasts": nb.store().podcasts(&id)? }))
}

#[derive(Deserialize)]
struct PodcastBody {
    episode_profile: Option<String>,
    speaker_profile: Option<String>,
    briefing: Option<String>,
    minutes: Option<f64>,
}

async fn create_podcast(State(nb): State<Notebooks>, Path(id): Path<String>, Json(b): Json<PodcastBody>) -> ApiResult {
    check(&id, "nb", "notebook")?;
    let briefing = text_field(b.briefing, "briefing", 4000)?;
    accepted(nb.queue_podcast(&id, b.episode_profile.as_deref(), b.speaker_profile.as_deref(), briefing.as_deref(), b.minutes)?)
}

async fn get_podcast(State(nb): State<Notebooks>, Path(p): Path<String>) -> ApiResult {
    check(&p, "p", "podcast")?;
    ok(nb.store().podcast(&p)?)
}

async fn podcast_audio(State(nb): State<Notebooks>, Path(p): Path<String>, headers: HeaderMap) -> ApiResult {
    check(&p, "p", "podcast")?;
    let pod = nb.store().podcast(&p)?;
    if pod.status != "ready" {
        return Err(Error::Conflict("the podcast is not ready yet".into()));
    }
    let name = format!("{}.mp3", pod.title.chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>().trim());
    serve_file(&nb.podcast_path(&p), "audio/mpeg", &headers, Some(format!("inline; filename*=UTF-8''{}", percent(&name)))).await
}

async fn delete_podcast(State(nb): State<Notebooks>, Path(p): Path<String>) -> ApiResult {
    check(&p, "p", "podcast")?;
    nb.store().delete_podcast(&p)?;
    let _ = tokio::fs::remove_file(nb.podcast_path(&p)).await;
    ok(json!({ "deleted": true }))
}

async fn health(State(nb): State<Notebooks>) -> ApiResult {
    ok(nb.health()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=990-2000", 1000), Some((990, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }

    #[test]
    fn upload_names_are_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("..hidden"), "hidden");
        assert_eq!(safe_name("C:\\x\\RM Class 2.pdf"), "RM Class 2.pdf");
        assert_eq!(safe_name(""), "file");
    }
}
