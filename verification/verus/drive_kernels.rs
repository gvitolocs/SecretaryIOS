// The Drive's safety kernels (crates/lucina-media/src/drive_kernel_shared.rs; protocol 16 in
// docs/verification.md). The bodies are the SAME source the server compiles: this file includes
// them with their contracts, `drive_kernel.rs` includes them with the contracts erased.
//
//   DK1  segment_ok: a path segment the API and the catalog accept is never `.` or `..`, empty,
//        longer than 255 bytes, or carrying `/`, `\` or NUL; so it is a Normal segment, and
//        (lemma) a root joined with any sequence of Normal segments never climbs above the root,
//        whereas a single `..` can.
//   DK2  scope_of / in_scope: gallery, films and drive partition the library: every item is in
//        exactly one scope, and a phone backup is in the Gallery even inside a film album.
//   DK3  blob_action: an owned copy (store, spool) is never given up for an in-place reference,
//        an incoming owned copy is dropped only when the library owns the same bytes, and after
//        the action the bytes are owned iff they were, or the incoming copy was.
//   DK4  may_prune / may_adopt_fs: a scan trashes catalog rows only after a clean walk of an
//        existing folder on the filesystem its files were indexed from, that saw files (or the
//        catalog has none there); a filesystem is adopted only when the walk saw every known file.
//   DK5  drive_may_work: background Drive work never starts at or above the temperature limit
//        (Tctl 90 °C by default) or over the load limit.
// The protocol around DK3 and DK4 (scans racing unmounts, deletes, edits, a stray file in an
// empty mountpoint, a phone upload of the same bytes) is the Stateright model
// verification/models/src/drive_index.rs and its TLA+ twin formal/drive/DriveIndex.tla.
use vstd::prelude::*;
macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        verus! { $(pub fn $name($($arg: $ty),*) -> ($ret: $rty)
            requires $($pre)*
            ensures $($post)*
            { $($proof)* $($body)* })* }
    };
}
include!("../../crates/lucina-media/src/drive_kernel_shared.rs");

verus! {

/// DK2: the one scope of an item (0 gallery, 1 films, 2 drive).
pub open spec fn scope_of_spec(phone: bool, in_film_folder: bool) -> u8 {
    if phone {
        0
    } else if in_film_folder {
        1
    } else {
        2
    }
}

/// DK3: whether the library owns the bytes after `action` (0 place, 1 drop incoming, 2 upgrade,
/// 3 record in place, 4 keep).
pub open spec fn owned_after(existing: u8, action: u8) -> bool {
    action == 0 || action == 1 || action == 2 || (action == 4 && existing == 1)
}

/// DK2: exactly one scope holds for every item.
proof fn lemma_scopes_partition(phone: bool, film: bool)
    ensures
        scope_of_spec(phone, film) <= 2,
        (scope_of_spec(phone, film) == 0) == phone,
        (scope_of_spec(phone, film) == 1) == (!phone && film),
        (scope_of_spec(phone, film) == 2) == (!phone && !film),
{
}

// ---------------------------------------------------------------- DK1: paths stay under the root

pub enum Seg {
    Normal,
    Dot,
    DotDot,
}

/// What a segment means to the file system.
pub open spec fn seg_kind(is_dot: bool, is_dotdot: bool) -> Seg {
    if is_dotdot {
        Seg::DotDot
    } else if is_dot {
        Seg::Dot
    } else {
        Seg::Normal
    }
}

/// The depth after resolving one segment from depth `d` (`..` at `/` stays at `/`).
pub open spec fn step(d: int, s: Seg) -> int {
    match s {
        Seg::Normal => d + 1,
        Seg::Dot => d,
        Seg::DotDot => if d > 0 {
            d - 1
        } else {
            0
        },
    }
}

/// The depth after resolving `segs` from depth `d`.
pub open spec fn depth_after(d: int, segs: Seq<Seg>) -> int
    decreases segs.len(),
{
    if segs.len() == 0 {
        d
    } else {
        depth_after(step(d, segs[0]), segs.subrange(1, segs.len() as int))
    }
}

/// Resolving `segs` from depth `d` never goes above depth `root` (never leaves the folder).
pub open spec fn stays_under(d: int, root: int, segs: Seq<Seg>) -> bool
    decreases segs.len(),
{
    if segs.len() == 0 {
        d >= root
    } else {
        d >= root && stays_under(step(d, segs[0]), root, segs.subrange(1, segs.len() as int))
    }
}

/// A segment `segment_ok` accepts is a Normal segment.
proof fn lemma_accepted_segment_is_normal(len: u64, is_dot: bool, is_dotdot: bool, bad_byte: bool)
    requires
        len > 0 && len <= 255 && !is_dot && !is_dotdot && !bad_byte,
    ensures
        seg_kind(is_dot, is_dotdot) == Seg::Normal,
{
}

/// The folder root joined with Normal segments stays under the root, one level per segment.
proof fn lemma_normal_segments_stay_under(d: int, root: int, segs: Seq<Seg>)
    requires
        d >= root,
        forall|i: int| 0 <= i < segs.len() ==> segs[i] == Seg::Normal,
    ensures
        stays_under(d, root, segs),
        depth_after(d, segs) == d + segs.len(),
    decreases segs.len(),
{
    if segs.len() > 0 {
        let rest = segs.subrange(1, segs.len() as int);
        assert forall|i: int| 0 <= i < rest.len() implies rest[i] == Seg::Normal by {
            assert(rest[i] == segs[i + 1]);
        }
        lemma_normal_segments_stay_under(d + 1, root, rest);
    }
}

/// Every path `safe_rel` accepts (each segment accepted by `segment_ok`) stays under the root.
proof fn lemma_accepted_paths_stay_under(
    root: int,
    dots: Seq<bool>,
    dotdots: Seq<bool>,
    segs: Seq<Seg>,
)
    requires
        root >= 0,
        dots.len() == segs.len(),
        dotdots.len() == segs.len(),
        forall|i: int| 0 <= i < segs.len() ==> !dots[i] && !dotdots[i],
        forall|i: int| 0 <= i < segs.len() ==> segs[i] == seg_kind(dots[i], dotdots[i]),
    ensures
        stays_under(root, root, segs),
{
    assert forall|i: int| 0 <= i < segs.len() implies segs[i] == Seg::Normal by {}
    lemma_normal_segments_stay_under(root, root, segs);
}

/// The guard is needed: one `..` from the root leaves it.
proof fn lemma_dotdot_escapes(root: int)
    requires
        root > 0,
    ensures
        !stays_under(root, root, seq![Seg::DotDot]),
{
    let s = seq![Seg::DotDot];
    assert(s.len() == 1);
    assert(s[0] == Seg::DotDot);
    assert(s.subrange(1, 1) =~= Seq::<Seg>::empty());
    assert(step(root, Seg::DotDot) == root - 1);
    reveal_with_fuel(stays_under, 2);
}

// ---------------------------------------------------------------- DK3: dedupe never loses bytes

/// Applying blob_action to the incoming copies of a sequence of ingests: once owned, owned.
proof fn lemma_owned_is_sticky(existing: u8, incoming_owned: bool, action: u8)
    requires
        existing == 1,
        action == (if incoming_owned {
            1u8
        } else {
            4u8
        }),
    ensures
        owned_after(existing, action),
{
}

} // verus!

fn main() {
}
