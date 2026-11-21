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

mod session;

/// Names that put a command or cwd in scope.
const SCOPE: &str = r"(?i)(Projects/Lucina\b|SecretaryIOS|\.worktrees/(SecretaryIOS|lucinaos|LucinaAndroidOS|lucina)|\blucinaos\b|LucinaAndroidOS|lucina-rs\b)";

/// The command without its here-documents' bodies: text a command writes to a file or feeds to
/// a program (a doc, a Python edit) is data, not something the shell runs.
pub fn without_heredocs(command: &str) -> String {
    let Ok(start) = Regex::new(r#"<<-?\s*(['"]?)([A-Za-z_][A-Za-z0-9_]*)['"]?"#) else {
        return command.to_string();
    };
    let shell = Regex::new(r"(^|[\s;&|(])(bash|sh|zsh|ssh|sudo)\b").ok();
    let mut out = String::new();
    let mut until: Option<(String, bool)> = None;
    for line in command.split('\n') {
        if let Some((tag, dash)) = &until {
            let l = if *dash {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if l.trim_end() == tag {
                until = None;
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(c) = start.captures(line) {
            // A body a shell runs (`bash <<EOF`, `ssh host <<EOF`) is commands: keep it.
            let before = &line[..c.get(0).map(|m| m.start()).unwrap_or(0)];
            let runs = shell.as_ref().is_some_and(|r| r.is_match(before));
            if !runs {
                until = Some((c[2].to_string(), c[0].starts_with("<<-")));
            }
        }
    }
    out
}

const BYPASS_WORD_CAP: usize = 4096;

/// Skips a `${ … }` whose `{` is next, by brace depth (`${X:-${Y}}` is one expansion). `false` when
/// it never closes or the word cap is passed: the value is then unknown.
fn skip_braces(it: &mut std::iter::Peekable<std::str::CharIndices<'_>>, seen: &mut usize) -> bool {
    it.next();
    let mut depth = 1usize;
    for (_, n) in it.by_ref() {
        *seen += 1;
        if *seen > BYPASS_WORD_CAP {
            return false;
        }
        match n {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The literal text of the `LAGENT_BYPASS=` value that starts `after`, and what follows the shell
/// word. The word is read like the shell does: quoted (`'…'`, `"…"`) and bare pieces join, up to an
/// unquoted whitespace or one of `; & | < > ( )`. `$VAR` and `${VAR}` are dropped (the guard cannot
/// expand them; a default such as `${X:-reason}` is unknown too, so it counts as nothing). `$(`,
/// `$((`, a backtick, `$'` and a backslash make the value unknown: `None`, which is no bypass.
fn bypass_value(after: &str) -> (Option<String>, &str) {
    // The value is read lazily and at most 4096 characters long: a longer word is unknown.
    const CAP: usize = BYPASS_WORD_CAP;
    let mut it = after.char_indices().peekable();
    let mut lit = String::new();
    let mut known = true;
    let mut seen = 0usize;
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    'word: while let Some(&(_, c)) = it.peek() {
        if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')') {
            break;
        }
        it.next();
        seen += 1;
        if seen > CAP {
            known = false;
            break;
        }
        match c {
            '\'' => {
                for (_, q) in it.by_ref() {
                    seen += 1;
                    if q == '\'' {
                        break;
                    }
                    if seen > CAP {
                        known = false;
                        break 'word;
                    }
                    lit.push(q);
                }
            }
            '"' => {
                while let Some((_, q)) = it.next() {
                    seen += 1;
                    if seen > CAP {
                        known = false;
                        break 'word;
                    }
                    match q {
                        '"' => break,
                        '\\' | '`' => known = false,
                        '$' => match it.peek().map(|&(_, n)| n) {
                            Some('(' | '\'') => known = false,
                            Some('{') => {
                                if !skip_braces(&mut it, &mut seen) {
                                    known = false;
                                    break 'word;
                                }
                            }
                            Some(n) if n.is_ascii_alphabetic() || n == '_' => {
                                while it.next_if(|&(_, n)| is_name(n)).is_some() {
                                    seen += 1;
                                }
                            }
                            // `$@ $* $! $? $# $$ $-` and `$1`: unknown, counts as nothing.
                            Some(n) if n.is_ascii_digit() || "@*#?!$-".contains(n) => {
                                it.next();
                            }
                            _ => lit.push('$'),
                        },
                        q => lit.push(q),
                    }
                }
            }
            '\\' | '`' => known = false,
            '$' => match it.peek().map(|&(_, n)| n) {
                Some('(' | '\'' | '"') => known = false,
                Some('{') => {
                    if !skip_braces(&mut it, &mut seen) {
                        known = false;
                        break 'word;
                    }
                }
                Some(n) if n.is_ascii_alphabetic() || n == '_' => {
                    while it.next_if(|&(_, n)| is_name(n)).is_some() {
                        seen += 1;
                    }
                }
                Some(n) if n.is_ascii_digit() || "@*#?!$-".contains(n) => {
                    it.next();
                }
                _ => lit.push('$'),
            },
            c => lit.push(c),
        }
    }
    let end = it.peek().map_or(after.len(), |&(b, _)| b);
    (known.then_some(lit), &after[end..])
}

/// Whether `command` carries a real bypass: some `LAGENT_BYPASS=<reason>` whose value (read by
/// `bypass_value`) has literal non-whitespace text left. `LAGENT_BYPASS=`, `=""`, `=''`, `=" "`,
/// `=$EMPTY`, `=;`, `=$(true)` and `=\ ` give none. One valid occurrence is enough; a key that ends
/// a longer name (`FOO_LAGENT_BYPASS=x`) is not one. At most 32 occurrences are read, then it
/// fails closed. The marker is textual wherever it stands in the command (an accepted limit).
/// Every rule that honours a bypass asks this one function.
fn bypass_given(command: &str) -> bool {
    const KEY: &str = "LAGENT_BYPASS=";
    let mut rest = command;
    for _ in 0..32 {
        let Some(at) = rest.find(KEY) else {
            return false;
        };
        let inside_name = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let (value, tail) = bypass_value(&rest[at + KEY.len()..]);
        rest = tail;
        if !inside_name && value.is_some_and(|v| v.chars().any(|c| !c.is_whitespace())) {
            return true;
        }
    }
    false
}

/// What a command does that the pipeline does instead, if anything.
pub fn verdict(command: &str, cwd: &str) -> Option<String> {
    verdict_with(command, cwd, bypass_given(command))
}

/// `verdict`, with the bypass answer for the raw text already known. When it is true the merge rule reads
/// the heredoc-stripped text once more (a marker inside a here-document body is data there).
/// Behaviour is the same as before the answer was passed down.
fn verdict_with(command: &str, cwd: &str, bypass: bool) -> Option<String> {
    let command = &without_heredocs(command);
    if bypass && bypass_given(command) {
        return None;
    }
    // LA-C045 (HOST-18): a guarded call is in scope when the directory it acts on, or a literal repo path in its own
    // words, is a Lucina repo or worktree: not when the session merely sits in the group folder (`guarded_calls`).
    // The old whole-text answer stays as the fallback for calls the scan cannot place.
    let old_scope = Regex::new(SCOPE).ok()?;
    let old_scope = old_scope.is_match(command) || old_scope.is_match(cwd);
    let mut calls = Vec::new();
    guarded_calls(command, cwd, 0, &mut calls);
    // One shell segment at a time (`;`, `&&`, `||`, `|`, newlines, quotes of ssh commands).
    let worktree = Regex::new(r"\bgit\s+(-C\s+\S+\s+|-c\s+\S+\s+)*worktree\s+add\b").ok()?;
    let wt = Regex::new(r#"(^|[\s;&|('"])(~/\.local/bin/)?wt\s+add\b"#).ok()?;
    let push = Regex::new(r#"\bgit\s+(-C\s+\S+\s+|-c\s+\S+\s+)*push\b([^;&|\n'"]*)"#).ok()?;
    let to_main = Regex::new(r"(^|\s|:|\+)(refs/heads/)?main(\s|$)").ok()?;
    let detached = Regex::new(r#"\bworktree\s+add\b[^;&|\n'"]*\s(--detach|-d)\b"#).ok()?;
    let start_hits = if detached.is_match(command) {
        0
    } else {
        worktree.find_iter(command).count()
    } + wt.find_iter(command).count();
    if rule_fires(&calls, Kind::Start, start_hits, old_scope) {
        return Some(
            "Lucina repos: start work with `lagent task start \"<title>\" [--repo <path>] [--branch <name>]` on nezopt \
             (Mac: ~/.local/bin/lagent forwards). It fetches, makes the worktree from origin/main and registers it with \
             your session, so the merge queue and the board know it. Then work and commit in the printed path."
                .into(),
        );
    }
    if hand_merge_into_main_checkout(command, cwd) {
        return Some(
            "Lucina repos: a repo's main checkout moves only through the merge queue (repos without a remote too: \
             LucinaOS). Commit in your workspace and run `lagent merge <workspace id | path>`; \
             it fast-forwards the main checkout itself. Merging origin/main into your own worktree is fine."
                .into(),
        );
    }
    let push_hits = push
        .captures_iter(command)
        .filter(|c| {
            let args = c.get(2).map(|m| m.as_str()).unwrap_or_default();
            to_main.is_match(args) && !args.contains("--dry-run")
        })
        .count();
    if rule_fires(&calls, Kind::Push, push_hits, old_scope) {
        return Some(
            "Lucina repos: main moves only through the merge queue. Commit in your workspace, then run \
             `lagent merge <workspace id | path>`: it merges into origin/main with a merge commit, runs the repo's gates (cargo test of \
             the touched crates, diffgate, ...), pushes and archives the workspace, one merge at a time. \
             Deploys: `lagent deploy <runtime>`."
                .into(),
        );
    }
    None
}

/// A directory INSIDE a Lucina repo or worktree: `…/Projects/Lucina/<Repo>` or `…/.worktrees/<Repo>`, with a
/// non-empty repo name. The bare group folder `…/Projects/Lucina` (where the lead sessions sit) is no repo.
const REPO_DIR: &str = r"(?i)((^|/)Projects/Lucina/[^/]+(/|$)|(^|/)\.worktrees/[^/]+(/|$))";

/// The repo part of `SCOPE`: a literal repo path or name in a shell command's own words. The group
/// folder alone (`Projects/Lucina`) is not here.
const REPO_NAME: &str = r"(?i)(Projects/Lucina/[A-Za-z0-9_.-]+|\.worktrees/[A-Za-z0-9_.-]+|SecretaryIOS|\blucinaos\b|LucinaAndroidOS|lucina-rs\b)";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `git worktree add`, `wt add`.
    Start,
    /// `git push`.
    Push,
}

/// One guarded git/`wt` call found in a command: whether it breaks the rule (`hits`: not a detached
/// worktree, a push to main without `--dry-run`) and whether it acts on a Lucina repo (`in_scope`).
struct Call {
    kind: Kind,
    hits: bool,
    in_scope: bool,
}

/// Whether `word` names its path without a variable or substitution (a leading `$HOME` is fine).
fn literal_path(word: &str) -> bool {
    let t = word.replace("${HOME}", "").replace("$HOME", "");
    !t.contains(['$', '`'])
}

/// The guarded calls in `text`, one shell command at a time (`scan`: quotes, `ssh host '…'`, `bash -c '…'`, a
/// here-string, `eval`, `$( )`). The effective directory starts at `start` and follows a `cd <literal path>`;
/// `git -C <literal path>` acts on that path for its call. A variable or substitution in a path leaves the
/// directory as it was.
///
/// Accepted fail-opens (LA-C045, HOST-18): with the session in the group folder, a repo path held in a
/// variable or substitution (`git -C $R worktree add`), `cd $VAR`, a symlink from scratch to a repo, a script
/// file run by `bash script.sh` (not read), an alias or a function. A `cd` inside a `( … )` group carries to
/// the commands after the group.
fn guarded_calls(text: &str, start: &str, depth: usize, out: &mut Vec<Call>) {
    if depth > 8 {
        return;
    }
    let (Ok(repo_dir), Ok(repo_name), Ok(to_main)) = (
        Regex::new(REPO_DIR),
        Regex::new(REPO_NAME),
        Regex::new(r"(^|\s|:|\+)(refs/heads/)?main(\s|$)"),
    ) else {
        return;
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let sc = scan(text);
    let mut dir = start.to_string();
    for words in &sc.cmds {
        let Some(p) = program(words) else { continue };
        let name = basename(&words[p].val);
        let args = &words[p + 1..];
        let named = words[p..].iter().any(|w| repo_name.is_match(&w.val));
        match name {
            "cd" | "pushd" => {
                if let Some(a) = args.iter().find(|a| !a.val.starts_with('-')) {
                    if literal_path(&a.val) {
                        dir = resolve_path(&a.val, &dir, &home);
                    }
                }
            }
            "git" => {
                let mut at = dir.clone();
                let mut i = 0;
                while let Some(w) = args.get(i) {
                    match w.val.as_str() {
                        "-C" => {
                            if let Some(d) = args.get(i + 1).filter(|d| literal_path(&d.val)) {
                                at = resolve_path(&d.val, &at, &home);
                            }
                            i += 2;
                        }
                        "-c" => i += 2,
                        v if v.starts_with('-') => i += 1,
                        _ => break,
                    }
                }
                let Some(sub) = args.get(i) else { continue };
                let rest = args.get(i + 1..).unwrap_or_default();
                let in_scope = repo_dir.is_match(&at) || named;
                if sub.val == "worktree" && rest.first().is_some_and(|w| w.val == "add") {
                    let detached = rest
                        .iter()
                        .any(|w| matches!(w.val.as_str(), "--detach" | "-d"));
                    out.push(Call {
                        kind: Kind::Start,
                        hits: !detached,
                        in_scope,
                    });
                } else if sub.val == "push" {
                    let joined = rest
                        .iter()
                        .map(|w| w.val.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push(Call {
                        kind: Kind::Push,
                        hits: to_main.is_match(&joined) && !joined.contains("--dry-run"),
                        in_scope,
                    });
                }
            }
            "wt" => {
                if args
                    .iter()
                    .find(|a| !a.val.starts_with('-'))
                    .is_some_and(|a| a.val == "add")
                {
                    out.push(Call {
                        kind: Kind::Start,
                        hits: true,
                        in_scope: repo_dir.is_match(&dir) || named,
                    });
                }
            }
            "ssh" => {
                for t in remote_text(args).unwrap_or_default() {
                    guarded_calls(&t, &dir, depth + 1, out);
                }
            }
            "bash" | "sh" | "zsh" => {
                if let Some(s) = shell_script(args) {
                    guarded_calls(s, &dir, depth + 1, out);
                } else {
                    // A here-string: `bash <<< '…'`.
                    for (j, w) in args.iter().enumerate() {
                        if let Some(rest) = w.val.strip_prefix("<<<") {
                            let s = if rest.is_empty() {
                                args.get(j + 1).map(|n| n.val.as_str())
                            } else {
                                Some(rest)
                            };
                            if let Some(s) = s {
                                guarded_calls(s, &dir, depth + 1, out);
                            }
                        }
                    }
                }
            }
            "eval" => {
                let s = args
                    .iter()
                    .map(|w| w.val.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                guarded_calls(&s, &dir, depth + 1, out);
            }
            _ => {}
        }
    }
    for inner in &sc.nested {
        guarded_calls(inner, &dir, depth + 1, out);
    }
}

/// Whether a rule of kind `kind` fires. `old_hits` is how many calls the old whole-text patterns saw; `old_scope`
/// the old whole-text scope answer. A call the scan found counts when it acts on a Lucina repo. Calls the scan could
/// not place (a wrapper it does not know, `xargs git …`, a mention) fall back to the old whole-text scope, so the
/// deny side never shrinks for a call it cannot read.
fn rule_fires(calls: &[Call], kind: Kind, old_hits: usize, old_scope: bool) -> bool {
    if old_hits == 0 {
        return false;
    }
    let mine = || calls.iter().filter(|c| c.kind == kind && c.hits);
    mine().any(|c| c.in_scope) || (mine().count() < old_hits && old_scope)
}

/// The owner's iPhone: its ids, and destinations that name it.
const PHONE: &str = r#"00008140-001225CE1A03801C|FA374B43-B705-5A50-8628-A175A5CA4201|platform=iOS([,'"\s]|$)|name=Giuseppe"#;

/// What replaces or removes the app on the owner's iPhone, if `text` does.
pub fn device_install(text: &str) -> Option<&'static str> {
    let rules: [(&str, &'static str); 4] = [
        (
            r"\bdevicectl\b[^;&|\n]*\bdevice\s+(install|uninstall)\b",
            "devicectl device install",
        ),
        (r"(^|[^\w-])ios-deploy([^\w-]|$)", "ios-deploy"),
        (r"\bideviceinstaller\b", "ideviceinstaller"),
        (
            r"\bpymobiledevice3\b[^;&|\n]*\bapps\s+(install|uninstall)\b",
            "pymobiledevice3 apps install",
        ),
    ];
    for (re, what) in rules {
        if Regex::new(re).ok()?.is_match(text) {
            return Some(what);
        }
    }
    let action = Regex::new(r"(^|\s)(test|test-without-building|install)(\s|$)").ok()?;
    let phone = Regex::new(PHONE).ok()?;
    let segments = Regex::new(r"[;\n]|&&|\|\|?").ok()?;
    let on_phone = segments.split(text).any(|seg| {
        seg.contains("xcodebuild")
            && action.is_match(seg)
            && phone.is_match(seg)
            && !seg.contains("Simulator")
    });
    on_phone.then_some("xcodebuild test/install on the iPhone")
}

/// A command that is one `ios-install` call: it checks its grant with agentd itself.
fn single_ios_install(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    (first == "ios-install" || first.ends_with("/ios-install"))
        && !["\n", ";", "&", "|", "$(", "`"]
            .iter()
            .any(|c| command.contains(c))
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
    Regex::new(SCOPE).is_ok_and(|s| s.is_match(text))
        || text.contains("Secretary")
        || text.contains("tech.getsecretary")
}

/// The whole decision: the command's own verdict, else an install inside a script it runs.
pub fn decide(command: &str, cwd: &str) -> Option<String> {
    decide_on(command, cwd, cfg!(target_os = "macos"))
}

/// The same as `decide`, with the platform given: on the Mac, local pattern kills are denied in every session.
fn decide_on(command: &str, cwd: &str, is_mac: bool) -> Option<String> {
    decide_with(command, cwd, is_mac, bypass_given(command))
}

/// `decide_on`, with the bypass answer for the raw text computed once by the caller and shared by every
/// rule. (The merge rule reads the heredoc-stripped text once more, and only when that answer is true.)
fn decide_with(command: &str, cwd: &str, is_mac: bool, bypass: bool) -> Option<String> {
    if let Some(r) = mac_kill_verdict(command, is_mac, bypass) {
        return Some(r);
    }
    if let Some(r) = target_verdict(command, cwd, !is_mac, bypass) {
        return Some(r);
    }
    if let Some(r) = verdict_with(command, cwd, bypass) {
        return Some(r);
    }
    if bypass || single_ios_install(command) {
        return None;
    }
    if in_scope(command) || in_scope(cwd) {
        if let Some(r) = device_verdict(command) {
            return Some(r);
        }
    }
    scripts(command, cwd).into_iter().find_map(|(f, text)| {
        device_install(&text)
            .filter(|_| in_scope(&text) || in_scope(cwd))
            .map(|w| device_reason(w, Some(&f)))
    })
}

/// A Lucina repo's main checkout (`…/Projects/Lucina/<Repo>`, not a worktree under `.worktrees`).
fn main_checkout(dir: &str) -> bool {
    Regex::new(r#"(^|/|~)Projects/Lucina/[A-Za-z0-9_.-]+/?$"#)
        .is_ok_and(|r| r.is_match(dir.trim_matches(|c| c == '"' || c == '\'')))
}

/// `git merge` (not `merge-base`, not `--abort`) run in a main checkout: the session's cwd, a
/// `cd` earlier in the command, or `git -C <dir>`.
fn hand_merge_into_main_checkout(command: &str, cwd: &str) -> bool {
    let Ok(split) = Regex::new(r"&&|\|\||;|\n|'|\x22") else {
        return false;
    };
    let Ok(cd) = Regex::new(r"^\s*cd\s+(\S+)") else {
        return false;
    };
    let Ok(merge) = Regex::new(r"\bgit\s+((-C\s+(\S+)|-c\s+\S+)\s+)*merge(\s|$)") else {
        return false;
    };
    let mut dir = cwd.to_string();
    for seg in split.split(command) {
        if let Some(c) = cd.captures(seg) {
            dir = c[1].to_string();
            continue;
        }
        if let Some(m) = merge.captures(seg) {
            let at = m
                .get(3)
                .map(|d| d.as_str().to_string())
                .unwrap_or_else(|| dir.clone());
            if main_checkout(&at) && !seg.contains("--abort") {
                return true;
            }
        }
    }
    false
}

/// One shell word: its unquoted text, and whether a command substitution (`$(…)`, backticks)
/// outside single quotes is part of it.
struct Word {
    val: String,
    subst: bool,
}

/// A command text split into simple commands, plus the bodies of the substitutions in it.
#[derive(Default)]
struct Scan {
    cmds: Vec<Vec<Word>>,
    /// `piped[k]`: command `k` reads the output of command `k - 1` (a single `|` came before it).
    piped: Vec<bool>,
    pipe_next: bool,
    nested: Vec<String>,
    cmd: Vec<Word>,
    cur: String,
    in_word: bool,
    subst: bool,
}

impl Scan {
    fn end_word(&mut self) {
        if self.in_word {
            let val = std::mem::take(&mut self.cur);
            self.cmd.push(Word {
                val,
                subst: self.subst,
            });
        }
        self.in_word = false;
        self.subst = false;
    }

    fn end_cmd(&mut self) {
        self.end_word();
        if !self.cmd.is_empty() {
            let cmd = std::mem::take(&mut self.cmd);
            self.piped.push(std::mem::take(&mut self.pipe_next));
            self.cmds.push(cmd);
        }
    }
}

/// The index of the `)` matching the `(` at `open` (or `ch.len()` when it never closes), quotes respected.
fn match_paren(ch: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < ch.len() {
        match ch[i] {
            '\\' => i += 1,
            '\'' => {
                i += 1;
                while i < ch.len() && ch[i] != '\'' {
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < ch.len() && ch[i] != '"' {
                    if ch[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    ch.len()
}

/// The body of a substitution opened at `open` (`(` of `$(`, or a backtick) and the index after it.
fn substitution(ch: &[char], open: usize) -> (String, usize) {
    if ch[open] == '`' {
        let mut i = open + 1;
        let mut body = String::new();
        while i < ch.len() && ch[i] != '`' {
            if ch[i] == '\\' && i + 1 < ch.len() && ch[i + 1] == '`' {
                i += 1;
            }
            body.push(ch[i]);
            i += 1;
        }
        return (body, i + 1);
    }
    let end = match_paren(ch, open);
    (ch[open + 1..end.min(ch.len())].iter().collect(), end + 1)
}

/// Splits `s` at top level on `;`, `&`, `|`, `(`, `)` and newlines (quotes and escapes respected).
/// `$( … )` and backtick bodies come back in `nested`: they run in the same shell.
fn scan(s: &str) -> Scan {
    let ch: Vec<char> = s.chars().collect();
    let n = ch.len();
    let mut sc = Scan::default();
    let mut i = 0;
    while i < n {
        let c = ch[i];
        match c {
            '\'' => {
                sc.in_word = true;
                i += 1;
                while i < n && ch[i] != '\'' {
                    sc.cur.push(ch[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                sc.in_word = true;
                i += 1;
                while i < n && ch[i] != '"' {
                    if ch[i] == '\\' && i + 1 < n {
                        if !matches!(ch[i + 1], '"' | '\\' | '$' | '`') {
                            sc.cur.push('\\');
                        }
                        sc.cur.push(ch[i + 1]);
                        i += 2;
                    } else if (ch[i] == '$' && i + 1 < n && ch[i + 1] == '(') || ch[i] == '`' {
                        let open = if ch[i] == '`' { i } else { i + 1 };
                        let (body, next) = substitution(&ch, open);
                        sc.cur.push_str("$(…)");
                        sc.nested.push(body);
                        sc.subst = true;
                        i = next;
                    } else {
                        sc.cur.push(ch[i]);
                        i += 1;
                    }
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < n && ch[i + 1] != '\n' {
                    sc.in_word = true;
                    sc.cur.push(ch[i + 1]);
                }
                i += 2;
            }
            '`' => {
                let (body, next) = substitution(&ch, i);
                sc.in_word = true;
                sc.cur.push_str("$(…)");
                sc.nested.push(body);
                sc.subst = true;
                i = next;
            }
            '$' if i + 1 < n && ch[i + 1] == '(' => {
                let (body, next) = substitution(&ch, i + 1);
                sc.in_word = true;
                sc.cur.push_str("$(…)");
                sc.nested.push(body);
                sc.subst = true;
                i = next;
            }
            '#' if !sc.in_word => {
                while i < n && ch[i] != '\n' {
                    i += 1;
                }
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                // A delimiter that flushed no command (a newline after `|`, `(`, the `&` of `|&`)
                // leaves the pipe flag alone; `||` is no pipe.
                let had = !sc.cmd.is_empty() || sc.in_word;
                sc.end_cmd();
                if c == '|' {
                    sc.pipe_next = ch.get(i + 1) != Some(&'|') && (i == 0 || ch[i - 1] != '|');
                } else if had {
                    sc.pipe_next = false;
                }
                i += 1;
            }
            c if c.is_whitespace() => {
                sc.end_word();
                i += 1;
            }
            c => {
                sc.in_word = true;
                sc.cur.push(c);
                i += 1;
            }
        }
    }
    sc.end_cmd();
    sc
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// The index of the program word of a simple command: after `VAR=value`, shell keywords and the
/// wrappers (`sudo`, `nohup`, `setsid`, `exec`, `command`, `env`, `time`, `timeout`, `nice`).
fn program(words: &[Word]) -> Option<usize> {
    let mut i = 0;
    loop {
        let v = words.get(i)?.val.as_str();
        if is_assignment(v)
            || matches!(
                v,
                "{" | "}" | "!" | "then" | "do" | "else" | "elif" | "if" | "while" | "until"
            )
        {
            i += 1;
            continue;
        }
        let base = basename(v);
        i += 1;
        match base {
            "sudo" => {
                while let Some(w) = words.get(i) {
                    if w.val.starts_with('-') {
                        i += if matches!(
                            w.val.as_str(),
                            "-u" | "-g" | "-h" | "-p" | "-C" | "-D" | "-R" | "-T" | "-U"
                        ) {
                            2
                        } else {
                            1
                        };
                    } else if is_assignment(&w.val) {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "nohup" | "setsid" | "exec" | "time" | "command" => {
                while let Some(w) = words.get(i).filter(|w| w.val.starts_with('-')) {
                    // `command -v pkill` only looks the name up.
                    if base == "command" && (w.val.contains('v') || w.val.contains('V')) {
                        return None;
                    }
                    i += 1;
                }
            }
            "env" => {
                while let Some(w) = words.get(i) {
                    if w.val.starts_with('-') {
                        i += if matches!(w.val.as_str(), "-u" | "-C" | "-S") {
                            2
                        } else {
                            1
                        };
                    } else if is_assignment(&w.val) {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            "timeout" => {
                while let Some(w) = words.get(i).filter(|w| w.val.starts_with('-')) {
                    i += if matches!(w.val.as_str(), "-k" | "-s") {
                        2
                    } else {
                        1
                    };
                }
                i += 1; // the duration
            }
            "nice" => {
                if let Some(w) = words.get(i).filter(|w| w.val.starts_with('-')) {
                    i += if w.val == "-n" { 2 } else { 1 };
                }
            }
            _ => return Some(i - 1),
        }
    }
}

fn kill_reason(what: &str) -> String {
    format!(
        "this would kill processes by pattern on the owner's Mac (`{what}`). On the Mac kill only a process id you started \
         (`kill <pid>`); if you meant nezopt, put the command inside `ssh nezopt '…'`."
    )
}

fn local_kill_in(text: &str, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let sc = scan(text);
    for words in &sc.cmds {
        let Some(p) = program(words) else { continue };
        let name = basename(&words[p].val);
        let args = &words[p + 1..];
        let found = match name {
            "pkill" | "killall" => Some(kill_reason(name)),
            "xargs" => {
                let mut j = 0;
                while let Some(w) = args.get(j).filter(|w| w.val.starts_with('-')) {
                    j += if matches!(
                        w.val.as_str(),
                        "-n" | "-I" | "-P" | "-L" | "-s" | "-d" | "-E" | "-a" | "-J"
                    ) {
                        2
                    } else {
                        1
                    };
                }
                let rest = args.get(j..).unwrap_or_default();
                program(rest).and_then(|q| {
                    let target = basename(&rest[q].val);
                    if matches!(target, "kill" | "pkill" | "killall") {
                        return Some(kill_reason(&format!("xargs {target}")));
                    }
                    // `xargs sh -c 'pkill …'`
                    if matches!(target, "bash" | "sh" | "zsh") {
                        return shell_script(&rest[q + 1..])
                            .and_then(|s| local_kill_in(s, depth + 1));
                    }
                    None
                })
            }
            "kill" => args
                .iter()
                .any(|a| a.subst)
                .then(|| kill_reason("kill $(…)")),
            "bash" | "sh" | "zsh" => shell_script(args).and_then(|s| local_kill_in(s, depth + 1)),
            // Remote text is not local: ssh, scp and rsync arguments are not read.
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    sc.nested
        .iter()
        .find_map(|inner| local_kill_in(inner, depth + 1))
}

/// A here-document fed to `ssh` is remote text: drop its body (the others `without_heredocs` handled).
fn without_remote_heredocs(command: &str) -> String {
    let (Ok(start), Ok(remote)) = (
        Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)"#),
        Regex::new(r"(^|[\s;&|(])(ssh|scp|rsync)\s"),
    ) else {
        return command.to_string();
    };
    let mut out = String::new();
    let mut until: Option<String> = None;
    for line in command.split('\n') {
        if let Some(tag) = &until {
            if line.trim().trim_start_matches('\t') == tag {
                until = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(c) = start.captures(line) {
            let before = &line[..c.get(0).map(|m| m.start()).unwrap_or(0)];
            if remote.is_match(before) {
                until = Some(c[1].to_string());
            }
        }
    }
    out
}

/// Why the LOCAL shell running `command` would kill processes by pattern, if it would: `pkill`,
/// `killall`, `xargs kill`, `kill $(pgrep …)`, also in `$( )`, backticks, groups and `bash -c`.
/// Text for another host (`ssh host '…'`) is not read. Incident 2026-11-11: `pkill -f` in the
/// Mac shell instead of on nezopt stopped about 200 of the owner's processes.
pub fn local_kill(command: &str) -> Option<String> {
    local_kill_in(&without_remote_heredocs(&without_heredocs(command)), 0)
}

/// The pattern-kill check as wired in: on the Mac only, every session, bypassable like the rest.
fn mac_kill_verdict(command: &str, is_mac: bool, bypass: bool) -> Option<String> {
    if !is_mac || bypass {
        return None;
    }
    local_kill(command)
}

fn target_reason(what: &str, path: &str) -> String {
    format!(
        "this deletes single files under a cargo target directory (`{what}`, `{path}`). Cargo does not notice a deleted \
         dependency output while its `.fingerprint` says fresh, so the next build breaks (HOST-12). Instead delete a whole \
         profile or target (`rm -rf <target>/<profile>` or `rm -rf <target>`), run `cargo clean` (or `cargo clean -p <crate>`), \
         or, once LB's rewrite lands, a by-unit sweep (not by file age)."
    )
}

/// `word` as an absolute, lexically normalised path: a leading `~/` and the text `$HOME` are
/// expanded, a relative word is joined on `dir`. Any other `$VAR` (`$CARGO_TARGET_DIR`) and
/// `$(…)` stay unexpanded, so they fail open here exactly as in every other rule of this hook.
fn resolve_path(word: &str, dir: &str, home: &str) -> String {
    let w = word.replace("${HOME}", home).replace("$HOME", home);
    let w = if w == "~" {
        home.to_string()
    } else if let Some(rest) = w.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        w
    };
    let joined = if w.starts_with('/') {
        w
    } else {
        format!("{dir}/{w}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Whether `path` is a cargo target directory or lies inside one: a component named `target` or
/// `target-*`, anything under `.cache/cargo-target`, the gate's own `lucina-check-target`, and
/// (`on_host` only: the Linux binary on the machine owning the disk) a directory holding both
/// `CACHEDIR.TAG` and `.rustc_info.json`. Every direct child of a target is a profile.
fn under_target(path: &str, on_host: bool) -> bool {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if comps.iter().any(|c| target_component(c)) {
        return true;
    }
    if !on_host {
        return false;
    }
    let mut cur = std::path::PathBuf::from("/");
    let marked = |d: &std::path::Path| {
        d.join("CACHEDIR.TAG").is_file() && d.join(".rustc_info.json").is_file()
    };
    if marked(&cur) {
        return true;
    }
    for c in comps {
        if c.contains(['*', '?', '[']) {
            break;
        }
        cur.push(c);
        if marked(&cur) {
            return true;
        }
    }
    false
}

fn has_glob(word: &str) -> bool {
    word.contains(['*', '?', '['])
}

/// `ssh [options] host command…` as the text the remote shell runs (a lone word as it is).
fn remote_text(args: &[Word]) -> Option<Vec<String>> {
    let mut j = 0;
    while let Some(w) = args.get(j).filter(|w| w.val.starts_with('-')) {
        j += if matches!(
            w.val.as_str(),
            "-B" | "-b"
                | "-c"
                | "-D"
                | "-E"
                | "-e"
                | "-F"
                | "-I"
                | "-i"
                | "-J"
                | "-L"
                | "-l"
                | "-m"
                | "-O"
                | "-o"
                | "-p"
                | "-Q"
                | "-R"
                | "-S"
                | "-W"
                | "-w"
        ) {
            2
        } else {
            1
        };
    }
    let rest = args.get(j + 1..).filter(|r| !r.is_empty())?; // after the host
    if let [one] = rest {
        return Some(vec![one.val.clone()]);
    }
    let quote = |w: &str| {
        if !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,~".contains(c))
        {
            w.to_string()
        } else {
            format!("'{}'", w.replace('\'', "'\\''"))
        }
    };
    // ssh joins its words with spaces and the remote shell parses them again; read both that and the words as given.
    Some(vec![
        rest.iter()
            .map(|w| w.val.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        rest.iter()
            .map(|w| quote(&w.val))
            .collect::<Vec<_>>()
            .join(" "),
    ])
}

/// Whether `text` runs `rm`, also inside `sh -c '…'` and `$( )`.
fn runs_rm(text: &str, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let sc = scan(text);
    sc.cmds.iter().any(|w| {
        program(w).is_some_and(|p| {
            let name = basename(&w[p].val);
            name == "rm"
                || (matches!(name, "bash" | "sh" | "zsh")
                    && shell_script(&w[p + 1..]).is_some_and(|s| runs_rm(s, depth + 1)))
        })
    }) || sc.nested.iter().any(|n| runs_rm(n, depth + 1))
}

/// A path word with a component that names a cargo target, judged by its text alone (no stat).
fn textual_target(word: &str) -> bool {
    word.split('/').any(target_component)
}

/// A path component that names a cargo target: `target`, `target-*`, `cargo-target` (the
/// `.cache/cargo-target` of the pipeline) or the gate's `lucina-check-target`. One list for `under_target`
/// and `textual_target`.
fn target_component(c: &str) -> bool {
    matches!(c, "target" | "cargo-target" | "lucina-check-target") || c.starts_with("target-")
}

/// The script of `sh -c '…'` (after the shell's own short options), if the arguments have one.
fn shell_script(args: &[Word]) -> Option<&str> {
    let mut j = 0;
    while let Some(w) = args.get(j) {
        if w.val.starts_with('-') && !w.val.starts_with("--") {
            if w.val.contains('c') {
                return args.get(j + 1).map(|s| s.val.as_str());
            }
            j += 1;
        } else {
            break;
        }
    }
    None
}

/// Why `command` deletes single files under a cargo target directory, if it does: `find … -delete`,
/// `find … -exec rm`, `… | xargs rm`, `rm` with a glob; also inside `$( )`, `bash -c` and the command
/// text of `ssh host '…'` (run in the remote home). A `cd` carries to the commands after it.
/// Whole profiles and targets (`rm -rf <target>/debug`) and `cargo clean` pass. Not seen: variables
/// (`$CARGO_TARGET_DIR`), `$(…)` paths, `python -c`, `perl -e`, `make clean`, `pushd`,
/// `env -C`, `find -path` forms, and the remote text of `scp` and `rsync`.
fn target_deletion(command: &str, cwd: &str, on_host: bool, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let text = if depth == 0 {
        without_heredocs(command)
    } else {
        command.to_string()
    };
    let sc = scan(&text);
    let mut dir = cwd.to_string();
    let hit = |w: &str, dir: &str| {
        let p = resolve_path(w, dir, &home);
        under_target(&p, on_host).then_some(p)
    };
    for (idx, words) in sc.cmds.iter().enumerate() {
        let Some(p) = program(words) else { continue };
        let name = basename(&words[p].val);
        let args = &words[p + 1..];
        let found = match name {
            "cd" => {
                match args.iter().find(|a| a.val != "--").map(|a| a.val.as_str()) {
                    Some("-") => {}
                    Some(d) => dir = resolve_path(d, &dir, &home),
                    None => dir = home.clone(),
                }
                None
            }
            "find" => {
                let how = args
                    .iter()
                    .enumerate()
                    .find_map(|(i, a)| match a.val.as_str() {
                        "-delete" => Some("find … -delete"),
                        "-exec" | "-execdir" | "-ok" | "-okdir"
                            if args.get(i + 1).is_some_and(|n| basename(&n.val) == "rm") =>
                        {
                            Some("find … -exec rm")
                        }
                        _ => None,
                    });
                how.and_then(|how| {
                    let mut j = 0;
                    while args
                        .get(j)
                        .is_some_and(|w| matches!(w.val.as_str(), "-H" | "-L" | "-P"))
                    {
                        j += 1;
                    }
                    let roots: Vec<&str> = args[j..]
                        .iter()
                        .take_while(|w| {
                            !w.val.starts_with('-') && !matches!(w.val.as_str(), "!" | "(")
                        })
                        .map(|w| w.val.as_str())
                        .collect();
                    let roots = if roots.is_empty() { vec!["."] } else { roots };
                    roots
                        .into_iter()
                        .find_map(|r| hit(r, &dir))
                        .map(|path| target_reason(how, &path))
                })
            }
            "rm" => args
                .iter()
                .filter(|a| !a.val.starts_with('-') && has_glob(&a.val))
                .find_map(|a| hit(&a.val, &dir))
                .map(|path| target_reason("rm with a glob", &path)),
            "xargs" => {
                let mut j = 0;
                while let Some(w) = args.get(j).filter(|w| w.val.starts_with('-')) {
                    j += if matches!(
                        w.val.as_str(),
                        "-n" | "-I" | "-P" | "-L" | "-s" | "-d" | "-E" | "-a" | "-J"
                    ) {
                        2
                    } else {
                        1
                    };
                }
                let rest = args.get(j..).unwrap_or_default();
                program(rest).and_then(|q| {
                    let prog = basename(&rest[q].val);
                    let script = shell_script(&rest[q + 1..])
                        .filter(|_| matches!(prog, "bash" | "sh" | "zsh"));
                    // `xargs rm`, or `xargs sh -c '… rm …'`.
                    let is_rm = prog == "rm" || script.is_some_and(|s| runs_rm(s, 0));
                    if !is_rm && script.is_none() {
                        return None;
                    }
                    // The files come down the pipe: any earlier stage of this pipeline, the directory, or
                    // its own operands. The first 64 stages are classified as paths (that may stat); the
                    // stages before them are read textually, and the walk stops after 10 000 stages.
                    let mut k = idx;
                    let mut near: Vec<&Word> = Vec::new();
                    let mut far: Vec<&Word> = Vec::new();
                    while k > 0 && sc.piped[k] && idx - k < 10_000 {
                        k -= 1;
                        let stage = sc.cmds[k].iter().skip(1);
                        if idx - k <= 64 {
                            near.extend(stage);
                        } else {
                            far.extend(stage);
                        }
                    }
                    let near_hit = near
                        .into_iter()
                        .chain(rest[q + 1..].iter())
                        .filter(|w| !w.val.starts_with('-'))
                        .find_map(|w| hit(&w.val, &dir));
                    let far_hit = far
                        .into_iter()
                        .filter(|w| !w.val.starts_with('-'))
                        .find(|w| textual_target(&w.val))
                        .map(|w| w.val.clone());
                    // `xargs rm` (or a script that runs rm) fed from a target: refused.
                    if is_rm {
                        let fed = near_hit
                            .clone()
                            .or_else(|| far_hit.clone())
                            .or_else(|| hit(".", &dir));
                        if let Some(path) = fed {
                            return Some(target_reason("… | xargs rm", &path));
                        }
                    }
                    // A shell script is also read on its own, from the real directory (a target path
                    // or a `cd` inside it), and then from the target path the stages name (for `{}`
                    // operands). `is_rm` never skips this.
                    script.and_then(|s| {
                        target_deletion(s, &dir, on_host, depth + 1).or_else(|| {
                            near_hit
                                .or(far_hit)
                                .and_then(|p| target_deletion(s, &p, on_host, depth + 1))
                        })
                    })
                })
            }
            "bash" | "sh" | "zsh" => {
                shell_script(args).and_then(|s| target_deletion(s, &dir, on_host, depth + 1))
            }
            "ssh" => remote_text(args).and_then(|ts| {
                ts.iter()
                    .find_map(|t| target_deletion(t, &home, on_host, depth + 1))
            }),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    sc.nested
        .iter()
        .find_map(|inner| target_deletion(inner, &dir, on_host, depth + 1))
}

/// The cargo-target deletion check as wired in: every session, bypassable like the rest.
fn target_verdict(command: &str, cwd: &str, on_host: bool, bypass: bool) -> Option<String> {
    if bypass {
        return None;
    }
    target_deletion(command, cwd, on_host, 0)
}

/// One `bypass.log` line: session, cwd, `honoured` or `denied`, the command. Control characters
/// (tab, CR, newline), U+2028/2029 and the format and bidi characters (see `plain`) become spaces and each field is cut at 2000 characters, so it stays one
/// bounded line.
fn bypass_log_line(session: &str, cwd: &str, honoured: bool, command: &str) -> String {
    let clean = |s: &str| plain(s, 2000);
    format!(
        "{}\t{}\t{}\t{}\n",
        clean(session),
        clean(cwd),
        if honoured { "honoured" } else { "denied" },
        clean(command)
    )
}

/// Rotates `bypass.log` once it is over 1 MiB: it becomes `bypass.log.1` (the old `.1` is replaced)
/// and the next line starts a new file. Only a regular `bypass.log` (not a symlink or FIFO, in a state
/// directory that is not a symlink) is renamed, and never onto a non-regular `.1`. Any failure is
/// ignored: it never changes a verdict.
fn rotate_log(path: &std::path::Path) {
    if path
        .parent()
        .is_some_and(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    {
        return;
    }
    let Slot::Regular(m) = slot(path) else {
        return;
    };
    if m.len() <= 1 << 20 {
        return;
    }
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    let old = std::path::PathBuf::from(old);
    if matches!(slot(&old), Slot::Other(_)) {
        return;
    }
    let _ = std::fs::rename(path, old);
}

/// Phase switch for the coordinator-only bypass. `false` (phase 1): an honoured override by a session that
/// is not a listed coordinator, or by a subagent, is only recorded as `would-refuse:…` in the ledger.
/// `true` (phase 2, the coordinator's word): it is denied after the ledger line is written.
const ENFORCE_IDENTITY: bool = false;

/// Control characters (tab, CR, newline, U+0085), U+2028/2029, the format and bidi characters U+200B-200F, U+202A-202E, U+2060-2064, U+2066-2069 and U+FEFF become spaces; cut by characters.
fn plain(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '\u{2028}' | '\u{2029}' | '\u{85}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
                )
            {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// `plain`, with `-` for an empty field.
fn field(s: &str, max: usize) -> String {
    let t = plain(s, max);
    if t.is_empty() {
        "-".to_string()
    } else {
        t
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch (civil-from-days, no dependency).
fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The literal value of the first honoured `LAGENT_BYPASS=` marker, read by the same word reader and
/// with the same rules as `bypass_given` (so it is `Some` exactly when `bypass_given` is true).
fn bypass_reason(command: &str) -> Option<String> {
    const KEY: &str = "LAGENT_BYPASS=";
    let mut rest = command;
    for _ in 0..32 {
        let at = rest.find(KEY)?;
        let inside_name = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let (value, tail) = bypass_value(&rest[at + KEY.len()..]);
        rest = tail;
        if let Some(v) = value {
            if !inside_name && v.chars().any(|c| !c.is_whitespace()) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// The columns of one `bypass-ledger.tsv` line.
struct Entry<'a> {
    time: &'a str,
    host: &'a str,
    session: &'a str,
    app: &'a str,
    agent: &'a str,
    status: &'a str,
    reason: &'a str,
    command: &'a str,
}

/// One ledger line: time, host, session id, app id, agent id, identity status, reason (200 characters),
/// command (120 characters). Every field is one line of plain text; empty ones are `-`.
fn ledger_line(e: &Entry) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        field(e.time, 40),
        field(e.host, 20),
        field(e.session, 200),
        field(e.app, 200),
        field(e.agent, 200),
        field(e.status, 40),
        field(e.reason, 200),
        field(e.command, 120)
    )
}

/// The coordinator list: one id per line; blank lines and `#` comments (whole-line or after an id) are
/// ignored; a leading BOM and a bad byte are tolerated. Only a regular file, read in at most 64 KiB, counts:
/// a missing, unreadable, FIFO or symlinked file is an empty list. The guard never writes this file.
fn read_coordinators(dir: &std::path::Path) -> Vec<String> {
    let path = dir.join("coordinators");
    if !matches!(slot(&path), Slot::Regular(_)) {
        return Vec::new();
    }
    let Ok(f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if f.take(64 * 1024 + 1).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    if bytes.len() > 64 * 1024 {
        // Longer than the bound: the last line is cut, so it is dropped (a cut id must not match a shorter one).
        bytes.truncate(64 * 1024);
        match bytes.iter().rposition(|&b| b == b'\n') {
            Some(i) => bytes.truncate(i),
            None => bytes.clear(),
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let text: &str = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines()
        .map(|l| l.split('#').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `coordinator`, `would-refuse:subagent` (an `agent_id` is set, even under a listed parent) or
/// `would-refuse:not-listed`.
fn identity_status(list: &[String], session: &str, app: Option<&str>, agent: &str) -> &'static str {
    if !agent.is_empty() {
        return "would-refuse:subagent";
    }
    let listed = |id: &str| !id.is_empty() && list.iter().any(|l| l == id);
    if listed(session) || app.is_some_and(listed) {
        "coordinator"
    } else {
        "would-refuse:not-listed"
    }
}

/// What is at a state path, looked at without following a symlink.
enum Slot {
    Absent,
    Regular(std::fs::Metadata),
    Other(&'static str),
}

fn slot(path: &std::path::Path) -> Slot {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Slot::Other("ledger path is a symlink"),
        Ok(m) if m.is_file() => Slot::Regular(m),
        Ok(_) => Slot::Other("not a regular file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Slot::Absent,
        Err(_) => Slot::Other("not accessible"),
    }
}

/// Appends one whole line to `dir/name` (created 0600, `O_APPEND`, one `write_all`, then `sync_all`).
/// A symlinked state directory (its last component only), or a symlink, FIFO or directory at the file, is
/// refused (`Err` names why). After the open the file must still be a regular file with one link and the
/// same (dev, ino) as the checked path (only when it existed before the open; a file created in the gap
/// is not compared), else it is refused: a hard link to a foreign file never gets text, and a swap between
/// the check and the open is narrowed, not removed. A file with group/other bits is set to 0600 on a best
/// effort basis (after those checks, so a foreign inode is never chmod-ed).
fn append_line(dir: &std::path::Path, name: &str, line: &str) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let kind = |e: std::io::Error| e.kind().to_string();
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("state directory is a symlink".to_string());
    }
    std::fs::create_dir_all(dir).map_err(kind)?;
    let path = dir.join(name);
    let before = match slot(&path) {
        Slot::Other(why) => return Err(why.to_string()),
        Slot::Regular(m) => Some((m.dev(), m.ino())),
        Slot::Absent => None,
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(kind)?;
    let m = f.metadata().map_err(kind)?;
    if !m.is_file() {
        return Err("not a regular file".to_string());
    }
    if m.nlink() > 1 || before.is_some_and(|b| b != (m.dev(), m.ino())) {
        return Err("ledger has several links or was swapped".to_string());
    }
    if m.permissions().mode() & 0o077 != 0 {
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(line.as_bytes()).map_err(kind)?;
    f.sync_all().map_err(kind)
}

/// What the verdict needs besides the command: the state directory, the clock, the platform, the app id
/// (`CLAUDE_CODE_HOST_SESSION_ID`) and whether identity is enforced.
struct Ctx<'a> {
    dir: &'a std::path::Path,
    now: u64,
    is_mac: bool,
    app_id: Option<&'a str>,
    enforce: bool,
}

fn rule_denial(reason: &str) -> String {
    format!("pipeline-guard: {reason} (Owner-approved exception: put LAGENT_BYPASS=<reason> in the command.)")
}

/// The full deny text for a hook call, or `None` to allow. A rule's own verdict (bypass honoured by the
/// rule or not) is exactly as before. A bypass that flips a verdict (the verdict with the answer forced
/// to false is a denial, the real one is not) is an honoured override: one ledger line first (a failed
/// write denies), then allowed, or denied when `enforce` and the identity would be refused.
fn judge(v: &Value, command: &str, cwd: &str, ctx: &Ctx) -> Option<String> {
    let bypass = bypass_given(command);
    if let Some(reason) = decide_with(command, cwd, ctx.is_mac, bypass) {
        return Some(rule_denial(&reason));
    }
    if !bypass || decide_with(command, cwd, ctx.is_mac, false).is_none() {
        return None;
    }
    let session = v["session_id"].as_str().unwrap_or_default();
    let agent = v["agent_id"].as_str().unwrap_or_default();
    let list = read_coordinators(ctx.dir);
    let status = identity_status(&list, session, ctx.app_id, agent);
    let time = utc_stamp(ctx.now);
    let reason = bypass_reason(command).unwrap_or_default();
    let line = ledger_line(&Entry {
        time: &time,
        host: if ctx.is_mac { "mac" } else { "nezopt" },
        session,
        app: ctx.app_id.unwrap_or_default(),
        agent,
        status,
        reason: &reason,
        command,
    });
    if let Err(kind) = append_line(ctx.dir, "bypass-ledger.tsv", &line) {
        return Some(format!(
            "pipeline-guard: bypass not recorded ({kind}): {} is not writable or not a regular ledger; tell the coordinator",
            ctx.dir.display()
        ));
    }
    if ctx.enforce && status != "coordinator" {
        let who = if status == "would-refuse:subagent" {
            "a subagent may not bypass"
        } else {
            "this session is not on the coordinator list"
        };
        return Some(format!(
            "pipeline-guard: LAGENT_BYPASS is for the coordinator only: {who}. Ask the coordinator to run the command (or to add the session to ~/.local/state/pipeline-guard/coordinators)."
        ));
    }
    None
}

/// Per class (`agent_id=present`, `agent_id=absent`) at most this many capture lines are written.
const CAPTURE_CLASS_MAX: usize = 50;

/// Appends one line to `payload-keys.tsv` for a PreToolUse payload: time, the sorted top-level key names,
/// and whether `agent_id` is a non-empty string. Never a value, never a nested key. Writes nothing once
/// the class has its 50 lines or the file is over 64 KiB. A failure changes nothing else.
fn capture_keys(dir: &std::path::Path, now: u64, v: &Value) {
    let _ = try_capture(dir, now, v);
}

fn try_capture(dir: &std::path::Path, now: u64, v: &Value) -> Option<()> {
    // Once both classes are full the sentinel is the only thing a call looks at.
    let sentinel = "payload-keys.done";
    if std::fs::symlink_metadata(dir.join(sentinel)).is_ok() {
        return None;
    }
    match v["hook_event_name"].as_str() {
        None | Some("PreToolUse") => {}
        Some(_) => return None,
    }
    let obj = v.as_object()?;
    let tag = if v["agent_id"].as_str().is_some_and(|a| !a.is_empty()) {
        "agent_id=present"
    } else {
        "agent_id=absent"
    };
    let path = dir.join("payload-keys.tsv");
    // Only a regular file of at most 64 KiB is read, and only through a bounded read.
    let seen = match slot(&path) {
        Slot::Other(_) => return None,
        Slot::Regular(m) if m.len() > 64 * 1024 => return None,
        Slot::Regular(_) => {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .ok()?
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
        Slot::Absent => String::new(),
    };
    let count = |t: &str| seen.lines().filter(|l| l.ends_with(t)).count();
    let (mine, other) = if tag == "agent_id=present" {
        (count(tag), count("agent_id=absent"))
    } else {
        (count(tag), count("agent_id=present"))
    };
    if mine >= CAPTURE_CLASS_MAX {
        if other >= CAPTURE_CLASS_MAX {
            let _ = append_line(dir, sentinel, "done\n");
        }
        return None;
    }
    let mut keys: Vec<String> = obj.keys().map(|k| plain(k, 64)).collect();
    keys.sort();
    let line = format!(
        "{}\t{}\t{tag}\n",
        utc_stamp(now),
        field(&keys.join(","), 1000)
    );
    append_line(dir, "payload-keys.tsv", &line).ok()?;
    if mine + 1 >= CAPTURE_CLASS_MAX && other >= CAPTURE_CLASS_MAX {
        let _ = append_line(dir, sentinel, "done\n");
    }
    Some(())
}

/// Records one attempt in `bypass.log`, best effort: through the checked append (a symlink, FIFO,
/// directory or hard link at the file, or a symlinked state directory, skips the write silently), after a
/// rotation check. A planted object never changes a verdict or blocks; a swap between the check and the open could still block.
fn log_attempt(dir: &std::path::Path, session: &str, cwd: &str, honoured: bool, command: &str) {
    rotate_log(&dir.join("bypass.log"));
    let _ = append_line(
        dir,
        "bypass.log",
        &bypass_log_line(session, cwd, honoured, command),
    );
}

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("session-hook") {
        std::process::exit(session::run(&input));
    }
    let Ok(v) = serde_json::from_str::<Value>(&input) else {
        return;
    };
    let command = v["tool_input"]["command"].as_str().unwrap_or_default();
    let cwd = v["cwd"].as_str().unwrap_or_default();
    let bypass = bypass_given(command);
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = format!("{home}/.local/state/pipeline-guard");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    capture_keys(std::path::Path::new(&dir), now, &v);
    if command.contains("LAGENT_BYPASS=") {
        // Every occurrence is logged, honoured or denied, so a blank attempt leaves a record too.
        log_attempt(
            std::path::Path::new(&dir),
            v["session_id"].as_str().unwrap_or("?"),
            cwd,
            bypass,
            command,
        );
    }
    let app_id = std::env::var("CLAUDE_CODE_HOST_SESSION_ID").ok();
    let ctx = Ctx {
        dir: std::path::Path::new(&dir),
        now,
        is_mac: cfg!(target_os = "macos"),
        app_id: app_id.as_deref(),
        enforce: ENFORCE_IDENTITY,
    };
    if let Some(text) = judge(&v, command, cwd, &ctx) {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": text,
            }
        });
        println!("{out}");
    }
}

#[cfg(test)]
mod tests {
    use super::{decide, decide_on, local_kill, verdict, without_heredocs};

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
        assert!(decide(
            "xcrun devicectl device install app --device X /tmp/s/Secretary.app",
            "/Users/g"
        )
        .is_some());
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
        assert!(decide(
            "xcrun devicectl device install app --device X build/CardRails.app",
            "/Users/g/Projects/CardRails-iOS"
        )
        .is_none());
    }

    #[test]
    fn installs_inside_a_script_count() {
        let dir = std::env::temp_dir().join(format!("pipeline-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("deploy.sh");
        std::fs::write(
            &bad,
            "#!/bin/bash\nxcrun devicectl device install app --device X ios/build/Secretary.app\n",
        )
        .unwrap();
        let fine = dir.join("test.sh");
        std::fs::write(&fine, "#!/bin/bash\ncargo test -p secretary-core\n").unwrap();
        assert!(decide(&format!("bash {}", bad.display()), LUCINA)
            .unwrap()
            .contains("deploy.sh"));
        assert!(
            decide(&format!("bash {}", bad.display()), "/tmp").is_some(),
            "the script names Secretary"
        );
        assert!(decide(&format!("bash {}", fine.display()), LUCINA).is_none());
        std::fs::remove_file(bad).unwrap();
        std::fs::remove_file(fine).unwrap();
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn worktrees_go_through_task_start() {
        assert!(verdict("git worktree add ../x -b x", LUCINA).is_some());
        assert!(verdict(
            "ssh nezopt 'cd ~/Projects/Lucina/SecretaryIOS && wt add ig-fix origin/main'",
            "/Users/g"
        )
        .is_some());
        assert!(verdict(
            "ssh nezopt 'cd ~/Projects/Lucina/lucinaos && git -C . worktree add /tmp/x'",
            "/Users/g"
        )
        .is_some());
        assert!(verdict("git worktree list", LUCINA).is_none());
        // A detached tree for a deploy or a test run is not a task.
        assert!(verdict(
            "git worktree add --detach /tmp/deploy-x origin/main",
            LUCINA
        )
        .is_none());
        assert!(verdict(
            "git -C ~/Projects/Lucina/SecretaryIOS worktree add -q /tmp/x -d abc",
            "/Users/g"
        )
        .is_none());
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
        assert!(verdict(
            "ssh nezopt 'cd ~/Projects/.worktrees/SecretaryIOS/x && git push origin HEAD:main'",
            "/Users/g"
        )
        .is_some());
        assert!(verdict("git push origin my-branch", LUCINA).is_none());
        assert!(verdict("git push origin main-fix", LUCINA).is_none());
        assert!(verdict("git push origin deploy/web/20261101T1200Z-abc", LUCINA).is_none());
        assert!(verdict("git push --dry-run origin HEAD:main", LUCINA).is_none());
        assert!(verdict("git log origin/main", LUCINA).is_none());
    }

    #[test]
    fn main_checkouts_move_only_through_the_queue() {
        let android = "/home/nez/Projects/Lucina/LucinaAndroidOS";
        assert!(verdict(
            "timeout 1500 flock /tmp/lucina-merge.lock git merge --ff-only gbgo-m2",
            android
        )
        .is_some());
        assert!(verdict(
            "ssh nezopt 'cd ~/Projects/Lucina/lucinaos && git merge --ff-only gbgo-ota'",
            "/Users/g"
        )
        .is_some());
        assert!(verdict(
            "git -C /home/nez/Projects/Lucina/LucinaAndroidOS merge topic",
            "/tmp"
        )
        .is_some());
        // In a worktree (resolving a CONFLICT the queue reported) or read-only: fine.
        assert!(verdict(
            "cd ~/Projects/.worktrees/LucinaAndroidOS/gbgo-m2 && git merge main",
            "/Users/g"
        )
        .is_none());
        assert!(verdict("git merge-base --is-ancestor a b", android).is_none());
        assert!(verdict("git merge --abort", android).is_none());
        assert!(verdict("git log main", android).is_none());
    }

    /// The lead sessions' cwd: the group folder (Mac mount, nezopt, trailing slash).
    const GROUPS: [&str; 3] = [
        "/Users/giuseppe/mnt/nezopt/Projects/Lucina",
        "/home/nez/Projects/Lucina",
        "/home/nez/Projects/Lucina/",
    ];

    #[test]
    fn scratch_repos_are_not_lucina_repos() {
        // LA-C045 (HOST-18): a guarded call acting on a scratch directory is none of the pipeline's business,
        // even when the session sits in the group folder.
        let cmds = [
            "ssh nezopt 'R=$HOME/scratch/tmp/t1; mkdir -p $R && git init -q $R/main && git -C $R/main worktree add -q $R/wt1'",
            r#"ssh nezopt "R=$HOME/scratch/tmp/t2; mkdir -p $R && git init -q $R/main && git -C $R/main worktree add -q $R/wt2""#,
            "bash -c 'R=$HOME/scratch/tmp/t3; git init -q $R/main && git -C $R/main worktree add -q $R/wt3'",
            r#"sh -c "R=$HOME/scratch/tmp/t4; git init -q $R/main && git -C $R/main worktree add -q $R/wt4""#,
            "bash <<< 'R=$HOME/scratch/tmp/t5; git init -q $R/main && git -C $R/main worktree add -q $R/wt5'",
            "cd $HOME/scratch/x && git worktree add ../y",
            "git -C /home/nez/scratch/a worktree add /home/nez/scratch/b",
            "cd ~/scratch/a && wt add b",
            "cd ~/scratch/bare-test && git push origin main",
            "git init -q ~/scratch/n && cd ~/scratch/n && git commit -q --allow-empty -m init && git worktree add ../n2",
            r#"echo "notes about SecretaryIOS"; git -C ~/scratch/z worktree add ../zz"#,
            r#"cd ~/scratch/z && git commit -q -m "mentions Projects/Lucina/SecretaryIOS" && git push origin main"#,
            "cd ~/scratch/x && git merge x",
            "ssh -o ConnectTimeout=8 nezopt 'cd ~/scratch/q && git worktree add ../q2'",
            "cd /home/nez/scratch/m && git worktree add -b topic ../m2 origin/main",
            "ssh nezopt 'cd $HOME/scratch/tmp/r && git push origin HEAD:main'",
            "cd ~/scratch/a && git -C ../b push origin main",
            "sh -c 'cd /home/nez/scratch/w && wt add v'",
            "cd ~/scratch/a ; wt add b ; cd ~/scratch/c ; git push origin main",
            // Accepted fail-opens: the directory is held in a variable or a substitution.
            "R=/home/nez/scratch/k; git -C $R worktree add ../k2",
            "cd $DIR && git worktree add ../x",
            r#"git -C "$(mktemp -d)" worktree add ../x"#,
        ];
        for cwd in GROUPS {
            for c in cmds {
                assert_eq!(verdict(c, cwd), None, "{cwd}: {c}");
            }
        }
    }

    #[test]
    fn a_real_lucina_repo_is_still_guarded_from_the_group_folder() {
        let cmds = [
            "cd /home/nez/Projects/Lucina/SecretaryIOS && git worktree add ../x",
            "ssh nezopt 'cd /home/nez/Projects/Lucina/SecretaryIOS && git worktree add ../x'",
            r#"ssh -o ConnectTimeout=8 nezopt "cd /home/nez/Projects/Lucina/SecretaryIOS && git worktree add ../x""#,
            "bash -c 'cd /home/nez/Projects/Lucina/SecretaryIOS && git worktree add ../x'",
            r#"sh -c "cd /home/nez/Projects/Lucina/SecretaryIOS && git worktree add ../x""#,
            "git -C /home/nez/Projects/Lucina/SecretaryIOS worktree add ../x",
            "git -C ~/Projects/Lucina/LucinaOS push origin HEAD:main",
            "git -C /Users/giuseppe/mnt/nezopt/Projects/Lucina/SecretaryIOS worktree add ../x",
            "cd /Users/giuseppe/mnt/nezopt/Projects/Lucina/SecretaryIOS && wt add x",
            "cd ~/Projects/Lucina/SecretaryIOS && git merge x",
            "git worktree add /home/nez/Projects/.worktrees/SecretaryIOS/x",
            "git -C '/home/nez/Projects/Lucina/SecretaryIOS' worktree add ../x",
            "git worktree add '/home/nez/Projects/.worktrees/SecretaryIOS/x'",
            "cd '~/Projects/Lucina/lucinaos' && git push origin HEAD:main",
            "ssh nezopt 'git -C ~/Projects/Lucina/SecretaryIOS push origin HEAD:main'",
            "cd ~/Projects/.worktrees/SecretaryIOS/a && git push origin main",
            // A call the scan cannot place falls back to the old whole-text rule.
            "echo x | xargs git worktree add ../x # SecretaryIOS",
        ];
        for cwd in GROUPS {
            for c in cmds {
                assert!(verdict(c, cwd).is_some(), "{cwd}: {c}");
            }
        }
        // Inside a repo or a worktree the cwd decides, also for a path held in a variable.
        let inside = "/home/nez/Projects/Lucina/SecretaryIOS";
        let tree = "/home/nez/Projects/.worktrees/SecretaryIOS/foo";
        for (c, cwd) in [
            ("git worktree add ../y", inside),
            ("wt add z", tree),
            ("git -C $R worktree add ../y", inside),
            ("cd $DIR && git worktree add ../y", inside),
            ("ssh nezopt 'git push origin main'", inside),
            ("git push origin main", inside),
            ("git push origin main", tree),
            ("git -C $(pwd)/x push origin HEAD:main", tree),
        ] {
            assert!(verdict(c, cwd).is_some(), "{cwd}: {c}");
        }
    }

    #[test]
    fn a_session_inside_a_repo_follows_its_own_cd_and_dash_c() {
        let inside = "/home/nez/Projects/Lucina/SecretaryIOS";
        assert_eq!(
            verdict("cd ~/scratch/a && git worktree add ../b", inside),
            None
        );
        assert_eq!(
            verdict("git -C ~/scratch/a worktree add ../b", inside),
            None
        );
    }

    #[test]
    fn a_bypass_with_a_reason_passes() {
        assert!(verdict(
            "LAGENT_BYPASS=owner-said-so git push origin HEAD:main",
            LUCINA
        )
        .is_none());
        assert!(verdict("LAGENT_BYPASS= git push origin HEAD:main", LUCINA).is_some());
    }

    #[test]
    fn heredoc_bodies_are_data() {
        let doc = "cat > docs/x.md <<'EOF'\nUse lagent task start, not wt add x origin/main or git push origin HEAD:main.\nEOF\ngit status";
        assert!(verdict(doc, LUCINA).is_none());
        let py = "python3 - <<EOF\ns = s.replace('wt add', 'lagent task start')\nEOF";
        assert!(verdict(py, LUCINA).is_none());
        // What runs after the heredoc still counts.
        let after = "cat > a <<'EOF'\nnotes\nEOF\ngit push origin HEAD:main";
        assert!(verdict(after, LUCINA).is_some());
        // A heredoc a shell runs is commands.
        assert!(verdict(
            "bash <<'EOF'\ncd ~/Projects/Lucina/SecretaryIOS && wt add x\nEOF",
            "/Users/g"
        )
        .is_some());
        assert!(verdict(
            "ssh nezopt bash -s <<EOF\ngit push origin HEAD:main\nEOF",
            LUCINA
        )
        .is_some());
        assert_eq!(
            without_heredocs("a <<-END\n\tbody\n\tEND\nb"),
            "a <<-END\n\tEND\nb\n"
        );
    }

    #[test]
    fn local_pattern_kills_are_denied() {
        for c in [
            "pkill -f x",
            "killall node",
            "/usr/bin/pkill -9 -f foo",
            "sudo pkill foo",
            "timeout 5 pkill -f foo",
            "FOO=1 pkill x",
            "ssh nezopt 'true'; pkill -f x",
            "ssh nezopt \"echo hi\" && killall node",
            "pgrep -f foo | xargs kill -9",
            "kill $(pgrep -f foo)",
            "kill -9 `pgrep foo`",
            "bash -c 'pkill -f foo'",
            "zsh -lc \"killall Finder\"",
            "echo start; (cd /tmp && pkill -f foo)",
            "x=$(pkill -f foo)",
            "{ echo a; pkill x; }",
            "timeout -k 5 10 nice -n 5 env A=1 pkill x",
            "nohup setsid sudo -u root killall node &",
        ] {
            assert!(local_kill(c).is_some(), "{c}");
        }
    }

    #[test]
    fn plain_kills_and_mentions_pass() {
        for c in [
            "kill 1234",
            "kill -TERM 1234",
            "kill \"$PID\"",
            "kill $!",
            "kill %1",
            "kill -0 1234",
            "ssh nezopt 'pkill -f \"[b]uild.sh kernel\"'",
            "ssh nezopt pkill -f foo",
            "ssh -o ConnectTimeout=8 nezopt 'cd ~/x && pkill -f \"[r]un.sh\"; echo done'",
            "echo pkill",
            "grep -n \"pkill\" notes.md",
            "git commit -m \"no pkill; killall is banned\"",
            "rg 'killall|pkill' src",
            "command -v pkill",
            "ssh nezopt bash -s <<EOF\npkill -f x\nEOF",
        ] {
            assert!(local_kill(c).is_none(), "{c}");
        }
        let doc = "cat > notes.md <<'EOF'\npkill -f x\nkillall node\nEOF\ngit status";
        assert!(local_kill(&without_heredocs(doc)).is_none());
        assert!(local_kill(doc).is_none());
        // What runs after the heredoc still counts.
        assert!(local_kill("cat > a <<'EOF'\nnotes\nEOF\npkill -f x").is_some());
    }

    #[test]
    fn the_kill_check_is_mac_only_and_bypassable() {
        assert!(decide_on("pkill -f x", "/tmp", true).is_some());
        assert!(decide_on("pkill -f x", "/tmp", false).is_none());
        assert!(decide_on("LAGENT_BYPASS=owner-said-so pkill -f x", "/tmp", true).is_none());
        assert!(decide_on("LAGENT_BYPASS= pkill -f x", "/tmp", true).is_some());
        assert!(decide_on("ssh nezopt 'pkill -f x'", LUCINA, true).is_none());
        // The same wiring in every session: also inside Lucina scope.
        assert!(decide_on("pkill -f x", LUCINA, true).is_some());
    }
    const TGT: &str = "/home/nez/.cache/cargo-target/secretaryios";
    const INCIDENT: &str = r#"find debug/deps debug/build probe/deps probe/build -type f -mmin +720 ! -name "*.d" -delete"#;

    #[test]
    fn single_file_deletes_under_a_cargo_target_are_refused() {
        for c in [
            format!("cd {TGT} && {INCIDENT}"),
            format!("ssh nezopt 'cd {TGT} && {INCIDENT}'"),
            format!("ssh nezopt \"cd ~/.cache/cargo-target/secretaryios && {INCIDENT}\""),
            "ssh -o ConnectTimeout=8 nezopt 'cd ~/.cache/cargo-target/x/debug && find . -delete'".to_string(),
            "ssh -J jumphost nezopt 'cd ~/.cache/cargo-target/x && find debug -delete'".to_string(),
            "ssh -o ConnectTimeout=8 nezopt cd '$HOME/.cache/cargo-target/x/debug' '&&' find . -delete".to_string(),
            format!("ssh nezopt bash -c 'cd {TGT} && find probe/deps -delete'"),
            format!("bash -c 'cd {TGT}/debug && find . -type f -delete'"),
            format!("ssh nezopt bash -s <<'EOF'\ncd {TGT}\nfind debug/deps -delete\nEOF"),
            format!("find {TGT}/debug/deps -exec rm {{}} \\;"),
            format!("find {TGT}/debug/deps -execdir rm {{}} +"),
            format!("ls {TGT}/debug/deps | xargs rm"),
            format!("find {TGT}/probe -name '*.rlib' -print0 | xargs -0 rm -f"),
            format!("cd {TGT}/debug && ls deps | xargs -n 50 rm"),
            format!("rm {TGT}/debug/deps/*.rlib"),
            "rm ~/.cache/cargo-target/secretaryios/debug/deps/lib?.rlib".to_string(),
            "rm $HOME/.cache/lucina-check-target/debug/deps/*.rmeta".to_string(),
            "cd target/release && rm -f deps/*.d".to_string(),
            "find ../target/debug -delete".to_string(),
        ] {
            for is_mac in [true, false] {
                let r = decide_on(&c, "/home/nez/Projects/Lucina/Other", is_mac);
                assert!(
                    r.as_ref().is_some_and(|r| !r.contains("sweep-targets") && r.contains("by-unit sweep")),
                    "{c} (mac {is_mac})"
                );
                assert!(r.is_some_and(|r| r.contains("rm -rf") && r.contains("cargo clean")), "{c} (mac {is_mac})");
            }
        }
        // The shared target as the session's own directory, no `cd`.
        for is_mac in [true, false] {
            assert!(decide_on(INCIDENT, TGT, is_mac).is_some());
            assert!(decide_on("rm *.rlib", &format!("{TGT}/debug/deps"), is_mac).is_some());
        }
    }

    #[test]
    fn whole_profiles_and_targets_and_other_deletes_pass() {
        for c in [
            format!("rm -rf {TGT}/debug"),
            "rm -rf ~/.cache/cargo-target/secretaryios".to_string(),
            format!("ssh nezopt 'rm -rf {TGT}/probe'"),
            "cargo clean".to_string(),
            "cargo clean -p lucina-agentd".to_string(),
            "ssh nezopt 'cd ~/Projects/x && cargo clean -p lucina-agentd'".to_string(),
            "find ./build-output -name '*.o' -delete".to_string(),
            "ssh nezopt 'find ~/scratch/out -type f -delete'".to_string(),
            "rm -f target/x.log".to_string(),
            "rm build/*.o".to_string(),
            "ls out | xargs rm".to_string(),
            format!("find {TGT}/debug -name '*.rlib'"),
            format!("echo \"find {TGT} -delete\""),
            format!("cat > notes.md <<'EOF'\nfind {TGT} -delete\nEOF"),
            format!("LAGENT_BYPASS=\"owner approved\" find {TGT}/debug -delete"),
        ] {
            for is_mac in [true, false] {
                assert!(
                    decide_on(&c, "/home/nez/Projects/Lucina/Other", is_mac).is_none(),
                    "{c} (mac {is_mac})"
                );
            }
        }
        assert!(decide_on(
            &format!("LAGENT_BYPASS= find {TGT} -delete"),
            "/home/nez/scratch",
            true
        )
        .is_some());
        // Unexpanded variables fail open.
        assert!(decide_on(
            "rm $CARGO_TARGET_DIR/debug/deps/*.rlib",
            "/home/nez/scratch",
            true
        )
        .is_none());
    }

    #[test]
    fn a_cargo_marker_directory_counts_only_on_its_host() {
        let dir = std::env::temp_dir().join(format!("pipeline-guard-tgt-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("probe/deps")).unwrap();
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        std::fs::write(dir.join(".rustc_info.json"), "{}").unwrap();
        let c = format!("find {}/probe/deps -type f -delete", dir.display());
        let on_host = decide_on(&c, "/home/nez/scratch", false).is_some();
        let on_mac = decide_on(&c, "/home/nez/scratch", true).is_none();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(on_host, "the Linux binary sees the markers");
        assert!(on_mac, "the Mac binary goes by names only");
    }
    /// The coordinator's recorded acceptance cases (nezopt `/home/nez/scratch/co-audit/la-c040-acceptance.jsonl`,
    /// 12 lines, then `la-c040-acceptance-2.jsonl`, 9 lines): the command, and whether it must be refused.
    #[test]
    fn the_recorded_acceptance_cases() {
        let cases: [(&str, bool); 29] = [
            // la-c040-acceptance.jsonl, lines 1-12
            ("find /home/nez/.cache/cargo-target/secretaryios/debug/deps -type f -mtime +2 -delete", true),
            ("ssh nezopt 'find ~/.cache/cargo-target/secretaryios/debug/deps -type f -mtime +2 -delete'", true),
            ("ssh -o ConnectTimeout=8 nezopt 'cd ~/.cache/cargo-target/secretaryios/debug && find deps build -mtime +2 -delete'", true),
            ("bash -c 'find target/debug/incremental -mmin +600 -exec rm {} +'", true),
            ("ls -t target/debug/deps/*.rlib | tail -n +50 | xargs rm", true),
            ("rm -f target/release/deps/*.rmeta", true),
            ("LAGENT_BYPASS=\"\" find target/debug/deps -type f -delete", true),
            ("rm -rf /home/nez/.cache/cargo-target/secretaryios/debug", false),
            ("cargo clean -p lucina-ci", false),
            ("ls -la", false),
            ("find ./logs -name '*.log' -mtime +7 -delete", false),
            ("ssh nezopt 'rm -rf ~/.cache/lucina-check-target'", false),
            // la-c040-acceptance-2.jsonl, lines 1-9: all refused
            ("find target/debug/deps -type f -delete", true),
            ("FOO=1 find target/debug/deps -type f -delete", true),
            ("LAGENT_BYPASS=\"\" rm -f target/release/deps/*.rmeta", true),
            ("ls target/debug/deps | xargs rm", true),
            ("ls -t target/debug/deps/*.rlib | tail -n +50 | xargs rm -f", true),
            ("find target/debug/deps -type f -mtime +2 | xargs rm -f", true),
            ("timeout 60 find target/debug/deps -type f -delete", true),
            ("nice -n 10 find target/debug/deps -type f -delete", true),
            ("cd /home/nez/.cache/cargo-target/secretaryios && FOO=1 rm -f debug/deps/*.rlib", true),
            // la-c040-acceptance-3.jsonl, lines 1-8: five refused, then three that pass
            ("ls -t target/debug/deps | grep rlib | head -100 | xargs -r rm -f", true),
            ("LAGENT_BYPASS=\" \" find target/debug/deps -type f -delete", true),
            ("find target/debug/build -mindepth 1 -mtime +1 -print0 | sort -z | xargs -0 rm -rf", true),
            ("ssh nezopt \"cd ~/.cache/lucina-check-target/debug && ls -t deps | tail -n +200 | xargs rm -f\"", true),
            ("LAGENT_BYPASS='' rm -f /home/nez/.cache/cargo-target/public-sync/release/deps/*.rlib", true),
            ("rm -rf target", false),
            ("cd /home/nez/Projects/Lucina/LucinaOS && cargo clean", false),
            ("grep -rn target_deletion tools/pipeline-guard/src | head", false),
        ];
        let mut wrong = Vec::new();
        for (n, (c, refused)) in cases.into_iter().enumerate() {
            for (is_mac, cwd) in [
                (true, "/home/nez/scratch"),
                (false, "/home/nez/scratch"),
                (true, "/home/nez"),
                (false, "/home/nez"),
            ] {
                if decide_on(c, cwd, is_mac).is_some() != refused {
                    wrong.push(format!("case {} (mac {is_mac}, cwd {cwd}): {c}", n + 1));
                }
            }
        }
        assert!(wrong.is_empty(), "wrong verdicts:\n{}", wrong.join("\n"));
    }

    #[test]
    fn a_stage_between_the_lister_and_xargs_rm_still_counts() {
        for c in [
            format!("ls -t {TGT}/debug/deps | head -n 5 | xargs rm"),
            format!(
                "ls {TGT}/debug/deps | sort | grep rlib | awk '{{print $1}}' | xargs -n 10 rm -f"
            ),
            format!("cd {TGT}/debug && ls deps | tail -n +3 | sed s/x/y/ | xargs rm"),
            format!("ls {TGT}/debug/deps | tail -n +3 | sed s/x/y/ | xargs rm"),
        ] {
            assert!(decide_on(&c, "/home/nez/scratch", false).is_some(), "{c}");
        }
        for c in [
            "ls ./logs | tail -n +5 | xargs rm",
            "ls ./logs | xargs rm",
            "cat list.txt | sort | xargs rm -f",
        ] {
            assert!(decide_on(c, "/home/nez/scratch", false).is_none(), "{c}");
        }
    }

    #[test]
    fn a_blank_bypass_is_no_bypass_for_every_rule() {
        let blanks = [
            "LAGENT_BYPASS=",
            "LAGENT_BYPASS=\"\"",
            "LAGENT_BYPASS=''",
            "LAGENT_BYPASS=\" \"",
            "LAGENT_BYPASS=$EMPTY",
            "LAGENT_BYPASS=${EMPTY}",
            "LAGENT_BYPASS=\"$EMPTY\"",
        ];
        let reasons = [
            "LAGENT_BYPASS=x",
            "LAGENT_BYPASS=\"owner approved\"",
            "LAGENT_BYPASS='x y'",
        ];
        let rules = [
            // merge / push-to-main rule, kill rule, install rule, target rule
            ("git push origin HEAD:main", LUCINA, false),
            ("pkill -f x", "/home/nez/scratch", true),
            (
                "xcodebuild test -scheme Secretary -destination 'id=00008140-001225CE1A03801C'",
                LUCINA,
                false,
            ),
            (
                "find target/debug/deps -type f -delete",
                "/home/nez/scratch",
                false,
            ),
        ];
        for (cmd, cwd, is_mac) in rules {
            assert!(decide_on(cmd, cwd, is_mac).is_some(), "{cmd}");
            for b in blanks {
                let c = format!("{b} {cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
            }
            for b in reasons {
                let c = format!("{b} {cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
            }
            // One valid occurrence is enough.
            let c = format!("LAGENT_BYPASS= LAGENT_BYPASS=ok {cmd}");
            assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
        }
    }
    /// One command per rule, with the cwd and `is_mac` it needs: merge/push-to-main, kill, install,
    /// cargo target.
    fn four_rules() -> [(&'static str, &'static str, bool); 4] {
        [
            ("git push origin HEAD:main", LUCINA, false),
            ("pkill -f x", "/home/nez/scratch", true),
            (
                "xcodebuild test -scheme Secretary -destination 'id=00008140-001225CE1A03801C'",
                LUCINA,
                false,
            ),
            (
                "find target/debug/deps -type f -delete",
                "/home/nez/scratch",
                false,
            ),
        ]
    }

    #[test]
    fn a_bypass_value_is_read_as_a_shell_word() {
        // Each of these is blank or unknown at run time: no bypass, the rule still denies.
        let blanks = [
            "LAGENT_BYPASS=;",
            "LAGENT_BYPASS=|",
            "LAGENT_BYPASS=) ",
            "LAGENT_BYPASS=>x ",
            "LAGENT_BYPASS=$(true) ",
            "LAGENT_BYPASS=$() ",
            "LAGENT_BYPASS=`true` ",
            "LAGENT_BYPASS=\"${X}$(true)\" ",
            "LAGENT_BYPASS=$'\\x20' ",
            "LAGENT_BYPASS=$'\\t' ",
            "LAGENT_BYPASS=\\  ",
            "LAGENT_BYPASS=${X:-reason} ",
            "LAGENT_BYPASS=\"\"\"\" ",
        ];
        for (cmd, cwd, is_mac) in four_rules() {
            for b in blanks {
                let c = format!("{b}{cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
            }
            // The marker after the command is no bypass either when it is blank.
            let c = format!("{cmd} LAGENT_BYPASS=\"\"");
            assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
            // Quoted and bare pieces join into one value.
            for b in [
                "LAGENT_BYPASS=\"\"x ",
                "LAGENT_BYPASS=x\"\" ",
                "LAGENT_BYPASS=''x ",
                "LAGENT_BYPASS=a\"b c\"d ",
            ] {
                let c = format!("{b}{cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
            }
            // One valid marker among blank ones is enough; a marker inside a longer name is not one.
            let c = format!("LAGENT_BYPASS= LAGENT_BYPASS=\"\" LAGENT_BYPASS=ok {cmd}");
            assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
            let c = format!("FOO_LAGENT_BYPASS=x {cmd}");
            assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
        }
    }

    #[test]
    fn many_blank_markers_do_not_hang() {
        let (cmd, cwd, is_mac) = four_rules()[3];
        let c = format!("{}{cmd}", "LAGENT_BYPASS=\"\" ".repeat(50_000));
        assert!(decide_on(&c, cwd, is_mac).is_some());
        let stages = format!("ls {TGT}/debug/deps{}| xargs rm", " | cat".repeat(5_000));
        assert!(decide_on(&stages, "/home/nez/scratch", false).is_some());
        let stages = format!("ls ./logs{}| xargs rm", " | cat".repeat(5_000));
        assert!(decide_on(&stages, "/home/nez/scratch", false).is_none());
    }

    #[test]
    fn pipelines_across_delimiters_and_shells_still_count() {
        let ls = format!("ls {TGT}/debug/deps");
        for c in [
            format!("{ls} |\nxargs rm"),
            format!("{ls} | (xargs rm)"),
            format!("{ls} |& xargs rm"),
            format!("{ls} | {{ xargs rm; }}"),
            format!("{ls} | sort | grep a | tail -n 3 | xargs rm"),
            format!("{ls} | xargs sh -c 'rm {{}}'"),
            format!("{ls} | xargs -I{{}} bash -c 'echo x; rm -f {{}}'"),
            format!("{ls} | xargs -n 1 zsh -c 'rm \"$0\"'"),
        ] {
            assert!(decide_on(&c, "/home/nez/scratch", false).is_some(), "{c}");
        }
        for c in [
            format!("{ls} || xargs rm"),
            "ls ./logs | xargs sh -c 'rm {}'".to_string(),
            "ls ./logs | tail -n +5 | xargs rm".to_string(),
            "cat list.txt | sort | xargs rm -f".to_string(),
            "rm -rf target/debug".to_string(),
            "cargo clean".to_string(),
        ] {
            assert!(decide_on(&c, "/home/nez/scratch", false).is_none(), "{c}");
        }
    }

    #[test]
    fn the_bypass_log_line_is_one_bounded_line() {
        let line = super::bypass_log_line(
            "s\t1",
            "/c\nwd",
            true,
            &format!("a\tb\rc\nd{}", "é".repeat(5_000)),
        );
        assert!(!line[..line.len() - 1].contains(['\n', '\r']));
        assert_eq!(line.matches('\t').count(), 3);
        assert!(line.chars().count() < 2_100, "{}", line.chars().count());
        assert!(line.contains("\thonoured\t"));
        assert!(super::bypass_log_line("s", "c", false, "x").contains("\tdenied\t"));
    }
    #[test]
    fn special_parameters_in_quotes_are_no_bypass() {
        for (cmd, cwd, is_mac) in four_rules() {
            for b in [
                "LAGENT_BYPASS=\"$@\" ",
                "LAGENT_BYPASS=\"$*\" ",
                "LAGENT_BYPASS=\"$!\" ",
                "LAGENT_BYPASS=\"$1\" ",
                "LAGENT_BYPASS=\"$?\" ",
                "LAGENT_BYPASS=\"$$\" ",
                "LAGENT_BYPASS=\"$#\" ",
                "LAGENT_BYPASS=\"$-\" ",
            ] {
                let c = format!("{b}{cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
            }
            // Single quotes keep the text: `$X` is the reason.
            let c = format!("LAGENT_BYPASS='$X' {cmd}");
            assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
        }
    }

    #[test]
    fn xargs_with_a_shell_script_is_read_for_deletes_and_kills() {
        let ls = format!("ls {TGT}/debug/deps");
        for c in [
            format!("{ls} | xargs sh -c 'find {{}} -type f -delete'"),
            format!("echo x | xargs sh -c 'find {TGT}/debug/deps -delete'"),
            format!("{ls} | xargs bash -c 'bash -c \"rm {{}}\"'"),
            format!("{ls} | sort | xargs -I{{}} sh -c 'find {{}} -exec rm {{}} +'"),
        ] {
            assert!(decide_on(&c, "/home/nez/scratch", false).is_some(), "{c}");
        }
        for c in [
            "ls | xargs sh -c 'pkill -f x'",
            "ps | xargs -n1 bash -c 'kill $(pgrep x); pkill y'",
        ] {
            assert!(decide_on(c, "/home/nez/scratch", true).is_some(), "{c}");
        }
        for c in [
            "ls ./logs | xargs sh -c 'echo hi'",
            "ls ./logs | xargs sh -c 'rm {}'",
            "ls ./logs | xargs sh -c 'find {} -delete'",
        ] {
            for is_mac in [true, false] {
                assert!(decide_on(c, "/home/nez/scratch", is_mac).is_none(), "{c}");
            }
        }
    }

    #[test]
    fn a_long_pipeline_is_read_textually_past_the_stage_cap() {
        let cats = " | cat".repeat(70);
        let c = format!("ls target/debug/deps{cats} | xargs rm");
        assert!(decide_on(&c, "/home/nez/scratch", false).is_some(), "{c}");
        let c = format!("ls ./logs{cats} | xargs rm");
        assert!(decide_on(&c, "/home/nez/scratch", false).is_none(), "{c}");
    }

    #[test]
    fn long_text_after_blank_markers_is_cheap_and_a_long_value_is_unknown() {
        let (cmd, cwd, is_mac) = four_rules()[3];
        let big = "x".repeat(300_000);
        let c = format!(
            "{}{cmd}",
            format!("LAGENT_BYPASS=\"\" echo {big}; ").repeat(32)
        );
        assert!(decide_on(&c, cwd, is_mac).is_some());
        let c = format!("LAGENT_BYPASS={} {cmd}", "a".repeat(5_000));
        assert!(
            decide_on(&c, cwd, is_mac).is_some(),
            "a value word over 4096 chars is unknown"
        );
        let c = format!("LAGENT_BYPASS={} {cmd}", "a".repeat(4_000));
        assert!(decide_on(&c, cwd, is_mac).is_none());
    }

    #[test]
    fn the_bypass_log_is_rotated_and_lines_have_no_unicode_breaks() {
        let line = super::bypass_log_line("s", "c", true, "a\u{2028}b\u{2029}c\u{85}d");
        assert!(
            !line.contains(['\u{2028}', '\u{2029}', '\u{85}']),
            "{line:?}"
        );
        // Under the host temp dir (nezopt: $HOME/scratch/tmp); removed on every path, a panic included.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("pipeline-guard-log-{}", std::process::id()));
        let _cleanup = Cleanup(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("bypass.log");
        std::fs::write(&log, vec![b'x'; 1_100_000]).unwrap();
        std::fs::write(dir.join("bypass.log.1"), "old").unwrap();
        super::rotate_log(&log);
        assert_eq!(
            std::fs::metadata(dir.join("bypass.log.1")).unwrap().len(),
            1_100_000
        );
        assert!(!log.exists());
        // A small log stays; a missing one is no error and no file.
        std::fs::write(&log, "small").unwrap();
        super::rotate_log(&log);
        assert!(log.exists());
        let missing = dir.join("missing.log");
        super::rotate_log(&missing);
        assert!(!missing.exists());
    }

    #[test]
    fn nested_parameter_expansions_are_no_bypass() {
        for (cmd, cwd, is_mac) in four_rules() {
            for b in [
                "LAGENT_BYPASS=${X:-${Y}} ",
                "LAGENT_BYPASS=\"${X:-${Y}}\" ",
                "LAGENT_BYPASS=${X:-${Y:-${Z}}} ",
                "LAGENT_BYPASS=${X ",
            ] {
                let c = format!("{b}{cmd}");
                assert!(decide_on(&c, cwd, is_mac).is_some(), "{c}");
            }
            let c = format!("LAGENT_BYPASS=a${{X}}b {cmd}");
            assert!(decide_on(&c, cwd, is_mac).is_none(), "{c}");
        }
    }

    #[test]
    fn xargs_shell_scripts_are_read_on_their_own() {
        let cats = " | cat".repeat(70);
        for c in [
            "ls x | xargs sh -c 'rm -f target/debug/deps/*.rlib'".to_string(),
            "ls x | xargs -I{} sh -c 'cd target/debug && rm -f deps/*'".to_string(),
            "ls x | xargs sh -c \"sh -c 'rm -f target/debug/deps/*.rlib'\"".to_string(),
            "xargs sh -c 'rm -f target/debug/deps/*.rlib'".to_string(),
            // A far textual hit, whatever the session directory is.
            format!("ls /home/nez/cargo-target/y{cats} | xargs sh -c 'find . -delete'"),
            "rm /home/nez/cargo-target/x/*.rlib".to_string(),
        ] {
            for cwd in ["/home/nez/scratch", &format!("{TGT}/debug")] {
                assert!(decide_on(&c, cwd, false).is_some(), "{c} in {cwd}");
            }
        }
        for c in [
            "ls x | xargs sh -c 'rm -f ./out/*.o'",
            "ls x | xargs sh -c 'echo hi'",
            "xargs sh -c 'echo hi'",
        ] {
            assert!(decide_on(c, "/home/nez/scratch", false).is_none(), "{c}");
        }
    }

    // ---- LA-C044: the bypass ledger, the coordinator list, the payload-key capture ----

    /// A directory under the host temp dir, removed on every path, a panic included.
    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(name: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("pipeline-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Tmp(d)
    }

    /// A command the target rule denies without a bypass.
    const DENIED: &str = "rm -f target/release/deps/*.rmeta";
    const CWD: &str = "/home/nez/scratch";

    fn ctx<'a>(dir: &'a std::path::Path, app: Option<&'a str>, enforce: bool) -> super::Ctx<'a> {
        super::Ctx {
            dir,
            now: 1_700_000_000,
            is_mac: false,
            app_id: app,
            enforce,
        }
    }

    fn ledger(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("bypass-ledger.tsv")).unwrap_or_default()
    }

    #[test]
    fn the_utc_stamp_is_civil_time() {
        assert_eq!(super::utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::utc_stamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn a_ledger_line_has_eight_clean_columns() {
        let long = "é".repeat(300);
        let e = super::Entry {
            time: "2023-11-14T22:13:20Z",
            host: "nezopt",
            session: "",
            app: "",
            agent: "",
            status: "coordinator",
            reason: &format!("a\tb\rc\nd\u{2028}e\u{2029}f\u{85}g{long}"),
            command: &format!("x\ty\nz\r\u{2028}{}", "ü".repeat(300)),
        };
        let line = super::ledger_line(&e);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        assert!(
            !line.contains(['\r', '\u{2028}', '\u{2029}', '\u{85}']),
            "{line:?}"
        );
        let cols: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        assert_eq!(cols.len(), 8, "{line:?}");
        assert_eq!(&cols[..2], ["2023-11-14T22:13:20Z", "nezopt"]);
        assert_eq!(&cols[2..5], ["-", "-", "-"]);
        assert_eq!(cols[5], "coordinator");
        assert_eq!(cols[6].chars().count(), 200, "{}", cols[6]);
        assert!(cols[6].starts_with("a b c d e f g"));
        assert_eq!(cols[7].chars().count(), 120, "{}", cols[7]);
        assert!(cols[7].starts_with("x y z"));
    }

    #[test]
    fn the_reason_is_the_first_honoured_marker() {
        use super::bypass_reason as r;
        assert_eq!(r("LAGENT_BYPASS=because rm x").as_deref(), Some("because"));
        assert_eq!(
            r("LAGENT_BYPASS=\"two words\" rm x").as_deref(),
            Some("two words")
        );
        assert_eq!(
            r("LAGENT_BYPASS='' LAGENT_BYPASS=ok rm x").as_deref(),
            Some("ok")
        );
        assert_eq!(r("FOO_LAGENT_BYPASS=x rm y"), None);
        assert_eq!(r("LAGENT_BYPASS=\" \" rm y"), None);
        assert_eq!(r("rm y"), None);
    }

    #[test]
    fn an_honoured_override_leaves_one_ledger_line() {
        let t = tmp("honoured");
        let c = format!("LAGENT_BYPASS=\"cleanup of mine\" {DENIED}");
        let v = serde_json::json!({"session_id": "sess-1", "hook_event_name": "PreToolUse"});
        let got = super::judge(&v, &c, CWD, &ctx(&t.0, Some("local_abc"), false));
        assert_eq!(got, None, "allowed");
        let text = ledger(&t.0);
        assert_eq!(text.lines().count(), 1, "{text:?}");
        let cols: Vec<&str> = text.trim_end().split('\t').collect();
        assert_eq!(cols.len(), 8, "{text:?}");
        assert_eq!(cols[0], "2023-11-14T22:13:20Z");
        assert_eq!(cols[1], "nezopt");
        assert_eq!(&cols[2..5], ["sess-1", "local_abc", "-"]);
        assert_eq!(cols[5], "would-refuse:not-listed");
        assert_eq!(cols[6], "cleanup of mine");
        assert!(cols[7].starts_with("LAGENT_BYPASS="), "{}", cols[7]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(t.0.join("bypass-ledger.tsv"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A second override appends.
        assert_eq!(super::judge(&v, &c, CWD, &ctx(&t.0, None, false)), None);
        assert_eq!(ledger(&t.0).lines().count(), 2);
    }

    #[test]
    fn only_a_flipped_verdict_is_an_override() {
        let t = tmp("flipped");
        let v = serde_json::json!({"session_id": "s"});
        // No bypass: denied as before, no line.
        let d = super::judge(&v, DENIED, CWD, &ctx(&t.0, None, false)).expect("denied");
        assert!(d.starts_with("pipeline-guard: "), "{d}");
        assert!(d.contains("LAGENT_BYPASS=<reason>"), "{d}");
        // A blank bypass: denied, no line.
        for b in [
            "LAGENT_BYPASS=",
            "LAGENT_BYPASS=\"\"",
            "LAGENT_BYPASS=\" \"",
        ] {
            let c = format!("{b} {DENIED}");
            assert!(
                super::judge(&v, &c, CWD, &ctx(&t.0, None, false)).is_some(),
                "{c}"
            );
        }
        assert_eq!(ledger(&t.0), "");
        // A marker in a command no rule denies: allowed, no line.
        for c in [
            "echo LAGENT_BYPASS=why done",
            "LAGENT_BYPASS=why ls -la",
            "cat <<'EOF'\nLAGENT_BYPASS=why\nEOF",
        ] {
            assert_eq!(
                super::judge(&v, c, CWD, &ctx(&t.0, None, false)),
                None,
                "{c}"
            );
        }
        assert_eq!(ledger(&t.0), "");
        assert!(!t.0.join("bypass-ledger.tsv").exists());
    }

    #[test]
    fn a_failed_ledger_write_denies_the_override() {
        let t = tmp("unwritable");
        std::fs::create_dir_all(&t.0).unwrap();
        let blocker = t.0.join("blocker");
        std::fs::write(&blocker, "a regular file").unwrap();
        let dir = blocker.join("state");
        let c = format!("LAGENT_BYPASS=why {DENIED}");
        let v = serde_json::json!({"session_id": "s"});
        let d = super::judge(&v, &c, CWD, &ctx(&dir, None, false)).expect("refused");
        assert!(d.contains("bypass not recorded ("), "{d}");
        assert!(d.contains("tell the coordinator"), "{d}");
        // The same command without a bypass still carries the rule's text.
        assert!(super::judge(&v, DENIED, CWD, &ctx(&dir, None, false)).is_some());
        // A command that is no override never touches the ledger, so it is not refused.
        assert_eq!(
            super::judge(&v, "echo LAGENT_BYPASS=x", CWD, &ctx(&dir, None, false)),
            None
        );
    }

    #[test]
    fn the_identity_status_follows_the_coordinator_list() {
        use super::{identity_status as st, read_coordinators};
        let t = tmp("identity");
        std::fs::create_dir_all(&t.0).unwrap();
        // Missing file: nobody is listed.
        assert!(read_coordinators(&t.0).is_empty());
        assert_eq!(
            st(&[], "s1", Some("local_a"), ""),
            "would-refuse:not-listed"
        );
        std::fs::write(t.0.join("coordinators"), "").unwrap();
        assert!(read_coordinators(&t.0).is_empty());
        std::fs::write(
            t.0.join("coordinators"),
            "# the coordinators\n\n  sess-1  \nlocal_app-9\n   # indented comment\n\n",
        )
        .unwrap();
        let list = read_coordinators(&t.0);
        assert_eq!(list, ["sess-1", "local_app-9"]);
        assert_eq!(st(&list, "sess-1", None, ""), "coordinator");
        assert_eq!(st(&list, "other", Some("local_app-9"), ""), "coordinator");
        assert_eq!(
            st(&list, "other", Some("local_x"), ""),
            "would-refuse:not-listed"
        );
        assert_eq!(st(&list, "other", None, ""), "would-refuse:not-listed");
        assert_eq!(st(&list, "", None, ""), "would-refuse:not-listed");
        assert_eq!(
            st(&list, "sess-1", None, "agent-7"),
            "would-refuse:subagent"
        );
        assert_eq!(
            st(&list, "sess-1", Some("local_app-9"), "agent-7"),
            "would-refuse:subagent"
        );
        assert_eq!(st(&list, "other", None, "agent-7"), "would-refuse:subagent");
        // A comment line is never an id.
        assert_eq!(
            st(&list, "# the coordinators", None, ""),
            "would-refuse:not-listed"
        );
    }

    #[test]
    fn the_ledger_records_the_status_and_enforcement_denies_it() {
        let t = tmp("enforce");
        std::fs::create_dir_all(&t.0).unwrap();
        std::fs::write(t.0.join("coordinators"), "# c\nsess-1\nlocal_app-9\n").unwrap();
        let c = format!("LAGENT_BYPASS=why {DENIED}");
        let last =
            |t: &Tmp| -> String { ledger(&t.0).lines().last().unwrap_or_default().to_string() };
        let cases = [
            // (payload, app id, status, enforce-denial text)
            (
                serde_json::json!({"session_id": "sess-1"}),
                None,
                "coordinator",
                None,
            ),
            (
                serde_json::json!({"session_id": "x"}),
                Some("local_app-9"),
                "coordinator",
                None,
            ),
            (
                serde_json::json!({"session_id": "x"}),
                Some("local_zzz"),
                "would-refuse:not-listed",
                Some("this session is not on the coordinator list"),
            ),
            (
                serde_json::json!({"session_id": "sess-1", "agent_id": "ag-1"}),
                None,
                "would-refuse:subagent",
                Some("a subagent may not bypass"),
            ),
            (
                serde_json::json!({"session_id": "sess-1", "agent_id": ""}),
                None,
                "coordinator",
                None,
            ),
        ];
        for (v, app, status, deny) in cases {
            // Phase 1: log only, always allowed.
            let before = ledger(&t.0).lines().count();
            assert_eq!(
                super::judge(&v, &c, CWD, &ctx(&t.0, app, false)),
                None,
                "{v}"
            );
            assert_eq!(ledger(&t.0).lines().count(), before + 1);
            assert!(last(&t).contains(&format!("\t{status}\t")), "{}", last(&t));
            // Phase 2: the would-refuse ones are denied, after their ledger line.
            let got = super::judge(&v, &c, CWD, &ctx(&t.0, app, true));
            assert_eq!(ledger(&t.0).lines().count(), before + 2, "{v}");
            assert!(last(&t).contains(&format!("\t{status}\t")), "{}", last(&t));
            match deny {
                None => assert_eq!(got, None, "{v}"),
                Some(who) => {
                    let g = got.expect("denied");
                    assert!(
                        g.contains(&format!(
                            "LAGENT_BYPASS is for the coordinator only: {who}."
                        )),
                        "{g}"
                    );
                    assert!(
                        g.contains("~/.local/state/pipeline-guard/coordinators"),
                        "{g}"
                    );
                }
            }
        }
        // The shipped constant is phase 1.
        const { assert!(!super::ENFORCE_IDENTITY) };
    }

    fn keys_file(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("payload-keys.tsv")).unwrap_or_default()
    }

    #[test]
    fn the_capture_records_key_names_only() {
        let t = tmp("capture");
        let v = serde_json::json!({
            "session_id": "VALUE-ONE",
            "tool_name": "Bash",
            "hook_event_name": "PreToolUse",
            "cwd": "VALUE-TWO",
            "tool_input": {"command": "VALUE-THREE", "nested_key": {"deeper": 1}},
        });
        super::capture_keys(&t.0, 1_700_000_000, &v);
        let text = keys_file(&t.0);
        assert_eq!(
            text,
            "2023-11-14T22:13:20Z\tcwd,hook_event_name,session_id,tool_input,tool_name\tagent_id=absent\n"
        );
        for no in ["VALUE", "nested_key", "deeper", "command"] {
            assert!(!text.contains(no), "{text}");
        }
        let w = serde_json::json!({"agent_id": "AGENT-VALUE", "session_id": "x"});
        super::capture_keys(&t.0, 1_700_000_001, &w);
        let text = keys_file(&t.0);
        assert!(
            text.ends_with("2023-11-14T22:13:21Z\tagent_id,session_id\tagent_id=present\n"),
            "{text}"
        );
        assert!(!text.contains("AGENT-VALUE"));
        // An empty or non-string agent_id counts as absent.
        for a in [
            serde_json::json!(""),
            serde_json::json!(7),
            serde_json::json!(null),
        ] {
            let before = keys_file(&t.0).lines().count();
            super::capture_keys(&t.0, 1, &serde_json::json!({"agent_id": a}));
            assert!(keys_file(&t.0)
                .lines()
                .nth(before)
                .unwrap()
                .ends_with("agent_id=absent"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(t.0.join("payload-keys.tsv"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn the_capture_covers_pretooluse_payloads_only() {
        let t = tmp("capture-events");
        super::capture_keys(
            &t.0,
            1,
            &serde_json::json!({"hook_event_name": "PostToolUse", "a": 1}),
        );
        super::capture_keys(&t.0, 1, &serde_json::json!({"hook_event_name": "Stop"}));
        super::capture_keys(&t.0, 1, &serde_json::json!(["not", "an", "object"]));
        assert!(!t.0.join("payload-keys.tsv").exists());
        super::capture_keys(&t.0, 1, &serde_json::json!({"a": 1}));
        assert_eq!(keys_file(&t.0).lines().count(), 1);
    }

    #[test]
    fn the_capture_stops_at_fifty_per_class_and_at_64_kib() {
        let t = tmp("capture-cap");
        let absent = serde_json::json!({"session_id": "s"});
        let present = serde_json::json!({"session_id": "s", "agent_id": "a"});
        for _ in 0..60 {
            super::capture_keys(&t.0, 1, &absent);
        }
        let text = keys_file(&t.0);
        assert_eq!(text.matches("agent_id=absent").count(), 50);
        // The other class still has its room.
        for _ in 0..60 {
            super::capture_keys(&t.0, 1, &present);
        }
        let text = keys_file(&t.0);
        assert_eq!(text.matches("agent_id=absent").count(), 50);
        assert_eq!(text.matches("agent_id=present").count(), 50);
        super::capture_keys(&t.0, 1, &absent);
        super::capture_keys(&t.0, 1, &present);
        assert_eq!(keys_file(&t.0).lines().count(), 100);
        // A file over 64 KiB gets nothing.
        let big = tmp("capture-big");
        std::fs::create_dir_all(&big.0).unwrap();
        std::fs::write(big.0.join("payload-keys.tsv"), vec![b'x'; 70_000]).unwrap();
        super::capture_keys(&big.0, 1, &absent);
        assert_eq!(
            std::fs::metadata(big.0.join("payload-keys.tsv"))
                .unwrap()
                .len(),
            70_000
        );
    }

    // ---- LA-C044 round 2: non-regular state files, parsing, sanitising, the sentinel ----
    /// `mkfifo path`; `false` (with a SKIP line on stderr) when the tool is missing: the test body is skipped.
    fn mkfifo(path: &std::path::Path) -> bool {
        let ok = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("SKIP: no mkfifo");
        }
        ok
    }

    /// Runs `f` on a thread; `None` when it does not return within 5 seconds (a blocked open). A panic
    /// inside `f` is re-raised here with its own message.
    fn within_5s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(r);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(t)) => Some(t),
            Ok(Err(p)) => std::panic::resume_unwind(p),
            Err(_) => None,
        }
    }
    fn judge_in(dir: &std::path::Path, v: serde_json::Value, c: String) -> Option<Option<String>> {
        let dir = dir.to_path_buf();
        within_5s(move || super::judge(&v, &c, CWD, &ctx(&dir, None, false)))
    }

    #[test]
    fn a_fifo_at_the_capture_file_does_not_block() {
        let t = tmp("fifo-capture");
        std::fs::create_dir_all(&t.0).unwrap();
        if !mkfifo(&t.0.join("payload-keys.tsv")) {
            return;
        }
        let dir = t.0.clone();
        let done = within_5s(move || {
            super::capture_keys(&dir, 1, &serde_json::json!({"a": 1}));
        });
        assert!(done.is_some(), "the capture blocked on a FIFO");
        // The verdict is unchanged: a rule's denial and an allowed command.
        let v = serde_json::json!({"session_id": "s"});
        let got = judge_in(&t.0, v.clone(), DENIED.to_string());
        assert!(got.expect("returned").is_some());
        assert_eq!(judge_in(&t.0, v, "ls -la".into()), Some(None));
    }

    #[test]
    fn a_fifo_at_the_coordinators_file_is_an_empty_list() {
        let t = tmp("fifo-coord");
        std::fs::create_dir_all(&t.0).unwrap();
        if !mkfifo(&t.0.join("coordinators")) {
            return;
        }
        let dir = t.0.clone();
        let list = within_5s(move || super::read_coordinators(&dir));
        assert_eq!(list, Some(Vec::<String>::new()), "blocked or not empty");
        let v = serde_json::json!({"session_id": "s"});
        let got = judge_in(&t.0, v, format!("LAGENT_BYPASS=why {DENIED}"));
        assert_eq!(got, Some(None), "allowed, as with an empty list");
        assert!(ledger(&t.0).contains("would-refuse:not-listed"));
    }

    #[test]
    fn a_symlinked_ledger_is_refused_and_the_target_untouched() {
        let t = tmp("symlink-ledger");
        std::fs::create_dir_all(&t.0).unwrap();
        let foreign = t.0.join("foreign.txt");
        std::fs::write(&foreign, "export X=1\n").unwrap();
        let state = t.0.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::os::unix::fs::symlink(&foreign, state.join("bypass-ledger.tsv")).unwrap();
        let v = serde_json::json!({"session_id": "s"});
        let d = judge_in(&state, v, format!("LAGENT_BYPASS=why {DENIED}"))
            .expect("returned")
            .expect("refused");
        assert!(d.contains("bypass not recorded ("), "{d}");
        assert!(d.contains("symlink"), "{d}");
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "export X=1\n");
    }

    #[test]
    fn a_fifo_ledger_and_a_directory_ledger_are_refused() {
        let t = tmp("fifo-ledger");
        let state = t.0.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let v = serde_json::json!({"session_id": "s"});
        let c = format!("LAGENT_BYPASS=why {DENIED}");
        if mkfifo(&state.join("bypass-ledger.tsv")) {
            let d = judge_in(&state, v.clone(), c.clone())
                .expect("returned (no blocking open)")
                .expect("refused");
            assert!(d.contains("bypass not recorded ("), "{d}");
            assert!(d.contains("not a regular file"), "{d}");
        }
        let t2 = tmp("dir-ledger");
        std::fs::create_dir_all(t2.0.join("bypass-ledger.tsv")).unwrap();
        let d = judge_in(&t2.0, v, c).expect("returned").expect("refused");
        assert!(d.contains("not a regular file"), "{d}");
    }

    #[test]
    fn an_existing_loose_ledger_becomes_0600() {
        use std::os::unix::fs::PermissionsExt;
        let t = tmp("loose-ledger");
        std::fs::create_dir_all(&t.0).unwrap();
        let p = t.0.join("bypass-ledger.tsv");
        std::fs::write(&p, "old\tline\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let v = serde_json::json!({"session_id": "s"});
        let c = format!("LAGENT_BYPASS=why {DENIED}");
        assert_eq!(super::judge(&v, &c, CWD, &ctx(&t.0, None, false)), None);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(ledger(&t.0).lines().count(), 2);
    }

    #[test]
    fn a_symlinked_state_directory_is_refused() {
        let t = tmp("symlink-dir");
        let real = t.0.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = t.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let v = serde_json::json!({"session_id": "s"});
        let c = format!("LAGENT_BYPASS=why {DENIED}");
        let d = super::judge(&v, &c, CWD, &ctx(&link, None, false)).expect("refused");
        assert!(d.contains("bypass not recorded ("), "{d}");
        assert!(d.contains(&link.display().to_string()), "{d}");
        assert!(!real.join("bypass-ledger.tsv").exists());
        // The capture skips it silently too.
        super::capture_keys(&link, 1, &serde_json::json!({"a": 1}));
        assert!(!real.join("payload-keys.tsv").exists());
    }

    #[test]
    fn the_coordinators_file_is_read_leniently() {
        let t = tmp("coord-parse");
        std::fs::create_dir_all(&t.0).unwrap();
        let p = t.0.join("coordinators");
        let mut bytes = b"\xEF\xBB\xBFsess-1\r\n# whole line\r\nsess-2 # a note\r\n\r\n   \r\nlocal_x\xFFy\r\nsess-3".to_vec();
        std::fs::write(&p, &bytes).unwrap();
        let list = super::read_coordinators(&t.0);
        assert_eq!(list.len(), 4, "{list:?}");
        assert_eq!(&list[..2], ["sess-1", "sess-2"]);
        assert!(
            list[2].starts_with("local_x") && list[2].ends_with('y'),
            "{list:?}"
        );
        assert_eq!(list[3], "sess-3");
        bytes.clear();
        std::fs::write(&p, b"sess-9 #\n#sess-8\n").unwrap();
        assert_eq!(super::read_coordinators(&t.0), ["sess-9"]);
    }

    #[test]
    fn format_and_bidi_characters_become_spaces() {
        for c in [
            '\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}',
            '\u{202C}', '\u{202D}', '\u{202E}', '\u{2060}', '\u{2064}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{FEFF}',
        ] {
            assert_eq!(super::plain(&format!("a{c}b"), 10), "a b", "{:x}", c as u32);
        }
        assert_eq!(super::plain("é–ü", 10), "é–ü");
    }

    #[test]
    fn a_full_capture_stops_at_one_stat() {
        let t = tmp("capture-sentinel");
        let absent = serde_json::json!({"session_id": "s"});
        let present = serde_json::json!({"session_id": "s", "agent_id": "a"});
        for _ in 0..50 {
            super::capture_keys(&t.0, 1, &absent);
        }
        assert!(!t.0.join("payload-keys.done").exists());
        for _ in 0..50 {
            super::capture_keys(&t.0, 1, &present);
        }
        assert!(t.0.join("payload-keys.done").exists());
        assert_eq!(keys_file(&t.0).lines().count(), 100);
        // The capture file is not opened any more: gone stays gone, a FIFO there does not block.
        std::fs::remove_file(t.0.join("payload-keys.tsv")).unwrap();
        super::capture_keys(&t.0, 1, &absent);
        super::capture_keys(&t.0, 1, &present);
        assert!(!t.0.join("payload-keys.tsv").exists());
    }

    #[test]
    fn another_rule_that_still_denies_leaves_no_ledger_line() {
        let t = tmp("second-rule");
        // The marker is only inside a heredoc body: the merge rule re-reads the stripped text and still denies.
        let c = "git push origin HEAD:main <<'EOF'\nLAGENT_BYPASS=why\nEOF";
        let v = serde_json::json!({"session_id": "s"});
        let cx = ctx(&t.0, None, false);
        assert!(super::judge(&v, c, "/home/nez/Projects/Lucina/SecretaryIOS", &cx).is_some());
        assert_eq!(ledger(&t.0), "");
    }

    #[test]
    fn a_heredoc_marker_honoured_by_the_target_rule_leaves_one_line() {
        let t = tmp("heredoc-honoured");
        let c = format!("cat <<'EOF'\nLAGENT_BYPASS=why\nEOF\n{DENIED}");
        let v = serde_json::json!({"session_id": "s"});
        assert_eq!(super::judge(&v, &c, CWD, &ctx(&t.0, None, false)), None);
        assert_eq!(ledger(&t.0).lines().count(), 1);
    }

    // ---- LA-C044 round 3: bypass.log is checked too, hard links, the cut last line ----

    fn log_in(dir: &std::path::Path, command: &str) -> Option<()> {
        let dir = dir.to_path_buf();
        let command = command.to_string();
        within_5s(move || super::log_attempt(&dir, "s", "/home/nez/scratch", true, &command))
    }

    #[test]
    fn a_fifo_at_bypass_log_does_not_block() {
        let t = tmp("log-fifo");
        std::fs::create_dir_all(&t.0).unwrap();
        if !mkfifo(&t.0.join("bypass.log")) {
            return;
        }
        assert!(
            log_in(&t.0, "LAGENT_BYPASS=why ls").is_some(),
            "the attempts log blocked on a FIFO"
        );
        // The verdict path is unchanged by it.
        let v = serde_json::json!({"session_id": "s"});
        let got = judge_in(&t.0, v, format!("LAGENT_BYPASS=why {DENIED}"));
        assert_eq!(got, Some(None));
    }

    #[test]
    fn a_symlinked_bypass_log_is_not_written_through() {
        let t = tmp("log-symlink");
        std::fs::create_dir_all(&t.0).unwrap();
        let foreign = t.0.join("foreign.txt");
        std::fs::write(&foreign, "keep\n").unwrap();
        std::os::unix::fs::symlink(&foreign, t.0.join("bypass.log")).unwrap();
        assert!(log_in(&t.0, "LAGENT_BYPASS=why ls").is_some());
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "keep\n");
    }

    #[test]
    fn rotation_leaves_non_regular_files_alone() {
        let t = tmp("rotate-odd");
        std::fs::create_dir_all(&t.0).unwrap();
        let log = t.0.join("bypass.log");
        // A symlink to a big file is not renamed.
        let big = t.0.join("big.txt");
        std::fs::write(&big, vec![b'x'; 1_100_000]).unwrap();
        std::os::unix::fs::symlink(&big, &log).unwrap();
        super::rotate_log(&log);
        assert!(std::fs::symlink_metadata(&log)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!t.0.join("bypass.log.1").exists());
        std::fs::remove_file(&log).unwrap();
        // A FIFO is not renamed.
        if mkfifo(&log) {
            let l = log.clone();
            assert!(within_5s(move || super::rotate_log(&l)).is_some());
            assert!(!t.0.join("bypass.log.1").exists());
            std::fs::remove_file(&log).unwrap();
        }
        // A big regular log is not renamed onto a symlinked `.1`.
        std::fs::write(&log, vec![b'y'; 1_100_000]).unwrap();
        let foreign = t.0.join("foreign.txt");
        std::fs::write(&foreign, "keep").unwrap();
        std::os::unix::fs::symlink(&foreign, t.0.join("bypass.log.1")).unwrap();
        super::rotate_log(&log);
        assert!(log.exists());
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "keep");
    }

    #[test]
    fn a_loose_bypass_log_becomes_0600() {
        use std::os::unix::fs::PermissionsExt;
        let t = tmp("log-loose");
        std::fs::create_dir_all(&t.0).unwrap();
        let p = t.0.join("bypass.log");
        std::fs::write(&p, "old\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(log_in(&t.0, "LAGENT_BYPASS=why ls").is_some());
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 2);
        // A new one is 0600 too.
        let t2 = tmp("log-new");
        assert!(log_in(&t2.0, "LAGENT_BYPASS=why ls").is_some());
        let mode = std::fs::metadata(t2.0.join("bypass.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn loose_foreign(t: &Tmp) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&t.0).unwrap();
        let foreign = t.0.join("foreign.txt");
        std::fs::write(&foreign, "export X=1\n").unwrap();
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o644)).unwrap();
        foreign
    }

    fn untouched(foreign: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::read_to_string(foreign).unwrap(), "export X=1\n");
        let mode = std::fs::metadata(foreign).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "the foreign file's mode changed");
    }

    #[test]
    fn a_hard_linked_ledger_is_refused() {
        let t = tmp("hardlink-ledger");
        let foreign = loose_foreign(&t);
        let state = t.0.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::hard_link(&foreign, state.join("bypass-ledger.tsv")).unwrap();
        let v = serde_json::json!({"session_id": "s"});
        let d = judge_in(&state, v, format!("LAGENT_BYPASS=why {DENIED}"))
            .expect("returned")
            .expect("refused");
        assert!(
            d.contains("bypass not recorded (ledger has several links or was swapped)"),
            "{d}"
        );
        untouched(&foreign);
    }

    #[test]
    fn hard_linked_capture_and_attempts_files_are_skipped() {
        let t = tmp("hardlink-others");
        let foreign = loose_foreign(&t);
        let state = t.0.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::hard_link(&foreign, state.join("payload-keys.tsv")).unwrap();
        super::capture_keys(&state, 1, &serde_json::json!({"a": 1}));
        untouched(&foreign);
        std::fs::remove_file(state.join("payload-keys.tsv")).unwrap();
        std::fs::hard_link(&foreign, state.join("bypass.log")).unwrap();
        assert!(log_in(&state, "LAGENT_BYPASS=why ls").is_some());
        untouched(&foreign);
    }

    #[test]
    fn a_cut_last_coordinator_line_is_dropped() {
        let t = tmp("coord-cut");
        std::fs::create_dir_all(&t.0).unwrap();
        // 64 KiB bound: the last line is cut after "abc" of "abc-long-id", which must not list as "abc".
        let mut s = String::from("sess-1\n");
        s.push('#');
        s.push_str(&"x".repeat(65_533 - s.len() - 1));
        s.push('\n');
        assert_eq!(s.len(), 65_533);
        s.push_str("abc-long-id\nlast\n");
        std::fs::write(t.0.join("coordinators"), &s).unwrap();
        let list = super::read_coordinators(&t.0);
        assert_eq!(list, ["sess-1"], "{list:?}");
        // A file under the bound keeps its last line without a newline.
        std::fs::write(t.0.join("coordinators"), "a\nb").unwrap();
        assert_eq!(super::read_coordinators(&t.0), ["a", "b"]);
    }

    #[test]
    fn the_attempts_log_uses_the_one_sanitiser() {
        let line = super::bypass_log_line(
            "s\u{202E}x",
            "c\u{200B}d",
            true,
            &format!("a\u{FEFF}b{}", "z".repeat(3000)),
        );
        assert!(
            !line.contains(['\u{202E}', '\u{200B}', '\u{FEFF}']),
            "{line:?}"
        );
        let cols: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        assert_eq!(cols.len(), 4);
        assert_eq!(cols[0], "s x");
        assert_eq!(cols[1], "c d");
        assert_eq!(cols[2], "honoured");
        assert_eq!(cols[3].chars().count(), 2000);
        assert!(cols[3].starts_with("a b"));
    }
}
