// OTP lifetime: `expires_at`, the relay store (receive / take / arm / disarm) and the
// phone's offline queue.
//
// Production (branch lucina-rs dd7ec9e, with F1 ac9e7be and F2 60472d9 from otp-fixes; before them c357646):
//   expires_at          <-> crates/lucina-phone-core/src/proto.rs `expires_at`, CODE_TTL_MS
//   Store / Core        <-> crates/lucina-otp-relay/src/store.rs `Store`
//                           (continuous, paused, limits, window, pending, seen)
//     receive           <-> `Store::receive` (validate, test, live window, windowId, prune,
//                           seen-id duplicate, expires_at, EventLimit, push seen, drop oldest,
//                           push pending)
//     take              <-> `Store::take` (not armed: drop all; else pop_back until fresh)
//     arm / disarm      <-> `Store::arm` (F2: keeps fresh pending codes and a live
//                           window's id), `Store::disarm`
//   retain_fresh / enqueue / flush
//                       <-> crates/lucina-android/src/bridge.rs `Pending::fresh`,
//                           `Bridge::enqueue`, `Bridge::flush_at`, the TTL gate in `forward`
//   forward_queue       <-> `Bridge::forward` after F1: write-ahead enqueue, then the POST;
//                           any answer but Retry removes the entry by event id
// Differential tests: verification/verus/diff/otp_expiry.rs (relay store, expires_at) and
// verification/verus/diff/otp_queue.rs (phone queue).
//
// Modelled: every field the decisions read, with u64 times and saturating adds as in the
// code. Abstracted: event ids, codes and window ids are u64 (the code compares them for
// equality only; F3 stores SHA-256 hashes of event ids, which keeps equality);
// `Incoming::validate` is the `valid` flag; F3's `restore_seen` after a restart is not
// modelled; sender cleaning and `source`
// are not modelled (they never affect what is stored or returned). The phone's network
// outcome is an oracle (`retry[i]`: the i-th POST of a flush answers Retry). Entry's
// `stored_at` is model-only (the relay's clock when the entry was stored).
// The history proofs (R5, R6) assume the relay clock never goes back (`now >= clock`).
//
// Proven:
//   E1 expires_at: Ok(e) ==> now < e <= now + TTL, e <= window_end, and
//      e <= received_at + TTL when received_at <= now; Err exactly when the
//      capped expiry is not after now.
//   R1 take never returns a code at or after its expires_at.
//   R2 take consumes: the returned entry's event id is no longer pending, and any later
//      delivery of the same event id was stored at or after the first one expired
//      (inv I6), i.e. a code is handed out at most once in its lifetime.
//   R3 pending.len() <= limits.pending (8) and seen.len() <= limits.seen (64), always.
//   R4 pending event ids are distinct; a receive whose id is in `seen` with
//      until > now (within SEEN_TTL = CODE_TTL of its first receipt) never stores.
//   R5 inv is inductive over receive / take / arm / disarm from the empty store.
//   R7 (F2) arming never drops a pending code that is still fresh at `now`.
//   P1 phone: forward never sends or queues a code past the TTL; enqueue keeps at most
//      MAX_QUEUE (8) entries, all fresh; flush only POSTs fresh entries and keeps only
//      fresh ones; F1's forward_queue keeps the code queued (last) unless the relay
//      answered, and then no entry with its event id is left.
//   Non-vacuity: witness_* run receive then take and get the code back, then None.
use vstd::prelude::*;

verus! {

pub const TTL: u64 = 120_000;
pub const SEEN_TTL: u64 = 120_000;
pub const U64MAX: u64 = 0xffff_ffff_ffff_ffff;
pub const MIN_ARM: u64 = 30;
pub const MAX_ARM: u64 = 300;
pub const MAX_QUEUE: usize = 8;

pub open spec fn sat(x: int) -> int {
    if x > U64MAX as int { U64MAX as int } else { x }
}

pub open spec fn min(a: int, b: int) -> int {
    if a <= b { a } else { b }
}

// ======================================================================== E1

pub open spec fn base(received_at: Option<u64>, now: u64) -> int {
    match received_at {
        Some(t) => min(t as int, now as int),
        None => now as int,
    }
}

pub open spec fn capped(received_at: Option<u64>, now: u64, window_end: u64) -> int {
    min(sat(base(received_at, now) + TTL), window_end as int)
}

pub open spec fn expires_spec(received_at: Option<u64>, now: u64, window_end: u64) -> Option<u64> {
    if capped(received_at, now, window_end) <= now as int {
        None
    } else {
        Some(capped(received_at, now, window_end) as u64)
    }
}

fn sat_add(a: u64, b: u64) -> (r: u64)
    ensures
        r as int == sat(a + b),
{
    if a > U64MAX - b { U64MAX } else { a + b }
}

/// `proto::expires_at` (Err(Expired) is None).
pub fn expires_at(received_at: Option<u64>, now: u64, window_end: u64) -> (r: Option<u64>)
    ensures
        r == expires_spec(received_at, now, window_end),
        r is Some ==> now < r->Some_0,
        r is Some ==> r->Some_0 <= now + TTL,
        r is Some ==> r->Some_0 <= window_end,
        r is Some && received_at is Some && received_at->Some_0 <= now ==> r->Some_0
            <= received_at->Some_0 + TTL,
        r is None <==> capped(received_at, now, window_end) <= now,
{
    let b = match received_at {
        Some(t) => if t <= now { t } else { now },
        None => now,
    };
    let e = sat_add(b, TTL);
    let e = if e <= window_end { e } else { window_end };
    if e <= now {
        None
    } else {
        Some(e)
    }
}

// ======================================================================== relay model

#[derive(Clone, Copy)]
pub struct Entry {
    pub id: u64,
    pub code: u64,
    pub received_at: u64,
    pub expires: u64,
    /// Model only: the relay clock when it was stored.
    pub stored_at: u64,
}

#[derive(Clone, Copy)]
pub struct Seen {
    pub id: u64,
    pub until: u64,
}

#[derive(Clone, Copy)]
pub struct Win {
    pub id: u64,
    pub end: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct Event {
    pub id: u64,
    pub code: u64,
    pub received_at: Option<u64>,
    pub window_id: Option<u64>,
    pub test: bool,
    /// `Incoming::validate` passed.
    pub valid: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    Invalid,
    TestAck,
    Paused,
    WrongWindow,
    Duplicate,
    Expired,
    EventLimit,
    Stored,
}

/// The real `Store`'s fields.
pub struct Core {
    pub continuous: bool,
    pub paused: bool,
    pub lim_pending: nat,
    pub lim_seen: nat,
    pub window: Option<Win>,
    pub pending: Seq<Entry>,
    pub seen: Seq<Seen>,
}

pub open spec fn live(w: Win, now: u64) -> bool {
    match w.end {
        None => true,
        Some(end) => now < end,
    }
}

pub open spec fn armed(c: Core, now: u64) -> bool {
    match c.window {
        None => false,
        Some(w) => live(w, now),
    }
}

pub open spec fn prune_p(p: Seq<Entry>, now: u64) -> Seq<Entry>
    decreases p.len(),
{
    if p.len() == 0 {
        p
    } else {
        let rest = prune_p(p.drop_last(), now);
        if p.last().expires > now { rest.push(p.last()) } else { rest }
    }
}

pub open spec fn prune_s(s: Seq<Seen>, now: u64) -> Seq<Seen>
    decreases s.len(),
{
    if s.len() == 0 {
        s
    } else {
        let rest = prune_s(s.drop_last(), now);
        if s.last().until > now { rest.push(s.last()) } else { rest }
    }
}

pub open spec fn seen_has(s: Seq<Seen>, id: u64) -> bool {
    exists|j: int| 0 <= j < s.len() && (#[trigger] s[j]).id == id
}

pub open spec fn receive_core(c: Core, ev: Event, now: u64) -> (Core, Outcome) {
    if !ev.valid {
        (c, Outcome::Invalid)
    } else if ev.test {
        (c, Outcome::TestAck)
    } else if !armed(c, now) {
        (c, Outcome::Paused)
    } else if ev.window_id is Some && ev.window_id->Some_0 != c.window->Some_0.id {
        (c, Outcome::WrongWindow)
    } else {
        let wend = match c.window->Some_0.end {
            Some(e) => e,
            None => U64MAX,
        };
        let c1 = Core { pending: prune_p(c.pending, now), seen: prune_s(c.seen, now), ..c };
        if seen_has(c1.seen, ev.id) {
            (c1, Outcome::Duplicate)
        } else {
            match expires_spec(ev.received_at, now, wend) {
                None => (c1, Outcome::Expired),
                Some(e) => if c1.seen.len() >= c1.lim_seen {
                    (c1, Outcome::EventLimit)
                } else {
                    let p1 = if c1.pending.len() >= c1.lim_pending && c1.pending.len() > 0 {
                        c1.pending.subrange(1, c1.pending.len() as int)
                    } else {
                        c1.pending
                    };
                    let entry = Entry {
                        id: ev.id,
                        code: ev.code,
                        received_at: base(ev.received_at, now) as u64,
                        expires: e,
                        stored_at: now,
                    };
                    (
                        Core {
                            pending: p1.push(entry),
                            seen: c1.seen.push(Seen { id: ev.id, until: sat(now + SEEN_TTL) as u64 }),
                            ..c1
                        },
                        Outcome::Stored,
                    )
                },
            }
        }
    }
}

/// Index of the newest entry that is still fresh (-1: none): what `pop_back` stops at.
pub open spec fn take_idx(p: Seq<Entry>, now: u64) -> int
    decreases p.len(),
{
    if p.len() == 0 {
        -1
    } else if p.last().expires > now {
        p.len() - 1
    } else {
        take_idx(p.drop_last(), now)
    }
}

pub open spec fn take_core(c: Core, now: u64) -> (Core, Option<Entry>) {
    if !armed(c, now) {
        (Core { window: None, pending: Seq::empty(), ..c }, None)
    } else {
        let k = take_idx(c.pending, now);
        if k < 0 {
            (Core { pending: Seq::empty(), ..c }, None)
        } else {
            (Core { pending: c.pending.subrange(0, k), ..c }, Some(c.pending[k]))
        }
    }
}

pub open spec fn clamp_arm(seconds: u64) -> int {
    if seconds < MIN_ARM { MIN_ARM as int } else if seconds > MAX_ARM { MAX_ARM as int } else { seconds as int }
}

/// F2 (60472d9): arming keeps waiting codes (only expired ones go) and a live
/// window keeps its id.
pub open spec fn arm_core(c: Core, seconds: u64, now: u64, wid: u64) -> Core {
    let id = if armed(c, now) { c.window->Some_0.id } else { wid };
    if c.continuous {
        Core { pending: prune_p(c.pending, now), paused: false, window: Some(Win { id, end: None }), ..c }
    } else {
        Core {
            pending: prune_p(c.pending, now),
            window: Some(Win { id, end: Some(sat(now + clamp_arm(seconds) * 1000) as u64) }),
            ..c
        }
    }
}

pub open spec fn disarm_core(c: Core) -> Core {
    Core { window: None, pending: Seq::empty(), paused: if c.continuous { true } else { c.paused }, ..c }
}

// ------------------------------------------------------------ lemmas on prune / take

pub open spec fn distinct(p: Seq<Entry>) -> bool {
    forall|i: int, j: int| 0 <= i < j < p.len() ==> (#[trigger] p[i]).id != (#[trigger] p[j]).id
}

proof fn lemma_prune_p(p: Seq<Entry>, now: u64)
    ensures
        prune_p(p, now).len() <= p.len(),
        forall|x: Entry| #[trigger] prune_p(p, now).contains(x) <==> (p.contains(x) && x.expires > now),
        distinct(p) ==> distinct(prune_p(p, now)),
    decreases p.len(),
{
    if p.len() > 0 {
        let d = p.drop_last();
        lemma_prune_p(d, now);
        let rest = prune_p(d, now);
        assert forall|x: Entry| p.contains(x) <==> (d.contains(x) || x == p.last()) by {
            if p.contains(x) {
                let i = choose|i: int| 0 <= i < p.len() && p[i] == x;
                if i < p.len() - 1 {
                    assert(d[i] == x);
                }
            }
            if d.contains(x) {
                let i = choose|i: int| 0 <= i < d.len() && d[i] == x;
                assert(p[i] == x);
            }
            if x == p.last() {
                assert(p[p.len() - 1] == x);
            }
        }
        if p.last().expires > now {
            let r = rest.push(p.last());
            assert forall|x: Entry| r.contains(x) <==> (rest.contains(x) || x == p.last()) by {
                if r.contains(x) {
                    let i = choose|i: int| 0 <= i < r.len() && r[i] == x;
                    if i < rest.len() {
                        assert(rest[i] == x);
                    }
                }
                if rest.contains(x) {
                    let i = choose|i: int| 0 <= i < rest.len() && rest[i] == x;
                    assert(r[i] == x);
                }
                if x == p.last() {
                    assert(r[r.len() - 1] == x);
                }
            }
            if distinct(p) {
                assert(distinct(d)) by {
                    assert forall|i: int, j: int| 0 <= i < j < d.len() implies (#[trigger] d[i]).id != (#[trigger] d[j]).id by {
                        assert(d[i] == p[i] && d[j] == p[j]);
                    }
                }
                assert forall|i: int, j: int| 0 <= i < j < r.len() implies (#[trigger] r[i]).id != (#[trigger] r[j]).id by {
                    if j == r.len() - 1 {
                        assert(rest.contains(r[i]));
                        assert(d.contains(r[i]));
                        let k = choose|k: int| 0 <= k < d.len() && d[k] == r[i];
                        assert(p[k] == r[i]);
                        assert(p[p.len() - 1] == r[j]);
                    } else {
                        assert(r[i] == rest[i] && r[j] == rest[j]);
                    }
                }
            }
        } else {
            if distinct(p) {
                assert forall|i: int, j: int| 0 <= i < j < d.len() implies (#[trigger] d[i]).id != (#[trigger] d[j]).id by {
                    assert(d[i] == p[i] && d[j] == p[j]);
                }
            }
        }
    }
}

proof fn lemma_prune_s(s: Seq<Seen>, now: u64)
    ensures
        prune_s(s, now).len() <= s.len(),
        forall|x: Seen| #[trigger] prune_s(s, now).contains(x) <==> (s.contains(x) && x.until > now),
    decreases s.len(),
{
    if s.len() > 0 {
        let d = s.drop_last();
        lemma_prune_s(d, now);
        let rest = prune_s(d, now);
        assert forall|x: Seen| s.contains(x) <==> (d.contains(x) || x == s.last()) by {
            if s.contains(x) {
                let i = choose|i: int| 0 <= i < s.len() && s[i] == x;
                if i < s.len() - 1 {
                    assert(d[i] == x);
                }
            }
            if d.contains(x) {
                let i = choose|i: int| 0 <= i < d.len() && d[i] == x;
                assert(s[i] == x);
            }
            if x == s.last() {
                assert(s[s.len() - 1] == x);
            }
        }
        if s.last().until > now {
            let r = rest.push(s.last());
            assert forall|x: Seen| r.contains(x) <==> (rest.contains(x) || x == s.last()) by {
                if r.contains(x) {
                    let i = choose|i: int| 0 <= i < r.len() && r[i] == x;
                    if i < rest.len() {
                        assert(rest[i] == x);
                    }
                }
                if rest.contains(x) {
                    let i = choose|i: int| 0 <= i < rest.len() && rest[i] == x;
                    assert(r[i] == x);
                }
                if x == s.last() {
                    assert(r[r.len() - 1] == x);
                }
            }
        }
    }
}

proof fn lemma_take_idx(p: Seq<Entry>, now: u64)
    ensures
        -1 <= take_idx(p, now) < p.len(),
        take_idx(p, now) >= 0 ==> p[take_idx(p, now)].expires > now,
        forall|j: int| take_idx(p, now) < j < p.len() ==> (#[trigger] p[j]).expires <= now,
    decreases p.len(),
{
    if p.len() > 0 && p.last().expires <= now {
        let d = p.drop_last();
        lemma_take_idx(d, now);
        assert forall|j: int| take_idx(p, now) < j < p.len() implies (#[trigger] p[j]).expires <= now by {
            if j < p.len() - 1 {
                assert(p[j] == d[j]);
            }
        }
    }
}

// ------------------------------------------------------------ the invariant

/// Relay state plus the model's history: the clock and every code handed out.
pub struct Relay {
    pub c: Core,
    pub clock: int,
    pub taken: Seq<Entry>,
}

pub open spec fn covered(s: Seq<Seen>, id: u64, t: int) -> bool {
    exists|j: int| 0 <= j < s.len() && (#[trigger] s[j]).id == id && s[j].until >= t
}

pub open spec fn inv(r: Relay) -> bool {
    &&& r.c.lim_pending >= 1
    &&& r.c.pending.len() <= r.c.lim_pending
    &&& r.c.seen.len() <= r.c.lim_seen
    // I3: every waiting code's event id is remembered at least as long as the code lives.
    &&& forall|i: int| 0 <= i < r.c.pending.len() ==> covered(r.c.seen, (#[trigger] r.c.pending[i]).id, r.c.pending[i].expires as int)
    // I4
    &&& distinct(r.c.pending)
    // I5: a delivered code's id is remembered until the code's expiry, or that expiry is past.
    &&& forall|i: int| 0 <= i < r.taken.len() ==> covered(r.c.seen, (#[trigger] r.taken[i]).id, r.taken[i].expires as int) || r.clock >= r.taken[i].expires
    // I6: a pending entry with a delivered code's id was stored after that code died.
    &&& forall|i: int, k: int| 0 <= i < r.taken.len() && 0 <= k < r.c.pending.len() && (#[trigger] r.taken[i]).id == (#[trigger] r.c.pending[k]).id ==> r.c.pending[k].stored_at >= r.taken[i].expires
}

pub open spec fn init(lim_pending: nat, lim_seen: nat, continuous: bool, paused: bool) -> Relay {
    Relay {
        c: Core { continuous, paused, lim_pending, lim_seen, window: None, pending: Seq::empty(), seen: Seq::empty() },
        clock: 0,
        taken: Seq::empty(),
    }
}

pub open spec fn step_receive(r: Relay, ev: Event, now: u64) -> Relay {
    Relay { c: receive_core(r.c, ev, now).0, clock: now as int, taken: r.taken }
}

pub open spec fn step_take(r: Relay, now: u64) -> Relay {
    let (c2, d) = take_core(r.c, now);
    Relay { c: c2, clock: now as int, taken: if d is Some { r.taken.push(d->Some_0) } else { r.taken } }
}

pub open spec fn step_arm(r: Relay, seconds: u64, now: u64, wid: u64) -> Relay {
    Relay { c: arm_core(r.c, seconds, now, wid), clock: now as int, taken: r.taken }
}

pub open spec fn step_disarm(r: Relay) -> Relay {
    Relay { c: disarm_core(r.c), ..r }
}

proof fn lemma_init(lim_pending: nat, lim_seen: nat, continuous: bool, paused: bool)
    requires
        lim_pending >= 1,
    ensures
        inv(init(lim_pending, lim_seen, continuous, paused)),
{
}

/// A store with no pending codes and the same seen ids keeps the invariant at a later clock.
proof fn lemma_clear_pending(r: Relay, r2: Relay)
    requires
        inv(r),
        r2.c.pending.len() == 0,
        r2.c.seen == r.c.seen,
        r2.c.lim_pending == r.c.lim_pending,
        r2.c.lim_seen == r.c.lim_seen,
        r2.taken == r.taken,
        r2.clock >= r.clock,
    ensures
        inv(r2),
{
    assert forall|i: int| 0 <= i < r2.taken.len() implies covered(r2.c.seen, (#[trigger] r2.taken[i]).id, r2.taken[i].expires as int) || r2.clock >= r2.taken[i].expires by {
        assert(covered(r.c.seen, r.taken[i].id, r.taken[i].expires as int) || r.clock >= r.taken[i].expires);
    }
}

proof fn lemma_receive_inv(r: Relay, ev: Event, now: u64)
    requires
        inv(r),
        now as int >= r.clock,
    ensures
        inv(step_receive(r, ev, now)),
        // R4: an id remembered past now is never stored again.
        seen_has(prune_s(r.c.seen, now), ev.id) ==> receive_core(r.c, ev, now).1 != Outcome::Stored,
        receive_core(r.c, ev, now).1 == Outcome::Stored ==> forall|j: int| 0 <= j < r.c.seen.len() && (#[trigger] r.c.seen[j]).id == ev.id ==> r.c.seen[j].until <= now,
{
    let c = r.c;
    let r2 = step_receive(r, ev, now);
    let (c2, o) = receive_core(c, ev, now);
    lemma_prune_p(c.pending, now);
    lemma_prune_s(c.seen, now);
    let pp = prune_p(c.pending, now);
    let ps = prune_s(c.seen, now);
    if !ev.valid || ev.test || !armed(c, now) || (ev.window_id is Some && ev.window_id->Some_0 != c.window->Some_0.id) {
        assert(c2 == c);
        assert forall|i: int| 0 <= i < r2.taken.len() implies covered(r2.c.seen, (#[trigger] r2.taken[i]).id, r2.taken[i].expires as int) || r2.clock >= r2.taken[i].expires by {
            assert(covered(r.c.seen, r.taken[i].id, r.taken[i].expires as int) || r.clock >= r.taken[i].expires);
        }
        return;
    }
    // Facts about the pruned store c1 shared by every remaining branch.
    // (a) I3 holds for c1.
    assert forall|i: int| 0 <= i < pp.len() implies covered(ps, (#[trigger] pp[i]).id, pp[i].expires as int) by {
        assert(pp.contains(pp[i]));
        assert(c.pending.contains(pp[i]));
        let k = choose|k: int| 0 <= k < c.pending.len() && c.pending[k] == pp[i];
        assert(covered(c.seen, c.pending[k].id, c.pending[k].expires as int));
        let j = choose|j: int| 0 <= j < c.seen.len() && (#[trigger] c.seen[j]).id == c.pending[k].id && c.seen[j].until >= c.pending[k].expires;
        assert(c.seen.contains(c.seen[j]));
        assert(ps.contains(c.seen[j]));
        let j2 = choose|j2: int| 0 <= j2 < ps.len() && ps[j2] == c.seen[j];
        assert(ps[j2].id == pp[i].id);
    }
    // (b) I5 holds for the pruned seen at clock now.
    assert forall|i: int| 0 <= i < r.taken.len() implies covered(ps, (#[trigger] r.taken[i]).id, r.taken[i].expires as int) || now >= r.taken[i].expires by {
        let t = r.taken[i];
        if covered(c.seen, t.id, t.expires as int) {
            let j = choose|j: int| 0 <= j < c.seen.len() && (#[trigger] c.seen[j]).id == t.id && c.seen[j].until >= t.expires;
            if c.seen[j].until > now {
                assert(c.seen.contains(c.seen[j]));
                assert(ps.contains(c.seen[j]));
                let j2 = choose|j2: int| 0 <= j2 < ps.len() && ps[j2] == c.seen[j];
                assert(ps[j2].id == t.id);
            }
        }
    }
    // (c) I6 holds for the pruned pending.
    assert forall|i: int, k: int| 0 <= i < r.taken.len() && 0 <= k < pp.len() && (#[trigger] r.taken[i]).id == (#[trigger] pp[k]).id implies pp[k].stored_at >= r.taken[i].expires by {
        assert(pp.contains(pp[k]));
        assert(c.pending.contains(pp[k]));
        let k2 = choose|k2: int| 0 <= k2 < c.pending.len() && c.pending[k2] == pp[k];
        assert(r.taken[i].id == c.pending[k2].id);
    }
    // R4 (second form).
    if !seen_has(ps, ev.id) {
        assert forall|j: int| 0 <= j < c.seen.len() && (#[trigger] c.seen[j]).id == ev.id implies c.seen[j].until <= now by {
            if c.seen[j].until > now {
                assert(c.seen.contains(c.seen[j]));
                assert(ps.contains(c.seen[j]));
                let j2 = choose|j2: int| 0 <= j2 < ps.len() && ps[j2] == c.seen[j];
                assert(ps[j2].id == ev.id);
            }
        }
    }
    let wend = match c.window->Some_0.end {
        Some(e) => e,
        None => U64MAX,
    };
    if seen_has(ps, ev.id) || expires_spec(ev.received_at, now, wend) is None || ps.len() >= c.lim_seen {
        assert(c2.pending == pp && c2.seen == ps);
        return;
    }
    // Stored.
    let e = expires_spec(ev.received_at, now, wend)->Some_0;
    let p1 = if (pp.len() >= c.lim_pending && pp.len() > 0) { pp.subrange(1, pp.len() as int) } else { pp };
    let entry = Entry { id: ev.id, code: ev.code, received_at: base(ev.received_at, now) as u64, expires: e, stored_at: now };
    let ns = Seen { id: ev.id, until: sat(now + SEEN_TTL) as u64 };
    assert(c2.pending == p1.push(entry));
    assert(c2.seen == ps.push(ns));
    let np = c2.pending;
    let nss = c2.seen;
    assert(e as int <= sat(now + SEEN_TTL));
    // Everything in p1 is in pp.
    assert forall|i: int| 0 <= i < p1.len() implies #[trigger] p1[i] == pp[if (pp.len() >= c.lim_pending && pp.len() > 0) { i + 1 } else { i }] by {}
    // Coverage lifts from ps to nss.
    assert forall|id: u64, t: int| covered(ps, id, t) implies covered(nss, id, t) by {
        let j = choose|j: int| 0 <= j < ps.len() && (#[trigger] ps[j]).id == id && ps[j].until >= t;
        assert(nss[j] == ps[j]);
    }
    // I3.
    assert forall|i: int| 0 <= i < np.len() implies covered(nss, (#[trigger] np[i]).id, np[i].expires as int) by {
        if i == np.len() - 1 {
            assert(np[i] == entry);
            assert(nss[nss.len() - 1] == ns);
        } else {
            let k = if (pp.len() >= c.lim_pending && pp.len() > 0) { i + 1 } else { i };
            assert(np[i] == p1[i]);
            assert(p1[i] == pp[k]);
            assert(covered(ps, pp[k].id, pp[k].expires as int));
        }
    }
    // I4: the new id is not pending (else it would be in ps by (a)).
    assert forall|i: int, j: int| 0 <= i < j < np.len() implies (#[trigger] np[i]).id != (#[trigger] np[j]).id by {
        let ki = if (pp.len() >= c.lim_pending && pp.len() > 0) { i + 1 } else { i };
        if j == np.len() - 1 {
            assert(np[i] == pp[ki]);
            assert(covered(ps, pp[ki].id, pp[ki].expires as int));
            if pp[ki].id == ev.id {
                let jj = choose|jj: int| 0 <= jj < ps.len() && (#[trigger] ps[jj]).id == pp[ki].id && ps[jj].until >= pp[ki].expires;
                assert(seen_has(ps, ev.id));
            }
        } else {
            let kj = if (pp.len() >= c.lim_pending && pp.len() > 0) { j + 1 } else { j };
            assert(np[i] == pp[ki] && np[j] == pp[kj]);
        }
    }
    // I5.
    assert forall|i: int| 0 <= i < r2.taken.len() implies covered(nss, (#[trigger] r2.taken[i]).id, r2.taken[i].expires as int) || r2.clock >= r2.taken[i].expires by {
        assert(covered(ps, r.taken[i].id, r.taken[i].expires as int) || now >= r.taken[i].expires);
    }
    // I6: the new entry's stored_at is now, past every delivered code with its id.
    assert forall|i: int, k: int| 0 <= i < r2.taken.len() && 0 <= k < np.len() && (#[trigger] r2.taken[i]).id == (#[trigger] np[k]).id implies np[k].stored_at >= r2.taken[i].expires by {
        let t = r.taken[i];
        if k == np.len() - 1 {
            assert(np[k] == entry);
            assert(covered(ps, t.id, t.expires as int) || now >= t.expires);
            if covered(ps, t.id, t.expires as int) {
                let jj = choose|jj: int| 0 <= jj < ps.len() && (#[trigger] ps[jj]).id == t.id && ps[jj].until >= t.expires;
                assert(seen_has(ps, ev.id));
            }
        } else {
            let kk = if (pp.len() >= c.lim_pending && pp.len() > 0) { k + 1 } else { k };
            assert(np[k] == pp[kk]);
        }
    }
}

proof fn lemma_take_inv(r: Relay, now: u64)
    requires
        inv(r),
        now as int >= r.clock,
    ensures
        inv(step_take(r, now)),
        // R1
        take_core(r.c, now).1 is Some ==> take_core(r.c, now).1->Some_0.expires > now,
        // R2: consumed.
        take_core(r.c, now).1 is Some ==> forall|k: int| 0 <= k < take_core(r.c, now).0.pending.len() ==> (#[trigger] take_core(r.c, now).0.pending[k]).id != take_core(r.c, now).1->Some_0.id,
        // R2: a code with an id delivered before was stored after that one expired.
        take_core(r.c, now).1 is Some ==> forall|i: int| 0 <= i < r.taken.len() && (#[trigger] r.taken[i]).id == take_core(r.c, now).1->Some_0.id ==> take_core(r.c, now).1->Some_0.stored_at >= r.taken[i].expires,
{
    let c = r.c;
    let r2 = step_take(r, now);
    if !armed(c, now) {
        lemma_clear_pending(r, r2);
        return;
    }
    lemma_take_idx(c.pending, now);
    let k = take_idx(c.pending, now);
    if k < 0 {
        lemma_clear_pending(r, r2);
        return;
    }
    let d = c.pending[k];
    let rest = c.pending.subrange(0, k);
    assert(r2.c.pending == rest);
    assert(r2.taken == r.taken.push(d));
    assert forall|i: int| 0 <= i < r2.taken.len() implies covered(r2.c.seen, (#[trigger] r2.taken[i]).id, r2.taken[i].expires as int) || r2.clock >= r2.taken[i].expires by {
        if i == r.taken.len() {
            assert(r2.taken[i] == d);
            assert(covered(c.seen, c.pending[k].id, c.pending[k].expires as int));
        } else {
            assert(r2.taken[i] == r.taken[i]);
            assert(covered(c.seen, r.taken[i].id, r.taken[i].expires as int) || r.clock >= r.taken[i].expires);
        }
    }
    assert forall|i: int| 0 <= i < rest.len() implies covered(r2.c.seen, (#[trigger] rest[i]).id, rest[i].expires as int) by {
        assert(rest[i] == c.pending[i]);
        assert(covered(c.seen, c.pending[i].id, c.pending[i].expires as int));
    }
    assert forall|i: int, j: int| 0 <= i < j < rest.len() implies (#[trigger] rest[i]).id != (#[trigger] rest[j]).id by {
        assert(rest[i] == c.pending[i] && rest[j] == c.pending[j]);
    }
    assert forall|i: int, m: int| 0 <= i < r2.taken.len() && 0 <= m < rest.len() && (#[trigger] r2.taken[i]).id == (#[trigger] rest[m]).id implies rest[m].stored_at >= r2.taken[i].expires by {
        assert(rest[m] == c.pending[m]);
        if i == r.taken.len() {
            assert(r2.taken[i] == c.pending[k]);
            assert(c.pending[m].id != c.pending[k].id);
        } else {
            assert(r2.taken[i] == r.taken[i]);
        }
    }
    assert forall|m: int| 0 <= m < rest.len() implies (#[trigger] rest[m]).id != d.id by {
        assert(rest[m] == c.pending[m]);
    }
    assert forall|i: int| 0 <= i < r.taken.len() && (#[trigger] r.taken[i]).id == d.id implies d.stored_at >= r.taken[i].expires by {
        assert(r.taken[i].id == c.pending[k].id);
    }
}

proof fn lemma_arm_disarm_inv(r: Relay, seconds: u64, now: u64, wid: u64)
    requires
        inv(r),
        now as int >= r.clock,
    ensures
        inv(step_arm(r, seconds, now, wid)),
        inv(step_disarm(r)),
        // R7 (F2): arming never drops a code that is still fresh.
        forall|i: int| 0 <= i < r.c.pending.len() && (#[trigger] r.c.pending[i]).expires > now ==> step_arm(
            r,
            seconds,
            now,
            wid,
        ).c.pending.contains(r.c.pending[i]),
{
    lemma_clear_pending(r, step_disarm(r));
    let c = r.c;
    let r2 = step_arm(r, seconds, now, wid);
    lemma_prune_p(c.pending, now);
    let pp = prune_p(c.pending, now);
    assert(r2.c.pending == pp && r2.c.seen == c.seen);
    assert forall|i: int| 0 <= i < pp.len() implies covered(r2.c.seen, (#[trigger] pp[i]).id, pp[i].expires as int) by {
        assert(pp.contains(pp[i]));
        let k = choose|k: int| 0 <= k < c.pending.len() && c.pending[k] == pp[i];
        assert(covered(c.seen, c.pending[k].id, c.pending[k].expires as int));
    }
    assert forall|i: int| 0 <= i < r2.taken.len() implies covered(r2.c.seen, (#[trigger] r2.taken[i]).id, r2.taken[i].expires as int) || r2.clock >= r2.taken[i].expires by {
        assert(covered(r.c.seen, r.taken[i].id, r.taken[i].expires as int) || r.clock >= r.taken[i].expires);
    }
    assert forall|i: int, k: int| 0 <= i < r2.taken.len() && 0 <= k < pp.len() && (#[trigger] r2.taken[i]).id == (#[trigger] pp[k]).id implies pp[k].stored_at >= r2.taken[i].expires by {
        assert(pp.contains(pp[k]));
        let k2 = choose|k2: int| 0 <= k2 < c.pending.len() && c.pending[k2] == pp[k];
        assert(r.taken[i].id == c.pending[k2].id);
    }
    assert forall|i: int| 0 <= i < c.pending.len() && (#[trigger] c.pending[i]).expires > now implies r2.c.pending.contains(c.pending[i]) by {
        assert(c.pending.contains(c.pending[i]));
    }
}

/// R4: an event whose id was stored less than SEEN_TTL ago is a duplicate (when it gets
/// past the window checks), so it is never stored twice within the TTL.
proof fn lemma_duplicate_within_ttl(c: Core, ev: Event, now: u64, j: int)
    requires
        0 <= j < c.seen.len(),
        c.seen[j].id == ev.id,
        c.seen[j].until > now,
    ensures
        receive_core(c, ev, now).1 != Outcome::Stored,
        receive_core(c, ev, now).0.pending.len() <= c.pending.len(),
{
    lemma_prune_s(c.seen, now);
    lemma_prune_p(c.pending, now);
    assert(c.seen.contains(c.seen[j]));
    let ps = prune_s(c.seen, now);
    assert(ps.contains(c.seen[j]));
    let j2 = choose|j2: int| 0 <= j2 < ps.len() && ps[j2] == c.seen[j];
    assert(seen_has(ps, ev.id));
}

// ------------------------------------------------------------ exec store

pub struct Store {
    pub continuous: bool,
    pub paused: bool,
    pub lim_pending: usize,
    pub lim_seen: usize,
    pub window: Option<Win>,
    pub pending: Vec<Entry>,
    pub seen: Vec<Seen>,
}

impl Store {
    pub open spec fn view(&self) -> Core {
        Core {
            continuous: self.continuous,
            paused: self.paused,
            lim_pending: self.lim_pending as nat,
            lim_seen: self.lim_seen as nat,
            window: self.window,
            pending: self.pending@,
            seen: self.seen@,
        }
    }

    fn armed_exec(&self, now: u64) -> (b: bool)
        ensures
            b == armed(self@, now),
    {
        match self.window {
            None => false,
            Some(w) => match w.end {
                None => true,
                Some(end) => now < end,
            },
        }
    }

    fn prune(&mut self, now: u64)
        ensures
            final(self)@ == (Core { pending: prune_p(old(self)@.pending, now), seen: prune_s(old(self)@.seen, now), ..old(self)@ }),
    {
        let mut p: Vec<Entry> = Vec::new();
        let mut i: usize = 0;
        while i < self.pending.len()
            invariant
                0 <= i <= self.pending.len(),
                p@ == prune_p(self.pending@.subrange(0, i as int), now),
            decreases self.pending.len() - i,
        {
            let x = self.pending[i];
            assert(self.pending@.subrange(0, i + 1).drop_last() =~= self.pending@.subrange(0, i as int));
            if x.expires > now {
                p.push(x);
            }
            i += 1;
        }
        assert(self.pending@.subrange(0, self.pending.len() as int) =~= self.pending@);
        let mut s: Vec<Seen> = Vec::new();
        let mut i: usize = 0;
        while i < self.seen.len()
            invariant
                0 <= i <= self.seen.len(),
                s@ == prune_s(self.seen@.subrange(0, i as int), now),
            decreases self.seen.len() - i,
        {
            let x = self.seen[i];
            assert(self.seen@.subrange(0, i + 1).drop_last() =~= self.seen@.subrange(0, i as int));
            if x.until > now {
                s.push(x);
            }
            i += 1;
        }
        assert(self.seen@.subrange(0, self.seen.len() as int) =~= self.seen@);
        self.pending = p;
        self.seen = s;
    }

    fn seen_has_exec(&self, id: u64) -> (b: bool)
        ensures
            b == seen_has(self@.seen, id),
    {
        let mut i: usize = 0;
        while i < self.seen.len()
            invariant
                0 <= i <= self.seen.len(),
                forall|j: int| 0 <= j < i ==> (#[trigger] self.seen@[j]).id != id,
            decreases self.seen.len() - i,
        {
            if self.seen[i].id == id {
                return true;
            }
            i += 1;
        }
        false
    }

    /// `Store::receive`.
    pub fn receive(&mut self, ev: &Event, now: u64) -> (o: Outcome)
        ensures
            (final(self)@, o) == receive_core(old(self)@, *ev, now),
    {
        if !ev.valid {
            return Outcome::Invalid;
        }
        if ev.test {
            return Outcome::TestAck;
        }
        if !self.armed_exec(now) {
            return Outcome::Paused;
        }
        let w = match self.window {
            Some(w) => w,
            None => { return Outcome::Paused; },
        };
        match ev.window_id {
            Some(id) => if id != w.id {
                return Outcome::WrongWindow;
            },
            None => {},
        }
        let wend = match w.end {
            Some(e) => e,
            None => U64MAX,
        };
        self.prune(now);
        if self.seen_has_exec(ev.id) {
            return Outcome::Duplicate;
        }
        let e = match expires_at(ev.received_at, now, wend) {
            Some(e) => e,
            None => { return Outcome::Expired; },
        };
        if self.seen.len() >= self.lim_seen {
            return Outcome::EventLimit;
        }
        self.seen.push(Seen { id: ev.id, until: sat_add(now, SEEN_TTL) });
        if self.pending.len() >= self.lim_pending && self.pending.len() > 0 {
            let ghost before = self.pending@;
            self.pending.remove(0);
            assert(self.pending@ =~= before.subrange(1, before.len() as int));
        }
        let b = match ev.received_at {
            Some(t) => if t <= now { t } else { now },
            None => now,
        };
        self.pending.push(Entry { id: ev.id, code: ev.code, received_at: b, expires: e, stored_at: now });
        Outcome::Stored
    }

    /// `Store::take`.
    pub fn take(&mut self, now: u64) -> (d: Option<Entry>)
        ensures
            (final(self)@, d) == take_core(old(self)@, now),
    {
        if !self.armed_exec(now) {
            self.window = None;
            self.pending = Vec::new();
            assert(self@.pending =~= Seq::<Entry>::empty());
            return None;
        }
        let ghost orig = self.pending@;
        while self.pending.len() > 0
            invariant
                take_idx(orig, now) == take_idx(self.pending@, now),
                self.pending@ == orig.subrange(0, self.pending@.len() as int),
                self.pending@.len() <= orig.len(),
                self@.window == old(self)@.window,
                self.continuous == old(self).continuous,
                self.paused == old(self).paused,
                self.lim_pending == old(self).lim_pending,
                self.lim_seen == old(self).lim_seen,
                self.seen@ == old(self).seen@,
                old(self)@.pending == orig,
                armed(old(self)@, now),
            decreases self.pending.len(),
        {
            let ghost before = self.pending@;
            let d = self.pending.pop().unwrap();
            assert(self.pending@ =~= before.drop_last());
            assert(self.pending@ =~= orig.subrange(0, self.pending@.len() as int));
            if d.expires > now {
                assert(take_idx(before, now) == before.len() - 1);
                assert(orig[before.len() - 1] == d);
                return Some(d);
            }
        }
        assert(self.pending@ =~= Seq::<Entry>::empty());
        None
    }

    fn prune_pending(&mut self, now: u64)
        ensures
            final(self)@ == (Core { pending: prune_p(old(self)@.pending, now), ..old(self)@ }),
    {
        let mut p: Vec<Entry> = Vec::new();
        let mut i: usize = 0;
        while i < self.pending.len()
            invariant
                0 <= i <= self.pending.len(),
                p@ == prune_p(self.pending@.subrange(0, i as int), now),
            decreases self.pending.len() - i,
        {
            let x = self.pending[i];
            assert(self.pending@.subrange(0, i + 1).drop_last() =~= self.pending@.subrange(0, i as int));
            if x.expires > now {
                p.push(x);
            }
            i += 1;
        }
        assert(self.pending@.subrange(0, self.pending.len() as int) =~= self.pending@);
        self.pending = p;
    }

    /// `Store::arm` (F2).
    pub fn arm(&mut self, seconds: u64, now: u64, wid: u64)
        ensures
            final(self)@ == arm_core(old(self)@, seconds, now, wid),
    {
        let id = if self.armed_exec(now) {
            match self.window {
                Some(w) => w.id,
                None => wid,
            }
        } else {
            wid
        };
        self.prune_pending(now);
        if self.continuous {
            self.paused = false;
            self.window = Some(Win { id, end: None });
        } else {
            let s = if seconds < MIN_ARM { MIN_ARM } else if seconds > MAX_ARM { MAX_ARM } else { seconds };
            self.window = Some(Win { id, end: Some(sat_add(now, s * 1000)) });
        }
    }

    /// `Store::disarm`.
    pub fn disarm(&mut self)
        ensures
            final(self)@ == disarm_core(old(self)@),
    {
        self.window = None;
        self.pending = Vec::new();
        if self.continuous {
            self.paused = true;
        }
        assert(self@.pending =~= Seq::<Entry>::empty());
    }
}

// ======================================================================== phone queue

#[derive(Clone, Copy)]
pub struct Pend {
    pub id: u64,
    pub received_at: u64,
}

pub open spec fn fresh(p: Pend, now: u64) -> bool {
    sat(p.received_at + TTL) > now
}

pub open spec fn all_fresh(q: Seq<Pend>, now: u64) -> bool {
    forall|i: int| 0 <= i < q.len() ==> fresh(#[trigger] q[i], now)
}

fn fresh_exec(p: Pend, now: u64) -> (b: bool)
    ensures
        b == fresh(p, now),
{
    sat_add(p.received_at, TTL) > now
}

/// `forward`'s received_at (`u64::try_from(ms).ok().filter(t > 0).map_or(now, min(now))`)
/// and its TTL gate: Some(received_at) when the code may be sent or queued.
pub fn forward_gate(received_at_ms: i64, now: u64) -> (r: Option<u64>)
    ensures
        r is Some ==> fresh(Pend { id: 0, received_at: r->Some_0 }, now) && r->Some_0 <= now,
        r is None ==> sat((if received_at_ms > 0 { min(received_at_ms as int, now as int) } else { now as int }) + TTL) <= now,
{
    let t = if received_at_ms > 0 {
        let m = received_at_ms as u64;
        if m < now { m } else { now }
    } else {
        now
    };
    if sat_add(t, TTL) <= now {
        None
    } else {
        Some(t)
    }
}

/// `queue.retain(|p| p.fresh(now))`.
pub fn retain_fresh(q: &mut Vec<Pend>, now: u64)
    ensures
        all_fresh(final(q)@, now),
        final(q)@.len() <= old(q)@.len(),
        forall|i: int| 0 <= i < final(q)@.len() ==> old(q)@.contains(#[trigger] final(q)@[i]),
{
    let mut out: Vec<Pend> = Vec::new();
    let mut i: usize = 0;
    while i < q.len()
        invariant
            0 <= i <= q.len(),
            out@.len() <= i,
            all_fresh(out@, now),
            forall|k: int| 0 <= k < out@.len() ==> q@.contains(#[trigger] out@[k]),
        decreases q.len() - i,
    {
        let p = q[i];
        if fresh_exec(p, now) {
            out.push(p);
            assert(q@[i as int] == p);
        }
        i += 1;
    }
    *q = out;
}

/// `Bridge::enqueue`: drop stale entries, then the oldest until there is room, then push.
pub fn enqueue(q: &mut Vec<Pend>, p: Pend, now: u64)
    requires
        fresh(p, now),
    ensures
        final(q)@.len() <= MAX_QUEUE,
        all_fresh(final(q)@, now),
        final(q)@.last() == p,
        final(q)@.len() >= 1,
        forall|i: int| 0 <= i < final(q)@.len() - 1 ==> old(q)@.contains(#[trigger] final(q)@[i]),
{
    retain_fresh(q, now);
    while q.len() >= MAX_QUEUE
        invariant
            all_fresh(q@, now),
            forall|i: int| 0 <= i < q@.len() ==> old(q)@.contains(#[trigger] q@[i]),
        decreases q.len(),
    {
        let ghost before = q@;
        q.remove(0);
        assert forall|i: int| 0 <= i < q@.len() implies fresh(#[trigger] q@[i], now) && old(q)@.contains(q@[i]) by {
            assert(q@[i] == before[i + 1]);
        }
    }
    let ghost before = q@;
    q.push(p);
    assert forall|i: int| 0 <= i < q@.len() - 1 implies old(q)@.contains(#[trigger] q@[i]) by {
        assert(q@[i] == before[i]);
    }
}

/// `Bridge::flush_at`: drop stale entries, then POST from the front until one answers
/// Retry (`retry[i]` for the i-th POST), which goes back to the front. Returns what was
/// POSTed, in order.
pub fn flush(q: &mut Vec<Pend>, now: u64, retry: &Vec<bool>) -> (posted: Vec<Pend>)
    ensures
        all_fresh(posted@, now),
        all_fresh(final(q)@, now),
        final(q)@.len() <= old(q)@.len(),
        forall|i: int| 0 <= i < posted@.len() ==> old(q)@.contains(#[trigger] posted@[i]),
{
    retain_fresh(q, now);
    let mut posted: Vec<Pend> = Vec::new();
    let ghost start_len = q@.len();
    let ghost q0 = q@;
    assert forall|x: Pend| #[trigger] q0.contains(x) implies old(q)@.contains(x) by {
        let i = choose|i: int| 0 <= i < q0.len() && q0[i] == x;
        assert(old(q)@.contains(q0[i]));
    }
    while q.len() > 0
        invariant
            forall|x: Pend| #[trigger] q0.contains(x) ==> old(q)@.contains(x),
            forall|i: int| 0 <= i < q@.len() ==> q0.contains(#[trigger] q@[i]),
            forall|i: int| 0 <= i < posted@.len() ==> q0.contains(#[trigger] posted@[i]),
            all_fresh(q@, now),
            all_fresh(posted@, now),
            q@.len() <= start_len,
        decreases q.len(),
    {
        let ghost before = q@;
        let p = q.remove(0);
        assert(fresh(before[0], now));
        assert(q0.contains(before[0]));
        assert forall|i: int| 0 <= i < q@.len() implies q0.contains(#[trigger] q@[i]) by {
            assert(q@[i] == before[i + 1]);
            assert(q0.contains(before[i + 1]));
        }
        assert forall|i: int| 0 <= i < q@.len() implies fresh(#[trigger] q@[i], now) by {
            assert(q@[i] == before[i + 1]);
        }
        let k = posted.len();
        posted.push(p);
        if k < retry.len() && retry[k] {
            q.insert(0, p);
            assert forall|i: int| 0 <= i < q@.len() implies q0.contains(#[trigger] q@[i]) by {
                if i > 0 {
                    assert(q@[i] == before[i]);
                    assert(q0.contains(before[i]));
                }
            }
            assert forall|i: int| 0 <= i < q@.len() implies fresh(#[trigger] q@[i], now) by {
                if i > 0 {
                    assert(q@[i] == before[i]);
                }
            }
            break;
        }
    }
    posted
}

/// Removes every entry with event id `id` (`queue.retain(|p| p.event_id != event_id)`).
pub fn remove_id(q: &mut Vec<Pend>, id: u64)
    ensures
        forall|i: int| 0 <= i < final(q)@.len() ==> (#[trigger] final(q)@[i]).id != id,
        forall|i: int| 0 <= i < final(q)@.len() ==> old(q)@.contains(#[trigger] final(q)@[i]),
        final(q)@.len() <= old(q)@.len(),
{
    let mut out: Vec<Pend> = Vec::new();
    let mut i: usize = 0;
    while i < q.len()
        invariant
            0 <= i <= q.len(),
            out@.len() <= i,
            forall|k: int| 0 <= k < out@.len() ==> (#[trigger] out@[k]).id != id,
            forall|k: int| 0 <= k < out@.len() ==> q@.contains(#[trigger] out@[k]),
        decreases q.len() - i,
    {
        let p = q[i];
        if p.id != id {
            out.push(p);
            assert(q@[i as int] == p);
        }
        i += 1;
    }
    *q = out;
}

/// `Bridge::forward` after F1: the code is queued (write-ahead) before the POST; an
/// answer other than Retry (`answered`) removes it again.
pub fn forward_queue(q: &mut Vec<Pend>, p: Pend, now: u64, answered: bool)
    requires
        fresh(p, now),
    ensures
        final(q)@.len() <= MAX_QUEUE,
        all_fresh(final(q)@, now),
        !answered ==> final(q)@.len() >= 1 && final(q)@.last() == p,
        answered ==> forall|i: int| 0 <= i < final(q)@.len() ==> (#[trigger] final(q)@[i]).id != p.id,
{
    enqueue(q, p, now);
    if answered {
        let ghost before = q@;
        remove_id(q, p.id);
        assert forall|i: int| 0 <= i < q@.len() implies fresh(#[trigger] q@[i], now) by {
            assert(before.contains(q@[i]));
            let k = choose|k: int| 0 <= k < before.len() && before[k] == q@[i];
        }
    }
}

// ======================================================================== non-vacuity

fn witness_receive_take_once() {
    let mut s = Store {
        continuous: false,
        paused: false,
        lim_pending: 8,
        lim_seen: 64,
        window: None,
        pending: Vec::new(),
        seen: Vec::new(),
    };
    s.arm(60, 1_000_000, 7);
    let ev = Event { id: 11, code: 482917, received_at: Some(990_000), window_id: None, test: false, valid: true };
    let o = s.receive(&ev, 1_000_000);
    assert(o == Outcome::Stored);
    assert(s.seen@.len() == 1 && s.seen@[0].id == 11 && s.seen@[0].until == 1_120_000);
    assert(s.pending@.len() == 1 && s.pending@[0].expires == 1_060_000);
    proof {
        let p1 = s.pending@;
        assert(p1.drop_last() =~= Seq::<Entry>::empty());
        assert(prune_p(p1.drop_last(), 1_000_001) =~= Seq::<Entry>::empty());
        assert(prune_p(p1, 1_000_001) =~= p1);
    }
    proof {
        let one = s.seen@;
        assert(one.drop_last() =~= Seq::<Seen>::empty());
        assert(prune_s(one.drop_last(), 1_000_001) =~= Seq::<Seen>::empty());
        assert(prune_s(one, 1_000_001) =~= seq![one[0]]);
        assert(seen_has(prune_s(one, 1_000_001), 11)) by {
            assert(prune_s(one, 1_000_001)[0].id == 11);
        }
    }
    let o2 = s.receive(&ev, 1_000_001);
    assert(o2 == Outcome::Duplicate);
    // F2: re-arming keeps the waiting code (and the live window's id).
    s.arm(30, 1_000_001, 8);
    assert(s.window is Some && s.window->Some_0.id == 7);
    let d = s.take(1_000_002);
    assert(d is Some);
    assert(d->Some_0.code == 482917 && d->Some_0.expires == 1_060_000);
    assert(s.pending@.len() == 0);
    let d2 = s.take(1_000_003);
    assert(d2 is None);
}

fn witness_queue() {
    let mut q: Vec<Pend> = Vec::new();
    enqueue(&mut q, Pend { id: 1, received_at: 1000 }, 2000);
    assert(q@.len() == 1) by {
        if q@.len() > 1 {
            assert(Seq::<Pend>::empty().contains(q@[0]));
        }
    }
    let retry: Vec<bool> = Vec::new();
    let later: u64 = 2000 + TTL;
    let posted = flush(&mut q, later, &retry);
    // received at 1000: stale at 122_000; nothing is posted.
    assert(posted@.len() == 0) by {
        if posted@.len() > 0 {
            assert(fresh(posted@[0], later));
        }
    }
}

fn main() {
}

} // verus!
