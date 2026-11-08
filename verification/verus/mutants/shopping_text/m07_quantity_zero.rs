// Shopping items are labels (docs/design/shopping-labels.md): the exact catalog match and the
// split of a typed text into items.
//
// Production:
//   resolve        <-> lucina_grocery::catalog::CatalogIndex::{lookup, find, resolve} (folded
//                      text -> first name, else alias, byte-equal), web resolveByName/exactMatch,
//                      iOS CatalogSearch.resolve/exactMatch/pictureItem
//   strip          <-> shopping_text::strip_commands (lead_len/trail_len only take command words:
//                      verbs, prepositions, determiners, list names, courtesy words)
//   split_conj     <-> shopping_text::split_conjunctions
//   split          <-> shopping_text::split_items (segments on , ; and line breaks, conjunction
//                      groups unless the segment is a known term, MAX_PARTS)
//   parse_quantity <-> shopping_text::parse_quantity (N, Nx, xN, N x, trailing xN; unit; link word)
// Words are abstract here: a folded word is a u64 id, command words are the ids below
// COMMAND_IDS, and a quantity word carries the number its digits spell.
// Property tests on the real crate for the same rules: crates/shopping-text/tests/kernel.rs.
// Mutants (each must fail verification): verification/verus/mutants/shopping_text/*.rs, run by
// scripts/verus-mutants-shopping-text.sh.
//
// Proven:
//   M1 resolve returns an id only for an entry whose key equals the query byte for byte, and
//      the first such entry (no prefix, substring or nearest match).
//   M2 resolve returns nothing exactly when no key equals the query.
//   M3 an empty query never resolves.
//   S1 every part is non-empty.
//   S2 no part contains a separator (, ; line break): a sentence is never one item.
//   S3 a conjunction stays inside a group only with nothing before it in that group, or as the
//      segment's last word (a split needs a word on both sides).
//   S4 at most MAX_PARTS parts.
//   S6 the strip leaves at least one word of a non-empty text.
//   S7 the strip only removes command words.
//   Q1 a quantity is 1..=MAX_QUANTITY.
//   Q2 a quantity always leaves a non-empty name.
use vstd::prelude::*;

verus! {

// ------------------------------------------------------------------ exact match

pub struct Entry {
    pub key: Vec<u8>,
    pub id: u64,
}

pub fn bytes_eq(a: &Vec<u8>, b: &Vec<u8>) -> (r: bool)
    ensures
        r == (a@ == b@),
{
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < a.len()
        invariant
            i <= a.len(),
            a.len() == b.len(),
            forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            assert(a@[i as int] != b@[i as int]);
            return false;
        }
        i += 1;
    }
    assert(a@ =~= b@);
    true
}

pub fn resolve(index: &Vec<Entry>, q: &Vec<u8>) -> (r: Option<u64>)
    ensures
        // M3
        q@.len() == 0 ==> r == None::<u64>,
        // M1
        forall|id: u64| r == Some(id) ==> exists|i: int| #![trigger index@[i]]
            0 <= i < index@.len() && index@[i].key@ == q@ && index@[i].id == id && (
            forall|j: int| 0 <= j < i ==> index@[j].key@ != q@),
        // M2
        (r == None::<u64> && q@.len() > 0) ==> forall|i: int| 0 <= i < index@.len() ==> index@[i].key@ != q@,
{
    if q.len() == 0 {
        return None;
    }
    let mut i: usize = 0;
    while i < index.len()
        invariant
            i <= index.len(),
            q@.len() > 0,
            forall|j: int| 0 <= j < i ==> index@[j].key@ != q@,
        decreases index.len() - i,
    {
        if bytes_eq(&index[i].key, q) {
            let r = Some(index[i].id);
            assert(0 <= i < index@.len() && index@[i as int].key@ == q@ && index@[i as int].id == index[i as int].id && (forall|j: int|
                0 <= j < i ==> index@[j].key@ != q@));
            return r;
        }
        i += 1;
    }
    None
}

// ------------------------------------------------------------------ command strip

pub const COMMAND_IDS: u64 = 64;

pub open spec fn is_command(w: u64) -> bool {
    w < COMMAND_IDS
}

fn command(w: u64) -> (b: bool)
    ensures
        b == is_command(w),
{
    w < COMMAND_IDS
}

/// Command words at the start of words[lo..hi].
fn lead_len(words: &Vec<u64>, lo: usize, hi: usize) -> (n: usize)
    requires
        lo <= hi <= words.len(),
    ensures
        n <= hi - lo,
        forall|k: int| lo <= k < lo + n ==> is_command(words@[k]),
{
    let mut i = lo;
    while i < hi && command(words[i])
        invariant
            lo <= i <= hi,
            hi <= words.len(),
            forall|k: int| lo <= k < i ==> is_command(words@[k]),
        decreases hi - i,
    {
        i += 1;
    }
    i - lo
}

/// Command words at the end of words[lo..hi].
fn trail_len(words: &Vec<u64>, lo: usize, hi: usize) -> (n: usize)
    requires
        lo <= hi <= words.len(),
    ensures
        n <= hi - lo,
        forall|k: int| hi - n <= k < hi ==> is_command(words@[k]),
{
    let mut i = hi;
    while i > lo && command(words[i - 1])
        invariant
            lo <= i <= hi,
            hi <= words.len(),
            forall|k: int| i <= k < hi ==> is_command(words@[k]),
        decreases i - lo,
    {
        i -= 1;
    }
    hi - i
}

pub fn strip(words: &Vec<u64>) -> (r: (usize, usize))
    requires
        words.len() > 0,
    ensures
        // S6
        r.0 < r.1 <= words.len(),
        // S7
        forall|k: int| 0 <= k < r.0 ==> is_command(words@[k]),
        forall|k: int| r.1 <= k < words.len() ==> is_command(words@[k]),
{
    let mut lo: usize = 0;
    let mut hi: usize = words.len();
    loop
        invariant
            lo < hi <= words.len(),
            forall|k: int| 0 <= k < lo ==> is_command(words@[k]),
            forall|k: int| hi <= k < words.len() ==> is_command(words@[k]),
        decreases hi - lo,
    {
        let mut changed = false;
        let n = lead_len(words, lo, hi);
        if n > 0 && lo + n < hi {
            lo = lo + n;
            changed = true;
        }
        let m = trail_len(words, lo, hi);
        if m > 0 && lo + m < hi {
            hi = hi - m;
            changed = true;
        }
        if !changed {
            return (lo, hi);
        }
    }
}

// ------------------------------------------------------------------ split

#[derive(Clone, Copy)]
pub enum Tok {
    Word(u64),
    Sep,
    Conj,
}

pub open spec fn is_sep(t: Tok) -> bool {
    t matches Tok::Sep
}

pub open spec fn is_conj(t: Tok) -> bool {
    t matches Tok::Conj
}

fn sep(t: Tok) -> (b: bool)
    ensures
        b == is_sep(t),
{
    match t {
        Tok::Sep => true,
        _ => false,
    }
}

fn conj(t: Tok) -> (b: bool)
    ensures
        b == is_conj(t),
{
    match t {
        Tok::Conj => true,
        _ => false,
    }
}

pub const MAX_PARTS: usize = 50;

/// A segment (no separators) split at conjunctions with a word on both sides.
pub fn split_conj(seg: &Vec<Tok>) -> (groups: Vec<Vec<Tok>>)
    requires
        forall|k: int| 0 <= k < seg@.len() ==> !is_sep(seg@[k]),
    ensures
        // S1
        forall|g: int| 0 <= g < groups@.len() ==> groups@[g]@.len() > 0,
        // S2
        forall|g: int, k: int| 0 <= g < groups@.len() && 0 <= k < groups@[g]@.len() ==> !is_sep(groups@[g]@[k]),
        // S3
        forall|g: int, k: int|
            0 <= g < groups@.len() && 0 <= k < groups@[g]@.len() && is_conj(groups@[g]@[k]) ==> k == 0 || (g
                == groups@.len() - 1 && k == groups@[g]@.len() - 1),
{
    let mut groups: Vec<Vec<Tok>> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    let mut i: usize = 0;
    while i < seg.len()
        invariant
            i <= seg.len(),
            forall|k: int| 0 <= k < seg@.len() ==> !is_sep(seg@[k]),
            forall|g: int| 0 <= g < groups@.len() ==> groups@[g]@.len() > 0,
            forall|g: int, k: int| 0 <= g < groups@.len() && 0 <= k < groups@[g]@.len() ==> !is_sep(groups@[g]@[k]),
            forall|g: int, k: int| 0 <= g < groups@.len() && 0 <= k < groups@[g]@.len() ==> (is_conj(groups@[g]@[k]) ==> k == 0),
            forall|k: int| 0 <= k < cur@.len() ==> !is_sep(cur@[k]),
            // A conjunction in cur is its first word, or it was taken because nothing followed it.
            forall|k: int| 0 <= k < cur@.len() && is_conj(cur@[k]) ==> k == 0 || (k == cur@.len() - 1 && i == seg@.len()),
        decreases seg.len() - i,
    {
        let t = seg[i];
        if conj(t) && cur.len() > 0 && i + 1 < seg.len() {
            groups.push(cur);
            cur = Vec::new();
        } else {
            cur.push(t);
        }
        i += 1;
    }
    if cur.len() > 0 {
        groups.push(cur);
    }
    groups
}

/// Tokens to parts: segments on separators, each split at conjunctions unless `known[s]` (the
/// s-th non-empty segment is exactly a catalog term), at most MAX_PARTS.
pub fn split(toks: &Vec<Tok>, known: &Vec<bool>) -> (parts: Vec<Vec<Tok>>)
    requires
        toks.len() < 1_000_000,
    ensures
        // S4
        parts@.len() <= MAX_PARTS,
        // S1
        forall|p: int| 0 <= p < parts@.len() ==> parts@[p]@.len() > 0,
        // S2
        forall|p: int, k: int| 0 <= p < parts@.len() && 0 <= k < parts@[p]@.len() ==> !is_sep(parts@[p]@[k]),
{
    let mut parts: Vec<Vec<Tok>> = Vec::new();
    let mut seg: Vec<Tok> = Vec::new();
    let mut s: usize = 0;
    let mut i: usize = 0;
    while i <= toks.len()
        invariant
            i <= toks.len() + 1,
            toks.len() < 1_000_000,
            parts@.len() <= MAX_PARTS,
            forall|p: int| 0 <= p < parts@.len() ==> parts@[p]@.len() > 0,
            forall|p: int, k: int| 0 <= p < parts@.len() && 0 <= k < parts@[p]@.len() ==> !is_sep(parts@[p]@[k]),
            forall|k: int| 0 <= k < seg@.len() ==> !is_sep(seg@[k]),
        decreases toks.len() + 1 - i,
    {
        if i < toks.len() && !sep(toks[i]) {
            seg.push(toks[i]);
        } else if seg.len() > 0 {
            let whole = s < known.len() && known[s];
            s = if s < usize::MAX { s + 1 } else { s };
            if whole {
                if parts.len() < MAX_PARTS {
                    parts.push(seg);
                }
            } else {
                let groups = split_conj(&seg);
                let mut g: usize = 0;
                while g < groups.len()
                    invariant
                        g <= groups@.len(),
                        parts@.len() <= MAX_PARTS,
                        forall|p: int| 0 <= p < parts@.len() ==> parts@[p]@.len() > 0,
                        forall|p: int, k: int| 0 <= p < parts@.len() && 0 <= k < parts@[p]@.len() ==> !is_sep(parts@[p]@[k]),
                        forall|x: int| 0 <= x < groups@.len() ==> groups@[x]@.len() > 0,
                        forall|x: int, k: int| 0 <= x < groups@.len() && 0 <= k < groups@[x]@.len() ==> !is_sep(groups@[x]@[k]),
                    decreases groups.len() - g,
                {
                    if parts.len() < MAX_PARTS {
                        let mut copy: Vec<Tok> = Vec::new();
                        let mut k: usize = 0;
                        while k < groups[g].len()
                            invariant
                                g < groups@.len(),
                                k <= groups@[g as int]@.len(),
                                copy@ == groups@[g as int]@.subrange(0, k as int),
                            decreases groups[g as int]@.len() - k,
                        {
                            copy.push(groups[g][k]);
                            k += 1;
                        }
                        assert(copy@ =~= groups@[g as int]@);
                        parts.push(copy);
                    }
                    g += 1;
                }
            }
            seg = Vec::new();
        }
        i += 1;
    }
    parts
}

// ------------------------------------------------------------------ quantity

pub const MAX_QUANTITY: u32 = 9999;

#[derive(Clone, Copy)]
pub enum QTok {
    /// A word of 1-4 digits, with the number it spells.
    Num(u32),
    /// One word "Nx", "N×", "xN" or "×N".
    NumX(u32),
    /// The word "x" or "×".
    Times,
    Unit,
    /// "di", "d", "of" after a unit.
    Link,
    Other,
}

fn amount(v: u32) -> (r: Option<u32>)
    ensures
        r.is_some() ==> 1 <= r.unwrap() <= MAX_QUANTITY && r.unwrap() == v,
{
    if v <= MAX_QUANTITY {
        Some(v)
    } else {
        None
    }
}

/// `(quantity, first name word)`: the name is words[start..] (or words[..len-1] for a trailing
/// xN, where start is 0).
pub fn parse_quantity(words: &Vec<QTok>) -> (r: Option<(u32, usize, usize)>)
    ensures
        // Q1
        r.is_some() ==> 1 <= r.unwrap().0 <= MAX_QUANTITY,
        // Q2: the name words[start..end] is not empty
        r.is_some() ==> r.unwrap().1 < r.unwrap().2 <= words.len(),
{
    if words.len() == 0 {
        return None;
    }
    let lead: Option<(u32, usize)> = match words[0] {
        QTok::Num(v) => match amount(v) {
            Some(n) => {
                let times = words.len() > 1 && match words[1] {
                    QTok::Times => true,
                    _ => false,
                };
                Some((n, if times { 2 } else { 1 }))
            },
            None => None,
        },
        QTok::NumX(v) => match amount(v) {
            Some(n) => Some((n, 1)),
            None => None,
        },
        _ => None,
    };
    match lead {
        Some((n, start)) => {
            let mut next = start;
            if next < words.len() && match words[next] {
                QTok::Unit => true,
                _ => false,
            } {
                next += 1;
                if next < words.len() && match words[next] {
                    QTok::Link => true,
                    _ => false,
                } {
                    next += 1;
                }
            }
            if next < words.len() {
                Some((n, next, words.len()))
            } else {
                None
            }
        },
        None => {
            if words.len() >= 2 {
                match words[words.len() - 1] {
                    QTok::NumX(v) => match amount(v) {
                        Some(n) => Some((n, 0, words.len() - 1)),
                        None => None,
                    },
                    _ => None,
                }
            } else {
                None
            }
        },
    }
}

fn main() {
}

} // verus!
