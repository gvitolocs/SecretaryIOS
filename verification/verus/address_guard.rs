// Address guard kernel (secretary-server: which IP literals an ICS subscription may name).
//
// Production (branch lucina-rs, crates/secretary-server/src/links.rs):
//   blocked_v4 / blocked_v6 <-> links::private_ip (the contract: "private and local
//                               addresses are not allowed", ics_url's error message)
//   guard_v4 / guard_v6     <-> private_ip after fix L1 (IPv4-mapped IPv6 addresses are
//                               judged as the IPv4 address they reach)
//   private_ip_before_l1    <-> private_ip before L1 (kept as the witness)
//   V4 = u32 (octets big-endian, Ipv4Addr::to_bits), V6 = the eight segments
//   (Ipv6Addr::segments).
// Std semantics used (std 1.8x, stable): Ipv4Addr::is_loopback 127/8, is_private 10/8,
// 172.16/12, 192.168/16, is_link_local 169.254/16, is_unspecified 0.0.0.0, is_broadcast
// 255.255.255.255; Ipv6Addr::is_loopback ::1, is_unspecified ::, to_ipv4_mapped
// ::ffff:0:0/96. The diff test checks the real ics_url against this table.
// Differential test: crates/secretary-server/tests/verus_ics_url.rs.
//
// Proven:
//   A1 guard_v4 / guard_v6 decide exactly blocked_v4 / blocked_v6.
//   A2 mapped_same_as_v4: for every IPv4 address a, ::ffff:a is refused iff a is
//      (no bypass of the IPv4 list through the IPv4-mapped form).
//   A3 the ranges the code names are refused whole: loopback 127/8, 10/8, 172.16/12,
//      192.168/16, 169.254/16 (cloud metadata 169.254.169.254 included), 100.64/10
//      (Tailscale, CGNAT), fc00::/7, fe80::/10, ::1, ::, 0.0.0.0, 255.255.255.255.
//   witness_l1_mapped_loopback: private_ip_before_l1 lets ::ffff:127.0.0.1 through
//      (and ::ffff:169.254.169.254), the contract and the fixed guard refuse it.
// Not covered (and not claimed by the code): host names that resolve to private
// addresses and redirects to them (the fetch follows up to 5 redirects and does not
// pin DNS); see docs/verification-gaps.md, L2.
//
// No Stateright model: one pure decision per URL.
use vstd::prelude::*;

verus! {

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

// ---------------------------------------------------------------- spec: the contract

pub open spec fn o1(a: u32) -> u32 {
    a / 0x100_0000
}

pub open spec fn o2(a: u32) -> u32 {
    (a / 0x1_0000) % 0x100
}

pub open spec fn blocked_v4(a: u32) -> bool {
    ||| o1(a) == 127  // loopback
    ||| o1(a) == 10  // private
    ||| (o1(a) == 172 && 16 <= o2(a) <= 31)  // private
    ||| (o1(a) == 192 && o2(a) == 168)  // private
    ||| (o1(a) == 169 && o2(a) == 254)  // link-local (cloud metadata)
    ||| a == 0  // unspecified
    ||| a == 0xffff_ffff  // broadcast
    ||| (o1(a) == 100 && 64 <= o2(a) <= 127)  // 100.64/10: Tailscale, CGNAT

}

/// ::ffff:a.b.c.d
pub open spec fn is_mapped(v: V6) -> bool {
    v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0xffff
}

pub open spec fn mapped_v4(v: V6) -> u32 {
    (v.s6 as int * 0x1_0000 + v.s7 as int) as u32
}

pub open spec fn blocked_v6(v: V6) -> bool {
    if is_mapped(v) {
        blocked_v4(mapped_v4(v))
    } else {
        ||| (v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0 && v.s6
            == 0 && v.s7 == 1)  // ::1
        ||| (v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0 && v.s6
            == 0 && v.s7 == 0)  // ::
        ||| (0xfc00 <= v.s0 && v.s0 <= 0xfdff)  // fc00::/7 (unique local)
        ||| (0xfe80 <= v.s0 && v.s0 <= 0xfebf)  // fe80::/10 (link-local)

    }
}

/// `private_ip` before L1: the IPv6 arm only looks at ::1, ::, fc00::/7 and fe80::/10.
pub open spec fn private_ip_before_l1(v: V6) -> bool {
    ||| (v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0 && v.s6 == 0
        && v.s7 == 1)
    ||| (v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0 && v.s6 == 0
        && v.s7 == 0)
    ||| (0xfc00 <= v.s0 && v.s0 <= 0xfdff)
    ||| (0xfe80 <= v.s0 && v.s0 <= 0xfebf)
}

pub open spec fn mapped(a: u32) -> V6 {
    V6 {
        s0: 0,
        s1: 0,
        s2: 0,
        s3: 0,
        s4: 0,
        s5: 0xffff,
        s6: (a / 0x1_0000) as u16,
        s7: (a % 0x1_0000) as u16,
    }
}

// ---------------------------------------------------------------- exec: the fixed code

/// `private_ip(IpAddr::V4(a))`, the code's expression order.
pub fn guard_v4(a: u32) -> (b: bool)
    ensures
        b == blocked_v4(a),
{
    let x = a / 0x100_0000;
    let y = (a / 0x1_0000) % 0x100;
    let loopback = x == 127;
    let private = x == 10 || (x == 172 && (y & 0xf0) == 16) || (x == 192 && y == 168);
    let link_local = x == 169 && y == 254;
    let unspecified = a == 0;
    let broadcast = a == 0xffff_ffff;
    // `v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64`
    let cgnat = x == 100 && (y & 0xc0) == 64;
    assert((y & 0xf0) == 16 <==> (16 <= y && y <= 31)) by (bit_vector)
        requires
            y < 0x100,
    ;
    assert((y & 0xc0) == 64 <==> (64 <= y && y <= 127)) by (bit_vector)
        requires
            y < 0x100,
    ;
    loopback || private || link_local || unspecified || broadcast || cgnat
}

/// `private_ip(IpAddr::V6(v))` after L1.
pub fn guard_v6(v: &V6) -> (b: bool)
    ensures
        b == blocked_v6(*v),
{
    if v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0xffff {
        // `if let Some(v4) = v.to_ipv4_mapped() { return private_ip(IpAddr::V4(v4)); }`
        let a: u32 = (v.s6 as u32) * 0x1_0000 + (v.s7 as u32);
        return guard_v4(a);
    }
    let loopback = v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0
        && v.s6 == 0 && v.s7 == 1;
    let unspecified = v.s0 == 0 && v.s1 == 0 && v.s2 == 0 && v.s3 == 0 && v.s4 == 0 && v.s5 == 0
        && v.s6 == 0 && v.s7 == 0;
    let s0 = v.s0;
    // `(segments()[0] & 0xfe00) == 0xfc00`, `(segments()[0] & 0xffc0) == 0xfe80`
    let ula = (s0 & 0xfe00) == 0xfc00;
    let ll = (s0 & 0xffc0) == 0xfe80;
    assert((s0 & 0xfe00) == 0xfc00 <==> (0xfc00 <= s0 && s0 <= 0xfdff)) by (bit_vector);
    assert((s0 & 0xffc0) == 0xfe80 <==> (0xfe80 <= s0 && s0 <= 0xfebf)) by (bit_vector);
    loopback || unspecified || ula || ll
}

// ---------------------------------------------------------------- theorems

/// A2: the mapped form of any IPv4 address is judged as that address.
pub proof fn mapped_same_as_v4(a: u32)
    ensures
        blocked_v6(mapped(a)) == blocked_v4(a),
{
    let v = mapped(a);
    assert(is_mapped(v));
    assert(mapped_v4(v) == a);
}

/// A3 (IPv4 side): every address of each named range is refused.
pub proof fn ranges_refused_v4(a: u32)
    ensures
        o1(a) == 127 ==> blocked_v4(a),
        o1(a) == 10 ==> blocked_v4(a),
        (o1(a) == 172 && 16 <= o2(a) <= 31) ==> blocked_v4(a),
        (o1(a) == 192 && o2(a) == 168) ==> blocked_v4(a),
        (o1(a) == 169 && o2(a) == 254) ==> blocked_v4(a),
        (o1(a) == 100 && 64 <= o2(a) <= 127) ==> blocked_v4(a),
        blocked_v4(0) && blocked_v4(0xffff_ffff),
        blocked_v4(0xa9fe_a9fe),  // 169.254.169.254
        blocked_v4(0x7f00_0001),  // 127.0.0.1
        !blocked_v4(0x0808_0808),  // 8.8.8.8 stays allowed (not vacuous)
{
}

/// A3 (IPv6 side), and the mapped forms of the IPv4 ranges.
pub proof fn ranges_refused_v6(v: V6, a: u32)
    ensures
        (0xfc00 <= v.s0 <= 0xfdff) ==> blocked_v6(v),
        (0xfe80 <= v.s0 <= 0xfebf) ==> blocked_v6(v),
        blocked_v4(a) ==> blocked_v6(mapped(a)),
        !blocked_v4(a) ==> !blocked_v6(mapped(a)),
{
    mapped_same_as_v4(a);
    if 0xfc00 <= v.s0 <= 0xfdff || 0xfe80 <= v.s0 <= 0xfebf {
        assert(!is_mapped(v));
    }
}

/// Witness for L1: the old code let ::ffff:127.0.0.1 and ::ffff:169.254.169.254 through.
pub proof fn witness_l1_mapped_loopback()
    ensures
        blocked_v6(mapped(0x7f00_0001)) && !private_ip_before_l1(mapped(0x7f00_0001)),
        blocked_v6(mapped(0xa9fe_a9fe)) && !private_ip_before_l1(mapped(0xa9fe_a9fe)),
{
    mapped_same_as_v4(0x7f00_0001);
    mapped_same_as_v4(0xa9fe_a9fe);
}

/// The fix only adds refusals: whatever the old code refused is still refused.
pub proof fn l1_only_tightens(v: V6)
    ensures
        private_ip_before_l1(v) ==> blocked_v6(v),
{
    if private_ip_before_l1(v) && is_mapped(v) {
        // ::ffff:x has s0 == 0 and is neither ::1 nor ::.
        assert(false);
    }
}

} // verus!

fn main() {}
