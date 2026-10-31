//! Differential test for the known-values pass of the redactor, against a
//! plain-Rust copy of `redact_exec` in `verification/verus/secrets_redactor.rs`.
//!
//! Belongs in `crates/lucina-secrets/tests/verus_redact.rs` (with
//! `proptest = "1"` in its dev-dependencies) once lucina-secrets is committed.
//! Until then it was run on a snapshot of the SecretaryIOS-secrets worktree:
//! copy the tree without target/, drop this file in as that test, then
//! `cargo test -p lucina-secrets --test verus_redact` (and `-- --ignored` for
//! the two known counterexamples).

use lucina_secrets::redact::{encodings, placeholder, MIN_PATTERN_LEN};
use lucina_secrets::Redactor;
use proptest::prelude::*;

/// Plain-Rust copy of the Verus kernel (`bytes_match_at`, `best_at`, `redact_exec`).
mod kernel {
    pub fn bytes_match_at(t: &[u8], i: usize, p: &[u8]) -> bool {
        if p.len() > t.len() || i > t.len() - p.len() {
            return false;
        }
        let mut j = 0;
        while j < p.len() {
            if t[i + j] != p[j] {
                return false;
            }
            j += 1;
        }
        true
    }

    pub fn best_at(t: &[u8], i: usize, pats: &[Vec<u8>]) -> Option<usize> {
        let mut cur: Option<usize> = None;
        for k in 0..pats.len() {
            if bytes_match_at(t, i, &pats[k]) {
                let better = match cur {
                    None => true,
                    Some(b) => pats[k].len() > pats[b].len(),
                };
                if better {
                    cur = Some(k);
                }
            }
        }
        cur
    }

    pub fn redact_exec(t: &[u8], pats: &[Vec<u8>], phs: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < t.len() {
            match best_at(t, i, pats) {
                Some(k) => {
                    out.extend_from_slice(&phs[k]);
                    i += pats[k].len();
                }
                None => {
                    out.push(t[i]);
                    i += 1;
                }
            }
        }
        out
    }

    /// The kernel's well-formedness condition.
    pub fn wf(pats: &[Vec<u8>], phs: &[Vec<u8>]) -> bool {
        pats.iter().all(|p| !p.is_empty() && !p.contains(&b'[') && !p.contains(&b']'))
            && phs.iter().all(|h| h.len() >= 2 && h[0] == b'[' && h[h.len() - 1] == b']')
            && pats.iter().all(|p| phs.iter().all(|h| !occurs(h, p)))
    }

    pub fn occurs(t: &[u8], p: &[u8]) -> bool {
        (0..=t.len()).any(|j| bytes_match_at(t, j, p))
    }
}

/// What `Redactor::register` stores, in registration order: (pattern, placeholder).
fn patterns(secrets: &[(String, String)]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut pats: Vec<Vec<u8>> = Vec::new();
    let mut phs: Vec<Vec<u8>> = Vec::new();
    for (name, value) in secrets {
        let v = value.trim_matches(|c: char| c == '\n' || c == '\r');
        if v.len() < MIN_PATTERN_LEN {
            continue;
        }
        for enc in encodings(v) {
            let b = enc.into_bytes();
            if b.len() < MIN_PATTERN_LEN || pats.contains(&b) {
                continue;
            }
            pats.push(b);
            phs.push(placeholder(name).into_bytes());
        }
    }
    (pats, phs)
}

fn secrets() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::vec(("[A-Z]{1,3}", "[abc\\[\\]x:%=]{4,10}"), 1..4)
}

/// Secrets without brackets (the kernel's wf mostly holds for these).
fn secrets_wf() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::vec(("[A-Z]{1,3}", "[abcx:%= ]{6,10}"), 1..4)
}

fn text(secrets: Vec<(String, String)>) -> impl Strategy<Value = (Vec<(String, String)>, String)> {
    let values: Vec<String> = secrets.iter().map(|(_, v)| v.clone()).collect();
    let piece = prop_oneof![
        3 => "[abc\\[\\]x:% ]{0,5}",
        2 => proptest::sample::select(values),
    ];
    (Just(secrets), proptest::collection::vec(piece, 0..8).prop_map(|v| v.concat()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// redact_known == the kernel loop, byte for byte, whatever the secrets.
    #[test]
    fn redact_known_matches_kernel((secrets, text) in secrets().prop_flat_map(text)) {
        let r = Redactor::new();
        for (name, value) in &secrets {
            r.register(name, value);
        }
        let (pats, phs) = patterns(&secrets);
        let k = kernel::redact_exec(text.as_bytes(), &pats, &phs);
        let p = r.redact_known(&text).into_owned().into_bytes();
        prop_assert_eq!(String::from_utf8_lossy(&p), String::from_utf8_lossy(&k));
    }

    /// R1-R3 on production whenever the kernel's wf holds.
    #[test]
    fn theorems_hold_on_production_when_wf((secrets, text) in secrets_wf().prop_flat_map(text)) {
        let (pats, phs) = patterns(&secrets);
        prop_assume!(kernel::wf(&pats, &phs));
        let r = Redactor::new();
        for (name, value) in &secrets {
            r.register(name, value);
        }
        let out = r.redact_known(&text).into_owned();
        for p in &pats {
            prop_assert!(!kernel::occurs(out.as_bytes(), p), "{:?} survives in {out:?}", String::from_utf8_lossy(p));
        }
        prop_assert_eq!(r.redact_known(&out).into_owned(), out.clone());
        let cap = phs.iter().map(Vec::len).max().unwrap_or(0).max(MIN_PATTERN_LEN);
        prop_assert!(MIN_PATTERN_LEN * out.len() <= cap * text.len());
    }
}

/// The counterexamples without wf (the kernel's bad_* lemmas): ignored while
/// production keeps the one-pass replacement; `cargo test -- --ignored` shows them.
#[test]
#[ignore = "fails on lucina-secrets: a secret inside a placeholder survives"]
fn a_secret_that_is_part_of_its_placeholder_does_not_survive() {
    let r = Redactor::new();
    r.register("X", "secret");
    let out = r.redact_known("my secret").into_owned();
    assert!(!out.contains("secret"), "{out}");
}

#[test]
#[ignore = "fails on lucina-secrets: a placeholder completes another secret"]
fn a_placeholder_does_not_complete_another_secret() {
    let r = Redactor::new();
    r.register("X", "aaaaaa");
    r.register("Y", "X]bbbbbb");
    let out = r.redact_known("aaaaaabbbbbb").into_owned();
    assert!(!out.contains("X]bbbbbb"), "{out}");
    assert_eq!(r.redact_known(&out), out, "not idempotent");
}
