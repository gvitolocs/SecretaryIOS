//! Differential tests for notes against a plain-Rust copy of the Verus
//! kernel `verification/verus/notes_kernel.rs`: chunked appends (line
//! endings, order, size limit) and slug assignment (create, update, delete,
//! restore) on a real `NoteStore`, plus the kernel theorems on production.

use proptest::prelude::*;
use secretary_server::notes::{
    slugify, AppendNote, Caller, CreateNote, NoteStore, UpdateNote, MAX_CONTENT_BYTES, MAX_SLUG_LEN,
};

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    pub const CR: u8 = 13;
    pub const LF: u8 = 10;

    pub fn normalize_keep(s: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.len() {
            if s[i] == CR {
                if i + 1 == s.len() {
                    out.push(CR);
                    i += 1;
                } else if s[i + 1] == LF {
                    out.push(LF);
                    i += 2;
                } else {
                    out.push(LF);
                    i += 1;
                }
            } else {
                out.push(s[i]);
                i += 1;
            }
        }
        out
    }

    pub fn append_exec(content: &[u8], chunk: &[u8], max: usize) -> Option<Vec<u8>> {
        let carry = content.last() == Some(&CR);
        let keep = if carry {
            content.len() - 1
        } else {
            content.len()
        };
        let mut out = content[..keep].to_vec();
        let mut tail = Vec::new();
        if carry {
            tail.push(CR);
        }
        tail.extend_from_slice(chunk);
        let norm_tail = normalize_keep(&tail);
        if norm_tail.len() > max - out.len() {
            return None;
        }
        out.extend_from_slice(&norm_tail);
        Some(out)
    }

    /// free_slug over the candidates.
    pub fn free_slug(taken: &[String], cands: &[String]) -> String {
        for c in cands {
            if !taken.contains(c) {
                return c.clone();
            }
        }
        unreachable!("n + 1 distinct candidates, n taken")
    }
}

/// The candidates NoteStore::free_slug tries: base, base-2, base-3, ...,
/// without repeats (a 63-64 byte base ending in `-2` comes back as the
/// truncated `base-2`), enough for n taken slugs: n + 2 tries, of which at
/// least n + 1 are distinct (the kernel's precondition).
fn candidates(base: &str, n: usize) -> Vec<String> {
    let mut v = vec![base.to_owned()];
    for i in 2..n + 3 {
        let suffix = format!("-{i}");
        let mut b: String = base.to_owned();
        b.truncate(MAX_SLUG_LEN - suffix.len());
        while b.ends_with('-') {
            b.pop();
        }
        let c = format!("{b}{suffix}");
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

fn store() -> (tempfile::TempDir, NoteStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = NoteStore::new(dir.path());
    (dir, s)
}

fn create(
    s: &NoteStore,
    title: &str,
    content: &str,
    slug: Option<&str>,
) -> Result<secretary_server::notes::Note, secretary_server::error::ApiError> {
    s.create(
        CreateNote {
            title: title.into(),
            content: content.into(),
            slug: slug.map(str::to_owned),
            ..CreateNote::default()
        },
        Caller("owner"),
    )
}

fn append(
    s: &NoteStore,
    key: &str,
    chunk: &str,
) -> Result<secretary_server::notes::Note, secretary_server::error::ApiError> {
    s.append(
        key,
        AppendNote {
            content: chunk.into(),
            separator: None,
            source: None,
        },
    )
}

fn chunk() -> impl Strategy<Value = String> {
    "[ab\r\n ]{1,6}"
}

#[derive(Debug, Clone)]
enum SlugOp {
    Create {
        title: usize,
        explicit: Option<usize>,
    },
    Update {
        idx: usize,
        slug: usize,
    },
    Delete {
        idx: usize,
    },
    Restore {
        idx: usize,
    },
}

const TITLES: [&str; 5] = [
    "Meeting",
    "Meeting!",
    "Trash",
    "Notes 2",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
];
const SLUGS: [&str; 4] = ["meeting", "meeting-2", "trash-note", "notes-2"];

fn slug_ops() -> impl Strategy<Value = Vec<SlugOp>> {
    let op = prop_oneof![
        4 => (0usize..5, proptest::option::of(0usize..4)).prop_map(|(title, explicit)| SlugOp::Create { title, explicit }),
        2 => (0usize..8, 0usize..4).prop_map(|(idx, slug)| SlugOp::Update { idx, slug }),
        1 => (0usize..8).prop_map(|idx| SlugOp::Delete { idx }),
        1 => (0usize..8).prop_map(|idx| SlugOp::Restore { idx }),
    ];
    proptest::collection::vec(op, 1..25)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// N1 + N2 on production: the stored content == the kernel's, and == one
    /// upload of the concatenated chunks; order is preserved.
    #[test]
    fn chunked_appends_match_kernel_and_one_shot(chunks in proptest::collection::vec(chunk(), 1..8)) {
        let (_d, s) = store();
        let first = create(&s, "Doc", &chunks[0], Some("doc")).unwrap();
        let mut k = kernel::append_exec(b"", chunks[0].as_bytes(), MAX_CONTENT_BYTES).unwrap();
        prop_assert_eq!(first.content.as_bytes(), &k[..]);
        for c in &chunks[1..] {
            let n = append(&s, "doc", c).unwrap();
            k = kernel::append_exec(&k, c.as_bytes(), MAX_CONTENT_BYTES).unwrap();
            prop_assert_eq!(n.content.as_bytes(), &k[..], "after {:?}", c);
        }
        let whole = create(&s, "Whole", &chunks.concat(), Some("whole")).unwrap();
        prop_assert_eq!(&s.get("doc").unwrap().content, &whole.content);
        prop_assert_eq!(whole.content.as_bytes(), &kernel::normalize_keep(chunks.concat().as_bytes())[..]);
    }

    /// N3 on production: slugs assigned like the kernel's free_slug, errors where the
    /// kernel refuses, and never two live notes with the same slug.
    #[test]
    fn slugs_match_kernel(ops in slug_ops()) {
        let (_d, s) = store();
        let mut live: Vec<(String, String)> = Vec::new(); // (id, slug)
        let mut trash: Vec<(String, String)> = Vec::new();
        for op in ops {
            let taken: Vec<String> = live.iter().map(|(_, sl)| sl.clone()).collect();
            match op {
                SlugOp::Create { title, explicit } => {
                    let t = TITLES[title];
                    match explicit {
                        Some(e) => {
                            let r = create(&s, t, "", Some(SLUGS[e]));
                            if taken.iter().any(|x| x == SLUGS[e]) {
                                prop_assert_eq!(r.unwrap_err().code, "slug_taken");
                            } else {
                                let n = r.unwrap();
                                prop_assert_eq!(&n.slug, SLUGS[e]);
                                live.push((n.id, n.slug));
                            }
                        }
                        None => {
                            let base = { let b = slugify(t); if b.is_empty() { "note".to_owned() } else { b } };
                            let want = kernel::free_slug(&taken, &candidates(&base, taken.len()));
                            let n = create(&s, t, "", None).unwrap();
                            prop_assert_eq!(&n.slug, &want);
                            live.push((n.id, n.slug));
                        }
                    }
                }
                SlugOp::Update { idx, slug } => {
                    if live.is_empty() { continue; }
                    let i = idx % live.len();
                    let id = live[i].0.clone();
                    let r = s.update(&id, UpdateNote { slug: Some(SLUGS[slug].into()), ..UpdateNote::default() });
                    if live.iter().enumerate().any(|(j, (_, sl))| j != i && sl == SLUGS[slug]) {
                        prop_assert_eq!(r.unwrap_err().code, "slug_taken");
                    } else {
                        prop_assert_eq!(&r.unwrap().slug, SLUGS[slug]);
                        live[i].1 = SLUGS[slug].into();
                    }
                }
                SlugOp::Delete { idx } => {
                    if live.is_empty() { continue; }
                    let (id, slug) = live.remove(idx % live.len());
                    s.delete(&id).unwrap();
                    trash.push((id, slug));
                }
                SlugOp::Restore { idx } => {
                    if trash.is_empty() { continue; }
                    let (id, old) = trash.remove(idx % trash.len());
                    let want = kernel::free_slug(&taken, &candidates(&old, taken.len()));
                    let n = s.restore(&id).unwrap();
                    prop_assert_eq!(&n.slug, &want);
                    live.push((n.id, n.slug));
                }
            }
            let listed: Vec<String> = s.list(None, None, 500).into_iter().map(|n| n.slug).collect();
            let mut uniq = listed.clone();
            uniq.sort();
            uniq.dedup();
            prop_assert_eq!(uniq.len(), listed.len(), "two live notes share a slug: {:?}", listed);
            prop_assert_eq!(listed.len(), live.len());
        }
    }

    /// The kernel's precondition on free_slug: after the base, the tries are
    /// pairwise distinct, so n + 2 tries hold n + 1 distinct slugs.
    #[test]
    fn slug_candidates_are_distinct(base in "[a-z0-9]([a-z0-9]|-[a-z0-9]){0,40}", n in 0usize..150) {
        let all: Vec<String> = (2..n + 3)
            .map(|i| {
                let suffix = format!("-{i}");
                let mut b = base.clone();
                b.truncate(MAX_SLUG_LEN - suffix.len());
                while b.ends_with('-') {
                    b.pop();
                }
                format!("{b}{suffix}")
            })
            .collect();
        let mut u = all.clone();
        u.sort();
        u.dedup();
        prop_assert_eq!(u.len(), all.len());
        prop_assert!(candidates(&base, n).len() > n);
    }
}

/// The counterexample of the kernel (bad_split_crlf), fixed.
#[test]
fn crlf_cut_between_two_appends_is_one_newline() {
    let (_d, s) = store();
    create(&s, "Doc", "x\r", Some("doc")).unwrap();
    let n = append(&s, "doc", "\ny").unwrap();
    assert_eq!(n.content, "x\ny");
}

/// N2: an append past the limit is refused and changes nothing.
#[test]
fn append_past_the_limit_is_refused() {
    let (_d, s) = store();
    let big = "a".repeat(MAX_CONTENT_BYTES - 10);
    create(&s, "Big", &big, Some("big")).unwrap();
    assert_eq!(
        append(&s, "big", &"b".repeat(11)).unwrap_err().code,
        "note_too_large"
    );
    assert_eq!(s.get("big").unwrap().content.len(), MAX_CONTENT_BYTES - 10);
    assert_eq!(
        append(&s, "big", &"b".repeat(10)).unwrap().content.len(),
        MAX_CONTENT_BYTES
    );
    assert_eq!(
        kernel::append_exec(big.as_bytes(), &[b'b'; 11], MAX_CONTENT_BYTES),
        None
    );
}
