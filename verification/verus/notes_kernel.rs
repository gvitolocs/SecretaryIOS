// Notes kernel: chunked append (line endings, order, size limit) and slug uniqueness.
//
// Production: crates/secretary-server/src/notes.rs
//   norm              <-> check_content's  c.replace("\r\n", "\n").replace('\r', "\n")
//   norm_keep / join  <-> check_content + NoteStore::append after the fix on verus-proofs:
//                         a lone '\r' at the very end is kept, and the next append starts
//                         with it, so a CRLF cut between two chunks is still one newline
//   append_exec       <-> NoteStore::append (separator, then the chunk; MAX_CONTENT_BYTES)
//   Note / notes      <-> the live notes (NoteStore's map); slug abstracted to a number
//   free_slug         <-> NoteStore::free_slug (base, base-2, base-3, ... first untaken)
//   create / update / delete / restore <-> NoteStore::{create, update, delete, restore}
// Differential test: crates/secretary-server/tests/verus_notes.rs (branch verus-proofs).
//
// Proven:
//   N1 chunked_equals_one_shot: appending the chunks of a text one by one (empty
//      separator, the MCP note_append continuation) stores exactly what one upload of the
//      whole text stores; order is preserved.
//   N2 append_exec: the stored content never exceeds MAX_CONTENT_BYTES (an append that
//      would is refused and changes nothing); normalising never lengthens a text.
//   N3 slug uniqueness: create (explicit or derived), update, delete and restore keep
//      "a slug names at most one live note"; free_slug always finds a free candidate
//      among n+1 distinct ones (n = number of notes), so its loop terminates. The
//      production tries after the base (base-2, base-3, ...) are pairwise distinct (the
//      differential test checks it), so n+2 tries hold n+1 distinct slugs; the base itself
//      can repeat one of them (a 63-64 byte base ending in "-2"), which only costs a try.
//
// Found (fixed on verus-proofs): before the fix each chunk was normalised alone, so a
// CRLF document cut between '\r' and '\n' gained a blank line:
//   append("x\r") then append("\ny") stored "x\n\ny", one upload stores "x\ny"
// (bad_split_crlf below).
use vstd::prelude::*;

verus! {

pub const CR: u8 = 13;
pub const LF: u8 = 10;
pub const MAX_CONTENT_BYTES: usize = 262144;

// ---------------------------------------------------------------- line endings

pub open spec fn norm(s: Seq<u8>) -> Seq<u8>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else if s[0] == CR {
        if s.len() >= 2 && s[1] == LF {
            seq![LF] + norm(s.skip(2))
        } else {
            seq![LF] + norm(s.skip(1))
        }
    } else {
        seq![s[0]] + norm(s.skip(1))
    }
}

/// norm, but a lone '\r' at the very end stays (the next chunk may start with '\n').
pub open spec fn norm_keep(s: Seq<u8>) -> Seq<u8>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else if s[0] == CR {
        if s.len() == 1 {
            seq![CR]
        } else if s[1] == LF {
            seq![LF] + norm_keep(s.skip(2))
        } else {
            seq![LF] + norm_keep(s.skip(1))
        }
    } else {
        seq![s[0]] + norm_keep(s.skip(1))
    }
}

pub open spec fn ends_cr(c: Seq<u8>) -> bool {
    c.len() > 0 && c[c.len() - 1] == CR
}

/// Stored content + the next raw chunk.
pub open spec fn join(c: Seq<u8>, chunk: Seq<u8>) -> Seq<u8> {
    if ends_cr(c) {
        c.drop_last() + norm_keep(seq![CR] + chunk)
    } else {
        c + norm_keep(chunk)
    }
}

pub open spec fn concat_all(chunks: Seq<Seq<u8>>) -> Seq<u8>
    decreases chunks.len(),
{
    if chunks.len() == 0 {
        Seq::empty()
    } else {
        concat_all(chunks.drop_last()) + chunks.last()
    }
}

pub open spec fn append_all(c: Seq<u8>, chunks: Seq<Seq<u8>>) -> Seq<u8>
    decreases chunks.len(),
{
    if chunks.len() == 0 {
        c
    } else {
        join(append_all(c, chunks.drop_last()), chunks.last())
    }
}

/// The output ends with '\r' only if the input does.
proof fn lemma_norm_keep_last(s: Seq<u8>)
    ensures
        ends_cr(norm_keep(s)) ==> ends_cr(s),
        norm_keep(s).len() <= s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        let o = norm_keep(s);
        if s[0] == CR && s.len() == 1 {
        } else if s[0] == CR && s[1] == LF {
            let r = s.skip(2);
            lemma_norm_keep_last(r);
            if r.len() == 0 {
                assert(o =~= seq![LF]);
            } else {
                assert(o[o.len() - 1] == norm_keep(r)[norm_keep(r).len() - 1]);
                assert(r[r.len() - 1] == s[s.len() - 1]);
            }
        } else {
            let r = s.skip(1);
            lemma_norm_keep_last(r);
            if r.len() == 0 {
                assert(s.len() == 1 && s[0] != CR);
                assert(o =~= seq![s[0]]);
            } else {
                assert(o[o.len() - 1] == norm_keep(r)[norm_keep(r).len() - 1]);
                assert(r[r.len() - 1] == s[s.len() - 1]);
            }
        }
    }
}

/// join distributes over a leading byte that is not '\r'.
proof fn lemma_join_cons(h: u8, c: Seq<u8>, b: Seq<u8>)
    requires
        h != CR,
    ensures
        join(seq![h] + c, b) == seq![h] + join(c, b),
{
    let hc = seq![h] + c;
    if c.len() > 0 {
        assert(hc[hc.len() - 1] == c[c.len() - 1]);
    }
    if ends_cr(c) {
        assert(hc.drop_last() =~= seq![h] + c.drop_last());
        assert(seq![h] + (c.drop_last() + norm_keep(seq![CR] + b)) =~= hc.drop_last() + norm_keep(seq![CR] + b));
    } else {
        assert(!ends_cr(hc));
        assert(seq![h] + (c + norm_keep(b)) =~= hc + norm_keep(b));
    }
}

/// The core: normalising a + b == normalising a, then joining b.
pub proof fn lemma_norm_keep_concat(a: Seq<u8>, b: Seq<u8>)
    ensures
        norm_keep(a + b) == join(norm_keep(a), b),
    decreases a.len(),
{
    let ab = a + b;
    if a.len() == 0 {
        assert(ab =~= b);
        assert(Seq::<u8>::empty() + norm_keep(b) =~= norm_keep(b));
    } else if a[0] != CR {
        let a1 = a.skip(1);
        lemma_norm_keep_concat(a1, b);
        assert(ab[0] == a[0]);
        assert(ab.skip(1) =~= a1 + b);
        lemma_join_cons(a[0], norm_keep(a1), b);
    } else if a.len() == 1 {
        assert(a =~= seq![CR]);
        assert(seq![CR].drop_last() + norm_keep(seq![CR] + b) =~= norm_keep(seq![CR] + b));
    } else if a[1] == LF {
        let a2 = a.skip(2);
        lemma_norm_keep_concat(a2, b);
        assert(ab[0] == CR && ab[1] == LF);
        assert(ab.skip(2) =~= a2 + b);
        lemma_join_cons(LF, norm_keep(a2), b);
    } else {
        let a1 = a.skip(1);
        lemma_norm_keep_concat(a1, b);
        assert(ab[0] == CR && ab[1] == a[1]);
        assert(ab.skip(1) =~= a1 + b);
        lemma_join_cons(LF, norm_keep(a1), b);
    }
}

/// N1: chunk-by-chunk appends store exactly what one upload of the whole text stores.
pub proof fn chunked_equals_one_shot(chunks: Seq<Seq<u8>>)
    ensures
        append_all(Seq::empty(), chunks) == norm_keep(concat_all(chunks)),
    decreases chunks.len(),
{
    if chunks.len() > 0 {
        chunked_equals_one_shot(chunks.drop_last());
        lemma_norm_keep_concat(concat_all(chunks.drop_last()), chunks.last());
    } else {
        assert(norm_keep(Seq::<u8>::empty()) =~= Seq::<u8>::empty());
    }
}

/// Without a trailing '\r', the kept form is the plain normal form.
pub proof fn lemma_norm_keep_is_norm(s: Seq<u8>)
    requires
        !ends_cr(s),
    ensures
        norm_keep(s) == norm(s),
    decreases s.len(),
{
    if s.len() > 0 {
        if s[0] == CR && s.len() >= 2 && s[1] == LF {
            let r = s.skip(2);
            if r.len() > 0 {
                assert(r[r.len() - 1] == s[s.len() - 1]);
            }
            lemma_norm_keep_is_norm(r);
        } else if s.len() >= 2 || s[0] != CR {
            let r = s.skip(1);
            if r.len() > 0 {
                assert(r[r.len() - 1] == s[s.len() - 1]);
            }
            lemma_norm_keep_is_norm(r);
        }
    }
}

/// The bug: normalising each chunk alone (old check_content + append).
pub proof fn bad_split_crlf()
    ensures
        ({
            let a = seq![120u8, CR];
            let b = seq![LF, 121u8];
            norm(a) + norm(b) != norm(a + b)
        }),
{
    let a = seq![120u8, CR];
    let b = seq![LF, 121u8];
    assert(norm(a.skip(1).skip(1)) =~= Seq::<u8>::empty());
    assert(norm(a.skip(1)) =~= seq![LF]);
    assert(norm(a) =~= seq![120u8, LF]);
    assert(norm(b.skip(1).skip(1)) =~= Seq::<u8>::empty());
    assert(norm(b.skip(1)) =~= seq![121u8]);
    assert(norm(b) =~= seq![LF, 121u8]);
    let ab = a + b;
    assert(ab =~= seq![120u8, CR, LF, 121u8]);
    assert(ab.skip(1).skip(2) =~= seq![121u8]);
    assert(norm(seq![121u8].skip(1)) =~= Seq::<u8>::empty());
    assert(norm(seq![121u8]) =~= seq![121u8]);
    assert(norm(ab.skip(1)) =~= seq![LF, 121u8]);
    assert(norm(ab) =~= seq![120u8, LF, 121u8]);
    assert((norm(a) + norm(b)).len() == 4);
}

// ---------------------------------------------------------------- append, exec

/// norm_keep as a loop.
pub fn normalize_keep(s: &Vec<u8>) -> (out: Vec<u8>)
    ensures
        out@ == norm_keep(s@),
{
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    proof {
        assert(s@.skip(0) =~= s@);
        assert(out@ + norm_keep(s@) =~= norm_keep(s@));
    }
    while i < s.len()
        invariant
            0 <= i <= s.len(),
            out@ + norm_keep(s@.skip(i as int)) == norm_keep(s@),
        decreases s.len() - i,
    {
        let ghost u = s@.skip(i as int);
        let ghost o0 = out@;
        if s[i] == CR {
            if i + 1 == s.len() {
                out.push(CR);
                proof {
                    assert(u =~= seq![CR]);
                    assert(s@.skip(i + 1) =~= Seq::<u8>::empty());
                    assert(o0 + norm_keep(u) =~= out@ + norm_keep(s@.skip(i + 1)));
                }
                i = i + 1;
            } else if s[i + 1] == LF {
                out.push(LF);
                proof {
                    assert(u.skip(2) =~= s@.skip(i + 2));
                    assert(o0 + norm_keep(u) =~= out@ + norm_keep(s@.skip(i + 2)));
                }
                i = i + 2;
            } else {
                out.push(LF);
                proof {
                    assert(u.skip(1) =~= s@.skip(i + 1));
                    assert(o0 + norm_keep(u) =~= out@ + norm_keep(s@.skip(i + 1)));
                }
                i = i + 1;
            }
        } else {
            out.push(s[i]);
            proof {
                assert(u.skip(1) =~= s@.skip(i + 1));
                assert(o0 + norm_keep(u) =~= out@ + norm_keep(s@.skip(i + 1)));
            }
            i = i + 1;
        }
    }
    proof {
        assert(s@.skip(i as int) =~= Seq::<u8>::empty());
        assert(out@ + Seq::<u8>::empty() =~= out@);
    }
    out
}

/// N2: NoteStore::append (with the fix). `chunk` = separator + text, raw.
pub fn append_exec(content: &Vec<u8>, chunk: &Vec<u8>) -> (r: Option<Vec<u8>>)
    requires
        content.len() <= MAX_CONTENT_BYTES,
    ensures
        match r {
            Some(c) => c@ == join(content@, chunk@) && c.len() <= MAX_CONTENT_BYTES,
            None => join(content@, chunk@).len() > MAX_CONTENT_BYTES,
        },
{
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    let n = content.len();
    let carry = n > 0 && content[n - 1] == CR;
    let keep = if carry {
        n - 1
    } else {
        n
    };
    while i < keep
        invariant
            keep <= content.len(),
            0 <= i <= keep,
            out@ == content@.subrange(0, i as int),
        decreases keep - i,
    {
        out.push(content[i]);
        i += 1;
        proof {
            assert(content@.subrange(0, i as int) =~= content@.subrange(0, i - 1).push(content@[i - 1]));
        }
    }
    let mut tail: Vec<u8> = Vec::new();
    if carry {
        tail.push(CR);
    }
    let mut j: usize = 0;
    let ghost t0 = tail@;
    while j < chunk.len()
        invariant
            0 <= j <= chunk.len(),
            tail@ == t0 + chunk@.subrange(0, j as int),
        decreases chunk.len() - j,
    {
        tail.push(chunk[j]);
        j += 1;
        proof {
            assert(chunk@.subrange(0, j as int) =~= chunk@.subrange(0, j - 1).push(chunk@[j - 1]));
        }
    }
    let norm_tail = normalize_keep(&tail);
    if norm_tail.len() > MAX_CONTENT_BYTES - out.len() {
        proof {
            assert(chunk@.subrange(0, chunk.len() as int) =~= chunk@);
            if carry {
                assert(content@.subrange(0, keep as int) =~= content@.drop_last());
                assert(t0 =~= seq![CR]);
            } else {
                assert(content@.subrange(0, keep as int) =~= content@);
                assert(t0 =~= Seq::<u8>::empty());
                assert(t0 + chunk@ =~= chunk@);
            }
        }
        return None;
    }
    let mut q: usize = 0;
    let ghost o0 = out@;
    while q < norm_tail.len()
        invariant
            0 <= q <= norm_tail.len(),
            out@ == o0 + norm_tail@.subrange(0, q as int),
            o0.len() + norm_tail.len() <= MAX_CONTENT_BYTES,
        decreases norm_tail.len() - q,
    {
        out.push(norm_tail[q]);
        q += 1;
        proof {
            assert(norm_tail@.subrange(0, q as int) =~= norm_tail@.subrange(0, q - 1).push(norm_tail@[q - 1]));
        }
    }
    proof {
        assert(chunk@.subrange(0, chunk.len() as int) =~= chunk@);
        assert(norm_tail@.subrange(0, q as int) =~= norm_tail@);
        if carry {
            assert(content@.subrange(0, keep as int) =~= content@.drop_last());
            assert(t0 =~= seq![CR]);
        } else {
            assert(content@.subrange(0, keep as int) =~= content@);
            assert(t0 =~= Seq::<u8>::empty());
            assert(t0 + chunk@ =~= chunk@);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- slugs

pub struct Note {
    pub id: nat,
    pub slug: nat,
}

pub open spec fn taken(notes: Seq<Note>, slug: nat) -> bool {
    exists|i: int| 0 <= i < notes.len() && (#[trigger] notes[i]).slug == slug
}

pub open spec fn taken_except(notes: Seq<Note>, slug: nat, idx: int) -> bool {
    exists|i: int| 0 <= i < notes.len() && i != idx && (#[trigger] notes[i]).slug == slug
}

/// A slug names at most one live note.
pub open spec fn unique_slugs(notes: Seq<Note>) -> bool {
    forall|i: int, j: int|
        0 <= i < notes.len() && 0 <= j < notes.len() && i != j ==> (#[trigger] notes[i]).slug != (
        #[trigger] notes[j]).slug
}

/// create with an explicit slug (409 slug_taken when taken), or with a free one.
pub proof fn create_keeps_unique(notes: Seq<Note>, n: Note)
    requires
        unique_slugs(notes),
        !taken(notes, n.slug),
    ensures
        unique_slugs(notes.push(n)),
{
    let t = notes.push(n);
    assert forall|i: int, j: int|
        0 <= i < t.len() && 0 <= j < t.len() && i != j implies (#[trigger] t[i]).slug != (#[trigger] t[j]).slug by {
        if i == notes.len() {
            assert(t[j] == notes[j]);
        } else if j == notes.len() {
            assert(t[i] == notes[i]);
        } else {
            assert(t[i] == notes[i] && t[j] == notes[j]);
        }
    }
}

/// update with a slug (409 slug_taken when another note has it).
pub proof fn update_keeps_unique(notes: Seq<Note>, idx: int, slug: nat)
    requires
        unique_slugs(notes),
        0 <= idx < notes.len(),
        !taken_except(notes, slug, idx),
    ensures
        unique_slugs(notes.update(idx, Note { id: notes[idx].id, slug })),
{
    let t = notes.update(idx, Note { id: notes[idx].id, slug });
    assert forall|i: int, j: int|
        0 <= i < t.len() && 0 <= j < t.len() && i != j implies (#[trigger] t[i]).slug != (#[trigger] t[j]).slug by {
        if i == idx {
            assert(t[j] == notes[j]);
        } else if j == idx {
            assert(t[i] == notes[i]);
        } else {
            assert(t[i] == notes[i] && t[j] == notes[j]);
        }
    }
}

/// delete (to the trash).
pub proof fn delete_keeps_unique(notes: Seq<Note>, idx: int)
    requires
        unique_slugs(notes),
        0 <= idx < notes.len(),
    ensures
        unique_slugs(notes.remove(idx)),
{
    let t = notes.remove(idx);
    assert forall|i: int, j: int|
        0 <= i < t.len() && 0 <= j < t.len() && i != j implies (#[trigger] t[i]).slug != (#[trigger] t[j]).slug by {
        let oi = if i < idx { i } else { i + 1 };
        let oj = if j < idx { j } else { j + 1 };
        assert(t[i] == notes[oi] && t[j] == notes[oj]);
    }
}

/// Some candidate among n+1 distinct ones is free when only n notes exist.
pub proof fn lemma_free_candidate_exists(notes: Seq<Note>, cands: Seq<nat>)
    requires
        cands.no_duplicates(),
        cands.len() > notes.len(),
    ensures
        exists|k: int| 0 <= k < cands.len() && !taken(notes, #[trigger] cands[k]),
{
    let slugs = notes.map_values(|n: Note| n.slug);
    if forall|k: int| 0 <= k < cands.len() ==> taken(notes, #[trigger] cands[k]) {
        assert(cands.to_set().subset_of(slugs.to_set())) by {
            assert forall|x: nat| cands.to_set().contains(x) implies slugs.to_set().contains(x) by {
                let k = choose|k: int| 0 <= k < cands.len() && cands[k] == x;
                assert(taken(notes, cands[k]));
                let i = choose|i: int| 0 <= i < notes.len() && (#[trigger] notes[i]).slug == cands[k];
                assert(slugs[i] == x);
            }
        }
        cands.unique_seq_to_set();
        slugs.lemma_cardinality_of_set();
        vstd::set_lib::lemma_len_subset(cands.to_set(), slugs.to_set());
        assert(false);
    }
}

/// free_slug as a loop over the candidates (base, base-2, base-3, ...): it returns a free
/// one, and it always finds one, so the production `for i in 2..` loop terminates.
pub fn free_slug(taken_slugs: &Vec<u64>, cands: &Vec<u64>) -> (r: u64)
    requires
        cands@.no_duplicates(),
        cands.len() > taken_slugs.len(),
    ensures
        cands@.contains(r),
        !taken_slugs@.contains(r),
{
    let ghost notes = taken_slugs@.map_values(|s: u64| Note { id: 0, slug: s as nat });
    let ghost cs = cands@.map_values(|c: u64| c as nat);
    proof {
        assert(cs.no_duplicates()) by {
            assert forall|a: int, b: int| 0 <= a < cs.len() && 0 <= b < cs.len() && a != b implies cs[a] != cs[b] by {
                assert(cands@[a] != cands@[b]);
            }
        }
        lemma_free_candidate_exists(notes, cs);
    }
    let mut k: usize = 0;
    while k < cands.len()
        invariant
            0 <= k <= cands.len(),
            notes == taken_slugs@.map_values(|s: u64| Note { id: 0, slug: s as nat }),
            cs == cands@.map_values(|c: u64| c as nat),
            forall|q: int| 0 <= q < k ==> taken(notes, #[trigger] cs[q]),
            exists|q: int| 0 <= q < cs.len() && !taken(notes, #[trigger] cs[q]),
        decreases cands.len() - k,
    {
        let c = cands[k];
        let mut t: usize = 0;
        let mut hit = false;
        while t < taken_slugs.len()
            invariant
                0 <= t <= taken_slugs.len(),
                hit ==> taken_slugs@.contains(c),
                !hit ==> forall|q: int| 0 <= q < t ==> taken_slugs@[q] != c,
            decreases taken_slugs.len() - t,
        {
            if taken_slugs[t] == c {
                hit = true;
            }
            t += 1;
        }
        if !hit {
            proof {
                assert(cands@[k as int] == c);
            }
            return c;
        }
        proof {
            let q = choose|q: int| 0 <= q < taken_slugs.len() && taken_slugs@[q] == c;
            assert(notes[q].slug == c as nat);
            assert(cs[k as int] == c as nat);
        }
        k += 1;
    }
    proof {
        let q = choose|q: int| 0 <= q < cs.len() && !taken(notes, #[trigger] cs[q]);
        assert(taken(notes, cs[q]));
    }
    0
}

} // verus!
fn main() {}
