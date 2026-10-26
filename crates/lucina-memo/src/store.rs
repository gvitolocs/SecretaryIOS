//! On-disk memo store: `<root>/memos/<id>/` holds `meta.json`, the audio
//! (`audio.part` while uploading, then `audio.<ext>`), `transcript.json`,
//! `summary.json`, `memo.html` and `memo.pdf`. Deleted memos move to
//! `<root>/trash/` (never removed here). JSON writes are atomic, mode 0600.
//!
//! Uploads resume from the length of `audio.part` on disk: a chunk that broke
//! halfway leaves its bytes, and the client continues from the offset it
//! reads back (`receivedBytes`).
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::model::{Memo, Status, Steps};

/// Largest accepted recording (about 18 hours of 128 kbit/s AAC).
pub const MAX_AUDIO_BYTES: u64 = 1 << 30;
/// Largest single upload request.
pub const MAX_CHUNK_BYTES: u64 = 16 << 20;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Invalid(String),
    #[error("memo not found")]
    NotFound,
    /// The client's offset is not where the file ends; carries the real one.
    #[error("upload offset mismatch: server has {0} bytes")]
    Offset(u64),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// What a device sends to start a memo.
#[derive(Debug, Clone)]
pub struct NewMemo {
    pub client_id: String,
    pub title: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub timezone: Option<String>,
    pub duration_sec: f64,
    pub mime: String,
    pub size_bytes: u64,
    pub source: String,
}

#[derive(Default)]
struct Inner {
    memos: HashMap<String, Memo>,
    by_client: HashMap<String, String>,
}

pub struct Store {
    root: PathBuf,
    inner: Mutex<Inner>,
    uploads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// `audio/mp4; codecs=...` → (`audio/mp4`, `m4a`). None = not an accepted type.
pub fn audio_type(mime: &str) -> Option<(&'static str, &'static str)> {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match base.as_str() {
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/mp4a-latm" => {
            ("audio/mp4", "m4a")
        }
        // ADTS AAC: the iPhone records it because a cut-off file still decodes (worker step 0 remuxes it to .m4a).
        "audio/aac" | "audio/x-aac" | "audio/aacp" => ("audio/aac", "aac"),
        "audio/webm" | "video/webm" => ("audio/webm", "webm"),
        "audio/ogg" | "audio/opus" => ("audio/ogg", "ogg"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => ("audio/wav", "wav"),
        "audio/mpeg" | "audio/mp3" => ("audio/mpeg", "mp3"),
        "audio/flac" | "audio/x-flac" => ("audio/flac", "flac"),
        _ => return None,
    })
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("mm_")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_client_id(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn clean_title(t: &str) -> String {
    let one_line: String = t
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let squashed = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    squashed.chars().take(160).collect()
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("system randomness");
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("mm_{hex}")
}

/// Atomic JSON write (temp file + rename), mode 0600.
pub fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    write_atomic(path, &bytes)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        let mut f = open_private(&tmp, true)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn open_private(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true);
    if truncate {
        o.truncate(true);
    } else {
        o.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> std::io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Store {
    /// Loads every memo under `root/memos`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("memos"))?;
        std::fs::create_dir_all(root.join("trash"))?;
        let mut inner = Inner::default();
        for entry in std::fs::read_dir(root.join("memos"))? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) {
                continue;
            }
            let meta = entry.path().join("meta.json");
            let mut m: Memo = match read_json(&meta) {
                Ok(Some(m)) => m,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(%id, error = %e, "unreadable memo meta, skipped");
                    continue;
                }
            };
            if m.status == Status::Uploading {
                let len =
                    std::fs::metadata(entry.path().join("audio.part")).map_or(0, |md| md.len());
                if len != m.received_bytes {
                    m.received_bytes = len;
                    write_json(&meta, &m)?;
                }
            }
            inner.by_client.insert(m.client_id.clone(), m.id.clone());
            inner.memos.insert(m.id.clone(), m);
        }
        Ok(Self {
            root,
            inner: Mutex::new(inner),
            uploads: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join("memos").join(id)
    }
    pub fn part_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("audio.part")
    }
    pub fn audio_path(&self, m: &Memo) -> PathBuf {
        self.dir(&m.id).join(format!("audio.{}", m.ext))
    }
    pub fn transcript_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("transcript.json")
    }
    /// The transcript the recording device made itself (short memos, iPhone).
    pub fn device_transcript_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("device-transcript.json")
    }
    pub fn summary_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("summary.json")
    }
    pub fn pdf_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("memo.pdf")
    }
    pub fn html_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("memo.html")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Creates a memo, or returns the existing one with the same `client_id`
    /// (a retried create). The bool is true when it was created now.
    pub fn create(&self, n: NewMemo) -> Result<(Memo, bool)> {
        if !valid_client_id(&n.client_id) {
            return Err(StoreError::Invalid(
                "clientId: 1-100 letters, digits, '-' or '_'".into(),
            ));
        }
        let (mime, ext) = audio_type(&n.mime)
            .ok_or_else(|| StoreError::Invalid(format!("unsupported audio type {:?}", n.mime)))?;
        if n.size_bytes == 0 || n.size_bytes > MAX_AUDIO_BYTES {
            return Err(StoreError::Invalid("sizeBytes: 1 byte to 1 GiB".into()));
        }
        if !n.duration_sec.is_finite() || n.duration_sec < 0.0 || n.duration_sec > 86_400.0 {
            return Err(StoreError::Invalid("durationSec: 0 to 86400".into()));
        }
        if let Some(tz) = &n.timezone {
            if tz.parse::<chrono_tz::Tz>().is_err() {
                return Err(StoreError::Invalid("timezone: an IANA zone".into()));
            }
        }
        let mut g = self.lock();
        if let Some(id) = g.by_client.get(&n.client_id) {
            if let Some(m) = g.memos.get(id) {
                return Ok((m.clone(), false));
            }
        }
        let now = Utc::now();
        let id = loop {
            let id = new_id();
            if !g.memos.contains_key(&id) {
                break id;
            }
        };
        let title = n
            .title
            .as_deref()
            .map(clean_title)
            .filter(|t| !t.is_empty());
        let m = Memo {
            id: id.clone(),
            client_id: n.client_id.clone(),
            // A title the device sends is the owner's (typed or chosen there); Lucina names the rest.
            title_source: if title.is_some() { "owner" } else { "auto" }.into(),
            title: title.unwrap_or_else(|| "Voice memo".into()),
            recorded_at: n.recorded_at,
            timezone: n.timezone,
            created_at: now,
            updated_at: now,
            duration_sec: n.duration_sec,
            mime: mime.into(),
            ext: ext.into(),
            size_bytes: n.size_bytes,
            received_bytes: 0,
            sha256: None,
            source: n.source,
            status: Status::Uploading,
            error: None,
            attempts: 0,
            next_attempt_at: None,
            language: None,
            engine: None,
            preview: None,
            action_items: 0,
            tags: vec![],
            obsidian_note: None,
            steps: Steps::default(),
            revision: 0,
            transcript_source: None,
            upgrade_attempts: 0,
            notify_telegram_rev: None,
        };
        let dir = self.dir(&id);
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        open_private(&self.part_path(&id), true)?;
        write_json(&dir.join("meta.json"), &m)?;
        g.by_client.insert(m.client_id.clone(), id.clone());
        g.memos.insert(id, m.clone());
        Ok((m, true))
    }

    pub fn get(&self, id: &str) -> Option<Memo> {
        self.lock().memos.get(id).cloned()
    }

    /// Newest recording first.
    pub fn list(&self) -> Vec<Memo> {
        let mut v: Vec<Memo> = self.lock().memos.values().cloned().collect();
        v.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at).then(b.id.cmp(&a.id)));
        v
    }

    /// Applies `f` to the memo and saves it. Returns the new record.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Memo)) -> Result<Memo> {
        let mut g = self.lock();
        let m = g.memos.get_mut(id).ok_or(StoreError::NotFound)?;
        let mut next = m.clone();
        f(&mut next);
        debug_assert!(
            crate::kernel::status_allowed(m.status, next.status),
            "status {:?} -> {:?} is not a transition of the pipeline",
            m.status,
            next.status
        );
        next.updated_at = Utc::now();
        write_json(&self.dir(id).join("meta.json"), &next)?;
        *m = next.clone();
        Ok(next)
    }

    fn upload_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.uploads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(id.to_owned())
            .or_default()
            .clone()
    }

    /// Appends one chunk at `offset`, which must be where the file ends.
    /// Bytes are kept even when the stream breaks halfway; the returned
    /// memo's `received_bytes` is the next offset either way.
    pub async fn append<S, E>(&self, id: &str, offset: u64, mut body: S) -> Result<Memo>
    where
        S: Stream<Item = std::result::Result<axum::body::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let lock = self.upload_lock(id);
        let _held = lock.lock().await;
        let m = self.get(id).ok_or(StoreError::NotFound)?;
        if m.status != Status::Uploading {
            return Err(StoreError::Conflict(
                "the upload is already complete".into(),
            ));
        }
        let path = self.part_path(id);
        let on_disk = tokio::fs::metadata(&path).await.map_or(0, |md| md.len());
        if offset != on_disk {
            if on_disk != m.received_bytes {
                self.update(id, |m| m.received_bytes = on_disk)?;
            }
            return Err(StoreError::Offset(on_disk));
        }
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await?;
        let mut written = on_disk;
        let mut failure: Option<StoreError> = None;
        let mut in_request = 0u64;
        while let Some(next) = body.next().await {
            match next {
                Ok(bytes) => {
                    in_request += bytes.len() as u64;
                    if written + bytes.len() as u64 > m.size_bytes || in_request > MAX_CHUNK_BYTES {
                        failure = Some(StoreError::Invalid(format!(
                            "chunk goes past sizeBytes ({}) or the {} MiB request limit",
                            m.size_bytes,
                            MAX_CHUNK_BYTES >> 20
                        )));
                        break;
                    }
                    if let Err(e) = f.write_all(&bytes).await {
                        failure = Some(e.into());
                        break;
                    }
                    written += bytes.len() as u64;
                }
                Err(e) => {
                    tracing::info!(%id, error = %e, "upload stream broke; keeping {written} bytes");
                    break;
                }
            }
        }
        f.flush().await?;
        f.sync_data().await?;
        drop(f);
        let m = self.update(id, |m| m.received_bytes = written)?;
        match failure {
            Some(e) => Err(e),
            None => Ok(m),
        }
    }

    /// Ends the upload: the file must be exactly `size_bytes` (and match
    /// `sha256` when given). Then the memo is queued for the pipeline.
    pub async fn complete(&self, id: &str, sha256: Option<&str>) -> Result<Memo> {
        let lock = self.upload_lock(id);
        let _held = lock.lock().await;
        let m = self.get(id).ok_or(StoreError::NotFound)?;
        if m.status != Status::Uploading {
            return Ok(m);
        }
        let part = self.part_path(id);
        let len = tokio::fs::metadata(&part).await.map_or(0, |md| md.len());
        if len != m.size_bytes {
            return Err(StoreError::Offset(len));
        }
        let digest = sha256_file(&part).await?;
        if let Some(want) = sha256 {
            if !want.eq_ignore_ascii_case(&digest) {
                // The bytes are wrong somewhere: start the upload over.
                tokio::fs::File::create(&part).await?;
                self.update(id, |m| m.received_bytes = 0)?;
                return Err(StoreError::Conflict(
                    "sha256 mismatch; the upload was reset to 0".into(),
                ));
            }
        }
        let target = self.audio_path(&m);
        tokio::fs::rename(&part, &target).await?;
        self.update(id, |m| {
            m.sha256 = Some(digest);
            m.received_bytes = len;
            m.status = Status::Queued;
            m.error = None;
        })
    }

    /// Moves the memo's folder to `trash/` and forgets it.
    pub fn trash(&self, id: &str) -> Result<()> {
        let mut g = self.lock();
        let m = g.memos.remove(id).ok_or(StoreError::NotFound)?;
        g.by_client.remove(&m.client_id);
        let to = self
            .root
            .join("trash")
            .join(format!("{id}-{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
        std::fs::rename(self.dir(id), to)?;
        Ok(())
    }
}

pub async fn sha256_file(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn new_memo(client: &str, size: u64) -> NewMemo {
        NewMemo {
            client_id: client.into(),
            title: Some("  Call  with\nAnna ".into()),
            recorded_at: Utc::now(),
            timezone: Some("Europe/Copenhagen".into()),
            duration_sec: 3.0,
            mime: "audio/mp4".into(),
            size_bytes: size,
            source: "ios".into(),
        }
    }

    fn chunks(
        parts: &[&'static [u8]],
    ) -> impl Stream<Item = std::result::Result<axum::body::Bytes, String>> + Unpin {
        stream::iter(
            parts
                .iter()
                .map(|p| Ok(axum::body::Bytes::from_static(p)))
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test]
    async fn create_is_idempotent_and_upload_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (m, created) = s.create(new_memo("c1", 6)).unwrap();
        assert!(created);
        assert_eq!(m.title, "Call with Anna");
        let (again, created) = s.create(new_memo("c1", 6)).unwrap();
        assert!(!created);
        assert_eq!(again.id, m.id);

        let m1 = s.append(&m.id, 0, chunks(&[b"abc"])).await.unwrap();
        assert_eq!(m1.received_bytes, 3);
        // A retry of the same chunk is refused with the real offset.
        match s.append(&m.id, 0, chunks(&[b"abc"])).await {
            Err(StoreError::Offset(3)) => {}
            other => panic!("{other:?}"),
        }
        // Past the declared size: refused, nothing beyond it written.
        assert!(s.append(&m.id, 3, chunks(&[b"defg"])).await.is_err());
        assert!(s.complete(&m.id, None).await.is_err());
        assert_eq!(s.get(&m.id).unwrap().received_bytes, 3);
        let m2 = s.append(&m.id, 3, chunks(&[b"def"])).await.unwrap();
        assert_eq!(m2.received_bytes, 6);
        let want = {
            let mut h = Sha256::new();
            h.update(b"abcdef");
            h.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let done = s.complete(&m.id, Some(&want)).await.unwrap();
        assert_eq!(done.status, Status::Queued);
        assert_eq!(std::fs::read(s.audio_path(&done)).unwrap(), b"abcdef");

        // Reopen: the record survives.
        drop(s);
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.get(&m.id).unwrap().status, Status::Queued);
        s.trash(&m.id).unwrap();
        assert!(s.get(&m.id).is_none());
    }

    #[tokio::test]
    async fn bad_hash_resets_the_upload() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let (m, _) = s.create(new_memo("c2", 3)).unwrap();
        s.append(&m.id, 0, chunks(&[b"xyz"])).await.unwrap();
        assert!(matches!(
            s.complete(&m.id, Some("00")).await,
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(s.get(&m.id).unwrap().received_bytes, 0);
    }

    #[test]
    fn types_and_ids() {
        assert_eq!(
            audio_type("audio/webm;codecs=opus"),
            Some(("audio/webm", "webm"))
        );
        assert_eq!(audio_type("application/pdf"), None);
        assert_eq!(audio_type("audio/aac"), Some(("audio/aac", "aac")));
        assert_eq!(audio_type("audio/mp4"), Some(("audio/mp4", "m4a")));
        assert!(valid_id("mm_0123456789abcdef"));
        assert!(!valid_id("mm_0123456789abcdeg"));
        assert!(!valid_id("../etc/passwd"));
    }
}
