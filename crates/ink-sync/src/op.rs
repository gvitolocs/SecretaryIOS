//! Wire ops of the device op log and their validation.
use ink_library::model::{HighlightKind, ReadingStatus};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const MAX_DEVICE_CHARS: usize = 64;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_ANCHOR_BYTES: usize = 1024;
pub const MAX_SESSION_ID_CHARS: usize = 100;

/// A client-generated op id: the device id and that device's monotonic
/// counter (from 1). On the wire: `"<device>:<counter>"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId {
    pub device: String,
    pub counter: u64,
}

pub fn valid_device(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= MAX_DEVICE_CHARS
        && d.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

impl OpId {
    pub fn new(device: impl Into<String>, counter: u64) -> Self {
        OpId {
            device: device.into(),
            counter,
        }
    }

    /// Canonical form only: a valid device id, `:`, a counter >= 1 without
    /// sign or leading zeros.
    pub fn parse(s: &str) -> Result<Self, String> {
        let (d, c) = s
            .rsplit_once(':')
            .ok_or_else(|| format!("op id {s:?} is not <device>:<counter>"))?;
        if !valid_device(d) {
            return Err("invalid device id in op id".into());
        }
        if c.is_empty()
            || c.len() > 20
            || !c.bytes().all(|b| b.is_ascii_digit())
            || c.starts_with('0')
        {
            return Err("op id counter must be a positive integer".into());
        }
        let counter = c
            .parse::<u64>()
            .map_err(|_| "op id counter out of range".to_string())?;
        Ok(OpId::new(d, counter))
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.device, self.counter)
    }
}

impl Serialize for OpId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OpId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        OpId::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// The last-writer-wins order: `(at, device, counter)`, lexicographic. Op ids
/// are unique, so two distinct ops never have equal stamps: a total order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamp {
    pub at_ms: i64,
    pub device: String,
    pub counter: u64,
}

impl Stamp {
    pub fn op_id(&self) -> OpId {
        OpId::new(self.device.clone(), self.counter)
    }
}

fn default_highlight_kind() -> HighlightKind {
    HighlightKind::Highlight
}

/// What an op does. `bookId` names a library book (`bk_` + 16 hex digits).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OpKind {
    /// Reading position. Last writer wins by [`Stamp`].
    SetProgress {
        book_id: String,
        #[serde(default)]
        anchor: Option<String>,
        #[serde(default)]
        percent: Option<f32>,
        #[serde(default)]
        location: Option<u32>,
    },
    /// Grow-only: keyed by `bookId|kind|anchor|text`.
    AddHighlight {
        book_id: String,
        #[serde(default = "default_highlight_kind")]
        kind: HighlightKind,
        text: String,
        anchor: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// The note attached to a highlight (device or Kindle); `null` clears it.
    EditNote {
        book_id: String,
        highlight_id: String,
        #[serde(default)]
        note: Option<String>,
    },
    SetStatus {
        book_id: String,
        status: ReadingStatus,
    },
    /// Recorded as a tombstone; nothing is removed until the owner confirms.
    DeleteHighlight {
        book_id: String,
        highlight_id: String,
    },
    StartSession {
        book_id: String,
        session_id: String,
        #[serde(default)]
        location: Option<u32>,
    },
    EndSession {
        book_id: String,
        session_id: String,
        #[serde(default)]
        location: Option<u32>,
    },
}

impl OpKind {
    pub fn book_id(&self) -> &str {
        match self {
            OpKind::SetProgress { book_id, .. }
            | OpKind::AddHighlight { book_id, .. }
            | OpKind::EditNote { book_id, .. }
            | OpKind::SetStatus { book_id, .. }
            | OpKind::DeleteHighlight { book_id, .. }
            | OpKind::StartSession { book_id, .. }
            | OpKind::EndSession { book_id, .. } => book_id,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            OpKind::SetProgress { .. } => "setProgress",
            OpKind::AddHighlight { .. } => "addHighlight",
            OpKind::EditNote { .. } => "editNote",
            OpKind::SetStatus { .. } => "setStatus",
            OpKind::DeleteHighlight { .. } => "deleteHighlight",
            OpKind::StartSession { .. } => "startSession",
            OpKind::EndSession { .. } => "endSession",
        }
    }
}

/// One op of the log: `{"opId":"kobo-1:7","at":"2026-10-24T09:00:00Z","type":"setProgress",…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Op {
    pub op_id: OpId,
    /// Device time, RFC 3339.
    pub at: String,
    #[serde(flatten)]
    pub kind: OpKind,
}

fn hex_id(s: &str, prefix: &str) -> bool {
    s.len() == prefix.len() + 16
        && s.starts_with(prefix)
        && s.as_bytes()[prefix.len()..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

pub fn valid_book_id(s: &str) -> bool {
    hex_id(s, "bk_")
}

pub fn valid_highlight_id(s: &str) -> bool {
    hex_id(s, "hl_")
}

fn text(s: &str, max: usize) -> bool {
    s.len() <= max
        && !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

fn session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SESSION_ID_CHARS
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}

/// RFC 3339 → milliseconds since the epoch.
pub fn parse_at(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|t| t.timestamp_millis())
}

impl Op {
    /// Checks the payload; returns `at` in milliseconds.
    pub fn validate(&self) -> Result<i64, String> {
        if !valid_device(&self.op_id.device) || self.op_id.counter == 0 {
            return Err("invalid op id".into());
        }
        let at = parse_at(&self.at).ok_or("at must be RFC 3339")?;
        if !valid_book_id(self.kind.book_id()) {
            return Err("invalid bookId".into());
        }
        let anchor_ok = |a: &Option<String>| a.as_ref().is_none_or(|a| text(a, MAX_ANCHOR_BYTES));
        let note_ok = |n: &Option<String>| n.as_ref().is_none_or(|n| text(n, MAX_TEXT_BYTES));
        match &self.kind {
            OpKind::SetProgress {
                anchor, percent, ..
            } => {
                if !anchor_ok(anchor) {
                    return Err("invalid anchor".into());
                }
                if percent.is_some_and(|p| !p.is_finite() || !(0.0..=100.0).contains(&p)) {
                    return Err("percent must be 0-100".into());
                }
            }
            OpKind::AddHighlight {
                text: t,
                anchor,
                note,
                ..
            } => {
                if t.trim().is_empty() || !text(t, MAX_TEXT_BYTES) {
                    return Err("invalid highlight text".into());
                }
                if anchor.is_empty() || !text(anchor, MAX_ANCHOR_BYTES) {
                    return Err("invalid anchor".into());
                }
                if !note_ok(note) {
                    return Err("invalid note".into());
                }
            }
            OpKind::EditNote {
                highlight_id, note, ..
            } => {
                if !valid_highlight_id(highlight_id) {
                    return Err("invalid highlightId".into());
                }
                if !note_ok(note) {
                    return Err("invalid note".into());
                }
            }
            OpKind::DeleteHighlight { highlight_id, .. } => {
                if !valid_highlight_id(highlight_id) {
                    return Err("invalid highlightId".into());
                }
            }
            OpKind::SetStatus { status, .. } => {
                if *status == ReadingStatus::Unknown {
                    return Err("a device cannot set status unknown".into());
                }
            }
            OpKind::StartSession { session_id: s, .. }
            | OpKind::EndSession { session_id: s, .. } => {
                if !session_id(s) {
                    return Err("invalid sessionId".into());
                }
            }
        }
        Ok(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn op_id_canonical_form() {
        assert_eq!(OpId::parse("kobo-1:7").unwrap(), OpId::new("kobo-1", 7));
        assert_eq!(OpId::new("a:b", 3).to_string(), "a:b:3");
        for bad in [
            "kobo",
            "kobo:0",
            "kobo:07",
            "kobo:+7",
            ":7",
            "ko bo:1",
            "k:99999999999999999999",
        ] {
            assert!(OpId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wire_shape_round_trips() {
        let v = json!({"opId":"kobo-1:7","at":"2026-10-24T09:00:00Z","type":"setProgress",
            "bookId":"bk_0123456789abcdef","anchor":"epubcfi(/6/4)","percent":12.5});
        let op: Op = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(op.op_id, OpId::new("kobo-1", 7));
        assert_eq!(op.validate().unwrap(), 1_792_832_400_000);
        let back = serde_json::to_value(&op).unwrap();
        assert_eq!(back["type"], "setProgress");
        assert_eq!(back["bookId"], v["bookId"]);
        let h: Op = serde_json::from_value(json!({"opId":"kobo-1:8","at":"2026-10-24T09:00:00Z",
            "type":"addHighlight","bookId":"bk_0123456789abcdef","text":"x","anchor":"a"}))
        .unwrap();
        assert!(matches!(
            h.kind,
            OpKind::AddHighlight {
                kind: HighlightKind::Highlight,
                ..
            }
        ));
    }

    #[test]
    fn validation_rejects_bad_payloads() {
        let base = |kind: OpKind| Op {
            op_id: OpId::new("d", 1),
            at: "2026-10-24T09:00:00Z".into(),
            kind,
        };
        let book = "bk_0123456789abcdef".to_string();
        assert!(base(OpKind::SetProgress {
            book_id: book.clone(),
            anchor: None,
            percent: Some(101.0),
            location: None
        })
        .validate()
        .is_err());
        assert!(base(OpKind::SetStatus {
            book_id: "bk_x".into(),
            status: ReadingStatus::Reading
        })
        .validate()
        .is_err());
        assert!(base(OpKind::SetStatus {
            book_id: book.clone(),
            status: ReadingStatus::Unknown
        })
        .validate()
        .is_err());
        assert!(base(OpKind::EditNote {
            book_id: book.clone(),
            highlight_id: "hl_nothex".into(),
            note: None
        })
        .validate()
        .is_err());
        let mut op = base(OpKind::SetStatus {
            book_id: book,
            status: ReadingStatus::Reading,
        });
        op.at = "yesterday".into();
        assert!(op.validate().is_err());
    }
}
