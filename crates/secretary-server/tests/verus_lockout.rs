//! Differential test: `OwnerStore::verify_at` (the real store on disk, Argon2
//! included) against a plain-Rust copy of the Verus kernel
//! `verification/verus/login_lockout.rs`. Random runs of logins (right
//! password, wrong password, wrong email) at random clock steps, from a few
//! seconds to past the lock: after every call the verdict and the stored
//! `failed_count` / `locked_until` must equal the kernel's. The kernel's
//! invariant (locked until `lock_for(failed)` after the last password check
//! once `failed >= 10`) is checked along the way.

use std::time::Duration;

use secretary_server::owner::{OwnerStore, Verdict};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub const LOCK_AFTER: u32 = 10;
    pub const FIRST_LOCK: u64 = 900;
    pub const MAX_LOCK: u64 = 86_400;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Account {
        pub failed: u32,
        pub locked_until: Option<u64>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Verdict {
        Ok,
        Invalid,
        Locked { wait: u64 },
    }

    pub fn lock_for_exec(c: u32) -> u64 {
        let d: u32 = if c <= LOCK_AFTER {
            0
        } else if c - LOCK_AFTER >= 16 {
            16
        } else {
            c - LOCK_AFTER
        };
        let mut v: u64 = FIRST_LOCK;
        for _ in 0..d {
            v *= 2;
        }
        if v > MAX_LOCK {
            MAX_LOCK
        } else {
            v
        }
    }

    pub fn step(a: &Account, now: u64, correct: bool) -> (Verdict, Account) {
        match a.locked_until {
            Some(until) if until > now => {
                return (Verdict::Locked { wait: until - now }, *a);
            }
            _ => {}
        }
        if correct {
            return (
                Verdict::Ok,
                Account {
                    failed: 0,
                    locked_until: None,
                },
            );
        }
        let f = if a.failed == u32::MAX {
            a.failed
        } else {
            a.failed + 1
        };
        let locked_until = if f >= LOCK_AFTER {
            Some(now + lock_for_exec(f))
        } else {
            a.locked_until
        };
        (
            Verdict::Invalid,
            Account {
                failed: f,
                locked_until,
            },
        )
    }
}

const EMAIL: &str = "owner@example.com";
const PASSWORD: &str = "correct horse battery";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn to_kernel(v: &Verdict) -> kernel::Verdict {
    match v {
        Verdict::Ok => kernel::Verdict::Ok,
        Verdict::Invalid => kernel::Verdict::Invalid,
        Verdict::Locked(d) => kernel::Verdict::Locked { wait: d.as_secs() },
    }
}

#[test]
fn random_login_runs_agree_with_the_kernel() {
    let mut rng = Rng(0x10c0_c0de);
    let (mut locked_seen, mut checks_while_locked_phase) = (0, 0);
    for run in 0..12 {
        let dir = tempfile::tempdir().unwrap();
        let store = OwnerStore::new(dir.path());
        store.set(EMAIL, PASSWORD, false).unwrap();
        let mut acc = kernel::Account {
            failed: 0,
            locked_until: None,
        };
        let mut now: u64 = 1_800_000_000 + run * 1_000_000;
        let mut last_check: u64 = now;
        for _ in 0..60 {
            now += match rng.below(6) {
                0 => 0,
                1 => rng.below(10),
                2 => rng.below(900),
                3 => 900 + rng.below(3),
                4 => kernel::lock_for_exec(acc.failed).saturating_sub(1) + rng.below(3),
                _ => rng.below(90_000),
            };
            // Mostly failures, as an attacker would make them.
            let (email, password, correct) = match rng.below(10) {
                0 => (EMAIL, PASSWORD, true),
                1 => ("OWNER@Example.com", PASSWORD, true),
                2 => ("x@example.com", PASSWORD, false),
                _ => (EMAIL, "wrong password!!", false),
            };
            let (want_v, want_acc) = kernel::step(&acc, now, correct);
            let got = store.verify_at(email, password, now).unwrap();
            assert_eq!(
                to_kernel(&got),
                want_v,
                "run {run} at {now}, before {acc:?}"
            );
            let o = store.load().unwrap().unwrap();
            assert_eq!(
                (o.failed_count, o.locked_until),
                (want_acc.failed, want_acc.locked_until),
                "run {run} at {now}"
            );
            let checked = !matches!(want_v, kernel::Verdict::Locked { .. });
            if acc.failed >= kernel::LOCK_AFTER && checked {
                // K5: at least lock_for(failed) after the previous password check.
                assert!(now >= last_check + kernel::lock_for_exec(acc.failed));
                checks_while_locked_phase += 1;
            }
            if checked {
                last_check = now;
            } else {
                locked_seen += 1;
            }
            acc = want_acc;
        }
    }
    // Not vacuous: locks were hit, and checks happened after locks expired.
    assert!(locked_seen > 50, "locked {locked_seen}");
    assert!(
        checks_while_locked_phase > 20,
        "{checks_while_locked_phase}"
    );
}

#[test]
fn lock_schedule_matches_the_kernel() {
    let dir = tempfile::tempdir().unwrap();
    let store = OwnerStore::new(dir.path());
    store.set(EMAIL, PASSWORD, false).unwrap();
    let mut now = 1_800_000_000u64;
    for failed in 1..=24u32 {
        assert_eq!(
            store.verify_at(EMAIL, "nope nope nope", now).unwrap(),
            Verdict::Invalid
        );
        let o = store.load().unwrap().unwrap();
        assert_eq!(o.failed_count, failed);
        if failed >= 10 {
            let lock = kernel::lock_for_exec(failed);
            assert_eq!(o.locked_until, Some(now + lock), "failed {failed}");
            // One second before the end: still locked, and nothing changes.
            assert_eq!(
                store.verify_at(EMAIL, PASSWORD, now + lock - 1).unwrap(),
                Verdict::Locked(Duration::from_secs(1))
            );
            now += lock;
        } else {
            assert_eq!(o.locked_until, None);
            now += 1;
        }
    }
    assert_eq!(kernel::lock_for_exec(24), 86_400);
}
