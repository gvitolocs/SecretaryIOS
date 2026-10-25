//! Differential test of `verification/verus/otp_expiry.rs` (expires_at and the relay
//! store) against the real `lucina_phone_core::proto::expires_at` and
//! `lucina_otp_relay::store::Store` (lucina-rs c357646).
//!
//! Belongs in `crates/lucina-otp-relay/tests/verus_otp_store.rs`; `diff/otp_run.sh`
//! runs it. Random operation sequences (arm, disarm, receive, take, with clocks that
//! mostly advance) drive the real store and a plain-Rust copy of the kernel `Store`
//! side by side; every answer, `pending_len` and `seen_len` must agree. All codes and
//! ids are synthetic.

use lucina_otp_relay::store::{Limits, Refusal, Store};
use lucina_phone_core::proto::{self, Incoming, OtpSource, CODE_TTL_MS};

const U64MAX: u64 = u64::MAX;

/// Plain-Rust copy of the kernel (`sat_add`, `expires_at`, `Store`).
mod kernel {
    pub const TTL: u64 = 120_000;
    pub const SEEN_TTL: u64 = 120_000;

    pub fn sat_add(a: u64, b: u64) -> u64 {
        if a > u64::MAX - b {
            u64::MAX
        } else {
            a + b
        }
    }

    pub fn expires_at(received_at: Option<u64>, now: u64, window_end: u64) -> Option<u64> {
        let b = match received_at {
            Some(t) => {
                if t <= now {
                    t
                } else {
                    now
                }
            }
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Entry {
        pub id: u64,
        pub code: u64,
        pub received_at: u64,
        pub expires: u64,
        pub stored_at: u64,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Seen {
        pub id: u64,
        pub until: u64,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Win {
        pub id: u64,
        pub end: Option<u64>,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Event {
        pub id: u64,
        pub code: u64,
        pub received_at: Option<u64>,
        pub window_id: Option<u64>,
        pub test: bool,
        pub valid: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        fn armed(&self, now: u64) -> bool {
            match self.window {
                None => false,
                Some(w) => match w.end {
                    None => true,
                    Some(end) => now < end,
                },
            }
        }

        fn prune(&mut self, now: u64) {
            self.pending = self.pending.iter().copied().filter(|x| x.expires > now).collect();
            self.seen = self.seen.iter().copied().filter(|x| x.until > now).collect();
        }

        pub fn receive(&mut self, ev: &Event, now: u64) -> Outcome {
            if !ev.valid {
                return Outcome::Invalid;
            }
            if ev.test {
                return Outcome::TestAck;
            }
            if !self.armed(now) {
                return Outcome::Paused;
            }
            let w = self.window.unwrap();
            if let Some(id) = ev.window_id {
                if id != w.id {
                    return Outcome::WrongWindow;
                }
            }
            let wend = w.end.unwrap_or(u64::MAX);
            self.prune(now);
            if self.seen.iter().any(|s| s.id == ev.id) {
                return Outcome::Duplicate;
            }
            let Some(e) = expires_at(ev.received_at, now, wend) else {
                return Outcome::Expired;
            };
            if self.seen.len() >= self.lim_seen {
                return Outcome::EventLimit;
            }
            self.seen.push(Seen { id: ev.id, until: sat_add(now, SEEN_TTL) });
            if self.pending.len() >= self.lim_pending && !self.pending.is_empty() {
                self.pending.remove(0);
            }
            let b = match ev.received_at {
                Some(t) => {
                    if t <= now {
                        t
                    } else {
                        now
                    }
                }
                None => now,
            };
            self.pending.push(Entry { id: ev.id, code: ev.code, received_at: b, expires: e, stored_at: now });
            Outcome::Stored
        }

        pub fn take(&mut self, now: u64) -> Option<Entry> {
            if !self.armed(now) {
                self.window = None;
                self.pending = Vec::new();
                return None;
            }
            while let Some(d) = self.pending.pop() {
                if d.expires > now {
                    return Some(d);
                }
            }
            None
        }

        pub fn arm(&mut self, seconds: u64, now: u64, wid: u64) {
            self.pending = Vec::new();
            if self.continuous {
                self.paused = false;
                self.window = Some(Win { id: wid, end: None });
            } else {
                let s = seconds.clamp(30, 300);
                self.window = Some(Win { id: wid, end: Some(sat_add(now, s * 1000)) });
            }
        }

        pub fn disarm(&mut self) {
            self.window = None;
            self.pending = Vec::new();
            if self.continuous {
                self.paused = true;
            }
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn event_id(id: u64) -> String {
    format!("8c1f0a52-7d2e-4f0b-9a51-{id:012}")
}
fn window_id(id: u64) -> String {
    format!("{id:032x}")
}
fn code(id: u64) -> String {
    format!("{:06}", id % 1_000_000)
}

fn outcome(r: Result<proto::IncomingAck, Refusal>) -> kernel::Outcome {
    use kernel::Outcome as O;
    match r {
        Ok(a) if a.test => O::TestAck,
        Ok(a) if a.duplicate => O::Duplicate,
        Ok(_) => O::Stored,
        Err(Refusal::Invalid(_)) => O::Invalid,
        Err(Refusal::Paused) => O::Paused,
        Err(Refusal::WrongWindow) => O::WrongWindow,
        Err(Refusal::Expired) => O::Expired,
        Err(Refusal::EventLimit) => O::EventLimit,
    }
}

#[test]
fn expires_at_matches_kernel() {
    let mut r = Rng(0x5eed_0201);
    let pick = |r: &mut Rng| -> u64 {
        match r.below(5) {
            0 => r.below(300_000),
            1 => U64MAX - r.below(300_000),
            2 => 1_700_000_000_000 + r.below(300_000),
            3 => r.next(),
            _ => 1_700_000_000_000,
        }
    };
    for _ in 0..200_000 {
        let now = pick(&mut r);
        let received = if r.below(4) == 0 { None } else { Some(pick(&mut r)) };
        let window = pick(&mut r);
        let real = proto::expires_at(received, now, window).ok();
        let k = kernel::expires_at(received, now, window);
        assert_eq!(real, k, "{received:?} {now} {window}");
        if let Some(e) = k {
            assert!(now < e && e as u128 <= now as u128 + CODE_TTL_MS as u128 && e <= window);
            if let Some(t) = received.filter(|&t| t <= now) {
                assert!(e as u128 <= t as u128 + CODE_TTL_MS as u128);
            }
        }
    }
}

#[test]
fn store_matches_kernel_and_theorems_hold() {
    let mut r = Rng(0x5eed_0202);
    let (mut stored, mut taken, mut dups, mut limits_hit) = (0, 0, 0, 0);
    for run in 0..600 {
        let continuous = r.below(3) == 0;
        let paused = r.below(4) == 0;
        let (lp, ls) = if run % 3 == 0 { (8, 64) } else { (1 + r.below(4) as usize, r.below(7) as usize) };
        let mut real = Store::new(continuous, paused).with_limits(Limits { pending: lp, seen: ls });
        let mut k = kernel::Store {
            continuous,
            paused,
            lim_pending: lp,
            lim_seen: ls,
            window: None,
            pending: Vec::new(),
            seen: Vec::new(),
        };
        let mut now: u64 = 1_700_000_000_000;
        let mut next_id = 1u64;
        let mut wid = 1u64;
        // (code, expires) of every delivered code, for the at-most-once check.
        let mut delivered: Vec<(u64, u64)> = Vec::new();
        for _ in 0..120 {
            now = match r.below(10) {
                0 => now.saturating_sub(r.below(5_000)), // a clock that goes back
                1..=2 => now + r.below(150_000),
                _ => now + r.below(5_000),
            };
            match r.below(10) {
                0 => {
                    wid += 1;
                    let secs = r.below(400);
                    real.arm(secs, now, window_id(wid));
                    k.arm(secs, now, wid);
                }
                1 => {
                    real.disarm();
                    k.disarm();
                }
                2..=6 => {
                    // A fresh id, or a repeat of a recent one.
                    let id = if next_id > 1 && r.below(3) == 0 { next_id - 1 - r.below(next_id - 1).min(3) } else { next_id };
                    if id == next_id {
                        next_id += 1;
                    }
                    let received_at = match r.below(5) {
                        0 => None,
                        1 => Some(now + r.below(60_000)),
                        2 => Some(now.saturating_sub(CODE_TTL_MS + r.below(10_000))),
                        _ => Some(now.saturating_sub(r.below(100_000))),
                    };
                    let w = match r.below(6) {
                        0 => Some(wid),
                        1 => Some(wid + 1000),
                        _ => None,
                    };
                    let ev = kernel::Event {
                        id,
                        code: id % 1_000_000,
                        received_at,
                        window_id: w,
                        test: r.below(12) == 0,
                        valid: r.below(15) != 0,
                    };
                    let inc = Incoming {
                        window_id: w.map(window_id),
                        event_id: event_id(id),
                        code: if ev.valid { code(id) } else { "12".into() },
                        sender: "Bank".into(),
                        received_at,
                        source: OtpSource::Sms,
                        test: ev.test,
                    };
                    let ro = outcome(real.receive(&inc, now));
                    let ko = k.receive(&ev, now);
                    assert_eq!(ro, ko, "run {run}: receive {ev:?} at {now}");
                    match ko {
                        kernel::Outcome::Stored => stored += 1,
                        kernel::Outcome::Duplicate => dups += 1,
                        kernel::Outcome::EventLimit => limits_hit += 1,
                        _ => {}
                    }
                }
                _ => {
                    let rd = real.take(now);
                    let kd = k.take(now);
                    match (&rd, &kd) {
                        (None, None) => {}
                        (Some(d), Some(e)) => {
                            assert_eq!((d.code.as_str(), d.received_at, d.expires_at), (code(e.id).as_str(), e.received_at, e.expires));
                            // R1
                            assert!(d.expires_at > now);
                            // R2: a second delivery of the same event was stored after the first died.
                            for &(c, exp) in &delivered {
                                if c == e.code {
                                    assert!(e.stored_at >= exp, "run {run}: {} delivered twice in its lifetime", e.code);
                                }
                            }
                            delivered.push((e.code, e.expires));
                            taken += 1;
                        }
                        _ => panic!("run {run}: take at {now}: real {rd:?}, kernel {kd:?}"),
                    }
                }
            }
            // R3
            assert_eq!(real.pending_len(), k.pending.len());
            assert_eq!(real.seen_len(), k.seen.len());
            assert!(real.pending_len() <= lp && real.seen_len() <= ls);
        }
    }
    assert!(stored > 1_000 && taken > 500 && dups > 100 && limits_hit > 10, "{stored} {taken} {dups} {limits_hit}");
    eprintln!("store == kernel: {stored} stored, {taken} taken, {dups} duplicates, {limits_hit} event limits");
}
