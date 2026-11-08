//! Which files the secret keeper may read: the pure check on a path, with no filesystem in it.
//!
//! Lucina also reads untrusted chats and web pages, so a hidden instruction can name any path. A
//! path passes only if it is clean, stands at least one folder below a configured root, and hits
//! nothing on the deny lists. The caller checks both the path as asked and the path with its
//! symlinks resolved, so a link can't lead out (`lucina-server`, `secret_keep/files.rs`).
//!
//! The rule is the Verus kernel `verification/verus/secret_source_path.rs`;
//! `tests/verus_source_path.rs` runs [`allowed`] against a plain copy of it and plants bugs.

/// Trees never read, whatever the roots say.
pub const DENY_PREFIXES: &[&str] = &[
    "/etc",
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/boot",
    "/root",
    "/var/log",
    "/var/lib",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
];

/// A folder or file with one of these names, anywhere in the path, in any case. Lowercase here.
pub const DENY_COMPONENTS: &[&str] = &[
    // Keys and logins.
    ".ssh",
    ".gnupg",
    ".gpg",
    ".password-store",
    "keyrings",
    ".netrc",
    ".pgpass",
    ".git-credentials",
    ".git",
    "authorized_keys",
    "known_hosts",
    "secrets",
    // Cloud credentials.
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    "gcloud",
    ".gcloud",
    "gh",
    "rclone",
    "cloudflared",
    // Browser profiles.
    "google-chrome",
    "google-chrome-beta",
    "chromium",
    "bravesoftware",
    "microsoft-edge",
    "vivaldi",
    "opera",
    ".mozilla",
    "mozilla",
    "firefox",
    // Other agents' logins.
    ".zcode",
    "zcode",
    ".codex",
    ".claude",
    ".cursor",
    // Infisical's own config, the broker's tokens, Lucina's own settings.
    "infisical",
    ".infisical",
    "infisical-mcp",
    "infisical-broker",
    "lucina-rs",
];

/// A folder or file whose name starts like this, in any case: `id_ed25519`, `id_ed25519.pub`.
pub const DENY_NAME_STARTS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_xmss"];

/// Anywhere in the path, in any case: the Google password is never read or moved.
pub const DENY_CONTAINS: &[&str] = &[
    "google_password",
    "google-password",
    "googlepassword",
    "google_passwd",
    "google_pwd",
];

/// Env keys never read or moved, in any case: see [`key_refused`].
pub const REFUSED_KEYS: &[&str] = &[
    "google_password",
    "googlepassword",
    "google_passwd",
    "google_pass",
    "google_pwd",
    "sudo_pass",
    "nez_password",
    "giuseppe_password",
];

/// The lists a path is checked against. Byte strings, since a path is bytes.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub roots: Vec<Vec<u8>>,
    pub deny_prefixes: Vec<Vec<u8>>,
    pub deny_components: Vec<Vec<u8>>,
    pub deny_name_starts: Vec<Vec<u8>>,
    pub deny_contains: Vec<Vec<u8>>,
}

fn bytes(list: &[&str]) -> Vec<Vec<u8>> {
    list.iter().map(|s| s.as_bytes().to_vec()).collect()
}

impl Rules {
    /// The fixed deny lists, plus the caller's roots and extra trees. Nothing here can shorten a
    /// fixed list.
    pub fn new(roots: Vec<Vec<u8>>, extra_deny: Vec<Vec<u8>>) -> Self {
        let mut deny_prefixes = bytes(DENY_PREFIXES);
        deny_prefixes.extend(extra_deny);
        Self {
            roots,
            deny_prefixes,
            deny_components: bytes(DENY_COMPONENTS),
            deny_name_starts: bytes(DENY_NAME_STARTS),
            deny_contains: bytes(DENY_CONTAINS),
        }
    }
}

const SLASH: u8 = b'/';
const DOT: u8 = b'.';

fn lower(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        c + 32
    } else {
        c
    }
}

fn lower_all(p: &[u8]) -> Vec<u8> {
    p.iter().map(|c| lower(*c)).collect()
}

fn at(h: &[u8], t: &[u8], i: usize) -> bool {
    i + t.len() <= h.len() && &h[i..i + t.len()] == t
}

/// Absolute, no NUL, and every component non-empty and neither `.` nor `..`. A trailing slash fails.
pub fn clean(p: &[u8]) -> bool {
    if p.is_empty() || p[0] != SLASH {
        return false;
    }
    let n = p.len();
    for i in 0..n {
        if p[i] == 0 {
            return false;
        }
        if p[i] == SLASH {
            if i + 1 == n || p[i + 1] == SLASH {
                return false;
            }
            if p[i + 1] == DOT {
                if i + 2 == n || p[i + 2] == SLASH {
                    return false;
                }
                if p[i + 2] == DOT && (i + 3 == n || p[i + 3] == SLASH) {
                    return false;
                }
            }
        }
    }
    true
}

/// Below `d` at a component boundary: `/a/bc` is not below `/a/b`.
fn under(p: &[u8], d: &[u8]) -> bool {
    !d.is_empty() && d[d.len() - 1] != SLASH && p.len() > d.len() && at(p, d, 0) && p[d.len()] == SLASH
}

/// At least one folder below `root`: `<root>/<folder>/<file>`.
fn deep(p: &[u8], root: &[u8]) -> bool {
    if !under(p, root) {
        return false;
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

fn component_hit(lp: &[u8], c: &[u8]) -> bool {
    for i in 0..lp.len() {
        if lp[i] == SLASH && at(lp, c, i + 1) && (i + 1 + c.len() == lp.len() || lp[i + 1 + c.len()] == SLASH) {
            return true;
        }
    }
    false
}

fn name_start_hit(lp: &[u8], c: &[u8]) -> bool {
    for i in 0..lp.len() {
        if lp[i] == SLASH && at(lp, c, i + 1) {
            return true;
        }
    }
    false
}

fn contains(lp: &[u8], c: &[u8]) -> bool {
    for i in 0..=lp.len() {
        if at(lp, c, i) {
            return true;
        }
    }
    false
}

/// May the keeper read this path at all?
pub fn allowed(p: &[u8], r: &Rules) -> bool {
    if !clean(p) || !r.roots.iter().any(|root| deep(p, root)) {
        return false;
    }
    if r.deny_prefixes.iter().any(|d| p == d.as_slice() || under(p, d)) {
        return false;
    }
    let lp = lower_all(p);
    !r.deny_components.iter().any(|c| component_hit(&lp, c))
        && !r.deny_name_starts.iter().any(|c| name_start_hit(&lp, c))
        && !r.deny_contains.iter().any(|c| contains(&lp, c))
}

/// An env key that is never read or moved. `GOOGLE_PASSWORD` stays where it is, under any spelling
/// or prefix (`KEEP_GOOGLE_PASSWORD`, `my-google-password`).
pub fn key_refused(key: &[u8]) -> bool {
    let norm: Vec<u8> = key.iter().map(|c| if *c == b'-' { b'_' } else { lower(*c) }).collect();
    REFUSED_KEYS.iter().any(|t| contains(&norm, t.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::new(
            vec![b"/home/nez/.config".to_vec(), b"/home/nez/Projects".to_vec()],
            vec![b"/home/nez/.config/keeper-tokens".to_vec()],
        )
    }

    fn ok(p: &str) -> bool {
        allowed(p.as_bytes(), &rules())
    }

    #[test]
    fn inside_the_roots() {
        for p in [
            "/home/nez/.config/flareon-list/login.key",
            "/home/nez/Projects/Work/app/.env",
            "/home/nez/Projects/Work/app/config/.env.local",
            "/home/nez/.config/a/b/c/d.key",
        ] {
            assert!(ok(p), "{p}");
        }
    }

    #[test]
    fn outside_or_not_clean() {
        for p in [
            "",
            "relative/.env",
            "~/.config/app/key",
            "/home/nez/.config/flareon-list/../../.ssh/id_ed25519",
            "/home/nez/.config/app/./key",
            "/home/nez/.config/app//key",
            "/home/nez/.config/app/",
            "/home/nez/.config/app/.",
            "/home/nez/.config/app/..",
            "/home/nez/.config/app/k\0ey",
            "/home/nez/.config-evil/app/key",
            "/home/nez/Projects2/app/.env",
            "/home/nez/.config",
            "/home/nez/.config/",
            // Directly in a root: there is no app folder.
            "/home/nez/.config/key",
            "/home/nez/Projects/.env",
            "/home/nez/other/app/.env",
            "/tmp/app/.env",
        ] {
            assert!(!ok(p), "{p:?}");
        }
    }

    #[test]
    fn denied_everywhere() {
        for p in [
            "/home/nez/.config/app/.ssh/id",
            "/home/nez/Projects/app/.SSH/key",
            "/home/nez/Projects/app/.gnupg/secring",
            "/home/nez/.config/google-chrome/Default/Login Data",
            "/home/nez/.config/chromium/Default/Cookies",
            "/home/nez/.config/gcloud/credentials.db",
            "/home/nez/Projects/app/.aws/credentials",
            "/home/nez/.config/gh/hosts.yml",
            "/home/nez/Projects/app/.zcode/x",
            "/home/nez/.config/infisical-mcp/owner.token",
            "/home/nez/.config/infisical/config.json",
            "/home/nez/.config/lucina-rs/mode.env",
            "/home/nez/.config/keeper-tokens/k/x",
            "/home/nez/Projects/app/id_ed25519",
            "/home/nez/Projects/app/id_ed25519.pub",
            "/home/nez/Projects/app/ID_RSA",
            "/home/nez/Projects/app/.git/config",
            "/home/nez/Projects/app/GOOGLE_PASSWORD",
            "/home/nez/Projects/app/my-google-password.txt",
            "/home/nez/Projects/app/.git-credentials",
        ] {
            assert!(!ok(p), "{p}");
        }
    }

    #[test]
    fn a_deny_name_inside_a_longer_name_is_not_a_hit() {
        for p in [
            "/home/nez/Projects/app/my.sshfoo/key",
            "/home/nez/Projects/gh-tool/.env",
            "/home/nez/.config/operator/key",
        ] {
            assert!(ok(p), "{p}");
        }
    }

    #[test]
    fn a_root_inside_a_denied_tree_gives_nothing() {
        let mut r = rules();
        r.roots = vec![b"/etc".to_vec(), b"/".to_vec(), b"/home/nez/.ssh".to_vec()];
        for p in ["/etc/app/key", "/home/nez/app/key", "/home/nez/.ssh/x/id"] {
            assert!(!allowed(p.as_bytes(), &r), "{p}");
        }
    }

    #[test]
    fn the_lists_are_lowercase() {
        for list in [DENY_COMPONENTS, DENY_NAME_STARTS, DENY_CONTAINS, REFUSED_KEYS] {
            for t in list {
                assert_eq!(*t, t.to_lowercase());
            }
        }
    }

    #[test]
    fn google_password_under_any_spelling() {
        for k in [
            "GOOGLE_PASSWORD",
            "google_password",
            "Google-Password",
            "KEEP_GOOGLE_PASSWORD",
            "KEPT_GOOGLE_PASSWORD_2",
            "GOOGLE_PWD",
            "GOOGLE_PASS",
            "SUDO_PASSWORD",
        ] {
            assert!(key_refused(k.as_bytes()), "{k}");
        }
        for k in ["GOOGLE_API_KEY", "KEEP_FLAREON_LIST_LOGIN", "DB_PASSWORD", "GOOGLE_OAUTH_SECRET"] {
            assert!(!key_refused(k.as_bytes()), "{k}");
        }
    }
}
