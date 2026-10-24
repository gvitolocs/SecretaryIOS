//! The device's durable op log (local-first outbox).
//!
//! One JSON record per line, appended with `fsync` before the call returns:
//!
//! - `{"r":"header","device":"kobo-1","next":1}`: first line; `next` is the
//!   next counter (kept across compactions so counters never repeat);
//! - `{"r":"op","op":{…}}`: an op, written before any network I/O;
//! - `{"r":"ack","ids":["kobo-1:3",…]}`: ops the server acknowledged.
//!
//! Recovery: a last line without its newline is a torn append and is
//! truncated. Any other unreadable line is an error: the log is never
//! silently shortened. Unacked ops are replayed in counter order; acked ones
//! are never sent again. When every op is acked (or the log grows), the log is
//! compacted to the header plus the unacked ops (temp file, fsync, rename,
//! directory fsync).
use crate::op::{valid_device, Op, OpId, OpKind};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Compact once this many records follow the header.
pub const COMPACT_AFTER: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("outbox line {line} is corrupt: {reason}")]
    Corrupt { line: usize, reason: String },
    #[error("outbox belongs to device {found:?}, not {expected:?}")]
    WrongDevice { expected: String, found: String },
    #[error("invalid device id")]
    InvalidDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "r", rename_all = "camelCase")]
enum Record {
    Header { device: String, next: u64 },
    Op { op: Op },
    Ack { ids: Vec<OpId> },
}

#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    file: File,
    device: String,
    next: u64,
    /// Unacked ops, by counter.
    pending: Vec<Op>,
    records: usize,
}

fn sync_dir(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn line(r: &Record) -> Vec<u8> {
    let mut v = serde_json::to_vec(r).expect("records serialize");
    v.push(b'\n');
    v
}

impl Outbox {
    /// Opens (or creates) the log of `device` at `path`, recovering a torn tail.
    pub fn open(path: impl AsRef<Path>, device: &str) -> Result<Self, OutboxError> {
        if !valid_device(device) {
            return Err(OutboxError::InvalidDevice);
        }
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            Self::write_fresh(&path, device, 1, &[])?;
        }
        let mut file = OpenOptions::new().read(true).append(true).open(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // A torn append: keep everything up to the last newline.
        let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if keep < bytes.len() {
            file.set_len(keep as u64)?;
            file.sync_all()?;
            bytes.truncate(keep);
        }
        let mut next = 0;
        let mut pending: Vec<Op> = Vec::new();
        let mut records = 0;
        for (i, raw) in bytes.split(|&b| b == b'\n').enumerate() {
            if raw.is_empty() {
                continue;
            }
            let corrupt = |reason: String| OutboxError::Corrupt {
                line: i + 1,
                reason,
            };
            let rec: Record = serde_json::from_slice(raw).map_err(|e| corrupt(e.to_string()))?;
            match (i, rec) {
                (0, Record::Header { device: d, next: n }) => {
                    if d != device {
                        return Err(OutboxError::WrongDevice {
                            expected: device.into(),
                            found: d,
                        });
                    }
                    next = n;
                }
                (0, _) => return Err(corrupt("missing header".into())),
                (_, Record::Header { .. }) => return Err(corrupt("second header".into())),
                (_, Record::Op { op }) => {
                    if op.op_id.device != device {
                        return Err(corrupt("op of another device".into()));
                    }
                    next = next.max(op.op_id.counter + 1);
                    if !pending.iter().any(|p| p.op_id == op.op_id) {
                        pending.push(op);
                    }
                    records += 1;
                }
                (_, Record::Ack { ids }) => {
                    pending.retain(|p| !ids.contains(&p.op_id));
                    records += 1;
                }
            }
        }
        if next == 0 {
            return Err(OutboxError::Corrupt {
                line: 1,
                reason: "empty log".into(),
            });
        }
        pending.sort_by_key(|o| o.op_id.counter);
        Ok(Outbox {
            path,
            file,
            device: device.into(),
            next,
            pending,
            records,
        })
    }

    fn write_fresh(path: &Path, device: &str, next: u64, ops: &[Op]) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            let mut buf = line(&Record::Header {
                device: device.into(),
                next,
            });
            for op in ops {
                buf.extend(line(&Record::Op { op: op.clone() }));
            }
            f.write_all(&buf)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        sync_dir(path)
    }

    fn append(&mut self, r: &Record) -> io::Result<()> {
        self.file.write_all(&line(r))?;
        self.file.sync_data()?;
        self.records += 1;
        Ok(())
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// Unacked ops, oldest first.
    pub fn pending(&self) -> &[Op] {
        &self.pending
    }

    pub fn next_counter(&self) -> u64 {
        self.next
    }

    /// Records a new op durably and returns it. Only after this returns may
    /// the op reach the network.
    pub fn record(&mut self, at: impl Into<String>, kind: OpKind) -> io::Result<Op> {
        let op = Op {
            op_id: OpId::new(self.device.clone(), self.next),
            at: at.into(),
            kind,
        };
        self.append(&Record::Op { op: op.clone() })?;
        self.next += 1;
        self.pending.push(op.clone());
        Ok(op)
    }

    /// Durably marks `ids` as acked (ids not pending are ignored). Returns how
    /// many pending ops were acked.
    pub fn ack(&mut self, ids: &[OpId]) -> io::Result<usize> {
        let ids: Vec<OpId> = ids
            .iter()
            .filter(|id| self.pending.iter().any(|p| p.op_id == **id))
            .cloned()
            .collect();
        if ids.is_empty() {
            return Ok(0);
        }
        self.append(&Record::Ack { ids: ids.clone() })?;
        self.pending.retain(|p| !ids.contains(&p.op_id));
        if self.pending.is_empty() || self.records >= COMPACT_AFTER {
            self.compact()?;
        }
        Ok(ids.len())
    }

    /// Rewrites the log as the header plus the unacked ops.
    pub fn compact(&mut self) -> io::Result<()> {
        Self::write_fresh(&self.path, &self.device, self.next, &self.pending)?;
        self.file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        self.records = self.pending.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_library::model::ReadingStatus;

    fn kind() -> OpKind {
        OpKind::SetStatus {
            book_id: "bk_0123456789abcdef".into(),
            status: ReadingStatus::Reading,
        }
    }

    #[test]
    fn ops_survive_restart_and_acks_are_durable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("outbox.jsonl");
        let mut o = Outbox::open(&p, "kobo-1").unwrap();
        let a = o.record("2026-10-24T09:00:00Z", kind()).unwrap();
        let b = o.record("2026-10-24T09:00:01Z", kind()).unwrap();
        assert_eq!((a.op_id.counter, b.op_id.counter), (1, 2));
        o.ack(std::slice::from_ref(&a.op_id)).unwrap();
        drop(o);
        let o = Outbox::open(&p, "kobo-1").unwrap();
        assert_eq!(o.pending(), &[b]);
        assert_eq!(o.next_counter(), 3);
    }

    #[test]
    fn counters_never_repeat_after_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("outbox.jsonl");
        let mut o = Outbox::open(&p, "d").unwrap();
        let a = o.record("2026-10-24T09:00:00Z", kind()).unwrap();
        o.ack(&[a.op_id]).unwrap(); // compacts: header only
        drop(o);
        let mut o = Outbox::open(&p, "d").unwrap();
        assert!(o.pending().is_empty());
        assert_eq!(
            o.record("2026-10-24T09:00:00Z", kind())
                .unwrap()
                .op_id
                .counter,
            2
        );
    }

    #[test]
    fn torn_tail_is_truncated_but_corruption_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("outbox.jsonl");
        let mut o = Outbox::open(&p, "d").unwrap();
        let a = o.record("2026-10-24T09:00:00Z", kind()).unwrap();
        drop(o);
        // A crash in the middle of the next append.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(br#"{"r":"op","op":{"opId":"d:2","at":"2026"#)
            .unwrap();
        drop(f);
        let mut o = Outbox::open(&p, "d").unwrap();
        assert_eq!(o.pending(), std::slice::from_ref(&a));
        // The torn op never existed: its counter is reused, nothing was sent.
        assert_eq!(
            o.record("2026-10-24T09:00:00Z", kind())
                .unwrap()
                .op_id
                .counter,
            2
        );
        drop(o);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"garbage\n").unwrap();
        drop(f);
        assert!(matches!(
            Outbox::open(&p, "d"),
            Err(OutboxError::Corrupt { line: 4, .. })
        ));
        assert!(matches!(
            Outbox::open(&p, "other"),
            Err(OutboxError::WrongDevice { .. })
        ));
    }
}
