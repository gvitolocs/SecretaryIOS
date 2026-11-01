//! What an upstream released since our pin ([`changes`]), through a [`Fetcher`]: `gh api`, `git` and
//! `curl` as subprocesses ([`SystemFetcher`]), or a fake in tests.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::{Source, Track, Upstream};
use crate::{clip, looks_like_sha, same_ref};

/// Release notes are clipped to this many characters.
pub const NOTES_MAX: usize = 1500;
pub const COMMITS_MAX: usize = 200;
pub const FILES_MAX: usize = 40;
/// Pages of 100 commits read when counting since a date.
const SINCE_PAGES: usize = 3;

/// The network, for [`changes`].
pub trait Fetcher {
    /// `gh api <path>` (`repos/o/n/releases?per_page=30`) as JSON.
    fn github(&self, path: &str) -> anyhow::Result<Value>;
    /// `git <args>` (in `cwd` when given), stdout.
    fn git(&self, args: &[&str], cwd: Option<&Path>) -> anyhow::Result<String>;
    /// A public JSON URL.
    fn http_json(&self, url: &str) -> anyhow::Result<Value>;
}

/// gh, git and curl from PATH, each under `timeout`.
pub struct SystemFetcher;

fn run(mut cmd: Command, what: &str) -> anyhow::Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("{what}: could not start"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let code = out.status.code().map_or("signal".into(), |c| c.to_string());
        // `timeout` exits 124 when it kills the command.
        if code == "124" {
            anyhow::bail!("{what}: timed out");
        }
        anyhow::bail!("{what}: exit {code}: {}", clip(err.trim(), 300));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Fetcher for SystemFetcher {
    fn github(&self, path: &str) -> anyhow::Result<Value> {
        let mut c = Command::new("timeout");
        c.args([
            "60",
            "gh",
            "api",
            "-H",
            "Accept: application/vnd.github+json",
            path,
        ]);
        let out = run(c, &format!("gh api {path}"))?;
        serde_json::from_str(&out).with_context(|| format!("gh api {path}: not JSON"))
    }

    fn git(&self, args: &[&str], cwd: Option<&Path>) -> anyhow::Result<String> {
        let mut c = Command::new("timeout");
        c.arg("120").arg("git").args(args);
        // Never prompt for credentials in a timer.
        c.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        run(c, &format!("git {}", args.first().unwrap_or(&"")))
    }

    fn http_json(&self, url: &str) -> anyhow::Result<Value> {
        let mut c = Command::new("curl");
        c.args(["-fsSL", "-m", "30", url]);
        let out = run(c, &format!("curl {url}"))?;
        serde_json::from_str(&out).with_context(|| format!("{url}: not JSON"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rel {
    pub tag: String,
    pub name: String,
    pub date: String,
    pub prerelease: bool,
    /// Clipped to [`NOTES_MAX`] characters.
    pub notes: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub sha7: String,
    pub date: String,
    pub subject: String,
}

/// An upstream's news since our pin.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChangeSet {
    pub id: String,
    /// The pin it was compared against (`pin`, else `pin_date`'s date).
    #[serde(default)]
    pub pin: Option<String>,
    /// Newest release, tag or version, else the head sha.
    pub latest: Option<String>,
    /// Newer than the pin, newest first.
    pub releases: Vec<Rel>,
    pub commits_total: u64,
    /// Newest first, at most [`COMMITS_MAX`].
    pub commits: Vec<Commit>,
    /// `dir (count)`, the top [`FILES_MAX`] directories.
    pub files_touched: Vec<String>,
    pub compare_url: Option<String>,
    /// False when only the head is known (`git ls-remote`): `latest` ≠ `pin` is then the news.
    #[serde(default = "yes")]
    pub commits_known: bool,
    pub skipped: Option<String>,
    pub error: Option<String>,
}

fn yes() -> bool {
    true
}

impl ChangeSet {
    fn new(up: &Upstream) -> Self {
        Self {
            id: up.id.clone(),
            pin: up.pin.clone().or_else(|| up.pin_date.clone()),
            commits_known: true,
            ..Self::default()
        }
    }

    /// Nothing new since the pin (also true when skipped or failed).
    pub fn is_empty(&self) -> bool {
        if self.skipped.is_some() || self.error.is_some() {
            return true;
        }
        if !self.releases.is_empty() || self.commits_total > 0 {
            return false;
        }
        if self.commits_known {
            return true;
        }
        match (&self.latest, &self.pin) {
            (Some(l), Some(p)) => same_ref(l, p),
            _ => true,
        }
    }

    /// What JEV reads: few releases with short notes, few subjects, few paths (keeps the
    /// `jev/upstream` body small).
    pub fn for_judge(&self) -> Self {
        let mut c = self.clone();
        c.releases.truncate(5);
        for r in &mut c.releases {
            r.notes = clip(&r.notes, 600);
        }
        c.commits.truncate(60);
        c.files_touched.truncate(20);
        c
    }
}

/// [`changes`], with a failure recorded in `error` instead.
pub fn changes_or_error(up: &Upstream, f: &dyn Fetcher, since_seen: Option<&str>) -> ChangeSet {
    changes(up, f, since_seen).unwrap_or_else(|e| ChangeSet {
        error: Some(clip(&format!("{e:#}"), 500)),
        ..ChangeSet::new(up)
    })
}

/// What `up` released since its pin. `since_seen` (the last `latest` seen) is the base only when the
/// entry has neither `pin` nor `pin_date`.
pub fn changes(
    up: &Upstream,
    f: &dyn Fetcher,
    since_seen: Option<&str>,
) -> anyhow::Result<ChangeSet> {
    let mut cs = ChangeSet::new(up);
    match up.source {
        Source::Frozen => {
            cs.skipped = Some("frozen: not watched".into());
        }
        Source::Manual => {
            cs.skipped = Some(format!(
                "manual: {}",
                up.check_hint.as_deref().unwrap_or(&up.url)
            ));
        }
        Source::Github => github(up, f, since_seen, &mut cs)?,
        Source::Git => git(up, f, &mut cs)?,
        Source::Debian => debian(up, f, &mut cs)?,
    }
    Ok(cs)
}

// ------------------------------------------------------------------ github

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_owned()
}

fn first_line(m: &str) -> String {
    m.lines().next().unwrap_or("").trim().to_owned()
}

fn github(
    up: &Upstream,
    f: &dyn Fetcher,
    since_seen: Option<&str>,
    cs: &mut ChangeSet,
) -> anyhow::Result<()> {
    let repo = up.repo.as_deref().context("github entry without repo")?;
    let branch = up.branch();
    match up.track() {
        Track::Releases => releases(up, repo, f, cs)?,
        Track::Tags => tags(up, repo, f, cs)?,
        Track::Commits => {}
    }
    let base = up.pin.as_deref().or(if up.pin_date.is_none() {
        since_seen
    } else {
        None
    });
    if let Some(base) = base {
        compare(up, repo, base, branch, f, cs)?;
    } else if let Some(date) = &up.pin_date {
        since_date(up, repo, date, branch, f, cs)?;
    }
    if cs.latest.is_none() {
        let head = f.github(&format!("repos/{repo}/commits/{branch}"))?;
        cs.latest = head["sha"]
            .as_str()
            .map(|h| h[..h.len().min(12)].to_owned());
    }
    Ok(())
}

fn release_of(repo: &str, r: &Value) -> Rel {
    let tag = s(r, "tag_name");
    Rel {
        name: r["name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&tag)
            .to_owned(),
        date: s(r, "published_at"),
        prerelease: r["prerelease"].as_bool().unwrap_or(false),
        notes: clip(r["body"].as_str().unwrap_or("").trim(), NOTES_MAX),
        url: r["html_url"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("https://github.com/{repo}/releases/tag/{tag}")),
        tag,
    }
}

/// Releases newer than the pin: by the pin release's `published_at`, else by `pin_date`, else by
/// list order (the API lists newest first).
fn releases(up: &Upstream, repo: &str, f: &dyn Fetcher, cs: &mut ChangeSet) -> anyhow::Result<()> {
    let list = f.github(&format!("repos/{repo}/releases?per_page=30"))?;
    let all: Vec<&Value> = list
        .as_array()
        .context("releases: not a list")?
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .collect();
    // Newest first by publication, never by tag name (Hermes' `v2026.9.24` sorts last).
    let mut all = all;
    all.sort_by_key(|r| std::cmp::Reverse(s(r, "published_at")));
    // A commit pin may be a release's commit (asterinas pins v0.19.0 by its sha): its tag.
    let pin_tag = match up.pin.as_deref() {
        Some(p) if looks_like_sha(p) && !all.iter().any(|r| r["tag_name"] == p) => f
            .github(&format!("repos/{repo}/tags?per_page=100"))
            .ok()
            .and_then(|t| {
                t.as_array()?
                    .iter()
                    .find(|t| t["commit"]["sha"].as_str().is_some_and(|c| same_ref(c, p)))
                    .map(|t| s(t, "name"))
            }),
        p => p.map(str::to_owned),
    };
    let pin_rel = pin_tag
        .as_deref()
        .and_then(|p| all.iter().position(|r| r["tag_name"].as_str() == Some(p)));
    let pin_at = match (pin_rel, up.pin.as_deref()) {
        (Some(i), _) => Some(s(all[i], "published_at")),
        // A commit pin: releases published after that commit.
        (None, Some(pin)) if looks_like_sha(pin) => f
            .github(&format!("repos/{repo}/commits/{pin}"))
            .ok()
            .and_then(|c| c["commit"]["committer"]["date"].as_str().map(str::to_owned)),
        _ => None,
    };
    let newer: Vec<&Value> = match (pin_rel, pin_at.filter(|d| !d.is_empty()), &up.pin_date) {
        (_, Some(at), _) => all
            .iter()
            .copied()
            .filter(|r| s(r, "published_at") > at)
            .collect(),
        (Some(i), None, _) => all[..i].to_vec(),
        (None, _, Some(date)) => {
            let from = format!("{date}T00:00:00Z");
            all.iter()
                .copied()
                .filter(|r| s(r, "published_at") >= from)
                .collect()
        }
        // The pin is older than the 30 listed (or not a release): all of them are newer.
        (None, _, None) if up.pin.is_some() => all.clone(),
        (None, _, None) => Vec::new(),
    };
    cs.releases = newer.iter().map(|r| release_of(repo, r)).collect();
    cs.latest = all
        .iter()
        .find(|r| !r["prerelease"].as_bool().unwrap_or(false))
        .or(all.first())
        .map(|r| s(r, "tag_name"));
    Ok(())
}

/// Tags newer than the pin. The tags API does not list newest first (dropbear: `maemo-0.52-2`,
/// `ltm-0.30-orig`, …), so all tags come from `git ls-remote` and are compared by number.
fn tags(up: &Upstream, repo: &str, f: &dyn Fetcher, cs: &mut ChangeSet) -> anyhow::Result<()> {
    let all = remote_tags(f, &format!("https://github.com/{repo}.git"))?;
    let Some(pin) = up.pin.as_deref().filter(|p| !looks_like_sha(p)) else {
        return Ok(());
    };
    let newer = newer_tags(all.keys().map(String::as_str), pin);
    cs.releases = newer
        .iter()
        .map(|t| Rel {
            tag: t.clone(),
            name: t.clone(),
            date: String::new(),
            prerelease: false,
            notes: String::new(),
            url: format!("https://github.com/{repo}/tree/{t}"),
        })
        .collect();
    cs.latest = newer.first().cloned().or_else(|| Some(pin.to_owned()));
    Ok(())
}

/// A remote's tags (`git ls-remote --tags`): name → commit. Without `--refs`, so an annotated tag's
/// commit (the `<tag>^{}` line) wins over the tag object.
fn remote_tags(f: &dyn Fetcher, url: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let out = f.git(&["ls-remote", "--tags", url], None)?;
    let mut tags = BTreeMap::new();
    for (sha, name) in out.lines().filter_map(|l| {
        let (sha, r) = l.split_once('\t')?;
        Some((sha.trim(), r.trim().strip_prefix("refs/tags/")?))
    }) {
        match name.strip_suffix("^{}") {
            Some(n) => {
                tags.insert(n.to_owned(), sha.to_owned());
            }
            None => {
                tags.entry(name.to_owned())
                    .or_insert_with(|| sha.to_owned());
            }
        }
    }
    Ok(tags)
}

/// Tags newer than `pin` by [`tag_numbers`], highest first. Only tags with the pin's prefix count
/// (`DROPBEAR_`, `v`, none), so other series in the same repo (`libtommath-0.40`) are left out.
pub fn newer_tags<'a>(names: impl Iterator<Item = &'a str>, pin: &str) -> Vec<String> {
    let prefix = |t: &'a str| &t[..t.find(|c: char| c.is_ascii_digit()).unwrap_or(t.len())];
    let Some(pv) = tag_numbers(pin) else {
        return Vec::new();
    };
    let pin_prefix = &pin[..pin.find(|c: char| c.is_ascii_digit()).unwrap_or(pin.len())];
    let mut newer: Vec<(Vec<u64>, &str)> = names
        .filter(|t| prefix(t) == pin_prefix)
        .filter_map(|t| tag_numbers(t).map(|v| (v, t)))
        .filter(|(v, _)| *v > pv)
        .collect();
    newer.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    newer.into_iter().map(|(_, t)| t.to_owned()).collect()
}

fn commit_of(c: &Value) -> Commit {
    Commit {
        sha7: s(c, "sha").chars().take(7).collect(),
        date: c["commit"]["committer"]["date"]
            .as_str()
            .unwrap_or("")
            .to_owned(),
        subject: first_line(c["commit"]["message"].as_str().unwrap_or("")),
    }
}

fn touches(file: &str, paths: &[String]) -> bool {
    paths.is_empty()
        || paths.iter().any(|p| {
            let p = p.trim_end_matches('/');
            file == p || file.starts_with(&format!("{p}/"))
        })
}

/// `dir (n)` for the top directories (two levels deep), most touched first.
pub fn top_dirs<'a>(files: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let dir = if parts.len() <= 1 {
            ".".to_owned()
        } else {
            parts[..(parts.len() - 1).min(2)].join("/")
        };
        *count.entry(dir).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter()
        .take(FILES_MAX)
        .map(|(d, n)| format!("{d} ({n})"))
        .collect()
}

/// Commits of `repo`'s `branch` since `since` (ISO), touching any of `paths` (all when empty),
/// newest first, deduplicated.
fn commits_since(
    repo: &str,
    branch: &str,
    since: &str,
    paths: &[String],
    f: &dyn Fetcher,
) -> anyhow::Result<Vec<Value>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let filters: Vec<Option<&str>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.iter().map(|p| Some(p.as_str())).collect()
    };
    for p in filters {
        for page in 1..=SINCE_PAGES {
            let mut q = format!("repos/{repo}/commits?sha={branch}&since={since}&per_page=100");
            if let Some(p) = p {
                q.push_str(&format!("&path={p}"));
            }
            if page > 1 {
                q.push_str(&format!("&page={page}"));
            }
            let v = f.github(&q)?;
            let list = v.as_array().context("commits: not a list")?;
            for c in list {
                if seen.insert(s(c, "sha")) {
                    out.push(c.clone());
                }
            }
            if list.len() < 100 {
                break;
            }
        }
    }
    out.sort_by(|a, b| {
        let d = |c: &Value| {
            c["commit"]["committer"]["date"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        };
        d(b).cmp(&d(a))
    });
    Ok(out)
}

/// `compare/{base}...{branch}`: count, commits (oldest first in the API), files.
fn compare(
    up: &Upstream,
    repo: &str,
    base: &str,
    branch: &str,
    f: &dyn Fetcher,
    cs: &mut ChangeSet,
) -> anyhow::Result<()> {
    let v = f.github(&format!("repos/{repo}/compare/{base}...{branch}"))?;
    cs.compare_url = v["html_url"].as_str().map(str::to_owned);
    let files: Vec<&str> = v["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["filename"].as_str()).collect())
        .unwrap_or_default();
    cs.files_touched = top_dirs(files.iter().copied().filter(|x| touches(x, &up.paths)));
    if up.paths.is_empty() {
        cs.commits_total = v["ahead_by"].as_u64().unwrap_or(0);
        let mut commits: Vec<Commit> = v["commits"]
            .as_array()
            .map(|a| a.iter().map(commit_of).collect())
            .unwrap_or_default();
        commits.reverse();
        commits.truncate(COMMITS_MAX);
        cs.commits = commits;
    } else if v["ahead_by"].as_u64().unwrap_or(0) > 0 {
        // Only commits touching `paths`: the commits endpoint since the merge base.
        let since = v["merge_base_commit"]["commit"]["committer"]["date"]
            .as_str()
            .context("compare: no merge base date")?;
        let list = commits_since(repo, branch, since, &up.paths, f)?;
        // `since` is inclusive: drop the base itself.
        let base_sha = s(&v["merge_base_commit"], "sha");
        let list: Vec<&Value> = list.iter().filter(|c| s(c, "sha") != base_sha).collect();
        cs.commits_total = list.len() as u64;
        cs.commits = list.into_iter().take(COMMITS_MAX).map(commit_of).collect();
    }
    Ok(())
}

/// No exact pin: commits since `pin_date`, and the files of the compare from the oldest one's parent.
fn since_date(
    up: &Upstream,
    repo: &str,
    date: &str,
    branch: &str,
    f: &dyn Fetcher,
    cs: &mut ChangeSet,
) -> anyhow::Result<()> {
    let list = commits_since(repo, branch, &format!("{date}T00:00:00Z"), &up.paths, f)?;
    cs.commits_total = list.len() as u64;
    cs.commits = list.iter().take(COMMITS_MAX).map(commit_of).collect();
    let parent = list
        .last()
        .and_then(|c| c["parents"][0]["sha"].as_str())
        .map(str::to_owned);
    if let Some(parent) = parent {
        let v = f.github(&format!("repos/{repo}/compare/{parent}...{branch}"))?;
        cs.compare_url = v["html_url"].as_str().map(str::to_owned);
        let files: Vec<&str> = v["files"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x["filename"].as_str()).collect())
            .unwrap_or_default();
        cs.files_touched = top_dirs(files.into_iter().filter(|x| touches(x, &up.paths)));
    }
    Ok(())
}

// --------------------------------------------------------------------- git

fn git(up: &Upstream, f: &dyn Fetcher, cs: &mut ChangeSet) -> anyhow::Result<()> {
    let branch = up.branch();
    if let Some(local) = &up.local {
        let dir = Path::new(local);
        // Offline or no remote: the clone as it is.
        let _ = f.git(&["fetch", "-q", "--tags"], Some(dir));
        let remote = format!("origin/{branch}");
        let head_ref = if f
            .git(&["rev-parse", "--verify", "-q", &remote], Some(dir))
            .is_ok()
        {
            remote
        } else {
            branch.to_owned()
        };
        let mut args: Vec<String> = vec!["log".into(), "--format=%H%x09%cI%x09%s".into()];
        match (&up.pin, &up.pin_date) {
            (Some(pin), _) => args.push(format!("{pin}..{head_ref}")),
            (None, Some(date)) => {
                args.push(format!("--since={date}T00:00:00Z"));
                args.push(head_ref.clone());
            }
            (None, None) => args.push(head_ref.clone()),
        }
        if !up.paths.is_empty() {
            args.push("--".into());
            args.extend(up.paths.iter().cloned());
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let log = f.git(&argv, Some(dir))?;
        let commits: Vec<Commit> = log
            .lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '\t');
                Some(Commit {
                    sha7: p.next()?.chars().take(7).collect(),
                    date: p.next()?.to_owned(),
                    subject: p.next().unwrap_or("").to_owned(),
                })
            })
            .collect();
        cs.commits_total = commits.len() as u64;
        cs.commits = commits.into_iter().take(COMMITS_MAX).collect();
        let tags: Vec<String> = f
            .git(&["tag", "--sort=-creatordate"], Some(dir))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        let newer: &[String] = match up
            .pin
            .as_deref()
            .and_then(|p| tags.iter().position(|t| t == p))
        {
            Some(i) => &tags[..i],
            None => &[],
        };
        cs.releases = newer
            .iter()
            .map(|t| Rel {
                tag: t.clone(),
                name: t.clone(),
                date: String::new(),
                prerelease: false,
                notes: String::new(),
                url: String::new(),
            })
            .collect();
        cs.latest = match newer.first() {
            Some(t) => Some(t.clone()),
            None => f
                .git(&["rev-parse", &head_ref], Some(dir))
                .ok()
                .map(|h| h.trim().chars().take(12).collect()),
        };
        return Ok(());
    }
    // Remote only: tags and the head, no commit list.
    cs.commits_known = false;
    let tags = remote_tags(f, &up.url)?;
    // Names do not sort by date (BusyBox `1_9_2` after `1_37_0`): compare the numbers.
    let pin_tag = up.pin.as_deref().filter(|p| !looks_like_sha(p));
    if let Some(pin) = pin_tag {
        cs.releases = newer_tags(tags.keys().map(String::as_str), pin)
            .into_iter()
            .map(|t| Rel {
                name: t.clone(),
                tag: t,
                date: String::new(),
                prerelease: false,
                notes: String::new(),
                url: String::new(),
            })
            .collect();
    }
    let head = f.git(&["ls-remote", &up.url, branch], None)?;
    let head: Option<String> = head
        .split_whitespace()
        .next()
        .map(|h| h.chars().take(12).collect());
    cs.latest = match (cs.releases.first(), pin_tag) {
        (Some(r), _) => Some(r.tag.clone()),
        // No newer tag: the head, or the pin itself when the head is still the pinned tag's commit.
        (None, Some(p)) => match (tags.get(p), &head) {
            (Some(sha), Some(h)) if same_ref(sha, h) => Some(p.to_owned()),
            _ => head,
        },
        (None, None) => head,
    };
    Ok(())
}

/// The numbers of a tag split on `_`, `.` and `-` after any prefix (`1_37_0`, `v2.1-3`,
/// `DROPBEAR_2025.88`), up to the first part that is not a number; `None` without one.
pub fn tag_numbers(tag: &str) -> Option<Vec<u64>> {
    let core = tag.trim_start_matches(|c: char| !c.is_ascii_digit());
    let nums: Vec<u64> = core
        .split(['_', '.', '-'])
        .map_while(|p| p.parse().ok())
        .collect();
    (!nums.is_empty()).then_some(nums)
}

// ------------------------------------------------------------------ debian

fn debian(up: &Upstream, f: &dyn Fetcher, cs: &mut ChangeSet) -> anyhow::Result<()> {
    let pkg = up
        .package
        .as_deref()
        .context("debian entry without package")?;
    let v = f.http_json(&format!("https://sources.debian.org/api/src/{pkg}/"))?;
    if let Some(e) = v["error"].as_str() {
        anyhow::bail!("sources.debian.org: {e}");
    }
    let all: Vec<(String, Vec<String>)> = v["versions"]
        .as_array()
        .context("sources.debian.org: no versions")?
        .iter()
        .map(|x| {
            let suites = x["suites"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (s(x, "version"), suites)
        })
        .filter(|(v, _)| !v.is_empty())
        .collect();
    let newest = all.first().map(|x| x.0.clone());
    let watched: Vec<&(String, Vec<String>)> = match &up.suite {
        Some(suite) => all.iter().filter(|(_, s)| s.contains(suite)).collect(),
        None => all.iter().collect(),
    };
    let newer: &[&(String, Vec<String>)] = match up
        .pin
        .as_deref()
        .and_then(|p| watched.iter().position(|(v, _)| v == p))
    {
        Some(i) => &watched[..i],
        None if up.pin.is_some() => &watched[..watched.len().min(1)],
        None => &[],
    };
    cs.releases = newer
        .iter()
        .take(10)
        .map(|(ver, suites)| {
            let mut name = format!("{pkg} {ver} ({})", suites.join(", "));
            if newest.as_deref().is_some_and(|n| n != ver)
                && Some(ver) == watched.first().map(|w| &w.0)
            {
                name.push_str(&format!(
                    "; newest overall {}",
                    newest.as_deref().unwrap_or("")
                ));
            }
            Rel {
                tag: ver.clone(),
                name,
                date: String::new(),
                prerelease: false,
                notes: String::new(),
                url: format!("https://sources.debian.org/src/{pkg}/{ver}/"),
            }
        })
        .collect();
    cs.latest = watched.first().map(|w| w.0.clone()).or(newest);
    Ok(())
}

// --------------------------------------------------------------- our side

/// Our recent commits on one `ours` path (the reverse direction, informational).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OursRecent {
    pub repo: String,
    pub path: String,
    pub count: u64,
    /// `<sha> <subject>`, the first 10.
    pub subjects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `git -C repo log --since=<since> -- path` for every `ours` whose repo is a local directory.
pub fn ours_recent(up: &Upstream, f: &dyn Fetcher, since: &str) -> Vec<OursRecent> {
    up.ours
        .iter()
        .filter(|o| Path::new(&o.repo).is_dir())
        .map(|o| {
            let since = format!("--since={since}");
            let path = if o.path.is_empty() {
                "."
            } else {
                o.path.as_str()
            };
            let args = ["log", since.as_str(), "--format=%h%x09%s", "--", path];
            let mut r = OursRecent {
                repo: o.repo.clone(),
                path: o.path.clone(),
                ..OursRecent::default()
            };
            match f.git(&args, Some(Path::new(&o.repo))) {
                Ok(out) => {
                    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
                    r.count = lines.len() as u64;
                    r.subjects = lines
                        .iter()
                        .take(10)
                        .map(|l| l.replacen('\t', " ", 1))
                        .collect();
                }
                Err(e) => r.error = Some(clip(&format!("{e:#}"), 300)),
            }
            r
        })
        .collect()
}
