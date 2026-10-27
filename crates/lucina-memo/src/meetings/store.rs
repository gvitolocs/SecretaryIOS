//! The meetings archive on the MyBook:
//! `<base>/<YYYY>/<YYYY-MM-DD HHmm> <company or calendar title> – <title>/` holds the tracks
//! (`.<name>.part` while uploading), `meeting.json`, `transcript.json`, `transcript.md`,
//! `speakers.json`, `brief.md`, and `.work/` (intermediate files, kept until done).
//!
//! The base directory must exist (the MyBook is mounted); it is never created here, so a
//! missing mount never fills the system disk. JSON writes are atomic (temp file + rename,
//! mode 0600), and a restart rebuilds the index from every `meeting.json` and the length of
//! every `.part` file: an upload resumes where the bytes on disk end.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use futures_util::{Stream, StreamExt};
use tokio::io::AsyncWriteExt;

use super::kernel::{self, Chunk, Range};
use super::model::{track_name, Meeting, SourceApp, Status, Steps, Track};
use crate::store::{read_json, sha256_file, write_json, StoreError};

pub type Result<T> = std::result::Result<T, StoreError>;

/// Largest audio track (about 70 hours of 128 kbit/s AAC) and video track.
pub const MAX_AUDIO_BYTES: u64 = 4 << 30;
pub const MAX_VIDEO_BYTES: u64 = 64 << 30;
/// Largest single `PUT` body (the Mac sends 8 MiB).
pub const MAX_CHUNK_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone)]
pub struct NewTrack {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub mime: String,
    pub start_offset_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewMeeting {
    pub idempotency_key: Option<String>,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub source_app: SourceApp,
    pub calendar_event_id: Option<String>,
    pub tracks: Vec<NewTrack>,
    /// "company or calendar title" for the folder name (see [`folder_name`]).
    pub folder_prefix: Option<String>,
    pub company: Option<String>,
    pub event: Option<crate::model::EventLink>,
}

/// What a `PUT …/tracks/{name}` did.
#[derive(Debug, Clone, PartialEq)]
pub enum PutResult {
    /// 308: bytes stored so far.
    Partial(u64),
    /// 200: whole and verified (the digest is the declared one).
    Complete { bytes: u64, sha256: String },
    /// 200 with another digest: the bytes differ from the client's file. The track was reset
    /// to 0 so a retry uploads it again; the client reports the mismatch.
    DigestMismatch { bytes: u64, sha256: String },
    /// 409: the stored offset is elsewhere.
    Mismatch(u64),
    /// 416.
    Unsatisfiable(u64),
}

/// `bytes */123` or `bytes 0-99/123`.
pub fn parse_content_range(h: &str) -> Option<Range> {
    let rest = h.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let total: u64 = total.trim().parse().ok()?;
    let range = range.trim();
    if range == "*" {
        return Some(Range::Query { total });
    }
    let (a, b) = range.split_once('-')?;
    Some(Range::Bytes {
        start: a.trim().parse().ok()?,
        end: b.trim().parse().ok()?,
        total,
    })
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 20
        && id.starts_with("mtg_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn new_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("system randomness");
    format!(
        "mtg_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// `<YYYY-MM-DD HHmm> <prefix> – <title>` in the owner's zone; the prefix is left out when it
/// is missing or the same as the title. Safe as one path component (and as an Obsidian name).
pub fn folder_name(start: DateTime<Utc>, tz: Tz, prefix: Option<&str>, title: &str) -> String {
    let when = start.with_timezone(&tz).format("%Y-%m-%d %H%M");
    let clean = |s: &str| {
        let s = crate::obsidian::safe_name(s);
        s.chars().take(60).collect::<String>().trim().to_owned()
    };
    let title = clean(title);
    match prefix
        .map(clean)
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case(&title))
    {
        Some(p) => format!("{when} {p} – {title}"),
        None => format!("{when} {title}"),
    }
}

/// The audio MIME types the recorder sends (and close relatives).
fn mime_ok(name: &str, mime: &str) -> bool {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if name == "video.mp4" {
        matches!(base.as_str(), "video/mp4" | "video/quicktime")
    } else {
        matches!(
            base.as_str(),
            "audio/mp4"
                | "audio/m4a"
                | "audio/x-m4a"
                | "audio/aac"
                | "audio/mpeg"
                | "audio/wav"
                | "audio/x-wav"
                | "audio/webm"
        )
    }
}

#[derive(Default)]
struct Inner {
    meetings: HashMap<String, Meeting>,
    by_key: HashMap<String, String>,
}

pub struct MeetingStore {
    base: PathBuf,
    tz: Tz,
    inner: Mutex<Inner>,
    uploads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl MeetingStore {
    /// Loads every meeting under `base` (which must exist).
    pub fn open(base: &Path, tz: Tz) -> Result<Self> {
        if !base.is_dir() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} does not exist (is the MyBook mounted?)", base.display()),
            )));
        }
        let mut inner = Inner::default();
        for year in std::fs::read_dir(base)? {
            let year = year?;
            let yname = year.file_name().to_string_lossy().into_owned();
            if yname.len() != 4
                || !yname.bytes().all(|b| b.is_ascii_digit())
                || !year.path().is_dir()
            {
                continue;
            }
            for entry in std::fs::read_dir(year.path())? {
                let dir = entry?.path();
                let meta = dir.join("meeting.json");
                let mut m: Meeting = match read_json(&meta) {
                    Ok(Some(m)) => m,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(dir = %dir.display(), error = %e, "unreadable meeting.json, skipped");
                        continue;
                    }
                };
                if !valid_id(&m.id) {
                    continue;
                }
                // The folder is where the file is (a moved archive still loads).
                m.folder = format!(
                    "{yname}/{}",
                    dir.file_name().and_then(|n| n.to_str()).unwrap_or_default()
                );
                let mut changed = false;
                for t in m.tracks.iter_mut().filter(|t| !t.whole()) {
                    let len =
                        std::fs::metadata(dir.join(part_name(&t.name))).map_or(0, |md| md.len());
                    if len != t.received {
                        t.received = len;
                        changed = true;
                    }
                }
                if changed {
                    write_json(&meta, &m)?;
                }
                if let Some(k) = &m.idempotency_key {
                    inner.by_key.insert(k.clone(), m.id.clone());
                }
                inner.meetings.insert(m.id.clone(), m);
            }
        }
        Ok(Self {
            base: base.to_path_buf(),
            tz,
            inner: Mutex::new(inner),
            uploads: Mutex::new(HashMap::new()),
        })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }
    pub fn dir(&self, m: &Meeting) -> PathBuf {
        self.base.join(&m.folder)
    }
    pub fn work(&self, m: &Meeting) -> PathBuf {
        self.dir(m).join(".work")
    }
    pub fn track_path(&self, m: &Meeting, name: &str) -> PathBuf {
        self.dir(m).join(name)
    }
    fn part_path(&self, m: &Meeting, name: &str) -> PathBuf {
        self.dir(m).join(part_name(name))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, id: &str) -> Option<Meeting> {
        self.lock().meetings.get(id).cloned()
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Meeting> {
        let mut v: Vec<Meeting> = self.lock().meetings.values().cloned().collect();
        v.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        v
    }

    /// Applies `f` and saves `meeting.json`. Every status change is a kernel transition (MT1).
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Meeting)) -> Result<Meeting> {
        let mut g = self.lock();
        let m = g.meetings.get_mut(id).ok_or(StoreError::NotFound)?;
        let mut next = m.clone();
        f(&mut next);
        if !kernel::status_allowed(m.status, next.status) {
            return Err(StoreError::Conflict(format!(
                "status {:?} -> {:?} is not a transition of the pipeline",
                m.status, next.status
            )));
        }
        next.updated_at = Utc::now();
        write_json(&self.base.join(&next.folder).join("meeting.json"), &next)?;
        *m = next.clone();
        Ok(next)
    }

    /// Creates a meeting, or returns the one with the same idempotency key. True when new.
    pub fn create(&self, n: NewMeeting) -> Result<(Meeting, bool)> {
        let bad = |m: &str| Err(StoreError::Invalid(m.into()));
        if let Some(k) = &n.idempotency_key {
            if !(1..=128).contains(&k.len())
                || !k
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return bad("Idempotency-Key: 1-128 letters, digits, '-' or '_'");
            }
        }
        let title = crate::store::clean_title(&n.title);
        if title.is_empty() {
            return bad("title: 1 to 160 characters");
        }
        if n.started_at > Utc::now() + chrono::Duration::days(1) {
            return bad("started_at is in the future");
        }
        if n.ended_at
            .is_some_and(|e| e < n.started_at || e > n.started_at + chrono::Duration::hours(24))
        {
            return bad("ended_at must be within 24 hours after started_at");
        }
        if n.tracks.is_empty() || n.tracks.len() > 3 {
            return bad("tracks: 1 to 3");
        }
        let mut tracks = Vec::new();
        for t in &n.tracks {
            let Some(name) = track_name(&t.name) else {
                return bad("track name: mic.m4a, system.m4a or video.mp4");
            };
            if tracks.iter().any(|x: &Track| x.name == name) {
                return bad("a track is listed twice");
            }
            let max = if name == "video.mp4" {
                MAX_VIDEO_BYTES
            } else {
                MAX_AUDIO_BYTES
            };
            if t.bytes == 0 || t.bytes > max {
                return bad("track bytes: 1 byte up to 4 GiB (audio) or 64 GiB (video)");
            }
            if t.sha256.len() != 64 || !t.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
                return bad("track sha256: 64 hex digits");
            }
            if !mime_ok(name, &t.mime) {
                return bad("track mime: audio/mp4 for audio tracks, video/mp4 for video");
            }
            tracks.push(Track {
                name: name.into(),
                bytes: t.bytes,
                sha256: t.sha256.to_ascii_lowercase(),
                mime: t
                    .mime
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase(),
                start_offset_ms: t.start_offset_ms.filter(|o| (0..86_400_000).contains(o)),
                received: 0,
                verified_at: None,
                mismatches: 0,
            });
        }
        let mut g = self.lock();
        if let Some(id) = n.idempotency_key.as_ref().and_then(|k| g.by_key.get(k)) {
            if let Some(m) = g.meetings.get(id) {
                return Ok((m.clone(), false));
            }
        }
        let id = loop {
            let id = new_id();
            if !g.meetings.contains_key(&id) {
                break id;
            }
        };
        let year = n
            .started_at
            .with_timezone(&self.tz)
            .format("%Y")
            .to_string();
        let name = folder_name(n.started_at, self.tz, n.folder_prefix.as_deref(), &title);
        std::fs::create_dir_all(self.base.join(&year))?;
        // A second recording with the same name gets " 2", " 3", … (like the Mac's folders).
        let mut folder = format!("{year}/{name}");
        for k in 2.. {
            match std::fs::create_dir(self.base.join(&folder)) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && k < 1000 => {
                    folder = format!("{year}/{name} {k}");
                }
                Err(e) => return Err(e.into()),
            }
        }
        let dir = self.base.join(&folder);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        for t in &tracks {
            open_part(&dir.join(part_name(&t.name)))?;
        }
        let now = Utc::now();
        let m = Meeting {
            id: id.clone(),
            idempotency_key: n.idempotency_key.clone(),
            title,
            started_at: n.started_at,
            ended_at: n.ended_at,
            source_app: n.source_app,
            calendar_event_id: n.calendar_event_id,
            tracks,
            folder,
            status: Status::Uploading,
            error: None,
            attempts: 0,
            next_attempt_at: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            event: n.event,
            company: n.company,
            language: None,
            duration_sec: n
                .ended_at
                .map(|e| (e - n.started_at).num_milliseconds() as f64 / 1000.0)
                .unwrap_or(0.0),
            steps: Steps::default(),
            revision: 0,
            notify_telegram_rev: None,
            memory_rev: None,
            obsidian_note: None,
            analysis_source: None,
            brief_source: None,
        };
        write_json(&dir.join("meeting.json"), &m)?;
        if let Some(k) = &n.idempotency_key {
            g.by_key.insert(k.clone(), id.clone());
        }
        g.meetings.insert(id, m.clone());
        Ok((m, true))
    }

    fn upload_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.uploads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    /// One `PUT …/tracks/{name}` (kernel MT2). Bytes that arrived before a broken connection
    /// are kept; a track that reaches its size is verified against the declared SHA-256.
    pub async fn put<S, E>(
        &self,
        id: &str,
        name: &str,
        range: Range,
        mut body: S,
    ) -> Result<PutResult>
    where
        S: Stream<Item = std::result::Result<axum::body::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let name = track_name(name).ok_or(StoreError::NotFound)?;
        let lock = self.upload_lock(&format!("{id}/{name}"));
        let _held = lock.lock().await;
        let m = self.get(id).ok_or(StoreError::NotFound)?;
        let t = m.track(name).ok_or(StoreError::NotFound)?.clone();
        let part = self.part_path(&m, name);
        let stored = if t.whole() {
            t.bytes
        } else {
            tokio::fs::metadata(&part).await.map_or(0, |md| md.len())
        };
        let complete = PutResult::Complete {
            bytes: t.bytes,
            sha256: t.sha256.clone(),
        };
        let (start, end) = match kernel::chunk(stored, t.bytes, t.whole(), range) {
            Chunk::WrongTotal => {
                return Err(StoreError::Invalid(format!("{name} is {} bytes", t.bytes)));
            }
            Chunk::Complete => return Ok(complete),
            Chunk::Partial => return Ok(PutResult::Partial(stored)),
            Chunk::Mismatch => return Ok(PutResult::Mismatch(stored)),
            Chunk::Unsatisfiable => return Ok(PutResult::Unsatisfiable(stored)),
            Chunk::Append => match range {
                Range::Bytes { start, end, .. } => (start, end),
                Range::Query { .. } => return Ok(PutResult::Partial(stored)),
            },
        };
        let want = end - start + 1;
        if want > MAX_CHUNK_BYTES {
            return Err(StoreError::Invalid(format!(
                "chunks up to {} MiB",
                MAX_CHUNK_BYTES >> 20
            )));
        }
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await?;
        let mut written = 0u64;
        let mut too_long = false;
        while let Some(next) = body.next().await {
            match next {
                Ok(bytes) => {
                    let room = (want - written) as usize;
                    let take = bytes.len().min(room);
                    f.write_all(&bytes[..take]).await?;
                    written += take as u64;
                    if take < bytes.len() {
                        too_long = true;
                        break;
                    }
                }
                Err(e) => {
                    tracing::info!(%id, track = name, error = %e, "track upload broke; keeping {written} bytes");
                    break;
                }
            }
        }
        f.flush().await?;
        f.sync_data().await?;
        drop(f);
        let now_stored = stored + written;
        if too_long {
            self.update(id, |m| set_received(m, name, now_stored))?;
            return Err(StoreError::Invalid(
                "the body is longer than Content-Range".into(),
            ));
        }
        if now_stored < t.bytes {
            self.update(id, |m| set_received(m, name, now_stored))?;
            return Ok(PutResult::Partial(now_stored));
        }
        let digest = sha256_file(&part).await?;
        if digest != t.sha256 {
            tokio::fs::File::create(&part).await?;
            self.update(id, |m| {
                set_received(m, name, 0);
                if let Some(t) = m.tracks.iter_mut().find(|t| t.name == name) {
                    t.mismatches += 1;
                }
            })?;
            tracing::warn!(%id, track = name, "track digest differs from the declared one; reset to 0");
            return Ok(PutResult::DigestMismatch {
                bytes: t.bytes,
                sha256: digest,
            });
        }
        tokio::fs::rename(&part, self.track_path(&m, name)).await?;
        self.update(id, |m| {
            if let Some(t) = m.tracks.iter_mut().find(|t| t.name == name) {
                t.received = t.bytes;
                t.verified_at = Some(Utc::now());
            }
        })?;
        Ok(complete)
    }

    /// Ends the upload (MT3): every track whole and verified. Idempotent.
    pub fn complete(&self, id: &str) -> Result<Meeting> {
        let m = self.get(id).ok_or(StoreError::NotFound)?;
        if m.status != Status::Uploading {
            return Ok(m);
        }
        let verified: Vec<bool> = m.tracks.iter().map(Track::whole).collect();
        let audio: Vec<bool> = m.tracks.iter().map(Track::has_audio).collect();
        if !kernel::may_complete(&verified, &audio) {
            let missing: Vec<&str> = m
                .tracks
                .iter()
                .filter(|t| !t.whole())
                .map(|t| t.name.as_str())
                .collect();
            return Err(StoreError::Conflict(format!(
                "tracks not uploaded yet: {}",
                missing.join(", ")
            )));
        }
        self.update(id, |m| {
            m.status = Status::Queued;
            m.completed_at = Some(Utc::now());
            m.error = None;
        })
    }
}

fn set_received(m: &mut Meeting, name: &str, n: u64) {
    if let Some(t) = m.tracks.iter_mut().find(|t| t.name == name) {
        t.received = n;
    }
}

fn part_name(name: &str) -> String {
    format!(".{name}.part")
}

fn open_part(path: &Path) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use sha2::{Digest, Sha256};

    fn hex(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    fn body(
        b: &'static [u8],
    ) -> impl Stream<Item = std::result::Result<axum::body::Bytes, String>> + Unpin {
        stream::iter(vec![Ok(axum::body::Bytes::from_static(b))])
    }

    /// A body cut off after its first piece.
    fn broken(
        b: &'static [u8],
    ) -> impl Stream<Item = std::result::Result<axum::body::Bytes, String>> + Unpin {
        stream::iter(vec![
            Ok(axum::body::Bytes::from_static(b)),
            Err("reset".to_owned()),
        ])
    }

    pub fn new_meeting(key: &str, tracks: &[(&str, &[u8])]) -> NewMeeting {
        NewMeeting {
            idempotency_key: Some(key.into()),
            title: "Weekly sync".into(),
            started_at: "2026-10-26T13:30:00Z".parse().unwrap(),
            ended_at: Some("2026-10-26T14:31:02Z".parse().unwrap()),
            source_app: SourceApp {
                bundle_id: Some("com.microsoft.teams2".into()),
                name: Some("Microsoft Teams".into()),
            },
            calendar_event_id: None,
            tracks: tracks
                .iter()
                .map(|(n, b)| NewTrack {
                    name: (*n).into(),
                    bytes: b.len() as u64,
                    sha256: hex(b),
                    mime: if *n == "video.mp4" {
                        "video/mp4"
                    } else {
                        "audio/mp4"
                    }
                    .into(),
                    start_offset_ms: Some(12),
                })
                .collect(),
            folder_prefix: Some("Acme".into()),
            company: Some("Acme".into()),
            event: None,
        }
    }

    #[test]
    fn folder_names() {
        let t: DateTime<Utc> = "2026-10-26T13:30:00Z".parse().unwrap();
        let tz = chrono_tz::Europe::Copenhagen;
        assert_eq!(
            folder_name(t, tz, Some("Acme"), "Weekly sync"),
            "2026-10-26 1430 Acme – Weekly sync"
        );
        assert_eq!(
            folder_name(t, tz, None, "Weekly sync"),
            "2026-10-26 1430 Weekly sync"
        );
        assert_eq!(
            folder_name(t, tz, Some("weekly SYNC"), "Weekly sync"),
            "2026-10-26 1430 Weekly sync"
        );
        assert_eq!(
            folder_name(t, tz, Some("A/B: c"), "x/../y"),
            "2026-10-26 1430 A B c – x .. y"
        );
        let long = "x".repeat(300);
        assert!(folder_name(t, tz, Some(&long), &long).chars().count() < 140);
        assert_eq!(
            parse_content_range("bytes */100"),
            Some(Range::Query { total: 100 })
        );
        assert_eq!(
            parse_content_range("bytes 0-9/100"),
            Some(Range::Bytes {
                start: 0,
                end: 9,
                total: 100
            })
        );
        assert_eq!(parse_content_range("items 0-9/100"), None);
        assert!(valid_id("mtg_0123456789abcdef") && !valid_id("mtg_../../etc/pas"));
    }

    #[tokio::test]
    async fn resumable_upload_verifies_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let tz = chrono_tz::Europe::Copenhagen;
        let s = MeetingStore::open(dir.path(), tz).unwrap();
        let mic: &'static [u8] = b"0123456789";
        let (m, created) = s.create(new_meeting("k1", &[("mic.m4a", mic)])).unwrap();
        assert!(created);
        assert_eq!(m.folder, "2026/2026-10-26 1430 Acme – Weekly sync");
        let (again, created) = s.create(new_meeting("k1", &[("mic.m4a", mic)])).unwrap();
        assert!(!created && again.id == m.id);
        // A second recording with the same name gets its own folder.
        let (other, _) = s.create(new_meeting("k2", &[("mic.m4a", mic)])).unwrap();
        assert_eq!(other.folder, "2026/2026-10-26 1430 Acme – Weekly sync 2");

        let q = Range::Query { total: 10 };
        let b = |start, end| Range::Bytes {
            start,
            end,
            total: 10,
        };
        assert_eq!(
            s.put(&m.id, "mic", q, body(b"")).await.unwrap(),
            PutResult::Partial(0)
        );
        // The connection drops after 3 bytes of a 4-byte chunk: the 3 bytes stay.
        assert_eq!(
            s.put(&m.id, "mic.m4a", b(0, 3), broken(b"012"))
                .await
                .unwrap(),
            PutResult::Partial(3)
        );
        assert_eq!(
            s.put(&m.id, "mic.m4a", b(0, 3), body(b"0123"))
                .await
                .unwrap(),
            PutResult::Mismatch(3)
        );
        assert_eq!(
            s.put(&m.id, "mic.m4a", b(3, 10), body(b"3")).await.unwrap(),
            PutResult::Unsatisfiable(3)
        );
        assert!(s.complete(&m.id).is_err());
        // Restart: the offset comes from the file on disk.
        drop(s);
        let s = MeetingStore::open(dir.path(), tz).unwrap();
        assert_eq!(s.get(&m.id).unwrap().tracks[0].received, 3);
        assert_eq!(
            s.put(&m.id, "mic.m4a", q, body(b"")).await.unwrap(),
            PutResult::Partial(3)
        );
        let done = s
            .put(&m.id, "mic.m4a", b(3, 9), body(b"3456789"))
            .await
            .unwrap();
        assert_eq!(
            done,
            PutResult::Complete {
                bytes: 10,
                sha256: hex(mic)
            }
        );
        assert_eq!(s.put(&m.id, "mic.m4a", q, body(b"")).await.unwrap(), done);
        assert_eq!(std::fs::read(s.track_path(&m, "mic.m4a")).unwrap(), mic);
        let q2 = s.complete(&m.id).unwrap();
        assert_eq!(q2.status, Status::Queued);
        assert_eq!(s.complete(&m.id).unwrap().status, Status::Queued);
        drop(s);
        let s = MeetingStore::open(dir.path(), tz).unwrap();
        assert_eq!(s.get(&m.id).unwrap().status, Status::Queued);
        assert_eq!(s.list().len(), 2);
    }

    #[tokio::test]
    async fn a_wrong_digest_resets_the_track() {
        let dir = tempfile::tempdir().unwrap();
        let s = MeetingStore::open(dir.path(), chrono_tz::UTC).unwrap();
        let (m, _) = s
            .create(new_meeting("k", &[("system.m4a", b"abcd")]))
            .unwrap();
        let r = s
            .put(
                &m.id,
                "system",
                Range::Bytes {
                    start: 0,
                    end: 3,
                    total: 4,
                },
                body(b"abXd"),
            )
            .await
            .unwrap();
        assert!(
            matches!(r, PutResult::DigestMismatch { bytes: 4, ref sha256 } if *sha256 == hex(b"abXd"))
        );
        let t = &s.get(&m.id).unwrap().tracks[0];
        assert_eq!((t.received, t.mismatches, t.whole()), (0, 1, false));
        assert_eq!(
            s.put(&m.id, "system", Range::Query { total: 4 }, body(b""))
                .await
                .unwrap(),
            PutResult::Partial(0)
        );
    }

    #[test]
    fn create_validates() {
        let dir = tempfile::tempdir().unwrap();
        let s = MeetingStore::open(dir.path(), chrono_tz::UTC).unwrap();
        let mut n = new_meeting("k", &[("mic.m4a", b"a")]);
        n.tracks[0].name = "notes.txt".into();
        assert!(s.create(n).is_err());
        let mut n = new_meeting("k", &[("mic.m4a", b"a"), ("mic", b"b")]);
        assert!(s.create(n.clone()).is_err());
        n.tracks.truncate(1);
        n.tracks[0].sha256 = "zz".into();
        assert!(s.create(n).is_err());
        let mut n = new_meeting("bad key!", &[("mic.m4a", b"a")]);
        assert!(s.create(n.clone()).is_err());
        n.idempotency_key = None;
        n.ended_at = Some(n.started_at - chrono::Duration::seconds(1));
        assert!(s.create(n).is_err());
        assert!(MeetingStore::open(&dir.path().join("missing"), chrono_tz::UTC).is_err());
    }
}
