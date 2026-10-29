// Login lockout kernel (secretary-server: how fast the owner's password can be guessed).
//
// Production (branch lucina-rs, crates/secretary-server/src/owner.rs):
//   lock_for_exec <-> owner::lock_for (15 min after the 10th consecutive failure, doubling
//                     per further failure, at most 24 h)
//   step          <-> OwnerStore::verify_at on the stored {failed_count, locked_until}:
//                     locked -> Verdict::Locked (no Argon2 verify, nothing written); right
//                     email + password -> Ok and reset; anything else -> Invalid,
//                     failed_count + 1 (saturating), and from the 10th failure on
//                     locked_until = now + lock_for(failed_count)
//   `correct`     <-> email_ok && password_ok (wrong emails count as failures too)
// verify_at runs under owner.lock (read-modify-write of owner.json), so calls are
// serialised: one step at a time, with the clock as input. Times are Unix seconds and
// never run backwards between steps (`now >= last`); `now <= MAX_NOW` keeps
// `now + lock_for` inside u64 (MAX_NOW is about 5.8e11 years from now).
// Differential test: crates/secretary-server/tests/verus_lockout.rs (the real store on
// disk, Argon2 included).
//
// Proven:
//   K1 lock_bounds / lock_doubles / lock_monotone: lock_for(c) is 900 s for c <= 10,
//      doubles per failure after that, never decreases, and is capped at 86 400 s.
//   K2 locked_step_checks_nothing: while locked, a step verifies no password and changes
//      nothing (a guess made then is not tried).
//   K3 counting: a failure adds exactly one (saturating), a success resets to zero.
//   K4 inv_preserved (over every step, all inputs): from the 10th consecutive failure on,
//      the account is locked until at least lock_for(failed) after the last password
//      check. K5 guess_spacing: hence a password check made while failed >= 10 happens at
//      least 900 s after the previous one, and at least lock_for(failed) after it: after
//      10 free guesses, an attacker gets at most one guess per 15 minutes, then one per
//      30, 60, ... minutes, never more often than one per 24 h once the cap is reached.
//
// No Stateright model: the file lock serialises the only state; one step relation
// covers every interleaving.
use vstd::prelude::*;

verus! {

pub const LOCK_AFTER: u32 = 10;
pub const FIRST_LOCK: u64 = 900;
pub const MAX_LOCK: u64 = 86_400;
pub const MAX_NOW: u64 = 0xffff_ffff_ffff_ffff - 86_400;

pub struct Account {
    pub failed: u32,
    pub locked_until: Option<u64>,
}

#[derive(PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Invalid,
    Locked { wait: u64 },
}

// ---------------------------------------------------------------- spec

pub open spec fn pow2(d: nat) -> nat
    decreases d,
{
    if d == 0 {
        1
    } else {
        2 * pow2((d - 1) as nat)
    }
}

pub open spec fn doublings(c: u32) -> nat {
    if c <= LOCK_AFTER {
        0
    } else if c - LOCK_AFTER >= 16 {
        16
    } else {
        (c - LOCK_AFTER) as nat
    }
}

pub open spec fn lock_for(c: u32) -> nat {
    let v = (FIRST_LOCK * pow2(doublings(c))) as nat;
    if v > MAX_LOCK {
        MAX_LOCK as nat
    } else {
        v
    }
}

pub open spec fn sat_inc(c: u32) -> u32 {
    if c == u32::MAX {
        c
    } else {
        (c + 1) as u32
    }
}

pub open spec fn is_locked(a: Account, now: u64) -> bool {
    a.locked_until is Some && a.locked_until.unwrap() > now
}

/// Whether the step runs a password check (an Argon2 verify of the typed password).
pub open spec fn checks(a: Account, now: u64) -> bool {
    !is_locked(a, now)
}

pub open spec fn next(a: Account, now: u64, correct: bool) -> Account {
    if is_locked(a, now) {
        a
    } else if correct {
        Account { failed: 0, locked_until: None }
    } else {
        let f = sat_inc(a.failed);
        Account {
            failed: f,
            locked_until: if f >= LOCK_AFTER {
                Some((now + lock_for(f)) as u64)
            } else {
                a.locked_until
            },
        }
    }
}

/// From the 10th failure on, locked until lock_for(failed) after the last check.
pub open spec fn inv(a: Account, last: int) -> bool {
    a.failed >= LOCK_AFTER ==> (a.locked_until is Some && a.locked_until.unwrap() >= last
        + lock_for(a.failed))
}

// ---------------------------------------------------------------- K1

proof fn lemma_pow2_pos(d: nat)
    ensures
        pow2(d) >= 1,
    decreases d,
{
    if d > 0 {
        lemma_pow2_pos((d - 1) as nat);
    }
}

proof fn lemma_pow2_mono(a: nat, b: nat)
    requires
        a <= b,
    ensures
        pow2(a) <= pow2(b),
    decreases b,
{
    if a < b {
        lemma_pow2_mono(a, (b - 1) as nat);
        lemma_pow2_pos((b - 1) as nat);
    }
}

pub proof fn lock_bounds(c: u32)
    ensures
        FIRST_LOCK <= lock_for(c) <= MAX_LOCK,
        c <= LOCK_AFTER ==> lock_for(c) == FIRST_LOCK,
{
    lemma_pow2_pos(doublings(c));
}

pub proof fn lock_monotone(c: u32, d: u32)
    requires
        c <= d,
    ensures
        lock_for(c) <= lock_for(d),
{
    lemma_pow2_mono(doublings(c), doublings(d));
    assert(FIRST_LOCK * pow2(doublings(c)) <= FIRST_LOCK * pow2(doublings(d))) by (nonlinear_arith)
        requires
            pow2(doublings(c)) <= pow2(doublings(d)),
    ;
}

pub proof fn lock_doubles(c: u32)
    requires
        LOCK_AFTER <= c < u32::MAX,
    ensures
        lock_for((c + 1) as u32) == if 2 * lock_for(c) > MAX_LOCK {
            MAX_LOCK as nat
        } else {
            2 * lock_for(c)
        },
{
    let c1 = (c + 1) as u32;
    if c - LOCK_AFTER < 16 {
        assert(doublings(c1) == doublings(c) + 1);
        assert(pow2(doublings(c1)) == 2 * pow2(doublings(c)));
    } else {
        // Both at 16 doublings: 900 * 65536 is far above the cap.
        assert(pow2(16) == 65536) by (compute);
        assert(doublings(c) == 16 && doublings(c1) == 16);
    }
}

pub proof fn lock_examples()
    ensures
        lock_for(10) == 900,
        lock_for(11) == 1800,
        lock_for(12) == 3600,
        lock_for(17) == 86_400,
        lock_for(40) == 86_400,
{
    assert(pow2(1) == 2) by (compute);
    assert(pow2(2) == 4) by (compute);
    assert(pow2(7) == 128) by (compute);
    assert(pow2(16) == 65536) by (compute);
}

// ---------------------------------------------------------------- K2-K5

pub proof fn locked_step_checks_nothing(a: Account, now: u64, correct: bool)
    requires
        is_locked(a, now),
    ensures
        !checks(a, now),
        next(a, now, correct) == a,
{
}

pub proof fn counting(a: Account, now: u64, correct: bool)
    requires
        checks(a, now),
    ensures
        correct ==> next(a, now, correct).failed == 0,
        !correct ==> next(a, now, correct).failed == sat_inc(a.failed),
{
}

/// K4: every step keeps the invariant, with `last` moving to `now` on a password check.
pub proof fn inv_preserved(a: Account, last: int, now: u64, correct: bool)
    requires
        inv(a, last),
        last <= now,
        now <= MAX_NOW,
    ensures
        inv(next(a, now, correct), if checks(a, now) { now as int } else { last }),
{
    let b = next(a, now, correct);
    if is_locked(a, now) {
    } else if correct {
    } else {
        lock_bounds(sat_inc(a.failed));
    }
}

/// K5: a check made while failed >= 10 comes at least lock_for(failed) >= 900 s after
/// the previous check.
pub proof fn guess_spacing(a: Account, last: int, now: u64)
    requires
        inv(a, last),
        a.failed >= LOCK_AFTER,
        checks(a, now),
    ensures
        now >= last + lock_for(a.failed),
        now >= last + FIRST_LOCK,
{
    lock_bounds(a.failed);
}

/// Not vacuous: ten failures from a fresh account lock it for 900 s.
pub proof fn witness_tenth_failure_locks() {
    let mut a = Account { failed: 9, locked_until: None };
    let b = next(a, 1000, false);
    assert(b.failed == 10);
    lock_bounds(10);
    assert(b.locked_until == Some(1900u64));
    assert(is_locked(b, 1899));
    assert(!is_locked(b, 1900));
}

// ---------------------------------------------------------------- exec

pub fn lock_for_exec(c: u32) -> (r: u64)
    ensures
        r as nat == lock_for(c),
{
    let d: u32 = if c <= LOCK_AFTER {
        0
    } else if c - LOCK_AFTER >= 16 {
        16
    } else {
        c - LOCK_AFTER
    };
    let mut v: u64 = FIRST_LOCK;
    let mut i: u32 = 0;
    while i < d
        invariant
            i <= d <= 16,
            d as nat == doublings(c),
            v as nat == FIRST_LOCK * pow2(i as nat),
            v <= FIRST_LOCK * 65536,
        decreases d - i,
    {
        proof {
            lemma_pow2_mono((i + 1) as nat, 16);
            assert(pow2(16) == 65536) by (compute);
            assert(pow2((i + 1) as nat) == 2 * pow2(i as nat));
            assert(FIRST_LOCK * pow2((i + 1) as nat) <= FIRST_LOCK * 65536) by (nonlinear_arith)
                requires
                    pow2((i + 1) as nat) <= 65536,
            ;
        }
        v = v * 2;
        i += 1;
    }
    if v > MAX_LOCK {
        MAX_LOCK
    } else {
        v
    }
}

/// `verify_at` on the stored account.
pub fn step(a: &Account, now: u64, correct: bool) -> (r: (Verdict, Account))
    requires
        now <= MAX_NOW,
    ensures
        r.1 == next(*a, now, correct),
        is_locked(*a, now) ==> r.0 == (Verdict::Locked { wait: (a.locked_until.unwrap() - now) as u64 }),
        !is_locked(*a, now) && correct ==> r.0 == Verdict::Ok,
        !is_locked(*a, now) && !correct ==> r.0 == Verdict::Invalid,
{
    match a.locked_until {
        Some(until) if until > now => {
            return (
                Verdict::Locked { wait: until - now },
                Account { failed: a.failed, locked_until: a.locked_until },
            );
        },
        _ => {},
    }
    if correct {
        return (Verdict::Ok, Account { failed: 0, locked_until: None });
    }
    let f = if a.failed == u32::MAX {
        a.failed
    } else {
        a.failed + 1
    };
    let locked_until = if f >= LOCK_AFTER {
        let l = lock_for_exec(f);
        proof {
            lock_bounds(f);
        }
        Some(now + l)
    } else {
        a.locked_until
    };
    (Verdict::Invalid, Account { failed: f, locked_until })
}

} // verus!

fn main() {}
