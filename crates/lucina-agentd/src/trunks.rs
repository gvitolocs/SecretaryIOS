//! Which branch is a repository's trunk: the one function every trunk question goes through
//! ([`resolve`]), and the table that overrides it for a repo whose remote does not carry its
//! trunk.
//!
//! The order is: [`TRUNKS`], then `trunk` in `.lucina/pipeline.toml` on origin's default branch,
//! then origin's HEAD, then `main`, then `master`. Nothing else sets a trunk: no environment
//! variable, no config file, no API call, no git config (`lucina.trunk` is not read).
//!
//! A repo lands with no remote ([`Trunk::remote`] is `None`) in two cases only: the table marks
//! its trunk `local`, or the repo has no `origin` at all. Then nothing is fetched or pushed, and
//! a landing moves `refs/heads/<trunk>` in this checkout. Every other repo lands through its
//! remote: a trunk the remote does not carry is a refused merge, never a landing on a local
//! branch of the same name. The order is decided on the local refs, in the state the usual fetch
//! left: it never runs `git ls-remote` and never fetches to decide. A repo's push URL is never
//! read or changed.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::pipeline::repo::{Rules, FILE as PIPELINE_FILE};

/// One repo's row of [`TRUNKS`].
pub struct TrunkEntry {
    /// The main checkout's directory name.
    pub repo: &'static str,
    /// Its trunk.
    pub branch: &'static str,
    /// Whether the trunk lives only on this machine.
    ///
    /// `local: true` lands merges on the local branch: no remote, nothing fetched, nothing
    /// pushed, whatever the remote has. `local: false` only names the branch: the repo is
    /// otherwise an ordinary remote repo, its merges go through its remote, and a trunk the
    /// remote does not carry is a refused merge. A repo whose trunk its remote can be pushed to
    /// is `local: false`: `true` is the marker for a fork whose trunk never reaches its origin.
    pub local: bool,
}

/// The repos whose trunk this table decides, whatever their remote or their pipeline file says.
///
/// **This table decides where merges land.** An entry moves a repo's merges to another branch,
/// and it changes only through a SecretaryIOS merge and an agentd deploy: that is the only way
/// it differs from one machine to another. Read [`TrunkEntry::local`] before adding a row: it is
/// what says whether the branch is a local one or a remote repo's, and a repo with a pushable
/// trunk set to `local: true` would stop pushing its merges without any error.
///
/// `LucinaNet-servo` is the LucinaNet fork of servo. Its `origin` is upstream servo (a real
/// fetch URL, a push URL of `no-push://…`) and origin's HEAD is upstream's `main`, while the
/// fork's work is on the local branch `lucinanet`, which has no branch on origin.
const TRUNKS: &[TrunkEntry] = &[TrunkEntry {
    repo: "LucinaNet-servo",
    branch: "lucinanet",
    local: true,
}];

/// The table's row for a repo whose checkout is named `name`.
fn table_entry(name: &str) -> Option<&'static TrunkEntry> {
    TRUNKS.iter().find(|e| e.repo == name)
}

/// A repo's trunk, and the remote it lives on when a remote has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trunk {
    /// The repo's main working tree.
    pub root: PathBuf,
    /// The main checkout's directory name: how the table, the ledger and a job know the repo.
    pub name: String,
    /// The branch workspaces start from and merges land on.
    pub branch: String,
    /// The remote merges land through: `None` when the repo has no `origin`, or when the table
    /// marks its trunk local and the branch lives only in this checkout.
    pub remote: Option<String>,
}

impl Trunk {
    /// The ref worktrees branch off and the merged/ahead check measures against: `origin/main`
    /// when the trunk is on a remote, the local branch when it is not.
    pub fn base_ref(&self) -> String {
        match &self.remote {
            Some(r) => format!("{r}/{}", self.branch),
            None => self.branch.clone(),
        }
    }
}

/// The repo around `dir` (any path in it, a worktree of it too): its trunk and its remote.
/// `None` when `dir` is not inside a git repository.
pub fn resolve(dir: &Path) -> Option<Trunk> {
    let common = PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    // The main working tree: the parent of `.git`, which for a linked worktree is the main
    // checkout. A directory that is itself a git directory (a bare repo) is its own root.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common.clone())
    } else {
        common.clone()
    };
    let name = root
        .file_name()?
        .to_string_lossy()
        .trim_end_matches(".git")
        .to_string();
    let remote = git(&root, &["remote", "get-url", "origin"]).map(|_| "origin".to_string());
    let entry = table_entry(&name);
    let branch = match entry {
        Some(e) => e.branch.to_string(),
        None => {
            let from_origin = remote.as_ref().and_then(|r| {
                git(
                    &root,
                    &[
                        "symbolic-ref",
                        "--quiet",
                        "--short",
                        &format!("refs/remotes/{r}/HEAD"),
                    ],
                )
                .map(|s| s.strip_prefix(&format!("{r}/")).unwrap_or(&s).to_string())
            });
            let default = from_origin
                .or_else(|| local_branch(&root, "main"))
                .or_else(|| local_branch(&root, "master"))
                .unwrap_or_else(|| "main".to_string());
            let shown = |t: &str| match &remote {
                Some(r) => format!("{r}/{t}"),
                None => t.to_string(),
            };
            git(
                &root,
                &["show", &format!("{}:{PIPELINE_FILE}", shown(&default))],
            )
            .and_then(|t| Rules::parse(&t).ok())
            .and_then(|r| r.trunk)
            .unwrap_or(default)
        }
    };
    // Only a `local: true` entry takes a repo off its remote. Whether the remote has the trunk
    // is not asked here: the merge that follows reads the remote's own refs and refuses if the
    // trunk is not among them.
    let remote = match entry {
        Some(e) if e.local => None,
        _ => remote,
    };
    Some(Trunk {
        root,
        name,
        branch,
        remote,
    })
}

/// `git <args>` in `dir`, its trimmed stdout when it succeeds. Local reads only: none of what
/// is asked here touches the network.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn ref_exists(dir: &Path, r: &str) -> bool {
    git(dir, &["show-ref", "--verify", "--quiet", r]).is_some()
}

fn local_branch(dir: &Path, name: &str) -> Option<String> {
    ref_exists(dir, &format!("refs/heads/{name}")).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn commit(dir: &Path, msg: &str) -> String {
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", msg]);
        git_ok(dir, &["rev-parse", "HEAD"])
    }

    /// A bare `origin` with `main` as its HEAD and a clone whose checkout is named `name`:
    /// `(origin, the clone)`.
    fn repo_with_main(root: &Path, name: &str) -> (PathBuf, PathBuf) {
        let origin = root.join(format!("{name}.git"));
        let repo = root.join(name);
        git_ok(
            root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.display().to_string(),
            ],
        );
        git_ok(
            root,
            &[
                "clone",
                "-q",
                &origin.display().to_string(),
                &repo.display().to_string(),
            ],
        );
        git_ok(&repo, &["config", "user.email", "t@t"]);
        git_ok(&repo, &["config", "user.name", "t"]);
        write(&repo, "a.txt", "1\n");
        commit(&repo, "a");
        git_ok(&repo, &["push", "-q", "origin", "main"]);
        git_ok(&repo, &["fetch", "-q", "origin"]);
        (origin, repo)
    }

    /// The LucinaNet-servo shape: a local trunk with no branch on origin, and a push URL that
    /// fails. `rules` is the pipeline file committed on `main` before that URL makes pushing
    /// impossible, so a test can see what the table overrules.
    fn servo(root: &Path, rules: Option<&str>) -> (PathBuf, PathBuf) {
        let (origin, repo) = repo_with_main(root, "LucinaNet-servo");
        if let Some(r) = rules {
            write(&repo, PIPELINE_FILE, r);
            commit(&repo, "rules");
            git_ok(&repo, &["push", "-q", "origin", "main"]);
            git_ok(&repo, &["fetch", "-q", "origin"]);
        }
        git_ok(&repo, &["config", "remote.origin.pushurl", "no-push://x"]);
        git_ok(&repo, &["branch", "lucinanet"]);
        (origin, repo)
    }

    fn servo_at_worktree(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let (origin, repo) = servo(root, None);
        let wt = root.join("wt/lucinanet-work");
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                &wt.display().to_string(),
                "lucinanet",
            ],
        );
        (origin, repo, wt)
    }

    /// A repo whose pipeline file names another trunk, but no entry in the table: what the
    /// default order gives, and what the table is measured against.
    #[test]
    fn without_a_table_entry_the_trunk_is_the_pipeline_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo) = repo_with_main(root, "Plain");
        write(
            &repo,
            PIPELINE_FILE,
            "trunk = \"release\"\n[merge]\ncargo = \"off\"\n",
        );
        commit(&repo, "rules");
        git_ok(&repo, &["push", "-q", "origin", "main"]);
        git_ok(&repo, &["fetch", "-q", "origin"]);
        git_ok(&repo, &["branch", "release", "main"]);
        git_ok(
            &repo,
            &["update-ref", "refs/remotes/origin/release", "main"],
        );
        let t = resolve(&repo).unwrap();
        assert_eq!(t.name, "Plain");
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("release", Some("origin"))
        );
        assert_eq!(t.base_ref(), "origin/release");
        // A trunk the remote does not carry keeps its remote: the merge refuses, it does not
        // fall back to a local branch of the same name.
        git_ok(&repo, &["update-ref", "-d", "refs/remotes/origin/release"]);
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("release", Some("origin"))
        );
        assert_eq!(t.base_ref(), "origin/release");
        // A trunk the file no longer names: the repo is back to what it was.
        git_ok(&repo, &["branch", "-D", "release"]);
        write(&repo, PIPELINE_FILE, "[merge]\ncargo = \"off\"\n");
        commit(&repo, "no trunk");
        git_ok(&repo, &["push", "-q", "origin", "main"]);
        git_ok(&repo, &["fetch", "-q", "origin"]);
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("main", Some("origin"))
        );
        assert_eq!(t.base_ref(), "origin/main");
    }

    #[test]
    fn without_a_table_entry_or_a_pipeline_file_the_trunk_is_origins_head() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo) = repo_with_main(root, "Plain");
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("main", Some("origin"))
        );
        assert_eq!(t.base_ref(), "origin/main");
        // Origin's HEAD as the fetch state records it: another default branch, another trunk.
        git_ok(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/heads/legacy",
            ],
        );
        git_ok(&repo, &["branch", "legacy", "main"]);
        git_ok(&repo, &["update-ref", "refs/remotes/origin/legacy", "main"]);
        assert_eq!(resolve(&repo).unwrap().branch, "legacy");
        git_ok(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/heads/main",
            ],
        );
        git_ok(&repo, &["branch", "-D", "legacy"]);
        git_ok(&repo, &["update-ref", "-d", "refs/remotes/origin/legacy"]);
        assert_eq!(resolve(&repo).unwrap().branch, "main");
    }

    #[test]
    fn without_a_remote_the_trunk_is_the_local_default_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("Solo");
        git_ok(
            root,
            &["init", "-q", "-b", "main", &repo.display().to_string()],
        );
        git_ok(&repo, &["config", "user.email", "t@t"]);
        git_ok(&repo, &["config", "user.name", "t"]);
        write(&repo, "a.txt", "1\n");
        commit(&repo, "a");
        let t = resolve(&repo).unwrap();
        assert_eq!((t.branch.as_str(), t.remote.as_deref()), ("main", None));
        assert_eq!(t.base_ref(), "main");
    }

    #[test]
    fn a_table_entry_is_the_trunk_even_against_the_pipeline_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo) = servo(root, Some("trunk = \"other\"\n"));
        git_ok(&repo, &["branch", "other", "main"]);
        git_ok(&repo, &["update-ref", "refs/remotes/origin/other", "main"]);
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.name.as_str(), t.branch.as_str()),
            ("LucinaNet-servo", "lucinanet")
        );
        assert_eq!(t.base_ref(), "lucinanet");
        // The very same repo under another name follows the file: the entry is what decides.
        std::fs::rename(&repo, root.join("Plain")).unwrap();
        let t = resolve(&root.join("Plain")).unwrap();
        assert_eq!(
            (t.name.as_str(), t.branch.as_str(), t.remote.as_deref()),
            ("Plain", "other", Some("origin"))
        );
    }

    /// A `local: true` entry is the only thing that makes a repo with an origin land locally.
    #[test]
    fn a_local_table_trunk_has_no_remote_even_when_origin_carries_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (origin, repo) = servo(root, None);
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("lucinanet", None)
        );
        assert_eq!(t.base_ref(), "lucinanet");
        assert_eq!(
            git_ok(&repo, &["config", "--get", "remote.origin.pushurl"]),
            "no-push://x",
            "the push URL is left as it is"
        );
        // And still local once origin does carry the trunk. Pushed by URL: the remote's own
        // push URL is the one that refuses.
        git_ok(
            &repo,
            &[
                "push",
                "-q",
                &origin.display().to_string(),
                "refs/heads/lucinanet",
            ],
        );
        git_ok(&repo, &["fetch", "-q", "origin"]);
        assert!(
            ref_exists(&repo, "refs/remotes/origin/lucinanet"),
            "origin carries the trunk now"
        );
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("lucinanet", None)
        );
        assert_eq!(t.base_ref(), "lucinanet");
    }

    /// A repo the table does not mark local keeps its remote even when nothing on it has been
    /// seen for the trunk: the missing ref is the merge's problem, not the resolver's.
    #[test]
    fn a_trunk_the_remote_has_no_ref_for_keeps_the_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo) = repo_with_main(root, "Plain");
        // origin's HEAD names a branch the usual fetch left no remote-tracking ref for.
        git_ok(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/heads/trunk2",
            ],
        );
        git_ok(&repo, &["branch", "trunk2", "main"]);
        let t = resolve(&repo).unwrap();
        assert_eq!(
            (t.branch.as_str(), t.remote.as_deref()),
            ("trunk2", Some("origin"))
        );
        assert_eq!(t.base_ref(), "origin/trunk2");
        assert!(
            !ref_exists(&repo, "refs/remotes/origin/trunk2"),
            "no remote-tracking ref for the trunk"
        );
    }

    #[test]
    fn the_table_names_the_checkout_also_from_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo, wt) = servo_at_worktree(root);
        let t = resolve(&wt).unwrap();
        assert_eq!(
            (t.name.as_str(), t.branch.as_str(), t.remote.as_deref()),
            ("LucinaNet-servo", "lucinanet", None)
        );
        assert_eq!(t.root, repo.canonicalize().unwrap());
    }

    #[test]
    fn a_git_config_trunk_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = &std::fs::canonicalize(tmp.path()).unwrap();
        let (_, repo) = servo(root, None);
        git_ok(&repo, &["config", "lucina.trunk", "other"]);
        assert_eq!(resolve(&repo).unwrap().branch, "lucinanet");
        git_ok(&repo, &["config", "lucina.trunk", "main"]);
        assert_eq!(resolve(&repo).unwrap().branch, "lucinanet");
        let (_, plain) = repo_with_main(root, "Plain");
        git_ok(&plain, &["config", "lucina.trunk", "somewhere"]);
        assert_eq!(resolve(&plain).unwrap().branch, "main");
    }

    #[test]
    fn only_a_git_repository_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve(&tmp.path().join("nowhere")).is_none());
    }
}
