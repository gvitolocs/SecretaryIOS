// Verus-verified kernels of the media player (docs/verification.md, protocol 10):
//
//   MP1 resume positions, last writer wins (`crates/secretary-player/src/progress.rs`
//       `Entry::is_newer_than`, `Book::merge`, `Book::acknowledge`; the server's
//       `progress::Store::put` in `crates/secretary-media-stream`): the order is a strict
//       total order, a device never adopts a stale entry, the server keeps the newest
//       write and sends back what is newer, an acknowledgement never clears a later
//       write, and merging is commutative, associative and idempotent (every device
//       ends on the same entry whatever order the updates arrive in).
//       A new write is stamped after the entry the device holds (fix R2, `write_stamp`).
//   MP2 positions: clamped to the duration; the resume point and the "finished" margin.
//   MP3 play tokens (`crates/secretary-media-stream/src/token.rs`): the expiry encoding
//       round-trips (base 36), and `verify` accepts only an untampered, unexpired token
//       of the right length.
//   MP4 the subtitle choice per film (`crates/secretary-player/src/prefs.rs`): the
//       owner's choice for a film always wins, "off" stays off, other films keep
//       theirs, and the language default picks the best matching track.
//   MP5 the C/wasm ABI wrappers (`crates/secretary-ffi`, `crates/secretary-player-wasm`):
//       every request path returns a JSON answer (bridge_error unless the engine ran),
//       and the wasm answer pointer/length pack round-trips exactly when both fit in
//       32 bits (wasm32), which is why the native 64-bit test cannot use it.
//
// Device names are abstracted as numbers with the same order as the strings.

use vstd::arithmetic::div_mod::*;
use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------------------
// MP1: last writer wins
// ---------------------------------------------------------------------------

/// `(updated_at, device)`: the order of `Entry::is_newer_than`.
#[derive(Clone, Copy)]
pub struct Stamp {
    pub at: u64,
    pub dev: u64,
}

#[derive(Clone, Copy)]
pub struct Pos {
    pub position: u64,
    pub duration: u64,
    pub stamp: Stamp,
}

pub open spec fn newer(a: Stamp, b: Stamp) -> bool {
    a.at > b.at || (a.at == b.at && a.dev > b.dev)
}

/// `Entry::is_newer_than`.
pub fn is_newer_than(a: &Stamp, b: &Stamp) -> (r: bool)
    ensures
        r == newer(*a, *b),
{
    a.at > b.at || (a.at == b.at && a.dev > b.dev)
}

/// A strict total order: one winner for any two different writes.
pub proof fn lemma_order(a: Stamp, b: Stamp, c: Stamp)
    ensures
        !newer(a, a),
        newer(a, b) ==> !newer(b, a),
        newer(a, b) && newer(b, c) ==> newer(a, c),
        (a.at != b.at || a.dev != b.dev) ==> (newer(a, b) || newer(b, a)),
{
}

/// The entry that wins between two.
pub open spec fn latest(a: Pos, b: Pos) -> Pos {
    if newer(b.stamp, a.stamp) {
        b
    } else {
        a
    }
}

/// One write per stamp: two entries with the same stamp are the same write.
pub open spec fn one_write(a: Pos, b: Pos) -> bool {
    (a.stamp.at == b.stamp.at && a.stamp.dev == b.stamp.dev) ==> a == b
}

/// Merging is a semilattice join: the order of arrival does not matter.
pub proof fn lemma_merge_converges(a: Pos, b: Pos, c: Pos)
    requires
        one_write(a, b),
        one_write(b, c),
        one_write(a, c),
    ensures
        latest(a, b) == latest(b, a),
        latest(latest(a, b), c) == latest(a, latest(b, c)),
        latest(a, a) == a,
{
    lemma_order(a.stamp, b.stamp, c.stamp);
    lemma_order(b.stamp, c.stamp, a.stamp);
    lemma_order(c.stamp, a.stamp, b.stamp);
    lemma_order(a.stamp, c.stamp, b.stamp);
    lemma_order(b.stamp, a.stamp, c.stamp);
    lemma_order(c.stamp, b.stamp, a.stamp);
}

/// `clamp_position`.
pub fn clamp_position(position: u64, duration: u64) -> (r: u64)
    ensures
        r == clamp_spec(position, duration),
        duration > 0 ==> r <= duration && r <= position && (position <= duration ==> r == position),
        duration == 0 ==> r == position,
{
    if duration > 0 && position > duration {
        duration
    } else {
        position
    }
}

/// `Book::merge` for one entry: the remote entry replaces the local one only when it is
/// newer, and it is clamped on the way in. Returns (entry, changed, dirty).
pub fn merge_entry(local: Option<Pos>, dirty: bool, remote: Pos) -> (r: (Option<Pos>, bool, bool))
    ensures
        // The result is the newer of the two.
        local.is_none() ==> r.1 && r.0 == Some(Pos { position: clamp_spec(remote.position, remote.duration), ..remote }),
        local.is_some() && newer(remote.stamp, local.unwrap().stamp) ==> r.1 && !r.2 && r.0
            == Some(Pos { position: clamp_spec(remote.position, remote.duration), ..remote }),
        // Never back to a stale entry: an older (or the same) write leaves everything as it was.
        local.is_some() && !newer(remote.stamp, local.unwrap().stamp) ==> !r.1 && r.0 == local && r.2 == dirty,
        // The stamp never goes back.
        local.is_some() ==> !newer(local.unwrap().stamp, r.0.unwrap().stamp),
        r.0.is_some(),
{
    let take = match &local {
        None => true,
        Some(l) => is_newer_than(&remote.stamp, &l.stamp),
    };
    if take {
        let position = clamp_position(remote.position, remote.duration);
        (Some(Pos { position, duration: remote.duration, stamp: Stamp { at: remote.stamp.at, dev: remote.stamp.dev } }), true, false)
    } else {
        proof {
            lemma_order(local.unwrap().stamp, local.unwrap().stamp, local.unwrap().stamp);
        }
        (local, false, dirty)
    }
}

pub open spec fn clamp_spec(position: u64, duration: u64) -> u64 {
    if duration > 0 && position > duration {
        duration
    } else {
        position
    }
}

/// The server's `Store::put` for one entry: store the incoming write if it is newer, and
/// send back the stored entry when it is newer than the incoming one (the client merges it).
pub fn server_put(current: Option<Pos>, incoming: Pos) -> (r: (Pos, Option<Pos>))
    ensures
        current.is_none() ==> r.0 == incoming && r.1.is_none(),
        current.is_some() ==> r.0 == latest(current.unwrap(), incoming),
        current.is_some() ==> (r.1.is_some() <==> newer(current.unwrap().stamp, incoming.stamp)),
        r.1.is_some() ==> r.1.unwrap() == r.0,
        // The stored stamp never goes back.
        current.is_some() ==> !newer(current.unwrap().stamp, r.0.stamp),
{
    match current {
        None => (incoming, None),
        Some(cur) => {
            if is_newer_than(&incoming.stamp, &cur.stamp) {
                proof { lemma_order(incoming.stamp, cur.stamp, cur.stamp); }
                (incoming, None)
            } else if is_newer_than(&cur.stamp, &incoming.stamp) {
                (cur, Some(cur))
            } else {
                // Same stamp: the same write delivered twice.
                (cur, None)
            }
        },
    }
}

/// `write_stamp` (fix R2): a new write is stamped after the entry the device holds, so it
/// wins over everything the device has seen, whatever the clocks say.
pub fn write_stamp(now: u64, held: Option<u64>) -> (r: u64)
    requires
        held.is_some() ==> held.unwrap() < u64::MAX,
    ensures
        r >= now,
        held.is_some() ==> r > held.unwrap(),
        held.is_none() || held.unwrap() < now ==> r == now,
{
    match held {
        Some(at) => {
            if at >= now {
                at + 1
            } else {
                now
            }
        },
        None => now,
    }
}

/// So the next write of a device is newer than whatever entry it holds, for any device.
pub proof fn lemma_new_write_wins(now: u64, held: Stamp, me: u64)
    requires
        held.at < u64::MAX,
    ensures
        newer(Stamp { at: if held.at >= now { (held.at + 1) as u64 } else { now }, dev: me }, held),
{
}

/// `Book::acknowledge`: the server stored (or overruled) the write stamped `acked_at`.
/// The entry stays dirty when it changed after that write.
pub fn acknowledge(dirty: bool, local_at: u64, acked_at: u64) -> (r: bool)
    ensures
        r == (dirty && local_at > acked_at),
        // A later write is never cleared by the acknowledgement of an earlier one.
        dirty && local_at > acked_at ==> r,
{
    if local_at <= acked_at {
        false
    } else {
        dirty
    }
}

// ---------------------------------------------------------------------------
// MP2: positions, resume point, finished
// ---------------------------------------------------------------------------

pub open spec fn margin_spec(duration: u64) -> u64 {
    if duration / 20 < 2000 {
        2000
    } else if duration / 20 > 120_000 {
        120_000
    } else {
        duration / 20
    }
}

/// `finish_margin`: 5% of the item, between 2 s and 2 min.
pub fn finish_margin(duration: u64) -> (m: u64)
    ensures
        2000 <= m <= 120_000,
        m == margin_spec(duration),
{
    let m = duration / 20;
    if m < 2000 {
        2000
    } else if m > 120_000 {
        120_000
    } else {
        m
    }
}

/// `is_finished`.
pub fn is_finished(position: u64, duration: u64) -> (r: bool)
    ensures
        r == (duration > 0 && position as int + margin_spec(duration) as int >= duration as int),
        position >= duration && duration > 0 ==> r,
{
    duration > 0 && (position as u128 + finish_margin(duration) as u128 >= duration as u128)
}

/// `resume_point` (the item's own duration known): never past the saved position, 0 for
/// finished or barely started items, otherwise 2 s before the saved position.
pub fn resume_point(position: u64, duration: u64, finished: bool) -> (r: u64)
    ensures
        r <= position,
        finished ==> r == 0,
        duration > 0 && position as int + margin_spec(duration) as int >= duration as int ==> r == 0,
        r == 0 || r + 2000 == position,
        duration > 0 && position <= duration ==> r <= duration,
{
    if finished || is_finished(position, duration) {
        return 0;
    }
    let min = if duration > 0 {
        if duration / 10 < 10_000 { duration / 10 } else { 10_000 }
    } else {
        10_000
    };
    if position < min {
        0
    } else if position < 2000 {
        0
    } else {
        position - 2000
    }
}

// ---------------------------------------------------------------------------
// MP3: play tokens
// ---------------------------------------------------------------------------

/// The expiry as base-36 digits (most significant first), as `token::base36` writes it.
pub open spec fn enc36(n: nat) -> Seq<nat>
    decreases n,
{
    if n < 36 {
        seq![n]
    } else {
        enc36(n / 36).push(n % 36)
    }
}

/// `u64::from_str_radix(_, 36)` on those digits.
pub open spec fn dec36(s: Seq<nat>) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        dec36(s.drop_last()) * 36 + s.last()
    }
}

/// The expiry survives its encoding: a token is checked against the exact expiry it was signed with.
pub proof fn lemma_base36_round_trip(n: nat)
    ensures
        dec36(enc36(n)) == n,
        forall|i: int| 0 <= i < enc36(n).len() ==> enc36(n)[i] < 36,
    decreases n,
{
    if n < 36 {
        assert(enc36(n).drop_last() =~= Seq::<nat>::empty());
        assert(dec36(Seq::<nat>::empty()) == 0);
    } else {
        lemma_base36_round_trip(n / 36);
        lemma_fundamental_div_mod(n as int, 36);
        let s = enc36(n);
        assert(s == enc36(n / 36).push(n % 36));
        assert(s.drop_last() =~= enc36(n / 36));
        assert(s.last() == n % 36);
        assert(dec36(s) == dec36(s.drop_last()) * 36 + s.last());
        assert((n / 36) * 36 == 36 * (n / 36)) by (nonlinear_arith);
        assert(dec36(s) == (n / 36) * 36 + n % 36);
        assert forall|i: int| 0 <= i < s.len() implies s[i] < 36 by {
            if i < s.len() - 1 {
                assert(s[i] == enc36(n / 36)[i]);
            }
        }
    }
}

/// `token::verify` after parsing: the MAC comparison (constant time, abstracted as
/// `tag_matches`: HMAC-SHA256 over the id and the expiry) and the expiry check.
pub fn token_valid(expiry: u64, now: u64, tag_len: usize, tag_matches: bool) -> (r: bool)
    ensures
        r <==> (expiry >= now && tag_len == 16 && tag_matches),
        expiry < now ==> !r,
        !tag_matches ==> !r,
{
    if expiry < now || tag_len != 16 {
        return false;
    }
    tag_matches
}

/// `Signer::base`: the expiry is `now + ttl`, so a fresh token is valid for exactly the TTL.
pub proof fn lemma_fresh_token_window(signed_at: u64, ttl: u64, now: u64)
    requires
        signed_at as int + ttl as int <= u64::MAX,
    ensures
        (signed_at <= now && now as int <= signed_at as int + ttl as int) ==> (signed_at + ttl) as u64 >= now,
        now as int > signed_at as int + ttl as int ==> ((signed_at + ttl) as u64) < now,
{
}

// ---------------------------------------------------------------------------
// MP4: subtitle choice per film
// ---------------------------------------------------------------------------

pub struct Track {
    pub id: u64,
    /// The normalised language (`normalise_lang`), when known.
    pub lang: Option<u64>,
    pub forced: bool,
    pub image: bool,
}

#[derive(Clone, Copy)]
pub enum Choice {
    Off,
    Track(u64),
}

pub open spec fn has_track(tracks: Seq<Track>, id: u64) -> bool {
    exists|k: int| 0 <= k < tracks.len() && tracks[k].id == id
}

/// The language default's preference: full text tracks first (`(forced, image)` smallest).
pub open spec fn rank(t: Track) -> nat {
    (if t.forced { 2nat } else { 0nat }) + (if t.image { 1nat } else { 0nat })
}

pub open spec fn in_lang(t: Track, lang: Option<u64>) -> bool {
    lang.is_some() && t.lang == lang
}

/// `SubtitlePrefs::pick` (with the film's stored choice already looked up).
pub fn pick(choice: Option<Choice>, lang: Option<u64>, tracks: &Vec<Track>) -> (r: Option<u64>)
    ensures
        // Only tracks of this film are ever picked.
        r.is_some() ==> has_track(tracks@, r.unwrap()),
        // The owner's choice for this film wins.
        (choice is Some && choice.unwrap() is Off) ==> r.is_none(),
        choice.is_some() && choice.unwrap() is Track && has_track(tracks@, choice.unwrap()->Track_0)
            ==> r == Some(choice.unwrap()->Track_0),
        // Without one, the language default is the best-ranked track in that language.
        (choice.is_none() || (choice.unwrap() is Track && !has_track(tracks@, choice.unwrap()->Track_0)))
            && (exists|k: int| 0 <= k < tracks@.len() && in_lang(tracks@[k], lang))
            ==> r.is_some() && exists|k: int| 0 <= k < tracks@.len() && tracks@[k].id == r.unwrap()
                && in_lang(tracks@[k], lang)
                && forall|j: int| 0 <= j < tracks@.len() && in_lang(tracks@[j], lang) ==> rank(tracks@[k]) <= rank(tracks@[j]),
{
    match choice {
        Some(Choice::Off) => {
            return None;
        },
        Some(Choice::Track(id)) => {
            let mut i: usize = 0;
            while i < tracks.len()
                invariant
                    0 <= i <= tracks.len(),
                    forall|j: int| 0 <= j < i ==> tracks@[j].id != id,
                    choice is Some && choice.unwrap() is Track && choice.unwrap()->Track_0 == id,
                decreases tracks.len() - i,
            {
                if tracks[i].id == id {
                    proof {
                        assert(tracks@[i as int].id == id);
                        assert(choice.unwrap() is Track && choice.unwrap()->Track_0 == id);
                    }
                    return Some(id);
                }
                i += 1;
            }
            proof {
                assert(!has_track(tracks@, id)) by {
                    assert forall|k: int| 0 <= k < tracks@.len() implies tracks@[k].id != id by {}
                }
            }
        },
        None => {},
    }
    // Here: no stored choice, or one for a track the film no longer has.
    assert(choice is None || (choice.unwrap() is Track && !has_track(tracks@, choice.unwrap()->Track_0)));
    // Language default: the first track with the smallest rank (`min_by_key`).
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < tracks.len()
        invariant
            0 <= i <= tracks.len(),
            best.is_some() ==> best.unwrap() < i && in_lang(tracks@[best.unwrap() as int], lang),
            best.is_some() ==> forall|j: int| 0 <= j < i && in_lang(tracks@[j], lang)
                ==> rank(tracks@[best.unwrap() as int]) <= rank(tracks@[j]),
            best.is_none() ==> forall|j: int| 0 <= j < i ==> !in_lang(tracks@[j], lang),
            choice is None || (choice.unwrap() is Track && !has_track(tracks@, choice.unwrap()->Track_0)),
        decreases tracks.len() - i,
    {
        let t = &tracks[i];
        let m = lang.is_some() && t.lang.is_some() && t.lang.unwrap() == lang.unwrap();
        assert(m == in_lang(tracks@[i as int], lang));
        if m {
            let better = match best {
                None => true,
                Some(b) => {
                    let rb = (if tracks[b].forced { 2u64 } else { 0 }) + (if tracks[b].image { 1u64 } else { 0 });
                    let rt = (if t.forced { 2u64 } else { 0 }) + (if t.image { 1u64 } else { 0 });
                    assert(rb as nat == rank(tracks@[b as int]));
                    assert(rt as nat == rank(tracks@[i as int]));
                    rt < rb
                },
            };
            if better {
                best = Some(i);
            }
        }
        i += 1;
    }
    match best {
        Some(b) => {
            proof {
                assert(tracks@[b as int].id == tracks[b as int].id);
                assert(has_track(tracks@, tracks@[b as int].id));
            }
            return Some(tracks[b].id);
        },
        None => {},
    }
    // Else a forced track (lines in a foreign language), else off.
    let mut i: usize = 0;
    while i < tracks.len()
        invariant
            0 <= i <= tracks.len(),
            forall|j: int| 0 <= j < tracks@.len() ==> !in_lang(tracks@[j], lang),
            choice is None || (choice.unwrap() is Track && !has_track(tracks@, choice.unwrap()->Track_0)),
        decreases tracks.len() - i,
    {
        if tracks[i].forced {
            proof {
                assert(has_track(tracks@, tracks@[i as int].id));
            }
            return Some(tracks[i].id);
        }
        i += 1;
    }
    None
}

/// `SubtitlePrefs::choose`, on the per-film map.
pub open spec fn choose_track(per_film: Map<nat, Choice>, film: nat, track: Option<u64>) -> Map<nat, Choice> {
    per_film.insert(film, if track.is_some() { Choice::Track(track.unwrap()) } else { Choice::Off })
}

/// A choice is remembered for its film and only for its film.
pub proof fn lemma_choice_memory(per_film: Map<nat, Choice>, film: nat, other: nat, track: Option<u64>)
    requires
        other != film,
    ensures
        choose_track(per_film, film, track)[film] == (if track.is_some() { Choice::Track(track.unwrap()) } else { Choice::Off }),
        choose_track(per_film, film, track).dom().contains(other) == per_film.dom().contains(other),
        per_film.dom().contains(other) ==> choose_track(per_film, film, track)[other] == per_film[other],
{
}

// ---------------------------------------------------------------------------
// MP5: ABI wrappers
// ---------------------------------------------------------------------------

pub enum Answer {
    /// The engine handled the request.
    Engine,
    /// `{"ok":false,"code":"bridge_error",...}`.
    BridgeError,
}

/// `request_engine` in `secretary-ffi`: null handle or request, non-UTF-8 input, a
/// poisoned lock or a panic all become a bridge_error answer; only a clean run is the
/// engine's own answer. There is always an answer (the caller frees it).
pub fn request_outcome(handle_null: bool, json_null: bool, utf8_ok: bool, lock_ok: bool, panicked: bool) -> (r: Answer)
    ensures
        (r is Engine) <==> (!handle_null && !json_null && utf8_ok && lock_ok && !panicked),
{
    if panicked || handle_null || json_null || !utf8_ok || !lock_ok {
        Answer::BridgeError
    } else {
        Answer::Engine
    }
}

/// `give` in `secretary-player-wasm`: the answer's address and length in one u64.
pub fn pack(ptr: u64, len: u64) -> (r: u64)
    requires
        ptr < 0x1_0000_0000,
        len < 0x1_0000_0000,
    ensures
        r >> 32u64 == ptr,
        r & 0xffff_ffffu64 == len,
{
    let r = (ptr << 32u64) | len;
    assert(((ptr << 32u64) | len) >> 32u64 == ptr && ((ptr << 32u64) | len) & 0xffff_ffffu64 == len) by (bit_vector)
        requires
            ptr < 0x1_0000_0000,
            len < 0x1_0000_0000,
    ;
    r
}

/// Why the pack is wasm32-only: a 64-bit address loses its high half (the native
/// SIGSEGV that moved the crate's test off the packed ABI).
pub proof fn lemma_pack_needs_32_bit_addresses()
    ensures
        ((0x1_0000_0000u64 << 32u64) | 5u64) >> 32u64 != 0x1_0000_0000u64,
{
    assert(((0x1_0000_0000u64 << 32u64) | 5u64) >> 32u64 != 0x1_0000_0000u64) by (bit_vector);
}

} // verus!

fn main() {}
