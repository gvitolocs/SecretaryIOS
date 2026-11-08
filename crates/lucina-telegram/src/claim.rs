//! At-most-once update handling: the highest claimed `update_id`, persisted
//! before any update of a batch reaches a handler.
//!
//! The two decision functions are pure (mirrored by a Verus kernel on another
//! branch): an update is handled only when its id is above the claim, and
//! claiming it raises the claim to the larger of the two. A crash after the
//! claim is written and before the handler finishes loses that update rather
//! than handling it twice.
//!
//! File: `<data_dir>/lucina-rs/telegram-<botKey>-claim.json`,
//! `{"claimedUpdateId": N, "updatedAt": "<iso>"}`, written to a temp file,
//! fsynced and renamed. A missing file is no claim.
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// `update_id > claimed`.
pub fn should_handle(update_id: u64, claimed: u64) -> bool {
    update_id > claimed
}

/// `max(update_id, claimed)`.
pub fn claim(update_id: u64, claimed: u64) -> u64 {
    update_id.max(claimed)
}

/// [`should_handle`] with "no claim yet" (a missing file): everything is new.
pub fn should_handle_opt(update_id: u64, claimed: Option<u64>) -> bool {
    claimed.is_none_or(|c| should_handle(update_id, c))
}

/// [`claim`] with "no claim yet".
pub fn claim_opt(update_id: u64, claimed: Option<u64>) -> u64 {
    claimed.map_or(update_id, |c| claim(update_id, c))
}

/// The `getUpdates` offset after `claimed`: everything up to it is confirmed.
pub fn next_offset(claimed: Option<u64>) -> i64 {
    claimed.map_or(0, |c| {
        i64::try_from(c.saturating_add(1)).unwrap_or(i64::MAX)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    claimed_update_id: u64,
    #[serde(default)]
    updated_at: String,
}

/// The claim file of one bot.
#[derive(Debug, Clone)]
pub struct ClaimFile {
    path: PathBuf,
}

impl ClaimFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<data_dir>/lucina-rs/telegram-<botKey>-claim.json`.
    pub fn for_bot(data_dir: &Path, bot_key: &str) -> Self {
        Self::new(
            data_dir
                .join("lucina-rs")
                .join(format!("telegram-{bot_key}-claim.json")),
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The persisted claim; `Ok(None)` when there is no file. An unreadable
    /// or corrupt file is an error: polling must not restart from zero and
    /// replay updates that were already claimed.
    pub fn load(&self) -> io::Result<Option<u64>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<Record>(&bytes)
                .map(|r| Some(r.claimed_update_id))
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Persist `claimed` durably (temp file, fsync, rename, fsync of the directory).
    pub fn store(&self, claimed: u64) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let record = Record {
            claimed_update_id: claimed,
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        let mut bytes = serde_json::to_vec_pretty(&record)?;
        bytes.push(b'\n');
        let tmp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn kernel_cases() {
        assert!(should_handle(5, 4));
        assert!(!should_handle(4, 4));
        assert!(!should_handle(3, 4));
        assert_eq!(claim(5, 4), 5);
        assert_eq!(claim(3, 4), 4);
        assert!(should_handle_opt(0, None));
        assert_eq!(claim_opt(7, None), 7);
        assert_eq!(next_offset(None), 0);
        assert_eq!(next_offset(Some(41)), 42);
        assert_eq!(next_offset(Some(u64::MAX)), i64::MAX);
    }

    proptest! {
        /// Once claimed, an id is never handled again, whatever comes after.
        #[test]
        fn claimed_ids_are_never_handled_again(ids in proptest::collection::vec(0u64..50, 0..40)) {
            let mut claimed: Option<u64> = None;
            let mut handled = std::collections::BTreeSet::new();
            for id in ids {
                if should_handle_opt(id, claimed) {
                    prop_assert!(handled.insert(id), "handled {} twice", id);
                    claimed = Some(claim_opt(id, claimed));
                }
                prop_assert!(handled.iter().all(|h| claimed.is_some_and(|c| *h <= c)));
            }
        }
    }

    #[test]
    fn file_round_trip_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let f = ClaimFile::for_bot(dir.path(), "123");
        assert_eq!(f.load().unwrap(), None);
        f.store(41).unwrap();
        assert_eq!(f.load().unwrap(), Some(41));
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.path()).unwrap()).unwrap();
        assert_eq!(raw["claimedUpdateId"], 41);
        assert!(raw["updatedAt"].as_str().unwrap().ends_with('Z'));
        assert!(f.path().ends_with("lucina-rs/telegram-123-claim.json"));
        std::fs::write(f.path(), b"{oops").unwrap();
        assert!(
            f.load().is_err(),
            "a corrupt claim never restarts from zero"
        );
    }
}
