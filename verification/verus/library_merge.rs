// Lucina Ink library sync and import kernel (crates/ink-sync, crates/ink-library).
//
// A simplified model of the pure functions, one book's registers (books are
// independent: every op names one book and touches only its registers):
//   Stamp / lt          <-> ink_sync::op::Stamp, derived Ord on (at_ms, device, counter);
//                           device ids are strings in the code, naturals here (an
//                           order-preserving encoding)
//   upd_progress        <-> state.rs effect() SetProgress + lww_progress (owner floor)
//   upd_status          <-> state.rs effect() SetStatus: owner `completed` freezes,
//                           owner floor, key (status == completed, stamp), pre-floor
//                           device values no longer count (`live`)
//   apply_raw / step    <-> apply_mut: the watermark gate, then the effect; clock + 1
//   Wm                  <-> ink_sync::watermark::Watermark (contiguous + sparse)
//   merge               <-> ink_library::import::merge: files (identity already
//                           resolved to a book id), clippings, then the kindle-usb
//                           presence pass
// Differential/property tests of the real code: crates/ink-sync/tests/sync.rs
// (permutations + duplicates converge, replays are no-ops, highlights only grow).
//
// Proven:
//   (a) step is idempotent per op id: a replayed op changes nothing; the raw
//       effect is idempotent too (a crash between sync.json and the records is
//       healed by re-applying).
//   (b) the highlight set (and the tombstone set) only grows, over any op sequence;
//       a delete request removes nothing.
//   (c) the stamp order is a strict total order on distinct op ids; step of two
//       ops with distinct ids commutes (progress, status, highlights, deletes),
//       so the state is independent of delivery order.
//   (d) the import merge never deletes a book, never removes a highlight and
//       never changes a reading status.
//   (e) the watermark: inserting records exactly the new counter, absorbing the
//       sparse set keeps the same membership.
use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------- (c) stamps

pub struct Stamp {
    pub at: int,
    pub dev: nat,
    pub ctr: nat,
}

pub open spec fn lt(a: Stamp, b: Stamp) -> bool {
    a.at < b.at || (a.at == b.at && a.dev < b.dev) || (a.at == b.at && a.dev == b.dev && a.ctr
        < b.ctr)
}

proof fn lemma_lt_strict_total_order(a: Stamp, b: Stamp, c: Stamp)
    ensures
        !lt(a, a),
        lt(a, b) ==> !lt(b, a),
        lt(a, b) && lt(b, c) ==> lt(a, c),
        a == b || lt(a, b) || lt(b, a),
{
}

/// Distinct op ids give distinct stamps, so last-writer-wins never ties.
proof fn lemma_distinct_ids_order(a: Op, b: Op)
    requires
        id(a) != id(b),
    ensures
        lt(stamp(a), stamp(b)) || lt(stamp(b), stamp(a)),
{
}

pub struct ExecStamp {
    pub at: i64,
    pub dev: u64,
    pub ctr: u64,
}

pub open spec fn sv(e: ExecStamp) -> Stamp {
    Stamp { at: e.at as int, dev: e.dev as nat, ctr: e.ctr as nat }
}

/// The comparison the code derives (`Ord` on the fields in order).
fn newer(a: &ExecStamp, b: &ExecStamp) -> (r: bool)
    ensures
        r == lt(sv(*b), sv(*a)),
{
    if a.at != b.at {
        a.at > b.at
    } else if a.dev != b.dev {
        a.dev > b.dev
    } else {
        a.ctr > b.ctr
    }
}

// ---------------------------------------------------------------- ops and state

pub open spec fn completed_status() -> nat {
    3
}

#[allow(inconsistent_fields)]
pub enum Kind {
    Progress { val: int },
    Status { val: nat },
    Highlight { key: nat },
    Delete { key: nat },
}

pub struct Op {
    pub dev: nat,
    pub ctr: nat,
    pub at: int,
    pub kind: Kind,
}

pub open spec fn stamp(op: Op) -> Stamp {
    Stamp { at: op.at, dev: op.dev, ctr: op.ctr }
}

pub open spec fn id(op: Op) -> (nat, nat) {
    (op.dev, op.ctr)
}

pub struct Reg {
    pub stamp: Stamp,
    pub val: int,
}

pub struct SReg {
    pub stamp: Stamp,
    pub val: nat,
}

/// One book's registers; floors and the freeze come from owner edits.
pub struct Book {
    pub progress: Option<Reg>,
    pub progress_floor: Option<int>,
    pub status: Option<SReg>,
    pub status_floor: Option<int>,
    pub owner_completed: bool,
}

pub struct State {
    pub applied: Set<(nat, nat)>,
    pub clock: nat,
    pub book: Book,
    pub highlights: Set<nat>,
    pub tombstones: Set<(nat, nat)>,
}

pub open spec fn below(floor: Option<int>, at: int) -> bool {
    match floor {
        Some(f) => at <= f,
        None => false,
    }
}

pub open spec fn upd_progress(p: Option<Reg>, floor: Option<int>, op: Op) -> Option<Reg> {
    match op.kind {
        Kind::Progress { val } => {
            if below(floor, op.at) {
                p
            } else {
                match p {
                    Some(r) => if lt(stamp(op), r.stamp) {
                        p
                    } else {
                        Some(Reg { stamp: stamp(op), val })
                    },
                    None => Some(Reg { stamp: stamp(op), val }),
                }
            }
        },
        _ => p,
    }
}

/// Status order: completed first, then the stamp.
pub open spec fn skey_lt(a: SReg, b: SReg) -> bool {
    (a.val != completed_status() && b.val == completed_status()) || ((a.val == completed_status()) == (b.val == completed_status())
        && lt(a.stamp, b.stamp))
}

/// A device value from before the owner's edit no longer counts.
pub open spec fn live(s: Option<SReg>, floor: Option<int>) -> Option<SReg> {
    match s {
        Some(r) => if below(floor, r.stamp.at) {
            None
        } else {
            s
        },
        None => None,
    }
}

pub open spec fn upd_status(s: Option<SReg>, floor: Option<int>, frozen: bool, op: Op) -> Option<
    SReg,
> {
    match op.kind {
        Kind::Status { val } => {
            if frozen || below(floor, op.at) {
                s
            } else {
                let new = SReg { stamp: stamp(op), val };
                match live(s, floor) {
                    Some(cur) => if skey_lt(new, cur) {
                        s
                    } else {
                        Some(new)
                    },
                    None => Some(new),
                }
            }
        },
        _ => s,
    }
}

pub open spec fn upd_highlights(h: Set<nat>, op: Op) -> Set<nat> {
    match op.kind {
        Kind::Highlight { key } => h.insert(key),
        _ => h,
    }
}

pub open spec fn upd_tombstones(t: Set<(nat, nat)>, op: Op) -> Set<(nat, nat)> {
    match op.kind {
        Kind::Delete { key: _ } => t.insert(id(op)),
        _ => t,
    }
}

pub open spec fn apply_raw(s: State, op: Op) -> State {
    State {
        applied: s.applied,
        clock: s.clock,
        book: Book {
            progress: upd_progress(s.book.progress, s.book.progress_floor, op),
            progress_floor: s.book.progress_floor,
            status: upd_status(s.book.status, s.book.status_floor, s.book.owner_completed, op),
            status_floor: s.book.status_floor,
            owner_completed: s.book.owner_completed,
        },
        highlights: upd_highlights(s.highlights, op),
        tombstones: upd_tombstones(s.tombstones, op),
    }
}

/// apply_mut: the op-id gate, then the effect exactly once.
pub open spec fn step(s: State, op: Op) -> State {
    if s.applied.contains(id(op)) {
        s
    } else {
        let r = apply_raw(s, op);
        State {
            applied: r.applied.insert(id(op)),
            clock: r.clock + 1,
            book: r.book,
            highlights: r.highlights,
            tombstones: r.tombstones,
        }
    }
}

pub open spec fn run(s: State, ops: Seq<Op>) -> State
    decreases ops.len(),
{
    if ops.len() == 0 {
        s
    } else {
        run(step(s, ops[0]), ops.drop_first())
    }
}

// ---------------------------------------------------------------- (a) idempotency

proof fn lemma_replay_is_noop(s: State, op: Op)
    requires
        s.applied.contains(id(op)),
    ensures
        step(s, op) == s,
{
}

proof fn lemma_step_idempotent(s: State, op: Op)
    ensures
        step(step(s, op), op) == step(s, op),
        step(s, op).applied.contains(id(op)),
{
}

/// The effect alone is idempotent too.
proof fn lemma_apply_raw_idempotent(s: State, op: Op)
    ensures
        apply_raw(apply_raw(s, op), op) =~~= apply_raw(s, op),
{
    let s1 = apply_raw(s, op);
    match op.kind {
        Kind::Highlight { key } => {
            assert(s1.highlights.insert(key) =~= s1.highlights);
        },
        Kind::Delete { key: _ } => {
            assert(s1.tombstones.insert(id(op)) =~= s1.tombstones);
        },
        _ => {},
    }
}

/// Every op is applied at most once: the clock counts distinct op ids.
proof fn lemma_clock_counts_ids(s: State, ops: Seq<Op>)
    requires
        s.clock == s.applied.len(),
    ensures
        run(s, ops).clock == run(s, ops).applied.len(),
    decreases ops.len(),
{
    if ops.len() > 0 {
        let s1 = step(s, ops[0]);
        if !s.applied.contains(id(ops[0])) {
            assert(s1.applied.len() == s.applied.len() + 1);
        }
        lemma_clock_counts_ids(s1, ops.drop_first());
    }
}

// ---------------------------------------------------------------- (b) grow-only

proof fn lemma_step_grows(s: State, op: Op)
    ensures
        s.highlights.subset_of(step(s, op).highlights),
        s.tombstones.subset_of(step(s, op).tombstones),
        s.applied.subset_of(step(s, op).applied),
        // a fresh highlight op is visible; a delete request removes nothing
        (!s.applied.contains(id(op)) && op.kind is Highlight) ==> step(s, op).highlights.contains(
            op.kind->Highlight_key,
        ),
        op.kind is Delete ==> step(s, op).highlights == s.highlights,
{
}

proof fn lemma_run_grows(s: State, ops: Seq<Op>)
    ensures
        s.highlights.subset_of(run(s, ops).highlights),
        s.tombstones.subset_of(run(s, ops).tombstones),
        s.applied.subset_of(run(s, ops).applied),
    decreases ops.len(),
{
    if ops.len() > 0 {
        let s1 = step(s, ops[0]);
        lemma_step_grows(s, ops[0]);
        lemma_run_grows(s1, ops.drop_first());
    }
}

// ---------------------------------------------------------------- (c) commutation

proof fn lemma_progress_commutes(p: Option<Reg>, floor: Option<int>, a: Op, b: Op)
    requires
        id(a) != id(b),
    ensures
        upd_progress(upd_progress(p, floor, a), floor, b) == upd_progress(
            upd_progress(p, floor, b),
            floor,
            a,
        ),
{
    lemma_distinct_ids_order(a, b);
}

proof fn lemma_status_commutes(s: Option<SReg>, floor: Option<int>, frozen: bool, a: Op, b: Op)
    requires
        id(a) != id(b),
    ensures
        upd_status(upd_status(s, floor, frozen, a), floor, frozen, b) == upd_status(
            upd_status(s, floor, frozen, b),
            floor,
            frozen,
            a,
        ),
{
    lemma_distinct_ids_order(a, b);
}

/// The diamond: two ops with distinct ids reach the same state in either order.
proof fn lemma_step_commutes(s: State, a: Op, b: Op)
    requires
        id(a) != id(b),
    ensures
        step(step(s, a), b) =~~= step(step(s, b), a),
{
    let fl = s.book.progress_floor;
    let sf = s.book.status_floor;
    let fr = s.book.owner_completed;
    lemma_progress_commutes(s.book.progress, fl, a, b);
    lemma_status_commutes(s.book.status, sf, fr, a, b);
    let ab = step(step(s, a), b);
    let ba = step(step(s, b), a);
    let ia = s.applied.contains(id(a));
    let ib = s.applied.contains(id(b));
    if !ia && !ib {
        assert(ab.applied =~= ba.applied);
        match (a.kind, b.kind) {
            (Kind::Highlight { key: x }, Kind::Highlight { key: y }) => {
                assert(s.highlights.insert(x).insert(y) =~= s.highlights.insert(y).insert(x));
            },
            (Kind::Delete { key: _ }, Kind::Delete { key: _ }) => {
                assert(s.tombstones.insert(id(a)).insert(id(b)) =~= s.tombstones.insert(
                    id(b),
                ).insert(id(a)));
            },
            _ => {},
        }
        assert(ab.highlights =~= ba.highlights);
        assert(ab.tombstones =~= ba.tombstones);
    }
}

/// Convergence for two-op schedules with duplicates: any interleaving of a,
/// b and a replayed a gives the same state as a then b.
proof fn lemma_two_ops_with_replay(s: State, a: Op, b: Op)
    requires
        id(a) != id(b),
    ensures
        run(s, seq![b, a, a]) =~~= run(s, seq![a, b]),
        run(s, seq![a, b, a]) =~~= run(s, seq![a, b]),
{
    reveal_with_fuel(run, 4);
    lemma_step_commutes(s, a, b);
    lemma_step_idempotent(step(s, b), a);
    lemma_replay_is_noop(step(step(s, a), b), a);
    assert(seq![b, a, a].drop_first() =~= seq![a, a]);
    assert(seq![a, a].drop_first() =~= seq![a]);
    assert(seq![a].drop_first() =~= Seq::<Op>::empty());
    assert(seq![a, b, a].drop_first() =~= seq![b, a]);
    assert(seq![b, a].drop_first() =~= seq![a]);
    assert(seq![a, b].drop_first() =~= seq![b]);
    assert(seq![b].drop_first() =~= Seq::<Op>::empty());
    let ab = step(step(s, a), b);
    assert(run(s, seq![a, b]) == ab);
    assert(run(s, seq![b, a, a]) == step(step(step(s, b), a), a));
    assert(run(s, seq![a, b, a]) == step(ab, a));
}

/// The owner protection: with the owner's `completed`, no device op changes
/// the status register.
proof fn lemma_owner_completed_frozen(s: State, ops: Seq<Op>)
    requires
        s.book.owner_completed,
    ensures
        run(s, ops).book.status == s.book.status,
        run(s, ops).book.owner_completed,
    decreases ops.len(),
{
    if ops.len() > 0 {
        lemma_owner_completed_frozen(step(s, ops[0]), ops.drop_first());
    }
}

/// Device `completed` is sticky: once the live status is completed, device ops
/// keep it completed.
proof fn lemma_device_completed_sticky(s: State, op: Op)
    requires
        live(s.book.status, s.book.status_floor) is Some,
        s.book.status->Some_0.val == completed_status(),
    ensures
        step(s, op).book.status is Some,
        step(s, op).book.status->Some_0.val == completed_status(),
{
}

// ---------------------------------------------------------------- (d) import merge

pub struct KBook {
    pub owned: bool,
    pub on_kindle: bool,
    pub status: nat,
    pub status_source: nat,
    pub highlights: Set<nat>,
}

pub struct Clip {
    pub book: nat,
    pub key: nat,
}

pub struct Batch {
    /// Scanned files, identity (fingerprint / ISBN / ASIN / title match)
    /// already resolved to a book id.
    pub files: Seq<nat>,
    pub clips: Seq<Clip>,
    pub usb: bool,
}

pub open spec fn new_book() -> KBook {
    KBook { owned: false, on_kindle: false, status: 0, status_source: 0, highlights: Set::empty() }
}

pub open spec fn get_or_new(m: Map<nat, KBook>, k: nat) -> KBook {
    if m.dom().contains(k) {
        m[k]
    } else {
        new_book()
    }
}

pub open spec fn merge_files(m: Map<nat, KBook>, files: Seq<nat>) -> Map<nat, KBook>
    decreases files.len(),
{
    if files.len() == 0 {
        m
    } else {
        let k = files[0];
        let b = get_or_new(m, k);
        merge_files(
            m.insert(
                k,
                KBook {
                    owned: true,
                    on_kindle: true,
                    status: b.status,
                    status_source: b.status_source,
                    highlights: b.highlights,
                },
            ),
            files.drop_first(),
        )
    }
}

pub open spec fn merge_clips(m: Map<nat, KBook>, clips: Seq<Clip>) -> Map<nat, KBook>
    decreases clips.len(),
{
    if clips.len() == 0 {
        m
    } else {
        let c = clips[0];
        let b = get_or_new(m, c.book);
        merge_clips(
            m.insert(
                c.book,
                KBook {
                    owned: b.owned,
                    on_kindle: b.on_kindle,
                    status: b.status,
                    status_source: b.status_source,
                    highlights: b.highlights.insert(c.key),
                },
            ),
            clips.drop_first(),
        )
    }
}

/// A complete kindle-usb inventory clears the presence of missing books.
pub open spec fn mark_removed(m: Map<nat, KBook>, seen: Set<nat>, usb: bool) -> Map<nat, KBook> {
    Map::new(
        m.dom(),
        |k: nat|
            if usb && !seen.contains(k) {
                KBook {
                    owned: m[k].owned,
                    on_kindle: false,
                    status: m[k].status,
                    status_source: m[k].status_source,
                    highlights: m[k].highlights,
                }
            } else {
                m[k]
            },
    )
}

pub open spec fn merge(m: Map<nat, KBook>, batch: Batch) -> Map<nat, KBook> {
    mark_removed(merge_clips(merge_files(m, batch.files), batch.clips), batch.files.to_set(), batch.usb)
}

/// Kept: present, same status, at least the same highlights.
pub open spec fn kept(old: Map<nat, KBook>, new: Map<nat, KBook>) -> bool {
    forall|k: nat|
        #![trigger old.dom().contains(k)]
        old.dom().contains(k) ==> new.dom().contains(k) && new[k].status == old[k].status
            && new[k].status_source == old[k].status_source && old[k].highlights.subset_of(
            new[k].highlights,
        )
}

proof fn lemma_kept_transitive(a: Map<nat, KBook>, b: Map<nat, KBook>, c: Map<nat, KBook>)
    requires
        kept(a, b),
        kept(b, c),
    ensures
        kept(a, c),
{
    assert forall|k: nat| #![trigger a.dom().contains(k)] a.dom().contains(k) implies c.dom().contains(k)
        && c[k].status == a[k].status && c[k].status_source == a[k].status_source
        && a[k].highlights.subset_of(c[k].highlights) by {
        assert(b.dom().contains(k));
        assert(b[k].status == a[k].status);
        assert(c[k].status == b[k].status);
    }
}

proof fn lemma_merge_files_keeps(m: Map<nat, KBook>, files: Seq<nat>)
    ensures
        kept(m, merge_files(m, files)),
    decreases files.len(),
{
    if files.len() > 0 {
        let k = files[0];
        let b = get_or_new(m, k);
        let m1 = m.insert(
            k,
            KBook {
                owned: true,
                on_kindle: true,
                status: b.status,
                status_source: b.status_source,
                highlights: b.highlights,
            },
        );
        assert(kept(m, m1));
        lemma_merge_files_keeps(m1, files.drop_first());
        lemma_kept_transitive(m, m1, merge_files(m, files));
    }
}

proof fn lemma_merge_clips_keeps(m: Map<nat, KBook>, clips: Seq<Clip>)
    ensures
        kept(m, merge_clips(m, clips)),
    decreases clips.len(),
{
    if clips.len() > 0 {
        let c = clips[0];
        let b = get_or_new(m, c.book);
        let m1 = m.insert(
            c.book,
            KBook {
                owned: b.owned,
                on_kindle: b.on_kindle,
                status: b.status,
                status_source: b.status_source,
                highlights: b.highlights.insert(c.key),
            },
        );
        assert(kept(m, m1));
        lemma_merge_clips_keeps(m1, clips.drop_first());
        lemma_kept_transitive(m, m1, merge_clips(m, clips));
    }
}

/// (d) The import merge never deletes a book or a highlight and never changes a
/// reading status (owner edits survive every import).
proof fn lemma_merge_never_deletes(m: Map<nat, KBook>, batch: Batch)
    ensures
        kept(m, merge(m, batch)),
        m.dom().subset_of(merge(m, batch).dom()),
{
    let m1 = merge_files(m, batch.files);
    let m2 = merge_clips(m1, batch.clips);
    let m3 = merge(m, batch);
    lemma_merge_files_keeps(m, batch.files);
    lemma_merge_clips_keeps(m1, batch.clips);
    lemma_kept_transitive(m, m1, m2);
    assert(kept(m2, m3));
    lemma_kept_transitive(m, m2, m3);
}

// ---------------------------------------------------------------- (e) watermark

pub struct Wm {
    pub contig: nat,
    pub sparse: Set<nat>,
}

/// Counters start at 1.
pub open spec fn wm_contains(w: Wm, c: nat) -> bool {
    (1 <= c && c <= w.contig) || w.sparse.contains(c)
}

pub open spec fn wm_insert(w: Wm, c: nat) -> Wm {
    if wm_contains(w, c) {
        w
    } else if c == w.contig + 1 {
        Wm { contig: c, sparse: w.sparse.remove(c) }
    } else {
        Wm { contig: w.contig, sparse: w.sparse.insert(c) }
    }
}

/// One iteration of the absorb loop (`while sparse.remove(contiguous + 1)`).
pub open spec fn absorb(w: Wm) -> Wm {
    if w.sparse.contains(w.contig + 1) {
        Wm { contig: w.contig + 1, sparse: w.sparse.remove(w.contig + 1) }
    } else {
        w
    }
}

proof fn lemma_wm_insert(w: Wm, c: nat, x: nat)
    requires
        c >= 1,
        x >= 1,
    ensures
        wm_contains(wm_insert(w, c), x) == (wm_contains(w, x) || x == c),
{
}

proof fn lemma_absorb_preserves(w: Wm, x: nat)
    requires
        x >= 1,
    ensures
        wm_contains(absorb(w), x) == wm_contains(w, x),
{
}

fn main() {
}

} // verus!
