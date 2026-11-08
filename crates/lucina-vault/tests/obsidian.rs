//! Real Obsidian against this crate: `tests/fixture/obsidian-golden.json` is what
//! Obsidian 1.15.0 decided on `tests/fixture/vault` (`tools/vault-oracle/run.sh`).
//! `examples/real_copy.rs` does the same on a copy of a real vault, which never enters the repo.
use lucina_vault::{Options, Vault};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[path = "support/compare.rs"]
mod support;
use support::compare;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture")
}

#[test]
fn fixture_matches_real_obsidian() {
    let g: Value =
        serde_json::from_slice(&std::fs::read(fixture().join("obsidian-golden.json")).unwrap())
            .unwrap();
    assert_eq!(g["obsidian"], "1.15.0");
    let v = Vault::open(fixture().join("vault"), Options::default()).unwrap();
    let d = compare(&v, &g);
    assert!(
        d.wrong.is_empty(),
        "{} of {} differ:\n{}",
        d.wrong.len(),
        d.checked,
        d.wrong.join("\n")
    );
}
