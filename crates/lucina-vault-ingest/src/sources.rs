//! Where items come from: the ops record, the first-parent history of the
//! Lucina repos, and Claude session reports. Every item is redacted and
//! clipped here, before it is queued.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use lucina_ops::StoredRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::queue::{Backlink, Item, SourceKind};
use crate::redact::redact;

/// What the model may read of one item.
const ITEM_TEXT: usize = 12_000;
/// Candidate files per item.
const MAX_FILES: usize = 40;
/// Commits taken from one repo per scan (the rest come next scan).
const MAX_COMMITS: usize = 50;
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Records the ingest writes itself are never ingested (no loops).
pub const OWN_SOURCE: &str = "vault-ingest";

fn hex16(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn text(s: &str) -> String {
    crate::clip(&redact(s), ITEM_TEXT)
}

/// An absolute path under the owner's projects or data that exists.
fn real_path(s: &str) -> Option<String> {
    let s = s.trim().trim_matches('`');
    let ok = (s.starts_with("/home/nez/Projects/") || s.starts_with("/home/nez/data/"))
        && !s.contains("/../")
        && Path::new(s).exists();
    ok.then(|| s.to_owned())
}

// ---------------------------------------------------------------- ops

/// One item per record (made at or after `since`, not the ingest's own).
pub fn ops_items(records: &[StoredRecord], since: DateTime<Utc>) -> Vec<Item> {
    let mut v: Vec<&StoredRecord> = records
        .iter()
        .filter(|r| r.recorded_at >= since && r.source != OWN_SOURCE)
        .collect();
    v.sort_by_key(|r| r.recorded_at);
    v.into_iter().map(ops_item).collect()
}

fn ops_item(r: &StoredRecord) -> Item {
    let mut t = format!("Kind: {}\n", r.kind.as_str());
    if let Some(s) = r.severity {
        t.push_str(&format!("Severity: {}\n", s.as_str()));
    }
    for (k, v) in [("Host", &r.host), ("Commit", &r.commit)] {
        if let Some(v) = v {
            t.push_str(&format!("{k}: {v}\n"));
        }
    }
    if let Some(d) = r.decided_by {
        t.push_str(&format!("Decided by: {}\n", d.as_str()));
    }
    if let Some(q) = &r.question {
        t.push_str(&format!("Question: {q}\n"));
    }
    t.push_str(&format!("Reported by: {}\n\n{}\n", r.source, r.body));
    for (k, v) in [("Cause", &r.cause), ("Fix", &r.fix)] {
        if let Some(v) = v {
            t.push_str(&format!("\n{k}: {v}\n"));
        }
    }
    for (k, list) in [
        ("Timeline", &r.timeline),
        ("Follow-ups", &r.follow_ups),
        ("Links", &r.links),
    ] {
        if !list.is_empty() {
            t.push_str(&format!("\n{k}:\n"));
            for l in list {
                t.push_str(&format!("- {l}\n"));
            }
        }
    }
    let mut files: Vec<String> = r.links.iter().filter_map(|l| real_path(l)).collect();
    files.dedup();
    files.truncate(MAX_FILES);
    Item {
        key: format!("ops:{}", r.id),
        source: SourceKind::Ops,
        project: r.project.clone(),
        title: redact(&r.title),
        at: r.at,
        text: text(&t),
        files,
        backlink: Backlink::Ops {
            id: r.id.clone(),
            log: r.paths.log.clone(),
            incident: r.paths.incident.clone(),
            commit: r.commit.clone(),
        },
    }
}

// ---------------------------------------------------------------- git

/// A repo whose branch is followed: `name=path@ref#project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepo {
    pub name: String,
    pub path: String,
    /// The ref followed (`origin/main`, `usb-dev-loop`). Never fetched here.
    pub refname: String,
    /// Project slug of its notes.
    pub project: String,
}

impl GitRepo {
    /// `name=path@ref#project` (`#project` defaults to the name).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        let (name, rest) = spec
            .split_once('=')
            .ok_or_else(|| format!("{spec:?}: expected name=path@ref[#project]"))?;
        let (rest, project) = match rest.rsplit_once('#') {
            Some((r, p)) => (r, p.trim()),
            None => (rest, name.trim()),
        };
        let (path, refname) = rest
            .rsplit_once('@')
            .ok_or_else(|| format!("{spec:?}: expected path@ref"))?;
        let slug = |s: &str| {
            !s.is_empty()
                && s.len() <= 40
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        };
        let refname = refname.trim();
        if !slug(name.trim()) || !slug(project) {
            return Err(format!(
                "{spec:?}: name and project must be lower-case slugs"
            ));
        }
        if !path.trim().starts_with('/') {
            return Err(format!("{spec:?}: the path must be absolute"));
        }
        if refname.is_empty()
            || refname.starts_with('-')
            || refname.contains("..")
            || !refname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        {
            return Err(format!("{spec:?}: bad ref"));
        }
        Ok(Self {
            name: name.trim().into(),
            path: path.trim().into(),
            refname: refname.into(),
            project: project.into(),
        })
    }

    /// `a=…;b=…` (also newline- or comma-separated).
    pub fn parse_list(s: &str) -> Result<Vec<Self>, String> {
        s.split([';', '\n', ','])
            .filter(|p| !p.trim().is_empty())
            .map(Self::parse)
            .collect()
    }

    async fn git(&self, args: &[&str]) -> Result<String, String> {
        let out = tokio::time::timeout(
            GIT_TIMEOUT,
            tokio::process::Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| format!("git {} timed out", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                crate::clip(String::from_utf8_lossy(&out.stderr).trim(), 300)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn head(&self) -> Result<String, String> {
        let r = format!("{}^{{commit}}", self.refname);
        Ok(self
            .git(&["rev-parse", "--verify", "--quiet", &r])
            .await?
            .trim()
            .to_owned())
    }

    /// The first-parent commits after `cursor` up to `head`, oldest first;
    /// with no usable cursor, the last `backfill` ones.
    pub async fn new_commits(
        &self,
        cursor: Option<&str>,
        head: &str,
        backfill: usize,
    ) -> Result<Vec<String>, String> {
        let from_cursor = match cursor {
            Some(c) if c == head => return Ok(Vec::new()),
            Some(c) => self
                .git(&["merge-base", "--is-ancestor", c, head])
                .await
                .is_ok()
                .then(|| c.to_owned()),
            None => None,
        };
        let out = match &from_cursor {
            Some(c) => {
                let range = format!("{c}..{head}");
                self.git(&["rev-list", "--first-parent", &range]).await?
            }
            None => {
                let n = backfill.to_string();
                self.git(&["rev-list", "--first-parent", "-n", &n, head])
                    .await?
            }
        };
        let mut shas: Vec<String> = out
            .lines()
            .map(str::trim)
            .filter(|l| l.len() == 40)
            .map(str::to_owned)
            .collect();
        // rev-list is newest first; keep the oldest MAX_COMMITS for this scan.
        shas.reverse();
        shas.truncate(MAX_COMMITS);
        Ok(shas)
    }

    /// The item of one commit (a merge carries the commits it brought in).
    pub async fn item(&self, sha: &str, tz: &chrono_tz::Tz) -> Result<Item, String> {
        const SEP: char = '\u{1f}';
        let meta = self
            .git(&["show", "-s", "--format=%h%x1f%aI%x1f%P%x1f%s%x1f%b", sha])
            .await?;
        let mut p = meta.splitn(5, SEP);
        let short = p.next().unwrap_or("").trim().to_owned();
        let date = p.next().unwrap_or("").trim().to_owned();
        let parents: Vec<String> = p
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let subject = p.next().unwrap_or("").trim().to_owned();
        let body = p.next().unwrap_or("").trim().to_owned();
        let merge = parents.len() > 1;
        let base = parents.first().cloned();
        let (stat, names) = match &base {
            Some(b) => (
                self.git(&["diff", "--stat=120,90", b, sha]).await?,
                self.git(&["diff", "--name-only", b, sha]).await?,
            ),
            None => (
                self.git(&["show", "--stat=120,90", "--format=", sha])
                    .await?,
                self.git(&["show", "--name-only", "--format=", sha]).await?,
            ),
        };
        let merged = if merge {
            let range = format!("{}..{}", parents[0], parents[1]);
            self.git(&["log", "--no-merges", "--format=%h %s", "-n", "30", &range])
                .await
                .unwrap_or_default()
        } else {
            String::new()
        };
        // Only files that exist at the followed ref are candidates.
        let changed: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(300)
            .collect();
        let mut files = Vec::new();
        for chunk in changed.chunks(100) {
            let mut args = vec!["ls-tree", "-r", "--name-only", self.refname.as_str(), "--"];
            args.extend(chunk.iter().copied());
            if let Ok(out) = self.git(&args).await {
                files.extend(
                    out.lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| format!("{}/{}", self.path.trim_end_matches('/'), l.trim())),
                );
            }
        }
        files.truncate(MAX_FILES);
        let stat_lines: Vec<&str> = stat.lines().collect();
        let stat_tail = stat_lines[stat_lines.len().saturating_sub(40)..].join("\n");
        let mut t = format!(
            "{} {short} on {} ({})\nDate: {date}\nSubject: {subject}\n",
            if merge { "Merge" } else { "Commit" },
            self.name,
            self.refname
        );
        if !body.is_empty() {
            t.push_str(&format!("\n{body}\n"));
        }
        if !merged.trim().is_empty() {
            t.push_str(&format!("\nCommits it merged:\n{}\n", merged.trim()));
        }
        t.push_str(&format!("\nFiles changed:\n{}\n", stat_tail.trim_end()));
        let at = DateTime::parse_from_rfc3339(&date)
            .map(|d| d.with_timezone(tz).fixed_offset())
            .unwrap_or_else(|_| Utc::now().with_timezone(tz).fixed_offset());
        Ok(Item {
            key: format!("git:{}:{sha}", self.name),
            source: SourceKind::Git,
            project: self.project.clone(),
            title: redact(&subject),
            at,
            text: text(&t),
            files,
            backlink: Backlink::Git {
                repo: self.name.clone(),
                path: self.path.clone(),
                branch: self.refname.clone(),
                sha: sha.to_owned(),
                merge,
            },
        })
    }
}

// ---------------------------------------------------------------- sessions

/// A Claude session wrap-up report: `{id?, title, project, summary|report|text, at?}`.
pub fn session_item(v: &Value, now: DateTime<FixedOffset>) -> Result<Item, String> {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
    };
    let title = s("title");
    let body = [s("summary"), s("report"), s("text")]
        .into_iter()
        .find(|b| !b.is_empty())
        .unwrap_or("");
    if title.is_empty() || body.is_empty() {
        return Err("a session report needs a title and a summary".into());
    }
    let project = match s("project") {
        "" => "lucina",
        p => p,
    };
    if !project
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || project.len() > 40
    {
        return Err("project must be a lower-case slug".into());
    }
    let id = match s("id") {
        "" => hex16(&format!("{title}\n{body}")),
        i => crate::clip(i, 80),
    };
    let at = DateTime::parse_from_rfc3339(s("at")).unwrap_or(now);
    let mut files: Vec<String> = v
        .get("files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(real_path)
                .collect()
        })
        .unwrap_or_default();
    files.truncate(MAX_FILES);
    Ok(Item {
        key: format!("session:{}", hex16(&id)),
        source: SourceKind::Session,
        project: project.to_owned(),
        title: redact(title),
        at,
        text: text(body),
        files,
        backlink: Backlink::Session { id: redact(&id) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_specs() {
        let r =
            GitRepo::parse("lucina-rs=/home/nez/Projects/Lucina/SecretaryIOS@origin/main#lucina")
                .unwrap();
        assert_eq!(
            (
                r.name.as_str(),
                r.path.as_str(),
                r.refname.as_str(),
                r.project.as_str()
            ),
            (
                "lucina-rs",
                "/home/nez/Projects/Lucina/SecretaryIOS",
                "origin/main",
                "lucina"
            )
        );
        let r = GitRepo::parse("lucinaos=/x/y@usb-dev-loop").unwrap();
        assert_eq!(r.project, "lucinaos");
        for bad in [
            "x",
            "x=/p",
            "X=/p@main",
            "x=rel@main",
            "x=/p@--upload-pack=evil",
            "x=/p@a..b",
            "x=/p@main#Bad Slug",
        ] {
            assert!(GitRepo::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(GitRepo::parse_list("a=/a@m; b=/b@n\n").unwrap().len(), 2);
    }

    #[test]
    fn sessions() {
        let now = DateTime::parse_from_rfc3339("2026-11-01T10:00:00+01:00").unwrap();
        let v = serde_json::json!({"title": "Kindle OTA", "project": "lucinaos",
            "summary": "A/B slots with psk=hunter2hunter2 in the config", "id": "local_77"});
        let a = session_item(&v, now).unwrap();
        let b = session_item(&v, now).unwrap();
        assert_eq!(a.key, b.key, "same report, same key");
        assert!(!a.text.contains("hunter2"));
        assert!(session_item(&serde_json::json!({"title": "x"}), now).is_err());
        assert!(session_item(
            &serde_json::json!({"title": "x", "summary": "y", "project": "Bad"}),
            now
        )
        .is_err());
    }
}
