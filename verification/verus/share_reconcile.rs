// Share-to-Lucina kernels, part 2: idempotent receive, the chat bubble for a share, and
// answering a share at most once across a crash (POST /v1/share, docs/share.md).
//
// Production (branch lucina-rs, commit 485d658):
//   Server.store            <-> the share dirs' status.json (shareId -> the saved Job, whose
//                               user_item.id is fixed when it is first saved)
//   Server.timeline         <-> timeline.jsonl (lucina-store TimelineStore)
//   Server.next             <-> fresh ids: normalize -> new_id(now), moved past the newest id
//                               (verification/verus/timeline_ids.rs T4/T5: ids never repeat)
//   receive                 <-> mod.rs::receive under `ingest`: find(root, id) -> 202
//                               duplicate with the saved job; else a fresh user item,
//                               save(status.json), ensure_user, 202. A crash before save
//                               leaves a dir without status.json, which the next receive
//                               replaces (`remove_dir_all`).
//   ensure_user             <-> mod.rs::ensure_user (has_item, then append with the saved id;
//                               TimelineStore::append also skips an id it already holds)
//   recover                 <-> mod.rs::recover -> spawn -> process -> ensure_user
//   Bubble / on_item /      <-> the chat client's share bubble: the optimistic message for
//   on_ack                      the share, timeline items merged into the list (by item id,
//                               and by meta.shareId), and the 202's userItemId adopted
//                               (web/src/state/chat.ts mergeItems / matchItem / adopt)
//   full_scan / existing    <-> mod.rs::existing_reply since 485d658 (every item, newest
//                               first; the `raw.contains(user_item)` prefilter is sound
//                               because a reply's raw line contains its replyTo)
//   window_scan             <-> existing_reply before 485d658 (`.rev().take(500)`)
//   process                 <-> mod.rs::process: ensure_user, existing_reply, else one model
//                               turn that writes the Lucina item (replyTo = user item), then
//                               save(status.json "done"); a crash in between leaves
//                               "processing", and recover runs process again
// Differential test: crates/lucina-server/tests/verus_share.rs (branch lucina-rs): repeated
// receives of one shareId through the real endpoint (one user item, one userItemId), and
// tests/share.rs `recovery_finds_a_reply_far_behind_newer_items` for SK3.
//
// Proven:
//   SK2.1 receive_preserves_inv / store_is_stable: a saved shareId keeps its userItemId
//         forever, through receives (fresh, duplicate, crash before or after the save),
//         recovery, and other writers.
//   SK2.2 responses_agree: over any trace, every 202 (fresh or duplicate) for a shareId
//         carries the same userItemId, the saved one.
//   SK2.3 one_user_item_per_share: the timeline never holds two items for one share, and
//         the one it holds has the saved userItemId; after a crash after the save, recovery
//         appends it (witness_crash_after_save_recovers).
//   SK2.4 one_bubble_per_share: matching a timeline item to the share's bubble by
//         (userItemId from the 202) OR (meta.shareId) leaves exactly one bubble for the
//         share over any order and repetition of the 202, the status polls and the item.
//   SK3.1 answered_at_most_once: with the full scan, any trace of processing runs (each may
//         crash between writing the reply and saving status.json) and other writers'
//         appends leaves at most one Lucina reply to the share's user item.
//   SK3.2 prefilter_is_sound: skipping items whose raw line does not contain the user item
//         id finds the same reply.
//
// Counterexamples (unfixed_*):
//   unfixed_match_by_id_only: matching by userItemId alone, a timeline item that arrives
//     before the 202 becomes a second bubble (the 202 then adopts the optimistic one).
//     web/src/state/chat.ts's adopt() folds that copy when the 202 or a status poll arrives,
//     so the client must keep that fold or match by meta.shareId (SK2.4).
//   unfixed_window_scan: with the newest-500 window, a reply followed by 500 other items
//     before the restart is not found, and the share is answered twice (fixed in 485d658).
//
//   SK3.3 voice_recovery_once (branch voice-notes, 438d748): every item a voice note can get
//     is a Lucina item with replyTo = the share's user item: the answer to the device or
//     whisper transcript (chat::share_turn / tools_turn, TimelineTurn::with_user_item), the
//     "couldn't hear it" item (share::process) and a tools-planner progress item (on_tool).
//     Each is found by the full scan after a crash, so the share is not answered again; and
//     because a progress item is found too, it would become "the answer" after a crash:
//     tools_turn must not write it for a share turn (the Stateright ablation shows the second
//     Lucina item it adds without a crash).
// No assume, admit or external_body.
use vstd::prelude::*;

verus! {

// ================================================================ SK2: receive

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Role {
    User,
    Lucina,
}

pub struct Entry {
    pub id: nat,
    /// meta.shareId
    pub share: Option<nat>,
    pub role: Role,
    pub reply_to: Option<nat>,
}

pub struct Server {
    pub store: Map<nat, nat>,
    pub timeline: Seq<Entry>,
    pub next: nat,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Crash {
    None,
    /// before save(status.json): the request fails, the staged dir has no status.json
    BeforeSave,
    /// after save, before ensure_user and the 202
    AfterSave,
}

pub struct Response {
    pub share: nat,
    pub user_item: nat,
    pub duplicate: bool,
}

pub enum Event {
    Receive { share: nat, crash: Crash },
    /// recover() resumes a saved, processing share
    Recover { share: nat },
    /// another writer appends (a chat message, a reply, another channel)
    Other { role: Role, reply_to: Option<nat> },
}

pub open spec fn has_item(tl: Seq<Entry>, id: nat) -> bool {
    exists|i: int| 0 <= i < tl.len() && (#[trigger] tl[i]).id == id
}

pub open spec fn ensure_user(s: Server, share: nat) -> Server {
    let id = s.store[share];
    if has_item(s.timeline, id) {
        s
    } else {
        Server {
            timeline: s.timeline.push(Entry { id, share: Some(share), role: Role::User, reply_to: None }),
            ..s
        }
    }
}

pub open spec fn receive(s: Server, share: nat, crash: Crash) -> (Server, Option<Response>) {
    if s.store.contains_key(share) {
        // find(root, id): the saved job, as a duplicate
        (s, Some(Response { share, user_item: s.store[share], duplicate: true }))
    } else {
        let id = s.next;
        let s1 = Server { next: s.next + 1, ..s };
        match crash {
            Crash::BeforeSave => (s1, None),
            Crash::AfterSave => (Server { store: s1.store.insert(share, id), ..s1 }, None),
            Crash::None => {
                let s2 = Server { store: s1.store.insert(share, id), ..s1 };
                (ensure_user(s2, share), Some(Response { share, user_item: id, duplicate: false }))
            },
        }
    }
}

pub open spec fn apply(s: Server, e: Event) -> (Server, Option<Response>) {
    match e {
        Event::Receive { share, crash } => receive(s, share, crash),
        Event::Recover { share } => if s.store.contains_key(share) {
            (ensure_user(s, share), None)
        } else {
            (s, None)
        },
        Event::Other { role, reply_to } => (
            Server {
                timeline: s.timeline.push(Entry { id: s.next, share: None, role, reply_to }),
                next: s.next + 1,
                ..s
            },
            None,
        ),
    }
}

pub open spec fn inv(s: Server) -> bool {
    &&& forall|i: int| 0 <= i < s.timeline.len() ==> (#[trigger] s.timeline[i]).id < s.next
    &&& forall|k: nat| s.store.contains_key(k) ==> #[trigger] s.store[k] < s.next
    &&& forall|i: int, j: int|
        0 <= i < j < s.timeline.len() ==> (#[trigger] s.timeline[i]).id != (#[trigger] s.timeline[j]).id
    &&& forall|i: int|
        0 <= i < s.timeline.len() && (#[trigger] s.timeline[i]).share is Some ==> s.store.contains_key(
            s.timeline[i].share->0,
        ) && s.timeline[i].id == s.store[s.timeline[i].share->0]
}

pub open spec fn initial() -> Server {
    Server { store: Map::empty(), timeline: Seq::empty(), next: 0 }
}

pub open spec fn store_grows(s: Server, t: Server) -> bool {
    forall|k: nat| s.store.contains_key(k) ==> #[trigger] t.store.contains_key(k) && t.store[k] == s.store[k]
}

proof fn lemma_ensure_user(s: Server, share: nat)
    requires
        inv(s),
        s.store.contains_key(share),
    ensures
        inv(ensure_user(s, share)),
        ensure_user(s, share).store == s.store,
        ensure_user(s, share).next == s.next,
        has_item(ensure_user(s, share).timeline, s.store[share]),
{
    let id = s.store[share];
    if !has_item(s.timeline, id) {
        let t = ensure_user(s, share).timeline;
        let n = s.timeline.len() as int;
        assert(t[n].id == id);
        assert forall|i: int, j: int| 0 <= i < j < t.len() implies (#[trigger] t[i]).id != (#[trigger] t[j]).id by {
            if j == n {
                assert(t[i] == s.timeline[i]);
            } else {
                assert(t[i] == s.timeline[i] && t[j] == s.timeline[j]);
            }
        }
        assert forall|i: int| 0 <= i < t.len() implies (#[trigger] t[i]).id < s.next by {
            if i < n {
                assert(t[i] == s.timeline[i]);
            }
        }
        assert forall|i: int| 0 <= i < t.len() && (#[trigger] t[i]).share is Some implies s.store.contains_key(
            t[i].share->0,
        ) && t[i].id == s.store[t[i].share->0] by {
            if i < n {
                assert(t[i] == s.timeline[i]);
            }
        }
    }
}

/// SK2.1: every step keeps the invariant and never changes a saved userItemId; a response
/// carries the saved one.
pub proof fn receive_preserves_inv(s: Server, e: Event)
    requires
        inv(s),
    ensures
        inv(apply(s, e).0),
        store_grows(s, apply(s, e).0),
        apply(s, e).1 matches Some(r) ==> apply(s, e).0.store.contains_key(r.share)
            && apply(s, e).0.store[r.share] == r.user_item,
{
    match e {
        Event::Receive { share, crash } => {
            if !s.store.contains_key(share) {
                let id = s.next;
                let s1 = Server { next: s.next + 1, ..s };
                assert(inv(s1));
                if crash != Crash::BeforeSave {
                    let s2 = Server { store: s1.store.insert(share, id), ..s1 };
                    assert(inv(s2)) by {
                        assert forall|k: nat| s2.store.contains_key(k) implies #[trigger] s2.store[k] < s2.next by {
                            if k != share {
                                assert(s.store.contains_key(k));
                            }
                        }
                        assert forall|i: int|
                            0 <= i < s2.timeline.len() && (#[trigger] s2.timeline[i]).share is Some implies s2.store.contains_key(
                            s2.timeline[i].share->0,
                        ) && s2.timeline[i].id == s2.store[s2.timeline[i].share->0] by {
                            // no item names a share that was not saved
                            assert(s.store.contains_key(s.timeline[i].share->0));
                            assert(s2.timeline[i].share->0 != share);
                        }
                    }
                    assert(store_grows(s, s2));
                    if crash == Crash::None {
                        lemma_ensure_user(s2, share);
                    }
                }
            }
        },
        Event::Recover { share } => {
            if s.store.contains_key(share) {
                lemma_ensure_user(s, share);
            }
        },
        Event::Other { role, reply_to } => {
            let t = apply(s, e).0.timeline;
            let n = s.timeline.len() as int;
            assert forall|i: int, j: int| 0 <= i < j < t.len() implies (#[trigger] t[i]).id != (#[trigger] t[j]).id by {
                if j == n {
                    assert(t[i] == s.timeline[i]);
                } else {
                    assert(t[i] == s.timeline[i] && t[j] == s.timeline[j]);
                }
            }
            assert forall|i: int| 0 <= i < t.len() implies (#[trigger] t[i]).id < s.next + 1 by {
                if i < n {
                    assert(t[i] == s.timeline[i]);
                }
            }
            assert forall|i: int| 0 <= i < t.len() && (#[trigger] t[i]).share is Some implies s.store.contains_key(
                t[i].share->0,
            ) && t[i].id == s.store[t[i].share->0] by {
                assert(i < n);
                assert(t[i] == s.timeline[i]);
            }
        },
    }
}

/// Run a trace; collect the responses.
pub open spec fn run(s: Server, es: Seq<Event>) -> (Server, Seq<Response>)
    decreases es.len(),
{
    if es.len() == 0 {
        (s, Seq::empty())
    } else {
        let (s1, r) = run(s, es.drop_last());
        let (s2, o) = apply(s1, es.last());
        match o {
            Some(x) => (s2, r.push(x)),
            None => (s2, r),
        }
    }
}

/// SK2.1 + SK2.2 over a trace.
pub proof fn store_is_stable(s: Server, es: Seq<Event>)
    requires
        inv(s),
    ensures
        inv(run(s, es).0),
        store_grows(s, run(s, es).0),
        forall|i: int| #![auto] 0 <= i < run(s, es).1.len() ==> run(s, es).0.store.contains_key(run(s, es).1[i].share)
            && run(s, es).0.store[run(s, es).1[i].share] == run(s, es).1[i].user_item,
    decreases es.len(),
{
    if es.len() > 0 {
        store_is_stable(s, es.drop_last());
        let (s1, r) = run(s, es.drop_last());
        receive_preserves_inv(s1, es.last());
        let (s2, o) = apply(s1, es.last());
        let out = run(s, es).1;
        assert forall|i: int| #![auto] 0 <= i < out.len() implies s2.store.contains_key(out[i].share)
            && s2.store[out[i].share] == out[i].user_item by {
            if i < r.len() {
                assert(out[i] == r[i]);
                assert(s1.store.contains_key(r[i].share));
            }
        }
    }
}

/// SK2.2: all responses for one shareId carry the same userItemId.
pub proof fn responses_agree(es: Seq<Event>, i: int, j: int)
    requires
        0 <= i < run(initial(), es).1.len(),
        0 <= j < run(initial(), es).1.len(),
        run(initial(), es).1[i].share == run(initial(), es).1[j].share,
    ensures
        run(initial(), es).1[i].user_item == run(initial(), es).1[j].user_item,
{
    store_is_stable(initial(), es);
}

/// SK2.3: at most one timeline item per share, carrying the saved userItemId.
pub proof fn one_user_item_per_share(es: Seq<Event>, i: int, j: int, share: nat)
    requires
        0 <= i < run(initial(), es).0.timeline.len(),
        0 <= j < run(initial(), es).0.timeline.len(),
        run(initial(), es).0.timeline[i].share == Some(share),
        run(initial(), es).0.timeline[j].share == Some(share),
    ensures
        i == j,
        run(initial(), es).0.timeline[i].id == run(initial(), es).0.store[share],
{
    store_is_stable(initial(), es);
    let tl = run(initial(), es).0.timeline;
    if i < j {
        assert(tl[i].id != tl[j].id);
    } else if j < i {
        assert(tl[j].id != tl[i].id);
    }
}

/// SK2.3 witness (not vacuous): a crash after the save, a retry (duplicate 202 with the
/// saved id), then recovery appends the one user item.
pub proof fn witness_crash_after_save_recovers()
    ensures
        ({
            let es = seq![
                Event::Receive { share: 7, crash: Crash::AfterSave },
                Event::Receive { share: 7, crash: Crash::None },
                Event::Recover { share: 7 },
            ];
            let (s, r) = run(initial(), es);
            &&& r.len() == 1 && r[0].duplicate && r[0].user_item == 0
            &&& s.timeline.len() == 1 && s.timeline[0].id == 0 && s.timeline[0].share == Some(7nat)
        }),
{
    let es = seq![
        Event::Receive { share: 7, crash: Crash::AfterSave },
        Event::Receive { share: 7, crash: Crash::None },
        Event::Recover { share: 7 },
    ];
    let s0 = initial();
    let e0 = Seq::<Event>::empty();
    let e1 = es.drop_last().drop_last();
    let e2 = es.drop_last();
    assert(e1.drop_last() =~= e0);
    assert(e2.drop_last() =~= e1);
    assert(e1.last() == es[0]);
    assert(e2.last() == es[1]);
    assert(es.last() == es[2]);
    assert(run(s0, e0) == (s0, Seq::<Response>::empty()));
    let s1 = apply(s0, es[0]).0;
    assert(run(s0, e1) == (s1, Seq::<Response>::empty()));
    assert(s1.store.contains_key(7) && s1.store[7] == 0 && s1.timeline.len() == 0 && s1.next == 1);
    let (s2, o2) = apply(s1, es[1]);
    assert(s2 == s1);
    assert(o2 == Some(Response { share: 7, user_item: 0, duplicate: true }));
    assert(run(s0, e2) == (s1, seq![Response { share: 7, user_item: 0, duplicate: true }]));
    assert(!has_item(s1.timeline, 0));
    let s3 = apply(s1, es[2]).0;
    assert(s3.timeline =~= seq![Entry { id: 0, share: Some(7), role: Role::User, reply_to: None }]);
}

// ================================================================ SK2.4: chat bubble

pub struct Bubble {
    /// meta.shareId (the optimistic bubble knows it; a timeline item carries it)
    pub share: Option<nat>,
    /// the server item id once known
    pub item: Option<nat>,
}

pub enum Ev {
    /// a timeline item (SSE, list reload; may repeat)
    Item { id: nat, share: Option<nat> },
    /// the 202 or a status poll: userItemId of the share (may repeat)
    Ack { user_item: nat },
}

pub open spec fn matches(b: Bubble, id: nat, share: Option<nat>, by_share: bool) -> bool {
    b.item == Some(id) || (by_share && share is Some && b.share == share)
}

pub open spec fn first_match(bs: Seq<Bubble>, id: nat, share: Option<nat>, by_share: bool) -> int
    decreases bs.len(),
{
    if bs.len() == 0 {
        -1
    } else {
        let k = first_match(bs.drop_last(), id, share, by_share);
        if k >= 0 {
            k
        } else if matches(bs.last(), id, share, by_share) {
            bs.len() - 1
        } else {
            -1
        }
    }
}

pub open spec fn on_item(bs: Seq<Bubble>, id: nat, share: Option<nat>, by_share: bool) -> Seq<Bubble> {
    let k = first_match(bs, id, share, by_share);
    if k >= 0 {
        bs.update(k, Bubble { share: bs[k].share, item: Some(id) })
    } else {
        bs.push(Bubble { share, item: Some(id) })
    }
}

/// adopt(): the share's own bubble (index 0) takes the server id.
pub open spec fn on_ack(bs: Seq<Bubble>, u: nat) -> Seq<Bubble> {
    bs.update(0, Bubble { share: bs[0].share, item: Some(u) })
}

pub open spec fn step_client(bs: Seq<Bubble>, e: Ev, by_share: bool) -> Seq<Bubble> {
    match e {
        Ev::Item { id, share } => on_item(bs, id, share, by_share),
        Ev::Ack { user_item } => on_ack(bs, user_item),
    }
}

pub open spec fn run_client(bs: Seq<Bubble>, es: Seq<Ev>, by_share: bool) -> Seq<Bubble>
    decreases es.len(),
{
    if es.len() == 0 {
        bs
    } else {
        step_client(run_client(bs, es.drop_last(), by_share), es.last(), by_share)
    }
}

/// A bubble shows the share s (user item u).
pub open spec fn for_share(b: Bubble, s: nat, u: nat) -> bool {
    b.share == Some(s) || b.item == Some(u)
}

/// What the server sends (SK2.2/SK2.3): the share's item is (u, s); every other item has
/// another id and another share; every ack says u.
pub open spec fn env_ok(e: Ev, s: nat, u: nat) -> bool {
    match e {
        Ev::Item { id, share } => (id == u && share == Some(s)) || (id != u && share != Some(s)),
        Ev::Ack { user_item } => user_item == u,
    }
}

pub open spec fn one_bubble(bs: Seq<Bubble>, s: nat, u: nat) -> bool {
    &&& bs.len() > 0
    &&& bs[0].share == Some(s)
    &&& (bs[0].item is None || bs[0].item == Some(u))
    &&& forall|i: int| 1 <= i < bs.len() ==> !for_share(#[trigger] bs[i], s, u)
}

proof fn lemma_first_match(bs: Seq<Bubble>, id: nat, share: Option<nat>, by_share: bool)
    ensures
        -1 <= first_match(bs, id, share, by_share) < bs.len(),
        first_match(bs, id, share, by_share) >= 0 ==> matches(
            bs[first_match(bs, id, share, by_share)],
            id,
            share,
            by_share,
        ),
        forall|i: int|
            0 <= i < bs.len() && (first_match(bs, id, share, by_share) < 0 || i < first_match(
                bs,
                id,
                share,
                by_share,
            )) ==> !matches(#[trigger] bs[i], id, share, by_share),
    decreases bs.len(),
{
    if bs.len() > 0 {
        let d = bs.drop_last();
        lemma_first_match(d, id, share, by_share);
        assert forall|i: int|
            0 <= i < bs.len() && (first_match(bs, id, share, by_share) < 0 || i < first_match(
                bs,
                id,
                share,
                by_share,
            )) implies !matches(#[trigger] bs[i], id, share, by_share) by {
            if i < d.len() {
                assert(bs[i] == d[i]);
            }
        }
    }
}

proof fn lemma_client_step(bs: Seq<Bubble>, e: Ev, s: nat, u: nat)
    requires
        one_bubble(bs, s, u),
        env_ok(e, s, u),
    ensures
        one_bubble(step_client(bs, e, true), s, u),
{
    match e {
        Ev::Item { id, share } => {
            lemma_first_match(bs, id, share, true);
            let k = first_match(bs, id, share, true);
            let out = on_item(bs, id, share, true);
            if id == u {
                // matched by meta.shareId at the share's own bubble
                assert(matches(bs[0], id, share, true));
                assert(k == 0);
                assert forall|i: int| 1 <= i < out.len() implies !for_share(#[trigger] out[i], s, u) by {
                    assert(out[i] == bs[i]);
                }
            } else {
                assert(!matches(bs[0], id, share, true));
                if k >= 0 {
                    assert(k >= 1);
                    assert forall|i: int| 1 <= i < out.len() implies !for_share(#[trigger] out[i], s, u) by {
                        if i != k {
                            assert(out[i] == bs[i]);
                        }
                    }
                } else {
                    assert forall|i: int| 1 <= i < out.len() implies !for_share(#[trigger] out[i], s, u) by {
                        if i < bs.len() {
                            assert(out[i] == bs[i]);
                        }
                    }
                }
            }
        },
        Ev::Ack { user_item } => {
            let out = on_ack(bs, user_item);
            assert forall|i: int| 1 <= i < out.len() implies !for_share(#[trigger] out[i], s, u) by {
                assert(out[i] == bs[i]);
            }
        },
    }
}

/// SK2.4: from the optimistic bubble (plus any earlier bubbles of other messages, which
/// never show this share), any order and repetition of items, 202s and status polls
/// leaves exactly one bubble for the share.
pub proof fn one_bubble_per_share(bs: Seq<Bubble>, es: Seq<Ev>, s: nat, u: nat)
    requires
        one_bubble(bs, s, u),
        forall|i: int| 0 <= i < es.len() ==> env_ok(#[trigger] es[i], s, u),
    ensures
        one_bubble(run_client(bs, es, true), s, u),
    decreases es.len(),
{
    if es.len() > 0 {
        let d = es.drop_last();
        assert forall|i: int| 0 <= i < d.len() implies env_ok(#[trigger] d[i], s, u) by {
            assert(d[i] == es[i]);
        }
        one_bubble_per_share(bs, d, s, u);
        lemma_client_step(run_client(bs, d, true), es.last(), s, u);
    }
}

/// The bug of an id-only matcher: the item arrives before the 202 -> two bubbles.
pub proof fn unfixed_match_by_id_only()
    ensures
        ({
            let start = seq![Bubble { share: Some(7), item: None }];
            let es = seq![Ev::Item { id: 3, share: Some(7) }, Ev::Ack { user_item: 3 }];
            let out = run_client(start, es, false);
            &&& out.len() == 2
            &&& for_share(out[0], 7, 3) && for_share(out[1], 7, 3)
            &&& one_bubble(run_client(start, es, true), 7, 3)
        }),
{
    let start = seq![Bubble { share: Some(7), item: None }];
    let es = seq![Ev::Item { id: 3, share: Some(7) }, Ev::Ack { user_item: 3 }];
    let e1 = es.drop_last();
    assert(e1.drop_last() =~= Seq::<Ev>::empty());
    assert(e1.last() == es[0]);
    assert(es.last() == es[1]);
    assert(start.drop_last() =~= Seq::<Bubble>::empty());
    assert(run_client(start, Seq::<Ev>::empty(), false) == start);
    let a = run_client(start, e1, false);
    assert(first_match(Seq::<Bubble>::empty(), 3, Some(7), false) == -1);
    assert(!matches(start.last(), 3, Some(7), false));
    assert(first_match(start, 3, Some(7), false) == -1);
    assert(a == start.push(Bubble { share: Some(7), item: Some(3) }));
    let b = run_client(start, es, false);
    assert(b == on_ack(a, 3));
    // the fixed matcher
    assert(one_bubble(start, 7, 3)) by {
        assert forall|i: int| 1 <= i < start.len() implies !for_share(#[trigger] start[i], 7, 3) by {}
    }
    assert forall|i: int| 0 <= i < es.len() implies env_ok(#[trigger] es[i], 7, 3) by {}
    one_bubble_per_share(start, es, 7, 3);
}

// ================================================================ SK3: answer once

pub open spec fn is_reply(e: Entry, u: nat) -> bool {
    e.role == Role::Lucina && e.reply_to == Some(u)
}

pub open spec fn replies(tl: Seq<Entry>, u: nat) -> nat
    decreases tl.len(),
{
    if tl.len() == 0 {
        0
    } else {
        replies(tl.drop_last(), u) + if is_reply(tl.last(), u) {
            1nat
        } else {
            0
        }
    }
}

/// existing_reply since 485d658: any item, newest first.
pub open spec fn full_scan(tl: Seq<Entry>, u: nat) -> bool {
    exists|i: int| 0 <= i < tl.len() && is_reply(#[trigger] tl[i], u)
}

/// existing_reply before 485d658: the newest `w` items only.
pub open spec fn window_scan(tl: Seq<Entry>, u: nat, w: nat) -> bool {
    exists|i: int| tl.len() - w <= i < tl.len() && 0 <= i && is_reply(#[trigger] tl[i], u)
}

pub enum Step3 {
    /// process(): scan, else write one reply; `crash` = no status.json "done" after it
    Process { crash: bool },
    /// another writer; never a reply to this share's user item
    Other { e: Entry },
}

pub struct Job {
    pub done: bool,
    pub timeline: Seq<Entry>,
    pub next: nat,
}

pub open spec fn process(j: Job, u: nat, crash: bool, window: Option<nat>) -> Job {
    let found = match window {
        None => full_scan(j.timeline, u),
        Some(w) => window_scan(j.timeline, u, w),
    };
    if j.done {
        j
    } else if found {
        Job { done: true, ..j }
    } else {
        let t = j.timeline.push(Entry { id: j.next, share: None, role: Role::Lucina, reply_to: Some(u) });
        Job { done: !crash, timeline: t, next: j.next + 1 }
    }
}

pub open spec fn step3(j: Job, u: nat, st: Step3, window: Option<nat>) -> Job {
    match st {
        Step3::Process { crash } => process(j, u, crash, window),
        Step3::Other { e } => Job { timeline: j.timeline.push(e), next: j.next + 1, ..j },
    }
}

pub open spec fn run3(j: Job, u: nat, sts: Seq<Step3>, window: Option<nat>) -> Job
    decreases sts.len(),
{
    if sts.len() == 0 {
        j
    } else {
        step3(run3(j, u, sts.drop_last(), window), u, sts.last(), window)
    }
}

pub open spec fn others_ok(sts: Seq<Step3>, u: nat) -> bool {
    forall|i: int| 0 <= i < sts.len() ==> (#[trigger] sts[i] matches Step3::Other { e } ==> !is_reply(e, u))
}

proof fn lemma_replies_push(tl: Seq<Entry>, e: Entry, u: nat)
    ensures
        replies(tl.push(e), u) == replies(tl, u) + if is_reply(e, u) {
            1nat
        } else {
            0
        },
{
    assert(tl.push(e).drop_last() =~= tl);
}

proof fn lemma_scan_count(tl: Seq<Entry>, u: nat)
    ensures
        full_scan(tl, u) <==> replies(tl, u) > 0,
    decreases tl.len(),
{
    if tl.len() > 0 {
        let d = tl.drop_last();
        lemma_scan_count(d, u);
        if full_scan(d, u) {
            let i = choose|i: int| 0 <= i < d.len() && is_reply(#[trigger] d[i], u);
            assert(tl[i] == d[i]);
        }
        if full_scan(tl, u) && !is_reply(tl.last(), u) {
            let i = choose|i: int| 0 <= i < tl.len() && is_reply(#[trigger] tl[i], u);
            assert(i < d.len());
            assert(d[i] == tl[i]);
        }
        if is_reply(tl.last(), u) {
            assert(is_reply(tl[tl.len() - 1], u));
        }
    }
}

/// SK3.1: with the full scan, at most one reply, over any trace of crashes and other
/// writers.
pub proof fn answered_at_most_once(j: Job, u: nat, sts: Seq<Step3>)
    requires
        replies(j.timeline, u) <= 1,
        others_ok(sts, u),
    ensures
        replies(run3(j, u, sts, None).timeline, u) <= 1,
    decreases sts.len(),
{
    if sts.len() > 0 {
        let d = sts.drop_last();
        assert(others_ok(d, u)) by {
            assert forall|i: int| 0 <= i < d.len() implies (#[trigger] d[i] matches Step3::Other { e } ==> !is_reply(
                e,
                u,
            )) by {
                assert(d[i] == sts[i]);
            }
        }
        answered_at_most_once(j, u, d);
        let j1 = run3(j, u, d, None);
        lemma_scan_count(j1.timeline, u);
        match sts.last() {
            Step3::Process { crash } => {
                let r = Entry { id: j1.next, share: None, role: Role::Lucina, reply_to: Some(u) };
                lemma_replies_push(j1.timeline, r, u);
            },
            Step3::Other { e } => {
                assert(sts[sts.len() - 1] == sts.last());
                lemma_replies_push(j1.timeline, e, u);
            },
        }
    }
}

/// The items a voice note can get. Text, meta.transcript and meta.voiceReply do not take
/// part in recovery: existing_reply matches role and replyTo only.
pub enum VoiceOutcome {
    /// the answer to the on-device transcript
    Device,
    /// the answer to the whisper transcript
    Server,
    /// "couldn't hear it" (share::process, `reply_to: Some(job.user_item.id)`)
    Unheard,
    /// a tools-planner progress item (on_tool: TimelineTurn::record_lucina)
    ToolProgress,
}

/// The item each outcome appends: every path writes role lucina, replyTo = u.
pub open spec fn voice_reply(id: nat, u: nat, outcome: VoiceOutcome) -> Entry {
    match outcome {
        VoiceOutcome::Device
        | VoiceOutcome::Server
        | VoiceOutcome::Unheard
        | VoiceOutcome::ToolProgress => Entry { id, share: None, role: Role::Lucina, reply_to: Some(u) },
    }
}

/// SK3.3: every voice outcome is found on restart, including "couldn't hear it" (and a
/// progress item, which is why tools_turn suppresses them for share turns).
pub proof fn voice_recovery_once(j: Job, u: nat, outcome: VoiceOutcome)
    requires !j.done, replies(j.timeline, u) == 0,
    ensures
        replies(j.timeline.push(voice_reply(j.next, u, outcome)), u) == 1,
        process(Job { timeline: j.timeline.push(voice_reply(j.next, u, outcome)),
            next: j.next + 1, ..j }, u, false, None).timeline
            == j.timeline.push(voice_reply(j.next, u, outcome)),
        process(Job { timeline: j.timeline.push(voice_reply(j.next, u, outcome)),
            next: j.next + 1, ..j }, u, false, None).done,
{
    let t = j.timeline.push(voice_reply(j.next, u, outcome));
    lemma_replies_push(j.timeline, voice_reply(j.next, u, outcome), u);
    lemma_scan_count(t, u);
}

pub open spec fn filler(i: int) -> Entry {
    Entry { id: (i + 2) as nat, share: None, role: Role::User, reply_to: None }
}

/// The bug before 485d658: user item, reply, 500 other items; the restarted job scans the
/// newest 500 and answers again.
pub proof fn unfixed_window_scan()
    ensures
        ({
            let u = 0nat;
            let tl = seq![
                Entry { id: 0, share: Some(9), role: Role::User, reply_to: None },
                Entry { id: 1, share: None, role: Role::Lucina, reply_to: Some(0) },
            ] + Seq::new(500, |i: int| filler(i));
            let j = Job { done: false, timeline: tl, next: 502 };
            &&& replies(tl, u) == 1
            &&& replies(process(j, u, false, Some(500)).timeline, u) == 2
            &&& replies(process(j, u, false, None).timeline, u) == 1
        }),
{
    let u = 0nat;
    let head = seq![
        Entry { id: 0, share: Some(9), role: Role::User, reply_to: None },
        Entry { id: 1, share: None, role: Role::Lucina, reply_to: Some(0) },
    ];
    let tail = Seq::new(500, |i: int| filler(i));
    let tl = head + tail;
    assert(!window_scan(tl, u, 500)) by {
        assert forall|i: int| tl.len() - 500 <= i < tl.len() && 0 <= i implies !is_reply(#[trigger] tl[i], u) by {
            assert(tl[i] == tail[i - 2]);
        }
    }
    assert(is_reply(tl[1], u));
    assert(full_scan(tl, u));
    lemma_scan_count(tl, u);
    // exactly one reply in tl
    assert(replies(tl, u) == 1) by {
        lemma_replies_split(head, tail, u);
        lemma_no_replies(tail, u);
        let h1 = head.drop_last();
        assert(h1.drop_last() =~= Seq::<Entry>::empty());
        assert(h1.last() == head[0]);
        assert(replies(Seq::<Entry>::empty(), u) == 0);
        assert(replies(h1, u) == 0);
        assert(replies(head, u) == 1);
    }
    let r = Entry { id: 502, share: None, role: Role::Lucina, reply_to: Some(u) };
    lemma_replies_push(tl, r, u);
}

proof fn lemma_replies_split(a: Seq<Entry>, b: Seq<Entry>, u: nat)
    ensures
        replies(a + b, u) == replies(a, u) + replies(b, u),
    decreases b.len(),
{
    if b.len() > 0 {
        lemma_replies_split(a, b.drop_last(), u);
        assert((a + b).drop_last() =~= a + b.drop_last());
        assert((a + b).last() == b.last());
    } else {
        assert(a + b =~= a);
    }
}

proof fn lemma_no_replies(b: Seq<Entry>, u: nat)
    requires
        forall|i: int| 0 <= i < b.len() ==> !is_reply(#[trigger] b[i], u),
    ensures
        replies(b, u) == 0,
    decreases b.len(),
{
    if b.len() > 0 {
        let d = b.drop_last();
        assert forall|i: int| 0 <= i < d.len() implies !is_reply(#[trigger] d[i], u) by {
            assert(d[i] == b[i]);
        }
        lemma_no_replies(d, u);
        assert(!is_reply(b[b.len() - 1], u));
    }
}

/// SK3.2: the raw-line prefilter. `mentions(e, u)` = the item's raw JSON contains u; a
/// reply to u always does (its "replyTo" field).
pub proof fn prefilter_is_sound(tl: Seq<Entry>, u: nat, mentions: spec_fn(Entry, nat) -> bool)
    requires
        forall|e: Entry| #[trigger] is_reply(e, u) ==> mentions(e, u),
    ensures
        full_scan(tl, u) <==> exists|i: int| 0 <= i < tl.len() && mentions(tl[i], u) && #[trigger] is_reply(
            tl[i],
            u,
        ),
{
    if full_scan(tl, u) {
        let i = choose|i: int| 0 <= i < tl.len() && is_reply(#[trigger] tl[i], u);
        assert(mentions(tl[i], u));
    }
}

} // verus!
fn main() {}
