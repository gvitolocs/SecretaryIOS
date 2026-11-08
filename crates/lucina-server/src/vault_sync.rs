//! The owner's vault for devices that sync it themselves (Juno; later the phones): `/api/secretary/vault/*`, behind the
//! Secretary bearer like every `/api/secretary` route, reached by secretary-server's `/v1/vault/*` for owner devices.
//! The folder is the canonical vault on nezopt (`LUCINA_VAULT_SYNC_DIR`, default `/home/nez/mnt/mybook/Obsidian`), read
//! and written through `lucina_vault::sync::Folder`: the same path rules, compare-and-swap and atomic writes as the
//! device side. `LUCINA_VAULT_SYNC=off` turns the routes off. Design: SecretaryIOS `docs/lucina-vault.md`, "Sync".
//!
//! - `GET  manifest`                       → `{"files": [{"path", "size", "rev"}]}` (sha256 revisions, cached by stat)
//! - `GET  file?path=`                     → the bytes, `X-Vault-Revision`
//! - `PUT  file?path=&expect=<rev>|absent` → `{"rev"}`, or 409 `conflict` with `current` (null for no file)
//! - `POST trash?path=&expect=<rev>`       → `{"to"}` (into the vault's `.trash`), or 409 like `PUT`
//!
//! Only paths that sync are served or written (`sync_rules::classify`); `.obsidian/<name>.json` can be read, never written.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use lucina_vault::sync::{Folder, DEFAULT_MAX_SIZE};
use lucina_vault::Error as VaultError;
use serde_json::json;

use crate::api::{query_param, ApiError};

type ApiResult = Result<Response, ApiError>;

pub const DEFAULT_DIR: &str = "/home/nez/mnt/mybook/Obsidian";

static FOLDER: Mutex<Option<Arc<Folder>>> = Mutex::new(None);

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn dir() -> PathBuf {
    env("LUCINA_VAULT_SYNC_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

/// The folder, opened on first use and again after a failure (the disk may be mounted after Lucina starts). A missing
/// folder is 503, never an empty vault: a device would read it as "everything was deleted".
fn folder() -> Result<Arc<Folder>, ApiError> {
    let mut slot = FOLDER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = slot.as_ref() {
        if f.root().is_dir() {
            return Ok(f.clone());
        }
        *slot = None;
    }
    let d = dir();
    match Folder::open(&d) {
        Ok(f) => {
            let f = Arc::new(f);
            *slot = Some(f.clone());
            Ok(f)
        }
        Err(e) => {
            tracing::warn!(dir = %d.display(), error = %e, "vault sync: the vault folder is not there");
            Err(ApiError::new(503, "vault_unavailable", "The vault folder is not available on the server"))
        }
    }
}

fn vault_error(e: VaultError) -> ApiError {
    match e {
        VaultError::Conflict { current, .. } => {
            let mut err = ApiError::new(409, "conflict", "The file changed on the server; nothing was written");
            err.extra = Some(json!({ "current": if current.is_empty() { None } else { Some(current) } }));
            err
        }
        VaultError::NotFound(_) => ApiError::not_found(),
        VaultError::Invalid(m) => ApiError::bad_request(m),
        VaultError::Exists(p) => ApiError::new(409, "conflict", format!("{p} already exists")),
        e => {
            tracing::error!(error = %e, "vault sync: file operation failed");
            ApiError::internal()
        }
    }
}

fn path_param(uri: &Uri) -> Result<String, ApiError> {
    query_param(uri, "path").filter(|p| !p.is_empty()).ok_or_else(|| ApiError::bad_request("path is required"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|_| ApiError::internal())?
}

/// `/api/secretary/vault/<rest>`; the bearer was checked by the caller.
pub async fn route(method: &Method, uri: &Uri, rest: &[String], body: Body) -> ApiResult {
    if env("LUCINA_VAULT_SYNC").as_deref() == Some("off") {
        return Err(ApiError::new(503, "unavailable", "Vault sync is turned off on the server"));
    }
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match (method.as_str(), rest.as_slice()) {
        ("GET", ["manifest"]) => {
            let f = folder()?;
            let m = blocking(move || f.manifest().map_err(vault_error)).await?;
            Ok((StatusCode::OK, axum::Json(json!({ "files": m.files }))).into_response())
        }
        ("GET", ["file"]) => {
            let path = path_param(uri)?;
            let f = folder()?;
            let (bytes, rev) = blocking(move || f.read(&path).map_err(vault_error)).await?;
            let mut r = Response::new(Body::from(bytes));
            let h = r.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            h.insert("x-vault-revision", HeaderValue::from_str(&rev).map_err(|_| ApiError::internal())?);
            Ok(r)
        }
        ("PUT", ["file"]) => {
            let path = path_param(uri)?;
            let expect = match query_param(uri, "expect").as_deref() {
                Some("absent") => None,
                Some(r) if r.len() == 64 && r.bytes().all(|b| b.is_ascii_hexdigit()) => Some(r.to_owned()),
                _ => return Err(ApiError::bad_request("expect must be a revision or \"absent\"")),
            };
            let bytes = axum::body::to_bytes(body, DEFAULT_MAX_SIZE as usize)
                .await
                .map_err(|_| ApiError::new(413, "payload_too_large", "A vault file is limited to 64 MB"))?;
            let f = folder()?;
            let p = path.clone();
            let rev = blocking(move || f.put(&p, &bytes, expect.as_deref(), false).map_err(vault_error)).await?;
            tracing::info!(path = %path, "vault sync: file written from a device");
            Ok((StatusCode::OK, axum::Json(json!({ "rev": rev }))).into_response())
        }
        ("POST", ["trash"]) => {
            let path = path_param(uri)?;
            let expect = query_param(uri, "expect").ok_or_else(|| ApiError::bad_request("expect is required"))?;
            let f = folder()?;
            let p = path.clone();
            let to = blocking(move || f.trash(&p, &expect).map_err(vault_error)).await?;
            tracing::info!(path = %path, to = %to, "vault sync: file moved to the vault trash from a device");
            Ok((StatusCode::OK, axum::Json(json!({ "to": to }))).into_response())
        }
        (_, ["manifest"]) => Err(ApiError::method_not_allowed(&["GET"])),
        (_, ["file"]) => Err(ApiError::method_not_allowed(&["GET", "PUT"])),
        (_, ["trash"]) => Err(ApiError::method_not_allowed(&["POST"])),
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>, Option<String>) {
        let uri: Uri = uri.parse().unwrap();
        let rest: Vec<String> = uri.path().split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        let r = match route(&method.parse().unwrap(), &uri, &rest, Body::from(body.to_vec())).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        };
        let status = r.status().as_u16();
        let rev = r.headers().get("x-vault-revision").map(|v| v.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap().to_vec();
        (status, bytes, rev)
    }

    // One test, as the folder and its settings are process-wide.
    #[tokio::test]
    async fn manifest_file_put_trash_and_conflicts() {
        let d = std::env::temp_dir().join(format!("vault-sync-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // SAFETY: the only test in this binary that reads these variables.
        unsafe { std::env::set_var("LUCINA_VAULT_SYNC_DIR", d.join("missing")) };
        assert_eq!(call("GET", "/manifest", b"").await.0, 503, "a missing folder is not an empty vault");
        std::fs::create_dir_all(d.join(".obsidian")).unwrap();
        std::fs::write(d.join(".obsidian/app.json"), "{}").unwrap();
        unsafe { std::env::set_var("LUCINA_VAULT_SYNC_DIR", &d) };

        let (s, b, _) = call("PUT", "/file?path=A%2FNote%20one.md&expect=absent", b"hello").await;
        assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
        let rev: serde_json::Value = serde_json::from_slice(&b).unwrap();
        let rev = rev["rev"].as_str().unwrap().to_owned();
        let (s, b, r) = call("GET", "/file?path=A%2FNote%20one.md", b"").await;
        assert_eq!((s, b.as_slice(), r.as_deref()), (200, &b"hello"[..], Some(rev.as_str())));

        // Creating over an existing file, or replacing a revision that is not there, writes nothing.
        let (s, b, _) = call("PUT", "/file?path=A%2FNote%20one.md&expect=absent", b"x").await;
        assert_eq!(s, 409);
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["current"].as_str(), Some(rev.as_str()));
        let other = "0".repeat(64);
        assert_eq!(call("PUT", &format!("/file?path=A%2FNote%20one.md&expect={other}"), b"x").await.0, 409);
        assert_eq!(std::fs::read_to_string(d.join("A/Note one.md")).unwrap(), "hello");

        // Settings are readable, never writable; dot paths and escapes are refused.
        assert_eq!(call("GET", "/file?path=.obsidian%2Fapp.json", b"").await.0, 200);
        assert_eq!(call("PUT", "/file?path=.obsidian%2Fapp.json&expect=absent", b"{}").await.0, 400);
        for bad in ["..%2Fx.md", ".trash%2Fa.md", "%2Fetc%2Fpasswd", "a%5Cb.md"] {
            assert_eq!(call("PUT", &format!("/file?path={bad}&expect=absent"), b"x").await.0, 400, "{bad}");
        }
        assert_eq!(call("PUT", "/file?path=a.md", b"x").await.0, 400, "expect is required");

        let (s, b, _) = call("GET", "/manifest", b"").await;
        assert_eq!(s, 200);
        let m: serde_json::Value = serde_json::from_slice(&b).unwrap();
        let paths: Vec<&str> = m["files"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert_eq!(paths, vec![".obsidian/app.json", "A/Note one.md"]);

        assert_eq!(call("POST", &format!("/trash?path=A%2FNote%20one.md&expect={other}"), b"").await.0, 409);
        let (s, b, _) = call("POST", &format!("/trash?path=A%2FNote%20one.md&expect={rev}"), b"").await;
        assert_eq!(s, 200, "{}", String::from_utf8_lossy(&b));
        assert!(d.join(".trash/A/Note one.md").exists());
        assert_eq!(call("GET", "/file?path=A%2FNote%20one.md", b"").await.0, 404);
        assert_eq!(call("DELETE", "/file?path=a.md", b"").await.0, 405);
        let _ = std::fs::remove_dir_all(&d);
    }
}
