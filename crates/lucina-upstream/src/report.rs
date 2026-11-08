//! One weekly run: its record (`<data_dir>/runs/<date>.json`) and Markdown report (`<date>.md`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fetch::{ChangeSet, OursRecent};
use crate::judge::Verdict;
use crate::registry::Relation;
use crate::summary::Summary;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryResult {
    pub id: String,
    pub name: String,
    pub relation: Relation,
    /// new | none | skipped | error.
    pub status: String,
    pub changes: ChangeSet,
    pub summary: Summary,
    pub verdict: Option<Verdict>,
    /// Why the heuristic judged instead of JEV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// `asked dc-…`, `pending dc-…`, `chosen port`, `would ask (dry run)`, `capped`…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default)]
    pub ours_recent: Vec<OursRecent>,
}

impl EntryResult {
    pub fn status_of(cs: &ChangeSet) -> &'static str {
        if cs.error.is_some() {
            "error"
        } else if cs.skipped.is_some() {
            "skipped"
        } else if cs.is_empty() {
            "none"
        } else {
            "new"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklogItem {
    pub id: String,
    pub latest: Option<String>,
    pub decided_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContribLine {
    pub upstream: String,
    pub id: String,
    pub title: String,
    /// The registry's status, then what the state knows (`asked dc-…`, `drafting`).
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Run {
    pub started_at: String,
    pub finished_at: String,
    pub dry_run: bool,
    /// Where the registry came from (`origin/main:docs/upstreams.toml`, a path, `built-in copy`).
    pub registry: String,
    pub entries: Vec<EntryResult>,
    pub backlog: Vec<BacklogItem>,
    pub contrib: Vec<ContribLine>,
    pub warnings: Vec<String>,
}

impl Run {
    pub fn new_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status == "new").count()
    }
    pub fn asked_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                e.decision
                    .as_deref()
                    .is_some_and(|d| d.starts_with("asked"))
            })
            .count()
    }
}

pub fn report_md(run: &Run) -> String {
    let date = run.started_at.get(..10).unwrap_or(&run.started_at);
    let mut md = format!(
        "# Upstream watch {date}{}\n\n{} entries: {} new, {} asked. Registry: {}.\n",
        if run.dry_run { " (dry run)" } else { "" },
        run.entries.len(),
        run.new_count(),
        run.asked_count(),
        run.registry
    );
    for w in &run.warnings {
        md.push_str(&format!("\n> Warning: {w}\n"));
    }
    for e in &run.entries {
        md.push_str(&format!("\n## {} ({}): {}\n\n", e.name, e.id, e.status));
        md.push_str(&format!("{}\n", e.summary.headline));
        let cs = &e.changes;
        if e.status == "new" || e.status == "none" {
            md.push_str(&format!(
                "\n- Pin {} → latest {}\n",
                cs.pin.as_deref().unwrap_or("?"),
                cs.latest.as_deref().unwrap_or("?")
            ));
        }
        if let Some(u) = &cs.compare_url {
            md.push_str(&format!("- Compare: {u}\n"));
        }
        if let Some(v) = &e.verdict {
            md.push_str(&format!("- Verdict ({}): {}\n", v.judged_by, v.line()));
        }
        if let Some(f) = &e.fallback {
            md.push_str(&format!("- JEV unavailable: {f}\n"));
        }
        if let Some(d) = &e.decision {
            md.push_str(&format!("- Decision: {d}\n"));
        }
        if !e.summary.lines.is_empty() {
            md.push('\n');
            for l in &e.summary.lines {
                md.push_str(&format!("- {l}\n"));
            }
        }
    }
    if !run.backlog.is_empty() {
        md.push_str("\n## Backlog (chose Later)\n\n");
        for b in &run.backlog {
            md.push_str(&format!(
                "- {} at {} (since {})\n",
                b.id,
                b.latest.as_deref().unwrap_or("?"),
                b.decided_at
                    .as_deref()
                    .map_or("?", |d| d.get(..10).unwrap_or(d))
            ));
        }
    }
    if !run.contrib.is_empty() {
        md.push_str("\n## Contributions upstream (draft only)\n\n");
        for c in &run.contrib {
            md.push_str(&format!(
                "- {} → {}: {} ({})\n",
                c.id, c.upstream, c.title, c.status
            ));
        }
    }
    let ours: Vec<&EntryResult> = run
        .entries
        .iter()
        .filter(|e| matches!(e.relation, Relation::Port | Relation::Fork))
        .filter(|e| e.ours_recent.iter().any(|o| o.count > 0))
        .collect();
    if !ours.is_empty() {
        md.push_str("\n## Our side: possible contributions\n");
        for e in ours {
            for o in e.ours_recent.iter().filter(|o| o.count > 0) {
                md.push_str(&format!(
                    "\n**{}**: {} commits in {}/{}\n",
                    e.name, o.count, o.repo, o.path
                ));
                for s in &o.subjects {
                    md.push_str(&format!("- {s}\n"));
                }
            }
        }
    }
    md
}

/// Write `<dir>/<stem>.json` and `<dir>/<stem>.md` (0600); a same-day rerun overwrites them.
pub fn save_run(dir: &Path, stem: &str, run: &Run) -> anyhow::Result<(PathBuf, PathBuf)> {
    let json = dir.join(format!("{stem}.json"));
    let md = dir.join(format!("{stem}.md"));
    crate::state::write_private(&json, &serde_json::to_vec_pretty(run)?)?;
    crate::state::write_private(&md, report_md(run).as_bytes())?;
    Ok((json, md))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::summarize;
    use crate::testutil::{changes, verdict_of};

    #[test]
    fn report_sections() {
        let cs = changes("v0.20.0", 3);
        let run = Run {
            started_at: "2026-11-02T07:30:00+01:00".into(),
            finished_at: "2026-11-02T07:31:00+01:00".into(),
            registry: "origin/main:docs/upstreams.toml".into(),
            entries: vec![EntryResult {
                id: "x".into(),
                name: "X".into(),
                relation: Relation::Fork,
                status: EntryResult::status_of(&cs).into(),
                summary: summarize(&cs),
                changes: cs,
                verdict: Some(verdict_of(3, 0.1, 0.6)),
                fallback: None,
                decision: Some("asked dc-0123456789ab".into()),
                ours_recent: vec![OursRecent {
                    repo: "/r".into(),
                    path: "k".into(),
                    count: 2,
                    subjects: vec!["abc1234 faster boot".into()],
                    error: None,
                }],
            }],
            backlog: vec![BacklogItem {
                id: "y".into(),
                latest: Some("v2".into()),
                decided_at: Some("2026-10-26T10:00:00Z".into()),
            }],
            contrib: vec![ContribLine {
                upstream: "X".into(),
                id: "x-faster-boot".into(),
                title: "a faster boot path".into(),
                status: "candidate".into(),
            }],
            ..Run::default()
        };
        let md = report_md(&run);
        assert!(
            md.starts_with("# Upstream watch 2026-11-02\n\n1 entries: 1 new, 1 asked."),
            "{md}"
        );
        assert!(md.contains("## X (x): new"));
        assert!(md.contains("- Decision: asked dc-0123456789ab"));
        assert!(md.contains("## Backlog (chose Later)\n\n- y at v2 (since 2026-10-26)"));
        assert!(md.contains("x-faster-boot → X"));
        assert!(md.contains("**X**: 2 commits in /r/k"));

        let dir = tempfile::tempdir().unwrap();
        let (j, m) = save_run(dir.path(), "2026-11-02", &run).unwrap();
        let back: Run = serde_json::from_str(&std::fs::read_to_string(j).unwrap()).unwrap();
        assert_eq!(back, run);
        assert_eq!(std::fs::read_to_string(m).unwrap(), md);
    }
}
