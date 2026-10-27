//! Differential test: `scope::Scopes` and `ratelimit::Limiter` against a
//! plain-Rust copy of the Verus kernel `verification/verus/mcp_scopes.rs`.
//!
//! - Scopes: every list of up to four names (known, unknown, upper case,
//!   empty) joined by ' ' and ',' goes through `parse_lenient` and
//!   `parse_list`; the results, their intersections and subset relations must
//!   equal the kernel's bitmasks.
//! - Limiter: the real `check` reads `Instant::now()`, so it runs with a
//!   window far longer than the test (nothing expires: every check is at the
//!   kernel's time 0) and with a zero window (everything expires at once),
//!   over random keys; accept / refuse must agree with the kernel.

use std::time::Duration;

use secretary_mcp::ratelimit::Limiter;
use secretary_mcp::scope::{Scope, Scopes};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub const READ: u8 = 1;
    pub const CHAT: u8 = 2;
    pub const WRITE: u8 = 4;
    pub const COMPUTER: u8 = 8;

    pub fn bit_exec(name: u8) -> u8 {
        if name == 1 {
            READ
        } else if name == 2 {
            CHAT
        } else if name == 3 {
            WRITE
        } else if name == 4 {
            COMPUTER
        } else {
            0
        }
    }

    pub fn parse_lenient(names: &[u8]) -> u8 {
        let mut m = 0;
        for &n in names {
            m |= bit_exec(n);
        }
        m
    }

    pub fn parse_list(names: &[u8]) -> Option<u8> {
        if names.iter().any(|&n| !(1..=4).contains(&n)) {
            return None;
        }
        Some(parse_lenient(names))
    }

    pub fn intersect(a: u8, b: u8) -> u8 {
        a & b
    }

    pub fn is_subset_of(a: u8, b: u8) -> bool {
        a & b == a
    }

    /// One key's hits; `(accepted, hits)`.
    pub fn check(v: &[u64], now: u64, window: u64, max: u64) -> (bool, Vec<u64>) {
        let mut kept: Vec<u64> = v.iter().copied().filter(|&t| now - t < window).collect();
        if kept.len() as u64 >= max {
            return (false, kept);
        }
        kept.push(now);
        (true, kept)
    }
}

const NAMES: &[(&str, u8)] = &[
    ("read", 1),
    ("chat", 2),
    ("write", 3),
    ("computer", 4),
    ("admin", 0),
    ("openid", 0),
    ("offline_access", 0),
    ("READ", 0),
    ("", 0),
];

fn mask(s: &Scopes) -> u8 {
    s.iter()
        .map(|x| match x {
            Scope::Read => 1,
            Scope::Chat => 2,
            Scope::Write => 4,
            Scope::Computer => 8,
        })
        .fold(0, |a, b| a | b)
}

fn from_mask(m: u8) -> Scopes {
    Scopes::of(
        [(1, Scope::Read), (2, Scope::Chat), (4, Scope::Write), (8, Scope::Computer)]
            .into_iter()
            .filter(|(b, _)| m & b != 0)
            .map(|(_, s)| s),
    )
}

fn lists(n: usize) -> Vec<Vec<usize>> {
    let mut out = vec![vec![]];
    let mut frontier = vec![vec![]];
    for _ in 0..n {
        let mut next = Vec::new();
        for l in &frontier {
            for i in 0..NAMES.len() {
                let mut m: Vec<usize> = l.clone();
                m.push(i);
                next.push(m);
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    out
}

#[test]
fn scope_parsing_agrees_with_the_kernel() {
    let mut n = 0;
    for l in lists(4) {
        // The code splits on ' ' and ',' and drops empty parts.
        let ids: Vec<u8> = l
            .iter()
            .filter(|&&i| !NAMES[i].0.is_empty())
            .map(|&i| NAMES[i].1)
            .collect();
        for sep in [" ", ",", ", "] {
            let text = l.iter().map(|&i| NAMES[i].0).collect::<Vec<_>>().join(sep);
            assert_eq!(
                mask(&Scopes::parse_lenient(&text)),
                kernel::parse_lenient(&ids),
                "{text:?}"
            );
            assert_eq!(
                Scopes::parse_list(&text).ok().map(|s| mask(&s)),
                kernel::parse_list(&ids),
                "{text:?}"
            );
            n += 1;
        }
    }
    assert!(n > 10_000);
}

#[test]
fn scope_algebra_agrees_with_the_kernel() {
    for a in 0..16u8 {
        for b in 0..16u8 {
            let (sa, sb) = (from_mask(a), from_mask(b));
            assert_eq!(mask(&sa.intersect(&sb)), kernel::intersect(a, b));
            assert_eq!(sa.is_subset_of(&sb), kernel::is_subset_of(a, b), "{a} {b}");
            for (bit, s) in [(1, Scope::Read), (2, Scope::Chat), (4, Scope::Write)] {
                assert_eq!(sa.contains(s), a & bit != 0);
            }
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[test]
fn limiter_agrees_with_the_kernel() {
    let mut rng = Rng(0x0001_1a17);
    // max = 0 panics in the real code (witness below); the kernel refuses everything.
    for max in [1usize, 2, 5, 20] {
        // An hour-long window: nothing expires during the test.
        let real = Limiter::new(max, Duration::from_secs(3600));
        let mut hits: Vec<Vec<u64>> = vec![Vec::new(); 4];
        let mut accepted = 0;
        for _ in 0..200 {
            let key = (rng.next() % 4) as usize;
            let (want, next) = kernel::check(&hits[key], 0, 3_600_000, max as u64);
            hits[key] = next;
            let got = real.check(&format!("k{key}"));
            assert_eq!(got.is_ok(), want, "max {max} key {key}");
            if let Err(wait) = got {
                assert!((1..=3600).contains(&wait));
            } else {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted,
            4 * max.min(200),
            "every key filled to max, never more"
        );
        // A zero window: every hit expires at once, nothing is ever refused (max > 0).
        let open = Limiter::new(max, Duration::ZERO);
        let mut v = Vec::new();
        for _ in 0..50 {
            let (want, next) = kernel::check(&v, 0, 0, max as u64);
            v = next;
            assert_eq!(open.check("k").is_ok(), want);
        }
    }
}

/// Latent gap R2 (unreachable today: every Limiter in oauth.rs has max >= 5):
/// with `max == 0` the first check indexes the empty hit list. The kernel
/// refuses every attempt instead. Flip to an `Err` assertion once fixed.
#[test]
#[should_panic(expected = "index out of bounds")]
fn witness_r2_zero_max_panics() {
    assert!(!kernel::check(&[], 0, 1, 0).0);
    let _ = Limiter::new(0, Duration::from_secs(60)).check("k");
}
