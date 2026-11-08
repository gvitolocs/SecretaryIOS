//! The porting brief for Lucina Agents ("Port into ours") and the contribution draft plan.

use crate::clip;
use crate::fetch::ChangeSet;
use crate::judge::Verdict;
use crate::registry::{Contrib, Upstream};
use crate::summary::{classify, Summary};

/// The fixed rules every porting agent follows.
pub const PORT_RULES: &[&str] = &[
    "Follow `docs/porting-guidelines.md` in this worktree.",
    "The original is the source of truth for behaviour: copy behaviour, not structure.",
    "Rust-first: no new Python or shell helpers.",
    "Record every upstream change as adopt / adapt / reference / reject, with one line why.",
    "Licence check before copying anything (see the licence pair above).",
    "Tests against the original's behaviour (its tests, its outputs), not only our own.",
    "When done, update `pin` in `docs/upstreams.toml` on main to the version you ported.",
    "Merge to main inside `flock /tmp/lucina-merge.lock`; deploy only from main through deploy-guard.",
    "No Codex on Lucina. Never the paid DeepSeek path.",
    "Go-aheads through lucina_decide, never by guessing.",
];

pub fn port_brief(
    up: &Upstream,
    cs: &ChangeSet,
    summary: &Summary,
    v: &Verdict,
    workspace_cwd: &str,
) -> String {
    let latest = cs.latest.as_deref().unwrap_or("its latest");
    let mut md = format!(
        "# Port {name} {latest} into {ours}\n\n\
         **Goal:** bring {ours} to parity with {name} {latest} (pinned at {pin}).\n\n\
         - Upstream: {url}\n- Workspace: `{workspace_cwd}`\n- Relation: {rel:?}\n\
         - Verdict ({by}): {line}\n- Summary: {headline}\n",
        name = up.name,
        ours = up.ours_label(),
        pin = cs.pin.as_deref().unwrap_or("?"),
        url = up.url,
        rel = up.relation,
        by = v.judged_by,
        line = v.line(),
        headline = summary.headline,
    );
    if let Some(u) = &cs.compare_url {
        md.push_str(&format!("- Compare: {u}\n"));
    }
    md.push_str(&format!(
        "- Licences: upstream {}, ours {}\n",
        up.licence_upstream, up.licence_ours
    ));
    if !up.rely_on.is_empty() {
        md.push_str(&format!("- We rely on: {}\n", up.rely_on.join(", ")));
    }

    if !cs.releases.is_empty() {
        md.push_str("\n## Releases\n");
        for r in &cs.releases {
            md.push_str(&format!(
                "\n### {}{}{}\n",
                r.tag,
                if r.date.len() >= 10 {
                    format!(" ({})", &r.date[..10])
                } else {
                    String::new()
                },
                if r.prerelease { " [pre-release]" } else { "" }
            ));
            if !r.url.is_empty() {
                md.push_str(&format!("{}\n", r.url));
            }
            if !r.notes.is_empty() {
                md.push_str(&format!("\n{}\n", clip(&r.notes, 1500)));
            }
        }
    }

    let picked: Vec<String> = cs
        .commits
        .iter()
        .filter_map(|c| {
            let cl = classify(&c.subject);
            let label = if cl.security {
                "security"
            } else if cl.breaking {
                "breaking"
            } else {
                match cl.kind {
                    crate::summary::Kind::Feat => "feat",
                    crate::summary::Kind::Fix => "fix",
                    _ => return None,
                }
            };
            Some(format!("- [{label}] `{}` {}", c.sha7, c.subject))
        })
        .take(60)
        .collect();
    if !picked.is_empty() {
        md.push_str(&format!(
            "\n## Changes ({} of {} commits; docs, CI and chores left out)\n\n{}\n",
            picked.len(),
            cs.commits_total,
            picked.join("\n")
        ));
    }
    if !cs.files_touched.is_empty() {
        md.push_str(&format!(
            "\n## Files touched (by directory)\n\n{}\n",
            cs.files_touched
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    md.push_str("\n## Rules\n\n");
    for r in PORT_RULES {
        md.push_str(&format!("- {r}\n"));
    }
    md
}

/// The draft plan for contributing one of our improvements back.
pub fn contrib_brief(up: &Upstream, c: &Contrib) -> String {
    let base = up.url.trim_end_matches('/');
    let mut md = format!(
        "# Contribution draft: {title} → {name}\n\n\
         **Draft only. Do not push, fork or open a PR upstream without the owner's explicit OK in chat.**\n\n\
         ## What\n\n{title}\n\n- Ours: {ours}\n",
        title = c.title,
        name = up.name,
        ours = c.ours,
    );
    if let Some(t) = &c.upstream_target {
        md.push_str(&format!("- Upstream target: {t}\n"));
    }
    if !c.notes.is_empty() {
        md.push_str(&format!("- Notes: {}\n", c.notes));
    }
    md.push_str(&format!(
        "\n## Plan\n\n\
         1. List our files and commits that make up the improvement ({ours}).\n\
         2. Map them onto the upstream's current code ({url}, branch {branch}).\n\
         3. Licence compatibility: ours {lo}, upstream {lu}. Only our own code may go upstream, under \
         the upstream's licence.\n\
         4. Read the upstream's contribution rules: {base}/blob/{branch}/CONTRIBUTING.md\n\
         5. Write the patch and its description as local files only, then ask the owner.\n",
        ours = c.ours,
        url = up.url,
        branch = up.branch(),
        lo = up.licence_ours,
        lu = up.licence_upstream,
    ));
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::summarize;
    use crate::testutil::{changes, upstream, verdict_of};

    #[test]
    fn port_brief_has_the_rules() {
        let up = upstream();
        let cs = changes("v0.20.0", 70);
        let md = port_brief(
            &up,
            &cs,
            &summarize(&cs),
            &verdict_of(3, 0.1, 0.6),
            "/w/port-x",
        );
        assert!(md.starts_with("# Port X v0.20.0 into /r/k"));
        for r in PORT_RULES {
            assert!(md.contains(r), "{r}");
        }
        assert!(md.contains("docs/porting-guidelines.md"));
        assert!(md.contains("flock /tmp/lucina-merge.lock"));
        assert!(md.contains("Changes (60 of 70 commits"));
        assert!(md.contains("Licences: upstream MPL-2.0, ours MPL-2.0"));
        assert!(!md.to_lowercase().contains("push"));
    }

    #[test]
    fn contrib_brief_is_draft_only() {
        let up = upstream();
        let md = contrib_brief(&up, &up.contrib[0]);
        assert!(md.contains("**Draft only. Do not push, fork or open a PR upstream without the owner's explicit OK in chat.**"));
        assert!(md.contains("https://github.com/a/x/blob/main/CONTRIBUTING.md"));
        assert_eq!(md.to_lowercase().matches("push").count(), 1, "{md}");
        assert!(md.contains("Do not push"));
    }
}
