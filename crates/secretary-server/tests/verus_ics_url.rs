//! Differential test: `links::ics_url(_, false)` against a plain-Rust copy of
//! the Verus kernel `verification/verus/address_guard.rs`. Every IP literal
//! goes into `https://<ip>/x.ics` (IPv6 in brackets): the URL is refused
//! exactly when the kernel says the address is private or local. Addresses
//! are random, biased towards the edges of every range the code names, and
//! their IPv4-mapped IPv6 forms.
//!
//! `mapped_ipv4_literals_are_refused` is the regression test of L1: it fails
//! on the code before the fix (an IPv4-mapped IPv6 literal such as
//! `[::ffff:127.0.0.1]` passed as a public host).

use std::net::{Ipv4Addr, Ipv6Addr};

use secretary_server::links::ics_url;

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub struct V6 {
        pub s0: u16,
        pub s1: u16,
        pub s2: u16,
        pub s3: u16,
        pub s4: u16,
        pub s5: u16,
        pub s6: u16,
        pub s7: u16,
    }

    pub fn guard_v4(a: u32) -> bool {
        let x = a / 0x100_0000;
        let y = (a / 0x1_0000) % 0x100;
        let loopback = x == 127;
        let private = x == 10 || (x == 172 && (y & 0xf0) == 16) || (x == 192 && y == 168);
        let link_local = x == 169 && y == 254;
        let unspecified = a == 0;
        let broadcast = a == 0xffff_ffff;
        let cgnat = x == 100 && (y & 0xc0) == 64;
        loopback || private || link_local || unspecified || broadcast || cgnat
    }

    pub fn guard_v6(v: &V6) -> bool {
        if v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0xffff {
            let a: u32 = (v.s6 as u32) * 0x1_0000 + (v.s7 as u32);
            return guard_v4(a);
        }
        let loopback = v.s0 == 0
            && v.s1 == 0
            && v.s2 == 0
            && v.s3 == 0
            && v.s4 == 0
            && v.s5 == 0
            && v.s6 == 0
            && v.s7 == 1;
        let unspecified = v.s0 == 0
            && v.s1 == 0
            && v.s2 == 0
            && v.s3 == 0
            && v.s4 == 0
            && v.s5 == 0
            && v.s6 == 0
            && v.s7 == 0;
        let ula = (v.s0 & 0xfe00) == 0xfc00;
        let ll = (v.s0 & 0xffc0) == 0xfe80;
        loopback || unspecified || ula || ll
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
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn refused(url: &str) -> bool {
    ics_url(url, false).is_err()
}

fn check_v4(a: u32) {
    let ip = Ipv4Addr::from(a);
    let want = kernel::guard_v4(a);
    assert_eq!(refused(&format!("https://{ip}/x.ics")), want, "{ip}");
    // The same address, IPv4-mapped.
    let m = ip.to_ipv6_mapped();
    assert_eq!(refused(&format!("https://[{m}]/x.ics")), want, "{m}");
}

fn check_v6(s: [u16; 8]) {
    let ip = Ipv6Addr::from(s);
    let v = kernel::V6 {
        s0: s[0],
        s1: s[1],
        s2: s[2],
        s3: s[3],
        s4: s[4],
        s5: s[5],
        s6: s[6],
        s7: s[7],
    };
    assert_eq!(
        refused(&format!("https://[{ip}]/x.ics")),
        kernel::guard_v6(&v),
        "{ip}"
    );
}

/// First octets and second octets around every range edge.
const EDGES1: &[u32] = &[0, 1, 9, 10, 11, 99, 100, 101, 126, 127, 128, 168, 169, 170, 171, 172, 173, 191, 192, 193, 223, 224, 254, 255];
const EDGES2: &[u32] = &[0, 1, 15, 16, 17, 31, 32, 63, 64, 65, 127, 128, 167, 168, 169, 253, 254, 255];
const SEG0: &[u16] = &[0, 1, 0x2001, 0x2002, 0xfbff, 0xfc00, 0xfcff, 0xfd00, 0xfdff, 0xfe00, 0xfe7f, 0xfe80, 0xfebf, 0xfec0, 0xff00, 0xffff];

#[test]
fn ipv4_literals_agree_with_the_kernel() {
    let mut rng = Rng(0xadd2_e55);
    let mut n_refused = 0;
    for &a in EDGES1 {
        for &b in EDGES2 {
            for &cd in &[0u32, 1, 0xa9fe, 0xfffe, 0xffff] {
                let addr = (a << 24) | (b << 16) | cd;
                check_v4(addr);
                n_refused += usize::from(kernel::guard_v4(addr));
            }
        }
    }
    for _ in 0..20_000 {
        check_v4(rng.next() as u32);
    }
    check_v4(0xffff_ffff);
    assert!(n_refused > 100, "not vacuous: {n_refused}");
}

#[test]
fn ipv6_literals_agree_with_the_kernel() {
    let mut rng = Rng(0x0006_0006);
    for _ in 0..20_000 {
        let mut s = [0u16; 8];
        let shape = rng.below(4);
        for (i, seg) in s.iter_mut().enumerate() {
            *seg = match shape {
                0 => rng.next() as u16,
                // mostly zero, so ::1, ::, ::ffff:x and IPv4-compatible forms come up
                1 => {
                    if rng.below(4) == 0 {
                        rng.next() as u16
                    } else {
                        0
                    }
                }
                2 if i == 0 => SEG0[rng.below(SEG0.len() as u64) as usize],
                2 => rng.next() as u16,
                _ => [0, 1, 0xffff][rng.below(3) as usize],
            };
        }
        check_v6(s);
    }
    for s0 in SEG0 {
        check_v6([*s0, 0, 0, 0, 0, 0, 0, 1]);
    }
    check_v6([0, 0, 0, 0, 0, 0, 0, 0]);
    check_v6([0, 0, 0, 0, 0, 0, 0, 1]);
    check_v6([0, 0, 0, 0, 0, 0, 0, 2]);
}

/// L1 regression: these passed as public hosts before the fix.
#[test]
fn mapped_ipv4_literals_are_refused() {
    for url in [
        "https://[::ffff:127.0.0.1]/x.ics",
        "https://[::ffff:7f00:1]/x.ics",
        "https://[::ffff:10.0.0.5]/x.ics",
        "https://[::ffff:192.168.1.1]/x.ics",
        "https://[::ffff:169.254.169.254]/x.ics",
        "https://[::ffff:100.107.164.33]/x.ics",
        "https://[::ffff:0.0.0.0]/x.ics",
        "webcal://[::ffff:127.0.0.1]/x.ics",
    ] {
        assert!(refused(url), "{url}");
    }
    // A mapped public address is still fine.
    assert!(!refused("https://[::ffff:8.8.8.8]/x.ics"));
    // WHATWG IPv4 spellings are normalised by the URL parser before the check.
    for url in ["https://0x7f.1/x", "https://2130706433/x", "https://127.1/x", "https://0177.0.0.1/x"] {
        assert!(refused(url), "{url}");
    }
}
