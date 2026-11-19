//! The read-only update channel server.
//!
//! It serves opaque bytes from a store it only ever reads:
//!
//! ```text
//! <store>/<product>/<channel>/manifest
//! <store>/<product>/<channel>/artefacts/<sha256>.<ext>      (immutable)
//! ```
//!
//! It holds no key, signs nothing and verifies nothing. A device accepts an update only because
//! the *signed manifest* (checked by the client and `lucina-manifest`, not here) names the
//! artefact's size and SHA-256, so this server is untrusted transport: its job is privacy (the
//! code is not public) and cheap, correct delivery (Range, strong ETag). It sits behind Caddy
//! `forward_auth` to secretary-server (`/v1/auth/updates`) and does no authentication itself.
//!
//! Routes (`GET` and `HEAD`; any other method is 405):
//! - `/v1/updates/<product>/<channel>/manifest`
//! - `/v1/updates/<product>/<channel>/artefacts/<sha256>.<ext>`
//!
//! Everything else is 404 with no listing. The file server is written here, not shared with
//! `lucina-media::http::serve_file`: that one is built on `tower_http::ServeFile`, which opens by
//! path (follows symlinks) and would need `lucina-media` (JPEG XL, SQLite, ML...) as a
//! dependency; here every file is opened without following a symlink and streamed.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

/// The listen address must be a loopback one (`127.0.0.0/8` or `::1`): the sidecar does no
/// authentication, so only the proxy on the same host may reach it.
pub fn require_loopback(addr: std::net::SocketAddr) -> Result<(), String> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!(
            "--addr {addr}: lucina-updates listens on loopback only"
        ))
    }
}

/// A manifest bigger than this is a publisher bug and is not served.
pub const MANIFEST_MAX: u64 = 16 * 1024;

const PREFIX: &str = "/v1/updates/";

/// What a request path asks for, after validation.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    Manifest {
        product: &'a str,
        channel: &'a str,
    },
    Artefact {
        product: &'a str,
        channel: &'a str,
        /// `<sha256>.<ext>`
        name: &'a str,
        /// The 64 lowercase hex digits.
        sha256: &'a str,
    },
}

/// `^[a-z0-9][a-z0-9-]{0,31}$`
fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[a-z0-9]{1,8}(\.[a-z0-9]{1,8})?$`
fn valid_ext(s: &str) -> bool {
    let part = |p: &str| {
        (1..=8).contains(&p.len())
            && p.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    match s.split_once('.') {
        None => part(s),
        Some((a, b)) => part(a) && part(b),
    }
}

/// Validate a raw (still percent-encoded) request path. Nothing is decoded: a `%` anywhere, a
/// `..`, `//`, a backslash or a NUL is refused before the filesystem is touched.
pub fn parse_route(path: &str) -> Option<Route<'_>> {
    if path.contains(['%', '\\', '\0']) || path.contains("..") || path.contains("//") {
        return None;
    }
    let rest = path.strip_prefix(PREFIX)?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [product, channel, "manifest"] if valid_slug(product) && valid_slug(channel) => {
            Some(Route::Manifest { product, channel })
        }
        [product, channel, "artefacts", name] if valid_slug(product) && valid_slug(channel) => {
            let (sha, ext) = name.split_at_checked(64)?;
            let ext = ext.strip_prefix('.')?;
            if sha
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                && valid_ext(ext)
            {
                Some(Route::Artefact {
                    product,
                    channel,
                    name,
                    sha256: sha,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

struct Inner {
    root: PathBuf,
    canon_root: PathBuf,
}

/// Why a file was not opened.
enum Refusal {
    NotFound,
    Io(std::io::Error),
}

impl Inner {
    /// Open `<root>/<components...>` read-only with no symlink followed at any component. The
    /// path is made of validated names only; each parent is checked with `lstat`, the file is
    /// opened `O_NOFOLLOW`, and the canonical path must still be under the canonical root.
    fn open(&self, components: &[&str]) -> Result<File, Refusal> {
        let mut cur = self.root.clone();
        for (i, c) in components.iter().enumerate() {
            cur.push(c);
            let md = std::fs::symlink_metadata(&cur).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
                    Refusal::NotFound
                }
                _ => Refusal::Io(e),
            })?;
            let last = i + 1 == components.len();
            let ok = if last { md.is_file() } else { md.is_dir() };
            if !ok {
                // a symlink, a directory where a file should be, anything else
                return Err(Refusal::NotFound);
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&cur)
            .map_err(|e| match e.raw_os_error() {
                // ELOOP: swapped for a symlink since the lstat
                Some(libc::ELOOP) => Refusal::NotFound,
                _ => match e.kind() {
                    std::io::ErrorKind::NotFound => Refusal::NotFound,
                    _ => Refusal::Io(e),
                },
            })?;
        let md = file.metadata().map_err(Refusal::Io)?;
        if !md.is_file() {
            return Err(Refusal::NotFound);
        }
        match std::fs::canonicalize(&cur) {
            Ok(c) if c.starts_with(&self.canon_root) => {}
            _ => return Err(Refusal::NotFound),
        }
        Ok(file)
    }
}

/// The router for a store directory. Fails when the store does not exist.
pub fn app(store: &Path) -> std::io::Result<Router> {
    let canon_root = std::fs::canonicalize(store)?;
    let inner = Arc::new(Inner {
        root: store.to_path_buf(),
        canon_root,
    });
    Ok(Router::new().fallback(handle).with_state(inner))
}

fn plain(status: StatusCode, body: &'static str) -> Response {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn handle(State(st): State<Arc<Inner>>, req: Request) -> Response {
    let Some(route) = parse_route(req.uri().path()) else {
        return plain(StatusCode::NOT_FOUND, "not found\n");
    };
    let head = match *req.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => {
            let mut r = plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
            r.headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return r;
        }
    };
    let headers = req.headers().clone();
    // blocking filesystem calls: tiny, and `open` is the only part that can wait on a slow disk
    let opened = {
        let st = st.clone();
        let comps: Vec<String> = match &route {
            Route::Manifest { product, channel } => {
                vec![product.to_string(), channel.to_string(), "manifest".into()]
            }
            Route::Artefact {
                product,
                channel,
                name,
                ..
            } => vec![
                product.to_string(),
                channel.to_string(),
                "artefacts".into(),
                name.to_string(),
            ],
        };
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = comps.iter().map(String::as_str).collect();
            st.open(&refs)
        })
        .await
    };
    let file = match opened {
        Ok(Ok(f)) => f,
        Ok(Err(Refusal::NotFound)) => return plain(StatusCode::NOT_FOUND, "not found\n"),
        Ok(Err(Refusal::Io(e))) => {
            tracing::error!(path = req.uri().path(), error = %e, "open failed");
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
        }
        Err(e) => {
            tracing::error!(error = %e, "open task failed");
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
        }
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            tracing::error!(error = %e, "fstat failed");
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
        }
    };
    match route {
        Route::Manifest { .. } => serve_manifest(file, len, head, req.uri().path()).await,
        Route::Artefact { sha256, .. } => serve_artefact(file, len, head, &headers, sha256).await,
    }
}

fn base_headers(r: &mut Response, cache: &'static str) {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
}

async fn serve_manifest(file: File, len: u64, head: bool, path: &str) -> Response {
    if len > MANIFEST_MAX {
        tracing::error!(
            path,
            len,
            max = MANIFEST_MAX,
            "manifest over the cap: publisher bug, not served"
        );
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
    }
    // One read from the one open file: a rename over `manifest` meanwhile cannot mix versions.
    let mut buf = Vec::with_capacity(len as usize);
    let mut f = tokio::fs::File::from_std(file).take(MANIFEST_MAX + 1);
    if let Err(e) = f.read_to_end(&mut buf).await {
        tracing::error!(path, error = %e, "manifest read failed");
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
    }
    if buf.len() as u64 > MANIFEST_MAX {
        tracing::error!(
            path,
            "manifest grew over the cap while read: publisher bug, not served"
        );
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
    }
    let n = buf.len();
    let mut r = Response::new(if head { Body::empty() } else { Body::from(buf) });
    base_headers(&mut r, "no-cache");
    r.headers_mut().insert(header::CONTENT_LENGTH, n.into());
    r
}

/// A parsed `Range` header against a file of `len` bytes.
#[derive(Debug, PartialEq, Eq)]
enum RangeSel {
    /// Absent, not `bytes`, several ranges, or malformed: the whole file (RFC 9110: ignore it).
    Whole,
    /// Inclusive start and end, already clamped.
    Part(u64, u64),
    Unsatisfiable,
}

fn select_range(value: &str, len: u64) -> RangeSel {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeSel::Whole;
    };
    if spec.contains(',') {
        return RangeSel::Whole;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeSel::Whole;
    };
    let num = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            None
        } else {
            // a number too big for u64 is unsatisfiable, not an error
            Some(s.parse().unwrap_or(u64::MAX))
        }
    };
    match (num(a), num(b)) {
        (Some(start), None) if b.is_empty() => {
            if start >= len {
                RangeSel::Unsatisfiable
            } else {
                RangeSel::Part(start, len - 1)
            }
        }
        (Some(start), Some(end)) => {
            if start > end {
                RangeSel::Whole
            } else if start >= len {
                RangeSel::Unsatisfiable
            } else {
                RangeSel::Part(start, end.min(len - 1))
            }
        }
        (None, Some(n)) if a.is_empty() => {
            if n == 0 || len == 0 {
                RangeSel::Unsatisfiable
            } else {
                RangeSel::Part(len.saturating_sub(n), len - 1)
            }
        }
        _ => RangeSel::Whole,
    }
}

async fn serve_artefact(
    mut file: File,
    len: u64,
    head: bool,
    req: &HeaderMap,
    sha256: &str,
) -> Response {
    let etag = format!("\"{sha256}\"");
    let hdr = |name| req.get(name).and_then(|v| v.to_str().ok());
    if hdr(header::IF_NONE_MATCH).is_some_and(|v| {
        v.split(',')
            .map(|t| t.trim().trim_start_matches("W/"))
            .any(|t| t == etag || t == "*")
    }) {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        r.headers_mut()
            .insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
        r.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, immutable"),
        );
        return r;
    }
    // A range is honoured when there is no If-Range, or its strong tag is this file's. A date
    // never matches (no Last-Modified is sent), nor does a weak tag or an old tag: whole file.
    let honour = match hdr(header::IF_RANGE) {
        None => true,
        Some(v) => v.trim() == etag,
    };
    let sel = match hdr(header::RANGE) {
        Some(v) if honour => select_range(v, len),
        _ => RangeSel::Whole,
    };
    let (status, start, count) = match sel {
        RangeSel::Whole => (StatusCode::OK, 0, len),
        RangeSel::Part(s, e) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        RangeSel::Unsatisfiable => {
            let mut r = plain(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable\n");
            r.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{len}")).unwrap(),
            );
            return r;
        }
    };
    let body = if head || count == 0 {
        Body::empty()
    } else {
        if start > 0 {
            if let Err(e) = file.seek(SeekFrom::Start(start)) {
                tracing::error!(error = %e, "seek failed");
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "error\n");
            }
        }
        // `take` bounds the stream even if the file grows; nothing is read into memory whole.
        let f = tokio::fs::File::from_std(file).take(count);
        Body::from_stream(ReaderStream::with_capacity(f, 64 * 1024))
    };
    let mut r = Response::new(body);
    *r.status_mut() = status;
    base_headers(&mut r, "private, immutable");
    let h = r.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    h.insert(header::CONTENT_LENGTH, count.into());
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{len}", start + count - 1)).unwrap(),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lb(s: &str) -> Result<(), String> {
        require_loopback(s.parse().unwrap())
    }

    #[test]
    fn only_loopback_addresses_are_accepted() {
        assert_eq!(lb("127.0.0.1:8811"), Ok(()));
        assert_eq!(lb("[::1]:8811"), Ok(()));
        // all of 127.0.0.0/8 is loopback to std, so 127.0.0.2 is accepted (behaviour as it was)
        assert_eq!(lb("127.0.0.2:8811"), Ok(()));
        // everything else is refused with the same message as before
        for a in [
            "0.0.0.0:8811",
            "[::]:8811",
            "192.168.1.20:8811",
            "100.64.1.5:8811",
        ] {
            assert_eq!(
                lb(a),
                Err(format!(
                    "--addr {a}: lucina-updates listens on loopback only"
                )),
                "{a}"
            );
        }
    }

    #[test]
    fn range_selection() {
        assert_eq!(select_range("bytes=0-9", 100), RangeSel::Part(0, 9));
        assert_eq!(select_range("bytes=90-", 100), RangeSel::Part(90, 99));
        assert_eq!(select_range("bytes=-10", 100), RangeSel::Part(90, 99));
        assert_eq!(select_range("bytes=-500", 100), RangeSel::Part(0, 99));
        assert_eq!(select_range("bytes=50-500", 100), RangeSel::Part(50, 99));
        assert_eq!(select_range("bytes=100-", 100), RangeSel::Unsatisfiable);
        assert_eq!(select_range("bytes=-0", 100), RangeSel::Unsatisfiable);
        assert_eq!(select_range("bytes=5-2", 100), RangeSel::Whole);
        assert_eq!(select_range("bytes=0-1,5-6", 100), RangeSel::Whole);
        assert_eq!(select_range("items=0-1", 100), RangeSel::Whole);
        assert_eq!(select_range("bytes=a-b", 100), RangeSel::Whole);
        assert_eq!(
            select_range("bytes=99999999999999999999999-", 100),
            RangeSel::Unsatisfiable
        );
    }

    // The path checks that an HTTP client cannot even send (a client normalises `..`, rejects a
    // backslash or a NUL), so they are checked on the parser; the server tests send the rest raw.
    #[test]
    fn route_parser_refuses_bad_paths() {
        let sha = "a".repeat(64);
        let ok = format!("/v1/updates/lucinanet/stable/artefacts/{sha}.tar.gz");
        assert!(parse_route(&ok).is_some());
        assert!(parse_route("/v1/updates/lucinanet/stable/manifest").is_some());
        for bad in [
            "/v1/updates/../etc/passwd".to_string(),
            "/v1/updates/lucinanet/../stable/manifest".into(),
            "/v1/updates/lucinanet/stable/%2e%2e/manifest".into(),
            "/v1/updates/lucinanet/stable/%2E%2E/manifest".into(),
            "/v1/updates/lucinanet%2fstable/manifest".into(),
            "/v1/updates/lucinanet/stable%5cmanifest".into(),
            "/v1/updates//lucinanet/stable/manifest".into(),
            "/v1/updates/lucinanet/stable//manifest".into(),
            "/v1/updates/lucinanet\\stable/manifest".into(),
            "/v1/updates/lucinanet/stable/manifest\0".into(),
            "/v1/updates/lucinanet/stable/manifest%00".into(),
            "/etc/passwd".into(),
            "v1/updates/lucinanet/stable/manifest".into(),
            "/v1/updates/lucinanet/stable/manifest/".into(),
            "/v1/updates/lucinanet/stable".into(),
            "/v1/updates/lucinanet/stable/artefacts".into(),
            "/v1/updates/lucinanet/stable/artefacts/".into(),
            format!("/v1/updates/lucinanet/stable/artefacts//{sha}.bin"),
            format!("/v1/updates/lucinanet/stable/artefacts/{}.bin", &sha[..63]),
            format!(
                "/v1/updates/lucinanet/stable/artefacts/{}.bin",
                "A".repeat(64)
            ),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}.BIN"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}.bin.gz.x"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}.verylongext"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}.bin/x"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}.bin-x"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}x.bin"),
            format!("/v1/updates/lucinanet/stable/artefacts/{sha}."),
            format!("/v1/updates/-x/stable/artefacts/{sha}.bin"),
            format!("/v1/updates/{}/stable/manifest", "a".repeat(33)),
            "/v1/updates/Lucinanet/stable/manifest".into(),
            "/v1/updates/lucinanet/stable_1/manifest".into(),
            "/v1/updates//stable/manifest".into(),
        ] {
            assert_eq!(parse_route(&bad), None, "{bad:?}");
        }
    }
}
