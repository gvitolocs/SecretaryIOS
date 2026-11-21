//! `lucina-ci key-scan [--root DIR] [FILE...]`: refuses a provider key format in a tracked text file.
//!
//! The formats are the lines of `tools/ci/key-patterns.txt` (`kind TAB regex`), compiled into this binary. There is no
//! allowlist: a match fails the scan wherever it is, test files included, and `.gitleaks.toml` is not read. A test
//! vector is a synthetic string no scanner matches, built at run time from parts that do not match one by one.
//!
//! The launcher takes no dependency for this, so the patterns are run by a small backtracking matcher for the subset of
//! the `regex` crate's syntax the table uses: literals, `\` escapes, `[...]` classes with ranges and `^`, `^`, groups
//! `( )` and `(?: )`, `|`, and the quantifiers `? * + {n} {n,} {n,m}`. Anything else in the table is an error, not a
//! silent mismatch.
//!
//! What it sees: the text of the files it is given (every `git ls-files` entry when none are named). What it cannot see:
//! a key assembled at run time from parts, a line that costs more than the per-line step budget (the file fails with
//! exit 2), compressed or binary files (a NUL in the first 8 KiB, skipped and named),
//! files over 4 MiB (skipped and named), a symlink (skipped and named, never followed), a file named on the command
//! line that cannot be read (named, exit 2),
//! untracked files, and history.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const PATTERNS: &str = include_str!("../../key-patterns.txt");
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const SNIFF_BYTES: usize = 8 * 1024;
const NO_MAX: usize = usize::MAX;

/// The kinds the table must hold.
#[cfg(test)]
const KINDS: [&str; 8] = [
    "google-api-key",
    "github-token",
    "github-pat",
    "aws-key",
    "slack-token",
    "openai-key",
    "pem-private-key",
    "jwt",
];

enum Node {
    Lit(char),
    Class(bool, Vec<(char, char)>),
    Bol,
    Group(Option<usize>, Vec<Vec<Item>>),
}

struct Item {
    node: Node,
    min: usize,
    max: usize,
}

pub struct Pattern {
    pub kind: String,
    alts: Vec<Item>,
    groups: usize,
}

type Caps = Vec<Option<(usize, usize)>>;

struct Parser<'a> {
    s: &'a [char],
    i: usize,
    groups: usize,
}

impl Parser<'_> {
    fn alt(&mut self, nested: bool) -> Result<Vec<Vec<Item>>, String> {
        let mut alts = vec![Vec::new()];
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return if nested {
                    Err("unclosed group".into())
                } else {
                    Ok(alts)
                };
            };
            self.i += 1;
            let node = match c {
                ')' if nested => return Ok(alts),
                ')' => return Err("unmatched )".into()),
                '|' => {
                    alts.push(Vec::new());
                    continue;
                }
                '(' => {
                    let cap = if self.s.get(self.i) == Some(&'?') {
                        if self.s.get(self.i + 1) != Some(&':') {
                            return Err("only (?: groups are supported".into());
                        }
                        self.i += 2;
                        None
                    } else {
                        self.groups += 1;
                        Some(self.groups - 1)
                    };
                    Node::Group(cap, self.alt(true)?)
                }
                '[' => self.class()?,
                '^' => Node::Bol,
                '\\' => {
                    let e = *self.s.get(self.i).ok_or("dangling \\")?;
                    self.i += 1;
                    if e.is_ascii_alphanumeric() {
                        return Err(format!("unsupported escape \\{e}"));
                    }
                    Node::Lit(e)
                }
                '.' | '$' | '*' | '+' | '?' | '{' | '}' | ']' => {
                    return Err(format!("unsupported or misplaced {c}"));
                }
                c => Node::Lit(c),
            };
            let (min, max) = self.quantifier()?;
            if matches!(node, Node::Bol) && (min, max) != (1, 1) {
                return Err("a quantified ^".into());
            }
            alts.last_mut()
                .ok_or("no branch")?
                .push(Item { node, min, max });
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.s.get(self.i) == Some(&'^');
        if neg {
            self.i += 1;
        }
        let mut ranges = Vec::new();
        loop {
            let c = *self.s.get(self.i).ok_or("unclosed class")?;
            self.i += 1;
            let lo = match c {
                ']' if !ranges.is_empty() => return Ok(Node::Class(neg, ranges)),
                '\\' => {
                    let e = *self.s.get(self.i).ok_or("dangling \\")?;
                    self.i += 1;
                    if e.is_ascii_alphanumeric() {
                        return Err(format!("unsupported escape \\{e}"));
                    }
                    e
                }
                c => c,
            };
            if self.s.get(self.i) == Some(&'-') && self.s.get(self.i + 1).is_some_and(|n| *n != ']')
            {
                let hi = self.s[self.i + 1];
                self.i += 2;
                if hi < lo {
                    return Err("reversed range".into());
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
    }

    fn quantifier(&mut self) -> Result<(usize, usize), String> {
        let q = match self.s.get(self.i) {
            Some('?') => (0, 1),
            Some('*') => (0, NO_MAX),
            Some('+') => (1, NO_MAX),
            Some('{') => {
                let end = self.s[self.i..]
                    .iter()
                    .position(|c| *c == '}')
                    .ok_or("unclosed {")?
                    + self.i;
                let body: String = self.s[self.i + 1..end].iter().collect();
                self.i = end;
                let num = |t: &str| {
                    t.parse::<usize>()
                        .map_err(|_| format!("bad count {{{body}}}"))
                };
                match body.split_once(',') {
                    None => (num(&body)?, num(&body)?),
                    Some((a, "")) => (num(a)?, NO_MAX),
                    Some((a, b)) => (num(a)?, num(b)?),
                }
            }
            _ => return Ok((1, 1)),
        };
        self.i += 1;
        Ok(q)
    }
}

/// The table, parsed. An error names the line and the kind.
pub fn load() -> Result<Vec<Pattern>, String> {
    load_from(PATTERNS)
}

/// Whether a repetition with no upper bound sits anywhere in `alts`, groups inside groups included.
fn holds_unbounded(alts: &[Vec<Item>]) -> bool {
    alts.iter()
        .flatten()
        .any(|it| it.max == NO_MAX || matches!(&it.node, Node::Group(_, a) if holds_unbounded(a)))
}

/// What a table line may not hold, beyond what the parser refuses: a count with n > m (it would match nothing, so a
/// typo would silently switch a detector off), an empty group, and a group that repeats and holds an unbounded
/// repetition or has no upper bound itself (exponential time on a long line, so a table edit could hang the step).
fn check_items(items: &[Item]) -> Result<(), String> {
    for it in items {
        if it.min > it.max {
            return Err(format!("a count {{{},{}}} with n > m", it.min, it.max));
        }
        if let Node::Group(_, alts) = &it.node {
            if alts.iter().any(Vec::is_empty) {
                return Err("an empty group or branch".into());
            }
            if it.max == NO_MAX {
                return Err("a group with no upper bound".into());
            }
            if it.max > 1 && holds_unbounded(alts) {
                return Err("a repeated group that holds an unbounded repetition".into());
            }
            for a in alts {
                check_items(a)?;
            }
        }
    }
    Ok(())
}

/// `load` over a given text, for the tests.
pub fn load_from(text: &str) -> Result<Vec<Pattern>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, re) = line
            .split_once('\t')
            .ok_or_else(|| format!("key-patterns.txt:{}: no tab", n + 1))?;
        let chars: Vec<char> = re.chars().collect();
        let mut p = Parser {
            s: &chars,
            i: 0,
            groups: 0,
        };
        let alts = p
            .alt(false)
            .map_err(|e| format!("key-patterns.txt:{}: {kind}: {e}", n + 1))?;
        if alts.len() != 1 {
            return Err(format!("key-patterns.txt:{}: {kind}: top-level |", n + 1));
        }
        if alts[0].is_empty() {
            return Err(format!(
                "key-patterns.txt:{}: {kind}: an empty regex",
                n + 1
            ));
        }
        check_items(&alts[0]).map_err(|e| format!("key-patterns.txt:{}: {kind}: {e}", n + 1))?;
        out.push(Pattern {
            kind: kind.to_string(),
            alts: alts.into_iter().next().unwrap_or_default(),
            groups: p.groups,
        });
    }
    Ok(out)
}

fn one(node: &Node, text: &[char], pos: usize) -> bool {
    match (node, text.get(pos)) {
        (Node::Lit(l), Some(c)) => l == c,
        (Node::Class(neg, rs), Some(c)) => rs.iter().any(|(a, b)| a <= c && c <= b) != *neg,
        _ => false,
    }
}

type Cont<'a> = &'a mut dyn FnMut(usize, &mut Caps) -> bool;

/// The step budget of one pattern over one line: `STEPS_PER_CHAR` for each character of the line and a floor. A table
/// line the parser accepts can still cost time quadratic in the line (`[A-Za-z0-9]{4,}x` tried at every start of a long
/// line, or the `jwt` pattern over a line of `-eyJ` repeated), and the line may be a 4 MiB minified file. Over the
/// budget the match gives up and `find_all` reports it: the file fails the scan, it is never passed as clean.
const STEPS_PER_CHAR: u64 = 400;
const STEPS_FLOOR: u64 = 200_000;

thread_local! {
    static STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static LIMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
}

/// Counts `n` steps; true once the budget of this line is spent.
fn tick(n: u64) -> bool {
    STEPS.with(|s| {
        let v = s.get().saturating_add(n);
        s.set(v);
        v > LIMIT.with(std::cell::Cell::get)
    })
}

fn run(items: &[Item], idx: usize, text: &[char], pos: usize, caps: &mut Caps, k: Cont) -> bool {
    if tick(1) {
        return false;
    }
    let Some(it) = items.get(idx) else {
        return k(pos, caps);
    };
    match &it.node {
        Node::Bol => pos == 0 && run(items, idx + 1, text, pos, caps, k),
        Node::Group(..) => group(it, 0, items, idx, text, pos, caps, k),
        node => {
            let mut n = 0;
            while n < it.max && one(node, text, pos + n) {
                n += 1;
            }
            if tick(n as u64) || n < it.min {
                return false;
            }
            for take in (it.min..=n).rev() {
                if run(items, idx + 1, text, pos + take, caps, k) {
                    return true;
                }
            }
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn group(
    it: &Item,
    count: usize,
    items: &[Item],
    idx: usize,
    text: &[char],
    pos: usize,
    caps: &mut Caps,
    k: Cont,
) -> bool {
    let Node::Group(cap, alts) = &it.node else {
        return false;
    };
    if count < it.max {
        for alt in alts {
            let saved = caps.clone();
            let hit = run(alt, 0, text, pos, caps, &mut |p, c: &mut Caps| {
                if let Some(i) = cap {
                    c[*i] = Some((pos, p));
                }
                if p == pos {
                    // An empty pass cannot loop: carry on with the rest.
                    return count + 1 >= it.min && run(items, idx + 1, text, p, c, &mut *k);
                }
                group(it, count + 1, items, idx, text, p, c, &mut *k)
            });
            if hit {
                return true;
            }
            *caps = saved;
        }
    }
    count >= it.min && run(items, idx + 1, text, pos, caps, k)
}

/// Every match of `p` in one line: (start of capture group 1, end of it). Without a group, the whole match. `Err` when
/// the line is over the step budget: what was found is then not the whole answer.
pub fn find_all(p: &Pattern, line: &[char]) -> Result<Vec<(usize, usize)>, String> {
    STEPS.with(|s| s.set(0));
    LIMIT.with(|l| l.set(STEPS_FLOOR + STEPS_PER_CHAR * line.len() as u64));
    let mut hits = Vec::new();
    let mut start = 0;
    while start <= line.len() {
        let mut caps: Caps = vec![None; p.groups];
        let mut end = None;
        run(&p.alts, 0, line, start, &mut caps, &mut |e, _| {
            end = Some(e);
            true
        });
        if let Some(e) = end {
            let (a, b) = caps.first().copied().flatten().unwrap_or((start, e));
            hits.push((a, b));
            start = e.max(start + 1);
        } else {
            start += 1;
        }
    }
    if STEPS.with(std::cell::Cell::get) > LIMIT.with(std::cell::Cell::get) {
        return Err(format!(
            "{}: over the step budget of {} steps",
            p.kind,
            STEPS_FLOOR + STEPS_PER_CHAR * line.len() as u64
        ));
    }
    Ok(hits)
}

/// The hits in one text, as (kind, 1-based line, first four characters of the key), and, when a line went over its step
/// budget, what names the line and the pattern. The scan stops at that line; the hits found before it are kept, so a
/// refused file still says what was already seen.
pub fn scan_text(pats: &[Pattern], text: &str) -> (Vec<(String, usize, String)>, Option<String>) {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        for p in pats {
            match find_all(p, &chars) {
                Ok(found) => {
                    for (a, _) in found {
                        out.push((
                            p.kind.clone(),
                            n + 1,
                            chars[a..(a + 4).min(chars.len())].iter().collect(),
                        ));
                    }
                }
                Err(e) => return (out, Some(format!("line {}: {e}", n + 1))),
            }
        }
    }
    (out, None)
}

enum Got {
    Text(String),
    Binary,
    TooBig,
    Symlink,
    Gone,
}

fn read_text(path: &Path) -> Got {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Got::Gone;
    };
    // A symlink is never followed: it could point at a file outside the repository.
    if meta.file_type().is_symlink() {
        return Got::Symlink;
    }
    if !meta.is_file() {
        return Got::Gone;
    }
    if meta.len() > MAX_BYTES {
        return Got::TooBig;
    }
    let mut bytes = Vec::new();
    if fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .is_err()
    {
        return Got::Gone;
    }
    if bytes.iter().take(SNIFF_BYTES).any(|b| *b == 0) {
        return Got::Binary;
    }
    Got::Text(String::from_utf8_lossy(&bytes).into_owned())
}

/// The names of a NUL-separated list (`git ... -z`). A name that is not valid UTF-8 is an error naming it with its
/// bytes escaped: a lossy conversion would turn it into a path that does not exist, which is then taken for a deleted
/// file and never scanned.
pub fn split_z(bytes: &[u8]) -> Result<Vec<String>, String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            String::from_utf8(s.to_vec())
                .map_err(|_| format!("a touched path is not valid UTF-8: {}", s.escape_ascii()))
        })
        .collect()
}

/// `git <args>` in `root`, whose output is a `-z` list of names.
pub fn git_z(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    split_z(&o.stdout)
}

fn tracked(root: &Path) -> Result<Vec<String>, String> {
    git_z(root, &["ls-files", "-z"])
}

/// Whether a touched path is gone from the work tree. A path that cannot be told apart from absent because it cannot be
/// statted (a parent directory without search permission) is NOT absent: it is handed over, and `key-scan` fails on it.
pub fn is_absent(root: &Path, path: &str) -> bool {
    matches!(root.join(path).try_exists(), Ok(false))
}

/// The touched paths that are to be handed to `key-scan`, and how many directory paths (a submodule) were left out. A
/// path whose `lstat` fails is handed over, so that `key-scan` names it and fails the step.
pub struct Handover {
    pub files: Vec<String>,
    pub directories: usize,
}

pub fn hand_over(root: &Path, touched: &[String]) -> Handover {
    let mut h = Handover {
        files: Vec::new(),
        directories: 0,
    };
    for p in touched {
        match root.join(p).symlink_metadata() {
            Ok(m) if m.is_dir() => h.directories += 1,
            _ => h.files.push(p.clone()),
        }
    }
    h
}

fn quote(p: &str) -> String {
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// The shell command of the `secrets` step: `key-scan` over `files` first (`--` ends its options, so a file called
/// `--root` is a file), then, when `gitleaks` is given as (config option, files), the gitleaks pass, whose `--` does the
/// same. `None` for an empty list: a `--` with nothing after it would be read as "scan every tracked file". The
/// launcher is quoted: it is one path.
pub fn secrets_command(
    exe: &str,
    files: &[String],
    gitleaks: Option<(&str, &[String])>,
) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let list = files.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
    let mut cmd = format!("{} key-scan --root . -- {list}", quote(exe));
    if let Some((config, files)) = gitleaks.filter(|(_, f)| !f.is_empty()) {
        let list = files.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
        cmd.push_str(&format!(
            " && for f in {list}; do gitleaks dir --no-banner --redact {config} -- \"$f\" || exit 1; done"
        ));
    }
    Some(cmd)
}

/// What the `secrets` step does for the touched paths that exist (`existing`): the command, if there is anything to
/// hand over, the line to print about what was not handed over, and the note for a step that has nothing to scan.
pub struct SecretsPlan {
    pub command: Option<String>,
    pub count_line: Option<String>,
    pub skip_note: Option<String>,
}

pub fn secrets_plan(
    root: &Path,
    changed_len: usize,
    existing: &[String],
    exe: &str,
    gitleaks_config: Option<&str>,
) -> SecretsPlan {
    let gone = changed_len - existing.len();
    let handover = hand_over(root, existing);
    let count_line = (gone > 0 || handover.directories > 0).then(|| {
        format!(
            "check-fast: secrets: {gone} deleted file(s) and {} directory path(s) not handed to key-scan",
            handover.directories
        )
    });
    let skip_note = (handover.files.is_empty() && !existing.is_empty()).then(|| {
        "secret scan: every touched path is a directory (a submodule): nothing to hand to key-scan or gitleaks"
            .to_string()
    });
    let gitleaks_files: Vec<String> = existing
        .iter()
        .filter(|p| !p.starts_with("third_party/"))
        .cloned()
        .collect();
    SecretsPlan {
        command: secrets_command(
            exe,
            &handover.files,
            gitleaks_config.map(|c| (c, &gitleaks_files[..])),
        ),
        count_line,
        skip_note,
    }
}

/// `args` are what follows `key-scan`:`--root DIR` and file names. Exit 0 clean, 1 a match, 2 an error.
pub fn cmd(args: &[String]) -> Result<i32, String> {
    let mut root = PathBuf::from(".");
    let mut files: Vec<String> = Vec::new();
    let mut it = args.iter();
    // Options come first. A `--`, or the first argument that is not an option, ends them: everything after is a file
    // name, so a touched file called `--root` is a file and never an option.
    while let Some(a) = it.next() {
        if a == "--" {
            files.extend(it.by_ref().cloned());
        } else if a == "--root" {
            root = PathBuf::from(it.next().ok_or("--root needs a directory")?);
        } else if a.starts_with("--") {
            return Err(format!("key-scan: unknown option {a}"));
        } else {
            files.push(a.clone());
            files.extend(it.by_ref().cloned());
        }
    }
    let named = !files.is_empty();
    let pats = load()?;
    if files.is_empty() {
        files = tracked(&root)?;
    }
    let (mut scanned, mut skipped, mut hits, mut unreadable) = (0usize, 0usize, 0usize, 0usize);
    for f in &files {
        match read_text(&root.join(f)) {
            Got::Text(t) => {
                let (found, overrun) = scan_text(&pats, &t);
                for (kind, line, head) in found {
                    hits += 1;
                    println!("key-scan: {kind} at {f}:{line} (redacted: {head}...)");
                }
                match overrun {
                    None => scanned += 1,
                    Some(why) => {
                        // Not scanned to the end: an error, never a clean file, and not counted as scanned.
                        unreadable += 1;
                        println!("key-scan: {f}: too costly to scan, {why}");
                    }
                }
            }
            Got::Binary => {
                skipped += 1;
                println!("key-scan: skipped {f} (binary: a NUL byte in the first 8 KiB)");
            }
            Got::TooBig => {
                skipped += 1;
                println!("key-scan: skipped {f} (over 4 MiB)");
            }
            Got::Symlink => {
                skipped += 1;
                println!("key-scan: skipped {f} (symlink: never followed)");
            }
            Got::Gone => {
                skipped += 1;
                println!("key-scan: skipped {f} (not a readable regular file)");
                // A file the caller named that cannot be read is an error, not a clean file. In the whole-tree mode a
                // tracked path that is gone from the work tree (deleted, a submodule) is only said.
                if named {
                    unreadable += 1;
                }
            }
        }
    }
    println!("key-scan: {scanned} file(s) scanned, {skipped} skipped, {hits} match(es)");
    if unreadable > 0 {
        return Ok(2);
    }
    Ok(i32::from(hits > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(kind: &str) -> Pattern {
        load()
            .unwrap()
            .into_iter()
            .find(|p| p.kind == kind)
            .unwrap()
    }

    fn hit(kind: &str, s: &str) -> bool {
        let c: Vec<char> = s.chars().collect();
        !find_all(&pat(kind), &c).unwrap().is_empty()
    }

    #[test]
    fn the_table_parses_and_holds_every_kind() {
        let p = load().unwrap();
        for k in KINDS {
            assert!(p.iter().any(|p| p.kind == k), "kind {k} is missing");
        }
        assert_eq!(p.len(), KINDS.len(), "an unexpected kind in the table");
    }

    #[test]
    fn the_table_holds_no_key_shaped_literal() {
        let pats = load().unwrap();
        assert!(scan_text(&pats, PATTERNS).0.is_empty());
    }

    #[test]
    fn each_kind_matches_a_synthetic_sample_and_not_a_near_miss() {
        let a = |n: usize| "a".repeat(n);
        let big = |n: usize| "A".repeat(n);
        assert!(hit("google-api-key", &format!("k={}{}", "AIza", a(35))));
        assert!(!hit("google-api-key", &format!("{}{}", "AIza", a(34))));
        assert!(hit("github-token", &format!("{}_{}", "ghp", a(36))));
        assert!(!hit("github-token", &format!("{}_{}", "ghp", a(35))));
        assert!(hit("github-pat", &format!("{}_{}", "github_pat", a(22))));
        assert!(hit("aws-key", &format!("{}{}", "AKIA", big(16))));
        assert!(hit("aws-key", &format!(" {}{}", "ASIA", big(16))));
        assert!(!hit("aws-key", &format!("{}{}", "AKIA", big(15))));
        assert!(hit(
            "slack-token",
            &format!("{}-{}", "xoxb", "1".repeat(10))
        ));
        assert!(hit("openai-key", &format!("{}-{}", "sk", a(20))));
        assert!(hit("openai-key", &format!("{}-proj-{}", "sk", a(20))));
        assert!(!hit("openai-key", &format!("{}-{}", "sk", a(19))));
        assert!(!hit("openai-key", &format!("{}-{}", "task", a(30))));
        assert!(!hit("openai-key", &format!("{}-{}", "ask", a(30))));
        let pem = |m: &str| format!("{}BEGIN {m}-----", "-----");
        assert!(hit("pem-private-key", &pem("PRIVATE KEY")));
        assert!(hit("pem-private-key", &pem("OPENSSH PRIVATE KEY")));
        assert!(hit("pem-private-key", &pem("ENCRYPTED PRIVATE KEY")));
        assert!(hit("pem-private-key", &pem("PGP PRIVATE KEY BLOCK")));
        assert!(!hit("pem-private-key", &pem("CERTIFICATE")));
        assert!(!hit("pem-private-key", &pem("PUBLIC KEY")));
        let part = "x".repeat(12);
        assert!(hit("jwt", &format!("eyJ{part}.eyJ{part}.{part}")));
        assert!(!hit("jwt", &format!("eyJ{part}.{part}.{part}")));
    }

    #[test]
    fn the_alphanumeric_prefix_is_enforced() {
        let k = format!("{}{}", "AIza", "a".repeat(35));
        assert!(hit("google-api-key", &k));
        assert!(hit("google-api-key", &format!("\"{k}\"")));
        assert!(!hit("google-api-key", &format!("x{k}")));
    }

    #[test]
    fn a_bad_pattern_is_an_error() {
        for bad in ["a.b", "(abc", "a)", "[abc", "\\d", "(?=a)", "a{x}"] {
            let chars: Vec<char> = bad.chars().collect();
            let mut p = Parser {
                s: &chars,
                i: 0,
                groups: 0,
            };
            assert!(p.alt(false).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn a_table_line_that_would_silently_switch_a_detector_off_or_hang_it_is_an_error() {
        for (bad, why) in [
            ("a{5,3}", "n > m"),
            ("", "an empty regex"),
            ("(?:)", "an empty group"),
            ("(?:a|)", "an empty group or branch"),
            ("(?:a+)?b{2,1}", "n > m"),
            (
                "(?:a+){2}",
                "a repeated group that holds an unbounded repetition",
            ),
            (
                "(?:(?:a+)?){2}",
                "a repeated group that holds an unbounded repetition",
            ),
            ("(?:ab)*", "a group with no upper bound"),
            ("(?:a){1,}", "a group with no upper bound"),
        ] {
            let e = load_from(&format!("some-kind\t{bad}\n"))
                .err()
                .unwrap_or_else(|| panic!("{bad} was accepted"));
            assert!(
                e.contains("some-kind"),
                "{bad}: the error names no kind: {e}"
            );
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert!(load_from("k\t(?:a|b){1,2}c{2,3}\n").is_ok());
    }

    /// The table is kept byte-identical in the two repositories that share the scanner, and nothing but this pin makes
    /// an edit of one copy fail where it is made.
    const TABLE_LEN: usize = 1107;
    const TABLE_FNV1A: u64 = 0xdcee_2987_d1b5_92d3;

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn the_table_is_the_one_both_repositories_pin() {
        let (len, sum) = (PATTERNS.len(), fnv1a(PATTERNS.as_bytes()));
        assert_eq!(
            (len, sum),
            (TABLE_LEN, TABLE_FNV1A),
            "tools/ci/key-patterns.txt changed. The file is kept byte-identical in the SecretaryIOS and the LucinaOS \
             repositories: copy it to the other repository and set TABLE_LEN = {len} and TABLE_FNV1A = {sum:#x} in \
             tools/ci/lucina-ci/src/keyscan.rs, in both."
        );
    }

    /// A scratch directory that is removed on panic too, with the permissions a test took away given back first.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("keyscan-unit-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(self.0.join("locked"), fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touched() -> Vec<String> {
        [
            "--root",
            "has space.txt",
            "it's.txt",
            "adir",
            "ghost.txt",
            "locked/secret.txt",
        ]
        .map(String::from)
        .to_vec()
    }

    fn tree(tag: &str) -> Dir {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new(tag);
        for f in ["--root", "has space.txt", "it's.txt", "locked/secret.txt"] {
            fs::create_dir_all(d.0.join(f).parent().unwrap()).unwrap();
            fs::write(d.0.join(f), "text\n").unwrap();
        }
        fs::create_dir_all(d.0.join("adir")).unwrap();
        fs::set_permissions(d.0.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        d
    }

    #[test]
    fn a_path_that_cannot_be_statted_is_not_absent_and_is_handed_over() {
        let d = tree("stat");
        // As root a directory without permissions is still searchable: the stat then works and there is nothing to test.
        let locked = d.0.join("locked/secret.txt").symlink_metadata().is_err();
        assert!(is_absent(&d.0, "ghost.txt"));
        assert!(!is_absent(&d.0, "has space.txt"));
        if locked {
            assert!(
                !is_absent(&d.0, "locked/secret.txt"),
                "an unreadable path was taken for a deleted one"
            );
        }
        let existing: Vec<String> = touched()
            .into_iter()
            .filter(|p| !is_absent(&d.0, p))
            .collect();
        assert!(!existing.contains(&"ghost.txt".to_string()));
        let h = hand_over(&d.0, &existing);
        assert_eq!(h.directories, 1);
        let mut want: Vec<&str> = vec!["--root", "has space.txt", "it's.txt"];
        if locked {
            want.push("locked/secret.txt");
        }
        assert_eq!(h.files, want);
    }

    /// An executable script, written by a child that is reaped before anything execs it (the test thread keeping the
    /// file open while another thread forks gives ETXTBSY).
    fn write_exec(path: &Path, text: &str) {
        use std::io::Write;
        let mut c = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        assert!(c.wait().unwrap().success());
    }

    #[test]
    fn the_secrets_command_hands_every_name_over_as_one_argument() {
        let d = tree("cmd");
        let existing: Vec<String> = touched()
            .into_iter()
            .filter(|p| !is_absent(&d.0, p))
            .collect();
        let h = hand_over(&d.0, &existing);
        // A launcher in a directory whose name holds a space prints each argument it was handed, as the shell split it:
        // the launcher is one quoted word.
        fs::create_dir_all(d.0.join("tool dir")).unwrap();
        let exe = d.0.join("tool dir/launcher");
        write_exec(
            &exe,
            "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\"; done\n",
        );
        let cmd = secrets_command(exe.to_str().unwrap(), &h.files, None).unwrap();
        let out = Command::new("sh").args(["-c", &cmd]).output().unwrap();
        let got = String::from_utf8_lossy(&out.stdout);
        let mut want = vec![
            "[key-scan]",
            "[--root]",
            "[.]",
            "[--]",
            "[--root]",
            "[has space.txt]",
            "[it's.txt]",
        ];
        if h.files.iter().any(|f| f == "locked/secret.txt") {
            want.push("[locked/secret.txt]");
        }
        assert_eq!(got.lines().collect::<Vec<_>>(), want, "{cmd}");
        // The gitleaks pass follows with the same quoting, and its own `--`.
        let both = secrets_command(
            "lucina-ci",
            &["it's.txt".to_string()],
            Some(("--config .gitleaks.toml", &["has space.txt".to_string()])),
        );
        assert_eq!(
            both.as_deref(),
            Some(
                "'lucina-ci' key-scan --root . -- 'it'\\''s.txt' && for f in 'has space.txt'; do gitleaks dir --no-banner --redact --config .gitleaks.toml -- \"$f\" || exit 1; done"
            )
        );
    }

    #[test]
    fn an_empty_list_builds_no_command_because_a_bare_double_dash_means_the_whole_tree() {
        assert_eq!(secrets_command("lucina-ci", &[], None), None);
        assert_eq!(
            secrets_command("lucina-ci", &[], Some(("", &["a.rs".to_string()]))),
            None
        );
    }

    #[test]
    fn the_plan_hands_the_handover_list_to_the_command_and_guards_the_step() {
        let d = tree("plan");
        let exist =
            |names: &[&str]| -> Vec<String> { names.iter().map(|s| s.to_string()).collect() };
        // A file and a directory: only the file is handed over; the directory and the deleted one are counted.
        let existing = exist(&["has space.txt", "adir"]);
        let p = secrets_plan(&d.0, 3, &existing, "lucina-ci", None);
        let cmd = p.command.unwrap();
        assert!(cmd.ends_with("-- 'has space.txt'"), "{cmd}");
        assert!(!cmd.contains("adir"), "{cmd}");
        assert_eq!(
            p.count_line.as_deref(),
            Some(
                "check-fast: secrets: 1 deleted file(s) and 1 directory path(s) not handed to key-scan"
            )
        );
        assert_eq!(p.skip_note, None);
        // Only a directory: no command, and the log says why.
        let p = secrets_plan(&d.0, 1, &exist(&["adir"]), "lucina-ci", None);
        assert_eq!(p.command, None);
        assert!(
            p.skip_note
                .unwrap()
                .contains("every touched path is a directory")
        );
        // Only deleted paths: no command, no directory note.
        let p = secrets_plan(&d.0, 2, &[], "lucina-ci", None);
        assert_eq!(p.command, None);
        assert_eq!(p.skip_note, None);
        assert!(p.count_line.unwrap().contains("2 deleted file(s)"));
        // third_party is for key-scan but not for gitleaks, as before.
        let p = secrets_plan(
            &d.0,
            2,
            &exist(&["has space.txt", "it's.txt"]),
            "lucina-ci",
            Some("--config c.toml"),
        );
        assert!(
            p.command
                .unwrap()
                .contains("for f in 'has space.txt' 'it'\\''s.txt';")
        );
    }

    #[test]
    fn a_name_is_split_as_bytes_and_a_name_that_is_not_utf8_fails_loudly_naming_it() {
        assert_eq!(
            split_z(b"a\0b\nc\0\0d e\0").unwrap(),
            vec!["a", "b\nc", "d e"]
        );
        let e = split_z(b"ok\0bad\xff\xfename\0").unwrap_err();
        assert!(e.contains("not valid UTF-8"), "{e}");
        assert!(e.contains("bad\\xff\\xfename"), "{e}");
    }

    fn git(d: &Path, args: &[&str]) {
        let mut c = Command::new("git");
        c.args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(d);
        for n in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
            c.env_remove(n);
        }
        let o = c.output().unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    #[test]
    fn the_z_readers_return_a_name_with_a_newline_and_the_new_name_of_a_rename() {
        let d = Dir::new("zreaders");
        git(&d.0, &["init", "-q"]);
        fs::write(
            d.0.join("old\nname.txt"),
            "some text that is long enough to be a rename\n".repeat(5),
        )
        .unwrap();
        git(&d.0, &["add", "-A"]);
        git(&d.0, &["commit", "-q", "-m", "one"]);
        git(&d.0, &["mv", "old\nname.txt", "new name.txt"]);
        git(&d.0, &["commit", "-q", "-m", "two"]);
        assert_eq!(
            git_z(&d.0, &["diff", "--name-only", "-z", "HEAD~1", "HEAD"]).unwrap(),
            vec!["new name.txt"]
        );
        fs::write(d.0.join("un\ntracked.txt"), "x\n").unwrap();
        assert_eq!(
            git_z(&d.0, &["ls-files", "-z", "-o", "--exclude-standard"]).unwrap(),
            vec!["un\ntracked.txt"]
        );
        assert_eq!(tracked(&d.0).unwrap(), vec!["new name.txt"]);
    }

    #[test]
    fn a_touched_name_that_is_not_utf8_is_an_error_not_a_deleted_file() {
        use std::os::unix::ffi::OsStrExt;
        let d = Dir::new("nonutf8");
        git(&d.0, &["init", "-q"]);
        fs::write(d.0.join("fine.txt"), "x\n").unwrap();
        git(&d.0, &["add", "-A"]);
        git(&d.0, &["commit", "-q", "-m", "one"]);
        fs::write(
            d.0.join(std::ffi::OsStr::from_bytes(b"bad\xffname.txt")),
            "x\n",
        )
        .unwrap();
        let e = git_z(&d.0, &["ls-files", "-z", "-o", "--exclude-standard"]).unwrap_err();
        assert!(
            e.contains("not valid UTF-8") && e.contains("bad\\xffname.txt"),
            "{e}"
        );
    }

    #[test]
    fn a_long_line_that_makes_a_pattern_quadratic_goes_over_the_budget_fast() {
        let jwt = pat("jwt");
        let line: Vec<char> = "-eyJ".repeat(50_000).chars().collect();
        let t = std::time::Instant::now();
        let r = find_all(&jwt, &line);
        assert!(r.is_err(), "the adversarial line was not refused");
        assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    }

    #[test]
    fn an_ordinary_long_line_stays_inside_the_budget_and_still_finds_its_key() {
        let key = format!("{}{}", "AIza", "q".repeat(35));
        let line = format!("{} {key}", "x ".repeat(100_000));
        let pats = load().unwrap();
        let (found, over) = scan_text(&pats, &line);
        assert_eq!(over, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "google-api-key");
    }
}
