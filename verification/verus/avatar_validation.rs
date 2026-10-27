// Avatar settings validation kernel (PUT /v1/avatar).
//
// Production: crates/secretary-server/src/avatar.rs
//   valid_id_spec / valid_id      <-> avatar::valid_id
//   validate_changes (this file)  <-> avatar::validate_changes(s, w, prev)
//   Entry                         <-> one (field, id) pair of the new settings, in the
//                                     order validate_changes builds them: outfits
//                                     (outfit, rules.{gym,shopping,default}.outfit),
//                                     hairs (hair, rules.*.hair), colours (hairColor)
//   Kept                          <-> the `kept` set built from `prev`
//   ko / kh / kc                  <-> ids(&w.outfits) / ids(&w.hairstyles) / ids(&w.colors)
//   natural()                     <-> NATURAL_HAIR
// Differential test: crates/secretary-server/tests/verus_avatar.rs (proptest, the
// production function against a plain-Rust copy of `validate_changes` below).
//
// Proven:
//   A1 rejection_names_a_new_id: every rejection points at a (field, id) pair that is
//      not in `prev` (given the stored settings are shape-valid, which
//      AvatarStore::load guarantees): unchanged ids never cause a rejection.
//   A2 new_unknown_id_is_rejected: a newly introduced id that is not in a non-empty
//      wardrobe list (and is not `natural` for a hair field) is rejected.
//   A3 natural_is_never_the_cause: `natural` in a hair field is never what a
//      rejection names (its shape is valid and it is always a known hairstyle).
//   A4 valid_id is exactly `[a-z0-9][a-z0-9_-]*`, 1..=40 bytes.
//   A5 accept_iff: the kernel accepts exactly when every id is well formed and every
//      new id is known (or its list is empty).
use vstd::prelude::*;

verus! {

pub const OUTFIT: u8 = 0;
pub const HAIR: u8 = 1;
pub const COLOR: u8 = 2;
pub const MAX_ID_LEN: usize = 40;

pub struct Entry {
    /// Field slot (0 outfit, 1 hair, 2 hairColor, 3.. rules.*): the `field` string.
    pub slot: u8,
    pub kind: u8,
    pub id: Vec<u8>,
}

pub struct Kept {
    pub slot: u8,
    pub id: Vec<u8>,
}

pub enum Verdict {
    Accept,
    /// `bad_request`: the entry's id is malformed.
    BadShape(usize),
    /// `unknown_outfit` / `unknown_hairstyle` / `unknown_hair_color`.
    Unknown(usize),
}

pub open spec fn first_ok(c: u8) -> bool {
    (97 <= c && c <= 122) || (48 <= c && c <= 57)
}

pub open spec fn rest_ok(c: u8) -> bool {
    first_ok(c) || c == 95 || c == 45
}

pub open spec fn valid_id_spec(id: Seq<u8>) -> bool {
    0 < id.len() && id.len() <= MAX_ID_LEN && first_ok(id[0]) && forall|i: int|
        0 <= i < id.len() ==> rest_ok(#[trigger] id[i])
}

/// "natural"
pub open spec fn natural() -> Seq<u8> {
    seq![110u8, 97u8, 116u8, 117u8, 114u8, 97u8, 108u8]
}

pub open spec fn kept_has(kept: Seq<Kept>, slot: u8, id: Seq<u8>) -> bool {
    exists|j: int| 0 <= j < kept.len() && (#[trigger] kept[j]).slot == slot && kept[j].id@ == id
}

pub open spec fn known_has(known: Seq<Vec<u8>>, id: Seq<u8>) -> bool {
    exists|j: int| 0 <= j < known.len() && (#[trigger] known[j])@ == id
}

/// Is the id acceptable for its kind (ignoring whether it is new)?
pub open spec fn accepted(e: Entry, ko: Seq<Vec<u8>>, kh: Seq<Vec<u8>>, kc: Seq<Vec<u8>>) -> bool {
    if e.kind == OUTFIT {
        ko.len() == 0 || known_has(ko, e.id@)
    } else if e.kind == HAIR {
        kh.len() == 0 || e.id@ == natural() || known_has(kh, e.id@)
    } else {
        kc.len() == 0 || known_has(kc, e.id@)
    }
}

pub open spec fn is_new(e: Entry, kept: Seq<Kept>) -> bool {
    !kept_has(kept, e.slot, e.id@)
}

pub open spec fn all_valid_upto(es: Seq<Entry>, n: int) -> bool {
    forall|k: int| 0 <= k < n ==> valid_id_spec((#[trigger] es[k]).id@)
}

pub open spec fn new_ok_upto(
    es: Seq<Entry>,
    n: int,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
) -> bool {
    forall|k: int| 0 <= k < n ==> (is_new(#[trigger] es[k], kept) ==> accepted(es[k], ko, kh, kc))
}

/// The contract of validate_changes, as a predicate the theorems below use.
pub open spec fn post(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
) -> bool {
    match r {
        Verdict::Accept => all_valid_upto(es, es.len() as int) && new_ok_upto(
            es,
            es.len() as int,
            kept,
            ko,
            kh,
            kc,
        ),
        Verdict::BadShape(i) => i < es.len() && !valid_id_spec(es[i as int].id@) && all_valid_upto(
            es,
            i as int,
        ),
        Verdict::Unknown(i) => i < es.len() && all_valid_upto(es, es.len() as int) && is_new(
            es[i as int],
            kept,
        ) && !accepted(es[i as int], ko, kh, kc) && new_ok_upto(es, i as int, kept, ko, kh, kc),
    }
}

// ---------------------------------------------------------------- exec kernel

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
            a.len() == b.len(),
            0 <= i <= a.len(),
            forall|k: int| 0 <= k < i ==> a@[k] == b@[k],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    assert(a@ =~= b@);
    true
}

/// avatar::valid_id
pub fn valid_id(id: &Vec<u8>) -> (r: bool)
    ensures
        r == valid_id_spec(id@),
{
    if id.len() == 0 || id.len() > MAX_ID_LEN {
        return false;
    }
    let c0 = id[0];
    if !((97 <= c0 && c0 <= 122) || (48 <= c0 && c0 <= 57)) {
        return false;
    }
    let mut i: usize = 0;
    while i < id.len()
        invariant
            0 <= i <= id.len(),
            forall|k: int| 0 <= k < i ==> rest_ok(#[trigger] id@[k]),
        decreases id.len() - i,
    {
        let c = id[i];
        if !((97 <= c && c <= 122) || (48 <= c && c <= 57) || c == 95 || c == 45) {
            assert(!rest_ok(id@[i as int]));
            return false;
        }
        i += 1;
    }
    true
}

pub fn is_natural(id: &Vec<u8>) -> (r: bool)
    ensures
        r == (id@ == natural()),
{
    let r = id.len() == 7 && id[0] == 110 && id[1] == 97 && id[2] == 116 && id[3] == 117 && id[4]
        == 114 && id[5] == 97 && id[6] == 108;
    proof {
        if r {
            assert(id@ =~= natural());
        } else if id@ == natural() {
            assert(natural().len() == 7);
            assert(id@[0] == 110u8 && id@[1] == 97u8 && id@[2] == 116u8 && id@[3] == 117u8);
            assert(id@[4] == 114u8 && id@[5] == 97u8 && id@[6] == 108u8);
        }
    }
    r
}

pub fn known_contains(known: &Vec<Vec<u8>>, id: &Vec<u8>) -> (r: bool)
    ensures
        r == known_has(known@, id@),
{
    let mut j: usize = 0;
    while j < known.len()
        invariant
            0 <= j <= known.len(),
            forall|k: int| 0 <= k < j ==> (#[trigger] known@[k])@ != id@,
        decreases known.len() - j,
    {
        if bytes_eq(&known[j], id) {
            assert(known@[j as int]@ == id@);
            return true;
        }
        j += 1;
    }
    false
}

pub fn kept_contains(kept: &Vec<Kept>, slot: u8, id: &Vec<u8>) -> (r: bool)
    ensures
        r == kept_has(kept@, slot, id@),
{
    let mut j: usize = 0;
    while j < kept.len()
        invariant
            0 <= j <= kept.len(),
            forall|k: int|
                0 <= k < j ==> !((#[trigger] kept@[k]).slot == slot && kept@[k].id@ == id@),
        decreases kept.len() - j,
    {
        if kept[j].slot == slot && bytes_eq(&kept[j].id, id) {
            assert(kept@[j as int].slot == slot && kept@[j as int].id@ == id@);
            return true;
        }
        j += 1;
    }
    false
}

pub fn accepted_exec(e: &Entry, ko: &Vec<Vec<u8>>, kh: &Vec<Vec<u8>>, kc: &Vec<Vec<u8>>) -> (r:
    bool)
    ensures
        r == accepted(*e, ko@, kh@, kc@),
{
    if e.kind == OUTFIT {
        ko.len() == 0 || known_contains(ko, &e.id)
    } else if e.kind == HAIR {
        kh.len() == 0 || is_natural(&e.id) || known_contains(kh, &e.id)
    } else {
        kc.len() == 0 || known_contains(kc, &e.id)
    }
}

/// avatar::validate_changes: shape of every id first, then (per kind, in entry
/// order) the first new id that is not in its non-empty wardrobe list.
pub fn validate_changes(
    es: &Vec<Entry>,
    kept: &Vec<Kept>,
    ko: &Vec<Vec<u8>>,
    kh: &Vec<Vec<u8>>,
    kc: &Vec<Vec<u8>>,
) -> (r: Verdict)
    ensures
        post(r, es@, kept@, ko@, kh@, kc@),
{
    let mut i: usize = 0;
    while i < es.len()
        invariant
            0 <= i <= es.len(),
            all_valid_upto(es@, i as int),
        decreases es.len() - i,
    {
        if !valid_id(&es[i].id) {
            return Verdict::BadShape(i);
        }
        i += 1;
    }
    let mut i: usize = 0;
    while i < es.len()
        invariant
            0 <= i <= es.len(),
            all_valid_upto(es@, es.len() as int),
            new_ok_upto(es@, i as int, kept@, ko@, kh@, kc@),
        decreases es.len() - i,
    {
        let e = &es[i];
        if !kept_contains(kept, e.slot, &e.id) {
            if !accepted_exec(e, ko, kh, kc) {
                return Verdict::Unknown(i);
            }
        }
        i += 1;
    }
    Verdict::Accept
}

// ---------------------------------------------------------------- theorems

/// A4: `natural` is a well-formed id.
pub proof fn natural_is_valid()
    ensures
        valid_id_spec(natural()),
{
    let n = natural();
    assert(n.len() == 7);
    assert forall|i: int| 0 <= i < n.len() implies rest_ok(#[trigger] n[i]) by {
        assert(n[0] == 110u8 && n[1] == 97u8 && n[2] == 116u8 && n[3] == 117u8);
        assert(n[4] == 114u8 && n[5] == 97u8 && n[6] == 108u8);
    }
}

/// A1: with shape-valid stored settings, every rejection names a (field, id)
/// pair that was not in `prev`.
pub proof fn rejection_names_a_new_id(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
)
    requires
        post(r, es, kept, ko, kh, kc),
        forall|j: int| 0 <= j < kept.len() ==> valid_id_spec((#[trigger] kept[j]).id@),
    ensures
        match r {
            Verdict::Accept => true,
            Verdict::BadShape(i) => is_new(es[i as int], kept),
            Verdict::Unknown(i) => is_new(es[i as int], kept),
        },
{
    match r {
        Verdict::BadShape(i) => {
            if kept_has(kept, es[i as int].slot, es[i as int].id@) {
                let j = choose|j: int|
                    0 <= j < kept.len() && (#[trigger] kept[j]).slot == es[i as int].slot
                        && kept[j].id@ == es[i as int].id@;
                assert(valid_id_spec(kept[j].id@));
            }
        },
        _ => {},
    }
}

/// A1 restated: if no id is new, the update is accepted.
pub proof fn unchanged_ids_are_accepted(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
)
    requires
        post(r, es, kept, ko, kh, kc),
        forall|j: int| 0 <= j < kept.len() ==> valid_id_spec((#[trigger] kept[j]).id@),
        forall|i: int| 0 <= i < es.len() ==> !is_new(#[trigger] es[i], kept),
    ensures
        r is Accept,
{
    rejection_names_a_new_id(r, es, kept, ko, kh, kc);
}

/// A2: a new, well-formed id missing from its non-empty wardrobe list (and not
/// `natural` for hair) makes the update fail.
pub proof fn new_unknown_id_is_rejected(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
    i: int,
)
    requires
        post(r, es, kept, ko, kh, kc),
        0 <= i < es.len(),
        is_new(es[i], kept),
        !accepted(es[i], ko, kh, kc),
    ensures
        !(r is Accept),
{
    if r is Accept {
        assert(new_ok_upto(es, es.len() as int, kept, ko, kh, kc));
        assert(is_new(es[i], kept) ==> accepted(es[i], ko, kh, kc));
    }
}

/// A3: `natural` in a hair field is never the id a rejection names.
pub proof fn natural_is_never_the_cause(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
)
    requires
        post(r, es, kept, ko, kh, kc),
    ensures
        match r {
            Verdict::Accept => true,
            Verdict::BadShape(i) => es[i as int].id@ != natural(),
            Verdict::Unknown(i) => !(es[i as int].kind == HAIR && es[i as int].id@ == natural()),
        },
{
    natural_is_valid();
}

/// A5: accept iff all ids are well formed and all new ids are acceptable.
pub proof fn accept_iff(
    r: Verdict,
    es: Seq<Entry>,
    kept: Seq<Kept>,
    ko: Seq<Vec<u8>>,
    kh: Seq<Vec<u8>>,
    kc: Seq<Vec<u8>>,
)
    requires
        post(r, es, kept, ko, kh, kc),
    ensures
        (r is Accept) == (all_valid_upto(es, es.len() as int) && new_ok_upto(
            es,
            es.len() as int,
            kept,
            ko,
            kh,
            kc,
        )),
{
    match r {
        Verdict::BadShape(i) => {
            assert(!valid_id_spec(es[i as int].id@));
        },
        Verdict::Unknown(i) => {
            assert(is_new(es[i as int], kept) && !accepted(es[i as int], ko, kh, kc));
        },
        Verdict::Accept => {},
    }
}

} // verus!
fn main() {}
