//! Attachments: content-addressed files in `notes/attachments/<sha256>.<ext>`
//! (0600), shared by every note that holds the same bytes. A file is removed
//! when the last note that refers to it lets go (the note is deleted forever,
//! or the attachment is removed from it).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use axum::http::StatusCode;
use sha2::{Digest, Sha256};

use super::model::*;
use super::store::{io_err, Index, NoteStore};
use crate::devices::ensure_dir;
use crate::error::ApiError;

/// Allowed mimetypes and the extension each is stored with.
const TYPES: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/heic", "heic"),
    ("image/heif", "heif"),
    ("audio/3gpp", "3gp"),
    ("audio/mp4", "m4a"),
    ("audio/aac", "aac"),
    ("audio/mpeg", "mp3"),
    ("audio/ogg", "ogg"),
    ("audio/webm", "weba"),
    ("audio/x-m4a", "m4a"),
    ("application/pdf", "pdf"),
];

/// The mimetype without parameters, lower-case.
fn bare_mime(m: &str) -> String {
    m.split(';').next().unwrap_or("").trim().to_lowercase()
}

/// The extension an allowed mimetype is stored with.
pub fn ext_for_mime(mimetype: &str) -> Option<&'static str> {
    let m = bare_mime(mimetype);
    TYPES.iter().find(|(t, _)| *t == m).map(|(_, e)| *e)
}

/// The mimetype a stored extension is served with.
pub fn mime_for_ext(ext: &str) -> Option<&'static str> {
    TYPES.iter().find(|(_, e)| *e == ext).map(|(t, _)| *t)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `^[0-9a-f]{64}$`.
pub fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn valid_ext(s: &str) -> bool {
    (1..=5).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// `^[0-9a-f]{64}\.[a-z0-9]{1,5}$` → (sha, ext).
pub fn split_file_name(file: &str) -> Option<(&str, &str)> {
    let (sha, ext) = file.split_once('.')?;
    (valid_sha(sha) && valid_ext(ext)).then_some((sha, ext))
}

/// A display name: the last path component, trimmed, no control characters.
fn clean_name(name: Option<&str>) -> Option<String> {
    let n = name?.rsplit(['/', '\\']).next()?.trim();
    let n: String = n
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ATTACHMENT_NAME_CHARS)
        .collect();
    let n = n.trim().to_owned();
    (!n.is_empty()).then_some(n)
}

fn unsupported(mimetype: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        format!(
            "attachments: {:?} is not allowed (images, audio and PDF only)",
            bare_mime(mimetype)
        ),
    )
}

fn check_upload(bytes: &[u8], mimetype: &str) -> Result<&'static str, ApiError> {
    let ext = ext_for_mime(mimetype).ok_or_else(|| unsupported(mimetype))?;
    if bytes.is_empty() {
        return Err(bad_request("attachments: the file is empty"));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment_too_large",
            format!(
                "an attachment is at most {} MB",
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ),
        ));
    }
    Ok(ext)
}

/// Write `bytes` to `dir/name` atomically with mode 0600.
fn write_file_atomic(dir: &std::path::Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    ensure_dir(dir)?;
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, dir.join(name))
}

impl NoteStore {
    /// Is `notes/attachments/<sha>.<ext>` there?
    pub fn has_attachment(&self, sha256_hex: &str, ext: &str) -> bool {
        valid_sha(sha256_hex)
            && valid_ext(ext)
            && self
                .attachments_dir()
                .join(format!("{sha256_hex}.{ext}"))
                .is_file()
    }

    /// The path of a stored file, for serving. `None` unless `file` has the
    /// stored shape and exists.
    pub fn attachment_path(&self, file: &str) -> Option<PathBuf> {
        split_file_name(file)?;
        let p = self.attachments_dir().join(file);
        p.is_file().then_some(p)
    }

    /// Write the bytes under their SHA-256 unless already there (0600) and
    /// describe them. Returns whether the file was written now.
    fn store_bytes(
        &self,
        bytes: &[u8],
        mimetype: &str,
        name: Option<&str>,
    ) -> Result<(Attachment, bool), ApiError> {
        let ext = check_upload(bytes, mimetype)?;
        let sha = hex(&Sha256::digest(bytes));
        let file = format!("{sha}.{ext}");
        let path = self.attachments_dir().join(&file);
        let created = !path.is_file();
        if created {
            write_file_atomic(&self.attachments_dir(), &file, bytes).map_err(io_err)?;
        }
        Ok((
            Attachment {
                id: sha,
                file,
                mimetype: bare_mime(mimetype),
                size: bytes.len() as u64,
                name: clean_name(name),
                created_at: rfc3339(now()),
            },
            created,
        ))
    }

    /// Store an upload without attaching it to a note (the importer does
    /// this, then hands the [`Attachment`] to `import_note`).
    pub fn put_attachment(
        &self,
        bytes: &[u8],
        mimetype: &str,
        name: Option<&str>,
    ) -> Result<Attachment, ApiError> {
        self.with(|st, _| st.store_bytes(bytes, mimetype, name).map(|(a, _)| a))
    }

    /// `POST /v1/notes/{key}/attachments`: store the bytes and attach them.
    /// The same bytes twice on one note are one attachment.
    pub fn add_attachment(
        &self,
        key: &str,
        bytes: &[u8],
        mimetype: &str,
        name: Option<&str>,
    ) -> Result<Note, ApiError> {
        check_upload(bytes, mimetype)?;
        self.with(|st, idx| {
            let mut note = idx.find_live(key).cloned().ok_or_else(|| not_found(key))?;
            let (att, created) = st.store_bytes(bytes, mimetype, name)?;
            if note.attachments.iter().any(|a| a.id == att.id) {
                return Ok(note);
            }
            if note.attachments.len() >= MAX_ATTACHMENTS {
                if created {
                    st.gc(idx, std::slice::from_ref(&att.file));
                }
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "attachments_full",
                    format!("a note holds at most {MAX_ATTACHMENTS} attachments"),
                ));
            }
            note.attachments.push(att.clone());
            note.updated_at = rfc3339(now());
            match st.put(idx, note) {
                Ok(n) => Ok(n),
                Err(e) => {
                    if created {
                        st.gc(idx, std::slice::from_ref(&att.file));
                    }
                    Err(e)
                }
            }
        })
    }

    /// `DELETE /v1/notes/{key}/attachments/{attId}`.
    pub fn remove_attachment(&self, key: &str, att_id: &str) -> Result<Note, ApiError> {
        self.with(|st, idx| {
            let mut note = idx.find_live(key).cloned().ok_or_else(|| not_found(key))?;
            let pos = note
                .attachments
                .iter()
                .position(|a| a.id == att_id)
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::NOT_FOUND,
                        "attachment_not_found",
                        format!("the note has no attachment {att_id:?}"),
                    )
                })?;
            let gone = note.attachments.remove(pos);
            note.updated_at = rfc3339(now());
            let note = st.put(idx, note)?;
            st.gc(idx, std::slice::from_ref(&gone.file));
            Ok(note)
        })
    }

    /// Attachments a body refers to: the bytes must already be stored.
    pub(super) fn resolve_refs(&self, refs: &[AttachmentRef]) -> Result<Vec<Attachment>, ApiError> {
        if refs.len() > MAX_ATTACHMENTS {
            return Err(bad_request(format!(
                "attachments: at most {MAX_ATTACHMENTS}"
            )));
        }
        let mut out: Vec<Attachment> = Vec::new();
        for r in refs {
            if !valid_sha(&r.id) {
                return Err(bad_request("attachments.id: a SHA-256 in hex"));
            }
            if out.iter().any(|a| a.id == r.id) {
                continue;
            }
            let ext = match (r.file.as_deref(), r.mimetype.as_deref()) {
                (Some(f), _) => match split_file_name(f) {
                    Some((sha, ext)) if sha == r.id => ext,
                    _ => return Err(bad_request("attachments.file: must be <id>.<ext>")),
                },
                (None, Some(m)) => ext_for_mime(m).ok_or_else(|| unsupported(m))?,
                (None, None) => {
                    return Err(bad_request(
                        "attachments: give file or mimetype with the id",
                    ))
                }
            };
            let path = self.attachments_dir().join(format!("{}.{ext}", r.id));
            let size = std::fs::metadata(&path)
                .map(|m| m.len())
                .map_err(|_| bad_request(format!("attachments: unknown attachment {}", r.id)))?;
            let mimetype = match r.mimetype.as_deref() {
                Some(m) if ext_for_mime(m) == Some(ext) => bare_mime(m),
                _ => mime_for_ext(ext)
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
            };
            out.push(Attachment {
                id: r.id.clone(),
                file: format!("{}.{ext}", r.id),
                mimetype,
                size,
                name: clean_name(r.name.as_deref()),
                created_at: rfc3339(now()),
            });
        }
        Ok(out)
    }

    /// Remove the files no note refers to any more.
    pub(super) fn gc(&self, idx: &Index, files: &[String]) {
        for f in files {
            if split_file_name(f).is_some() && !idx.references(f) {
                if let Err(e) = std::fs::remove_file(self.attachments_dir().join(f)) {
                    if e.kind() != io::ErrorKind::NotFound {
                        tracing::warn!(file = %f, error = %e, "could not remove an attachment");
                    }
                }
            }
        }
    }
}
