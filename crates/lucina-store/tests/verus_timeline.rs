//! Differential tests for timeline ids against a plain-Rust copy of the
//! Verus kernel `verification/verus/timeline_ids.rs` (branch verus-proofs):
//! `ulid::successor` vs `successor_digits`, string order vs numeric order,
//! `ulid::new_id` monotonicity, and `TimelineStore::append` vs `choose_id`
//! with foreign lines (the Node writer), rotations and truncation in between.

use std::io::Write;

use lucina_store::timeline::DEFAULT_MAX_BYTES;
use lucina_store::ulid::{is_timeline_id, new_id, successor};
use lucina_store::{NewItem, TimelineStore};
use proptest::prelude::*;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    /// successor_digits; None when every digit is 31 (choose_id refuses it).
    pub fn successor_digits(d: &[u8]) -> Option<Vec<u8>> {
        let mut r = d.to_vec();
        let mut i = r.len();
        while i > 0 {
            let x = r[i - 1];
            if x == 31 {
                r[i - 1] = 0;
                i -= 1;
            } else {
                r[i - 1] = x + 1;
                return Some(r);
            }
        }
        None
    }

    /// choose_id on digit strings (fixed-width, so string order = value order).
    pub fn choose_id(
        gen: &str,
        floor: Option<&str>,
        succ: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        match floor {
            Some(f) if gen <= f => succ(f),
            _ => Some(gen.to_owned()),
        }
    }
}

fn digits(id: &str) -> Vec<u8> {
    id.bytes()
        .map(|b| CROCKFORD.iter().position(|c| *c == b).unwrap() as u8)
        .collect()
}

fn text(d: &[u8]) -> String {
    d.iter().map(|x| CROCKFORD[*x as usize] as char).collect()
}

/// The value as (top digit, the other 25 digits as a u128): 130 bits.
fn value(d: &[u8]) -> (u8, u128) {
    (
        d[0],
        d[1..].iter().fold(0u128, |a, x| a * 32 + u128::from(*x)),
    )
}

fn id_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        proptest::collection::vec(0u8..32, 26).prop_map(|d| text(&d)),
        // runs of Z at the end (carries), and the maximum
        (proptest::collection::vec(0u8..32, 1..26), 0usize..26).prop_map(|(d, _)| {
            let mut v = d.clone();
            v.resize(26, 31);
            text(&v)
        }),
        Just("Z".repeat(26)),
    ]
}

fn item(text: &str) -> NewItem {
    NewItem {
        channel: "web".into(),
        role: "user".into(),
        text: text.into(),
        ..NewItem::default()
    }
}

fn foreign_line(path: &std::path::Path, id: &str) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, r#"{{"id":"{id}","ts":"2026-10-22T10:00:00.000Z","channel":"web","role":"user","text":"node"}}"#).unwrap();
}

#[derive(Debug, Clone)]
enum Act {
    Append,
    /// A line from another writer with a far-future id (above any clock id).
    Foreign(Vec<u8>),
    /// Another process truncates the file and drops the rotated ones.
    Wipe,
}

fn acts() -> impl Strategy<Value = Vec<Act>> {
    let a = prop_oneof![
        5 => Just(Act::Append),
        2 => proptest::collection::vec(0u8..32, 25).prop_map(|mut d| { d.insert(0, 31); d.truncate(26); Act::Foreign(d) }),
        1 => Just(Act::Wipe),
    ];
    proptest::collection::vec(a, 1..30)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// T2: successor == the kernel loop, and is value + 1 (None at the maximum).
    #[test]
    fn successor_matches_kernel(id in id_strategy()) {
        let k = kernel::successor_digits(&digits(&id)).map(|d| text(&d));
        prop_assert_eq!(successor(&id), k.clone());
        if let Some(s) = k {
            prop_assert!(is_timeline_id(&s));
            let (a, b) = (value(&digits(&id)), value(&digits(&s)));
            let plus_one = if a.1 + 1 == 1u128 << 125 { (a.0 + 1, 0) } else { (a.0, a.1 + 1) };
            prop_assert_eq!(b, plus_one);
        } else {
            prop_assert_eq!(id, "Z".repeat(26));
        }
    }

    /// T1: string order == numeric order.
    #[test]
    fn string_order_is_value_order(a in id_strategy(), b in id_strategy()) {
        prop_assert_eq!(a < b, value(&digits(&a)) < value(&digits(&b)));
    }

    /// T3: new_id strictly increases whatever the clock does.
    #[test]
    fn new_id_strictly_increases(times in proptest::collection::vec(1_700_000_000_000i64..1_800_000_000_000, 1..50)) {
        let mut prev = new_id(times[0]);
        for t in &times[1..] {
            let id = new_id(*t);
            prop_assert!(is_timeline_id(&id));
            prop_assert!(id > prev, "{id} <= {prev}");
            prev = id;
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// T4 + T5 on a real store: every generated id is above every id in the
    /// file and above the store's previous id; when a foreign line is above the
    /// clock, the id is exactly the kernel's choose_id (its successor).
    #[test]
    fn append_matches_kernel(acts in acts()) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeline.jsonl");
        let s = TimelineStore::new(&path, DEFAULT_MAX_BYTES, true);
        let mut last: Option<String> = None;
        for (n, act) in acts.into_iter().enumerate() {
            match act {
                Act::Foreign(d) => foreign_line(&path, &text(&d)),
                Act::Wipe => {
                    std::fs::write(&path, b"").unwrap();
                    for g in 1..=2 {
                        let _ = std::fs::remove_file(dir.path().join(format!("timeline.jsonl.{g}")));
                    }
                }
                Act::Append => {
                    let read: Vec<String> = s.read_all().unwrap().into_iter().map(|i| i.id).collect();
                    let newest = read.iter().max().cloned();
                    let floor = match (&newest, &last) {
                        (Some(a), Some(b)) => Some(a.max(b).clone()),
                        (a, b) => a.clone().or(b.clone()),
                    };
                    match s.append(item(&format!("m{n}"))) {
                        Ok(it) => {
                            for r in &read {
                                prop_assert!(it.id > *r, "{} <= read {}", it.id, r);
                            }
                            if let Some(l) = &last {
                                prop_assert!(it.id > *l, "{} <= own previous {}", it.id, l);
                            }
                            // A floor above any clock id: the kernel's exact choice.
                            if let Some(f) = floor.as_deref().filter(|f| f.starts_with('Z')) {
                                let want = kernel::choose_id(&new_id(0), Some(f), |x| kernel::successor_digits(&digits(x)).map(|d| text(&d)));
                                prop_assert_eq!(Some(it.id.clone()), want);
                            }
                            last = Some(it.id);
                        }
                        Err(e) => {
                            // Only when nothing sorts after the floor.
                            prop_assert_eq!(floor, Some("Z".repeat(26)), "{}", e);
                        }
                    }
                }
            }
        }
    }
}

/// The two counterexamples of the kernel, fixed.
#[test]
fn a_maximal_foreign_id_is_refused_not_wrapped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    let s = TimelineStore::new(&path, DEFAULT_MAX_BYTES, true);
    s.append(item("a")).unwrap();
    foreign_line(&path, &"Z".repeat(26));
    assert!(
        s.append(item("b")).is_err(),
        "would have wrapped to 000…0 and sorted first"
    );
}

#[test]
fn ids_do_not_go_back_after_the_file_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    let s = TimelineStore::new(&path, DEFAULT_MAX_BYTES, true);
    foreign_line(&path, "7ZZZZZZZZZ0000000000000000");
    let bumped = s.append(item("a")).unwrap().id;
    assert_eq!(bumped, "7ZZZZZZZZZ0000000000000001");
    std::fs::write(&path, b"").unwrap();
    let next = s.append(item("b")).unwrap().id;
    assert!(next > bumped, "{next} <= {bumped}");
}
