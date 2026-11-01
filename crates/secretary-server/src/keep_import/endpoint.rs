//! `POST /v1/notes/import/google-keep[?dryRun=1]`: the Takeout zip as the raw
//! body, streamed to a 0600 temp file under `$SECRETARY_DATA_DIR/tmp/` (a
//! zip is never held in memory), imported on a blocking thread, deleted
//! afterwards. Devices only (the service token gets 403); one import at a
//! time per data dir (409 `import_running`).

use std::collections::HashSet;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::{import_takeout_labelled, ImportReport};
use crate::error::{ApiError, QueryArgs};
use crate::notes::Caller;
use crate::AppState;

/// The zip body cap.
pub const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    #[serde(default, rename = "dryRun")]
    dry_run: Option<String>,
    /// A label added to every imported note (e.g. the account it came from).
    #[serde(default)]
    label: Option<String>,
}

fn running() -> &'static Mutex<HashSet<PathBuf>> {
    static R: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Holds "an import is running here" until dropped.
struct RunGuard(PathBuf);

impl RunGuard {
    fn take(dir: &Path) -> Option<Self> {
        let mut set = running().lock().unwrap_or_else(|p| p.into_inner());
        set.insert(dir.to_owned()).then(|| Self(dir.to_owned()))
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// The temp zip; removed when dropped, whatever happened.
struct TempZip(PathBuf);

impl Drop for TempZip {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn is_dry_run(q: &ImportQuery) -> bool {
    match q.dry_run.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        None => false,
        Some(s) => !matches!(s.as_str(), "0" | "false" | "no" | "off"),
    }
}

fn too_big() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        "the zip is over 2 GB",
    )
}

fn bad(code: &str, msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, msg)
}

fn io_err(what: &str, e: std::io::Error) -> ApiError {
    tracing::error!(error = %e, "keep import: {what}");
    ApiError::internal("could not store the upload")
}

pub async fn import_google_keep(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    QueryArgs(q): QueryArgs<ImportQuery>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Result<Json<ImportReport>, ApiError> {
    if caller.0 == "lucina" {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the Google Keep import is for the owner's devices only",
        ));
    }
    let dry_run = is_dry_run(&q);
    let label = q
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    if label
        .as_ref()
        .is_some_and(|l| l.chars().count() > 50 || l.chars().any(|c| c.is_control() || c == ','))
    {
        return Err(bad(
            "invalid_label",
            "label: up to 50 characters, no commas",
        ));
    }
    if headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_ZIP_BYTES)
    {
        return Err(too_big());
    }

    let store = st.notes.clone();
    let data_dir = store
        .dir()
        .parent()
        .map(Path::to_owned)
        .ok_or_else(|| ApiError::internal("no data dir"))?;
    let run = RunGuard::take(&data_dir).ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "import_running",
            "another Google Keep import is running; try again when it is done",
        )
    })?;

    // Temp file: 0600 in a 0700 directory; leftovers of a crashed import go.
    let tmp_dir = data_dir.join("tmp");
    crate::devices::ensure_dir(&tmp_dir).map_err(|e| io_err("tmp dir", e))?;
    if let Ok(rd) = std::fs::read_dir(&tmp_dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.starts_with("keep-import-") && n.ends_with(".zip") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|e| ApiError::internal(&format!("random: {e}")))?;
    let name: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let path = tmp_dir.join(format!("keep-import-{name}.zip"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| io_err("create", e))?;
    let tmp = TempZip(path.clone());
    let mut file = tokio::fs::File::from_std(file);

    let mut total: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| bad("upload_failed", format!("reading the body: {e}")))?;
        total += chunk.len() as u64;
        if total > MAX_ZIP_BYTES {
            return Err(too_big());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| io_err("write", e))?;
    }
    file.flush().await.map_err(|e| io_err("flush", e))?;
    drop(file);
    if total == 0 {
        return Err(bad(
            "empty_body",
            "send the Takeout zip as the request body",
        ));
    }

    // The guards move into the blocking task: the lock and the file last as
    // long as the import does, even if the client hangs up.
    let result = tokio::task::spawn_blocking(move || {
        let _run = run;
        let tmp = tmp;
        import_takeout_labelled(
            &store,
            std::slice::from_ref(&tmp.0),
            dry_run,
            caller,
            label.as_deref(),
        )
    })
    .await
    .map_err(|_| ApiError::internal("import task failed"))?;
    match result {
        Ok(rep) => {
            tracing::info!(
                by = caller.0,
                dry_run,
                notes = rep.notes.total,
                new = rep.notes.new,
                updated = rep.notes.updated,
                errors = rep.errors.len(),
                "google keep import"
            );
            Ok(Json(rep))
        }
        Err(msg) => {
            tracing::warn!(error = %msg, "google keep import: unreadable zip");
            Err(bad("invalid_zip", "the body is not a readable zip file"))
        }
    }
}
