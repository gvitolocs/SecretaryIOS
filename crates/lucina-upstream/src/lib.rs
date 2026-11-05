//! The weekly upstream watch (docs/upstream-watch.md): for every project Lucina ported, forked or
//! vendored (`docs/upstreams.toml`), what the upstream released since the version we are based on,
//! how much that matters (JEV, or a deterministic heuristic), and what to ask the owner.
//!
//! - [`registry`]: the TOML registry and its validation.
//! - [`fetch`]: the [`fetch::ChangeSet`] since the pin, through a [`fetch::Fetcher`] (gh, git, curl).
//! - [`summary`]: a deterministic summary and the heuristic verdict.
//! - [`judge`]: the JEV questions, the state sent to JEV and the [`judge::Verdict`].
//! - [`state`]: `<data_dir>/state.json`, what was asked and decided.
//! - [`decision`]: the decide API bodies ("Port into ours / Later / Ignore") and their answers.
//! - [`brief`]: the porting brief and the contribution draft plan.
//! - [`report`]: the run record and its Markdown report.
//!
//! No network code here except behind [`fetch::Fetcher`]; the CLI (`lucina-rs upstream …`) lives in
//! lucina-server.

pub mod brief;
pub mod decision;
pub mod fetch;
pub mod judge;
pub mod registry;
pub mod report;
pub mod state;
pub mod summary;

/// At most `max` characters (not bytes), with `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Numbers of a version tag (`v0.20.1` → `[0, 20, 1]`, `release-2.3` → `[2, 3]`); `None` without digits.
pub fn version_numbers(tag: &str) -> Option<Vec<u64>> {
    // Pre-release suffixes (`-beta.1`, `-rc2`) are not part of the version core.
    let core = tag
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(['-', '+', '~'])
        .next()
        .unwrap_or("");
    let nums: Vec<u64> = core
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect();
    (!nums.is_empty()).then_some(nums)
}

/// A git sha (7-40 hex digits).
pub fn looks_like_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Two refs name the same thing (one sha may be a prefix of the other).
pub fn same_ref(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    looks_like_sha(a) && looks_like_sha(b) && (a.starts_with(b) || b.starts_with(a))
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::fetch::{ChangeSet, Commit};
    use crate::judge::Verdict;
    use crate::registry::{Registry, Upstream};

    pub const ENTRY: &str = r#"[[upstream]]
id = "x"
name = "X"
url = "https://github.com/a/x"
source = "github"
repo = "a/x"
pin = "v0.19.0"
ours = [{ repo = "/r", path = "k" }]
licence_upstream = "MPL-2.0"
licence_ours = "MPL-2.0"
relation = "fork"
confidence = "likely"
rely_on = ["virtio", "memory management"]

[[upstream.contrib]]
id = "x-faster-boot"
title = "a faster boot path"
ours = "k/src/boot.rs"
upstream_target = "ostd/src/boot"
notes = "Halves the boot time on aarch64."
"#;

    pub fn upstream() -> Upstream {
        Registry::parse(ENTRY).unwrap().upstream.remove(0)
    }

    /// `n` fix commits up to `latest`.
    pub fn changes(latest: &str, n: u64) -> ChangeSet {
        ChangeSet {
            id: "x".into(),
            pin: Some("v0.19.0".into()),
            latest: Some(latest.into()),
            commits_total: n,
            commits: (0..n.min(200))
                .map(|i| Commit {
                    sha7: format!("{i:07x}"),
                    date: "2026-10-20T00:00:00Z".into(),
                    subject: format!("fix: thing {i}"),
                })
                .collect(),
            commits_known: true,
            compare_url: Some("https://github.com/a/x/compare/v0.19.0...main".into()),
            ..ChangeSet::default()
        }
    }

    pub fn verdict_of(importance: u32, security_p: f64, relies_p: f64) -> Verdict {
        Verdict {
            importance,
            importance_conf: 0.7,
            breaking_p: 0.2,
            security_p,
            relies_p,
            effort: "medium".into(),
            effort_conf: 0.6,
            judged_by: "jev".into(),
            model: "jev-1.14.0".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_refs() {
        assert_eq!(version_numbers("v0.20.1"), Some(vec![0, 20, 1]));
        assert_eq!(version_numbers("v0.15.0-beta.1"), Some(vec![0, 15, 0]));
        assert_eq!(version_numbers("2.40.1-1"), Some(vec![2, 40, 1]));
        assert_eq!(version_numbers("main"), None);
        assert!(same_ref("abcdef1", "abcdef1234567"));
        assert!(!same_ref("v1", "v2"));
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
    }
}
