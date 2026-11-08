// The rules of vault sync that must hold whatever the two sides hold: what one file's sync does, given the copy on this
// device, the copy on the server and the copy both had at the last sync (the base), and which paths take part. Verus
// checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011, as in LucinaOS). Mutants
// that Verus must refuse: verification/vault-sync/mutants.sh. `sync.rs` computes the facts and carries out the act.
//
// The promises, all proved below:
// - An edit is never lost. A file that changed on one side since the base is never overwritten, moved to the trash or
//   forgotten. It is sent to the other side, or, when both sides changed it differently, both texts are kept: the other
//   side's at the path, this side's as a conflict copy (`Note (conflict 2026-11-08 juno).md`).
// - A deletion yields to an edit: a file deleted on one side and edited on the other comes back with the edit.
// - Only a file that is still the base copy may follow a deletion, and it goes to the vault's `.trash`, never away.
// - Two equal copies need nothing but a new base.
// - Paths: only clean relative paths take part; dot folders (Obsidian's `.trash`, Syncthing's `.stversions`, temporary
//   files) never do; `.obsidian/<name>.json` comes from the server and never goes back.
//
// Not proved here: that the facts match the bytes (sha256 revisions compared in `sync.rs`, tested), and the
// compare-and-swap that makes each act refuse when a side changed after the manifests were read (`write.rs`, tested).

/// The longest path that takes part, in bytes.
pub const MAX_PATH: usize = 1024;
/// The longest single name in a path, in bytes (ext4, APFS and XFS limit).
pub const MAX_NAME: usize = 255;

/// What one file's sync does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] //@omit
pub enum Act {
    /// Absent on both sides: forget the base.
    Nothing,
    /// The same on both sides: the base becomes that copy.
    Record,
    /// Bring the server's copy here (in place of this side's base copy, or where there is none).
    Pull,
    /// Send this side's copy to the server (in place of its base copy, or where there is none).
    Push,
    /// Deleted on the server and unchanged here: move this side's copy to the trash.
    TrashLocal,
    /// Deleted here and unchanged on the server: move the server's copy to the trash.
    TrashRemote,
    /// Changed differently on both sides: keep this side's copy as a conflict copy, then pull.
    Conflict,
}

/// What a path is to sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] //@omit
pub enum PathClass {
    /// Both ways.
    Sync,
    /// From the server only (Obsidian's settings, which the core never writes).
    PullOnly,
    /// Not at all.
    Skip,
}

/// One file's state. `local_is_base` and the like are only true when both copies exist and have the same revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] //@omit
pub struct Facts {
    pub base: bool,
    pub local: bool,
    pub remote: bool,
    pub local_is_base: bool,
    pub remote_is_base: bool,
    pub local_is_remote: bool,
}

//@ pub open spec fn well_formed(f: Facts) -> bool {
//@     (f.local_is_base ==> f.local && f.base) && (f.remote_is_base ==> f.remote && f.base)
//@     && (f.local_is_remote ==> f.local && f.remote)
//@     && (f.local_is_base && f.remote_is_base ==> f.local_is_remote)
//@     && (f.local_is_base && f.local_is_remote ==> f.remote_is_base)
//@     && (f.remote_is_base && f.local_is_remote ==> f.local_is_base)
//@ }

/// The act for one file. Any facts give an act; the promises are for facts that come from real revisions.
pub fn decide(f: Facts) -> Act //@ret
//@ ensures well_formed(f) ==> (
//@     // An edit here is never lost: sent, or kept as a conflict copy.
//@     (f.local && !f.local_is_base && !f.local_is_remote ==> (ret == Act::Push || ret == Act::Conflict))
//@     // An edit on the server is never lost: brought here, or kept there while this side's text becomes a copy.
//@     && (f.remote && !f.remote_is_base && !f.local_is_remote ==> (ret == Act::Pull || ret == Act::Conflict))
//@     // A deletion yields to an edit.
//@     && (f.base && !f.local && f.remote && !f.remote_is_base ==> ret == Act::Pull)
//@     && (f.base && !f.remote && f.local && !f.local_is_base ==> ret == Act::Push)
//@     // Equal copies, or none.
//@     && (f.local_is_remote ==> ret == Act::Record)
//@     && (!f.local && !f.remote ==> ret == Act::Nothing)
//@     // Each act has what it needs, and the trash only takes a base copy whose other side is gone.
//@     && (ret == Act::Pull ==> f.remote && !f.local_is_remote && (!f.local || f.local_is_base))
//@     && (ret == Act::Push ==> f.local && !f.local_is_remote && (!f.remote || f.remote_is_base))
//@     && (ret == Act::TrashLocal ==> f.local && f.local_is_base && !f.remote)
//@     && (ret == Act::TrashRemote ==> f.remote && f.remote_is_base && !f.local)
//@     && (ret == Act::Conflict ==> f.local && f.remote && !f.local_is_remote && !f.local_is_base && !f.remote_is_base)
//@     && (ret == Act::Record ==> f.local_is_remote)
//@     && (ret == Act::Nothing ==> !f.local && !f.remote)
//@ ),
{
    if !f.local && !f.remote {
        return Act::Nothing;
    }
    if f.local_is_remote {
        return Act::Record;
    }
    if !f.local {
        // Only the server has it: new there, edited there, or deleted here.
        if f.remote_is_base {
            return Act::TrashRemote;
        }
        return Act::Pull;
    }
    if !f.remote {
        if f.local_is_base {
            return Act::TrashLocal;
        }
        return Act::Push;
    }
    // Both have it, and the copies differ.
    if f.local_is_base {
        return Act::Pull;
    }
    if f.remote_is_base {
        return Act::Push;
    }
    Act::Conflict
}

/// A byte that never appears in a path that takes part: controls, DEL and the backslash.
pub fn bad_byte(b: u8) -> bool //@ret
//@ ensures ret == (b < 32 || b == 127 || b == 92),
{
    b < 32 || b == 127 || b == 92
}

//@ pub open spec fn starts_obsidian(p: Seq<u8>) -> bool {
//@     p.len() > 10 && p[0] == 46u8 && p[1] == 111u8 && p[2] == 98u8 && p[3] == 115u8 && p[4] == 105u8
//@     && p[5] == 100u8 && p[6] == 105u8 && p[7] == 97u8 && p[8] == 110u8 && p[9] == 47u8
//@ }
//@ pub open spec fn ends_json(p: Seq<u8>) -> bool {
//@     p.len() >= 5 && p[p.len() - 5] == 46u8 && p[p.len() - 4] == 106u8 && p[p.len() - 3] == 115u8
//@     && p[p.len() - 2] == 111u8 && p[p.len() - 1] == 110u8
//@ }

/// `.obsidian/` at the start (10 bytes).
pub fn is_obsidian_dir(p: &[u8]) -> bool //@ret
//@ ensures ret == (p@.len() > 10 && starts_obsidian(p@)),
{
    p.len() > 10 && p[0] == b'.' && p[1] == b'o' && p[2] == b'b' && p[3] == b's' && p[4] == b'i' && p[5] == b'd'
        && p[6] == b'i' && p[7] == b'a' && p[8] == b'n' && p[9] == b'/'
}

/// `.json` at the end.
pub fn is_json(p: &[u8]) -> bool //@ret
//@ ensures ret == ends_json(p@),
{
    let n = p.len();
    n >= 5 && p[n - 5] == b'.' && p[n - 4] == b'j' && p[n - 3] == b's' && p[n - 2] == b'o' && p[n - 1] == b'n'
}

/// Which way a path (UTF-8 bytes, `/` between names, relative to the vault) takes part.
pub fn classify(p: &[u8]) -> PathClass //@ret
//@ ensures
//@     ret != PathClass::Skip ==> (0 < p@.len() <= MAX_PATH && forall|i: int| 0 <= i < p@.len() ==> !(p@[i] < 32u8 || p@[i] == 127u8 || p@[i] == 92u8)),
//@     ret != PathClass::Skip ==> (p@[0] != 47u8 && p@[p@.len() - 1] != 47u8
//@         && forall|i: int| 0 <= i < p@.len() - 1 ==> !(#[trigger] p@[i] == 47u8 && p@[i + 1] == 47u8)),
//@     ret == PathClass::Sync ==> (p@[0] != 46u8 && forall|i: int| 0 <= i < p@.len() - 1 ==> !(#[trigger] p@[i] == 47u8 && p@[i + 1] == 46u8)),
//@     ret == PathClass::PullOnly ==> (starts_obsidian(p@) && ends_json(p@)
//@         && forall|i: int| 10 <= i < p@.len() ==> p@[i] != 47u8),
{
    let n = p.len();
    if n == 0 || n > MAX_PATH {
        return PathClass::Skip;
    }
    let obsidian = is_obsidian_dir(p);
    // One pass: bad bytes, empty names, names too long, dot names (except `.obsidian` itself), and for a settings path
    // any folder below `.obsidian/`.
    let mut i: usize = 0;
    let mut name_len: usize = 0;
    while i < n
    //@ invariant n == p@.len(), 0 < n <= MAX_PATH, i <= n, name_len <= i, name_len <= MAX_NAME,
    //@     forall|j: int| 0 <= j < i ==> !(p@[j] < 32u8 || p@[j] == 127u8 || p@[j] == 92u8),
    //@     i > 0 ==> p@[0] != 47u8,
    //@     forall|j: int| 0 <= j < i - 1 ==> !(#[trigger] p@[j] == 47u8 && p@[j + 1] == 47u8),
    //@     i > 0 && name_len == 0 ==> p@[i - 1] == 47u8,
    //@     i > 0 && p@[i - 1] == 47u8 ==> name_len == 0,
    //@     (!obsidian && i > 0) ==> p@[0] != 46u8,
    //@     !obsidian ==> forall|j: int| 0 <= j < i - 1 ==> !(#[trigger] p@[j] == 47u8 && p@[j + 1] == 46u8),
    //@     obsidian ==> starts_obsidian(p@),
    //@     obsidian ==> forall|j: int| 10 <= j < i ==> p@[j] != 47u8,
    //@ decreases n - i,
    {
        let b = p[i];
        if bad_byte(b) {
            return PathClass::Skip;
        }
        if b == b'/' {
            if name_len == 0 {
                // A leading `/` or `//`.
                return PathClass::Skip;
            }
            if obsidian && i >= 10 {
                // A folder below `.obsidian/`: plugins, themes, snippets.
                return PathClass::Skip;
            }
            name_len = 0;
        } else {
            if b == b'.' && name_len == 0 && !(obsidian && i == 0) {
                return PathClass::Skip;
            }
            if name_len == MAX_NAME {
                return PathClass::Skip;
            }
            name_len += 1;
        }
        i += 1;
    }
    if name_len == 0 {
        // A trailing `/`.
        return PathClass::Skip;
    }
    if obsidian {
        if is_json(p) {
            return PathClass::PullOnly;
        }
        return PathClass::Skip;
    }
    PathClass::Sync
}
