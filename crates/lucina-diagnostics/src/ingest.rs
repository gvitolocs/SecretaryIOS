//! Turns a `POST /v1/diagnostics/reports` body into a scrubbed report.
//!
//! Bodies:
//! - `application/json`: the report alone;
//! - `multipart/form-data`: a part named `report` (the JSON) and one part per
//!   attachment (any field name, with a filename).
//!
//! Either may be sent with `Content-Encoding: gzip`; each attachment may also
//! be gzip-compressed on its own (`.gz` name, `application/gzip`, or the gzip
//! magic). Attachments are stored decompressed; the caps apply to the
//! decompressed bytes, so a gzip bomb stops at the cap.
use std::io::Read;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::{AppName, Attachment, IncomingReport, Kind, Report};
use crate::store::{new_id, safe_name, Prepared};
use crate::{fingerprint, scrub};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// All attachments of a report together, decompressed.
    pub attachment_bytes: usize,
    pub attachments: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            attachment_bytes: 20 * 1024 * 1024,
            attachments: 16,
        }
    }
}

impl Limits {
    /// What a request body may be (compressed or not): the attachments plus
    /// room for the report and the multipart framing.
    pub fn body_bytes(&self) -> usize {
        self.attachment_bytes + 1024 * 1024
    }
}

pub const MAX_MESSAGE: usize = 8 * 1024;
pub const MAX_STACK: usize = 128 * 1024;
pub const MAX_CONTEXT: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum IngestError {
    Bad(String),
    TooLarge(String),
    Unsupported(String),
}

fn bad(m: impl Into<String>) -> IngestError {
    IngestError::Bad(m.into())
}

/// Decompresses at most `cap` bytes; more is `TooLarge`.
pub fn gunzip(data: &[u8], cap: usize, what: &str) -> Result<Vec<u8>, IngestError> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(data)
        .take(cap as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| bad(format!("{what}: not valid gzip ({e})")))?;
    if out.len() > cap {
        return Err(IngestError::TooLarge(format!(
            "{what}: more than {cap} bytes decompressed"
        )));
    }
    Ok(out)
}

fn is_gzip(name: &str, content_type: Option<&str>, data: &[u8]) -> bool {
    data.starts_with(&[0x1f, 0x8b])
        || name.to_ascii_lowercase().ends_with(".gz")
        || content_type.is_some_and(|c| c.contains("gzip"))
}

/// Minidumps and other binary files are stored untouched (not scrubbed, not
/// shown to the agent tools).
fn is_text(name: &str, data: &[u8]) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let binary_ext = matches!(
        ext.as_str(),
        "dmp" | "mdmp" | "bin" | "png" | "jpg" | "jpeg" | "gif" | "heic" | "webp" | "zip" | "core"
    );
    !binary_ext && !data.starts_with(b"MDMP") && !data[..data.len().min(8192)].contains(&0)
}

fn content_type_for(name: &str, text: bool) -> &'static str {
    if !text {
        return "application/octet-stream";
    }
    if name.to_ascii_lowercase().ends_with(".json") {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    }
}

fn clip(field: &str, s: &str, max: usize, truncated: &mut Vec<String>) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    truncated.push(field.to_owned());
    format!("{}\n[…truncated]", &s[..end])
}

fn short(field: &str, s: &str, max: usize) -> Result<String, IngestError> {
    let t = s.trim();
    if t.chars().count() > max || t.chars().any(char::is_control) {
        return Err(bad(format!(
            "{field}: at most {max} characters, no control characters"
        )));
    }
    Ok(t.to_owned())
}

/// Parses the request body.
pub async fn parse(
    limits: &Limits,
    content_type: &str,
    content_encoding: Option<&str>,
    body: Bytes,
) -> Result<(IncomingReport, Vec<(String, Option<String>, Vec<u8>)>), IngestError> {
    let body = match content_encoding.map(|e| e.trim().to_ascii_lowercase()) {
        None => body,
        Some(e) if e.is_empty() || e == "identity" => body,
        Some(e) if e == "gzip" || e == "x-gzip" => {
            Bytes::from(gunzip(&body, limits.body_bytes(), "request body")?)
        }
        Some(e) => return Err(IngestError::Unsupported(format!("Content-Encoding {e}"))),
    };
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("application/json") {
        let r: IncomingReport =
            serde_json::from_slice(&body).map_err(|e| bad(format!("report: {e}")))?;
        return Ok((r, Vec::new()));
    }
    if !ct.starts_with("multipart/form-data") {
        return Err(IngestError::Unsupported(
            "Content-Type: application/json or multipart/form-data".into(),
        ));
    }
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| bad("multipart/form-data without a boundary"))?;
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut mp = multer::Multipart::new(stream, boundary);
    let mut report = None;
    let mut files = Vec::new();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| bad(format!("multipart: {e}")))?
    {
        let name = field.name().unwrap_or("").to_owned();
        let file_name = field.file_name().map(str::to_owned);
        let part_type = field.content_type().map(|m| m.to_string());
        let data = field
            .bytes()
            .await
            .map_err(|e| bad(format!("multipart: {e}")))?;
        match file_name {
            None if name == "report" => {
                report = Some(
                    serde_json::from_slice::<IncomingReport>(&data)
                        .map_err(|e| bad(format!("report: {e}")))?,
                );
            }
            None => return Err(bad(format!("unexpected multipart field {name:?}"))),
            Some(f) => {
                if files.len() >= limits.attachments {
                    return Err(IngestError::TooLarge(format!(
                        "at most {} attachments",
                        limits.attachments
                    )));
                }
                files.push((f, part_type, data.to_vec()));
            }
        }
    }
    let report = report.ok_or_else(|| bad("multipart body without a `report` part"))?;
    Ok((report, files))
}

/// Validates, scrubs and fingerprints a report and its attachments.
pub fn prepare(
    limits: &Limits,
    r: IncomingReport,
    files: Vec<(String, Option<String>, Vec<u8>)>,
    now: DateTime<Utc>,
) -> Result<Prepared, IngestError> {
    let app = AppName::parse(&r.app)
        .ok_or_else(|| bad("app: one of secretary-code, ios, android, web, server".to_string()))?;
    let kind = Kind::parse(&r.kind)
        .ok_or_else(|| bad("kind: one of crash, fatal_error, error, user_report, hang"))?;
    let app_version = short("app_version", &r.app_version, 64)?;
    let device = short("device", &r.device, 128)?;
    let os = short("os", r.os.as_deref().unwrap_or(""), 128)?;
    if app_version.is_empty() || device.is_empty() {
        return Err(bad("app_version and device must not be empty"));
    }
    let mut truncated = Vec::new();
    let mut redactions = 0;
    let (message, n) = scrub::text(r.message.trim());
    redactions += n;
    let message = clip("message", &message, MAX_MESSAGE, &mut truncated);
    let (stack, n) = scrub::text(r.stack.as_deref().unwrap_or(""));
    redactions += n;
    let stack = clip("stack", &stack, MAX_STACK, &mut truncated);
    let mut context = r.context.unwrap_or(Value::Null);
    if !matches!(context, Value::Object(_) | Value::Null) {
        context = json!({ "value": context });
    }
    redactions += scrub::json(&mut context);
    let ctx_len = serde_json::to_vec(&context).map(|v| v.len()).unwrap_or(0);
    if ctx_len > MAX_CONTEXT {
        let text = context.to_string();
        let mut end = 4096.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        context = json!({ "truncated": true, "preview": &text[..end] });
        truncated.push("context".into());
    }
    let occurred_at = r.occurred_at.unwrap_or(now).min(now);

    let mut total = 0usize;
    let mut attachments = Vec::new();
    let mut stored = Vec::new();
    for (raw_name, part_type, data) in files {
        let mut name = safe_name(&raw_name);
        let remaining = limits.attachment_bytes.saturating_sub(total);
        let gz = is_gzip(&name, part_type.as_deref(), &data);
        let mut data = if gz {
            if name.to_ascii_lowercase().ends_with(".gz") && name.len() > 3 {
                name.truncate(name.len() - 3);
            }
            gunzip(&data, remaining, &name)?
        } else {
            data
        };
        if data.len() > remaining {
            return Err(IngestError::TooLarge(format!(
                "attachments: more than {} bytes in total",
                limits.attachment_bytes
            )));
        }
        let text = is_text(&name, &data);
        if text {
            let (clean, n) = scrub::text(&String::from_utf8_lossy(&data));
            redactions += n;
            data = clean.into_bytes();
        }
        // Unique names within a report.
        let mut unique = name.clone();
        let mut i = 1;
        while attachments.iter().any(|a: &Attachment| a.name == unique) {
            i += 1;
            unique = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{i}.{ext}"),
                _ => format!("{name}-{i}"),
            };
        }
        total += data.len();
        let sha: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let a = Attachment {
            content_type: content_type_for(&unique, text).to_owned(),
            name: unique,
            size: data.len() as u64,
            text,
            gzip: gz,
            sha256: sha,
        };
        attachments.push(a.clone());
        stored.push((a, data));
    }

    let top_frames = fingerprint::top_frames(&stack, &message);
    let fp = fingerprint::compute(app.as_str(), &app_version, &stack, &top_frames);
    Ok(Prepared {
        report: Report {
            id: new_id(),
            app,
            app_version,
            os,
            device,
            kind,
            message,
            stack,
            context,
            occurred_at,
            received_at: now,
            fingerprint: fp,
            top_frames,
            attachments,
            redactions,
            truncated,
        },
        files: stored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_bomb_stops_at_cap() {
        let big = gz(&vec![b'a'; 1_000_000]);
        assert!(big.len() < 10_000);
        assert!(matches!(
            gunzip(&big, 1000, "x"),
            Err(IngestError::TooLarge(_))
        ));
        assert_eq!(gunzip(&big, 1_000_000, "x").unwrap().len(), 1_000_000);
        assert!(matches!(gunzip(b"nope", 10, "x"), Err(IngestError::Bad(_))));
    }

    #[test]
    fn text_or_binary() {
        assert!(is_text("main.log", b"hello"));
        assert!(is_text("crash.ips", b"{\"a\":1}"));
        assert!(!is_text("x.dmp", b"hello"));
        assert!(!is_text("x", b"MDMP\x93\xa7"));
        assert!(!is_text("x.log", b"a\0b"));
    }
}
