//! The private on-device call journal: one record per SIM call, Lucina voice
//! conversation or imported recording, with its transcript turns.
//!
//! It lives in `<filesDir>/calls.json` (mode 0600, written atomically) in the
//! same format as app 0.1.0, so an upgrade keeps the history. The file holds
//! at most [`MAX_RECORDS`] records, oldest first; [`history`](Op::History)
//! and [`view`](Op::View) return them newest first.
//!
//! Text that is or asks for a verification code ([`crate::sensitive`]) is
//! never written: [`append`] answers `{"saved": false, "sensitive": true}`.

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ui, Result};

/// Records kept; the oldest is dropped first.
pub const MAX_RECORDS: usize = 100;
/// Transcript turns per record.
pub const MAX_TURNS: usize = 300;
/// Characters per turn.
pub const MAX_TURN_CHARS: usize = 40_000;
/// Characters of a peer name or number.
pub const MAX_PEER_CHARS: usize = 120;
/// Characters of a final status.
pub const MAX_STATUS_CHARS: usize = 40;
/// Size of `calls.json`, read or written.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The only directory the journal may live in (Android's `getFilesDir()`).
const APP_FILES_SUFFIX: &str = "/tech.getsecretary.secretary/files";

/// Serialises every read-modify-write of the file within the process.
static LOCK: Mutex<()> = Mutex::new(());

/// What a record is.
pub const KINDS: [&str; 3] = ["sim", "internet", "recording"];
/// Who spoke a turn.
pub const ROLES: [&str; 3] = ["user", "lucina", "recording"];

/// One call, conversation or recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    /// 24 lowercase hex characters (96 random bits).
    pub id: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// The other party: a number, "Private number", "Lucina", …
    pub peer: String,
    /// Milliseconds since the epoch.
    pub started_at: u64,
    /// `active` until [`end`], then the final status.
    pub status: String,
    /// Transcript, oldest first.
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Milliseconds since the epoch, once ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
}

/// One transcript turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// One of [`ROLES`].
    pub role: String,
    /// What was said or transcribed.
    pub text: String,
    /// Milliseconds since the epoch.
    pub at: u64,
}

/// A journal operation, as named over JNI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `{"kind", "peer"?}` → `{"id"}`.
    Begin,
    /// `{"id", "role", "text"}` → `{"saved": bool, "sensitive"?: true}`.
    Append,
    /// `{"id", "status"?}` → `{"saved": true}`.
    End,
    /// `{}` → `{"records": [Record…]}`, newest first.
    History,
    /// `{"tzOffsetMin"?}` → `{"records": [ui::RecordView…]}`, newest first.
    View,
}

impl Op {
    /// Parse a JNI operation name.
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "begin" => Self::Begin,
            "append" => Self::Append,
            "end" => Self::End,
            "history" => Self::History,
            "view" => Self::View,
            _ => return Err("Unknown call operation.".into()),
        })
    }

    fn writes(self) -> bool {
        matches!(self, Self::Begin | Self::Append | Self::End)
    }
}

/// Start a record and return its id. `peer` defaults to "Lucina"; control
/// characters are removed and it is cut to [`MAX_PEER_CHARS`].
pub fn begin(
    rows: &mut Vec<Record>,
    kind: &str,
    peer: Option<&str>,
    now: u64,
    id: String,
) -> Result<String> {
    if !KINDS.contains(&kind) {
        return Err("Unknown call type.".into());
    }
    let peer = clean(peer.unwrap_or("Lucina"), MAX_PEER_CHARS);
    rows.push(Record {
        id: id.clone(),
        kind: kind.into(),
        peer,
        started_at: now,
        status: "active".into(),
        turns: Vec::new(),
        ended_at: None,
    });
    if rows.len() > MAX_RECORDS {
        let excess = rows.len() - MAX_RECORDS;
        rows.drain(..excess);
    }
    Ok(id)
}

/// Add a turn to record `id`. Returns `false` (and stores nothing) when the
/// text is [sensitive](crate::sensitive).
pub fn append(rows: &mut [Record], id: &str, role: &str, text: &str, now: u64) -> Result<bool> {
    let row = find(rows, id)?;
    if !ROLES.contains(&role) {
        return Err("Unknown speaker.".into());
    }
    if text.chars().count() > MAX_TURN_CHARS {
        return Err("Transcript limit.".into());
    }
    if crate::sensitive(text) {
        return Ok(false);
    }
    if row.turns.len() >= MAX_TURNS {
        return Err("Transcript limit.".into());
    }
    row.turns.push(Turn {
        role: role.into(),
        text: text.into(),
        at: now,
    });
    Ok(true)
}

/// Close record `id` with `status` (default "ended").
pub fn end(rows: &mut [Record], id: &str, status: Option<&str>, now: u64) -> Result<()> {
    let row = find(rows, id)?;
    row.ended_at = Some(now);
    row.status = clean(status.unwrap_or("ended"), MAX_STATUS_CHARS);
    Ok(())
}

/// Apply `op` with arguments `v` to `rows`. `fresh_id` is only called by
/// [`Op::Begin`]. Pure: the caller loads and saves.
pub fn apply(
    rows: &mut Vec<Record>,
    op: Op,
    v: &Value,
    now: u64,
    fresh_id: impl FnOnce() -> Result<String>,
) -> Result<Value> {
    let id = || v["id"].as_str().unwrap_or("");
    match op {
        Op::Begin => {
            let kind = v["kind"].as_str().unwrap_or("");
            if !KINDS.contains(&kind) {
                return Err("Unknown call type.".into());
            }
            let id = begin(rows, kind, v["peer"].as_str(), now, fresh_id()?)?;
            Ok(json!({ "id": id }))
        }
        Op::Append => {
            let text = v["text"].as_str().ok_or("Transcript limit.")?;
            if append(rows, id(), v["role"].as_str().unwrap_or(""), text, now)? {
                Ok(json!({ "saved": true }))
            } else {
                Ok(json!({ "saved": false, "sensitive": true }))
            }
        }
        Op::End => {
            end(rows, id(), v["status"].as_str(), now)?;
            Ok(json!({ "saved": true }))
        }
        Op::History => Ok(json!({ "records": rows.iter().rev().collect::<Vec<_>>() })),
        Op::View => {
            let offset = v["tzOffsetMin"]
                .as_i64()
                .unwrap_or(0)
                .clamp(-24 * 60, 24 * 60);
            Ok(
                json!({ "records": rows.iter().rev().map(|r| ui::record_view(r, offset)).collect::<Vec<_>>() }),
            )
        }
    }
}

/// The journal file of the app's files directory.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Open the journal in `files_dir`, which must be the app's own files
    /// directory (`…/tech.getsecretary.secretary/files`).
    pub fn open(files_dir: &str) -> Result<Self> {
        if !files_dir.ends_with(APP_FILES_SUFFIX) || files_dir.contains("/../") {
            return Err("Invalid app storage.".into());
        }
        Ok(Self {
            path: Path::new(files_dir).join("calls.json"),
        })
    }

    /// Run one operation under the process-wide lock; writes are atomic.
    pub fn run(&self, op: Op, v: &Value) -> Result<Value> {
        let _lock = LOCK.lock().map_err(|_| "History busy.")?;
        let mut rows = self.load()?;
        let out = apply(&mut rows, op, v, crate::now_ms(), random_id)?;
        if op.writes() && out["saved"] != false {
            self.save(&rows)?;
        }
        Ok(out)
    }

    /// Shorthand for [`Op::Begin`].
    pub fn begin(&self, kind: &str, peer: &str) -> Result<String> {
        let out = self.run(Op::Begin, &json!({ "kind": kind, "peer": peer }))?;
        Ok(out["id"].as_str().unwrap_or_default().to_owned())
    }

    /// Shorthand for [`Op::Append`]; returns whether the turn was stored.
    pub fn append(&self, id: &str, role: &str, text: &str) -> Result<bool> {
        let out = self.run(Op::Append, &json!({ "id": id, "role": role, "text": text }))?;
        Ok(out["saved"] == true)
    }

    /// Shorthand for [`Op::End`].
    pub fn end(&self, id: &str, status: &str) -> Result<()> {
        self.run(Op::End, &json!({ "id": id, "status": status }))
            .map(drop)
    }

    fn load(&self) -> Result<Vec<Record>> {
        match fs::metadata(&self.path) {
            Ok(m) if m.len() > MAX_FILE_BYTES => Err("Call history exceeds limit.".into()),
            Ok(_) => {
                let bytes = fs::read(&self.path).map_err(|_| "History unavailable.")?;
                serde_json::from_slice(&bytes).map_err(|_| "History unavailable.".into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err("History unavailable.".into()),
        }
    }

    fn save(&self, rows: &[Record]) -> Result<()> {
        let encoded = serde_json::to_vec(rows).map_err(|_| "History save failed.")?;
        if encoded.len() as u64 > MAX_FILE_BYTES {
            return Err("Call history storage limit reached.".into());
        }
        let temp = self.path.with_extension("json.tmp");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| "History save failed.")?;
        f.write_all(&encoded)
            .and_then(|()| f.sync_all())
            .map_err(|_| "History save failed.")?;
        fs::rename(&temp, &self.path).map_err(|_| "History save failed.")?;
        if let Some(dir) = self.path.parent().and_then(|d| fs::File::open(d).ok()) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

fn find<'a>(rows: &'a mut [Record], id: &str) -> Result<&'a mut Record> {
    rows.iter_mut()
        .find(|r| r.id == id && !id.is_empty())
        .ok_or_else(|| "Call not found.".into())
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// 96 random bits from the kernel, as 24 hex characters.
fn random_id() -> Result<String> {
    let mut random = [0u8; 12];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .map_err(|_| "Call ID unavailable.")?;
    Ok(random.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh `…/tech.getsecretary.secretary/files` directory under the
    /// system temp dir.
    pub(crate) fn app_dir(name: &str) -> String {
        let dir = std::env::temp_dir()
            .join(format!(
                "lucina-calls-{name}-{}-{}",
                std::process::id(),
                crate::now_ms()
            ))
            .join("tech.getsecretary.secretary/files");
        fs::create_dir_all(&dir).unwrap();
        dir.to_str().unwrap().to_owned()
    }

    fn id(n: u8) -> impl FnOnce() -> Result<String> {
        move || Ok(format!("{n:024x}"))
    }

    #[test]
    fn journal_is_confined_to_the_android_app() {
        assert!(Journal::open("/etc").is_err());
        assert!(Journal::open(
            "/data/user/0/tech.getsecretary.secretary/../x/tech.getsecretary.secretary/files"
        )
        .is_err());
        assert!(Journal::open("/data/user/0/tech.getsecretary.secretary/files").is_ok());
    }

    #[test]
    fn begin_append_end_and_history_newest_first() {
        let mut rows = Vec::new();
        let a = apply(
            &mut rows,
            Op::Begin,
            &json!({"kind": "sim", "peer": "+45\u{7}1234"}),
            10,
            id(1),
        )
        .unwrap();
        assert_eq!(a["id"], format!("{:024x}", 1));
        apply(
            &mut rows,
            Op::Begin,
            &json!({"kind": "internet"}),
            20,
            id(2),
        )
        .unwrap();
        assert_eq!(rows[0].peer, "+451234");
        assert_eq!(rows[1].peer, "Lucina");
        let first = rows[0].id.clone();
        let saved = apply(
            &mut rows,
            Op::Append,
            &json!({"id": first, "role": "user", "text": "hello"}),
            11,
            id(0),
        )
        .unwrap();
        assert_eq!(saved, json!({"saved": true}));
        apply(
            &mut rows,
            Op::End,
            &json!({"id": first, "status": "disconnected"}),
            12,
            id(0),
        )
        .unwrap();
        assert_eq!(rows[0].ended_at, Some(12));
        assert_eq!(rows[0].status, "disconnected");
        let history = apply(&mut rows, Op::History, &json!({}), 0, id(0)).unwrap();
        assert_eq!(history["records"][0]["kind"], "internet");
        assert_eq!(history["records"][1]["turns"][0]["text"], "hello");
        assert_eq!(history["records"][1]["endedAt"], 12);
        assert!(history["records"][0].get("endedAt").is_none());
    }

    #[test]
    fn invalid_operations_are_refused() {
        let mut rows = Vec::new();
        assert_eq!(
            apply(&mut rows, Op::Begin, &json!({"kind": "video"}), 0, id(1)).unwrap_err(),
            "Unknown call type."
        );
        apply(&mut rows, Op::Begin, &json!({"kind": "sim"}), 0, id(1)).unwrap();
        let rid = rows[0].id.clone();
        assert_eq!(
            apply(
                &mut rows,
                Op::Append,
                &json!({"id": "nope", "role": "user", "text": "x"}),
                0,
                id(0)
            )
            .unwrap_err(),
            "Call not found."
        );
        assert_eq!(
            apply(
                &mut rows,
                Op::Append,
                &json!({"id": rid, "role": "robot", "text": "x"}),
                0,
                id(0)
            )
            .unwrap_err(),
            "Unknown speaker."
        );
        let long = "a".repeat(MAX_TURN_CHARS + 1);
        assert_eq!(
            apply(
                &mut rows,
                Op::Append,
                &json!({"id": rid, "role": "user", "text": long}),
                0,
                id(0)
            )
            .unwrap_err(),
            "Transcript limit."
        );
        assert_eq!(
            apply(&mut rows, Op::End, &json!({}), 0, id(0)).unwrap_err(),
            "Call not found."
        );
        assert!(Op::parse("drop").is_err());
    }

    #[test]
    fn codes_never_enter_the_journal() {
        let mut rows = Vec::new();
        begin(&mut rows, "internet", None, 0, "a".into()).unwrap();
        for text in ["show my OTP", "Your verification code is 482913"] {
            let out = apply(
                &mut rows,
                Op::Append,
                &json!({"id": "a", "role": "user", "text": text}),
                0,
                id(0),
            )
            .unwrap();
            assert_eq!(out, json!({"saved": false, "sensitive": true}));
        }
        assert!(rows[0].turns.is_empty());
    }

    #[test]
    fn limits_drop_the_oldest_record_and_cap_turns() {
        let mut rows = Vec::new();
        for n in 0..=MAX_RECORDS {
            begin(&mut rows, "sim", None, n as u64, format!("{n}")).unwrap();
        }
        assert_eq!(rows.len(), MAX_RECORDS);
        assert_eq!(rows[0].id, "1");
        for _ in 0..MAX_TURNS {
            assert!(append(&mut rows, "1", "user", "hi", 0).unwrap());
        }
        assert_eq!(
            append(&mut rows, "1", "user", "hi", 0).unwrap_err(),
            "Transcript limit."
        );
    }

    #[test]
    fn file_round_trip_is_private_and_reads_0_1_0_history() {
        use std::os::unix::fs::PermissionsExt;
        let dir = app_dir("journal");
        let old = r#"[{"id":"00ff","kind":"sim","peer":"+4512345678","startedAt":5,"status":"active","turns":[{"role":"user","text":"hi","at":6}]}]"#;
        fs::write(Path::new(&dir).join("calls.json"), old).unwrap();
        let j = Journal::open(&dir).unwrap();
        j.end("00ff", "disconnected").unwrap();
        let id = j.begin("recording", "Imported recording").unwrap();
        assert_eq!(id.len(), 24);
        assert!(j.append(&id, "recording", "transcript").unwrap());
        assert!(!j.append(&id, "recording", "/otp").unwrap());
        let h = j.run(Op::History, &json!({})).unwrap();
        assert_eq!(h["records"][1]["status"], "disconnected");
        assert_eq!(h["records"][1]["turns"][0]["text"], "hi");
        assert_eq!(h["records"][0]["turns"].as_array().unwrap().len(), 1);
        let mode = fs::metadata(Path::new(&dir).join("calls.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!Path::new(&dir).join("calls.json.tmp").exists());
    }
}
