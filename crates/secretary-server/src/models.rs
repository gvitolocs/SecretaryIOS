//! Models: the owner's private 3D gallery (Meshy models Lucina made, with
//! their prompt and metadata), docs/server-api.md "Models".
//!
//! - `GET /v1/models?limit=&q=` → `{models: [Model]}`, newest first; `q`
//!   looks in the title, prompt and animation names.
//! - `GET /v1/models/{id}` → `{model}`; `DELETE /v1/models/{id}` → `{ok, id}`
//!   (the model and its files are removed).
//! - `GET /v1/models/{id}/files/{name}` → the file (`model.glb`,
//!   `thumb.png|jpg|webp`, `preview.mp4`, `model.fbx|usdz|obj|stl|3mf`),
//!   `Cache-Control: private`, `ETag`, byte ranges.
//! - Lucina only (service token): `POST /v1/models` `{title, prompt?, source,
//!   jobId?, credits?, animations?, clips?, formats?, parent?, meshy?}` → 201
//!   `{model}`, and `PUT /v1/models/{id}/files/{name}` with the raw bytes
//!   (checked against the format's magic bytes) → `{model}`.
//!
//! Each model is a directory `$SECRETARY_DATA_DIR/models/<id>/` (0700) with
//! `model.json` and its files (0600). Who may read or delete: a device
//! (Bearer, or the cookie plus `X-Secretary-CSRF: 1` for the delete) or
//! Lucina. Files are never served without auth and never from another path.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::devices::{ensure_dir, save_json_atomic};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

pub const MODELS_DIR: &str = "models";
pub const META_FILE: &str = "model.json";
pub const MAX_MODELS: usize = 500;
pub const MAX_LIST_LIMIT: usize = 100;
/// The largest file (an 8K-texture GLB stays well below).
pub const MAX_FILE_BYTES: usize = 200 * 1024 * 1024;
const MAX_SMALL_FILE: usize = 20 * 1024 * 1024;
const MAX_META_JSON: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Caller(pub &'static str);

/// `mdl_` + 8-40 lowercase letters/digits.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("mdl_").is_some_and(|r| {
        (8..=40).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// Content type for an allowed file name; `None` refuses it.
pub fn content_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "model.glb" => "model/gltf-binary",
        "thumb.png" => "image/png",
        "thumb.jpg" => "image/jpeg",
        "thumb.webp" => "image/webp",
        "preview.mp4" => "video/mp4",
        "model.usdz" => "model/vnd.usdz+zip",
        "model.stl" => "model/stl",
        "model.fbx" | "model.obj" | "model.3mf" => "application/octet-stream",
        _ => return None,
    })
}

fn max_bytes(name: &str) -> usize {
    if name.starts_with("thumb.") || name == "preview.mp4" {
        MAX_SMALL_FILE
    } else {
        MAX_FILE_BYTES
    }
}

/// The first bytes must match the format (a GLB is `glTF`, a PNG is a PNG, …).
pub fn magic_ok(name: &str, head: &[u8]) -> bool {
    match name {
        "model.glb" => head.starts_with(b"glTF"),
        "thumb.png" => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        "thumb.jpg" => head.starts_with(&[0xff, 0xd8, 0xff]),
        "thumb.webp" => head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP",
        "preview.mp4" => head.len() >= 8 && &head[4..8] == b"ftyp",
        "model.usdz" | "model.3mf" => head.starts_with(b"PK\x03\x04"),
        _ => !head.is_empty(),
    }
}

pub struct ModelStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

fn now_iso() -> String {
    crate::links::now_iso()
}

fn random_id() -> String {
    let mut b = [0u8; 10];
    let _ = getrandom::fill(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("mdl_{hex}")
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn text(v: &Value, field: &str, max: usize, required: bool) -> Result<Option<String>, ApiError> {
    match v {
        Value::Null if !required => Ok(None),
        Value::String(s) => {
            let t: String = s
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .collect();
            let t = t.trim();
            if t.is_empty() && required {
                return Err(bad(format!("{field}: required")));
            }
            if t.chars().count() > max {
                return Err(bad(format!("{field}: at most {max} characters")));
            }
            Ok((!t.is_empty()).then(|| t.to_owned()))
        }
        _ if required => Err(bad(format!("{field}: required"))),
        _ => Err(bad(format!("{field}: must be a string"))),
    }
}

fn strings(
    v: &Value,
    field: &str,
    max_items: usize,
    max_len: usize,
) -> Result<Vec<String>, ApiError> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) if a.len() <= max_items => a
            .iter()
            .map(|x| text(x, field, max_len, true).map(Option::unwrap_or_default))
            .collect(),
        _ => Err(bad(format!("{field}: at most {max_items} strings"))),
    }
}

/// Validate a create body into the stored model (without id/files/times).
pub fn validate_create(b: &Value) -> Result<Value, ApiError> {
    let title = text(&b["title"], "title", 120, true)?;
    let prompt = text(&b["prompt"], "prompt", 2000, false)?;
    let source = text(&b["source"], "source", 16, false)?.unwrap_or_else(|| "meshy".into());
    if !["meshy", "upload"].contains(&source.as_str()) {
        return Err(bad("source: meshy or upload"));
    }
    let job_id = text(&b["jobId"], "jobId", 64, false)?;
    if job_id
        .as_deref()
        .is_some_and(|j| !j.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    {
        return Err(bad("jobId: letters, digits and -"));
    }
    let credits = match &b["credits"] {
        Value::Null => None,
        v => Some(
            v.as_u64()
                .filter(|c| *c <= 10_000)
                .ok_or_else(|| bad("credits: 0 to 10000"))?,
        ),
    };
    let animations = strings(&b["animations"], "animations", 20, 60)?;
    let formats = strings(&b["formats"], "formats", 6, 8)?;
    if let Some(f) = formats
        .iter()
        .find(|f| !["glb", "fbx", "usdz", "obj", "stl", "3mf"].contains(&f.as_str()))
    {
        return Err(bad(format!("formats: unknown format {f}")));
    }
    let parent = text(&b["parent"], "parent", 48, false)?;
    if parent.as_deref().is_some_and(|p| !valid_id(p)) {
        return Err(bad("parent: a model id"));
    }
    let clips = match &b["clips"] {
        Value::Null => json!([]),
        Value::Array(a) if a.len() <= 20 => json!(a
            .iter()
            .map(|c| json!({"actionId": c["actionId"].as_i64(), "name": c["name"].as_str().map(|s| s.chars().take(60).collect::<String>()), "inPlace": c["inPlace"].as_bool()}))
            .collect::<Vec<_>>()),
        _ => return Err(bad("clips: at most 20")),
    };
    let meshy = match &b["meshy"] {
        Value::Null => Value::Null,
        v @ Value::Object(_) if v.to_string().len() <= MAX_META_JSON => v.clone(),
        _ => return Err(bad("meshy: an object of at most 16 KB")),
    };
    Ok(json!({
        "title": title, "prompt": prompt, "source": source, "jobId": job_id, "credits": credits,
        "animations": animations, "clips": clips, "formats": formats, "parent": parent, "meshy": meshy,
    }))
}

impl ModelStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(MODELS_DIR),
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn model_dir(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.dir.join(id))
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let d = self.model_dir(id)?;
        let bytes = std::fs::read(d.join(META_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, m: &Value) -> Result<(), ApiError> {
        let id = m["id"].as_str().unwrap_or_default();
        let d = self.model_dir(id).ok_or_else(ApiError::not_found)?;
        save_json_atomic(&d, META_FILE, m).map_err(|_| ApiError::internal("model write failed"))
    }

    pub fn list(&self, q: Option<&str>, limit: usize) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let q = q.map(str::to_lowercase).filter(|s| !s.is_empty());
        let mut out: Vec<Value> = rd
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter_map(|id| self.get(&id))
            .filter(|m| match &q {
                None => true,
                Some(q) => {
                    let hay = format!("{} {} {}", m["title"], m["prompt"], m["animations"])
                        .to_lowercase();
                    hay.contains(q.as_str())
                }
            })
            .collect();
        out.sort_by(|a, b| b["createdAt"].as_str().cmp(&a["createdAt"].as_str()));
        out.truncate(limit);
        out
    }

    pub fn create(&self, fields: Value, caller: Caller) -> Result<Value, ApiError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        ensure_dir(&self.dir).map_err(|_| ApiError::internal("models dir"))?;
        let count = std::fs::read_dir(&self.dir).map(|r| r.count()).unwrap_or(0);
        if count >= MAX_MODELS {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "too_many_models",
                format!("at most {MAX_MODELS} models; delete some first"),
            ));
        }
        let id = random_id();
        let now = now_iso();
        let mut m = fields;
        m["id"] = json!(id);
        m["files"] = json!([]);
        m["createdAt"] = json!(now);
        m["updatedAt"] = json!(now);
        m["createdBy"] = json!(caller.0);
        ensure_dir(&self.dir.join(&id)).map_err(|_| ApiError::internal("model dir"))?;
        self.save(&m)?;
        Ok(m)
    }

    /// Record a file that was written into the model's directory.
    fn add_file(&self, id: &str, name: &str, bytes: u64) -> Result<Value, ApiError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut m = self.get(id).ok_or_else(ApiError::not_found)?;
        let files = m["files"]
            .as_array_mut()
            .ok_or_else(|| ApiError::internal("model files"))?;
        files.retain(|f| f["name"] != name);
        files.push(json!({"name": name, "bytes": bytes, "contentType": content_type(name), "updatedAt": now_iso()}));
        // Thumbnails replace each other.
        if name.starts_with("thumb.") {
            files.retain(|f| {
                f["name"] == name || !f["name"].as_str().unwrap_or("").starts_with("thumb.")
            });
        }
        m["updatedAt"] = json!(now_iso());
        self.save(&m)?;
        Ok(m)
    }

    pub fn delete(&self, id: &str) -> Result<(), ApiError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let d = self.model_dir(id).ok_or_else(ApiError::not_found)?;
        if !d.join(META_FILE).exists() {
            return Err(ApiError::not_found());
        }
        std::fs::remove_dir_all(&d).map_err(|_| ApiError::internal("model delete failed"))
    }

    /// The path of a file the model lists (never anything else).
    pub fn file_path(&self, id: &str, name: &str) -> Option<PathBuf> {
        content_type(name)?;
        let m = self.get(id)?;
        let listed = m["files"].as_array()?.iter().any(|f| f["name"] == name);
        listed.then(|| self.dir.join(id).join(name))
    }
}

// ---------------------------------------------------------------- HTTP

fn client_label(headers: &axum::http::HeaderMap) -> &'static str {
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

fn lucina_only(c: Caller) -> Result<(), ApiError> {
    if c.0 == "lucina" {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only Lucina adds models",
        ))
    }
}

async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> Result<R, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("models task failed"))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    q: Option<String>,
    limit: Option<usize>,
}

async fn list(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.q.as_deref().is_some_and(|s| s.chars().count() > 200) {
        return Err(bad("q: at most 200 characters"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_LIST_LIMIT);
    let store = st.models.clone();
    let models = blocking(move || store.list(q.q.as_deref(), limit)).await?;
    Ok(Json(json!({"models": models})))
}

async fn create(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    lucina_only(caller)?;
    let fields = validate_create(&body)?;
    let store = st.models.clone();
    let m = blocking(move || store.create(fields, caller)).await??;
    tracing::info!(id = %m["id"].as_str().unwrap_or(""), by = caller.0, "model created");
    Ok((StatusCode::CREATED, Json(json!({"model": m}))))
}

async fn get_one(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.models.clone();
    let m = blocking(move || store.get(&id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({"model": m})))
}

async fn delete_one(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let store = st.models.clone();
    let id2 = id.clone();
    blocking(move || store.delete(&id2)).await??;
    tracing::info!(id = %id, by = caller.0, "model deleted");
    Ok(Json(json!({"ok": true, "id": id})))
}

async fn put_file(
    State(st): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath((id, name)): UrlPath<(String, String)>,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    lucina_only(caller)?;
    if content_type(&name).is_none() {
        return Err(bad(
            "name: model.glb, thumb.png|jpg|webp, preview.mp4 or model.fbx|usdz|obj|stl|3mf",
        ));
    }
    if st.models.get(&id).is_none() {
        return Err(ApiError::not_found());
    }
    let dir = st.models.dir().join(&id);
    let tmp = dir.join(format!(".{name}.{}.part", std::process::id()));
    let limit = max_bytes(&name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|_| ApiError::internal("model file"))?;
    let mut stream = body.into_data_stream();
    let mut total = 0usize;
    let mut head: Vec<u8> = Vec::new();
    let outcome: Result<(), ApiError> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| bad("upload interrupted"))?;
            total += chunk.len();
            if total > limit {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "too_large",
                    format!("{name}: at most {} MB", limit / (1024 * 1024)),
                ));
            }
            if head.len() < 16 {
                head.extend_from_slice(&chunk[..chunk.len().min(16 - head.len())]);
            }
            file.write_all(&chunk)
                .map_err(|_| ApiError::internal("model file write"))?;
        }
        if total == 0 || !magic_ok(&name, &head) {
            return Err(bad(format!("{name}: the bytes are not a {name} file")));
        }
        file.sync_all()
            .map_err(|_| ApiError::internal("model file sync"))?;
        std::fs::rename(&tmp, dir.join(&name))
            .map_err(|_| ApiError::internal("model file rename"))?;
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let store = st.models.clone();
    let (id2, name2) = (id.clone(), name.clone());
    let m = blocking(move || store.add_file(&id2, &name2, total as u64)).await??;
    tracing::info!(id = %id, file = %name, bytes = total, by = caller.0, "model file stored");
    Ok(Json(json!({"model": m})))
}

async fn get_file(
    State(st): State<AppState>,
    UrlPath((id, name)): UrlPath<(String, String)>,
    req: Request,
) -> Response {
    let (Some(path), Some(ct)) = (st.models.file_path(&id, &name), content_type(&name)) else {
        return ApiError::not_found().into_response();
    };
    let Some(tag) = crate::assets::etag(&path) else {
        return ApiError::not_found().into_response();
    };
    let not_modified = req
        .headers()
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == tag);
    let mut resp = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mime: mime::Mime = ct.parse().expect("static mime");
        match ServeFile::new_with_mime(&path, &mime).oneshot(req).await {
            Ok(r) => r.into_response(),
            Err(never) => match never {},
        }
    };
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    if let Ok(v) = HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{id}-{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

pub fn routes(state: AppState) -> Router<AppState> {
    let files = Router::new()
        .route("/v1/models/{id}/files/{name}", put(put_file).get(get_file))
        .layer(DefaultBodyLimit::max(MAX_FILE_BYTES));
    Router::new()
        .route("/v1/models", get(list).post(create))
        .route("/v1/models/{id}", get(get_one).delete(delete_one))
        .merge(files)
        .route_layer(middleware::from_fn_with_state(state, device_or_service))
}

/// Shared handle for [`crate::Inner`].
pub fn store(data_dir: &Path) -> Arc<ModelStore> {
    let s = ModelStore::new(data_dir);
    if let Err(e) = ensure_dir(s.dir()) {
        tracing::warn!(dir = %s.dir().display(), error = %e, "models dir not created yet");
    }
    Arc::new(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_names_and_magic() {
        assert!(valid_id("mdl_0123456789abcdef"));
        for bad in [
            "mdl_ABCDEFGH12",
            "mdl_short",
            "../models",
            "mdl_0123456789abcdef/../x",
            "",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert_eq!(content_type("model.glb"), Some("model/gltf-binary"));
        for bad in ["../model.glb", "model.exe", "MODEL.GLB", "thumb.gif", ""] {
            assert_eq!(content_type(bad), None, "{bad}");
        }
        assert!(magic_ok("model.glb", b"glTF\x02\0\0\0"));
        assert!(!magic_ok("model.glb", b"<html>"));
        assert!(magic_ok("preview.mp4", b"\0\0\0\x20ftypisom"));
        assert!(!magic_ok("thumb.png", b"GIF89a"));
    }

    #[test]
    fn store_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let s = ModelStore::new(dir.path());
        assert!(validate_create(&json!({})).is_err());
        assert!(validate_create(&json!({"title": "x", "formats": ["exe"]})).is_err());
        assert!(validate_create(&json!({"title": "x", "parent": "../x"})).is_err());
        assert!(validate_create(&json!({"title": "x", "meshy": "y".repeat(10)})).is_err());
        let f = validate_create(&json!({"title": " Knight ", "prompt": "a knight", "credits": 41, "animations": ["Walk"], "formats": ["glb", "usdz"], "meshy": {"taskId": "t"}})).unwrap();
        let m = s.create(f, Caller("lucina")).unwrap();
        let id = m["id"].as_str().unwrap().to_owned();
        assert!(valid_id(&id));
        assert_eq!(s.get(&id).unwrap()["title"], "Knight");
        assert!(s.file_path(&id, "model.glb").is_none(), "not stored yet");
        std::fs::write(s.dir().join(&id).join("model.glb"), b"glTF").unwrap();
        s.add_file(&id, "model.glb", 4).unwrap();
        s.add_file(&id, "thumb.png", 1).unwrap();
        let m = s.add_file(&id, "thumb.jpg", 1).unwrap();
        let names: Vec<_> = m["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].clone())
            .collect();
        assert_eq!(names, vec![json!("model.glb"), json!("thumb.jpg")]);
        assert!(s.file_path(&id, "model.glb").is_some());
        assert_eq!(s.list(Some("walk"), 10).len(), 1);
        assert_eq!(s.list(Some("dragon"), 10).len(), 0);
        s.delete(&id).unwrap();
        assert!(s.get(&id).is_none());
        assert!(s.delete(&id).is_err());
    }
}
