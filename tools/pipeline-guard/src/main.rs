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
    let scope = Regex::new(SCOPE).ok()?;
    if !scope.is_match(command) && !scope.is_match(cwd) {
        return None;
    }
    if bypass && bypass_given(command) {
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
    if hand_merge_into_main_checkout(command, cwd) {
        return Some(
            "Lucina repos: a repo's main checkout moves only through the merge queue (repos without a remote too: \
             LucinaOS). Commit in your workspace and run `lagent merge <workspace id | path>`; \
             it fast-forwards the main checkout itself. Merging origin/main into your own worktree is fine."
                .into(),
        );
    }
    for c in push.captures_iter(command) {
        let args = c.get(2).map(|m| m.as_str()).unwrap_or_default();
        if to_main.is_match(args) && !args.contains("--dry-run") {
            return Some(
                "Lucina repos: main moves only through the merge queue. Commit in your workspace, then run \
                 `lagent merge <workspace id | path>`: it merges into origin/main with a merge commit, runs the repo's gates (cargo test of \
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
/// (tab, CR, newline) become spaces and each field is cut at 2000 characters, so it stays one
/// bounded line.
fn bypass_log_line(session: &str, cwd: &str, honoured: bool, command: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .take(2000)
            .map(|c| {
                if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                    ' '
                } else {
                    c
                }
            })
            .collect()
    };
    format!(
        "{}\t{}\t{}\t{}\n",
        clean(session),
        clean(cwd),
        if honoured { "honoured" } else { "denied" },
        clean(command)
    )
}

/// Rotates `bypass.log` once it is over 1 MiB: it becomes `bypass.log.1` (the old `.1` is replaced)
/// and the next line starts a new file. Any failure is ignored: it never changes a verdict.
fn rotate_log(path: &std::path::Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
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
    if command.contains("LAGENT_BYPASS=") {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = format!("{home}/.local/state/pipeline-guard");
        if std::fs::create_dir_all(&dir).is_ok() {
            rotate_log(&std::path::Path::new(&dir).join("bypass.log"));
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{dir}/bypass.log"))
            {
                // Every occurrence is logged, honoured or denied, so a blank attempt leaves a record too.
                let _ = f.write_all(
                    bypass_log_line(
                        v["session_id"].as_str().unwrap_or("?"),
                        cwd,
                        bypass,
                        command,
                    )
                    .as_bytes(),
                );
            }
        }
    }
    if let Some(reason) = decide_with(command, cwd, cfg!(target_os = "macos"), bypass) {
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
        let cases: [(&str, bool); 21] = [
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
        // Under the scratch tree, never /tmp; removed on every path, a panic included.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::path::PathBuf::from(format!(
            "/home/nez/scratch/pipeline-guard-log-{}",
            std::process::id()
        ));
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
}
