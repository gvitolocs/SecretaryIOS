//! Tests of `sync_rules` (kept out of the file Verus reads).
use crate::sync_rules::*;

fn facts(base: Option<u8>, local: Option<u8>, remote: Option<u8>) -> Facts {
    let eq = |a: Option<u8>, b: Option<u8>| a.is_some() && a == b;
    Facts {
        base: base.is_some(),
        local: local.is_some(),
        remote: remote.is_some(),
        local_is_base: eq(local, base),
        remote_is_base: eq(remote, base),
        local_is_remote: eq(local, remote),
    }
}

#[test]
fn every_case() {
    use Act::*;
    let (a, b, c) = (Some(1), Some(2), Some(3));
    let cases = [
        ((None, None, None), Nothing),
        ((a, None, None), Nothing),
        ((None, a, a), Record),
        ((None, a, None), Push),
        ((None, None, a), Pull),
        ((None, a, b), Conflict),
        ((a, a, a), Record),
        ((a, a, b), Pull),
        ((a, b, a), Push),
        ((a, b, c), Conflict),
        ((a, b, b), Record),
        ((a, a, None), TrashLocal),
        ((a, None, a), TrashRemote),
        ((a, b, None), Push),
        ((a, None, b), Pull),
    ];
    for ((base, l, r), want) in cases {
        assert_eq!(decide(facts(base, l, r)), want, "{base:?} {l:?} {r:?}");
    }
}

#[test]
fn paths() {
    use PathClass::*;
    for (p, want) in [
        ("a.md", Sync),
        ("Folder/Note with spaces.md", Sync),
        ("café/ü.md", Sync),
        ("a.sync-conflict-20261108-101010-ABCDEFG.md", Sync),
        (".obsidian/app.json", PullOnly),
        (".obsidian/plugins/x/main.js", Skip),
        (".obsidian/plugins/x/data.json", Skip),
        (".obsidian/workspace.js", Skip),
        (".obsidian", Skip),
        (".obsidian/", Skip),
        (".trash/a.md", Skip),
        (".stversions/a.md", Skip),
        ("a/.lucina-vault-1-2.tmp", Skip),
        ("a/._b.md", Skip),
        ("", Skip),
        ("/a.md", Skip),
        ("a//b.md", Skip),
        ("a/", Skip),
        ("a\\b.md", Skip),
        ("a\nb.md", Skip),
        ("Icon\r", Skip),
    ] {
        assert_eq!(classify(p.as_bytes()), want, "{p:?}");
    }
    assert_eq!(classify("a".repeat(MAX_NAME).as_bytes()), Sync);
    assert_eq!(classify("a".repeat(MAX_NAME + 1).as_bytes()), Skip);
    assert_eq!(classify(format!("{}/b", "a/".repeat(511)).as_bytes()), Skip);
}
