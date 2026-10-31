// Quiet-hours kernel (secretary-server notifications: when a push is held back, and which
// devices may get it).
//
// Production (branch lucina-rs, crates/secretary-server/src/notify/):
//   quiet  <-> settings::Settings::quiet_at: DND until a later instant -> quiet; else, with
//              quiet hours on and both HH:MM valid (parse_hhmm: Some(minutes)), start ==
//              end -> not quiet, start < end -> [start, end), start > end -> wraps
//              midnight; `local` is local_minutes(now) in the settings' zone (0..1440)
//   held   <-> mod.rs admit: quiet_at(now) && !(category == Reminder && reminders_bypass_quiet)
//   allows <-> Settings::allows (global category switch, then the device's `enabled` and
//              its per-category override, missing = on); `sub_allows` = allows for the
//              subscription id && allows for its device id
// Differential test: crates/secretary-server/tests/verus_quiet_hours.rs (zone UTC, so
// local = now / 60000 mod 1440).
//
// Proven:
//   Q1 dnd_holds: while DND runs, everything but a bypassing reminder is held.
//   Q2 reminders_bypass: with the bypass on, a reminder is never held.
//   Q3 window: with quiet hours on and valid, a minute is quiet iff it lies in the
//      half-open window from start to end, going round midnight when start > end.
//   Q4 quiet_minutes: such a window silences exactly (end - start) mod 1440 minutes of
//      every day; start == end silences none (not "all day": docs/verification-gaps.md).
//   Q5 off_or_invalid: hours off, or an unparsable start or end, never silence (only DND).
//   P1 muted_device / category_off: a muted device gets nothing; a category switched off
//      globally reaches no device whatever its override; an override only narrows.
//
// No Stateright model: pure decisions over immutable settings. (The digest of held
// pushes after quiet hours is a separate protocol, not covered here.)
use vstd::prelude::*;

verus! {

pub const DAY: u32 = 1440;

pub struct Quiet {
    pub dnd_until: Option<i64>,
    pub enabled: bool,
    pub start: Option<u32>,
    pub end: Option<u32>,
}

pub open spec fn in_window(m: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        start <= m && m < end
    } else {
        m >= start || m < end
    }
}

pub open spec fn dnd(q: Quiet, now: i64) -> bool {
    q.dnd_until is Some && q.dnd_until.unwrap() > now
}

pub open spec fn quiet_spec(q: Quiet, now: i64, local: u32) -> bool {
    if dnd(q, now) {
        true
    } else if !q.enabled || q.start is None || q.end is None {
        false
    } else {
        in_window(local, q.start.unwrap(), q.end.unwrap())
    }
}

pub fn quiet(q: &Quiet, now: i64, local: u32) -> (b: bool)
    ensures
        b == quiet_spec(*q, now, local),
{
    match q.dnd_until {
        Some(until) => {
            if until > now {
                return true;
            }
        },
        None => {},
    }
    if !q.enabled {
        return false;
    }
    let (start, end) = match (q.start, q.end) {
        (Some(s), Some(e)) => (s, e),
        _ => return false,
    };
    if start == end {
        false
    } else if start < end {
        local >= start && local < end
    } else {
        local >= start || local < end
    }
}

pub fn held(q: &Quiet, now: i64, local: u32, reminder: bool, bypass: bool) -> (b: bool)
    ensures
        b == (quiet_spec(*q, now, local) && !(reminder && bypass)),
{
    quiet(q, now, local) && !(reminder && bypass)
}

pub proof fn dnd_holds(q: Quiet, now: i64, local: u32, reminder: bool, bypass: bool)
    requires
        dnd(q, now),
        !(reminder && bypass),
    ensures
        quiet_spec(q, now, local) && !(reminder && bypass),
{
}

pub proof fn reminders_bypass(q: Quiet, now: i64, local: u32)
    ensures
        !(quiet_spec(q, now, local) && !(true && true)),
{
}

pub proof fn off_or_invalid(q: Quiet, now: i64, local: u32)
    requires
        !dnd(q, now),
        !q.enabled || q.start is None || q.end is None,
    ensures
        !quiet_spec(q, now, local),
{
}

/// The minutes m < n of the day that are in the window.
pub open spec fn count(n: nat, start: u32, end: u32) -> nat
    decreases n,
{
    if n == 0 {
        0
    } else {
        count((n - 1) as nat, start, end) + if in_window((n - 1) as u32, start, end) {
            1nat
        } else {
            0nat
        }
    }
}

proof fn lemma_count(n: nat, start: u32, end: u32)
    requires
        n <= DAY,
        start < DAY,
        end < DAY,
    ensures
        count(n, start, end) == if start == end {
            0
        } else if start < end {
            if n <= start {
                0
            } else if n <= end {
                n - start
            } else {
                end - start
            }
        } else {
            // wraps: [0, end) and [start, 1440)
            (if n <= end {
                n
            } else {
                end as nat
            }) + (if n <= start {
                0
            } else {
                n - start
            })
        },
    decreases n,
{
    if n > 0 {
        lemma_count((n - 1) as nat, start, end);
    }
}

/// Q4.
pub proof fn quiet_minutes(start: u32, end: u32)
    requires
        start < DAY,
        end < DAY,
    ensures
        count(DAY as nat, start, end) == ((end as int - start as int) + DAY) % (DAY as int),
{
    lemma_count(DAY as nat, start, end);
}

/// Not vacuous: 22:30-07:30 silences 23:00 and 03:00, not 12:00, 540 minutes a day.
pub proof fn witness_default_window() {
    assert(in_window(1380, 1350, 450));
    assert(in_window(180, 1350, 450));
    assert(!in_window(720, 1350, 450));
    quiet_minutes(1350, 450);
    assert(count(1440, 1350, 450) == 540);
}

// ---------------------------------------------------------------- devices

/// A device's entry: `None` = no entry; else (enabled, override for this category).
pub open spec fn allows_spec(cat_on: bool, dev: Option<(bool, Option<bool>)>) -> bool {
    cat_on && match dev {
        None => true,
        Some((enabled, ov)) => enabled && match ov {
            None => true,
            Some(b) => b,
        },
    }
}

pub fn allows(cat_on: bool, dev: Option<(bool, Option<bool>)>) -> (b: bool)
    ensures
        b == allows_spec(cat_on, dev),
{
    if !cat_on {
        return false;
    }
    match dev {
        None => true,
        Some((enabled, ov)) => enabled && match ov {
            None => true,
            Some(b) => b,
        },
    }
}

pub proof fn muted_device(cat_on: bool, ov: Option<bool>)
    ensures
        !allows_spec(cat_on, Some((false, ov))),
{
}

pub proof fn category_off(dev: Option<(bool, Option<bool>)>)
    ensures
        !allows_spec(false, dev),
{
}

/// An entry never allows more than no entry.
pub proof fn override_only_narrows(cat_on: bool, dev: Option<(bool, Option<bool>)>)
    ensures
        allows_spec(cat_on, dev) ==> allows_spec(cat_on, None),
{
}

} // verus!

fn main() {}
