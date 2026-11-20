//! `lucina-ci key-scan`: a provider key format in a tracked text file fails the scan, wherever the file is, and no
//! allowlist (a `.gitleaks.toml` one included) lets it through.
//!
//! Every sample below is built at run time from parts that match nothing on their own, so this file holds no key-shaped
//! literal and the scanner passes over it. The real binary runs over throwaway repositories under the target directory,
//! removed on panic too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A throwaway git repository, removed when it goes out of scope, a panic included.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Tree {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "key-scan-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let t = Tree(d);
        t.git(&["init", "-q"]);
        t
    }

    fn git(&self, args: &[&str]) {
        let mut c = Command::new("git");
        c.args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(&self.0);
        // `bq` leaves GIT_CONFIG_COUNT behind without its KEY: no git can read that.
        for n in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
            c.env_remove(n);
        }
        let o = c.output().unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
        self.git(&["add", "-f", rel]);
    }

    /// `key-scan --root <tree> files...`; with no file, the whole tracked tree.
    fn scan(&self, files: &[&str]) -> (i32, String) {
        let o: Output = Command::new(env!("CARGO_BIN_EXE_lucina-ci"))
            .arg("key-scan")
            .arg("--root")
            .arg(&self.0)
            .args(files)
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap(), text)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(Path::new(&self.0));
    }
}

/// One synthetic key of each format, assembled here. Neither half matches a format alone.
fn samples() -> Vec<(&'static str, String)> {
    let lower = |n: usize| "q".repeat(n);
    let upper = |n: usize| "Q".repeat(n);
    let dashes = "-".repeat(5);
    vec![
        ("google-api-key", format!("{}{}", "AIza", lower(35))),
        ("github-token", format!("{}_{}", "ghp", lower(36))),
        ("github-pat", format!("{}_{}", "github_pat", lower(22))),
        ("aws-key", format!("{}{}", "AKIA", upper(16))),
        ("slack-token", format!("{}-{}", "xoxb", "1".repeat(12))),
        ("openai-key", format!("{}-{}", "sk", lower(24))),
        (
            "pem-private-key",
            format!("{dashes}BEGIN {} KEY{dashes}", "OPENSSH PRIVATE"),
        ),
        (
            "jwt",
            format!("eyJ{}.eyJ{}.{}", lower(12), lower(12), lower(12)),
        ),
    ]
}

#[test]
fn a_key_of_each_format_in_a_tests_file_fails() {
    for (kind, key) in samples() {
        let t = Tree::new();
        t.put(
            "crates/x/tests/vector.rs",
            format!("let k = \"{key}\";\n").as_bytes(),
        );
        let (code, out) = t.scan(&["crates/x/tests/vector.rs"]);
        assert_eq!(code, 1, "{kind}: {out}");
        assert!(
            out.contains(&format!(
                "key-scan: {kind} at crates/x/tests/vector.rs:1 (redacted: "
            )),
            "{kind}: {out}"
        );
        // And the same through the whole-tree mode, which asks git for the files.
        let (code, out) = t.scan(&[]);
        assert_eq!(code, 1, "{kind} (tree): {out}");
    }
}

#[test]
fn a_key_assembled_at_runtime_is_not_visible_to_a_source_scan() {
    // The limit, written down: a scanner over source text cannot see a key that exists only once the program runs.
    // Only the assembled literal is visible, and this is why test vectors must be built from parts that match nothing.
    let t = Tree::new();
    let tail = "q".repeat(35);
    let source = format!(
        "const A: &str = concat!(\"AI\", \"za\", \"{tail}\");\nfn b() -> String {{ \"AIza\".to_string() + \"{tail}\" }}\nfn c() -> String {{ format!(\"{{}}{{}}\", \"AIza\", \"{tail}\") }}\n"
    );
    t.put("src/lib.rs", source.as_bytes());
    let (code, out) = t.scan(&["src/lib.rs"]);
    assert_eq!(code, 0, "{out}");
    // The assembled literal, written out whole, is caught.
    t.put(
        "src/whole.rs",
        format!("const A: &str = \"AIza{tail}\";\n").as_bytes(),
    );
    let (code, out) = t.scan(&["src/whole.rs"]);
    assert_eq!(code, 1, "{out}");
}

#[test]
fn text_that_only_looks_like_a_key_passes() {
    let t = Tree::new();
    let near = [
        format!("{}-{}", "task", "q".repeat(30)),
        format!("{}-{}", "ask", "q".repeat(30)),
        format!("{}{}", "AIz", "q".repeat(34)),
        format!("{}_{}", "ghp", "q".repeat(35)),
        format!("{}-{}", "sk", "q".repeat(19)),
        format!("{}{}", "AKIA", "Q".repeat(15)),
        format!("{}BEGIN CERTIFICATE{}", "-".repeat(5), "-".repeat(5)),
        format!("{}BEGIN PUBLIC KEY{}", "-".repeat(5), "-".repeat(5)),
        format!("x{}{}", "AIza", "q".repeat(35)),
    ];
    t.put("notes.txt", near.join("\n").as_bytes());
    let (code, out) = t.scan(&["notes.txt"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("0 match(es)"), "{out}");
}

#[test]
fn a_gitleaks_allowlisted_path_holding_a_key_still_fails() {
    let t = Tree::new();
    t.put(
        ".gitleaks.toml",
        b"[allowlist]\ndescription = \"checked by hand\"\npaths = ['''tests/data/allowed\\.txt$''', '''^evidence/''']\n",
    );
    for (kind, key) in samples() {
        t.put("tests/data/allowed.txt", format!("{key}\n").as_bytes());
        let (code, out) = t.scan(&["tests/data/allowed.txt"]);
        assert_eq!(code, 1, "{kind}: {out}");
    }
    t.put(
        "evidence/log.txt",
        format!("{}\n", samples()[0].1).as_bytes(),
    );
    let (code, out) = t.scan(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("evidence/log.txt:1"), "{out}");
}

#[test]
fn the_full_key_never_appears_in_the_output() {
    for (kind, key) in samples() {
        let t = Tree::new();
        t.put("f.txt", format!("secret = {key}\n").as_bytes());
        let (code, out) = t.scan(&["f.txt"]);
        assert_eq!(code, 1, "{kind}: {out}");
        let tail = &key[key.len().min(8)..];
        assert!(
            !out.contains(&key),
            "{kind}: the key is in the output: {out}"
        );
        assert!(
            !out.contains(tail),
            "{kind}: part of the key is in the output: {out}"
        );
        assert!(out.contains("(redacted: "), "{kind}: {out}");
    }
}

#[test]
fn a_binary_file_is_skipped_and_said_so() {
    let t = Tree::new();
    let mut bytes = vec![0u8, 1, 2, 0];
    bytes.extend_from_slice(samples()[0].1.as_bytes());
    t.put("blob.bin", &bytes);
    let (code, out) = t.scan(&["blob.bin"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("skipped blob.bin (binary"), "{out}");
    assert!(out.contains("1 skipped"), "{out}");
}

#[test]
fn a_file_over_four_mib_is_skipped_and_said_so() {
    let t = Tree::new();
    let mut bytes = vec![b'a'; 4 * 1024 * 1024 + 1];
    bytes.extend_from_slice(samples()[0].1.as_bytes());
    t.put("big.txt", &bytes);
    let (code, out) = t.scan(&["big.txt"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("skipped big.txt (over 4 MiB)"), "{out}");
}

#[test]
fn a_clean_tree_passes_and_says_how_much_it_read() {
    let t = Tree::new();
    t.put("a.rs", b"fn main() {}\n");
    let (code, out) = t.scan(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("1 file(s) scanned, 0 skipped, 0 match(es)"),
        "{out}"
    );
}
