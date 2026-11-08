//! The pure core of resumable uploads (IETF draft-ietf-httpbis-resumable-upload), as PhotoKit's
//! background upload extension speaks it (`bgupload.rs` is the HTTP side). Kept free of I/O so it
//! can be unit-tested.
//!
//! An upload is an append-only byte stream with an optional announced length. Unlike the app's
//! own protocol (`protocol.rs`), the draft has every append start exactly at the server's offset:
//! anything else is a 409 that tells the client where to resume. Completion is the client's
//! `Upload-Complete: ?1` (or, in the older drafts Apple's example uses, `Upload-Incomplete: ?0`).

use axum::http::HeaderMap;

/// The largest upload accepted: p1's Caddy caps upload bodies at 20 GB.
pub const LIMIT: u64 = 20_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub offset: u64,
    pub length: Option<u64>,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResumableError {
    #[error("upload already complete")]
    Completed,
    #[error("offset does not match; resume at {expected}")]
    OffsetMismatch { expected: u64 },
    #[error("upload length conflicts with what was announced or sent")]
    LengthConflict,
    #[error("upload too large")]
    TooLarge,
    #[error("{missing} bytes still missing")]
    Incomplete { missing: u64 },
}

/// Which completion field the client speaks; replies use the same one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// `Upload-Complete` (draft 04 and later).
    Complete,
    /// `Upload-Incomplete` (drafts 01 to 03, Apple's PhotoKit example).
    Incomplete,
}

impl Dialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Complete => "complete",
            Dialect::Incomplete => "incomplete",
        }
    }
    pub fn parse(s: &str) -> Dialect {
        if s == "incomplete" {
            Dialect::Incomplete
        } else {
            Dialect::Complete
        }
    }
    /// The reply header saying whether the upload is complete.
    pub fn header(self, complete: bool) -> (&'static str, &'static str) {
        match (self, complete) {
            (Dialect::Complete, true) => ("upload-complete", "?1"),
            (Dialect::Complete, false) => ("upload-complete", "?0"),
            (Dialect::Incomplete, true) => ("upload-incomplete", "?0"),
            (Dialect::Incomplete, false) => ("upload-incomplete", "?1"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad {0} header")]
pub struct BadHeader(pub &'static str);

/// A Structured Field boolean (`?1` / `?0`).
fn sf_bool(v: &str) -> Option<bool> {
    match v.trim() {
        "?1" => Some(true),
        "?0" => Some(false),
        _ => None,
    }
}

/// Whether this request ends the upload, and in which dialect. `None`: the client sent neither
/// field (a plain upload, not a resumable one).
pub fn completion(h: &HeaderMap) -> Result<Option<(bool, Dialect)>, BadHeader> {
    if let Some(v) = h.get("upload-complete") {
        let b = v
            .to_str()
            .ok()
            .and_then(sf_bool)
            .ok_or(BadHeader("Upload-Complete"))?;
        return Ok(Some((b, Dialect::Complete)));
    }
    if let Some(v) = h.get("upload-incomplete") {
        let b = v
            .to_str()
            .ok()
            .and_then(sf_bool)
            .ok_or(BadHeader("Upload-Incomplete"))?;
        return Ok(Some((!b, Dialect::Incomplete)));
    }
    Ok(None)
}

/// A non-negative integer header (`Upload-Offset`, `Upload-Length`, `Content-Length`).
pub fn header_u64(h: &HeaderMap, name: &'static str) -> Result<Option<u64>, BadHeader> {
    match h.get(name) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
            .map(Some)
            .ok_or(BadHeader(name)),
    }
}

/// The interop version the client announced, to echo back (digits only).
pub fn interop_version(h: &HeaderMap) -> Option<String> {
    h.get("upload-draft-interop-version")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
}

/// Record an announced length (`Upload-Length`, or the size of a body that completes the upload).
pub fn announce_length(s: State, length: Option<u64>) -> Result<State, ResumableError> {
    let Some(l) = length else { return Ok(s) };
    if l > LIMIT {
        return Err(ResumableError::TooLarge);
    }
    if s.offset > l || s.length.is_some_and(|k| k != l) {
        return Err(ResumableError::LengthConflict);
    }
    Ok(State {
        length: Some(l),
        ..s
    })
}

/// Whether an append of `len` bytes (if the request says) may start at `offset`.
pub fn check_append(s: State, offset: u64, len: Option<u64>) -> Result<(), ResumableError> {
    if s.complete {
        return Err(ResumableError::Completed);
    }
    if offset != s.offset {
        return Err(ResumableError::OffsetMismatch { expected: s.offset });
    }
    if let Some(len) = len {
        let end = offset.checked_add(len).ok_or(ResumableError::TooLarge)?;
        if end > LIMIT {
            return Err(ResumableError::TooLarge);
        }
        if s.length.is_some_and(|l| end > l) {
            return Err(ResumableError::LengthConflict);
        }
    }
    Ok(())
}

/// The most bytes the next append may still bring.
pub fn room(s: State) -> u64 {
    s.length.unwrap_or(LIMIT).saturating_sub(s.offset)
}

/// After an append that said it completes the upload: the final state, or why it can't end.
pub fn complete(s: State) -> Result<State, ResumableError> {
    if s.complete {
        return Err(ResumableError::Completed);
    }
    if let Some(l) = s.length {
        if s.offset < l {
            return Err(ResumableError::Incomplete {
                missing: l - s.offset,
            });
        }
    }
    Ok(State {
        length: Some(s.offset),
        complete: true,
        ..s
    })
}

/// `<id>.<token>`, the capability in an upload's URL: `bgu_` plus 32 hex digits, then 32 hex digits.
pub fn parse_capability(seg: &str) -> Option<(&str, &str)> {
    let (id, token) = seg.split_once('.')?;
    let hex32 = |s: &str| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit());
    (id.strip_prefix("bgu_").is_some_and(hex32) && hex32(token)).then_some((id, token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn st(offset: u64, length: Option<u64>) -> State {
        State {
            offset,
            length,
            complete: false,
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn both_completion_dialects_are_understood() {
        assert_eq!(
            completion(&headers(&[("upload-complete", "?1")])),
            Ok(Some((true, Dialect::Complete)))
        );
        assert_eq!(
            completion(&headers(&[("upload-complete", "?0")])),
            Ok(Some((false, Dialect::Complete)))
        );
        // Apple's example: `Upload-Incomplete: ?0` is a complete upload.
        assert_eq!(
            completion(&headers(&[("upload-incomplete", "?0")])),
            Ok(Some((true, Dialect::Incomplete)))
        );
        assert_eq!(
            completion(&headers(&[("upload-incomplete", "?1")])),
            Ok(Some((false, Dialect::Incomplete)))
        );
        assert_eq!(completion(&headers(&[])), Ok(None));
        assert!(completion(&headers(&[("upload-complete", "true")])).is_err());
    }

    #[test]
    fn replies_answer_in_the_clients_dialect() {
        assert_eq!(
            Dialect::Incomplete.header(true),
            ("upload-incomplete", "?0")
        );
        assert_eq!(Dialect::Complete.header(false), ("upload-complete", "?0"));
        assert_eq!(
            Dialect::parse(Dialect::Incomplete.as_str()),
            Dialect::Incomplete
        );
    }

    #[test]
    fn integer_headers_are_strict() {
        let h = headers(&[("upload-offset", "42"), ("upload-length", "-1")]);
        assert_eq!(header_u64(&h, "upload-offset"), Ok(Some(42)));
        assert!(header_u64(&h, "upload-length").is_err());
        assert_eq!(header_u64(&h, "content-length"), Ok(None));
        assert_eq!(
            interop_version(&headers(&[("upload-draft-interop-version", "6")])),
            Some("6".into())
        );
        assert_eq!(
            interop_version(&headers(&[("upload-draft-interop-version", "6a")])),
            None
        );
    }

    #[test]
    fn appends_must_start_at_the_offset() {
        assert_eq!(check_append(st(10, None), 10, Some(5)), Ok(()));
        assert_eq!(
            check_append(st(10, None), 0, Some(15)),
            Err(ResumableError::OffsetMismatch { expected: 10 })
        );
        assert_eq!(
            check_append(st(10, None), 12, None),
            Err(ResumableError::OffsetMismatch { expected: 10 })
        );
        let done = State {
            complete: true,
            ..st(10, Some(10))
        };
        assert_eq!(
            check_append(done, 10, Some(0)),
            Err(ResumableError::Completed)
        );
    }

    #[test]
    fn appends_stay_within_the_length_and_the_limit() {
        assert_eq!(
            check_append(st(10, Some(12)), 10, Some(5)),
            Err(ResumableError::LengthConflict)
        );
        assert_eq!(
            check_append(st(0, None), 0, Some(LIMIT + 1)),
            Err(ResumableError::TooLarge)
        );
        assert_eq!(
            check_append(st(1, None), 1, Some(u64::MAX)),
            Err(ResumableError::TooLarge)
        );
        assert_eq!(room(st(10, Some(12))), 2);
        assert_eq!(room(st(10, None)), LIMIT - 10);
    }

    #[test]
    fn a_length_can_be_announced_once() {
        let s = announce_length(st(0, None), Some(100)).unwrap();
        assert_eq!(s.length, Some(100));
        assert_eq!(announce_length(s, Some(100)), Ok(s));
        assert_eq!(
            announce_length(s, Some(99)),
            Err(ResumableError::LengthConflict)
        );
        assert_eq!(announce_length(s, None), Ok(s));
        assert_eq!(
            announce_length(st(50, None), Some(40)),
            Err(ResumableError::LengthConflict)
        );
        assert_eq!(
            announce_length(st(0, None), Some(LIMIT + 1)),
            Err(ResumableError::TooLarge)
        );
    }

    #[test]
    fn completion_needs_every_announced_byte() {
        assert_eq!(
            complete(st(90, Some(100))),
            Err(ResumableError::Incomplete { missing: 10 })
        );
        assert_eq!(
            complete(st(100, Some(100))),
            Ok(State {
                offset: 100,
                length: Some(100),
                complete: true
            })
        );
        // No announced length: what arrived is the upload.
        assert_eq!(complete(st(7, None)).map(|s| s.length), Ok(Some(7)));
        let done = complete(st(7, None)).unwrap();
        assert_eq!(complete(done), Err(ResumableError::Completed));
    }

    #[test]
    fn capabilities_are_checked_for_shape() {
        let id = "bgu_0123456789abcdef0123456789abcdef";
        let token = "fedcba9876543210fedcba9876543210";
        assert_eq!(
            parse_capability(&format!("{id}.{token}")),
            Some((id, token))
        );
        assert_eq!(parse_capability(id), None);
        assert_eq!(parse_capability(&format!("{id}.{token}0")), None);
        assert_eq!(parse_capability(&format!("upl_{}.{token}", &id[4..])), None);
        assert_eq!(parse_capability(&format!("{id}.../{}", &token[3..])), None);
    }
}
