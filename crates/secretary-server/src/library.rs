//! Library: authenticated, local JSON records backed by ink-library's pure merge.
//! Kindle imports never delete data or infer reading time. Writes are serialized,
//! atomic (0600), and recoverable through a write-ahead snapshot. Logs omit content.
use crate::{
    devices::{ensure_dir, save_json_atomic},
    error::{ApiError, JsonBody, QueryArgs},
    AppState,
};
use axum::{
    extract::{DefaultBodyLimit, OriginalUri, Path as UrlPath, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Extension, Json, Router,
};
use ink_library::{
    import::{merge, ImportBatch, ImportOutcome},
    model::*,
    search,
};
use ink_sync::{
    state::{self as sync_state, SyncState},
    transport::{check_request, serve, SyncRequest, SyncResponse},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const BODY_LIMIT: usize = 32 * 1024 * 1024;
pub const MAX_BOOKS: usize = 50_000;
pub const MAX_HIGHLIGHTS: usize = 200_000;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_LIST_LIMIT: usize = 500;
pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_TITLE_CHARS: usize = 1000;
pub const MAX_LOG_READ_BYTES: u64 = 1024 * 1024;
pub const MAX_BRIEF_HIGHLIGHTS: usize = 10;
fn bad(s: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", s)
}
fn io_err(_: std::io::Error) -> ApiError {
    ApiError::internal("library storage failed")
}
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}
pub fn valid_id(s: &str) -> bool {
    s.len() == 19
        && s.starts_with("bk_")
        && s.as_bytes()[3..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}
fn text(s: &str, max: usize) -> bool {
    s.len() <= max
        && !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}
fn safe_relative(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 4096
        && !s.contains(['\0', '\\'])
        && !Path::new(s).is_absolute()
        && Path::new(s)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
fn validate(batch: &ImportBatch) -> Result<(), ApiError> {
    if !crate::notes::valid_source(&batch.source)
        || batch.device.as_ref().is_some_and(|d| !text(d, 100))
        || chrono::DateTime::parse_from_rfc3339(&batch.scanned_at).is_err()
        || batch.books.len() > MAX_BOOKS
        || batch.clippings.len() > MAX_HIGHLIGHTS
    {
        return Err(bad(
            "invalid source, device label, scannedAt or batch count",
        ));
    }
    for b in &batch.books {
        let m = &b.metadata;
        if !safe_relative(&b.path)
            || b.fingerprint.len() != 64
            || !b
                .fingerprint
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || m.title
                .as_ref()
                .is_none_or(|t| t.trim().is_empty() || !text(t, MAX_TITLE_CHARS))
            || m.authors.len() > 100
            || m.authors.iter().any(|s| !text(s, MAX_TITLE_CHARS))
            || [&m.isbn, &m.asin, &m.publisher, &m.language, &m.cdetype]
                .iter()
                .any(|s| s.as_ref().is_some_and(|s| !text(s, MAX_TITLE_CHARS)))
        {
            return Err(bad("invalid book metadata, fingerprint or path"));
        }
    }
    for e in &batch.clippings {
        let h = &e.highlight;
        if e.title.trim().is_empty()
            || !text(&e.title, MAX_TITLE_CHARS)
            || e.authors.len() > 100
            || e.authors.iter().any(|a| !text(a, MAX_TITLE_CHARS))
            || !text(&h.text, MAX_TEXT_BYTES)
            || h.note.as_ref().is_some_and(|n| !text(n, MAX_TEXT_BYTES))
            || !text(&h.added_at_raw, 1000)
            || !safe_relative(&h.provenance.file)
            || !crate::notes::valid_source(&h.source)
            || h.location.as_ref().is_some_and(|l| l.end < l.start)
            || h.page.as_ref().is_some_and(|s| !text(s, 100))
        {
            return Err(bad("invalid clipping"));
        }
    }
    Ok(())
}
/// Records plus the device-sync registers (`sync.json`, the source of truth for
/// device-originated data; [`sync_state::project`] writes it into the records).
struct Loaded {
    lib: LibraryState,
    sync: SyncState,
}
/// Lazily loaded records and a write-ahead snapshot for crash recovery across files.
pub struct LibraryStore {
    dir: PathBuf,
    cache: Mutex<Option<Loaded>>,
}
pub fn store(data: &Path) -> Arc<LibraryStore> {
    Arc::new(LibraryStore {
        dir: data.join("library"),
        cache: Mutex::new(None),
    })
}
impl LibraryStore {
    fn project(&self, state: &LibraryState) -> Result<(), ApiError> {
        ensure_dir(&self.dir.join("books")).map_err(io_err)?;
        ensure_dir(&self.dir.join("highlights")).map_err(io_err)?;
        for b in state.books.values() {
            save_json_atomic(&self.dir.join("books"), &format!("{}.json", b.id), b)
                .map_err(io_err)?;
            save_json_atomic(
                &self.dir.join("highlights"),
                &format!("{}.json", b.id),
                &state.highlights.get(&b.id).cloned().unwrap_or_default(),
            )
            .map_err(io_err)?;
        }
        save_json_atomic(
            &self.dir,
            "activity.json",
            &json!({"sessions":state.sessions,"changes":state.changes}),
        )
        .map_err(io_err)?;
        Ok(())
    }
    /// Records, then the sync registers projected onto them: a crash between
    /// writing `sync.json` and the records is healed here.
    fn load(&self) -> Result<Loaded, ApiError> {
        let mut lib = self.load_records()?;
        let path = self.dir.join("sync.json");
        let sync: SyncState = if path.exists() {
            serde_json::from_reader(fs::File::open(&path).map_err(io_err)?)
                .map_err(|_| ApiError::internal("invalid library sync state"))?
        } else {
            SyncState::default()
        };
        sync_state::project(&sync, &mut lib, None, &timestamp());
        Ok(Loaded { lib, sync })
    }
    fn load_records(&self) -> Result<LibraryState, ApiError> {
        let pending = self.dir.join("pending.json");
        if pending.exists() {
            let s: LibraryState =
                serde_json::from_reader(fs::File::open(&pending).map_err(io_err)?)
                    .map_err(|_| ApiError::internal("invalid library recovery snapshot"))?;
            self.project(&s)?;
            fs::remove_file(pending).map_err(io_err)?;
            return Ok(s);
        }
        let mut s = LibraryState::default();
        let activity = self.dir.join("activity.json");
        if activity.exists() {
            #[derive(Deserialize)]
            struct Activity {
                sessions: Vec<ReadingSession>,
                changes: Vec<ReadingChange>,
            }
            let a: Activity = serde_json::from_reader(fs::File::open(activity).map_err(io_err)?)
                .map_err(|_| ApiError::internal("invalid library activity"))?;
            s.sessions = a.sessions;
            s.changes = a.changes;
        }
        let dir = self.dir.join("books");
        if !dir.exists() {
            return Ok(s);
        }
        for e in fs::read_dir(dir).map_err(io_err)? {
            let p = e.map_err(io_err)?.path();
            let Some(id) = p
                .file_stem()
                .and_then(|n| n.to_str())
                .filter(|s| valid_id(s))
            else {
                continue;
            };
            if p.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let b: Book = serde_json::from_reader(fs::File::open(&p).map_err(io_err)?)
                .map_err(|_| ApiError::internal("invalid library book"))?;
            if b.id != id {
                return Err(ApiError::internal("library id mismatch"));
            }
            s.books.insert(id.into(), b);
            let hp = self.dir.join("highlights").join(format!("{id}.json"));
            if hp.exists() {
                let h = serde_json::from_reader(fs::File::open(hp).map_err(io_err)?)
                    .map_err(|_| ApiError::internal("invalid library highlights"))?;
                s.highlights.insert(id.into(), h);
            }
        }
        Ok(s)
    }
    fn with<R>(
        &self,
        f: impl FnOnce(&Self, &mut LibraryState) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        self.with_sync(|st, lib, _| f(st, lib))
    }
    /// Like `with`, plus the sync registers. After a failed write the cache is
    /// dropped, so the next request reloads what reached the disk.
    fn with_sync<R>(
        &self,
        f: impl FnOnce(&Self, &mut LibraryState, &mut SyncState) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        let mut lock = self
            .cache
            .lock()
            .map_err(|_| ApiError::internal("library lock poisoned"))?;
        if lock.is_none() {
            *lock = Some(self.load()?);
        }
        let loaded = lock
            .as_mut()
            .ok_or_else(|| ApiError::internal("library cache unavailable"))?;
        let result = f(self, &mut loaded.lib, &mut loaded.sync);
        if result.is_err()
            && (self.dir.join("pending.json").exists() || self.dir.join(".sync.json.tmp").exists())
        {
            *lock = None;
        }
        result
    }
    fn persist_sync(&self, s: &SyncState) -> Result<(), ApiError> {
        ensure_dir(&self.dir).map_err(io_err)?;
        save_json_atomic(&self.dir, "sync.json", s).map_err(io_err)
    }
    /// `POST /v1/library/sync`: apply the ops (pure, idempotent), then write
    /// `sync.json` before the records it projects onto.
    fn sync(&self, req: &SyncRequest) -> Result<SyncResponse, ApiError> {
        self.with_sync(|st, lib, sync| {
            let mut next_sync = sync.clone();
            let resp = serve(&mut next_sync, req).map_err(|e| bad(&e))?;
            if next_sync.clock == sync.clock {
                return Ok(resp); // only duplicates or deferred ops
            }
            let books: std::collections::BTreeSet<String> = req
                .ops
                .iter()
                .map(|o| o.kind.book_id().to_owned())
                .collect();
            let mut next = lib.clone();
            let changed = sync_state::project(&next_sync, &mut next, Some(&books), &timestamp());
            st.persist_sync(&next_sync)?;
            *sync = next_sync;
            if changed {
                st.persist(&next)?;
                *lib = next;
            }
            Ok(resp)
        })
    }
    /// The owner confirms a device's deletion request.
    fn confirm_tombstone(&self, op_id: &str) -> Result<Value, ApiError> {
        self.with_sync(|st, lib, sync| {
            let mut next_sync = sync.clone();
            let t = sync_state::confirm_tombstone(&mut next_sync, op_id).ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "tombstone_not_found",
                    "no such deletion request",
                )
            })?;
            let mut next = lib.clone();
            let books = [t.book_id.clone()].into_iter().collect();
            let changed = sync_state::project(&next_sync, &mut next, Some(&books), &timestamp());
            st.persist_sync(&next_sync)?;
            *sync = next_sync;
            if changed {
                st.persist(&next)?;
                *lib = next;
            }
            Ok(json!({"tombstone":t}))
        })
    }
    fn persist(&self, s: &LibraryState) -> Result<(), ApiError> {
        ensure_dir(&self.dir).map_err(io_err)?;
        save_json_atomic(&self.dir, "pending.json", s).map_err(io_err)?;
        self.project(s)?;
        fs::remove_file(self.dir.join("pending.json")).map_err(io_err)?;
        Ok(())
    }
    fn logs(&self) -> Result<Vec<Value>, ApiError> {
        let p = self.dir.join("imports.jsonl");
        if !p.exists() {
            return Ok(vec![]);
        }
        let mut f = fs::File::open(p).map_err(io_err)?;
        let len = f.metadata().map_err(io_err)?.len();
        let start = len.saturating_sub(MAX_LOG_READ_BYTES);
        f.seek(SeekFrom::Start(start)).map_err(io_err)?;
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(io_err)?;
        let mut lines: Vec<_> = s
            .lines()
            .skip(usize::from(start > 0))
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if lines.len() > 20 {
            lines.drain(..lines.len() - 20);
        }
        Ok(lines)
    }
    fn import(&self, batch: ImportBatch, caller: &str) -> Result<ImportOutcome, ApiError> {
        validate(&batch)?;
        self.with_sync(|st,s,sync|{let now=timestamp();let (mut next,o)=merge(s,&batch,&now);sync_state::project(sync,&mut next,None,&now);if next.books.len()>MAX_BOOKS||next.highlights.values().map(Vec::len).sum::<usize>()>MAX_HIGHLIGHTS{return Err(bad("library capacity exceeded"));}st.persist(&next)?;*s=next;
        let mut options=OpenOptions::new();options.create(true).append(true);#[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}let mut f=options.open(st.dir.join("imports.jsonl")).map_err(io_err)?;
        let log=json!({"time":timestamp(),"caller":caller,"source":batch.source,"booksAdded":o.books_added,"booksUpdated":o.books_updated,"booksUnchanged":o.books_unchanged,"highlightsAdded":o.highlights_added,"highlightsSkipped":o.highlights_skipped,"removedFromKindle":o.removed_from_kindle,"conflictCount":o.conflicts.len(),"warningCount":o.warnings.len()});writeln!(f,"{log}").map_err(io_err)?;f.sync_all().map_err(io_err)?;Ok(o)})
    }
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Query {
    kind: Option<String>,
    q: Option<String>,
    status: Option<ReadingStatus>,
    on_kindle: Option<bool>,
    limit: Option<usize>,
    days: Option<i64>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Patch {
    status: Option<ReadingStatus>,
    tags: Option<Vec<String>>,
    progress: Option<Progress>,
    base_updated_at: Option<String>,
}
#[derive(Clone, Copy)]
struct Caller(&'static str);
async fn auth(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if crate::service::service_bearer(req.headers()).is_some() {
        req.extensions_mut().insert(Caller("lucina"));
        return crate::service::require_service(State(st), req, next).await;
    }
    req.extensions_mut().insert(Caller("device"));
    crate::auth::require_device(State(st), req, next).await
}
async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> Result<R, ApiError> + Send + 'static,
) -> Result<Json<R>, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("library task failed"))?
        .map(Json)
}
fn book<'a>(s: &'a LibraryState, id: &str) -> Result<&'a Book, ApiError> {
    s.books
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "book_not_found", "no such book"))
}
fn summary(b: &Book, hs: &[Highlight]) -> Value {
    json!({"id":b.id,"kind":b.kind,"title":b.title,"authors":b.authors,"status":b.status,"progress":b.progress,"onKindle":b.on_kindle,"owned":b.owned,"formats":b.formats,"drm":b.drm,"highlightCount":hs.len(),"lastHighlightAt":hs.iter().filter_map(|h|h.added_at.as_ref()).max(),"updatedAt":b.updated_at})
}
fn recent_time(s: &str, cutoff: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| t >= cutoff)
        .unwrap_or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .is_ok_and(|t| t >= cutoff.naive_utc())
        })
}
fn kind_filter(raw: Option<&str>) -> Result<Vec<BookKind>, ApiError> {
    raw.unwrap_or("book,document")
        .split(',')
        .map(|s| match s.trim() {
            "book" => Ok(BookKind::Book),
            "dictionary" => Ok(BookKind::Dictionary),
            "document" => Ok(BookKind::Document),
            _ => Err(bad(
                "kind must contain book, dictionary or document (comma-separated)",
            )),
        })
        .collect()
}
fn list_books(s: &LibraryState, q: &Query, limit: usize) -> Result<Value, ApiError> {
    let kinds = kind_filter(q.kind.as_deref())?;
    let ids: std::collections::BTreeSet<_> =
        search::search(s, q.q.as_deref().unwrap_or(""), usize::MAX)
            .into_iter()
            .map(|r| r.book_id)
            .collect();
    let mut books: Vec<_> = s
        .books
        .values()
        .filter(|b| {
            kinds.contains(&b.kind)
                && ids.contains(&b.id)
                && q.status.is_none_or(|v| b.status == v)
                && q.on_kindle.is_none_or(|v| b.on_kindle.is_some() == v)
        })
        .collect();
    books.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    Ok(json!({"books": books.into_iter().take(limit).map(|b| {
        summary(b, s.highlights.get(&b.id).map(Vec::as_slice).unwrap_or(&[]))
    }).collect::<Vec<_>>()}))
}
fn stats(s: &LibraryState, last_import: Option<Value>) -> Value {
    fn count<T: serde::Serialize>(map: &mut std::collections::BTreeMap<String, usize>, v: T) {
        let key = serde_json::to_value(v)
            .unwrap_or_default()
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        *map.entry(key).or_default() += 1;
    }
    let mut statuses = std::collections::BTreeMap::new();
    let mut formats = std::collections::BTreeMap::new();
    let mut drm = std::collections::BTreeMap::new();
    for b in s.books.values().filter(|b| b.kind != BookKind::Dictionary) {
        count(&mut statuses, b.status);
        count(&mut drm, b.drm);
        for f in &b.formats {
            count(&mut formats, f);
        }
    }
    json!({"books":s.books.values().filter(|b|b.kind!=BookKind::Dictionary).count(),
        "dictionaries":s.books.values().filter(|b|b.kind==BookKind::Dictionary).count(),
        "byStatus":statuses,"byFormat":formats,"byDrm":drm,
        "highlights":s.highlights.values().map(Vec::len).sum::<usize>(),
        "booksOnKindle":s.books.values().filter(|b|b.kind!=BookKind::Dictionary&&b.on_kindle.is_some()).count(),
        "lastImport":last_import})
}
fn recent(s: &LibraryState, days: i64, limit: usize) -> Value {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let mut highlights: Vec<_> = s
        .highlights
        .values()
        .flatten()
        .filter(|h| h.added_at.as_ref().is_some_and(|t| recent_time(t, cutoff)))
        .collect();
    highlights.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.id.cmp(&b.id)));
    highlights.truncate(limit);
    let changes: Vec<_> = s
        .changes
        .iter()
        .rev()
        .filter(|c| recent_time(&c.at, cutoff))
        .take(limit)
        .collect();
    let sessions: Vec<_> = s
        .sessions
        .iter()
        .filter(|c| recent_time(&c.started_at, cutoff))
        .take(limit)
        .collect();
    json!({"highlights":highlights,"changes":changes,"sessions":sessions,
        "readingSessions":if s.sessions.is_empty(){"unavailable_on_stock_kindle"}else{"available"},
        "unknownDateHighlightsExcluded":true,"kindleDatesHaveNoTimezone":true})
}
async fn read(
    State(st): State<AppState>,
    OriginalUri(uri): OriginalUri,
    QueryArgs(q): QueryArgs<Query>,
) -> Result<Json<Value>, ApiError> {
    if q.q
        .as_ref()
        .is_some_and(|s| s.chars().count() > MAX_QUERY_CHARS)
        || q.days.is_some_and(|n| !(1..=3650).contains(&n))
    {
        return Err(bad("invalid query length or days (1-3650)"));
    }
    let path = uri.path().to_owned();
    let store = st.library.clone();
    blocking(move || {
        store.with_sync(|st, s, sync| {
            let limit = q.limit.unwrap_or(200).clamp(1, MAX_LIST_LIMIT);
            let query = q.q.as_deref().unwrap_or("");
            let tail = path.trim_start_matches("/v1/library/");
            match tail {
                "books" => list_books(s, &q, limit),
                "search" => {
                    let kinds = q
                        .kind
                        .as_deref()
                        .map(|k| kind_filter(Some(k)))
                        .transpose()?;
                    let results: Vec<_> = search::search(s, query, usize::MAX)
                        .into_iter()
                        .filter(|r| {
                            kinds.as_ref().is_none_or(|kinds| {
                                s.books
                                    .get(&r.book_id)
                                    .is_some_and(|b| kinds.contains(&b.kind))
                            })
                        })
                        .take(limit)
                        .collect();
                    Ok(json!({"results":results}))
                }
                "sync" => Ok(json!({"imports":st.logs()?,"books":s.books.len(),
                "highlights":s.highlights.values().map(Vec::len).sum::<usize>(),
                "serverClock":sync.clock,
                "devices":sync.devices.iter().map(|(d,w)|(d.clone(),w.contiguous)).collect::<std::collections::BTreeMap<_,_>>(),
                "pendingTombstones":sync.pending_tombstones().collect::<Vec<_>>(),
                "recentConflicts":sync.conflicts.iter().rev().take(20).collect::<Vec<_>>()})),
                "stats" => Ok(stats(s, st.logs()?.pop())),
                "recent" => Ok(recent(s, q.days.unwrap_or(7), limit)),
                _ => {
                    let parts: Vec<_> = tail.split('/').collect();
                    let id = parts.get(1).copied().unwrap_or("");
                    let b = book(s, id)?;
                    let hs = s.highlights.get(id).cloned().unwrap_or_default();
                    if parts.get(2) == Some(&"highlights") {
                        let highlights: Vec<_> = hs
                            .into_iter()
                            .filter(|h| {
                                search::matches(
                                    &format!("{} {}", h.text, h.note.as_deref().unwrap_or("")),
                                    query,
                                )
                            })
                            .take(limit)
                            .collect();
                        Ok(json!({"highlights":highlights}))
                    } else {
                        Ok(json!({"book":b,"highlights":hs,
                        "sessions":s.sessions.iter().filter(|x|x.book_id==id).collect::<Vec<_>>()}))
                    }
                }
            }
        })
    })
    .await
}
async fn import_batch(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(batch): JsonBody<ImportBatch>,
) -> Result<Json<ImportOutcome>, ApiError> {
    let store = st.library.clone();
    let result = blocking(move || store.import(batch, caller.0)).await?;
    tracing::info!(by = caller.0, "library import");
    Ok(result)
}
async fn patch(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Extension(caller): Extension<Caller>,
    JsonBody(req): JsonBody<Patch>,
) -> Result<Json<Book>, ApiError> {
    let tags = req
        .tags
        .as_deref()
        .map(crate::notes::normalize_tags)
        .transpose()?;
    if req.progress.as_ref().is_some_and(|p| {
        p.percent
            .is_some_and(|n| !n.is_finite() || !(0.0..=100.0).contains(&n))
    }) {
        return Err(bad("progress percent must be 0-100"));
    }
    let store = st.library.clone();
    let result = blocking(move || {
        store.with_sync(|st, s, sync| {
            let mut b = book(s, &id)?.clone();
            if req
                .base_updated_at
                .as_ref()
                .is_some_and(|base| *base != b.updated_at)
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "conflict",
                    "book changed; reload before editing",
                ));
            }
            // Owner edits set the sync floors: older device ops lose, and an
            // owner `completed` is never reverted by a device.
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut next_sync = sync.clone();
            if let Some(v) = req.status {
                b.status = v;
                b.status_source = Some("owner".into());
                sync_state::owner_set_status(&mut next_sync, &id, v, now_ms);
            }
            if let Some(v) = tags {
                b.tags = v;
            }
            if let Some(mut v) = req.progress {
                v.source = "owner".into();
                v.at = timestamp();
                b.progress = Some(v);
                sync_state::owner_set_progress(&mut next_sync, &id, now_ms);
            }
            b.updated_at = timestamp();
            if next_sync != *sync {
                st.persist_sync(&next_sync)?;
                *sync = next_sync;
            }
            let mut next = s.clone();
            if req.status.is_some() || b.progress != book(s, &id)?.progress {
                next.changes.push(ReadingChange {
                    book_id: id.clone(),
                    status: req.status,
                    progress: b.progress.clone(),
                    at: b.updated_at.clone(),
                    source: "owner".into(),
                });
            }
            next.books.insert(id, b.clone());
            st.persist(&next)?;
            *s = next;
            Ok(b)
        })
    })
    .await?;
    tracing::info!(by = caller.0, "library owner edit");
    Ok(result)
}
async fn sync_ops(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(req): JsonBody<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    check_request(&req).map_err(|e| bad(&e))?;
    let store = st.library.clone();
    let ops = req.ops.len();
    let result = blocking(move || store.sync(&req)).await?;
    tracing::info!(
        by = caller.0,
        ops,
        acked = result.0.acked.len(),
        conflicts = result.0.conflicts.len(),
        "library sync"
    );
    Ok(result)
}
async fn confirm_tombstone(
    State(st): State<AppState>,
    UrlPath(op_id): UrlPath<String>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Value>, ApiError> {
    // Deletions need the owner: Lucina's service token cannot confirm them.
    if caller.0 != "device" {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_required",
            "only the owner's device can confirm a deletion",
        ));
    }
    let store = st.library.clone();
    let result = blocking(move || store.confirm_tombstone(&op_id)).await?;
    tracing::info!(by = caller.0, "library deletion confirmed");
    Ok(result)
}
async fn brief(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Brief>, ApiError> {
    let store = st.library.clone();
    let result = blocking(move || {
        store.with(|st, s| {
            book(s, &id)?;
            let mut hs: Vec<_> = s
                .highlights
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|h| h.kind == HighlightKind::Highlight)
                .collect();
            hs.sort_by_key(|h| {
                (
                    h.location.as_ref().map(|l| l.start).unwrap_or(u32::MAX),
                    h.id.clone(),
                )
            });
            hs.truncate(MAX_BRIEF_HIGHLIGHTS);
            let ids: Vec<_> = hs.iter().map(|h| h.id.clone()).collect();
            let text = hs
                .iter()
                .map(|h| format!("{} [{}]", h.text, h.id))
                .collect::<Vec<_>>()
                .join("\n\n");
            let brief = Brief {
                id: ink_library::import::stable_id("br_", &ids.join("|")),
                kind: BriefKind::Extractive,
                text,
                generated_at: timestamp(),
                model: None,
                based_on: ids,
            };
            let mut next = s.clone();
            let b = next.books.get_mut(&id).ok_or_else(ApiError::not_found)?;
            if let Some(old) = b.briefs.iter().find(|b| b.id == brief.id) {
                return Ok(old.clone());
            }
            b.briefs.push(brief.clone());
            b.updated_at = timestamp();
            st.persist(&next)?;
            *s = next;
            Ok(brief)
        })
    })
    .await?;
    tracing::info!(by = caller.0, "library generated extractive brief");
    Ok(result)
}
pub fn routes(st: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/library/books", get(read))
        .route("/v1/library/books/{id}", get(read).patch(patch))
        .route("/v1/library/books/{id}/highlights", get(read))
        .route("/v1/library/books/{id}/brief", post(brief))
        .route("/v1/library/search", get(read))
        .route("/v1/library/recent", get(read))
        .route("/v1/library/stats", get(read))
        .route("/v1/library/sync", get(read).post(sync_ops))
        .route(
            "/v1/library/sync/tombstones/{op_id}/confirm",
            post(confirm_tombstone),
        )
        .route("/v1/library/import", post(import_batch))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .route_layer(middleware::from_fn_with_state(st, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_projection_recovers_before_serving_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let batch = ImportBatch {
            source: "kindle-usb".into(),
            device: None,
            scanned_at: timestamp(),
            books: vec![],
            clippings: ink_library::clippings::parse(
                b"Book (Writer)\nunknown\n\nQuoted text\n==========\n",
            )
            .unwrap()
            .entries,
        };
        let (state, _) = merge(&LibraryState::default(), &batch, &timestamp());
        ensure_dir(&store.dir).unwrap();
        save_json_atomic(&store.dir, "pending.json", &state).unwrap();
        // A failed projection must leave the snapshot available to replay.
        fs::write(store.dir.join("highlights"), b"blocked directory").unwrap();
        assert!(store.with(|_, s| Ok(s.clone())).is_err());
        assert!(store.dir.join("pending.json").exists());
        fs::remove_file(store.dir.join("highlights")).unwrap();
        assert_eq!(store.with(|_, s| Ok(s.clone())).unwrap(), state);
        assert!(!store.dir.join("pending.json").exists());
        assert_eq!(
            super::store(dir.path()).with(|_, s| Ok(s.clone())).unwrap(),
            state
        );
    }
}
