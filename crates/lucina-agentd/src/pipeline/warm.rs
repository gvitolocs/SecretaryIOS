//! What the merge warm-up warms besides `cargo check`: the clippy the gate's `check-fast` runs
//! (LA-C026, finding GATE-13). The gate's clippy keeps build artefacts of its own, apart from
//! `cargo check`'s, so a warm `cargo check` does not help it; `lucina-server`'s clippy took 296 s
//! cold at load 45. The commands here are the gate's own, read from
//! `tools/ci/lucina-ci/src/fast.rs` (`owner_of`, `cargo_steps`) and quoted flag for flag: the flags
//! decide which artefacts clippy reuses, so a warm-up with other flags warms nothing.
//!
//! This is pure: which commands a branch's touched files call for. Running them, under the
//! warm-up's one cap, and never as a gate, is `Exec::warm_up`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Flags every gate clippy command starts with after `cargo clippy`.
pub(super) const CLIPPY_HEAD: &str = "--quiet --locked";
/// The tail of the gate's workspace clippy step (the touched members' `-p` flags go before it).
pub(super) const CLIPPY_MEMBER_TAIL: &str = "--all-targets --no-deps --message-format=short";
/// The tail of the gate's per-standalone-manifest clippy step. `-D warnings` stays: lint flags are
/// part of clippy's fingerprint, so a warm-up without it is a cold run at the gate.
pub(super) const CLIPPY_STANDALONE_TAIL: &str = "--all-targets -- -D warnings";

/// Who a changed file belongs to, as `owner_of` in `lucina-ci` decides it.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Member(String),
    Standalone(String),
}

/// The workspace members of a `cargo metadata --no-deps --format-version 1` answer, as
/// directory (relative to `root`, which must be the canonical path of the workspace root) to
/// package name. `None` when the answer is not one.
pub(super) fn member_map(json: &[u8], root: &Path) -> Option<BTreeMap<String, String>> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let ids = v["workspace_members"].as_array()?;
    let mut out = BTreeMap::new();
    for p in v["packages"].as_array()? {
        if !ids.contains(&p["id"]) {
            continue;
        }
        let manifest = Path::new(p["manifest_path"].as_str()?);
        let dir = manifest.parent()?.strip_prefix(root).ok()?;
        out.insert(dir.display().to_string(), p["name"].as_str()?.to_string());
    }
    Some(out)
}

/// The owner of one changed file: the nearest manifest above it decides. Only `.rs` files and
/// `Cargo.toml` count, and the root manifest never does (the gate's rule).
fn owner_of(
    members: &BTreeMap<String, String>,
    manifest: &dyn Fn(&str) -> Option<String>,
    path: &str,
) -> Option<Owner> {
    if !(path.ends_with(".rs") || path.ends_with("Cargo.toml")) {
        return None;
    }
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        let s = d.display().to_string();
        if s.is_empty() {
            return None;
        }
        if let Some(text) = manifest(&s) {
            if let Some(name) = members.get(&s) {
                return Some(Owner::Member(name.clone()));
            }
            if text.contains("[workspace]") && s.starts_with("tools/") {
                return Some(Owner::Standalone(format!("{s}/Cargo.toml")));
            }
            return None;
        }
        dir = d.parent();
    }
    None
}

/// The clippy commands the gate would run for a branch touching `files`, in the gate's order: one
/// workspace command for all touched members, then one per touched standalone manifest. Empty when
/// the change gives the gate no clippy step (docs, a crate outside the workspace and `tools/`).
/// `manifest(dir)` reads `<dir>/Cargo.toml` of the tree the warm-up runs in.
pub(super) fn clippy_commands(
    files: &[String],
    members: &BTreeMap<String, String>,
    manifest: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let owners: BTreeSet<Owner> = files
        .iter()
        .filter_map(|f| owner_of(members, manifest, f))
        .collect();
    let mut cmds = Vec::new();
    let p: Vec<String> = owners
        .iter()
        .filter_map(|o| match o {
            Owner::Member(n) => Some(format!("-p {n}")),
            Owner::Standalone(_) => None,
        })
        .collect();
    if !p.is_empty() {
        cmds.push(format!(
            "cargo clippy {CLIPPY_HEAD} {} {CLIPPY_MEMBER_TAIL}",
            p.join(" ")
        ));
    }
    for o in &owners {
        if let Owner::Standalone(m) = o {
            cmds.push(format!(
                "cargo clippy {CLIPPY_HEAD} --manifest-path {m} {CLIPPY_STANDALONE_TAIL}"
            ));
        }
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> BTreeMap<String, String> {
        [
            ("crates/lucina-server", "lucina-server"),
            ("crates/lucina-agentd", "lucina-agentd"),
        ]
        .iter()
        .map(|(d, n)| (d.to_string(), n.to_string()))
        .collect()
    }

    /// Every manifest of a small repo: the two members, a nested non-member, a standalone tool.
    fn manifest(dir: &str) -> Option<String> {
        match dir {
            "crates/lucina-server" | "crates/lucina-agentd" => {
                Some("[package]\nname = \"x\"\n".into())
            }
            "tools/ci/lucina-ci" => Some("[package]\nname = \"lucina-ci\"\n[workspace]\n".into()),
            "vendor/thing" => Some("[package]\nname = \"thing\"\n".into()),
            _ => None,
        }
    }

    fn cmds(files: &[&str]) -> Vec<String> {
        let files: Vec<String> = files.iter().map(|s| s.to_string()).collect();
        clippy_commands(&files, &members(), &manifest)
    }

    #[test]
    fn a_member_file_gives_the_gate_s_workspace_clippy() {
        assert_eq!(
            cmds(&["crates/lucina-server/src/lib.rs"]),
            [
                "cargo clippy --quiet --locked -p lucina-server --all-targets --no-deps --message-format=short"
            ]
        );
    }

    #[test]
    fn a_nested_member_file_belongs_to_its_member() {
        assert_eq!(
            cmds(&["crates/lucina-agentd/src/pipeline/deep/x.rs"]),
            [
                "cargo clippy --quiet --locked -p lucina-agentd --all-targets --no-deps --message-format=short"
            ]
        );
    }

    #[test]
    fn a_standalone_tool_manifest_gives_its_own_clippy() {
        assert_eq!(
            cmds(&["tools/ci/lucina-ci/Cargo.toml"]),
            [
                "cargo clippy --quiet --locked --manifest-path tools/ci/lucina-ci/Cargo.toml --all-targets -- -D warnings"
            ]
        );
    }

    #[test]
    fn docs_and_other_manifests_give_no_step() {
        assert!(cmds(&["docs/x.md", "README.md", "crates/lucina-server/notes.txt"]).is_empty());
        // A crate that is neither a member nor under tools/, and the root manifest.
        assert!(cmds(&["vendor/thing/src/lib.rs", "Cargo.toml", "build.rs"]).is_empty());
    }

    #[test]
    fn a_mix_is_one_workspace_command_then_one_per_standalone() {
        assert_eq!(
            cmds(&[
                "docs/x.md",
                "crates/lucina-server/src/a.rs",
                "tools/ci/lucina-ci/src/fast.rs",
                "crates/lucina-agentd/Cargo.toml",
                "crates/lucina-server/src/b.rs",
            ]),
            [
                "cargo clippy --quiet --locked -p lucina-agentd -p lucina-server --all-targets --no-deps --message-format=short",
                "cargo clippy --quiet --locked --manifest-path tools/ci/lucina-ci/Cargo.toml --all-targets -- -D warnings",
            ]
        );
    }

    #[test]
    fn the_member_map_reads_cargo_metadata() {
        let json = br#"{"workspace_members":["a 0.1.0 (path+file:///w/crates/a)"],"packages":[
            {"id":"a 0.1.0 (path+file:///w/crates/a)","name":"a-b","manifest_path":"/w/crates/a/Cargo.toml"},
            {"id":"dep 1.0.0","name":"dep","manifest_path":"/home/.cargo/dep/Cargo.toml"}]}"#;
        let m = member_map(json, Path::new("/w")).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("crates/a").map(String::as_str), Some("a-b"));
        assert!(member_map(b"", Path::new("/w")).is_none());
    }

    /// The warm-up must follow the gate. The flags above are copied from `lucina-ci`; if PF
    /// changes them there, this fails and the warm-up (`CLIPPY_*` here) must follow, or it warms a
    /// clippy the gate no longer runs.
    #[test]
    fn the_warm_up_flags_are_the_gate_s_flags() {
        let fast =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/ci/lucina-ci/src/fast.rs");
        let text = std::fs::read_to_string(&fast)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", fast.display()));
        assert_eq!(CLIPPY_HEAD, "--quiet --locked");
        assert_eq!(
            CLIPPY_MEMBER_TAIL,
            "--all-targets --no-deps --message-format=short"
        );
        assert_eq!(CLIPPY_STANDALONE_TAIL, "--all-targets -- -D warnings");
        for flags in [CLIPPY_HEAD, CLIPPY_MEMBER_TAIL, CLIPPY_STANDALONE_TAIL] {
            assert!(
                text.contains(flags),
                "tools/ci/lucina-ci/src/fast.rs no longer contains `{flags}`: the gate's clippy flags changed, so the merge warm-up (pipeline/warm.rs, LA-C026) must follow"
            );
        }
    }
}
