//! `pipeline-guard`: a Claude Code PreToolUse(Bash) hook. In Lucina repos, work starts with
//! `lagent task start` and lands with `lagent merge` (docs/repo-workflow.md), so this denies:
//!
//! - making a worktree by hand: `git worktree add …` (a `--detach` deploy or test tree is fine: it is
//!   not a task), `wt add …`;
//! - landing by hand: `git push <remote> <ref>:main` / `git push <remote> main`.
//!
//! It reads the hook's JSON on stdin and prints a `deny` decision with the command to use
//! instead; anything else passes (exit 0, no output). Scope: the session's cwd or the
//! command names a Lucina repo. `LAGENT_BYPASS=<reason>` in the command lets it through
//! (logged to `~/.local/state/pipeline-guard/bypass.log`); agentd's own jobs never pass
//! through Claude hooks.

use std::io::{Read, Write};

use regex::Regex;
use serde_json::{json, Value};

/// Names that put a command or cwd in scope.
const SCOPE: &str = r"(?i)(Projects/Lucina\b|SecretaryIOS|\.worktrees/(SecretaryIOS|lucinaos|LucinaAndroidOS|lucina)|\blucinaos\b|LucinaAndroidOS|lucina-rs\b)";

/// What a command does that the pipeline does instead, if anything.
pub fn verdict(command: &str, cwd: &str) -> Option<String> {
    let scope = Regex::new(SCOPE).ok()?;
    if !scope.is_match(command) && !scope.is_match(cwd) {
        return None;
    }
    if Regex::new(r"\bLAGENT_BYPASS=\S").ok()?.is_match(command) {
        return None;
    }
    // One shell segment at a time (`;`, `&&`, `||`, `|`, newlines, quotes of ssh commands).
    let worktree = Regex::new(r"\bgit\s+(-C\s+\S+\s+|-c\s+\S+\s+)*worktree\s+add\b").ok()?;
    let wt = Regex::new(r#"(^|[\s;&|('"])(~/\.local/bin/)?wt\s+add\b"#).ok()?;
    let push = Regex::new(r#"\bgit\s+(-C\s+\S+\s+|-c\s+\S+\s+)*push\b([^;&|\n'"]*)"#).ok()?;
    let to_main = Regex::new(r"(^|\s|:|\+)(refs/heads/)?main(\s|$)").ok()?;
    let detached = Regex::new(r"\bworktree\s+add\b[^;&|\n'\"]*\s(--detach|-d)\b").ok()?;
    if (worktree.is_match(command) && !detached.is_match(command)) || wt.is_match(command) {
        return Some(
            "Lucina repos: start work with `lagent task start \"<title>\" [--repo <path>] [--branch <name>]` on nezopt \
             (Mac: ~/.local/bin/lagent forwards). It fetches, makes the worktree from origin/main and registers it with \
             your session, so the merge queue and the board know it. Then work and commit in the printed path."
                .into(),
        );
    }
    for c in push.captures_iter(command) {
        let args = c.get(2).map(|m| m.as_str()).unwrap_or_default();
        if to_main.is_match(args) && !args.contains("--dry-run") {
            return Some(
                "Lucina repos: main moves only through the merge queue. Commit in your workspace, then run \
                 `lagent merge <workspace id | path>`: it rebases on origin/main, runs the repo's gates (cargo test of \
                 the touched crates, diffgate, ...), pushes and archives the workspace, one merge at a time. \
                 Deploys: `lagent deploy <runtime>`."
                    .into(),
            );
        }
    }
    None
}

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(&input) else { return };
    let command = v["tool_input"]["command"].as_str().unwrap_or_default();
    let cwd = v["cwd"].as_str().unwrap_or_default();
    if command.contains("LAGENT_BYPASS=") {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = format!("{home}/.local/state/pipeline-guard");
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/bypass.log")) {
                let _ = writeln!(f, "{}\t{}\t{}", v["session_id"].as_str().unwrap_or("?"), cwd, command.replace('\n', " "));
            }
        }
    }
    if let Some(reason) = verdict(command, cwd) {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": format!("pipeline-guard: {reason} (Owner-approved exception: put LAGENT_BYPASS=<reason> in the command.)"),
            }
        });
        println!("{out}");
    }
}

#[cfg(test)]
mod tests {
    use super::verdict;

    const LUCINA: &str = "/home/nez/Projects/Lucina/SecretaryIOS";

    #[test]
    fn worktrees_go_through_task_start() {
        assert!(verdict("git worktree add ../x -b x", LUCINA).is_some());
        assert!(verdict("ssh nezopt 'cd ~/Projects/Lucina/SecretaryIOS && wt add ig-fix origin/main'", "/Users/g").is_some());
        assert!(verdict("ssh nezopt 'cd ~/Projects/Lucina/lucinaos && git -C . worktree add /tmp/x'", "/Users/g").is_some());
        assert!(verdict("git worktree list", LUCINA).is_none());
        // A detached tree for a deploy or a test run is not a task.
        assert!(verdict("git worktree add --detach /tmp/deploy-x origin/main", LUCINA).is_none());
        assert!(verdict("git -C ~/Projects/Lucina/SecretaryIOS worktree add -q /tmp/x -d abc", "/Users/g").is_none());
        assert!(verdict("git worktree add -b topic ../x origin/main", LUCINA).is_some());
        assert!(verdict("wt ls", LUCINA).is_none());
        // Outside Lucina: not our business.
        assert!(verdict("git worktree add ../x", "/home/nez/Projects/AI/lane").is_none());
    }

    #[test]
    fn main_moves_only_through_the_queue() {
        assert!(verdict("git push origin HEAD:main", LUCINA).is_some());
        assert!(verdict("git push -q origin abc123:refs/heads/main", LUCINA).is_some());
        assert!(verdict("cd x && git push origin main && echo ok", LUCINA).is_some());
        assert!(verdict("ssh nezopt 'cd ~/Projects/.worktrees/SecretaryIOS/x && git push origin HEAD:main'", "/Users/g").is_some());
        assert!(verdict("git push origin my-branch", LUCINA).is_none());
        assert!(verdict("git push origin main-fix", LUCINA).is_none());
        assert!(verdict("git push origin deploy/web/20261101T1200Z-abc", LUCINA).is_none());
        assert!(verdict("git push --dry-run origin HEAD:main", LUCINA).is_none());
        assert!(verdict("git log origin/main", LUCINA).is_none());
    }

    #[test]
    fn a_bypass_with_a_reason_passes() {
        assert!(verdict("LAGENT_BYPASS=owner-said-so git push origin HEAD:main", LUCINA).is_none());
        assert!(verdict("LAGENT_BYPASS= git push origin HEAD:main", LUCINA).is_some());
    }
}
