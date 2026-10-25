//! Durable share manifests and streaming multipart uploads; no file bodies in memory.
use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{journal::Journal, net::ORIGIN, Result};

const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_PARTS: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    share_id: String,
    #[serde(default = "ask")]
    intent: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    texts: Vec<String>,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    files: Vec<SharedFile>,
}

#[derive(Deserialize)]
struct SharedFile {
    path: PathBuf,
    name: String,
    #[serde(default)]
    mime: String,
}

fn ask() -> String {
    "ask".into()
}

/// A sequence of small headers/fields and open files. Only the current reader is read.
struct Multipart {
    readers: VecDeque<Box<dyn Read>>,
    len: u64,
    parts: usize,
}

impl Read for Multipart {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while let Some(reader) = self.readers.front_mut() {
            let n = reader.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            self.readers.pop_front();
        }
        Ok(0)
    }
}

impl Multipart {
    fn push_bytes(&mut self, bytes: Vec<u8>) {
        self.len += bytes.len() as u64;
        self.readers.push_back(Box::new(Cursor::new(bytes)));
    }

    fn field(&mut self, boundary: &str, name: &str, value: &str) {
        self.parts += 1;
        self.push_bytes(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").into_bytes());
    }
}

fn inside(path: &Path, root: &Path) -> Result<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Invalid share path.".into());
    }
    let canonical = path.canonicalize().map_err(|_| "Share file unavailable.")?;
    if !canonical.starts_with(root) || canonical == root {
        return Err("Invalid share path.".into());
    }
    Ok(canonical)
}

fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn single_url(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://"))
        && !s.chars().any(char::is_whitespace)
        && s.parse::<ureq::http::Uri>()
            .is_ok_and(|uri| uri.host().is_some())
}

fn multipart(m: &Manifest, root: &Path, boundary: &str) -> Result<Multipart> {
    if !uuid(&m.share_id)
        || !["ask", "library", "shopping", "note", "remind"].contains(&m.intent.as_str())
        || m.note.chars().count() > 4000
    {
        return Err("Invalid share details.".into());
    }
    if m.texts.iter().chain(&m.urls).all(|s| s.trim().is_empty()) && m.files.is_empty() {
        return Err("Nothing to share.".into());
    }
    let mut body = Multipart {
        readers: VecDeque::new(),
        len: 0,
        parts: 0,
    };
    for (name, value) in [
        ("shareId", m.share_id.as_str()),
        ("intent", &m.intent),
        ("note", &m.note),
        ("lang", &m.lang),
        ("title", &m.title),
        ("channel", "web"),
        ("source", "android-share"),
    ] {
        if !value.is_empty() {
            body.field(boundary, name, value);
        }
    }
    for text in &m.texts {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            body.field(
                boundary,
                if single_url(trimmed) { "url" } else { "text" },
                if single_url(trimmed) { trimmed } else { text },
            );
        }
    }
    for url in &m.urls {
        if !url.trim().is_empty() {
            body.field(boundary, "url", url.trim());
        }
    }
    for file in &m.files {
        let path = inside(&file.path, root)?;
        let reader = File::open(path).map_err(|_| "Share file unavailable.")?;
        let meta = reader.metadata().map_err(|_| "Share file unavailable.")?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err("Share exceeds 2 GB.".into());
        }
        let name: String = file
            .name
            .chars()
            .map(|c| {
                if c.is_control() || ['"', '\\', '/'].contains(&c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let mime = if file.mime.is_empty() {
            "application/octet-stream"
        } else {
            &file.mime
        };
        if !mime.is_ascii()
            || !mime.contains('/')
            || mime.bytes().any(|c| c.is_ascii_control() || c == b'"')
        {
            return Err("Invalid file type.".into());
        }
        if [
            "image/heic",
            "image/heif",
            "image/heic-sequence",
            "image/heif-sequence",
        ]
        .contains(&mime)
        {
            return Err("Convert HEIC/HEIF to JPEG before sharing.".into());
        }
        body.parts += 1;
        body.push_bytes(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes());
        body.len += meta.len();
        body.readers.push_back(Box::new(reader.take(meta.len())));
        body.push_bytes(b"\r\n".to_vec());
    }
    body.push_bytes(format!("--{boundary}--\r\n").into_bytes());
    if body.parts > MAX_PARTS || body.len > MAX_BYTES {
        return Err("Share exceeds 32 parts or 2 GB.".into());
    }
    Ok(body)
}

/// Injectable streaming transport. Errors represent an interrupted/network request.
pub trait Backend {
    /// Send the body with the computed Content-Length and return its HTTP status.
    fn post(&self, boundary: &str, length: u64, body: &mut dyn Read) -> Result<u16>;
}

/// Fixed-origin cookie-authenticated upload transport.
pub struct Https<'a>(pub &'a str);

impl Backend for Https<'_> {
    fn post(&self, boundary: &str, length: u64, body: &mut dyn Read) -> Result<u16> {
        static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
        let agent = AGENT.get_or_init(|| {
            let tls = ureq::tls::TlsConfig::builder()
                .unversioned_rustls_crypto_provider(Arc::new(rustls_graviola::default_provider()))
                .build();
            ureq::Agent::config_builder()
                .tls_config(tls)
                .https_only(true)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(30 * 60)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .new_agent()
        });
        agent
            .post(format!("{ORIGIN}/v1/share"))
            .header("Cookie", self.0)
            .header("Origin", ORIGIN)
            .header("X-Secretary-CSRF", "1")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header("Content-Length", length.to_string())
            .send(ureq::SendBody::from_reader(body))
            .map(|r| r.status().as_u16())
            .map_err(|_| "Upload interrupted. Will retry.".into())
    }
}

/// Retry only transient failures; a missing status is a network error.
pub fn retryable(status: Option<u16>) -> bool {
    status.is_none_or(|s| s == 408 || s == 429 || (500..600).contains(&s))
}

fn outcome(id: &str, status: u16, error: Option<&str>) -> Value {
    let ok = status == 202;
    let mut result = json!({"ok": ok, "status": status, "retry": !ok && retryable(if status == 0 { None } else { Some(status) }), "shareId": id});
    if let Some(error) = error {
        result["error"] = json!(error);
    }
    result
}

/// Read and validate a staged share, then make one streaming request. No secrets are journaled.
pub fn upload(files_dir: &str, cookie: &str, manifest_path: &str, backend: &dyn Backend) -> Value {
    let mut id = String::new();
    let result = (|| -> Result<Value> {
        let files = Path::new(files_dir)
            .canonicalize()
            .map_err(|_| "Storage unavailable.")?;
        let cache = files
            .parent()
            .ok_or("Storage unavailable.")?
            .join("cache")
            .canonicalize()
            .map_err(|_| "Storage unavailable.")?;
        let root = inside(&cache.join("shares"), &cache)?;
        let manifest = inside(Path::new(manifest_path), &root)?;
        let file = File::open(&manifest).map_err(|_| "Share manifest unavailable.")?;
        if file
            .metadata()
            .map_err(|_| "Share manifest unavailable.")?
            .len()
            > 1024 * 1024
        {
            return Err("Share manifest exceeds limit.".into());
        }
        let m: Manifest = serde_json::from_reader(file).map_err(|_| "Invalid share manifest.")?;
        id.clone_from(&m.share_id);
        let boundary = format!("lucina-{}", m.share_id);
        let mut body = multipart(
            &m,
            manifest.parent().ok_or("Invalid share path.")?,
            &boundary,
        )?;
        if cookie.trim().is_empty() {
            return Ok(outcome(&id, 401, Some("Please sign in to Secretary.")));
        }
        if cookie.contains(['\r', '\n']) {
            return Err("Invalid session cookie.".into());
        }
        let journal = Journal::open(files_dir).ok();
        let record = journal
            .as_ref()
            .and_then(|j| j.begin("share", "Lucina").ok());
        if let (Some(j), Some(record)) = (&journal, &record) {
            let _ = j.append(
                record,
                "user",
                &format!("Share {} · {} · {} parts", m.share_id, m.intent, body.parts),
            );
        }
        let (status, error) = match backend.post(&boundary, body.len, &mut body) {
            Ok(202) => (202, None),
            Ok(401) => (401, Some("Please sign in to Secretary.")),
            Ok(s) => (s, Some("Share was not accepted.")),
            Err(_) => (0, Some("Upload interrupted. Will retry.")),
        };
        if let (Some(j), Some(record)) = (journal, record) {
            let _ = j.end(
                &record,
                if status == 202 {
                    "sent"
                } else if retryable(if status == 0 { None } else { Some(status) }) {
                    "retry"
                } else {
                    "failed"
                },
            );
        }
        Ok(outcome(&id, status, error))
    })();
    result.unwrap_or_else(|e| outcome(&id, 400, Some(&e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs};

    const ID: &str = "12345678-1234-4321-8123-123456789abc";
    fn fixture() -> (PathBuf, Manifest) {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir()
            .join(format!(
                "lucina-share-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
            .join("tech.getsecretary.secretary");
        fs::create_dir_all(dir.join("files")).unwrap();
        let root = dir.join("cache/shares").join(ID);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), b"abc").unwrap();
        let m = json!({"shareId": ID, "texts": ["hello"], "urls": ["https://example.com/"],
            "files": [{"path": root.join("a.txt"), "name": "a.txt", "mime": "text/plain"}]});
        fs::write(root.join("manifest.json"), m.to_string()).unwrap();
        (dir, serde_json::from_value(m).unwrap())
    }

    #[test]
    fn exact_stream_and_length() {
        let (dir, m) = fixture();
        let mut body = multipart(&m, &dir.join("cache/shares"), "fixed").unwrap();
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        let expected = format!(concat!(
            "--fixed\r\nContent-Disposition: form-data; name=\"shareId\"\r\n\r\n{}\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"intent\"\r\n\r\nask\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"channel\"\r\n\r\nweb\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"source\"\r\n\r\nandroid-share\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\nhello\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"url\"\r\n\r\nhttps://example.com/\r\n",
            "--fixed\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nabc\r\n",
            "--fixed--\r\n"), ID);
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(body.len, bytes.len() as u64);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn traversal_and_symlink_refused() {
        let (dir, mut m) = fixture();
        let root = dir.join("cache/shares");
        m.files[0].path = root.join("../outside");
        assert!(multipart(&m, &root, "x").is_err());
        m.files[0].path = dir.join("outside");
        fs::write(&m.files[0].path, "private").unwrap();
        assert!(multipart(&m, &root, "x").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&m.files[0].path, root.join("link")).unwrap();
            m.files[0].path = root.join("link");
            assert!(multipart(&m, &root, "x").is_err());
        }
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn retry_classification() {
        for s in [None, Some(408), Some(429), Some(500), Some(503)] {
            assert!(retryable(s));
        }
        for s in [202, 301, 400, 401, 403, 413, 422] {
            assert!(!retryable(Some(s)));
        }
    }

    struct Fake {
        status: Option<u16>,
        calls: Cell<usize>,
    }
    impl Backend for Fake {
        fn post(&self, boundary: &str, length: u64, body: &mut dyn Read) -> Result<u16> {
            self.calls.set(self.calls.get() + 1);
            assert!(boundary.starts_with("lucina-"));
            assert_eq!(io::copy(body, &mut io::sink()).unwrap(), length);
            self.status.ok_or("network".into())
        }
    }

    #[test]
    fn upload_without_network_and_signed_out() {
        let (dir, _) = fixture();
        let manifest = dir.join("cache/shares").join(ID).join("manifest.json");
        for status in [
            Some(202),
            Some(400),
            Some(401),
            Some(408),
            Some(413),
            Some(429),
            Some(503),
            None,
        ] {
            let fake = Fake {
                status,
                calls: Cell::new(0),
            };
            let r = upload(
                dir.join("files").to_str().unwrap(),
                "session=x",
                manifest.to_str().unwrap(),
                &fake,
            );
            assert_eq!(r["ok"], status == Some(202));
            assert_eq!(r["retry"], retryable(status));
            assert_eq!(r["shareId"], ID);
            assert_eq!(fake.calls.get(), 1);
            let signed_out = upload(
                dir.join("files").to_str().unwrap(),
                "",
                manifest.to_str().unwrap(),
                &fake,
            );
            assert_eq!(signed_out["status"], 401);
            assert_eq!(fake.calls.get(), 1);
        }
        let journal: Value =
            serde_json::from_slice(&fs::read(dir.join("files/calls.json")).unwrap()).unwrap();
        assert_eq!(journal.as_array().unwrap().len(), 8);
        assert_eq!(journal[0]["kind"], "share");
        assert_eq!(journal[0]["status"], "sent");
        assert!(!journal.to_string().contains("session=x"));
        assert!(!journal.to_string().contains("hello"));
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_manifest_paths_never_reach_transport() {
        let (dir, _) = fixture();
        let fake = Fake {
            status: Some(202),
            calls: Cell::new(0),
        };
        let original = dir.join("cache/shares").join(ID).join("manifest.json");
        let outside = dir.join("manifest.json");
        fs::copy(&original, &outside).unwrap();
        for path in [
            outside,
            original
                .parent()
                .unwrap()
                .join("../")
                .join(ID)
                .join("manifest.json"),
        ] {
            let r = upload(
                dir.join("files").to_str().unwrap(),
                "session=x",
                path.to_str().unwrap(),
                &fake,
            );
            assert_eq!(r["ok"], false);
            assert_eq!(r["retry"], false);
        }
        assert_eq!(fake.calls.get(), 0);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn limits_and_url_normalization() {
        let (dir, mut m) = fixture();
        let root = dir.join("cache/shares");
        m.texts = vec![" https://example.com/ ".into()];
        let mut b = multipart(&m, &root, "fixed").unwrap();
        let mut text = String::new();
        b.read_to_string(&mut text).unwrap();
        assert!(!text.contains("name=\"text\""));
        m.note = "x".repeat(4001);
        assert!(multipart(&m, &root, "x").is_err());
        m.note.clear();
        m.texts = vec!["a".into(); 33];
        assert!(multipart(&m, &root, "x").is_err());
        m.texts.clear();
        File::options()
            .write(true)
            .open(&m.files[0].path)
            .unwrap()
            .set_len(MAX_BYTES)
            .unwrap();
        assert!(multipart(&m, &root, "x").is_err());
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
