//! Differential test: `source_path::allowed` against a plain-Rust copy of the Verus kernel
//! `verification/verus/secret_source_path.rs`, on paths built from the pieces that matter: the real
//! roots and deny lists, look-alike siblings, `.` and `..`, doubled slashes, NUL, mixed case.
//!
//! The same corpus runs against copies with one bug planted each. Every bug must be caught: a
//! corpus that misses one would pass a broken check too. Hits on a real deny list must exist as well,
//! or the corpus never exercised that list.

use lucina_secrets::source_path::{allowed, key_refused, Rules, DENY_COMPONENTS, DENY_CONTAINS, DENY_NAME_STARTS, DENY_PREFIXES, REFUSED_KEYS};

/// The kernel's exec part. `Bug` plants one fault at a time.
mod kernel {
    pub const SLASH: u8 = 47;
    pub const DOT: u8 = 46;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Bug {
        None,
        /// `/a/bc` counts as below `/a/b`.
        NoRootBoundary,
        /// `/home/.sshx/y` counts as below the denied `/home/.ssh`.
        NoDenyBoundary,
        /// A path equal to a denied tree passes.
        NoDenyEqual,
        /// `..` is let through.
        DotDotAllowed,
        /// `.` is let through.
        DotAllowed,
        /// `//` is let through.
        EmptyComponentAllowed,
        /// A trailing `/` is let through.
        TrailingSlashAllowed,
        /// NUL is let through.
        NulAllowed,
        /// A relative path is let through.
        RelativeAllowed,
        /// `<root>/file`, with no folder between, passes.
        NoDepth,
        /// `.SSH` is not `.ssh`.
        CaseSensitive,
        /// The component match ignores the end of the name: `.sshx` hits `.ssh`.
        ComponentNoEnd,
        /// The name-start list is skipped.
        NoNameStarts,
        /// The substring list is skipped.
        NoContains,
        /// The denied-tree list is skipped.
        NoDenyTrees,
        /// The component list is skipped.
        NoComponents,
        /// Any root passes the path, with no root check.
        NoRootCheck,
    }

    fn low(c: u8) -> u8 {
        if (65..=90).contains(&c) {
            c + 32
        } else {
            c
        }
    }

    fn at(h: &[u8], t: &[u8], i: usize) -> bool {
        if i + t.len() > h.len() {
            return false;
        }
        let mut k = 0;
        while k < t.len() {
            if h[i + k] != t[k] {
                return false;
            }
            k += 1;
        }
        true
    }

    fn clean(p: &[u8], bug: Bug) -> bool {
        let n = p.len();
        if n == 0 || (p[0] != SLASH && bug != Bug::RelativeAllowed) {
            return false;
        }
        let mut i = 0;
        while i < n {
            if p[i] == 0 && bug != Bug::NulAllowed {
                return false;
            }
            if p[i] == SLASH {
                if i + 1 == n {
                    if bug != Bug::TrailingSlashAllowed {
                        return false;
                    }
                } else if p[i + 1] == SLASH {
                    if bug != Bug::EmptyComponentAllowed {
                        return false;
                    }
                } else if p[i + 1] == DOT {
                    if (i + 2 == n || p[i + 2] == SLASH) && bug != Bug::DotAllowed {
                        return false;
                    }
                    if i + 2 < n && p[i + 2] == DOT && (i + 3 == n || p[i + 3] == SLASH) && bug != Bug::DotDotAllowed {
                        return false;
                    }
                }
            }
            i += 1;
        }
        true
    }

    fn under(p: &[u8], d: &[u8], boundary: bool) -> bool {
        if d.is_empty() || d[d.len() - 1] == SLASH || p.len() <= d.len() {
            return false;
        }
        at(p, d, 0) && (!boundary || p[d.len()] == SLASH)
    }

    fn deep(p: &[u8], root: &[u8], bug: Bug) -> bool {
        if !under(p, root, bug != Bug::NoRootBoundary) {
            return false;
        }
        if bug == Bug::NoDepth {
            return true;
        }
        let mut j = root.len() + 1;
        while j < p.len() {
            if p[j] == SLASH {
                return true;
            }
            j += 1;
        }
        false
    }

    fn component(lp: &[u8], c: &[u8], bug: Bug) -> bool {
        let mut i = 0;
        while i < lp.len() {
            if lp[i] == SLASH && at(lp, c, i + 1) && (bug == Bug::ComponentNoEnd || i + 1 + c.len() == lp.len() || lp[i + 1 + c.len()] == SLASH) {
                return true;
            }
            i += 1;
        }
        false
    }

    fn start(lp: &[u8], c: &[u8]) -> bool {
        let mut i = 0;
        while i < lp.len() {
            if lp[i] == SLASH && at(lp, c, i + 1) {
                return true;
            }
            i += 1;
        }
        false
    }

    fn contains(lp: &[u8], c: &[u8]) -> bool {
        let mut i = 0;
        while i <= lp.len() {
            if at(lp, c, i) {
                return true;
            }
            i += 1;
        }
        false
    }

    pub struct Lists<'a> {
        pub roots: &'a [Vec<u8>],
        pub prefixes: &'a [Vec<u8>],
        pub components: &'a [Vec<u8>],
        pub starts: &'a [Vec<u8>],
        pub substrings: &'a [Vec<u8>],
    }

    pub fn allowed(p: &[u8], l: &Lists<'_>, bug: Bug) -> bool {
        if !clean(p, bug) {
            return false;
        }
        if bug != Bug::NoRootCheck && !l.roots.iter().any(|r| deep(p, r, bug)) {
            return false;
        }
        if bug != Bug::NoDenyTrees {
            for d in l.prefixes {
                if (p == d.as_slice() && bug != Bug::NoDenyEqual) || under(p, d, bug != Bug::NoDenyBoundary) {
                    return false;
                }
            }
        }
        let lp: Vec<u8> = if bug == Bug::CaseSensitive { p.to_vec() } else { p.iter().map(|c| low(*c)).collect() };
        if bug != Bug::NoComponents && l.components.iter().any(|c| component(&lp, c, bug)) {
            return false;
        }
        if bug != Bug::NoNameStarts && l.starts.iter().any(|c| start(&lp, c)) {
            return false;
        }
        if bug != Bug::NoContains && l.substrings.iter().any(|c| contains(&lp, c)) {
            return false;
        }
        true
    }

    pub const BUGS: &[Bug] = &[
        Bug::NoRootBoundary,
        Bug::NoDenyBoundary,
        Bug::NoDenyEqual,
        Bug::DotDotAllowed,
        Bug::DotAllowed,
        Bug::EmptyComponentAllowed,
        Bug::TrailingSlashAllowed,
        Bug::NulAllowed,
        Bug::RelativeAllowed,
        Bug::NoDepth,
        Bug::CaseSensitive,
        Bug::ComponentNoEnd,
        Bug::NoNameStarts,
        Bug::NoContains,
        Bug::NoDenyTrees,
        Bug::NoComponents,
        Bug::NoRootCheck,
    ];
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, v: &'a [String]) -> &'a str {
        &v[self.next() as usize % v.len()]
    }
}

fn rules() -> Rules {
    // One root sits inside a denied tree: the tree must still win.
    Rules::new(
        vec![
            b"/home/nez/.config".to_vec(),
            b"/home/nez/Projects".to_vec(),
            b"/etc/apps".to_vec(),
            b"/".to_vec(),
            b"relative".to_vec(),
        ],
        vec![b"/home/nez/.config/tokens".to_vec(), b"/home/nez/.config/app/keeper.token".to_vec()],
    )
}

/// Paths from every list's own words, with look-alikes and path tricks mixed in.
fn corpus() -> Vec<Vec<u8>> {
    let mut starts: Vec<String> = vec![
        "/home/nez/.config".into(),
        "/home/nez/Projects".into(),
        "/home/nez/.config-evil".into(),
        "/home/nez/Projects2".into(),
        "/home/nez/.config/tokens".into(),
        "/home/nez/.config/tokensx".into(),
        "/etc/apps".into(),
        "/etc".into(),
        "/tmp".into(),
        "".into(),
        "relative".into(),
    ];
    starts.extend(DENY_PREFIXES.iter().map(|s| s.to_string()));
    let mut pieces: Vec<String> = vec![
        "app".into(),
        "flareon-list".into(),
        "work".into(),
        ".env".into(),
        "login.key".into(),
        "..".into(),
        ".".into(),
        "".into(),
        "...".into(),
        "..x".into(),
        ".x".into(),
        "x\0y".into(),
        "ok".into(),
    ];
    for list in [DENY_COMPONENTS, DENY_NAME_STARTS, DENY_CONTAINS] {
        for t in list {
            pieces.push(t.to_string());
            pieces.push(t.to_uppercase());
            pieces.push(format!("{t}x"));
            pieces.push(format!("x{t}"));
        }
    }
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = Vec::new();
    for _ in 0..60_000 {
        let mut p = r.pick(&starts).to_owned();
        for _ in 0..(r.next() % 4 + 1) {
            p.push('/');
            p.push_str(r.pick(&pieces));
        }
        if r.next() % 20 == 0 {
            p.push('/');
        }
        out.push(p.into_bytes());
    }
    // The cases a random walk rarely lands on, one per bug at least.
    for p in [
        "/home/nez/.config/key",
        "/home/nez/.config",
        "/home/nez/.config-evil/a/b",
        "/home/nez/.config/tokens",
        "/home/nez/.config/tokens/a/b",
        "/home/nez/.config/app/keeper.token",
        "/home/nez/.config/tokensx/a/b",
        "/etc/apps/x/y",
        "/etc",
        "relative/a/b",
        "/home/nez/Projects/a/.SSH/k",
        "/home/nez/Projects/a/.sshx/k",
        "/home/nez/Projects/a/id_ed25519.pub",
        "/home/nez/Projects/a/my-google-password",
        "/home/nez/Projects/a/b/",
        "/home/nez/Projects/a//b",
        "/home/nez/Projects/a/./b",
        "/home/nez/Projects/a/../b",
        "/home/nez/Projects/a/b\0c",
    ] {
        out.push(p.as_bytes().to_vec());
    }
    out
}

#[test]
fn the_real_check_is_the_kernel() {
    let r = rules();
    let l = kernel::Lists {
        roots: &r.roots,
        prefixes: &r.deny_prefixes,
        components: &r.deny_components,
        starts: &r.deny_name_starts,
        substrings: &r.deny_contains,
    };
    let (mut yes, mut no) = (0, 0);
    for p in corpus() {
        let want = kernel::allowed(&p, &l, kernel::Bug::None);
        assert_eq!(allowed(&p, &r), want, "{:?}", String::from_utf8_lossy(&p));
        if want {
            yes += 1;
        } else {
            no += 1;
        }
    }
    assert!(yes > 500 && no > 500, "the corpus tests both ways: {yes} allowed, {no} refused");
}

#[test]
fn every_planted_bug_is_caught() {
    let r = rules();
    let l = kernel::Lists {
        roots: &r.roots,
        prefixes: &r.deny_prefixes,
        components: &r.deny_components,
        starts: &r.deny_name_starts,
        substrings: &r.deny_contains,
    };
    let corpus = corpus();
    for bug in kernel::BUGS {
        let caught = corpus.iter().any(|p| kernel::allowed(p, &l, *bug) != allowed(p, &r));
        assert!(caught, "the corpus would not notice {bug:?}");
    }
}

#[test]
fn the_hostile_paths_of_the_design() {
    let r = Rules::new(vec![b"/home/nez/.config".to_vec(), b"/home/nez/Projects".to_vec()], Vec::new());
    for p in [
        "/home/nez/.ssh/id_ed25519",
        "/home/nez/.gnupg/private-keys-v1.d/a.key",
        "/home/nez/.config/flareon-list/../../.ssh/id_ed25519",
        "/home/nez/.config/google-chrome/Default/Login Data",
        "/home/nez/.config/infisical-mcp/owner.token",
        "/home/nez/.zcode/cli/config.json",
        "/etc/infisical-broker/broker.toml",
        "/etc/shadow",
        "/proc/self/environ",
    ] {
        assert!(!allowed(p.as_bytes(), &r), "{p}");
    }
    assert!(allowed(b"/home/nez/.config/flareon-list/login.key", &r));
}

/// The Google password's name, and only names like it.
#[test]
fn key_refused_is_the_list() {
    fn plain(key: &[u8]) -> bool {
        let norm: Vec<u8> = key.iter().map(|c| if *c == b'-' { b'_' } else { c.to_ascii_lowercase() }).collect();
        REFUSED_KEYS.iter().any(|t| norm.windows(t.len()).any(|w| w == t.as_bytes()))
    }
    let mut r = Rng(0x1234_5678_9abc_def1);
    let words: Vec<String> = ["GOOGLE", "PASSWORD", "PASS", "API", "KEY", "SUDO", "google", "-", "_", "KEEP", "NEZ", "PWD"].iter().map(|s| s.to_string()).collect();
    for _ in 0..20_000 {
        let mut k = String::new();
        for _ in 0..(r.next() % 5 + 1) {
            k.push_str(r.pick(&words));
        }
        assert_eq!(key_refused(k.as_bytes()), plain(k.as_bytes()), "{k}");
    }
}
