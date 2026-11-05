//! `<data_dir>/state.json` (0600): what each upstream last showed, what the owner was asked and what
//! he chose. Written through a temp file and a rename.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::fetch::ChangeSet;
use crate::judge::{significant, Verdict};
use crate::registry::{Settings, Upstream};
use crate::summary::Summary;

/// What the owner was asked about, kept for the brief when he answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asked {
    pub changes: ChangeSet,
    pub summary: Summary,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryState {
    pub last_checked: Option<String>,
    pub last_latest: Option<String>,
    pub last_commits_total: Option<u64>,
    /// The `latest` the owner was last asked about.
    pub notified_latest: Option<String>,
    pub decision_id: Option<String>,
    /// pending | decided | expired | withdrawn.
    pub decision_status: Option<String>,
    /// port | later | ignore.
    pub chosen: Option<String>,
    /// His own words instead of an option (recorded, no action).
    pub owner_reply: Option<String>,
    pub decided_at: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_cwd: Option<String>,
    pub brief_path: Option<String>,
    /// He chose "Later": listed in every weekly report.
    pub backlog: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<Asked>,
}

impl EntryState {
    pub fn pending(&self) -> bool {
        self.decision_status.as_deref() == Some("pending")
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ContribState {
    pub decision_id: Option<String>,
    pub decision_status: Option<String>,
    /// draft | later | never.
    pub chosen: Option<String>,
    pub owner_reply: Option<String>,
    pub decided_at: Option<String>,
    /// drafting once the draft plan is written.
    pub status: Option<String>,
    pub brief_path: Option<String>,
}

impl ContribState {
    pub fn pending(&self) -> bool {
        self.decision_status.as_deref() == Some("pending")
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// When the last (not dry) check finished: "our side" counts from there.
    pub last_run: Option<String>,
    pub upstream: BTreeMap<String, EntryState>,
    pub contrib: BTreeMap<String, ContribState>,
}

impl State {
    /// A missing file is an empty state.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{}: not a state file", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        write_private(path, &serde_json::to_vec_pretty(self)?)
    }
}

/// Write `bytes` to `path` with mode 0600, through a temp file and a rename.
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id()
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("{}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("{}", path.display()))?;
    Ok(())
}

/// Ask the owner: an entry that asks (`notify` is not `report`), significant, something new,
/// nothing pending, and newer than what he was last asked about ("Later" and "Ignore" are not asked
/// again until something newer arrives).
pub fn needs_asking(
    up: &Upstream,
    es: &EntryState,
    cs: &ChangeSet,
    v: &Verdict,
    s: &Settings,
) -> bool {
    up.asks() && worth_asking(es, cs, v, s)
}

/// [`needs_asking`] without the entry's `notify` (the report says "report only" for those).
pub fn worth_asking(es: &EntryState, cs: &ChangeSet, v: &Verdict, s: &Settings) -> bool {
    significant(v, s)
        && !cs.is_empty()
        && !es.pending()
        && cs.latest.is_some()
        && cs.latest != es.notified_latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Notify;
    use crate::testutil::{changes, upstream, verdict_of};

    #[test]
    fn asking_rules() {
        let s = Settings::default();
        let up = upstream();
        let needs_asking = |es: &EntryState, cs: &ChangeSet, v: &Verdict, s: &Settings| {
            needs_asking(&up, es, cs, v, s)
        };
        let cs = changes("v0.20.0", 30);
        let big = verdict_of(3, 0.1, 0.1);
        let fresh = EntryState::default();
        assert!(needs_asking(&fresh, &cs, &big, &s), "new");
        assert!(
            !needs_asking(&fresh, &cs, &verdict_of(2, 0.1, 0.1), &s),
            "not significant"
        );
        assert!(
            !needs_asking(&fresh, &changes("v0.19.0", 0), &big, &s),
            "empty"
        );

        let asked = EntryState {
            notified_latest: Some("v0.20.0".into()),
            decision_status: Some("decided".into()),
            chosen: Some("later".into()),
            backlog: true,
            ..EntryState::default()
        };
        assert!(!needs_asking(&asked, &cs, &big, &s), "same latest");
        assert!(
            needs_asking(&asked, &changes("v0.21.0", 60), &big, &s),
            "later → newer"
        );

        let pending = EntryState {
            notified_latest: Some("v0.19.5".into()),
            decision_status: Some("pending".into()),
            ..EntryState::default()
        };
        assert!(!needs_asking(&pending, &cs, &big, &s), "pending");

        let mut quiet = upstream();
        quiet.notify = Some(Notify::Report);
        assert!(
            !super::needs_asking(&quiet, &fresh, &cs, &big, &s),
            "notify = report"
        );
        assert!(worth_asking(&fresh, &cs, &big, &s));
    }

    #[test]
    fn round_trip_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/state.json");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let mut st = State::default();
        st.upstream.insert(
            "x".into(),
            EntryState {
                decision_id: Some("dc-0123456789ab".into()),
                backlog: true,
                ..EntryState::default()
            },
        );
        st.contrib.insert("c".into(), ContribState::default());
        st.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), st);
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temp file left"
        );
    }
}
