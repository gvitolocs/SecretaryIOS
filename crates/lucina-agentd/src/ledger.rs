//! The commit attribution ledger: which model wrote a commit, recorded when the commit is still
//! amendable, in an append-only hash-chained JSON Lines file in the daemon's state directory
//! (`commit-ledger.jsonl`, mode 0600). The proof-roles gate (`lucina-ci roles` in LucinaOS) reads it
//! through `LAGENT_COMMIT_LEDGER` instead of trusting a hand-typed `Co-Authored-By` trailer.
//!
//! One line per attested commit:
//! `{"v":1,"commit","repo","workspace","model","kind","lane","session","declared_by","time","prev"}`.
//! `prev` is the lowercase hex SHA-256 of the previous line's bytes (without its newline), 64 zeros
//! for the first line. Lines are appended under an exclusive `flock` on the file itself, after the
//! whole chain was checked. A commit is attested once per repo.
//!
//! Threat model: honest mistakes and forgetfulness, not a malicious agent (every agent is the same
//! Unix user). The chain detects an edited or removed line in the middle; it cannot detect lines
//! cut from the end.
//!
//! [`attest`] refuses (and writes nothing for any commit of the call) when a commit has no
//! `Co-Authored-By` trailer, its trailer names another model than the declared one, or the model is
//! not in the table. The table is `[attribution.models]` of `.lucina/pipeline.toml` **on the trunk**
//! (never the branch), else [`default_models`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pipeline::repo::{Rules, FILE as PIPELINE_FILE};

/// The ledger's file name inside the state directory.
pub const FILE: &str = "commit-ledger.jsonl";

/// `prev` of the first line.
pub const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub const KINDS: [&str; 3] = ["lane", "subagent", "lead"];

/// One ledger line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u32,
    pub commit: String,
    pub repo: String,
    pub workspace: String,
    pub model: String,
    pub kind: String,
    pub lane: String,
    pub session: String,
    pub declared_by: String,
    pub time: String,
    pub prev: String,
}

/// The built-in model-id to trailer-name table (used when the trunk has no `[attribution.models]`).
pub fn default_models() -> BTreeMap<String, String> {
    [
        ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        ("claude-opus-5-5", "Claude Opus 5.5"),
        ("claude-fable-5-2", "Claude Fable 5.2"),
        ("qwen3.8-27b", "Qwen 3.8 27B"),
        ("qoder", "Qoder"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_full_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The ledger file.
#[derive(Debug, Clone)]
pub struct Ledger {
    pub path: PathBuf,
}

/// Why the chain is not valid: the 1-based line and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub line: usize,
    pub reason: String,
}

/// Check the chain of a ledger's text; returns its entries.
pub fn verify_text(text: &str) -> Result<Vec<Entry>, Broken> {
    let mut entries = Vec::new();
    let mut prev = ZERO.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        let n = text.lines().count();
        return Err(Broken {
            line: n,
            reason: "the last line has no newline (a partial write)".into(),
        });
    }
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let e: Entry = serde_json::from_str(line).map_err(|e| Broken {
            line: n,
            reason: format!("not a ledger entry: {e}"),
        })?;
        if e.v != 1 {
            return Err(Broken {
                line: n,
                reason: format!("unknown version {}", e.v),
            });
        }
        if !is_full_hex(&e.commit) {
            return Err(Broken {
                line: n,
                reason: format!("commit {:?} is not 40 lowercase hex", e.commit),
            });
        }
        if e.prev != prev {
            // The chain links each line to the one before it, so a changed line shows up in
            // the next line's `prev`: blame the earlier line, which is the one that changed.
            return Err(if n == 1 {
                Broken {
                    line: 1,
                    reason: "the first line's prev is not 64 zeros (the line was changed)".into(),
                }
            } else {
                Broken {
                    line: n - 1,
                    reason: format!(
                        "line {n}'s prev no longer matches this line's hash: line {} (or line {n}) was changed or removed",
                        n - 1
                    ),
                }
            });
        }
        prev = sha256_hex(line.as_bytes());
        entries.push(e);
    }
    Ok(entries)
}

/// What [`Ledger::append`] did with one draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Written(Entry),
    /// The commit was already attested with the same model; nothing written.
    Already(Entry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    /// The call is refused; every reason, nothing written.
    Refused(Vec<String>),
    /// The ledger or git could not be used.
    Other(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Refused(v) => write!(f, "{}", v.join("\n")),
            LedgerError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl Ledger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Ledger { path: path.into() }
    }

    /// `<state dir>/commit-ledger.jsonl`.
    pub fn in_state_dir(dir: &Path) -> Self {
        Ledger::new(dir.join(FILE))
    }

    /// The entries, chain checked; a missing file is an empty ledger.
    pub fn read(&self) -> Result<Vec<Entry>, Broken> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => verify_text(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Broken {
                line: 0,
                reason: format!("{}: {e}", self.path.display()),
            }),
        }
    }

    /// Every parseable entry, chain not checked (for the merge log's count).
    pub fn read_lenient(&self) -> Vec<Entry> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Append drafts (`prev` and `time` are filled in here) under an exclusive lock, after
    /// checking the chain. A draft whose repo and commit are already attested with the same model
    /// is a no-op; with another model the whole call is refused and nothing is written.
    pub fn append(&self, drafts: Vec<Entry>) -> Result<Vec<Appended>, LedgerError> {
        let io = |e: std::io::Error| LedgerError::Other(format!("{}: {e}", self.path.display()));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(io)?;
        f.lock().map_err(io)?;
        let mut text = String::new();
        f.read_to_string(&mut text).map_err(io)?;
        let mut entries = verify_text(&text).map_err(|b| {
            LedgerError::Other(format!(
                "{} is broken at line {}: {}; run `lagent ledger verify`",
                self.path.display(),
                b.line,
                b.reason
            ))
        })?;
        let mut prev = match text.lines().last() {
            Some(l) => sha256_hex(l.as_bytes()),
            None => ZERO.to_string(),
        };
        let mut out = Vec::new();
        let mut refusals = Vec::new();
        let mut add = String::new();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        for mut d in drafts {
            if let Some(old) = entries
                .iter()
                .find(|e| e.repo == d.repo && e.commit == d.commit)
            {
                if old.model == d.model {
                    out.push(Appended::Already(old.clone()));
                } else {
                    refusals.push(format!(
                        "commit {} of {} is already attested as {} (by {}, {}); it cannot also be attested as {}",
                        &d.commit[..12], d.repo, old.model, old.session, old.time, d.model
                    ));
                }
                continue;
            }
            d.v = 1;
            d.time = now.clone();
            d.prev = prev.clone();
            let line = serde_json::to_string(&d)
                .map_err(|e| LedgerError::Other(format!("serialize: {e}")))?;
            prev = sha256_hex(line.as_bytes());
            add.push_str(&line);
            add.push('\n');
            entries.push(d.clone());
            out.push(Appended::Written(d));
        }
        if !refusals.is_empty() {
            return Err(LedgerError::Refused(refusals));
        }
        if !add.is_empty() {
            f.write_all(add.as_bytes()).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        Ok(out)
    }
}

// ---- git ----

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            err
        })
    }
}

/// Where a repo is and which ref its trunk is.
#[derive(Debug, Clone)]
pub struct RepoCtx {
    /// A path inside the repo (the worktree commits are read from).
    pub dir: PathBuf,
    /// The repo's name as lagent knows it: the main checkout's directory name.
    pub name: String,
    /// `origin/<trunk>` when the repo has an origin, else the local trunk branch.
    pub trunk_ref: String,
}

/// Find the repo around `dir`: its name (from the main checkout, also for a worktree) and its
/// trunk ref, decided like the daemon does (origin's HEAD, else main/master; the trunk's
/// `.lucina/pipeline.toml` may name another trunk).
pub fn repo_ctx(dir: &Path) -> Result<RepoCtx, String> {
    let common = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map_err(|e| format!("not a git repository: {} ({e})", dir.display()))?;
    let common = PathBuf::from(common);
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf).unwrap_or(common.clone())
    } else {
        common.clone()
    };
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(".git").to_string())
        .unwrap_or_default();
    let remote = git(dir, &["remote", "get-url", "origin"]).ok().map(|_| "origin");
    let branch_exists =
        |b: &str| git(dir, &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")]).is_ok();
    let default = remote
        .and_then(|r| {
            git(
                dir,
                &["symbolic-ref", "--quiet", "--short", &format!("refs/remotes/{r}/HEAD")],
            )
            .ok()
            .map(|s| s.trim_start_matches(&format!("{r}/")).to_string())
        })
        .unwrap_or_else(|| {
            if branch_exists("main") {
                "main".into()
            } else if branch_exists("master") {
                "master".into()
            } else {
                "main".into()
            }
        });
    let at = |t: &str| match remote {
        Some(r) => format!("{r}/{t}"),
        None => t.to_string(),
    };
    let trunk = git(dir, &["show", &format!("{}:{PIPELINE_FILE}", at(&default))])
        .ok()
        .and_then(|t| Rules::parse(&t).ok())
        .and_then(|r| r.trunk)
        .unwrap_or(default);
    let trunk_ref = at(&trunk);
    git(
        dir,
        &["rev-parse", "--verify", "-q", &format!("{trunk_ref}^{{commit}}")],
    )
    .map_err(|_| format!("the trunk {trunk_ref} does not exist in {}", dir.display()))?;
    Ok(RepoCtx {
        dir: dir.to_path_buf(),
        name,
        trunk_ref,
    })
}

/// The model table of the trunk: `[attribution.models]` of its `.lucina/pipeline.toml`, else the
/// built-in default. Never read from the branch.
pub fn trunk_models(ctx: &RepoCtx) -> Result<BTreeMap<String, String>, String> {
    match git(&ctx.dir, &["show", &format!("{}:{PIPELINE_FILE}", ctx.trunk_ref)]) {
        Ok(text) => Ok(Rules::parse(&text)?
            .attribution
            .models
            .unwrap_or_else(default_models)),
        Err(_) => Ok(default_models()),
    }
}

/// The names in a commit's `Co-Authored-By` trailers (the part before ` <email>`).
fn trailer_names(dir: &Path, sha: &str) -> Result<Vec<String>, String> {
    let out = git(
        dir,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Co-Authored-By,valueonly,unfold)",
            sha,
        ],
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.split('<').next().unwrap_or(l).trim().to_string())
        .collect())
}

/// Which commits an attest call covers.
#[derive(Debug, Clone)]
pub enum Selection {
    /// `trunk..HEAD`, non-merge commits.
    Default,
    Commit(String),
    Range(String),
}

#[derive(Debug, Clone)]
pub struct AttestArgs {
    pub model: String,
    pub kind: String,
    pub lane: String,
    pub session: String,
    pub declared_by: String,
    pub workspace: String,
    pub selection: Selection,
}

/// What attesting one commit came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub appended: Appended,
    pub subject: String,
}

/// Attest the selected commits of the repo around `dir`. Refuses, writing nothing, when any
/// commit has no trailer, names another model, or the model is unknown.
pub fn attest(ledger: &Ledger, dir: &Path, a: &AttestArgs) -> Result<Vec<Line>, LedgerError> {
    let other = LedgerError::Other;
    if !KINDS.contains(&a.kind.as_str()) {
        return Err(other(format!(
            "--kind {:?} is not one of lane, subagent, lead",
            a.kind
        )));
    }
    let ctx = repo_ctx(dir).map_err(other)?;
    let models = trunk_models(&ctx).map_err(other)?;
    let Some(expected) = models.get(&a.model) else {
        let known: Vec<&str> = models.keys().map(String::as_str).collect();
        return Err(LedgerError::Refused(vec![format!(
            "model {:?} is not in the attribution table of {} ({}); declare one of: {}. A new model is added to [attribution.models] in .lucina/pipeline.toml on the trunk",
            a.model,
            ctx.trunk_ref,
            PIPELINE_FILE,
            known.join(", ")
        )]));
    };
    // The commits, oldest first.
    let shas: Vec<String> = match &a.selection {
        Selection::Default => git(
            dir,
            &["rev-list", "--no-merges", "--reverse", &format!("{}..HEAD", ctx.trunk_ref)],
        )
        .map_err(other)?
        .lines()
        .map(str::to_string)
        .collect(),
        Selection::Range(r) => git(dir, &["rev-list", "--no-merges", "--reverse", r])
            .map_err(other)?
            .lines()
            .map(str::to_string)
            .collect(),
        Selection::Commit(rev) => {
            let sha = git(dir, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
                .map_err(other)?;
            let parents = git(dir, &["rev-list", "--parents", "-n", "1", &sha]).map_err(other)?;
            if parents.split_whitespace().count() > 2 {
                Vec::new() // a merge commit is skipped
            } else {
                vec![sha]
            }
        }
    };
    let mut refusals = Vec::new();
    let mut drafts = Vec::new();
    let mut subjects = Vec::new();
    let mut seen = BTreeSet::new();
    for sha in shas {
        if !seen.insert(sha.clone()) {
            continue;
        }
        let names = trailer_names(dir, &sha).map_err(other)?;
        let subject = git(dir, &["log", "-1", "--format=%s", &sha]).map_err(other)?;
        let short = &sha[..12];
        let fix = "amend the commit so its trailer names the model that wrote it, then attest again";
        if names.is_empty() {
            refusals.push(format!(
                "commit {short} ({subject}) has no Co-Authored-By trailer, but you declared {} (expected \"Co-Authored-By: {expected} <...>\"); {fix}",
                a.model
            ));
        } else if !names.iter().any(|n| n == expected) {
            refusals.push(format!(
                "commit {short} ({subject}) names {} in its Co-Authored-By trailer, but you declared {} (\"{expected}\"); {fix}",
                names.join(", "),
                a.model
            ));
        } else if let Some(wrong) = names
            .iter()
            .find(|n| *n != expected && models.values().any(|v| v == *n))
        {
            refusals.push(format!(
                "commit {short} ({subject}) names both \"{expected}\" and \"{wrong}\" in its Co-Authored-By trailers, so it is not clear which model wrote it; {fix}"
            ));
        } else {
            drafts.push(Entry {
                v: 1,
                commit: sha,
                repo: ctx.name.clone(),
                workspace: a.workspace.clone(),
                model: a.model.clone(),
                kind: a.kind.clone(),
                lane: a.lane.clone(),
                session: a.session.clone(),
                declared_by: a.declared_by.clone(),
                time: String::new(),
                prev: String::new(),
            });
            subjects.push(subject);
        }
    }
    if !refusals.is_empty() {
        return Err(LedgerError::Refused(refusals));
    }
    let done = ledger.append(drafts)?;
    Ok(done
        .into_iter()
        .zip(subjects)
        .map(|(appended, subject)| Line { appended, subject })
        .collect())
}

/// For the merge log: of the branch's non-merge commits `base..head`, how many have an entry for
/// this repo. Returns `(commits, attested)`.
pub fn count_attested(ledger: &Path, repo_dir: &Path, repo_name: &str, base: &str, head: &str) -> Option<(usize, usize)> {
    let list = git(repo_dir, &["rev-list", "--no-merges", &format!("{base}..{head}")]).ok()?;
    let commits: Vec<&str> = list.lines().collect();
    let have: BTreeSet<String> = Ledger::new(ledger)
        .read_lenient()
        .into_iter()
        .filter(|e| e.repo == repo_name)
        .map(|e| e.commit)
        .collect();
    let n = commits.iter().filter(|c| have.contains(**c)).count();
    Some((commits.len(), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// A repo (named `proj`) with a `main` and no origin, and a state dir.
    fn setup() -> (tempfile::TempDir, PathBuf, Ledger) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("proj");
        std::fs::create_dir_all(&repo).unwrap();
        g(&repo, &["init", "-q", "-b", "main"]);
        g(&repo, &["config", "user.email", "t@t"]);
        g(&repo, &["config", "user.name", "t"]);
        g(&repo, &["config", "commit.gpgsign", "false"]);
        commit(&repo, "a.txt", "1\n", "base", None);
        g(&repo, &["checkout", "-q", "-b", "topic"]);
        let ledger = Ledger::in_state_dir(&root.path().join("state"));
        (root, repo, ledger)
    }

    fn commit(dir: &Path, file: &str, text: &str, msg: &str, trailer: Option<&str>) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        g(dir, &["add", "-A"]);
        let full = match trailer {
            Some(t) => format!("{msg}\n\nCo-Authored-By: {t} <noreply@example.com>"),
            None => msg.to_string(),
        };
        g(dir, &["commit", "-q", "-m", &full]);
        g(dir, &["rev-parse", "HEAD"])
    }

    fn args(model: &str) -> AttestArgs {
        AttestArgs {
            model: model.into(),
            kind: "lane".into(),
            lane: "claude-sonnet".into(),
            session: "tester@host".into(),
            declared_by: "flag".into(),
            workspace: String::new(),
            selection: Selection::Default,
        }
    }

    const SONNET: &str = "Claude Sonnet 5.5";
    const OPUS: &str = "Claude Opus 5.5";

    fn lines(l: &Ledger) -> Vec<String> {
        std::fs::read_to_string(&l.path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn attest_writes_a_hash_chain_and_verify_catches_a_flipped_byte() {
        let (_r, repo, ledger) = setup();
        let c1 = commit(&repo, "b", "1", "one", Some(SONNET));
        let c2 = commit(&repo, "c", "1", "two", Some(SONNET));
        let c3 = commit(&repo, "d", "1", "three", Some(SONNET));
        let out = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap();
        assert_eq!(out.len(), 3);
        let ls = lines(&ledger);
        assert_eq!(ls.len(), 3);
        let es = ledger.read().unwrap();
        assert_eq!(
            es.iter().map(|e| e.commit.clone()).collect::<Vec<_>>(),
            vec![c1, c2, c3]
        );
        assert_eq!(es[0].prev, ZERO);
        assert_eq!(es[1].prev, sha256_hex(ls[0].as_bytes()));
        assert_eq!(es[2].prev, sha256_hex(ls[1].as_bytes()));
        assert_eq!(es[0].repo, "proj");
        assert_eq!(es[0].model, "claude-sonnet-5-5");
        assert_eq!(es[0].kind, "lane");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&ledger.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Flip a byte in the middle line (a letter of the session).
        let text = std::fs::read_to_string(&ledger.path).unwrap();
        let mid = ls[1].clone();
        let tampered = mid.replace("tester", "tester2");
        std::fs::write(&ledger.path, text.replace(&mid, &tampered)).unwrap();
        // The middle line still parses; the next line's prev shows it was changed.
        let b = ledger.read().unwrap_err();
        assert_eq!(b.line, 2, "{b:?}");
        // Corrupting the first line's content as well: now line 1 is the first bad line.
        let text = lines(&ledger).join("\n") + "\n";
        std::fs::write(&ledger.path, text.replacen("tester@host", "tester@hosX", 1)).unwrap();
        assert_eq!(ledger.read().unwrap_err().line, 1);
        // And an append is refused on a broken ledger.
        let err = ledger.append(vec![]).unwrap_err();
        assert!(matches!(err, LedgerError::Other(m) if m.contains("broken at line 1")));
    }

    #[test]
    fn a_trailer_for_another_model_or_none_or_an_unknown_model_is_refused() {
        let (_r, repo, ledger) = setup();
        commit(&repo, "b", "1", "opus wrote it", Some(OPUS));
        let err = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains("Claude Opus 5.5") && m[0].contains("claude-sonnet-5-5"), "{m:?}");
        assert!(m[0].contains("amend the commit"), "{m:?}");
        assert!(lines(&ledger).is_empty());

        g(&repo, &["reset", "-q", "--hard", "main"]);
        commit(&repo, "b", "1", "no trailer", None);
        let err = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains("no Co-Authored-By trailer"), "{m:?}");

        let err = attest(&ledger, &repo, &args("gpt-9")).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains("not in the attribution table"), "{m:?}");
        assert!(lines(&ledger).is_empty());
    }

    #[test]
    fn two_models_in_one_trailer_block_are_ambiguous() {
        let (_r, repo, ledger) = setup();
        std::fs::write(repo.join("b"), "1").unwrap();
        g(&repo, &["add", "-A"]);
        g(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                &format!("both\n\nCo-Authored-By: {OPUS} <n@e>\nCo-Authored-By: {SONNET} <n@e>"),
            ],
        );
        let err = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains("not clear which model"), "{m:?}");
    }

    #[test]
    fn a_wrong_commit_in_a_range_writes_nothing() {
        let (_r, repo, ledger) = setup();
        commit(&repo, "b", "1", "one", Some(SONNET));
        commit(&repo, "c", "1", "two", Some(OPUS));
        commit(&repo, "d", "1", "three", Some(SONNET));
        assert!(attest(&ledger, &repo, &args("claude-sonnet-5-5")).is_err());
        assert!(lines(&ledger).is_empty());
        assert!(!ledger.path.exists() || std::fs::read_to_string(&ledger.path).unwrap().is_empty());
    }

    #[test]
    fn attesting_twice_is_a_noop_and_another_model_is_refused() {
        let (_r, repo, ledger) = setup();
        let c = commit(&repo, "b", "1", "one", Some(SONNET));
        let first = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap();
        assert!(matches!(first[0].appended, Appended::Written(_)));
        let second = attest(&ledger, &repo, &args("claude-sonnet-5-5")).unwrap();
        assert!(matches!(second[0].appended, Appended::Already(_)));
        assert_eq!(lines(&ledger).len(), 1);
        // The same commit declared as Opus is refused by the trailer check, so use a
        // table where both names are one commit's trailers: attest it directly.
        let mut d = ledger.read().unwrap()[0].clone();
        d.model = "claude-opus-5-5".into();
        let err = ledger.append(vec![d]).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains(&c[..12]) && m[0].contains("claude-sonnet-5-5") && m[0].contains("claude-opus-5-5"), "{m:?}");
        assert_eq!(lines(&ledger).len(), 1);
    }

    #[test]
    fn merge_commits_in_a_range_are_skipped() {
        let (_r, repo, ledger) = setup();
        let c1 = commit(&repo, "b", "1", "one", Some(SONNET));
        g(&repo, &["checkout", "-q", "main"]);
        let cm = commit(&repo, "m", "1", "on main", Some(SONNET));
        g(&repo, &["checkout", "-q", "topic"]);
        // A merge commit with a message that has no trailer.
        g(&repo, &["merge", "-q", "--no-ff", "-m", "merge main", "main"]);
        let c2 = commit(&repo, "c", "1", "two", Some(SONNET));
        let mut a = args("claude-sonnet-5-5");
        a.selection = Selection::Range(format!("{c1}~1..HEAD"));
        let out = attest(&ledger, &repo, &a).unwrap();
        let got: Vec<String> = ledger.read().unwrap().into_iter().map(|e| e.commit).collect();
        // c1, the commit that came in from main, and c2; the merge commit itself is not there.
        assert_eq!(out.len(), 3);
        let mut want = vec![c1, cm, c2];
        want.sort();
        let mut got_sorted = got.clone();
        got_sorted.sort();
        assert_eq!(got_sorted, want);
        // An explicit merge commit is skipped too.
        let merge = g(&repo, &["log", "-1", "--merges", "--format=%H"]);
        let mut a = args("claude-sonnet-5-5");
        a.selection = Selection::Commit(merge);
        assert!(attest(&ledger, &repo, &a).unwrap().is_empty());
        assert_eq!(lines(&ledger).len(), 3);
    }

    #[test]
    fn the_model_table_comes_from_the_trunk_not_the_branch() {
        let (_r, repo, ledger) = setup();
        // The branch adds a table with a new model: not honoured.
        std::fs::create_dir_all(repo.join(".lucina")).unwrap();
        std::fs::write(
            repo.join(".lucina/pipeline.toml"),
            "[attribution.models]\n\"newbie\" = \"New Bie\"\n\"claude-sonnet-5-5\" = \"Claude Sonnet 5.5\"\n",
        )
        .unwrap();
        commit(&repo, "b", "1", "new model", Some("New Bie"));
        let err = attest(&ledger, &repo, &args("newbie")).unwrap_err();
        let LedgerError::Refused(m) = err else { panic!() };
        assert!(m[0].contains("not in the attribution table"), "{m:?}");
        // Once the trunk has the table, it is honoured, and replaces the default.
        g(&repo, &["checkout", "-q", "main"]);
        g(&repo, &["checkout", "-q", "topic", "--", ".lucina/pipeline.toml"]);
        g(&repo, &["commit", "-q", "-m", "table"]);
        g(&repo, &["checkout", "-q", "topic"]);
        g(&repo, &["merge", "-q", "main"]);
        commit(&repo, "c", "1", "by newbie", Some("New Bie"));
        // Both commits by New Bie (the earlier one too) are in the range now.
        let out = attest(&ledger, &repo, &args("newbie")).unwrap();
        assert_eq!(out.len(), 2);
        // The table replaced the default: qoder is no longer known on this trunk.
        let err = attest(&ledger, &repo, &args("qoder")).unwrap_err();
        assert!(matches!(err, LedgerError::Refused(_)));
    }

    #[test]
    fn counting_attested_commits_for_the_merge_log() {
        let (_r, repo, ledger) = setup();
        let c1 = commit(&repo, "b", "1", "one", Some(SONNET));
        commit(&repo, "c", "1", "two", Some(SONNET));
        let mut a = args("claude-sonnet-5-5");
        a.selection = Selection::Commit(c1);
        attest(&ledger, &repo, &a).unwrap();
        assert_eq!(count_attested(&ledger.path, &repo, "proj", "main", "topic"), Some((2, 1)));
        assert_eq!(count_attested(&ledger.path, &repo, "other", "main", "topic"), Some((2, 0)));
    }
}
