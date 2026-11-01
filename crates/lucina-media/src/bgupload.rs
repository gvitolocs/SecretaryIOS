//! Background uploads from the iPhone's PhotoKit upload extension (iOS 26.1+): iOS itself sends a
//! new photo to `POST /v1/media/bgupload` while Secretary is closed. The system speaks the IETF
//! resumable upload draft (`resumable.rs`):
//!
//! - `OPTIONS` (preflight, cached by iOS): 200 with `Upload-Limit`.
//! - `POST /v1/media/bgupload`: the file's bytes, with the photo's metadata in `X-Lucina-Meta`
//!   (base64url JSON [`ClientMeta`]). The server answers `104 Upload Resumption Supported` with the
//!   upload's URL as soon as it has created it, then 201 when the body is in.
//! - `HEAD /v1/media/bgupload/<id>.<token>`: the offset to resume from.
//! - `PATCH` the same URL with `Upload-Offset`: more bytes. `DELETE`: give up.
//!
//! The POST passes p1's device auth like every upload. The upload's URL is a capability (a random
//! token, stored as its blake3), because iOS may resume without the original headers; p1 lets it
//! through without device auth but still adds the upstream key.
//!
//! hyper's server can't send a 1xx response, so this API has its own listener ([`serve`]) whose
//! connections hand the handler the socket: [`Informational`] writes the 104 straight onto it while
//! hyper waits for the handler (nothing else is written in between on an HTTP/1.1 connection).
//! A completed upload is ingested like the app's own (`ingest::ingest_file`): dedupe by device and
//! asset id, then by content hash.

use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{ready, Context, Poll};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{middleware, Extension, Json, Router};
use base64::Engine;
use futures_util::StreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

use crate::db::{self, now_str};
use crate::ingest::{self, Library};
use crate::meta::ClientMeta;
use crate::resumable::{self, Dialect, ResumableError};

pub const META_HEADER: &str = "x-lucina-meta";
/// The item a completed upload became.
pub const ITEM_HEADER: &str = "x-lucina-item";
const BASE: &str = "/v1/media/bgupload";

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bg_uploads (
            id TEXT PRIMARY KEY,
            token_hash TEXT NOT NULL,
            filename TEXT NOT NULL,
            meta TEXT NOT NULL,
            length INTEGER,
            offset INTEGER NOT NULL DEFAULT 0,
            complete INTEGER NOT NULL DEFAULT 0,
            dialect TEXT NOT NULL,
            interop TEXT,
            item_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )
}

/// The socket of the connection a request came in on, for the 104.
#[derive(Clone)]
pub struct Informational(Option<Arc<TcpStream>>);

impl Informational {
    /// For tests and routers served without [`serve`]: informational responses are skipped.
    pub fn none() -> Self {
        Informational(None)
    }

    async fn send(&self, head: &str) -> io::Result<()> {
        let Some(s) = &self.0 else { return Ok(()) };
        let mut bytes = head.as_bytes();
        while !bytes.is_empty() {
            s.writable().await?;
            match s.try_write(bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// A TCP stream hyper and the handler share (`&TcpStream` can read and write).
struct Shared(Arc<TcpStream>);

impl AsyncRead for Shared {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            ready!(self.0.poll_read_ready(cx))?;
            match self.0.try_read(buf.initialize_unfilled()) {
                Ok(n) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
}

impl AsyncWrite for Shared {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.0.poll_write_ready(cx))?;
            match self.0.try_write(buf) {
                Ok(n) => return Poll::Ready(Ok(n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    // The socket closes when the last owner (hyper's connection, a finished handler) drops it.
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Serve [`router`] on `addr`, HTTP/1.1 only (p1's Caddy proxies with HTTP/1.1).
pub async fn serve(lib: Library, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "serving background uploads");
    serve_listener(lib, listener).await
}

pub async fn serve_listener(lib: Library, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let app = router(lib);
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let stream = Arc::new(stream);
        let svc = app
            .clone()
            .layer(Extension(Informational(Some(stream.clone()))));
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(Shared(stream));
            let svc = hyper_util::service::TowerToHyperService::new(svc);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await
            {
                tracing::debug!(error = %e, "background upload connection ended");
            }
        });
    }
}

/// The routes, behind the upstream key like the rest of `/v1/media`. Without [`serve`] (tests),
/// add `Extension(Informational::none())`.
pub fn router(lib: Library) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(BASE, any(collection))
        .route(&format!("{BASE}/{{cap}}"), any(resource))
        .with_state(lib.clone())
        .layer(middleware::from_fn_with_state(
            lib,
            crate::http::upstream_guard,
        ))
}

fn reply(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn limit_headers(h: &mut HeaderMap) {
    h.insert("upload-limit", HeaderValue::from(resumable::LIMIT));
}

async fn collection(
    State(lib): State<Library>,
    Extension(info): Extension<Informational>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match method {
        Method::OPTIONS => preflight(),
        Method::POST => create(lib, info, headers, body).await,
        _ => reply(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

fn preflight() -> Response {
    let mut r = StatusCode::OK.into_response();
    limit_headers(r.headers_mut());
    r.headers_mut().insert(
        header::ALLOW,
        HeaderValue::from_static("OPTIONS, POST, HEAD, PATCH, DELETE"),
    );
    r
}

/// Uploads that have a request streaming into them right now: a second one gets a 409.
fn busy() -> &'static Mutex<HashSet<String>> {
    static BUSY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    BUSY.get_or_init(Default::default)
}

struct Claim(String);

impl Claim {
    fn take(id: &str) -> Option<Claim> {
        busy()
            .lock()
            .unwrap()
            .insert(id.to_string())
            .then(|| Claim(id.to_string()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        busy().lock().unwrap().remove(&self.0);
    }
}

#[derive(Debug)]
struct Row {
    state: resumable::State,
    filename: String,
    meta: String,
    dialect: Dialect,
    interop: Option<String>,
    item_id: Option<String>,
}

fn token_hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

fn random_hex() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The upload's absolute URL as the client reached p1 (relative without a Host).
fn location(headers: &HeaderMap, id: &str, token: &str) -> String {
    let path = format!("{BASE}/{id}.{token}");
    let get = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
    };
    let host = get("x-forwarded-host").or_else(|| get("host"));
    match host {
        Some(h)
            if h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b)) =>
        {
            let proto = match get("x-forwarded-proto") {
                Some("http") => "http",
                _ => "https",
            };
            format!("{proto}://{h}{path}")
        }
        _ => path,
    }
}

fn decode_meta(headers: &HeaderMap) -> Result<ClientMeta, &'static str> {
    let Some(v) = headers.get(META_HEADER) else {
        return Ok(ClientMeta::default());
    };
    let raw = v
        .to_str()
        .map_err(|_| "bad X-Lucina-Meta")?
        .trim()
        .trim_end_matches('=');
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "bad X-Lucina-Meta")?;
    serde_json::from_slice(&json).map_err(|_| "bad X-Lucina-Meta")
}

fn filename_ok(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 255
        && !f.contains('/')
        && !f.contains('\0')
        && f != "."
        && f != ".."
}

fn status_of(e: ResumableError) -> StatusCode {
    match e {
        ResumableError::Completed => StatusCode::BAD_REQUEST,
        ResumableError::OffsetMismatch { .. } => StatusCode::CONFLICT,
        ResumableError::LengthConflict => StatusCode::BAD_REQUEST,
        ResumableError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ResumableError::Incomplete { .. } => StatusCode::BAD_REQUEST,
    }
}

fn state_headers(h: &mut HeaderMap, row: &Row) {
    h.insert("upload-offset", HeaderValue::from(row.state.offset));
    if let Some(l) = row.state.length {
        h.insert("upload-length", HeaderValue::from(l));
    }
    let (k, v) = row.dialect.header(row.state.complete);
    h.insert(k, HeaderValue::from_static(v));
    if let Some(i) = row
        .interop
        .as_deref()
        .and_then(|i| HeaderValue::from_str(i).ok())
    {
        h.insert("upload-draft-interop-version", i);
    }
    limit_headers(h);
}

fn error_with_state(e: ResumableError, row: &Row) -> Response {
    let mut r = reply(status_of(e), &e.to_string());
    state_headers(r.headers_mut(), row);
    r
}

async fn create(lib: Library, info: Informational, headers: HeaderMap, body: Body) -> Response {
    let meta = match decode_meta(&headers) {
        Ok(m) => m,
        Err(e) => return reply(StatusCode::BAD_REQUEST, e),
    };
    let filename = meta.filename.clone().unwrap_or_default();
    if !filename_ok(&filename) {
        return reply(StatusCode::BAD_REQUEST, "metadata needs a filename");
    }
    if let Some(d) = &meta.device {
        if !crate::http::device_id_ok(d) {
            return reply(StatusCode::BAD_REQUEST, "bad device id");
        }
    }
    let (complete, dialect) = match resumable::completion(&headers) {
        Ok(c) => c.unwrap_or((true, Dialect::Complete)),
        Err(e) => return reply(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let resumable_client = headers.contains_key("upload-complete")
        || headers.contains_key("upload-incomplete")
        || headers.contains_key("upload-draft-interop-version");
    let (announced, content_length) = match (
        resumable::header_u64(&headers, "upload-length"),
        resumable::header_u64(&headers, "content-length"),
    ) {
        (Ok(a), Ok(c)) => (a, c),
        (Err(e), _) | (_, Err(e)) => return reply(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let start = resumable::State {
        offset: 0,
        length: None,
        complete: false,
    };
    let length = announced.or(if complete { content_length } else { None });
    let state = match resumable::announce_length(start, length)
        .and_then(|s| resumable::check_append(s, 0, content_length).map(|_| s))
    {
        Ok(s) => s,
        Err(e) => return reply(status_of(e), &e.to_string()),
    };
    let id = db::new_id("bgu");
    let token = random_hex();
    let row = Row {
        state,
        filename,
        meta: serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into()),
        dialect,
        interop: resumable::interop_version(&headers),
        item_id: None,
    };
    if let Err(e) = tokio::fs::File::create(lib.cfg.uploads_dir().join(&id)).await {
        tracing::warn!(error = %e, "background upload: no spool file");
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    if let Err(e) = insert(&lib, &id, &token, &row).await {
        tracing::warn!(error = %e, "background upload: insert failed");
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    let url = location(&headers, &id, &token);
    tracing::info!(upload = %id, filename = %row.filename, length = ?row.state.length, complete, "background upload started");
    let Some(claim) = Claim::take(&id) else {
        return reply(StatusCode::CONFLICT, "busy");
    };
    if resumable_client {
        let mut head = format!("HTTP/1.1 104 Upload Resumption Supported\r\nlocation: {url}\r\n");
        if let Some(i) = &row.interop {
            head.push_str(&format!("upload-draft-interop-version: {i}\r\n"));
        }
        head.push_str("\r\n");
        if let Err(e) = info.send(&head).await {
            tracing::info!(upload = %id, error = %e, "could not send 104");
        }
    }
    let res = append_and_maybe_finish(&lib, &id, row, complete, body).await;
    drop(claim);
    let mut r = finish_response(res, StatusCode::CREATED);
    if let Ok(v) = HeaderValue::from_str(&url) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// The outcome of an append: the row after it, and the ingest result when it completed the upload.
type AppendResult =
    Result<(Row, Option<ingest::IngestResult>), (StatusCode, String, Option<Box<Row>>)>;

fn finish_response(res: AppendResult, created: StatusCode) -> Response {
    match res {
        Ok((row, done)) => {
            let status = if done.is_some() || created == StatusCode::CREATED {
                created
            } else {
                StatusCode::NO_CONTENT
            };
            let mut r = match done {
                Some(d) => {
                    let mut r = (
                        status,
                        Json(json!({
                            "item_id": d.item_id, "duplicate": d.duplicate, "version": d.version
                        })),
                    )
                        .into_response();
                    // PhotoKit hands the extension the response headers only, not the body.
                    if let Ok(v) = HeaderValue::from_str(&d.item_id) {
                        r.headers_mut().insert(ITEM_HEADER, v);
                    }
                    r
                }
                None => status.into_response(),
            };
            state_headers(r.headers_mut(), &row);
            r
        }
        Err((status, msg, row)) => {
            let mut r = reply(status, &msg);
            if let Some(row) = row {
                state_headers(r.headers_mut(), &row);
            }
            r
        }
    }
}

/// Stream `body` onto the upload, then ingest it if the request completes it.
async fn append_and_maybe_finish(
    lib: &Library,
    id: &str,
    mut row: Row,
    complete: bool,
    body: Body,
) -> AppendResult {
    let path = lib.cfg.uploads_dir().join(id);
    let internal = |e: &dyn std::fmt::Display, row: &Row| {
        tracing::warn!(upload = %id, error = %e, "background upload failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
            Some(Box::new(clone_row(row))),
        )
    };
    let file = match tokio::fs::OpenOptions::new().write(true).open(&path).await {
        Ok(f) => f,
        Err(e) => return Err(internal(&e, &row)),
    };
    // A cut-off earlier request can leave bytes past the recorded offset.
    if let Err(e) = file.set_len(row.state.offset).await {
        return Err(internal(&e, &row));
    }
    let mut f = tokio::io::BufWriter::with_capacity(1 << 20, file);
    {
        use tokio::io::AsyncSeekExt;
        if let Err(e) = f.seek(io::SeekFrom::Start(row.state.offset)).await {
            return Err(internal(&e, &row));
        }
    }
    let room = resumable::room(row.state);
    let mut written: u64 = 0;
    let mut cut = false;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            cut = true;
            break;
        };
        if written + chunk.len() as u64 > room {
            let _ = f.flush().await;
            row.state.offset += written;
            let _ = save_offset(lib, id, row.state.offset).await;
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "longer than the upload".into(),
                Some(Box::new(row)),
            ));
        }
        if let Err(e) = f.write_all(&chunk).await {
            return Err(internal(&e, &row));
        }
        written += chunk.len() as u64;
    }
    if let Err(e) = f.flush().await {
        return Err(internal(&e, &row));
    }
    if let Err(e) = f.get_mut().sync_data().await {
        return Err(internal(&e, &row));
    }
    row.state.offset += written;
    if let Err(e) = save_offset(lib, id, row.state.offset).await {
        return Err(internal(&e, &row));
    }
    if cut {
        tracing::info!(upload = %id, offset = row.state.offset, "background upload cut off; it can resume");
        return Err((
            StatusCode::BAD_REQUEST,
            "body cut off".into(),
            Some(Box::new(row)),
        ));
    }
    if !complete {
        return Ok((row, None));
    }
    let done = match resumable::complete(row.state) {
        Ok(s) => s,
        Err(e) => return Err((status_of(e), e.to_string(), Some(Box::new(row)))),
    };
    let mut meta: ClientMeta = serde_json::from_str(&row.meta).unwrap_or_default();
    meta.filename = Some(row.filename.clone());
    let l = lib.clone();
    let uid = id.to_string();
    let p2 = path.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<ingest::IngestResult> {
        let res = ingest::ingest_file(&l, &p2, None, &meta, None)?;
        l.db.with(|c| {
            c.execute(
                "UPDATE bg_uploads SET complete = 1, length = ?2, item_id = ?3, updated_at = ?4 WHERE id = ?1",
                params![uid, done.offset as i64, res.item_id, now_str()],
            )
        })?;
        Ok(res)
    })
    .await;
    match res {
        Ok(Ok(r)) => {
            tracing::info!(upload = %id, item = %r.item_id, duplicate = r.duplicate, bytes = done.offset, "background upload ingested");
            row.state = done;
            row.item_id = Some(r.item_id.clone());
            Ok((row, Some(r)))
        }
        Ok(Err(e)) => Err(internal(&e, &row)),
        Err(e) => Err(internal(&e, &row)),
    }
}

fn clone_row(r: &Row) -> Row {
    Row {
        state: r.state,
        filename: r.filename.clone(),
        meta: r.meta.clone(),
        dialect: r.dialect,
        interop: r.interop.clone(),
        item_id: r.item_id.clone(),
    }
}

async fn insert(lib: &Library, id: &str, token: &str, row: &Row) -> anyhow::Result<()> {
    let (id, th, filename, meta, length, dialect, interop) = (
        id.to_string(),
        token_hash(token),
        row.filename.clone(),
        row.meta.clone(),
        row.state.length.map(|l| l as i64),
        row.dialect.as_str(),
        row.interop.clone(),
    );
    lib.db
        .call(move |c| {
            c.execute(
                "INSERT INTO bg_uploads (id, token_hash, filename, meta, length, dialect, interop, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![id, th, filename, meta, length, dialect, interop, now_str()],
            )
        })
        .await?;
    Ok(())
}

async fn save_offset(lib: &Library, id: &str, offset: u64) -> anyhow::Result<()> {
    let id = id.to_string();
    lib.db
        .call(move |c| {
            c.execute(
                "UPDATE bg_uploads SET offset = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, offset as i64, now_str()],
            )
        })
        .await?;
    Ok(())
}

/// The upload a capability names, if its token matches.
async fn load(lib: &Library, cap: &str) -> Option<(String, Row)> {
    let (id, token) = resumable::parse_capability(cap)?;
    let (id, given) = (id.to_string(), token_hash(token));
    let found = lib
        .db
        .call({
            let id = id.clone();
            move |c| {
                c.query_row(
                    "SELECT token_hash, filename, meta, length, offset, complete, dialect, interop, item_id
                     FROM bg_uploads WHERE id = ?1",
                    [&id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            Row {
                                filename: r.get(1)?,
                                meta: r.get(2)?,
                                state: resumable::State {
                                    length: r.get::<_, Option<i64>>(3)?.map(|l| l as u64),
                                    offset: r.get::<_, i64>(4)? as u64,
                                    complete: r.get::<_, i64>(5)? != 0,
                                },
                                dialect: Dialect::parse(&r.get::<_, String>(6)?),
                                interop: r.get(7)?,
                                item_id: r.get(8)?,
                            },
                        ))
                    },
                )
                .optional()
            }
        })
        .await
        .ok()??;
    let (stored, row) = found;
    bool::from(stored.as_bytes().ct_eq(given.as_bytes())).then_some((id, row))
}

async fn resource(
    State(lib): State<Library>,
    Path(cap): Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if method == Method::OPTIONS {
        return preflight();
    }
    let Some((id, row)) = load(&lib, &cap).await else {
        return reply(StatusCode::NOT_FOUND, "not found");
    };
    match method {
        Method::HEAD => {
            let mut r = StatusCode::NO_CONTENT.into_response();
            state_headers(r.headers_mut(), &row);
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Method::PATCH => patch(lib, id, row, headers, body).await,
        Method::DELETE => {
            if row.state.complete {
                return reply(StatusCode::BAD_REQUEST, "upload already complete");
            }
            let Some(_claim) = Claim::take(&id) else {
                return reply(StatusCode::CONFLICT, "busy");
            };
            let _ = tokio::fs::remove_file(lib.cfg.uploads_dir().join(&id)).await;
            let i2 = id.clone();
            let _ = lib
                .db
                .call(move |c| c.execute("DELETE FROM bg_uploads WHERE id = ?1", [&i2]))
                .await;
            tracing::info!(upload = %id, "background upload cancelled");
            StatusCode::NO_CONTENT.into_response()
        }
        _ => reply(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

async fn patch(lib: Library, id: String, mut row: Row, headers: HeaderMap, body: Body) -> Response {
    let (offset, announced, content_length) = match (
        resumable::header_u64(&headers, "upload-offset"),
        resumable::header_u64(&headers, "upload-length"),
        resumable::header_u64(&headers, "content-length"),
    ) {
        (Ok(Some(o)), Ok(a), Ok(c)) => (o, a, c),
        (Ok(None), _, _) => return reply(StatusCode::BAD_REQUEST, "Upload-Offset is required"),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            return reply(StatusCode::BAD_REQUEST, &e.to_string())
        }
    };
    let complete = match resumable::completion(&headers) {
        Ok(Some((c, _))) => c,
        // Not said: complete only when this body brings the last announced byte.
        Ok(None) => match (row.state.length, content_length) {
            (Some(l), Some(c)) => offset.checked_add(c) == Some(l),
            _ => false,
        },
        Err(e) => return reply(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let length = announced.or(if complete {
        content_length.and_then(|c| offset.checked_add(c))
    } else {
        None
    });
    // The offset first: a client resuming from the wrong place gets a 409 telling it where to go on.
    match resumable::check_append(row.state, offset, content_length)
        .and_then(|_| resumable::announce_length(row.state, length))
    {
        Ok(s) => row.state = s,
        Err(e) => return error_with_state(e, &row),
    }
    if let Some(l) = length {
        let lid = id.clone();
        let _ = lib
            .db
            .call(move |c| {
                c.execute(
                    "UPDATE bg_uploads SET length = ?2 WHERE id = ?1",
                    params![lid, l as i64],
                )
            })
            .await;
    }
    let Some(claim) = Claim::take(&id) else {
        return error_with_state(
            ResumableError::OffsetMismatch {
                expected: row.state.offset,
            },
            &row,
        );
    };
    let res = append_and_maybe_finish(&lib, &id, row, complete, body).await;
    drop(claim);
    finish_response(res, StatusCode::OK)
}

/// Drop uploads left unfinished for a week (their spool files too).
pub fn sweep(lib: &Library) -> anyhow::Result<usize> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let stale: Vec<String> = lib.db.with(|c| {
        let mut st =
            c.prepare("SELECT id FROM bg_uploads WHERE complete = 0 AND updated_at < ?1")?;
        let rows = st.query_map([&cutoff], |r| r.get(0))?;
        rows.collect()
    })?;
    for id in &stale {
        let _ = std::fs::remove_file(lib.cfg.uploads_dir().join(id));
        lib.db
            .with(|c| c.execute("DELETE FROM bg_uploads WHERE id = ?1", [id]))?;
    }
    Ok(stale.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_uses_the_host_the_client_reached() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("100.107.164.33:8831"));
        h.insert(
            "x-forwarded-host",
            HeaderValue::from_static("getsecretary.tech"),
        );
        assert_eq!(
            location(&h, "bgu_1", "t"),
            "https://getsecretary.tech/v1/media/bgupload/bgu_1.t"
        );
        let mut bad = HeaderMap::new();
        bad.insert("host", HeaderValue::from_static("evil.example/x?"));
        assert_eq!(location(&bad, "bgu_1", "t"), "/v1/media/bgupload/bgu_1.t");
    }

    #[test]
    fn metadata_comes_base64url_encoded() {
        let meta = ClientMeta {
            filename: Some("IMG_1.HEIC".into()),
            asset_id: Some("A/L0/001".into()),
            ..Default::default()
        };
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&meta).unwrap());
        let mut h = HeaderMap::new();
        h.insert(META_HEADER, HeaderValue::from_str(&enc).unwrap());
        assert_eq!(decode_meta(&h), Ok(meta));
        h.insert(META_HEADER, HeaderValue::from_static("%%%"));
        assert!(decode_meta(&h).is_err());
        assert!(!filename_ok("../x"));
        assert!(filename_ok("IMG_1.HEIC"));
    }
}
