// Verus verification of core invariants of the NClientV3 Rust port.
// Mirrors algorithms from nclientv3-rust/core/src (which themselves mirror
// the original Java). Run: verus verus/verify_core.rs

use vstd::prelude::*;

verus! {

// ===== util.rs: Character.digit(ch, 16) =====

/// `Character.digit(ch, 16)` — total, matches Java for hex digits, -1 otherwise.
pub open spec fn hex_digit_value(c: char) -> i32
    decreases c,
{
    match c {
        '0' => 0i32, '1' => 1i32, '2' => 2i32, '3' => 3i32, '4' => 4i32,
        '5' => 5i32, '6' => 6i32, '7' => 7i32, '8' => 8i32, '9' => 9i32,
        'a' => 10i32, 'b' => 11i32, 'c' => 12i32, 'd' => 13i32, 'e' => 14i32, 'f' => 15i32,
        'A' => 10i32, 'B' => 11i32, 'C' => 12i32, 'D' => 13i32, 'E' => 14i32, 'F' => 15i32,
        _ => -1i32,
    }
}

/// Exec wrapper for the spec digit value (mode bridge).
pub exec fn d3_exec(c: char) -> (r: i32)
    ensures r == hex_digit_value(c),
{
    char_digit_exec(c)
}

/// Exec mirror of `char_digit` in core/src/util.rs.
pub exec fn char_digit_exec(c: char) -> (r: i32)
    ensures
        r == hex_digit_value(c),
        r >= -1 && r <= 15,
{
    match c {
        '0'..='9' => c as i32 - '0' as i32,
        'a'..='f' => c as i32 - 'a' as i32 + 10,
        'A'..='F' => c as i32 - 'A' as i32 + 10,
        _ => -1,
    }
}

/// The `\uXXXX` accumulator stays in [0, 65535] when all four digits are
/// valid hex — proving the Java `writer.write(toCreate)` never overflows a
/// UTF-16 code unit for well-formed escapes.
pub exec fn escape_accumulate(digits: [char; 4]) -> (acc: i32)
    requires
        hex_digit_value(digits[0]) >= 0 && hex_digit_value(digits[0]) <= 15,
        hex_digit_value(digits[1]) >= 0 && hex_digit_value(digits[1]) <= 15,
        hex_digit_value(digits[2]) >= 0 && hex_digit_value(digits[2]) <= 15,
        hex_digit_value(digits[3]) >= 0 && hex_digit_value(digits[3]) <= 15,
    ensures acc >= 0 && acc <= 0xFFFF,
{
    // concrete indices: d3_exec's ensures connects each to the spec digit
    let d0 = d3_exec(digits[0]);
    let d1 = d3_exec(digits[1]);
    let d2 = d3_exec(digits[2]);
    let d3 = d3_exec(digits[3]);
    proof {
        assert(d0 == hex_digit_value(digits[0]));
        assert(d1 == hex_digit_value(digits[1]));
        assert(d2 == hex_digit_value(digits[2]));
        assert(d3 == hex_digit_value(digits[3]));
        assert(d0 >= 0 && d0 <= 15 && d1 >= 0 && d1 <= 15
            && d2 >= 0 && d2 <= 15 && d3 >= 0 && d3 <= 15);
    }
    // Java: toCreate = toCreate*16 + digit, four times
    let mut acc: i32 = 0;
    acc = acc * 16 + d0;
    proof { assert(acc >= 0 && acc <= 15); }
    acc = acc * 16 + d1;
    proof { assert(acc >= 0 && acc <= 255); }
    acc = acc * 16 + d2;
    proof { assert(acc >= 0 && acc <= 4095); }
    acc = acc * 16 + d3;
    proof { assert(acc >= 0 && acc <= 0xFFFF); }
    acc
}

// ===== local_sort.rs: LocalSortType encode/decode =====

pub open spec fn kind_index(k: LocalSortKind) -> usize {
    match k {
        LocalSortKind::Title => 0,
        LocalSortKind::Date => 1,
        LocalSortKind::PageCount => 2,
        LocalSortKind::Artist => 3,
        LocalSortKind::Group => 4,
        LocalSortKind::Random => 5,
    }
}

/// Ghost-to-exec bridge for the enum's index (0..=5).
pub exec fn kind_idx(kind: LocalSortKind) -> (i: usize)
    ensures i == kind_index(kind) && i < 6,
{
    match kind {
        LocalSortKind::Title => 0,
        LocalSortKind::Date => 1,
        LocalSortKind::PageCount => 2,
        LocalSortKind::Artist => 3,
        LocalSortKind::Group => 4,
        LocalSortKind::Random => 5,
    }
}

/// The exec core of encode+decode in one function (mirrors
/// `LocalSortType::hash` then `LocalSortType::from_hash`), proven to
/// roundtrip: decode(encode(k, d)) == (k, d). Java's byte 0x80
/// sign-extension is exactly the u32 constant 0xFFFFFF80, and the low-7-bit
/// mask is 0x7F, so the u32 formulation is semantics-preserving.
pub exec fn roundtrip_check(k32: u32, descending: bool) -> (out: (u32, bool))
    requires k32 < 6u32,
    ensures out.0 == k32 && out.1 == descending,
{
    let h32: u32 = if descending { k32 | 0xFFFFFF80u32 } else { k32 };
    let masked: u32 = h32 & 127u32;
    let m: u32 = masked % 6;
    proof {
        assert(m == k32 && ((h32 & 0xFFFFFF80u32) != 0u32) == descending) by (bit_vector)
            requires
                masked == (h32 & 127u32),
                m == (masked % 6),
                (descending ==> h32 == (k32 | 0xFFFFFF80u32)),
                (!descending ==> h32 == k32),
                k32 < 6u32;
    }
    (m, (h32 & 0xFFFFFF80u32) != 0u32)
}

/// Decode totality at the u32 level (the Rust port computes the index from
/// `(hash as u32) & 0x7F % 6`): the modulo result is always < 6, so the
/// 6-entry enum lookup never indexes out of bounds.
pub exec fn decode_index(masked: u32) -> (idx: u32)
    ensures idx < 6u32,
{
    let m: u32 = masked % 6;
    proof {
        assert(m < 6u32) by (bit_vector)
            requires m == (masked % 6);
    }
    m
}

// ===== tag.rs: to_query_tag builder postconditions =====

/// Exec builder mirroring `Tag::to_query_tag_with` on a byte vector
/// (Verus-friendly subset of the String version). Postconditions capture the
/// structure Java relies on: optional `-` prefix, then `<type>:"<name>"`.
/// (No promise about out[0] when !avoided — a type name may itself start
/// with '-', like Java.)
pub exec fn build_query_tag(
    type_single: &Vec<u8>,
    name: &Vec<u8>,
    avoided: bool,
) -> (out: Vec<u8>)
    ensures
        out.len() == (if avoided { 1usize } else { 0usize }) + type_single.len() + 2
            + name.len() + 1,
        avoided ==> (out.len() > 0 && out@[0] == 45u8),
        out@[(if avoided { 1int } else { 0int }) + type_single.len() as int] == 58u8,
        out@[(if avoided { 1int } else { 0int }) + type_single.len() as int + 1] == 34u8,
        out@[(if avoided { 1int } else { 0int }) + type_single.len() as int + 1
            + name.len() as int + 1] == 34u8,
{
    let skip: usize = if avoided { 1 } else { 0 };
    let mut out: Vec<u8> = Vec::new();
    if avoided {
        out.push(45u8);
    }
    let mut i: usize = 0;
    while i < type_single.len()
        invariant
            i <= type_single.len(),
            out.len() == skip + i,
            avoided ==> (out.len() > 0 && out@[0] == 45u8),
            forall|j: int| 0 <= j < i ==> #[trigger] out@[skip + j] == type_single@[j],
        decreases type_single.len() - i,
    {
        out.push(type_single[i]);
        proof { assert(out@[skip + i as int] == type_single@[i as int]); }
        i += 1;
    }
    proof { assert(out.len() == skip + type_single.len()); }
    out.push(58u8);
    proof { assert(out@[skip + type_single.len() as int] == 58u8); }
    out.push(34u8);
    proof { assert(out@[skip + type_single.len() as int + 1] == 34u8); }
    let mut j: usize = 0;
    while j < name.len()
        invariant
            j <= name.len(),
            out.len() == skip + type_single.len() + 2 + j,
            avoided ==> (out.len() > 0 && out@[0] == 45u8),
            out@[skip + type_single.len()] == 58u8,
            out@[skip + type_single.len() + 1] == 34u8,
            forall|q: int| 0 <= q < j ==>
                #[trigger] out@[skip + type_single.len() + 2 + q] == name@[q],
        decreases name.len() - j,
    {
        out.push(name[j]);
        proof {
            assert(out@[skip + type_single.len() + 2 + j as int] == name@[j as int]);
        }
        j += 1;
    }
    out.push(34u8);
    proof {
        assert(out@[skip + type_single.len() as int + 1 + name.len() as int + 1] == 34u8);
    }
    out
}

// ===== enums.rs: TagType id roundtrip =====

pub open spec fn tagtype_from_id(id: u8) -> TagTypeV {
    match id {
        1 => TagTypeV::Parody,
        2 => TagTypeV::Character,
        3 => TagTypeV::Tag,
        4 => TagTypeV::Artist,
        5 => TagTypeV::Group,
        6 => TagTypeV::Language,
        7 => TagTypeV::Category,
        _ => TagTypeV::Unknown,
    }
}

pub exec fn tagtype_id_roundtrip(t: TagTypeV) -> (out: TagTypeV)
    ensures out == t,
{
    let id = t.id();
    proof {
        assert(tagtype_from_id(t.id_spec()) == t);
        assert(tagtype_from_id(id) == t);
    }
    match id {
        1 => TagTypeV::Parody,
        2 => TagTypeV::Character,
        3 => TagTypeV::Tag,
        4 => TagTypeV::Artist,
        5 => TagTypeV::Group,
        6 => TagTypeV::Language,
        7 => TagTypeV::Category,
        _ => TagTypeV::Unknown,
    }
}

/// Simplified stand-in for TagType (Verus enums without data).
pub enum TagTypeV {
    Unknown, Parody, Character, Tag, Artist, Group, Language, Category,
}

impl TagTypeV {
    pub exec fn id(&self) -> (id: u8)
        ensures id == self.id_spec(),
    {
        match self {
            TagTypeV::Unknown => 0,
            TagTypeV::Parody => 1,
            TagTypeV::Character => 2,
            TagTypeV::Tag => 3,
            TagTypeV::Artist => 4,
            TagTypeV::Group => 5,
            TagTypeV::Language => 6,
            TagTypeV::Category => 7,
        }
    }

    pub open spec fn id_spec(&self) -> u8 {
        match self {
            TagTypeV::Unknown => 0,
            TagTypeV::Parody => 1,
            TagTypeV::Character => 2,
            TagTypeV::Tag => 3,
            TagTypeV::Artist => 4,
            TagTypeV::Group => 5,
            TagTypeV::Language => 6,
            TagTypeV::Category => 7,
        }
    }

    pub open spec fn from_id(id: u8) -> TagTypeV {
        match id {
            1 => TagTypeV::Parody,
            2 => TagTypeV::Character,
            3 => TagTypeV::Tag,
            4 => TagTypeV::Artist,
            5 => TagTypeV::Group,
            6 => TagTypeV::Language,
            7 => TagTypeV::Category,
            _ => TagTypeV::Unknown,
        }
    }
}

pub enum LocalSortKind {
    Title,
    Date,
    PageCount,
    Artist,
    Group,
    Random,
}

fn main() {
    let (k, d) = roundtrip_check(5u32, true);
    assert(k == 5u32 && d);
    let (k2, d2) = roundtrip_check(2u32, false);
    assert(k2 == 2u32 && !d2);
    let mut ty: Vec<u8> = Vec::new();
    ty.push(116u8); ty.push(97u8); ty.push(103u8); // "tag"
    let mut nm: Vec<u8> = Vec::new();
    nm.push(98u8); nm.push(105u8); nm.push(103u8); nm.push(32u8);
    nm.push(98u8); nm.push(114u8); nm.push(101u8); nm.push(97u8);
    nm.push(115u8); nm.push(116u8); nm.push(115u8);
    let tag = build_query_tag(&ty, &nm, true);
    assert(tag[0] == 45u8);
    assert(tag[1 + ty.len() as int] == 58u8);
}

} // verus!
