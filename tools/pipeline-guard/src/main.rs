//! `pipeline-guard`: a Claude Code PreToolUse(Bash) hook. In Lucina repos, work starts with
//! `lagent task start` and lands with `lagent merge` (docs/repo-workflow.md), so this denies:
//!
//! - making a worktree by hand: `git worktree add …` (a `--detach` deploy or test tree is fine: it is
//!   not a task), `wt add …`;
//! - landing by hand: `git push <remote> <ref>:main` / `git push <remote> main`;
//! - installing on the owner's iPhone by hand: `devicectl device install|uninstall`, `ios-deploy`,
//!   `ideviceinstaller`, `pymobiledevice3 apps install`, `xcodebuild test|install` against the phone,
//!   also inside a script the command runs. Builds go through `lagent device install iphone`; one
//!   plain `ios-install …` call (the Mac executor, `ios-install exec --grant`) passes. Scope here
//!   also counts a command, script or cwd that names Secretary.
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
    let detached = Regex::new(r#"\bworktree\s+add\b[^;&|\n'"]*\s(--detach|-d)\b"#).ok()?;
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

/// The owner's iPhone: its ids, and destinations that name it.
const PHONE: &str = r#"00008140-001225CE1A03801C|FA374B43-B705-5A50-8628-A175A5CA4201|platform=iOS([,'"\s]|$)|name=Giuseppe"#;

/// What replaces or removes the app on the owner's iPhone, if `text` does.
pub fn device_install(text: &str) -> Option<&'static str> {
    let rules: [(&str, &'static str); 4] = [
        (r"\bdevicectl\b[^;&|\n]*\bdevice\s+(install|uninstall)\b", "devicectl device install"),
        (r"(^|[^\w-])ios-deploy([^\w-]|$)", "ios-deploy"),
        (r"\bideviceinstaller\b", "ideviceinstaller"),
        (r"\bpymobiledevice3\b[^;&|\n]*\bapps\s+(install|uninstall)\b", "pymobiledevice3 apps install"),
    ];
    for (re, what) in rules {
        if Regex::new(re).ok()?.is_match(text) {
            return Some(what);
        }
    }
    let action = Regex::new(r"(^|\s)(test|test-without-building|install)(\s|$)").ok()?;
    let phone = Regex::new(PHONE).ok()?;
    let segments = Regex::new(r"[;\n]|&&|\|\|?").ok()?;
    let on_phone = segments
        .split(text)
        .any(|seg| seg.contains("xcodebuild") && action.is_match(seg) && phone.is_match(seg) && !seg.contains("Simulator"));
    on_phone.then_some("xcodebuild test/install on the iPhone")
}

/// A command that is one `ios-install` call: it checks its grant with agentd itself.
fn single_ios_install(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    (first == "ios-install" || first.ends_with("/ios-install")) && !["\n", ";", "&", "|", "$(", "`"].iter().any(|c| command.contains(c))
}

fn device_reason(what: &str, file: Option<&str>) -> String {
    format!(
        "{what}{}: builds reach the owner's iPhone only through Lucina. `lagent device install iphone` installs \
         origin/main's tip (built on the Mac, recorded by deploy-guard). A branch: `lagent device lease iphone --purpose \"…\"`, \
         the owner's go through lucina_decide naming the sha8, then `lagent device install iphone --test --commit <sha> \
         --approval dc-…`. A phone test run: `lagent device grant iphone --commit <sha>`, then on the Mac \
         `ios-install exec --grant <id> -- xcodebuild test …`. `lagent device ls` shows who holds it; `--dry-run` checks.",
        file.map(|f| format!(" (in {f})")).unwrap_or_default()
    )
}

fn device_verdict(command: &str) -> Option<String> {
    if single_ios_install(command) {
        return None;
    }
    device_install(command).map(|w| device_reason(w, None))
}

/// The script files a command runs (`bash x.sh`, `./x.sh`), read so their installs count too.
fn scripts(command: &str, cwd: &str) -> Vec<(String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();
    command
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'' || c == ';'))
        .filter(|t| !t.ends_with("ios-install"))
        .filter_map(|t| {
            let p = match t.strip_prefix("~/") {
                Some(rest) => std::path::PathBuf::from(&home).join(rest),
                None if t.starts_with('/') => std::path::PathBuf::from(t),
                None => std::path::Path::new(cwd).join(t),
            };
            let meta = std::fs::metadata(&p).ok()?;
            if !meta.is_file() || meta.len() > 512 * 1024 {
                return None;
            }
            let text = std::fs::read_to_string(&p).ok()?;
            (t.ends_with(".sh") || text.starts_with("#!")).then(|| (p.display().to_string(), text))
        })
        .collect()
}

/// Lucina scope, or the text names Secretary (its builds live in scratch dirs too).
fn in_scope(text: &str) -> bool {
    Regex::new(SCOPE).is_ok_and(|s| s.is_match(text)) || text.contains("Secretary") || text.contains("tech.getsecretary")
}

/// The whole decision: the command's own verdict, else an install inside a script it runs.
pub fn decide(command: &str, cwd: &str) -> Option<String> {
    if let Some(r) = verdict(command, cwd) {
        return Some(r);
    }
    if command.contains("LAGENT_BYPASS=") || single_ios_install(command) {
        return None;
    }
    if in_scope(command) || in_scope(cwd) {
        if let Some(r) = device_verdict(command) {
            return Some(r);
        }
    }
    scripts(command, cwd)
        .into_iter()
        .find_map(|(f, text)| device_install(&text).filter(|_| in_scope(&text) || in_scope(cwd)).map(|w| device_reason(w, Some(&f))))
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
    if let Some(reason) = decide(command, cwd) {
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
    use super::{decide, verdict};

    const LUCINA: &str = "/home/nez/Projects/Lucina/SecretaryIOS";
    const PHONE: &str = "-destination 'id=00008140-001225CE1A03801C'";

    #[test]
    fn iphone_installs_go_through_lagent_device() {
        for c in [
            "xcrun devicectl device install app --device FA374B43-B705-5A50-8628-A175A5CA4201 build/X.app".to_string(),
            "cd ios && xcrun devicectl device uninstall app --device X tech.getsecretary.app".into(),
            "ios-deploy --bundle Secretary.app".into(),
            "pymobiledevice3 apps install x.ipa".into(),
            format!("lockf -k /tmp/iphone16e.lock xcodebuild test-without-building -scheme Secretary {PHONE}"),
            "xcodebuild test -scheme Secretary -destination 'platform=iOS,name=Giuseppe - iPhone'".into(),
            format!("ios-install status; xcodebuild test -scheme Secretary {PHONE}"),
        ] {
            assert!(decide(&c, LUCINA).is_some(), "{c}");
        }
        // A Secretary build installed from elsewhere is still in scope.
        assert!(decide("xcrun devicectl device install app --device X /tmp/s/Secretary.app", "/Users/g").is_some());
        for c in [
            format!("xcodebuild -scheme Secretary -configuration Release {PHONE} build"),
            "xcodebuild test -scheme Secretary -destination 'platform=iOS Simulator,name=iPhone 16'".into(),
            "xcrun devicectl list devices".into(),
            "xcrun devicectl device process launch --device X tech.getsecretary.app".into(),
            format!("ios-install exec --grant gr_1 -- xcodebuild test -scheme Secretary {PHONE}"),
            "ssh nezopt '~/.local/bin/lagent device install iphone'".into(),
            format!("LAGENT_BYPASS=owner-said-so xcodebuild test -scheme Secretary {PHONE}"),
        ] {
            assert!(decide(&c, LUCINA).is_none(), "{c}");
        }
        // Another project's app is not Lucina's business.
        assert!(decide("xcrun devicectl device install app --device X build/CardRails.app", "/Users/g/Projects/CardRails-iOS").is_none());
    }

    #[test]
    fn installs_inside_a_script_count() {
        let dir = std::env::temp_dir().join(format!("pipeline-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("deploy.sh");
        std::fs::write(&bad, "#!/bin/bash\nxcrun devicectl device install app --device X ios/build/Secretary.app\n").unwrap();
        let fine = dir.join("test.sh");
        std::fs::write(&fine, "#!/bin/bash\ncargo test -p secretary-core\n").unwrap();
        assert!(decide(&format!("bash {}", bad.display()), LUCINA).unwrap().contains("deploy.sh"));
        assert!(decide(&format!("bash {}", bad.display()), "/tmp").is_some(), "the script names Secretary");
        assert!(decide(&format!("bash {}", fine.display()), LUCINA).is_none());
        std::fs::remove_file(bad).unwrap();
        std::fs::remove_file(fine).unwrap();
        let _ = std::fs::remove_dir(dir);
    }

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
