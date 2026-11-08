//! Lucina media library: the Drive's folder index against a disk that comes and goes
//! (`crates/lucina-media/src/drive.rs::index_drive_folder`, `ingest.rs::keep_owned_bytes`; the
//! pure kernels are `crates/lucina-media/src/drive_kernel_shared.rs`, proved in
//! `verification/verus/drive_kernels.rs`). TLA+ twin: `formal/drive/DriveIndex.tla`.
//!
//! A Drive folder that is the big disk's mountpoint holds `FILES` files. A scan walks them in
//! order (`Visit`), then decides whether to trash the catalog rows of files it did not see
//! (`Finish`, kernel DK4). Meanwhile the environment unmounts and remounts the disk (an unmounted
//! mountpoint is an empty directory on another filesystem, where a stray file can be written),
//! deletes and edits files, and a phone uploads the same bytes as file 0's first version.
//!
//! A walk step behaves as on Linux: a file read while its filesystem is gone is a walk error
//! (`metadata` by path fails); a scan started on the empty mountpoint sees only what is there.
//! Ingest of the phone copy is one step under the catalog lock (`keep_owned_bytes`, DK3).
//!
//! Switches (the fixes, and ablations that show each guard is needed):
//! - `prune`: `Kernel` (DK4 as shipped), `NoFsCheck` (DK4 without the filesystem check), `NoGuard`
//!   (trash whatever the walk did not see);
//! - `owned_upgrade`: DK3 as shipped (an in-place blob is upgraded to the phone's owned copy);
//!   off: the phone copy is dropped whenever a blob row exists (the code before the Drive).

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const FILES: usize = 2;
/// The walk position of the stray file (after the real files).
const STRAY: usize = FILES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prune {
    Kernel,
    NoFsCheck,
    NoGuard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fs {
    Disk,
    Mountpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    pub present: bool,
    pub trashed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scan {
    pub fs: Fs,
    pub root_ok: bool,
    pub same_fs: bool,
    pub next: usize,
    pub seen: [bool; FILES + 1],
    pub errors: u8,
    /// Ghost: the real file was on the disk at scan start and has not been deleted since.
    pub kept_since_start: [bool; FILES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Blob {
    /// The library owns a copy (store or spool).
    Owned,
    /// Only file 0 on the Drive holds these bytes.
    InPlace,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub mounted: bool,
    /// File versions on the disk (`None`: deleted).
    pub disk: [Option<u8>; FILES],
    pub stray: bool,
    /// Catalog rows: the real files, then the stray file.
    pub rows: [Row; FILES + 1],
    pub recorded_fs: Option<Fs>,
    pub scan: Option<Scan>,
    /// The bytes of file 0's version 0 (`None`: no row).
    pub blob: Option<Blob>,
    pub phone_item: bool,
    // Budgets.
    pub scans: u8,
    pub mounts: u8,
    pub deletes: u8,
    pub edits: u8,
    // Ghosts and witnesses.
    pub bad_prune: bool,
    pub was_owned: bool,
    pub pruned_deleted: bool,
    pub stray_seen: bool,
    pub walk_error: bool,
    pub upgraded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    StartScan,
    Visit,
    Finish,
    Unmount,
    Mount,
    Delete(usize),
    Edit(usize),
    StrayWrite,
    PhoneUpload,
}

#[derive(Clone, Debug)]
pub struct DriveIndex {
    pub prune: Prune,
    pub owned_upgrade: bool,
    pub scans: u8,
    pub mounts: u8,
    pub deletes: u8,
    pub edits: u8,
}

/// DK4, as in drive_kernel_shared.rs.
fn may_prune(root_ok: bool, same_fs: bool, errors: u8, seen: usize, known: usize) -> bool {
    root_ok && same_fs && errors == 0 && (seen > 0 || known == 0)
}

/// DK4b.
fn may_adopt_fs(root_ok: bool, errors: u8, seen: usize, missing: usize) -> bool {
    root_ok && errors == 0 && seen > 0 && missing == 0
}

impl Model for DriveIndex {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        // Both files indexed from the disk; file 0's bytes are an in-place blob.
        vec![State {
            mounted: true,
            disk: [Some(0); FILES],
            stray: false,
            rows: {
                let mut r = [Row { present: true, trashed: false }; FILES + 1];
                r[STRAY] = Row { present: false, trashed: false };
                r
            },
            recorded_fs: Some(Fs::Disk),
            scan: None,
            blob: Some(Blob::InPlace),
            phone_item: false,
            scans: self.scans,
            mounts: self.mounts,
            deletes: self.deletes,
            edits: self.edits,
            bad_prune: false,
            was_owned: false,
            pruned_deleted: false,
            stray_seen: false,
            walk_error: false,
            upgraded: false,
        }]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        match &s.scan {
            None if s.scans > 0 => a.push(Action::StartScan),
            Some(sc) if sc.next <= STRAY => a.push(Action::Visit),
            Some(_) => a.push(Action::Finish),
            None => {}
        }
        if s.mounts > 0 {
            a.push(if s.mounted { Action::Unmount } else { Action::Mount });
        }
        for f in 0..FILES {
            if s.deletes > 0 && s.disk[f].is_some() {
                a.push(Action::Delete(f));
            }
            if s.edits > 0 && s.disk[f] == Some(0) {
                a.push(Action::Edit(f));
            }
        }
        if !s.mounted && !s.stray {
            a.push(Action::StrayWrite);
        }
        if !s.phone_item {
            a.push(Action::PhoneUpload);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::StartScan => {
                s.scans -= 1;
                let fs = if s.mounted { Fs::Disk } else { Fs::Mountpoint };
                let mut kept = [false; FILES];
                for (f, k) in kept.iter_mut().enumerate() {
                    *k = s.disk[f].is_some();
                }
                s.scan = Some(Scan {
                    fs,
                    // The mountpoint always exists.
                    root_ok: true,
                    same_fs: s.recorded_fs.is_none() || s.recorded_fs == Some(fs),
                    next: 0,
                    seen: [false; FILES + 1],
                    errors: 0,
                    kept_since_start: kept,
                });
            }
            Action::Visit => {
                let mounted = s.mounted;
                let (disk, stray) = (s.disk, s.stray);
                let sc = s.scan.as_mut()?;
                let i = sc.next;
                sc.next += 1;
                let mut index = None;
                match (sc.fs, i < STRAY) {
                    // A disk file: read on the disk's filesystem.
                    (Fs::Disk, true) => {
                        if !mounted {
                            sc.errors += 1;
                        } else if disk[i].is_some() {
                            sc.seen[i] = true;
                            index = Some(i);
                        }
                    }
                    (Fs::Disk, false) => {}
                    // The empty mountpoint holds no disk file.
                    (Fs::Mountpoint, true) => {}
                    (Fs::Mountpoint, false) => {
                        if mounted {
                            // The directory was read before the mount; the path now leads elsewhere.
                            if stray {
                                sc.errors += 1;
                            }
                        } else if stray {
                            sc.seen[STRAY] = true;
                            index = Some(STRAY);
                        }
                    }
                }
                if sc.errors > 0 {
                    s.walk_error = true;
                }
                if let Some(i) = index {
                    // register_external: a seen file's row is (re)indexed and untrashed.
                    s.rows[i] = Row { present: true, trashed: false };
                    if i == STRAY {
                        s.stray_seen = true;
                    }
                }
            }
            Action::Finish => {
                let sc = s.scan.take()?;
                let known = s.rows.iter().filter(|r| r.present).count();
                let seen = sc.seen.iter().filter(|x| **x).count();
                let missing = (0..=STRAY).filter(|i| s.rows[*i].present && !sc.seen[*i]).count();
                if may_adopt_fs(sc.root_ok, sc.errors, seen, missing) {
                    s.recorded_fs = Some(sc.fs);
                }
                let prune = match self.prune {
                    Prune::Kernel => may_prune(sc.root_ok, sc.same_fs, sc.errors, seen, known),
                    Prune::NoFsCheck => may_prune(sc.root_ok, true, sc.errors, seen, known),
                    Prune::NoGuard => true,
                };
                if prune {
                    for i in 0..=STRAY {
                        if s.rows[i].present && !sc.seen[i] {
                            // The row goes (trashed in the catalog; the file is never touched).
                            if i < FILES && sc.kept_since_start[i] && s.disk[i].is_some() {
                                s.bad_prune = true;
                            }
                            if i < FILES && s.disk[i].is_none() {
                                s.pruned_deleted = true;
                            }
                            s.rows[i] = Row { present: false, trashed: true };
                        }
                    }
                }
            }
            Action::Unmount => {
                s.mounts -= 1;
                s.mounted = false;
            }
            Action::Mount => {
                s.mounts -= 1;
                s.mounted = true;
            }
            Action::Delete(f) => {
                s.deletes -= 1;
                s.disk[f] = None;
                if let Some(sc) = s.scan.as_mut() {
                    sc.kept_since_start[f] = false;
                }
            }
            Action::Edit(f) => {
                s.edits -= 1;
                s.disk[f] = Some(1);
            }
            Action::StrayWrite => s.stray = true,
            Action::PhoneUpload => {
                s.phone_item = true;
                s.blob = Some(match s.blob {
                    None => Blob::Owned,
                    Some(Blob::Owned) => Blob::Owned,
                    Some(Blob::InPlace) if self.owned_upgrade => {
                        s.upgraded = true;
                        Blob::Owned
                    }
                    Some(Blob::InPlace) => Blob::InPlace,
                });
            }
        }
        if s.blob == Some(Blob::Owned) {
            s.was_owned = true;
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // The catalog never trashes a file that stayed on the disk through the scan.
            Property::always("unmount_never_trashes", |_, s: &State| !s.bad_prune),
            // The phone's original is always somewhere: owned, or file 0 still holds its bytes.
            Property::always("phone_original_kept", |_, s: &State| {
                !s.phone_item
                    || s.blob == Some(Blob::Owned)
                    || (s.blob == Some(Blob::InPlace) && s.disk[0] == Some(0))
            }),
            Property::always("owned_never_downgraded", |_, s: &State| {
                !s.was_owned || s.blob == Some(Blob::Owned)
            }),
            Property::sometimes("pruned_a_deleted_file", |_, s: &State| s.pruned_deleted),
            Property::sometimes("stray_file_seen", |_, s: &State| s.stray_seen),
            Property::sometimes("walk_error", |_, s: &State| s.walk_error),
            Property::sometimes("dedupe_upgraded", |_, s: &State| s.upgraded),
        ]
    }
}

pub const PROTOCOL: &str = "drive-index";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let model = |prune, owned_upgrade| DriveIndex {
        prune,
        owned_upgrade,
        scans: if deep { 4 } else { 3 },
        mounts: if deep { 3 } else { 2 },
        deletes: 1,
        edits: 1,
    };
    let reached = |v: &mut Vec<(&'static str, Expect)>, upgrade: bool| {
        v.push(("pruned_a_deleted_file", Reached));
        v.push(("stray_file_seen", Reached));
        v.push(("walk_error", Reached));
        if upgrade {
            v.push(("dedupe_upgraded", Reached));
        }
    };
    let expect = |trash: Expect, kept: Expect, upgrade: bool| {
        let mut v = vec![
            ("unmount_never_trashes", trash),
            ("phone_original_kept", kept),
            ("owned_never_downgraded", Holds),
        ];
        reached(&mut v, upgrade);
        v
    };
    vec![
        Case::new(
            PROTOCOL,
            "DI current (DK3 upgrade, DK4 with the filesystem check)",
            model(Prune::Kernel, true),
            None,
            expect(Holds, Holds, true),
        ),
        Case::new(
            PROTOCOL,
            "DI ablation: DK4 without the filesystem check (stray file in the mountpoint)",
            model(Prune::NoFsCheck, true),
            None,
            expect(Violated, Holds, true),
        ),
        Case::new(
            PROTOCOL,
            "DI ablation: trash whatever the walk did not see",
            model(Prune::NoGuard, true),
            None,
            expect(Violated, Holds, true),
        ),
        Case::new(
            PROTOCOL,
            "DI before DK3 (a phone copy of Drive bytes was dropped)",
            model(Prune::Kernel, false),
            None,
            expect(Holds, Violated, false),
        ),
    ]
}
