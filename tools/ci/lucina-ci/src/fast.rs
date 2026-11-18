//! `check-fast`: what an agent runs in its worktree before a commit. Only what the change touched, each step capped in memory and
//! time, in one shared Cargo target. It never replaces a gate: `lagent merge` and GitHub still decide. docs/ci/SHIFT-LEFT.md.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Where a changed file's crate lives.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum Owner {
    /// A root workspace member: its package name.
    Member(String),
    /// A crate with its own workspace: its manifest.
    Standalone(String),
    /// A Rust file no fast step can build (its gate runs on merge).
    Other(String),
}

/// The package name in a manifest.
pub fn package_name(toml: &str) -> Option<String> {
    let mut in_package = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
        } else if in_package && t.starts_with("name") {
            return t
                .split_once('=')?
                .1
                .trim()
                .trim_matches('"')
                .to_string()
                .into();
        }
    }
    None
}

pub fn root_members(root_toml: &str) -> BTreeSet<String> {
    // One line or many: everything from `members = [` to the closing `]`, comment lines dropped.
    let Some(i) = root_toml
        .find("\nmembers = [")
        .map(|i| i + 1)
        .or_else(|| root_toml.starts_with("members = [").then_some(0))
    else {
        return BTreeSet::new();
    };
    let rest = &root_toml[i + "members = [".len()..];
    let body = &rest[..rest.find(']').unwrap_or(rest.len())];
    body.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(|l| {
            l.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The crate a changed `.rs` or `Cargo.toml` belongs to, by the nearest manifest.
pub fn owner_of(root: &Path, members: &BTreeSet<String>, path: &str) -> Option<Owner> {
    if !(path.ends_with(".rs") || path.ends_with("Cargo.toml")) {
        return None;
    }
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        let s = d.display().to_string();
        if s.is_empty() {
            return None;
        }
        let manifest = root.join(d).join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&manifest) {
            if members.contains(&s) {
                return package_name(&text).map(Owner::Member);
            }
            if text.contains("[workspace]") && s.starts_with("tools/") {
                return Some(Owner::Standalone(format!("{s}/Cargo.toml")));
            }
            return Some(Owner::Other(s));
        }
        dir = d.parent();
    }
    None
}

/// Steps to run, as (name, shell command, timeout in seconds).
pub fn cargo_steps(owners: &BTreeSet<Owner>, changed: &[String]) -> Vec<(String, String, u64)> {
    let mut steps = Vec::new();
    let members: Vec<String> = owners
        .iter()
        .filter_map(|o| {
            if let Owner::Member(n) = o {
                Some(format!("-p {n}"))
            } else {
                None
            }
        })
        .collect();
    if !members.is_empty() {
        let p = members.join(" ");
        // Only warnings in the files this change touched fail the loop: older warnings elsewhere in a crate are not its doing.
        let paths: Vec<String> = changed
            .iter()
            .filter(|c| c.ends_with(".rs"))
            .map(|c| c.replace('.', "\\."))
            .collect();
        let re = format!("^({}):[0-9]+:[0-9]+: (warning|error)", paths.join("|"));
        steps.push((
            "fmt".to_string(),
            format!("out=$(cargo fmt {p} -- --check 2>&1); rc=$?; if printf '%s\\n' \"$out\" | grep -E \"^Diff in .*({}):\"; then exit 1; fi; if printf '%s\\n' \"$out\" | grep -q '^Diff in '; then exit 0; fi; exit $rc", paths.join("|")),
            60,
        ));
        steps.push((
            "clippy".to_string(),
            format!("out=$(cargo clippy --quiet --locked {p} --all-targets --no-deps --message-format=short 2>&1); rc=$?; if printf '%s\\n' \"$out\" | grep -E \"{re}\"; then exit 1; fi; exit $rc"),
            300,
        ));
        // The tests are built and run as two steps (finding GATE-08): one `cargo test` builds and links the test binaries of
        // the touched crates and only then runs them, and that build was the whole wait — JD-C018 measured `cargo test` for
        // `lucina-server` at 8 m 45 s under a 1-minute load of 24.59, with 40 s of tests in it
        // (`/home/nez/scratch/platform-tools/jd-c018-tests.log`). Both steps take the same flags and the same shared target
        // dir, so the run finds exactly what the build left there.
        // 1200 s, and the merge gate gives it the same 1200 s (`MERGE_STEP_LIMITS`, `main.rs`): a table limit only ever cuts,
        // so the room a slow build needs has to be in the step too. Past it the gate says `build too slow at load <n>: not a
        // test verdict` (`gated_step`) — a build is not a test, but it fails the merge all the same, so nothing lands unbuilt.
        steps.push((
            "unit-tests-build".to_string(),
            format!("cargo test --no-run --quiet --locked {p} --lib"),
            1200,
        ));
        // 300 s, the room this step had before the temporary 480 s of GATE-02, which was really a slow build wearing a test's
        // clothes. The run alone is back at the 240 s the measured week asked for (coordinator allow 2026-11-18, GATE-08).
        steps.push((
            "unit-tests".to_string(),
            format!("cargo test --quiet --locked {p} --lib"),
            300,
        ));
    }
    for o in owners {
        if let Owner::Standalone(m) = o {
            let d = m.trim_end_matches("/Cargo.toml").replace('/', "-");
            steps.push((
                format!("fmt-{d}"),
                format!("cargo fmt --manifest-path {m} -- --check"),
                60,
            ));
            steps.push((
                format!("clippy-{d}"),
                format!(
                    "cargo clippy --quiet --locked --manifest-path {m} --all-targets -- -D warnings"
                ),
                300,
            ));
            // The same split as the workspace members, for this crate's own manifest. No table row names a per-crate step, so
            // the gate keeps cutting both of these at the 180 default they ran under as one step.
            steps.push((
                format!("unit-tests-build-{d}"),
                format!("cargo test --no-run --quiet --locked --manifest-path {m}"),
                1200,
            ));
            steps.push((
                format!("unit-tests-{d}"),
                format!("cargo test --quiet --locked --manifest-path {m}"),
                300,
            ));
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_is_from_the_package_table() {
        let t = "[workspace]\nname = \"no\"\n[package]\nversion = \"1\"\nname = \"yes\"\n";
        assert_eq!(package_name(t).as_deref(), Some("yes"));
    }

    #[test]
    fn members_line() {
        assert_eq!(
            root_members("[workspace]\nmembers = [\"a\", \"b\"]\n").len(),
            2
        );
        let many = "[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/a\",\n    # \"crates/old\",\n    \"crates/b\", \"c\"\n]\n[workspace.package]\nx = [\"no\"]\n";
        let m = root_members(many);
        assert_eq!(m.len(), 3, "{m:?}");
        assert!(m.contains("crates/b") && !m.contains("crates/old") && !m.contains("no"));
    }

    #[test]
    fn steps_cover_members_together_and_standalone_apart() {
        let o: BTreeSet<Owner> = [
            Owner::Member("a".into()),
            Owner::Member("b".into()),
            Owner::Standalone("tools/ci/x/Cargo.toml".into()),
            Owner::Other("office/y".into()),
        ]
        .into_iter()
        .collect();
        let s = cargo_steps(&o, &["crates/a/src/lib.rs".to_string()]);
        assert!(s[1].1.contains("crates/a/src/lib\\.rs"));
        assert_eq!(s.len(), 8);
        assert!(s[0].1.contains("-p a -p b"));
        assert!(
            s.iter()
                .any(|x| x.1.contains("--manifest-path tools/ci/x/Cargo.toml"))
        );
        assert!(
            !s.iter().any(|x| x.1.contains("office")),
            "a crate we cannot build is not faked"
        );
    }

    /// GATE-08: the tests are built by one step and run by the next, and the two are the same command but for `--no-run` —
    /// the same crates, the same flags, so the run finds the binaries the build left in the shared target.
    #[test]
    fn the_tests_are_built_by_one_step_and_run_by_the_next() {
        let o: BTreeSet<Owner> = [
            Owner::Member("a".into()),
            Owner::Standalone("tools/ci/x/Cargo.toml".into()),
        ]
        .into_iter()
        .collect();
        let s = cargo_steps(&o, &["crates/a/src/lib.rs".to_string()]);
        let step = |n: &str| -> (String, u64) {
            s.iter()
                .find(|x| x.0 == n)
                .map(|x| (x.1.clone(), x.2))
                .unwrap_or_else(|| panic!("no step {n}: {s:?}"))
        };
        let (built, built_t) = step("unit-tests-build");
        let (ran, ran_t) = step("unit-tests");
        assert!(built.contains("cargo test --no-run"), "{built}");
        assert_eq!(
            built.replace("cargo test --no-run", "cargo test"),
            ran,
            "the run is the build but for --no-run"
        );
        assert!(
            built_t == 1200 && ran_t == 300,
            "the build has the room of its cap, the run is back at its own 300: {built_t} vs {ran_t}"
        );
        let (built_c, _) = step("unit-tests-build-tools-ci-x");
        let (ran_c, _) = step("unit-tests-tools-ci-x");
        assert!(
            built_c.contains("--no-run --quiet --locked --manifest-path tools/ci/x/Cargo.toml"),
            "{built_c}"
        );
        assert_eq!(
            built_c.replace("cargo test --no-run", "cargo test"),
            ran_c,
            "the per-crate steps are split the same way"
        );
        // The build comes first: a run that had nothing built yet would build it itself and be timed for it.
        let names: Vec<&str> = s.iter().map(|x| x.0.as_str()).collect();
        assert!(
            names.iter().position(|n| *n == "unit-tests-build")
                < names.iter().position(|n| *n == "unit-tests"),
            "unit-tests-build before unit-tests: {names:?}"
        );
        assert!(
            names
                .iter()
                .position(|n| *n == "unit-tests-build-tools-ci-x")
                < names.iter().position(|n| *n == "unit-tests-tools-ci-x"),
            "the per-crate build before its run: {names:?}"
        );
    }

    #[test]
    fn nothing_to_do_means_no_steps() {
        assert!(cargo_steps(&BTreeSet::new(), &[]).is_empty());
    }
}
