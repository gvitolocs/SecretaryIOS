//! `changes` against hand-written GitHub / Debian fixtures and a real temporary git repository.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lucina_upstream::fetch::{changes, changes_or_error, ours_recent, Fetcher, SystemFetcher};
use lucina_upstream::registry::{Registry, Upstream};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// GitHub and HTTP answers by exact path; git answers by the joined args. Records what was asked.
#[derive(Default)]
struct Fake {
    github: BTreeMap<String, Value>,
    http: BTreeMap<String, Value>,
    git: BTreeMap<String, String>,
    asked: RefCell<Vec<String>>,
}

impl Fake {
    fn gh(mut self, path: &str, fixture_name: &str) -> Self {
        self.github.insert(path.into(), fixture(fixture_name));
        self
    }
}

impl Fetcher for Fake {
    fn github(&self, path: &str) -> anyhow::Result<Value> {
        self.asked.borrow_mut().push(path.to_owned());
        self.github
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("gh api {path}: exit 1: HTTP 404"))
    }
    fn git(&self, args: &[&str], _cwd: Option<&Path>) -> anyhow::Result<String> {
        let k = args.join(" ");
        self.asked.borrow_mut().push(format!("git {k}"));
        self.git
            .get(&k)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("git: no {k}"))
    }
    fn http_json(&self, url: &str) -> anyhow::Result<Value> {
        self.asked.borrow_mut().push(url.to_owned());
        self.http
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("curl {url}: 404"))
    }
}

fn entry(extra: &str) -> Upstream {
    let text = format!(
        r#"[[upstream]]
id = "x"
name = "X"
url = "https://github.com/a/x"
ours = [{{ repo = "/r", path = "k" }}]
licence_upstream = "MIT"
licence_ours = "MIT"
relation = "fork"
confidence = "confirmed"
{extra}
"#
    );
    Registry::parse(&text).unwrap().upstream.remove(0)
}

#[test]
fn releases_after_a_tag_pin_and_the_compare() {
    let up = entry("source = \"github\"\nrepo = \"a/x\"\npin = \"v0.19.0\"");
    let f = Fake::default()
        .gh("repos/a/x/releases?per_page=30", "releases.json")
        .gh("repos/a/x/compare/v0.19.0...main", "compare.json");
    let cs = changes(&up, &f, None).unwrap();
    let tags: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(
        tags,
        ["v0.20.1", "v0.20.0-beta.1"],
        "drafts skipped, pre-releases kept"
    );
    assert!(cs.releases[1].prerelease);
    assert_eq!(cs.latest.as_deref(), Some("v0.20.1"));
    assert_eq!(cs.commits_total, 3);
    let subjects: Vec<&str> = cs.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(
        subjects,
        ["fix(mm): frame leak", "docs: typo", "feat(vfs): overlayfs"],
        "newest first, subject line only"
    );
    assert_eq!(cs.commits[0].sha7, "3333333");
    assert_eq!(
        cs.files_touched,
        ["kernel/src (2)", ". (1)", "ostd/src (1)"]
    );
    assert_eq!(
        cs.compare_url.as_deref(),
        Some("https://github.com/a/x/compare/v0.19.0...main")
    );
    assert!(!cs.is_empty());
}

#[test]
fn a_commit_pin_dates_the_releases() {
    let sha = "c4b7ed575b7b32773475b194d2ad41fb6d03765b";
    let up = entry(&format!(
        "source = \"github\"\nrepo = \"a/x\"\npin = \"{sha}\""
    ));
    let mut f = Fake::default()
        .gh("repos/a/x/releases?per_page=30", "releases.json")
        .gh(&format!("repos/a/x/compare/{sha}...main"), "compare.json");
    f.github.insert(
        format!("repos/a/x/commits/{sha}"),
        serde_json::json!({"sha": sha, "commit": {"committer": {"date": "2026-10-12T21:18:07Z"}}}),
    );
    let cs = changes(&up, &f, None).unwrap();
    let tags: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(
        tags,
        ["v0.20.1", "v0.20.0-beta.1", "v0.19.0"],
        "published after the pinned commit"
    );

    // The sha is a release's commit: that release is the pin, not news.
    f.github.insert(
        "repos/a/x/tags?per_page=100".into(),
        serde_json::json!([{"name": "v0.20.1", "commit": {"sha": "ffff"}}, {"name": "v0.19.0", "commit": {"sha": sha}}]),
    );
    let cs = changes(&up, &f, None).unwrap();
    let tags: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["v0.20.1", "v0.20.0-beta.1"]);
}

#[test]
fn releases_by_pin_date_and_commits_since() {
    let up = entry("source = \"github\"\nrepo = \"a/x\"\npin_date = \"2026-10-20\"");
    let f = Fake::default()
        .gh("repos/a/x/releases?per_page=30", "releases.json")
        .gh(
            "repos/a/x/commits?sha=main&since=2026-10-20T00:00:00Z&per_page=100",
            "commits_since.json",
        )
        .gh(
            "repos/a/x/compare/0ld0000000000000000000000000000000000000...main",
            "compare_since.json",
        );
    let cs = changes(&up, &f, None).unwrap();
    assert_eq!(cs.releases.len(), 2, "v0.20.1 and the beta of 2026-10-20");
    assert_eq!(cs.pin.as_deref(), Some("2026-10-20"));
    assert_eq!(cs.commits_total, 2);
    assert_eq!(cs.commits[0].subject, "fix(server): retry a host");
    assert_eq!(cs.files_touched[0], "packages/server (2)");
}

#[test]
fn paths_filter_commits_and_files() {
    let up = entry("source = \"github\"\nrepo = \"a/x\"\npin = \"v0.19.0\"\ntrack = \"commits\"\npaths = [\"ostd\"]");
    let mut f = Fake::default()
        .gh("repos/a/x/compare/v0.19.0...main", "compare.json")
        .gh(
            "repos/a/x/commits?sha=main&since=2026-10-12T21:18:07Z&per_page=100&path=ostd",
            "commits_ostd.json",
        );
    f.github.insert(
        "repos/a/x/commits/main".into(),
        serde_json::json!({"sha": "3333333ccccccccccccccccccccccccccccccccc"}),
    );
    let cs = changes(&up, &f, None).unwrap();
    assert_eq!(
        cs.commits_total, 1,
        "only the ostd commit; the merge base is not new"
    );
    assert_eq!(cs.commits[0].subject, "fix(mm): frame leak");
    assert_eq!(cs.files_touched, ["ostd/src (1)"]);
    assert!(cs.releases.is_empty());
    assert_eq!(
        cs.latest.as_deref(),
        Some("3333333ccccc"),
        "commits track: the head"
    );
}

#[test]
fn tags_track() {
    let up =
        entry("source = \"github\"\nrepo = \"a/x\"\ntrack = \"tags\"\npin = \"DROPBEAR_2025.88\"");
    // Listed as the tags API does (not newest first), with other series in the same repo.
    let mut f = Fake::default();
    f.git.insert(
        "ls-remote --tags https://github.com/a/x.git".into(),
        "s1\trefs/tags/maemo-0.52-2\n\
         s2\trefs/tags/ltm-0.30-orig\n\
         s3\trefs/tags/libtommath-9.40\n\
         s4\trefs/tags/DROPBEAR_2025.87\n\
         s5\trefs/tags/DROPBEAR_2026.90\n\
         s6\trefs/tags/DROPBEAR_2025.88\n\
         s7\trefs/tags/DROPBEAR_2026.89\n"
            .into(),
    );
    f.github.insert(
        "repos/a/x/compare/DROPBEAR_2025.88...main".into(),
        serde_json::json!({"html_url": "u", "ahead_by": 0, "commits": [], "files": []}),
    );
    let cs = changes(&up, &f, None).unwrap();
    let tags: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["DROPBEAR_2026.90", "DROPBEAR_2026.89"]);
    assert_eq!(cs.latest.as_deref(), Some("DROPBEAR_2026.90"));

    let newest =
        entry("source = \"github\"\nrepo = \"a/x\"\ntrack = \"tags\"\npin = \"DROPBEAR_2026.90\"");
    let mut f2 = Fake {
        git: f.git.clone(),
        ..Fake::default()
    };
    f2.github.insert(
        "repos/a/x/compare/DROPBEAR_2026.90...main".into(),
        serde_json::json!({"html_url": "u", "ahead_by": 0, "commits": [], "files": []}),
    );
    let cs = changes(&newest, &f2, None).unwrap();
    assert!(cs.releases.is_empty());
    assert_eq!(cs.latest.as_deref(), Some("DROPBEAR_2026.90"));
    assert!(cs.is_empty());
}

#[test]
fn debian_versions() {
    let up =
        entry("source = \"debian\"\npackage = \"linux\"\nsuite = \"trixie\"\npin = \"6.12.107-1\"");
    let mut f = Fake::default();
    f.http.insert(
        "https://sources.debian.org/api/src/linux/".into(),
        fixture("debian.json"),
    );
    let cs = changes(&up, &f, None).unwrap();
    assert_eq!(
        cs.latest.as_deref(),
        Some("6.12.110-1"),
        "newest in the suite"
    );
    assert_eq!(cs.releases.len(), 1);
    assert!(
        cs.releases[0].name.contains("newest overall 6.17.6-1"),
        "{}",
        cs.releases[0].name
    );
    let same =
        entry("source = \"debian\"\npackage = \"linux\"\nsuite = \"trixie\"\npin = \"6.12.110-1\"");
    let cs = changes(&same, &f, None).unwrap();
    assert!(cs.releases.is_empty());
    assert!(cs.is_empty());
}

#[test]
fn frozen_and_manual_are_skipped_without_fetching() {
    let f = Fake::default();
    let frozen = changes(&entry("source = \"frozen\""), &f, None).unwrap();
    assert!(frozen.skipped.is_some());
    let manual = changes(
        &entry("source = \"manual\"\ncheck_hint = \"look at the Play store\""),
        &f,
        None,
    )
    .unwrap();
    assert_eq!(
        manual.skipped.as_deref(),
        Some("manual: look at the Play store")
    );
    assert!(manual.is_empty());
    assert!(f.asked.borrow().is_empty());
}

#[test]
fn a_fetch_error_is_recorded() {
    let up = entry("source = \"github\"\nrepo = \"a/x\"\npin = \"v1\"");
    let cs = changes_or_error(&up, &Fake::default(), None);
    assert!(cs.error.as_deref().unwrap().contains("404"));
    assert!(cs.is_empty());
}

#[test]
fn git_remote_tags_and_head() {
    let up = entry("source = \"git\"\nbranch = \"master\"\npin = \"1_37_0\"");
    let tags = "a1\trefs/tags/1_36_1\n\
                a2\trefs/tags/1_37_0\n\
                c2\trefs/tags/1_37_0^{}\n\
                a3\trefs/tags/1_38_0\n\
                a5\trefs/tags/1_38_1\n\
                a4\trefs/tags/1_9_2\n\
                a6\trefs/tags/stable\n";
    let mut f = Fake::default();
    f.git.insert(
        "ls-remote --tags https://github.com/a/x".into(),
        tags.into(),
    );
    f.git.insert(
        "ls-remote https://github.com/a/x master".into(),
        "deadbeefdeadbeef\trefs/heads/master\n".into(),
    );
    let cs = changes(&up, &f, None).unwrap();
    let newer: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(
        newer,
        ["1_38_1", "1_38_0"],
        "1_9_2 is older, `stable` is not a version"
    );
    assert_eq!(cs.latest.as_deref(), Some("1_38_1"), "the highest");
    assert!(!cs.is_empty());

    // No newer tag: the head decides. The pinned tag is annotated: its commit is the `^{}` line.
    let newest = entry("source = \"git\"\nbranch = \"master\"\npin = \"1_38_1\"");
    let cs = changes(&newest, &f, None).unwrap();
    assert!(cs.releases.is_empty());
    assert_eq!(cs.latest.as_deref(), Some("deadbeefdead"));
    assert!(
        !cs.is_empty(),
        "head ≠ the pinned tag: new commits, count unknown"
    );
    assert!(!cs.commits_known);
    f.git.insert(
        "ls-remote https://github.com/a/x master".into(),
        "c2c2c2c2c2c2c2c2\trefs/heads/master\n".into(),
    );
    let mut on_pin = f.git.clone();
    on_pin.insert(
        "ls-remote --tags https://github.com/a/x".into(),
        "a2\trefs/tags/1_37_0\nc2c2c2c2c2c2c2c2c2c2\trefs/tags/1_37_0^{}\n".into(),
    );
    f.git = on_pin;
    let cs = changes(&up, &f, None).unwrap();
    assert_eq!(
        cs.latest.as_deref(),
        Some("1_37_0"),
        "head is the pinned tag's commit"
    );
    assert!(cs.is_empty());

    let sha_pin = entry("source = \"git\"\nbranch = \"master\"\npin = \"c2c2c2c2\"");
    assert!(
        changes(&sha_pin, &f, None).unwrap().is_empty(),
        "short sha = head"
    );
    let old = entry("source = \"git\"\nbranch = \"master\"\npin = \"0123456789ab\"");
    let cs = changes(&old, &f, None).unwrap();
    assert!(!cs.is_empty(), "head moved, commits unknown");
    assert_eq!(cs.commits_total, 0);
}

#[test]
fn tag_numbers_compare_by_number() {
    use lucina_upstream::fetch::tag_numbers;
    assert_eq!(tag_numbers("1_37_0"), Some(vec![1, 37, 0]));
    assert!(tag_numbers("1_9_2") < tag_numbers("1_37_0"));
    assert_eq!(tag_numbers("DROPBEAR_2025.88"), Some(vec![2025, 88]));
    assert_eq!(tag_numbers("v2.1-3"), Some(vec![2, 1, 3]));
    assert_eq!(tag_numbers("stable"), None);
}

#[test]
fn releases_by_publication_not_tag_name() {
    let up = entry("source = \"github\"\nrepo = \"a/x\"\ntrack = \"commits\"\npin = \"v0.22.5\"");
    let mut up = up;
    up.track = Some(lucina_upstream::registry::Track::Releases);
    let mut f = Fake::default();
    f.github.insert(
        "repos/a/x/releases?per_page=30".into(),
        serde_json::json!([
            {"tag_name": "v2026.9.24", "draft": false, "prerelease": false, "published_at": "2026-09-24T10:00:00Z", "body": ""},
            {"tag_name": "v0.23.0", "draft": false, "prerelease": false, "published_at": "2026-10-28T10:00:00Z", "body": ""},
            {"tag_name": "v0.22.5", "draft": false, "prerelease": false, "published_at": "2026-10-20T10:00:00Z", "body": ""}
        ]),
    );
    f.github.insert(
        "repos/a/x/compare/v0.22.5...main".into(),
        serde_json::json!({"html_url": "u", "ahead_by": 0, "commits": [], "files": []}),
    );
    let cs = changes(&up, &f, None).unwrap();
    let tags: Vec<&str> = cs.releases.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(
        tags,
        ["v0.23.0"],
        "the odd tag was published before the pin"
    );
    assert_eq!(cs.latest.as_deref(), Some("v0.23.0"));
}

fn sh(dir: &Path, args: &[&str]) -> String {
    sh_at(dir, args, "2026-10-01T10:00:00Z")
}

fn sh_at(dir: &Path, args: &[&str], date: &str) -> String {
    let out = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn git_local_log_since_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    sh(d, &["init", "-q", "-b", "main"]);
    let commit = |file: &str, msg: &str, date: &str| {
        std::fs::create_dir_all(d.join(file).parent().unwrap()).unwrap();
        std::fs::write(d.join(file), msg).unwrap();
        sh(d, &["add", "."]);
        sh_at(d, &["commit", "-q", "-m", msg], date);
    };
    commit("src/a.rs", "first", "2026-10-01T10:00:00Z");
    sh(d, &["tag", "v1.0.0"]);
    let pin = sh(d, &["rev-parse", "HEAD"]);
    commit("src/b.rs", "fix: second", "2026-10-02T10:00:00Z");
    commit("docs/c.md", "docs: third", "2026-10-03T10:00:00Z");
    sh(d, &["tag", "v1.1.0"]);

    let up = entry(&format!(
        "source = \"git\"\nlocal = \"{}\"\npin = \"{pin}\"",
        d.display()
    ));
    let cs = changes(&up, &SystemFetcher, None).unwrap();
    assert_eq!(cs.commits_total, 2);
    assert_eq!(cs.commits[0].subject, "docs: third");
    assert_eq!(
        cs.latest.as_deref().map(|l| l.len()),
        Some(12),
        "a sha pin: no tag after it is known"
    );

    let up = entry(&format!(
        "source = \"git\"\nlocal = \"{}\"\npin = \"v1.0.0\"\npaths = [\"src\"]",
        d.display()
    ));
    let cs = changes(&up, &SystemFetcher, None).unwrap();
    assert_eq!(cs.commits_total, 1, "paths narrow it");
    assert_eq!(cs.commits[0].subject, "fix: second");
    assert_eq!(cs.latest.as_deref(), Some("v1.1.0"));
    assert_eq!(cs.releases[0].tag, "v1.1.0");

    // Our side: recent commits on a path.
    let mut mine = entry("source = \"frozen\"");
    mine.ours[0].repo = d.display().to_string();
    mine.ours[0].path = "src".into();
    let recent = ours_recent(&mine, &SystemFetcher, "1 year ago");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].count, 2);
    assert!(recent[0].subjects[0].ends_with(" fix: second"));
}
