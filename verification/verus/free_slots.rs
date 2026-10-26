// Free-slot kernel (secretary-calendar: the gaps Lucina offers when she proposes a time).
//
// Production (branch lucina-rs, crates/secretary-calendar/src/view.rs):
//   day_slots <-> the inner sweep of view::free_slots for one day: `busy` is the timed
//                 events (start_ms, end_ms) with end > start, sorted (sort_unstable on
//                 the pairs); `open` / `close` are that day's working hours clipped to
//                 [from, to); `need` = max(minutes, 1) in ms; `left` = max - slots so far.
//                 The order of the tests is the code's: cursor >= close -> stop;
//                 e <= cursor -> skip; s >= close -> stop; a gap s - cursor >= need ->
//                 slot; cursor = max(cursor, e); after the events, the tail
//                 (cursor, close) when long enough.
// Times are epoch milliseconds (DateTime<Utc> in the code, compared at ms precision:
// every value comes from_timestamp_millis). |t| < 2^60 keeps the subtractions in i64.
// The day loop (local midnight, working hours, DST via local_to_utc) is outside the
// kernel; the diff test runs it in UTC over several days.
// Differential test: crates/secretary-calendar/tests/verus_free_slots.rs.
//
// Proven (for every sorted, well-formed busy list):
//   FS1 within: every slot lies in [open, close] and is at least `need` long.
//   FS2 free: no slot overlaps any busy event.
//   FS3 ordered: slots are in time order and do not overlap each other.
//   FS4 bounded: at most `left` slots.
// Not proven: completeness (that no gap of `need` is missed); the diff test checks it
// on random inputs against a brute-force minute grid.
//
// No Stateright model: a pure function of one query.
use vstd::prelude::*;

verus! {

pub const BOUND: i64 = 0x1000_0000_0000_0000;

#[derive(Clone, Copy)]
pub struct Span {
    pub s: i64,
    pub e: i64,
}

pub open spec fn in_range(t: i64) -> bool {
    -BOUND < t < BOUND
}

pub open spec fn wf(busy: Seq<Span>) -> bool {
    &&& forall|k: int| 0 <= k < busy.len() ==> (#[trigger] busy[k]).s < busy[k].e && in_range(
        busy[k].s,
    ) && in_range(busy[k].e)
    &&& forall|j: int, k: int| 0 <= j <= k < busy.len() ==> (#[trigger] busy[j]).s <= (
    #[trigger] busy[k]).s
}

pub open spec fn disjoint(a: Span, b: Span) -> bool {
    a.e <= b.s || b.e <= a.s
}

pub open spec fn sound(out: Seq<Span>, busy: Seq<Span>, open: i64, close: i64, need: i64) -> bool {
    &&& forall|j: int|
        0 <= j < out.len() ==> open <= (#[trigger] out[j]).s && out[j].e <= close && out[j].e
            - out[j].s >= need
    &&& forall|j: int, k: int|
        0 <= j < out.len() && 0 <= k < busy.len() ==> disjoint(#[trigger] out[j], #[trigger] busy[k])
    &&& forall|j: int, k: int| 0 <= j < k < out.len() ==> (#[trigger] out[j]).e <= (#[trigger] out[k]).s

}

pub fn day_slots(busy: &Vec<Span>, open: i64, close: i64, need: i64, left: usize) -> (out: Vec<
    Span,
>)
    requires
        wf(busy@),
        in_range(open),
        in_range(close),
        0 < need < BOUND,
    ensures
        sound(out@, busy@, open, close, need),
        out@.len() <= left,
{
    let mut out: Vec<Span> = Vec::new();
    if left == 0 {
        return out;
    }
    let mut cursor = open;
    let mut i: usize = 0;
    let mut stopped_at_close = false;
    while i < busy.len()
        invariant
            wf(busy@),
            in_range(open),
            in_range(close),
            0 < need < BOUND,
            0 < left,
            i <= busy.len(),
            open <= cursor,
            in_range(cursor),
            out@.len() < left,
            stopped_at_close ==> i < busy.len() && busy@[i as int].s >= close,
            // every processed event ends by the cursor
            forall|k: int| 0 <= k < i ==> (#[trigger] busy@[k]).e <= cursor,
            // slots so far: in bounds, long enough, free of the processed events, before
            // the cursor and before every event still to come
            forall|j: int|
                0 <= j < out@.len() ==> open <= (#[trigger] out@[j]).s && out@[j].e <= close
                    && out@[j].e - out@[j].s >= need && out@[j].e <= cursor,
            forall|j: int, k: int|
                0 <= j < out@.len() && 0 <= k < i ==> disjoint(#[trigger] out@[j], #[trigger] busy@[k]),
            forall|j: int, k: int|
                0 <= j < out@.len() && i <= k < busy@.len() ==> (#[trigger] out@[j]).e <= (
                #[trigger] busy@[k]).s,
            forall|j: int, k: int| 0 <= j < k < out@.len() ==> (#[trigger] out@[j]).e <= (
            #[trigger] out@[k]).s,
        ensures
            i == busy.len() || cursor >= close || stopped_at_close,
        decreases busy.len() - i,
    {
        if cursor >= close {
            break;
        }
        let b = busy[i];
        if b.e <= cursor {
            i += 1;
            continue;
        }
        if b.s >= close {
            stopped_at_close = true;
            break;
        }
        let end = if b.s < close {
            b.s
        } else {
            close
        };
        if b.s > cursor && end - cursor >= need {
            let slot = Span { s: cursor, e: end };
            proof {
                assert forall|k: int| i <= k < busy@.len() implies slot.e <= (#[trigger] busy@[k]).s by {
                    assert(busy@[i as int].s <= busy@[k].s);
                }
            }
            out.push(slot);
            if out.len() >= left {
                proof {
                    assert forall|j: int, k: int|
                        0 <= j < out@.len() && 0 <= k < busy@.len() implies disjoint(
                        #[trigger] out@[j],
                        #[trigger] busy@[k],
                    ) by {
                        if k >= i {
                            assert(out@[j].e <= busy@[k].s);
                        }
                    }
                }
                return out;
            }
        }
        if b.e > cursor {
            cursor = b.e;
        }
        i += 1;
    }
    // The tail.
    if cursor < close && close - cursor >= need {
        let slot = Span { s: cursor, e: close };
        proof {
            assert forall|k: int| i <= k < busy@.len() implies disjoint(slot, #[trigger] busy@[k]) by {
                if stopped_at_close {
                    assert(busy@[i as int].s <= busy@[k].s);
                    assert(busy@[i as int].s >= close);
                }
            }
        }
        // Without a stop at `close`, the loop ran through every event or stopped with
        // cursor >= close (then there is no tail).
        proof {
            if !stopped_at_close {
                assert(i == busy.len() || cursor >= close);
            }
        }
        out.push(slot);
    }
    proof {
        assert forall|j: int, k: int|
            0 <= j < out@.len() && 0 <= k < busy@.len() implies disjoint(
            #[trigger] out@[j],
            #[trigger] busy@[k],
        ) by {
            if k >= i && j < out@.len() - 1 {
                assert(out@[j].e <= busy@[k].s);
            }
        }
    }
    out
}

} // verus!

fn main() {}
