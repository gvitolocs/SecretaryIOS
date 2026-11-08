//! A deterministic summary of a [`ChangeSet`] (no model), and the [`heuristic`] verdict used when
//! JEV is unavailable.

use serde::{Deserialize, Serialize};

use crate::fetch::ChangeSet;
use crate::judge::Verdict;
use crate::registry::Upstream;
use crate::{clip, version_numbers};

pub const LINES_MAX: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Counts {
    pub releases: u64,
    pub commits: u64,
    pub security: u64,
    pub breaking: u64,
    pub feat: u64,
    pub fix: u64,
    pub perf: u64,
    /// docs, CI, chores, tests, deps and anything unclassified: counted, not listed.
    pub other: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Summary {
    /// `v0.20.0, v0.20.1 + 143 commits (12 fixes, 5 features, 1 security)`.
    pub headline: String,
    /// At most [`LINES_MAX`]: releases, then security, breaking, features, fixes.
    pub lines: Vec<String>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Feat,
    Fix,
    Perf,
    Other,
}

/// What one commit subject is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    pub kind: Kind,
    pub security: bool,
    pub breaking: bool,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// CVE, security, vuln…, overflow, UAF, sanitize…, auth (not "author").
pub fn is_security(text: &str) -> bool {
    if text.to_ascii_uppercase().contains("CVE-") {
        return true;
    }
    words(text).iter().any(|w| {
        matches!(
            w.as_str(),
            "cve" | "security" | "overflow" | "uaf" | "auth" | "authn" | "authz"
        ) || w.starts_with("vuln")
            || w.starts_with("sanitiz")
            || w.starts_with("sanitis")
            || w.starts_with("authenticat")
            || w.starts_with("authoriz")
            || w.starts_with("authoris")
            || w == "use-after-free"
    }) || text.to_lowercase().contains("use-after-free")
}

/// BREAKING, `!:`, breaking, remove, drop support.
pub fn is_breaking(text: &str) -> bool {
    text.contains("BREAKING") || text.contains("!:") || soft_breaking(text)
}

/// The weaker signs (breaking, remove, drop support): not counted on fix/test/ci/chore/docs commits,
/// where they mostly mean "removed a warning" or "without breaking".
fn soft_breaking(text: &str) -> bool {
    let l = text.to_lowercase();
    l.contains("breaking")
        || l.contains("drop support")
        || words(text)
            .iter()
            .any(|w| matches!(w.as_str(), "remove" | "removes" | "removed"))
}

/// Commit types whose "remove"/"breaking" is not a behaviour change.
const SOFT_TYPES: &[&str] = &[
    "fix", "bugfix", "hotfix", "test", "tests", "ci", "chore", "docs", "doc", "style", "build",
];

/// The conventional-commit type (`feat(x)!: …` → `feat`), lowercase.
fn conventional(subject: &str) -> Option<String> {
    let colon = subject.find(':')?;
    let head = subject[..colon].trim_end_matches('!');
    let ty = head.split('(').next()?.trim();
    (!ty.is_empty() && ty.len() <= 12 && ty.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| ty.to_ascii_lowercase())
}

pub fn classify(subject: &str) -> Class {
    let kind = match conventional(subject).as_deref() {
        Some("feat" | "feature") => Kind::Feat,
        Some("fix" | "bugfix" | "hotfix") => Kind::Fix,
        Some("perf") => Kind::Perf,
        Some(_) => Kind::Other,
        None => {
            let w = words(subject);
            let first = w.first().map(String::as_str).unwrap_or("");
            let tests = w.iter().any(|x| x.starts_with("test"));
            match first {
                "add" | "adds" | "added" | "implement" | "implements" | "support" | "introduce"
                | "new"
                    if !tests =>
                {
                    Kind::Feat
                }
                "fix" | "fixes" | "fixed" | "correct" | "handle" | "prevent" | "avoid" => Kind::Fix,
                "optimize" | "optimise" | "speed" | "perf" | "faster" => Kind::Perf,
                _ if w.iter().any(|x| x == "fix" || x == "fixes" || x == "bug") => Kind::Fix,
                _ => Kind::Other,
            }
        }
    };
    let ty = conventional(subject);
    let hard = subject.contains("BREAKING") || subject.contains("!:");
    let soft_ok = ty.as_deref().is_none_or(|t| !SOFT_TYPES.contains(&t)) && kind != Kind::Fix;
    Class {
        kind,
        security: is_security(subject),
        breaking: hard || (soft_ok && soft_breaking(subject)),
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn summarize(cs: &ChangeSet) -> Summary {
    if let Some(why) = &cs.skipped {
        return Summary {
            headline: format!("skipped ({why})"),
            ..Summary::default()
        };
    }
    if let Some(e) = &cs.error {
        return Summary {
            headline: format!("error: {}", clip(e, 200)),
            ..Summary::default()
        };
    }
    let mut c = Counts {
        releases: cs.releases.len() as u64,
        commits: cs.commits_total,
        ..Counts::default()
    };
    let (mut sec, mut brk, mut feat, mut fix) = (vec![], vec![], vec![], vec![]);
    for k in &cs.commits {
        let cl = classify(&k.subject);
        if cl.security {
            c.security += 1;
            sec.push(&k.subject);
        }
        if cl.breaking {
            c.breaking += 1;
            brk.push(&k.subject);
        }
        match cl.kind {
            Kind::Feat => {
                c.feat += 1;
                feat.push(&k.subject);
            }
            Kind::Fix => {
                c.fix += 1;
                fix.push(&k.subject);
            }
            Kind::Perf => c.perf += 1,
            Kind::Other => c.other += 1,
        }
    }

    // Headline: releases oldest first, then the commit count and its breakdown.
    let mut parts = Vec::new();
    if !cs.releases.is_empty() {
        let tags: Vec<&str> = cs.releases.iter().rev().map(|r| r.tag.as_str()).collect();
        parts.push(if tags.len() > 4 {
            format!(
                "{}, … {} ({} releases)",
                tags[0],
                tags[tags.len() - 1],
                tags.len()
            )
        } else {
            tags.join(", ")
        });
    }
    if cs.commits_total > 0 {
        let mut kinds = Vec::new();
        if c.fix > 0 {
            kinds.push(plural(c.fix, "fix", "fixes"));
        }
        if c.feat > 0 {
            kinds.push(plural(c.feat, "feature", "features"));
        }
        if c.perf > 0 {
            kinds.push(format!("{} perf", c.perf));
        }
        if c.security > 0 {
            kinds.push(format!("{} security", c.security));
        }
        if c.breaking > 0 {
            kinds.push(format!("{} breaking", c.breaking));
        }
        let mut s = plural(cs.commits_total, "commit", "commits");
        if !kinds.is_empty() {
            s.push_str(&format!(" ({})", kinds.join(", ")));
        }
        parts.push(s);
    }
    let headline = if !parts.is_empty() {
        parts.join(" + ")
    } else if !cs.is_empty() {
        format!(
            "new head {} (was {})",
            cs.latest.as_deref().unwrap_or("?"),
            cs.pin.as_deref().unwrap_or("?")
        )
    } else {
        "nothing new".into()
    };

    let mut lines = Vec::new();
    for r in &cs.releases {
        let note = r
            .notes
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("<!--"));
        let mut l = format!(
            "Release {}{}{}",
            r.tag,
            if r.date.len() >= 10 {
                format!(" ({})", &r.date[..10])
            } else {
                String::new()
            },
            if r.prerelease { " [pre-release]" } else { "" }
        );
        if let Some(n) = note {
            l.push_str(&format!(
                ": {}",
                clip(n.trim_start_matches(['-', '*', ' ']), 160)
            ));
        }
        lines.push(l);
    }
    for (label, list) in [
        ("Security", &sec),
        ("Breaking", &brk),
        ("Feature", &feat),
        ("Fix", &fix),
    ] {
        for subj in list.iter() {
            lines.push(format!("{label}: {}", clip(subj, 160)));
        }
    }
    lines.dedup();
    lines.truncate(LINES_MAX);
    Summary {
        headline,
        lines,
        counts: c,
    }
}

/// Every text JEV (or the heuristic) reads: subjects, notes, paths.
fn texts(cs: &ChangeSet) -> impl Iterator<Item = &str> {
    cs.commits
        .iter()
        .map(|c| c.subject.as_str())
        .chain(cs.releases.iter().map(|r| r.notes.as_str()))
        .chain(cs.releases.iter().map(|r| r.name.as_str()))
        .chain(cs.files_touched.iter().map(String::as_str))
}

/// A `rely_on` feature (or one of its words, three letters or more) appears in the changes.
pub fn touches_rely_on(up: &Upstream, cs: &ChangeSet) -> bool {
    let hay: Vec<String> = texts(cs).map(str::to_lowercase).collect();
    let hay_words: std::collections::HashSet<String> = hay.iter().flat_map(|t| words(t)).collect();
    up.rely_on.iter().any(|f| {
        let f = f.to_lowercase();
        hay.iter().any(|t| t.contains(&f))
            || words(&f)
                .iter()
                .filter(|w| w.len() >= 3)
                .any(|w| hay_words.contains(w))
    })
}

/// A release bumps the major (or the minor while the major is 0): 2; a minor bump: 1; else 0.
fn bump(pin: Option<&str>, cs: &ChangeSet) -> u32 {
    let Some(p) = pin.and_then(version_numbers) else {
        return 0;
    };
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    cs.releases
        .iter()
        .filter(|r| !r.prerelease)
        .filter_map(|r| version_numbers(&r.tag))
        .map(|v| {
            let major = at(&v, 0) > at(&p, 0);
            let minor = at(&v, 0) == at(&p, 0) && at(&v, 1) > at(&p, 1);
            if major || (minor && at(&p, 0) == 0) {
                2
            } else if minor {
                1
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0)
}

/// The verdict without JEV (see the module docs of [`crate::judge`] for the questions).
pub fn heuristic(up: &Upstream, cs: &ChangeSet, s: &Summary) -> Verdict {
    if cs.is_empty() {
        return Verdict {
            importance: 0,
            importance_conf: 0.9,
            breaking_p: 0.0,
            security_p: 0.0,
            relies_p: 0.0,
            effort: "small".into(),
            effort_conf: 0.9,
            judged_by: "heuristic".into(),
            model: "heuristic".into(),
        };
    }
    let any_text = |f: fn(&str) -> bool| texts(cs).any(f);
    let security = s.counts.security > 0 || any_text(is_security);
    let breaking = s.counts.breaking > 0 || any_text(is_breaking);
    let n = cs.commits_total;
    let importance = match bump(cs.pin.as_deref(), cs) {
        2 => 4,
        1 => 3,
        _ if n >= 100 => 3,
        _ if n >= 20 || !cs.releases.is_empty() => 2,
        _ => 1,
    };
    Verdict {
        importance,
        importance_conf: 0.5,
        breaking_p: if breaking { 0.7 } else { 0.1 },
        security_p: if security { 0.9 } else { 0.05 },
        relies_p: if touches_rely_on(up, cs) { 0.5 } else { 0.1 },
        effort: match n {
            0..=20 => "small",
            21..=150 => "medium",
            _ => "large",
        }
        .into(),
        effort_conf: 0.5,
        judged_by: "heuristic".into(),
        model: "heuristic".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{Commit, Rel};
    use crate::testutil::upstream;

    fn commit(subject: &str) -> Commit {
        Commit {
            sha7: "abcdef1".into(),
            date: "2026-10-20T00:00:00Z".into(),
            subject: subject.into(),
        }
    }

    fn rel(tag: &str) -> Rel {
        Rel {
            tag: tag.into(),
            name: tag.into(),
            date: "2026-10-20T10:00:00Z".into(),
            prerelease: false,
            notes: "## What's changed\n- Faster boot".into(),
            url: String::new(),
        }
    }

    fn cs(releases: Vec<Rel>, subjects: &[&str], total: u64) -> ChangeSet {
        ChangeSet {
            id: "x".into(),
            pin: Some("v0.19.0".into()),
            latest: Some("v0.20.1".into()),
            releases,
            commits_total: total,
            commits: subjects.iter().map(|s| commit(s)).collect(),
            commits_known: true,
            ..ChangeSet::default()
        }
    }

    #[test]
    fn classification() {
        assert_eq!(classify("feat(vfs): add overlayfs").kind, Kind::Feat);
        assert_eq!(classify("fix: off by one").kind, Kind::Fix);
        assert_eq!(classify("perf(mm): faster alloc").kind, Kind::Perf);
        assert_eq!(classify("docs: typo").kind, Kind::Other);
        assert_eq!(classify("Add support for riscv").kind, Kind::Feat);
        assert_eq!(classify("Fix a deadlock in the scheduler").kind, Kind::Fix);
        assert!(classify("fix: CVE-2026-1234 heap overflow").security);
        assert!(classify("Sanitize user paths").security);
        assert!(!classify("Update author list").security);
        assert!(classify("feat!: new config format").breaking);
        assert!(classify("Remove the legacy API").breaking);
        assert!(classify("feat: drop support for Node 18").breaking);
        assert!(classify("chore!: drop Node 18").breaking);
        assert!(!classify("fix(app): remove blank space above footers").breaking);
        assert!(!classify("test(server): remove timing assumptions").breaking);
        assert!(!classify("Fix links without breaking Files").breaking);
        assert_eq!(
            classify("Add a regression test for mounts").kind,
            Kind::Other
        );
    }

    #[test]
    fn headline_and_lines() {
        let c = cs(
            vec![rel("v0.20.1"), rel("v0.20.0")],
            &[
                "fix: a",
                "fix: b",
                "feat: c",
                "docs: d",
                "fix(net): CVE-2026-1 overflow in parser",
            ],
            143,
        );
        let s = summarize(&c);
        assert_eq!(
            s.headline,
            "v0.20.0, v0.20.1 + 143 commits (3 fixes, 1 feature, 1 security)"
        );
        assert_eq!(s.counts.other, 1);
        assert_eq!(s.lines[0], "Release v0.20.1 (2026-10-20): Faster boot");
        assert_eq!(
            s.lines[2],
            "Security: fix(net): CVE-2026-1 overflow in parser"
        );
        assert_eq!(s.lines[3], "Feature: feat: c");
        assert!(s.lines.len() <= LINES_MAX);
        let many: Vec<String> = (0..40).map(|i| format!("fix: {i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(summarize(&cs(vec![], &refs, 40)).lines.len(), LINES_MAX);
        assert_eq!(summarize(&cs(vec![], &[], 0)).headline, "nothing new");
        let skipped = ChangeSet {
            skipped: Some("frozen: not watched".into()),
            ..ChangeSet::default()
        };
        assert!(summarize(&skipped).headline.starts_with("skipped"));
    }

    #[test]
    fn heuristic_levels() {
        let up = upstream();
        let v = |c: &ChangeSet| heuristic(&up, c, &summarize(c));
        assert_eq!(v(&cs(vec![], &[], 0)).importance, 0);
        assert_eq!(v(&cs(vec![], &["fix: x"], 3)).importance, 1);
        assert_eq!(v(&cs(vec![], &["fix: x"], 25)).importance, 2);
        assert_eq!(v(&cs(vec![], &["fix: x"], 120)).importance, 3);
        // 0.19 → 0.20 while the major is 0: a major bump.
        let big = v(&cs(vec![rel("v0.20.0")], &["fix: x"], 5));
        assert_eq!(big.importance, 4);
        let mut patch = cs(vec![rel("v0.19.1")], &["fix: x"], 5);
        assert_eq!(v(&patch).importance, 2);
        patch.pin = Some("v1.2.0".into());
        patch.releases = vec![rel("v1.3.0")];
        assert_eq!(v(&patch).importance, 3);
        patch.releases = vec![rel("v2.0.0")];
        assert_eq!(v(&patch).importance, 4);

        let sec = v(&cs(vec![], &["fix: use-after-free in virtio"], 3));
        assert_eq!(sec.security_p, 0.9);
        assert_eq!(sec.relies_p, 0.5);
        assert_eq!(sec.effort, "small");
        assert_eq!(sec.judged_by, "heuristic");
        let plain = v(&cs(vec![], &["feat: Remove x"], 200));
        assert_eq!(
            (plain.security_p, plain.breaking_p, plain.relies_p),
            (0.05, 0.7, 0.1)
        );
        assert_eq!(plain.effort, "large");
        assert_eq!(v(&cs(vec![], &["fix: x"], 100)).effort, "medium");
    }
}
