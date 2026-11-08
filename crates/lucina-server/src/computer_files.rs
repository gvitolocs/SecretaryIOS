//! Files between the owner's computer and Lucina's desktop, while the owner
//! drives it (the takeover lease in `lucina_computer_use::control`).
//!
//! - `POST /api/secretary/computer/files?sandbox=&name=&device=`: the raw file
//!   (at most [`FILE_LIMIT`]) lands in `~/Desktop/From Mac/` on her desktop
//!   (`LUCINA_FROM_MAC_DIR`), renamed `name (2).ext` and so on rather than
//!   overwriting. Answers `{path, name, bytes}`.
//! - `POST /api/secretary/computer/clipboard/files` `{sandbox, paths}`: puts
//!   files uploaded that way on her clipboard (Cua's `file_paths`), so Ctrl+V
//!   in her file manager or an upload dialog pastes them. Answers `{generation}`.
//! - `GET /api/secretary/computer/clipboard/files?sandbox=`: the files on her
//!   clipboard (copied in her file manager): `{generation, files: [{path, name, bytes}]}`.
//! - `GET /api/secretary/computer/files?sandbox=&path=&device=`: one of those,
//!   streamed as a download — only a path that is on her clipboard right now.
//!
//! Lucina talks to the sandbox's own API (`FilesystemService`,
//! `ComputerService`) with the sandbox credential; the bytes go through
//! signed URLs (`CreateSignedUrl`), streamed both ways, never held whole. The
//! audit records names and sizes, never contents. secretary-server forwards
//! these as `/v1/computer/files…` behind the device login.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, TryStreamExt};
use lucina_computer_use::viewer::ViewerConfig;
use lucina_computer_use::{control, SandboxId};
use serde_json::{json, Value};

use crate::api::{ok, ApiError};
use crate::cua_wire::clip_proto::{self, fields, put_len, put_varint, Field};
use crate::cua_wire::{grpc_web, read_capped};
use crate::AppState;

/// Largest file sent either way.
pub const FILE_LIMIT: u64 = 100 * 1024 * 1024;
/// Files on her clipboard listed at once.
const MAX_LISTED: usize = 50;
/// Uploaded paths remembered per sandbox (what `clipboard/files` may name).
const MAX_UPLOADED: usize = 256;
const SIGNED_URL_TTL: Duration = Duration::from_secs(300);
/// A unary answer, `GetClipboard`'s PNG included.
const ANSWER_LIMIT: usize = 40 * 1024 * 1024;

const GRPC_NOT_FOUND: &str = "5";

fn inbox() -> String {
    std::env::var("LUCINA_FROM_MAC_DIR")
        .ok()
        .filter(|d| d.starts_with('/') && !d.split('/').any(|s| s == ".."))
        .unwrap_or_else(|| "/home/cua/Desktop/From Mac".into())
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

/// Paths `POST files` wrote, per sandbox, newest last.
fn uploaded() -> &'static Mutex<HashMap<&'static str, VecDeque<String>>> {
    static U: OnceLock<Mutex<HashMap<&'static str, VecDeque<String>>>> = OnceLock::new();
    U.get_or_init(Default::default)
}

fn remember_upload(sb: SandboxId, path: &str) {
    let mut map = uploaded().lock().unwrap_or_else(|e| e.into_inner());
    let list = map.entry(sb.as_str()).or_default();
    list.retain(|p| p != path);
    list.push_back(path.to_owned());
    while list.len() > MAX_UPLOADED {
        list.pop_front();
    }
}

fn was_uploaded(sb: SandboxId, path: &str) -> bool {
    let map = uploaded().lock().unwrap_or_else(|e| e.into_inner());
    map.get(sb.as_str())
        .is_some_and(|l| l.iter().any(|p| p == path))
}

// ------------------------------------------------------------ the sandbox API

/// The sandbox's API: its address and credential.
pub(crate) struct Sandbox {
    machine: String,
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: String,
    /// `FileType`: 1 file, 2 directory, 3 symlink.
    pub(crate) kind: u64,
    pub(crate) size: u64,
}

impl Sandbox {
    fn of(sb: SandboxId) -> Result<Self, ApiError> {
        #[cfg(test)]
        if let Some(m) = tests::FAKE.get() {
            return Ok(Self::at(m));
        }
        let (machine, token) =
            lucina_computer_use::viewer::api_credential(&ViewerConfig::from_env(), sb)
                .map_err(|e| ApiError::new(503, "sandbox_unreachable", e.to_string()))?;
        Ok(Self { machine, token })
    }

    #[cfg(test)]
    pub(crate) fn at(machine: &str) -> Self {
        Self {
            machine: machine.into(),
            token: "test".into(),
        }
    }

    /// One unary gRPC-web call; `Ok(None)` when the sandbox answers NOT_FOUND.
    async fn call(&self, method: &str, message: &[u8]) -> Result<Option<Vec<u8>>, ApiError> {
        let up = http()
            .post(format!("{}/cua.env.v1.{method}", self.machine))
            .header(header::CONTENT_TYPE, "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .header("x-cua-env-authorization", format!("Bearer {}", self.token))
            .timeout(Duration::from_secs(20))
            .body(grpc_web::frame(0, message))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(method, error = %e.without_url(), "computer files: sandbox call failed");
                ApiError::new(502, "sandbox_unreachable", "")
            })?;
        if !up.status().is_success() {
            return Err(ApiError::new(
                502,
                "sandbox_refused",
                format!("{method}: HTTP {}", up.status()),
            ));
        }
        if let Some(code) = up
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
        {
            if code == GRPC_NOT_FOUND {
                return Ok(None);
            }
            if code != "0" {
                return Err(ApiError::new(
                    502,
                    "sandbox_refused",
                    format!("{method}: gRPC {code}"),
                ));
            }
        }
        let body = read_capped(up, ANSWER_LIMIT).await.ok_or_else(|| {
            ApiError::new(
                502,
                "sandbox_refused",
                format!("{method}: answer too large"),
            )
        })?;
        let frames = grpc_web::frames(&body).ok_or_else(|| {
            ApiError::new(
                502,
                "sandbox_refused",
                format!("{method}: malformed answer"),
            )
        })?;
        for (flags, t) in &frames {
            if flags & 0x80 != 0 && !grpc_web::status_ok(t) {
                let text = String::from_utf8_lossy(t);
                if text.contains(&format!("grpc-status:{GRPC_NOT_FOUND}")) {
                    return Ok(None);
                }
                return Err(ApiError::new(
                    502,
                    "sandbox_refused",
                    format!("{method} failed"),
                ));
            }
        }
        Ok(Some(
            frames
                .iter()
                .find(|(f, _)| *f == 0)
                .map(|(_, m)| m.to_vec())
                .unwrap_or_default(),
        ))
    }

    async fn stat(&self, path: &str) -> Result<Option<Entry>, ApiError> {
        let mut req = Vec::new();
        put_len(&mut req, 1, path.as_bytes());
        let Some(answer) = self.call("FilesystemService/Stat", &req).await? else {
            return Ok(None);
        };
        Ok(stat_entry(&answer))
    }

    async fn make_dir(&self, path: &str) -> Result<(), ApiError> {
        let mut req = Vec::new();
        put_len(&mut req, 1, path.as_bytes());
        put_varint(&mut req, 2 << 3); // parents
        put_varint(&mut req, 1);
        self.call("FilesystemService/MakeDir", &req).await.map(drop)
    }

    async fn remove(&self, path: &str) {
        let mut req = Vec::new();
        put_len(&mut req, 1, path.as_bytes());
        put_varint(&mut req, 3 << 3); // missing_ok
        put_varint(&mut req, 1);
        let _ = self.call("FilesystemService/Remove", &req).await;
    }

    /// `method`: 1 GET, 2 PUT. Answers the URL to call, on the sandbox.
    async fn signed_url(
        &self,
        path: &str,
        method: u64,
        download_name: Option<&str>,
    ) -> Result<String, ApiError> {
        let mut req = Vec::new();
        put_len(&mut req, 1, path.as_bytes());
        put_varint(&mut req, 2 << 3);
        put_varint(&mut req, method);
        let mut ttl = Vec::new();
        put_varint(&mut ttl, 1 << 3);
        put_varint(&mut ttl, SIGNED_URL_TTL.as_secs());
        put_len(&mut req, 3, &ttl);
        if let Some(n) = download_name {
            put_len(&mut req, 5, n.as_bytes());
        }
        let answer = self
            .call("FilesystemService/CreateSignedUrl", &req)
            .await?
            .ok_or_else(|| ApiError::new(404, "not_found", "no such file on her desktop"))?;
        let url_path = fields(&answer)
            .and_then(|f| {
                f.into_iter().find_map(|(n, f)| match (n, f) {
                    (1, Field::Len(v)) => std::str::from_utf8(v).ok().map(str::to_owned),
                    _ => None,
                })
            })
            .filter(|u| u.starts_with('/'))
            .ok_or_else(|| ApiError::new(502, "sandbox_refused", "no signed URL"))?;
        Ok(format!("{}{url_path}", self.machine))
    }

    /// The file paths on her clipboard, and its generation.
    async fn clipboard_files(&self) -> Result<(Vec<String>, u64), ApiError> {
        let answer = self
            .call("ComputerService/GetClipboard", &[])
            .await?
            .unwrap_or_default();
        let (content, generation) = clip_proto::get_response(&answer).ok_or_else(|| {
            ApiError::new(502, "sandbox_refused", "malformed GetClipboardResponse")
        })?;
        Ok((
            content.files.iter().map(|f| (*f).to_owned()).collect(),
            generation,
        ))
    }

    async fn set_clipboard_files(&self, paths: &[String]) -> Result<u64, ApiError> {
        let content = clip_proto::Content {
            text: None,
            files: paths.iter().map(String::as_str).collect(),
            png: None,
        };
        let answer = self
            .call(
                "ComputerService/SetClipboard",
                &clip_proto::set_request_bytes(&content),
            )
            .await?
            .unwrap_or_default();
        Ok(clip_proto::set_response_generation(&answer).unwrap_or(0))
    }
}

/// `StatResponse { EntryInfo entry = 1 }`, `EntryInfo { name = 1; path = 2; type = 3; size = 4 }`.
pub(crate) fn stat_entry(b: &[u8]) -> Option<Entry> {
    let entry = fields(b)?.into_iter().find_map(|(n, f)| match (n, f) {
        (1, Field::Len(v)) => Some(v),
        _ => None,
    })?;
    let mut e = Entry {
        name: String::new(),
        path: String::new(),
        kind: 0,
        size: 0,
    };
    for (n, f) in fields(entry)? {
        match (n, f) {
            (1, Field::Len(v)) => e.name = String::from_utf8_lossy(v).into_owned(),
            (2, Field::Len(v)) => e.path = String::from_utf8_lossy(v).into_owned(),
            (3, Field::Varint(k)) => e.kind = k,
            (4, Field::Varint(s)) => e.size = s,
            _ => {}
        }
    }
    Some(e)
}

// ---------------------------------------------------------------- the rules

/// A file name from the owner's computer: one path component, printable, short.
pub(crate) fn file_name(name: &str) -> Option<String> {
    let n = name.trim();
    let ok = !n.is_empty()
        && n.len() <= 255
        && n != "."
        && n != ".."
        && !n.contains('/')
        && !n.contains('\\')
        && !n.chars().any(char::is_control);
    ok.then(|| n.to_owned())
}

/// `report.pdf`, `report (2).pdf`, `report (3).pdf`…
pub(crate) fn numbered(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// `attachment; filename*=UTF-8''…` (RFC 6266/5987).
pub(crate) fn attachment(name: &str) -> String {
    let mut out = String::from("attachment; filename*=UTF-8''");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn device(q: &HashMap<String, String>) -> String {
    let d: String = q
        .get("device")
        .map(String::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    if d.trim().is_empty() {
        "owner".into()
    } else {
        d.trim().to_owned()
    }
}

/// The workspace, while the owner drives it.
fn driving(sandbox: Option<&str>) -> Result<SandboxId, ApiError> {
    let sb = SandboxId::parse(sandbox.unwrap_or("workspace"))
        .ok_or_else(|| ApiError::bad_request("sandbox must be workspace"))?;
    // (Tests use the Android lease: the workspace one is taken by other tests running alongside.)
    if sb != SandboxId::Workspace && !cfg!(test) {
        return Err(ApiError::new(
            400,
            "files_unsupported",
            "files go to the workspace desktop only",
        ));
    }
    if !control().owner_driving(sb) {
        return Err(ApiError::new(
            409,
            "not_driving",
            "take over before sending files",
        ));
    }
    Ok(sb)
}

// ---------------------------------------------------------------- routes

/// `POST|GET /api/secretary/computer/files` (bearer-authorized).
pub async fn files_route(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    let r = match *req.method() {
        Method::POST => upload(&q, req).await,
        Method::GET => download(&q).await,
        _ => Err(ApiError::method_not_allowed(&["POST", "GET"])),
    };
    r.unwrap_or_else(IntoResponse::into_response)
}

/// `GET|POST /api/secretary/computer/clipboard/files` (bearer-authorized).
pub async fn clipboard_route(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    let r = match *req.method() {
        Method::GET => copied(&q).await,
        Method::POST => put_on_clipboard(req).await,
        _ => Err(ApiError::method_not_allowed(&["GET", "POST"])),
    };
    r.unwrap_or_else(IntoResponse::into_response)
}

async fn upload(q: &HashMap<String, String>, req: Request) -> Result<Response, ApiError> {
    let sb = driving(q.get("sandbox").map(String::as_str))?;
    let name = q
        .get("name")
        .and_then(|n| file_name(n))
        .ok_or_else(|| ApiError::bad_request("name must be a plain file name"))?;
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > FILE_LIMIT) {
        return Err(too_large());
    }
    let api = Sandbox::of(sb)?;
    let dir = inbox();
    api.make_dir(&dir).await?;
    let mut path = None;
    for n in 1..=100 {
        let candidate = format!("{dir}/{}", numbered(&name, n));
        if api.stat(&candidate).await?.is_none() {
            path = Some(candidate);
            break;
        }
    }
    let path =
        path.ok_or_else(|| ApiError::new(409, "name_taken", "too many files with that name"))?;
    let url = api.signed_url(&path, 2, None).await?;
    // Streamed through, counted: a body over the cap fails the PUT mid-way.
    let mut sent = 0u64;
    let stream = req.into_body().into_data_stream().map(move |chunk| {
        let chunk = chunk.map_err(std::io::Error::other)?;
        sent += chunk.len() as u64;
        if sent > FILE_LIMIT {
            return Err(std::io::Error::other("file over the size limit"));
        }
        Ok(chunk)
    });
    let mut put = http().put(&url).body(reqwest::Body::wrap_stream(stream));
    if let Some(n) = declared {
        put = put.header(header::CONTENT_LENGTH, n);
    }
    let done = put.send().await;
    let stored = match done {
        Ok(r) if r.status().is_success() => api.stat(&path).await?,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "computer files: upload refused by the sandbox");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e.without_url(), "computer files: upload failed");
            None
        }
    };
    let Some(entry) = stored else {
        api.remove(&path).await;
        return Err(ApiError::new(
            502,
            "upload_failed",
            "the file did not reach her desktop",
        ));
    };
    if entry.size > FILE_LIMIT {
        api.remove(&path).await;
        return Err(too_large());
    }
    remember_upload(sb, &path);
    let stored_name = path.rsplit('/').next().unwrap_or(&name).to_owned();
    control().note(
        sb,
        "file_to_desktop",
        json!({"device": device(q), "name": stored_name, "bytes": entry.size}),
    );
    Ok(ok(
        json!({"path": path, "name": stored_name, "bytes": entry.size}),
    ))
}

async fn download(q: &HashMap<String, String>) -> Result<Response, ApiError> {
    let sb = driving(q.get("sandbox").map(String::as_str))?;
    let path = q
        .get("path")
        .filter(|p| p.starts_with('/'))
        .ok_or_else(|| ApiError::bad_request("path is required"))?;
    let api = Sandbox::of(sb)?;
    let (files, _) = api.clipboard_files().await?;
    if !files.iter().any(|f| f == path) {
        return Err(ApiError::new(
            409,
            "not_on_clipboard",
            "only files copied on her desktop can be downloaded",
        ));
    }
    let entry = api
        .stat(path)
        .await?
        .ok_or_else(|| ApiError::new(404, "not_found", "the copied file is gone"))?;
    if entry.kind != 1 {
        return Err(ApiError::new(
            400,
            "not_a_file",
            "folders are not downloaded",
        ));
    }
    if entry.size > FILE_LIMIT {
        return Err(too_large());
    }
    let url = api.signed_url(path, 1, Some(&entry.name)).await?;
    let up = http().get(&url).send().await.map_err(|e| {
        tracing::warn!(error = %e.without_url(), "computer files: download failed");
        ApiError::new(502, "sandbox_unreachable", "")
    })?;
    if !up.status().is_success() {
        return Err(ApiError::new(
            502,
            "sandbox_refused",
            format!("download: HTTP {}", up.status()),
        ));
    }
    control().note(
        sb,
        "file_from_desktop",
        json!({"device": device(q), "name": entry.name, "bytes": entry.size}),
    );
    let mut resp = Response::new(Body::from_stream(
        up.bytes_stream().map_err(std::io::Error::other),
    ));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(entry.size));
    if let Ok(v) = HeaderValue::from_str(&attachment(&entry.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

async fn copied(q: &HashMap<String, String>) -> Result<Response, ApiError> {
    let sb = driving(q.get("sandbox").map(String::as_str))?;
    let api = Sandbox::of(sb)?;
    let (paths, generation) = api.clipboard_files().await?;
    let mut files = Vec::new();
    for p in paths.iter().take(MAX_LISTED) {
        if let Some(e) = api.stat(p).await? {
            if e.kind == 1 {
                files.push(json!({"path": p, "name": e.name, "bytes": e.size}));
            }
        }
    }
    Ok(ok(json!({"generation": generation, "files": files})))
}

async fn put_on_clipboard(req: Request) -> Result<Response, ApiError> {
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map_err(|_| too_large())?;
    let v: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("JSON body expected"))?;
    let sb = driving(v.get("sandbox").and_then(Value::as_str))?;
    let paths: Vec<String> = v
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() || paths.len() > MAX_LISTED {
        return Err(ApiError::bad_request("paths: 1 to 50 uploaded files"));
    }
    if let Some(p) = paths.iter().find(|p| !was_uploaded(sb, p)) {
        return Err(ApiError::new(
            409,
            "not_uploaded",
            format!("{p} was not sent from this computer"),
        ));
    }
    let generation = Sandbox::of(sb)?.set_clipboard_files(&paths).await?;
    Ok(ok(json!({"generation": generation})))
}

fn too_large() -> ApiError {
    ApiError::new(413, "too_large", "files over 100 MiB are not sent")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_plain_component() {
        assert_eq!(file_name(" report.pdf "), Some("report.pdf".into()));
        assert_eq!(
            file_name("Screenshot 2026-10-27 at 22.10.03.png").as_deref(),
            Some("Screenshot 2026-10-27 at 22.10.03.png")
        );
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "../x",
            "a\\b",
            "a\nb",
            &"x".repeat(256),
        ] {
            assert_eq!(file_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn clashes_are_numbered_before_the_extension() {
        assert_eq!(numbered("report.pdf", 1), "report.pdf");
        assert_eq!(numbered("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered("README", 2), "README (2)");
        assert_eq!(numbered(".bashrc", 2), ".bashrc (2)");
    }

    #[test]
    fn downloads_carry_an_encoded_name() {
        assert_eq!(
            attachment("report.pdf"),
            "attachment; filename*=UTF-8''report.pdf"
        );
        assert_eq!(
            attachment("a b\"è.txt"),
            "attachment; filename*=UTF-8''a%20b%22%C3%A8.txt"
        );
    }

    #[test]
    fn stat_answers_are_read() {
        let mut entry = Vec::new();
        put_len(&mut entry, 1, b"report.pdf");
        put_len(&mut entry, 2, b"/home/cua/report.pdf");
        put_varint(&mut entry, 3 << 3);
        put_varint(&mut entry, 1);
        put_varint(&mut entry, 4 << 3);
        put_varint(&mut entry, 2_202_009);
        let mut answer = Vec::new();
        put_len(&mut answer, 1, &entry);
        assert_eq!(
            stat_entry(&answer),
            Some(Entry {
                name: "report.pdf".into(),
                path: "/home/cua/report.pdf".into(),
                kind: 1,
                size: 2_202_009
            })
        );
        assert_eq!(stat_entry(&[]), None);
    }

    /// The fake sandbox every `Sandbox::of` answers with, once started.
    pub(crate) static FAKE: OnceLock<String> = OnceLock::new();

    #[derive(Default)]
    struct Disk {
        files: HashMap<String, Vec<u8>>,
        clipboard: Vec<String>,
        generation: u64,
    }

    /// A sandbox speaking just the RPCs and signed URLs `computer_files` uses.
    async fn fake_sandbox() -> std::sync::Arc<tokio::sync::Mutex<Disk>> {
        use axum::extract::Path;
        use axum::routing::{any, post};
        use std::sync::Arc;
        let disk = Arc::new(tokio::sync::Mutex::new(Disk::default()));
        let grpc = |message: Vec<u8>| {
            let mut body = grpc_web::frame(0, &message);
            body.extend(grpc_web::frame(0x80, b"grpc-status:0\r\n"));
            ([(header::CONTENT_TYPE, "application/grpc-web+proto")], body).into_response()
        };
        let not_found = || ([("grpc-status", "5")], Vec::new()).into_response();
        let first_string = |m: &[u8]| -> String {
            fields(m)
                .and_then(|f| {
                    f.into_iter().find_map(|(n, f)| match (n, f) {
                        (1, Field::Len(v)) => Some(String::from_utf8_lossy(v).into_owned()),
                        _ => None,
                    })
                })
                .unwrap_or_default()
        };
        let d = disk.clone();
        let rpc = post(move |Path(method): Path<String>, body: axum::body::Bytes| {
            let d = d.clone();
            async move {
                let m = grpc_web::single_message(&body).unwrap_or_default().to_vec();
                let mut disk = d.lock().await;
                match method.as_str() {
                    "cua.env.v1.FilesystemService/Stat" => {
                        let path = first_string(&m);
                        let Some(data) = disk.files.get(&path) else {
                            return not_found();
                        };
                        let mut entry = Vec::new();
                        put_len(&mut entry, 1, path.rsplit('/').next().unwrap().as_bytes());
                        put_len(&mut entry, 2, path.as_bytes());
                        put_varint(&mut entry, 3 << 3);
                        put_varint(&mut entry, 1);
                        put_varint(&mut entry, 4 << 3);
                        put_varint(&mut entry, data.len() as u64);
                        let mut answer = Vec::new();
                        put_len(&mut answer, 1, &entry);
                        grpc(answer)
                    }
                    "cua.env.v1.FilesystemService/MakeDir" => grpc(Vec::new()),
                    "cua.env.v1.FilesystemService/Remove" => {
                        disk.files.remove(&first_string(&m));
                        grpc(Vec::new())
                    }
                    "cua.env.v1.FilesystemService/CreateSignedUrl" => {
                        let path = first_string(&m);
                        let mut answer = Vec::new();
                        let url = format!(
                            "/files?path={}",
                            attachment(&path).trim_start_matches("attachment; filename*=UTF-8''")
                        );
                        put_len(&mut answer, 1, url.as_bytes());
                        grpc(answer)
                    }
                    "cua.env.v1.ComputerService/GetClipboard" => {
                        let c = clip_proto::Content {
                            text: None,
                            files: disk.clipboard.iter().map(String::as_str).collect(),
                            png: None,
                        };
                        grpc(clip_proto::get_response_bytes(&c, disk.generation))
                    }
                    "cua.env.v1.ComputerService/SetClipboard" => {
                        let c = clip_proto::set_request(&m).unwrap();
                        disk.clipboard = c.files.iter().map(|f| (*f).to_owned()).collect();
                        disk.generation += 1;
                        let mut answer = Vec::new();
                        put_varint(&mut answer, 1 << 3);
                        put_varint(&mut answer, disk.generation);
                        grpc(answer)
                    }
                    _ => ([("grpc-status", "12")], Vec::new()).into_response(),
                }
            }
        });
        let d = disk.clone();
        let files = any(
            move |Query(q): Query<HashMap<String, String>>, req: Request| {
                let d = d.clone();
                async move {
                    let path = q.get("path").cloned().unwrap_or_default();
                    if req.method() == Method::PUT {
                        let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                            .await
                            .unwrap();
                        d.lock().await.files.insert(path, body.to_vec());
                        return axum::http::StatusCode::CREATED.into_response();
                    }
                    match d.lock().await.files.get(&path) {
                        Some(b) => b.clone().into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            },
        );
        let app = axum::Router::new()
            .route("/{*method}", rpc)
            .route("/files", files);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let _ = FAKE.set(format!("http://{addr}"));
        disk
    }

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    async fn body_json(r: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn files_go_both_ways_only_while_driving() {
        let disk = fake_sandbox().await;
        let _one = crate::computer_view::ANDROID_LEASE.lock().await;
        let sb = SandboxId::Android;
        let q = query(&[
            ("sandbox", "android"),
            ("name", "report.pdf"),
            ("device", "Mac"),
        ]);
        let file = || {
            Request::builder()
                .method("POST")
                .body(Body::from(vec![7u8; 3000]))
                .unwrap()
        };
        let refused = upload(&q, file()).await.unwrap_err();
        assert_eq!(refused.status, 409, "not driving");

        control().take(sb, "files-test");
        let r = upload(&q, file()).await.unwrap();
        let first = body_json(r).await["data"].clone();
        assert_eq!(first["path"], format!("{}/report.pdf", inbox()));
        assert_eq!(first["bytes"], 3000);
        let second = body_json(upload(&q, file()).await.unwrap()).await["data"].clone();
        assert_eq!(second["name"], "report (2).pdf", "never overwrites");
        let bad = upload(
            &query(&[
                ("sandbox", "android"),
                ("name", "../../.ssh/authorized_keys"),
            ]),
            file(),
        )
        .await;
        assert_eq!(bad.unwrap_err().status, 400);
        let mut huge = file();
        huge.headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(FILE_LIMIT + 1));
        assert_eq!(upload(&q, huge).await.unwrap_err().status, 413);

        // Onto her clipboard: only files sent from here.
        let set = |paths: Value| {
            Request::builder()
                .method("POST")
                .body(Body::from(
                    json!({"sandbox": "android", "paths": paths}).to_string(),
                ))
                .unwrap()
        };
        let r = put_on_clipboard(set(json!([first["path"], second["path"]])))
            .await
            .unwrap();
        assert_eq!(body_json(r).await["data"]["generation"], 1);
        assert_eq!(disk.lock().await.clipboard.len(), 2);
        let refused = put_on_clipboard(set(json!(["/home/cua/.ssh/id_ed25519"])))
            .await
            .unwrap_err();
        assert_eq!(refused.status, 409);

        // Copied on her side: listed, and only those download.
        disk.lock()
            .await
            .files
            .insert("/home/cua/Documents/notes.txt".into(), b"hello".to_vec());
        disk.lock()
            .await
            .files
            .insert("/home/cua/secret.txt".into(), b"no".to_vec());
        disk.lock().await.clipboard = vec!["/home/cua/Documents/notes.txt".into()];
        let listed = body_json(copied(&query(&[("sandbox", "android")])).await.unwrap()).await;
        assert_eq!(
            listed["data"]["files"],
            json!([{"path": "/home/cua/Documents/notes.txt", "name": "notes.txt", "bytes": 5}])
        );
        let r = download(&query(&[
            ("sandbox", "android"),
            ("path", "/home/cua/Documents/notes.txt"),
        ]))
        .await
        .unwrap();
        assert_eq!(
            r.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename*=UTF-8''notes.txt"
        );
        assert_eq!(
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            b"hello"
        );
        let refused = download(&query(&[
            ("sandbox", "android"),
            ("path", "/home/cua/secret.txt"),
        ]))
        .await
        .unwrap_err();
        assert_eq!(refused.status, 409, "not on her clipboard");

        control().release(sb, "files-test");
        assert_eq!(
            copied(&query(&[("sandbox", "android")]))
                .await
                .unwrap_err()
                .status,
            409
        );
    }

    #[test]
    fn uploads_are_remembered_per_sandbox_and_capped() {
        let sb = SandboxId::Workspace;
        for i in 0..(MAX_UPLOADED + 5) {
            remember_upload(sb, &format!("/tmp/f{i}"));
        }
        assert!(!was_uploaded(sb, "/tmp/f0"), "the oldest are forgotten");
        assert!(was_uploaded(sb, &format!("/tmp/f{}", MAX_UPLOADED + 4)));
        assert!(!was_uploaded(SandboxId::Android, "/tmp/f10"));
    }
}
