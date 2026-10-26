//! Differential test: `Policy::check`, `SecretRef::in_folder` and
//! `policy::hard_denied` against a plain-Rust copy of the Verus kernel
//! `verification/verus/secrets_policy.rs`. References are random
//! (`secret://<project>/<env>/<path>#<KEY>` built from pieces that hit the
//! hard list, the deny terms, look-alike folders and mixed case), checked
//! against random scopes (with and without the leading or trailing '/') and
//! allow lists. The deny-term match is the kernel's input `deny_hit`, computed
//! here with the real `policy::tokens`.
//!
//! `witness_s1_pin_with_digit_passes` keeps the known gap S1 visible: it
//! fails once `tokens` splits digits from letters (then flip it).

use lucina_secrets::policy::{hard_denied, tokens, Policy, Scope, DEFAULT_DENY, HARD_DENY};
use lucina_secrets::{SecretError, SecretRef};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub const SLASH: u8 = 47;
    pub const BAR: u8 = 124;
    pub const UNDERSCORE: u8 = 95;

    fn trim_end_exec(f: &[u8]) -> usize {
        let mut n = f.len();
        while n > 0 && f[n - 1] == SLASH {
            n -= 1;
        }
        n
    }

    pub fn in_folder_exec(p: &[u8], folder: &[u8]) -> bool {
        let n = trim_end_exec(folder);
        if n == 0 {
            return true;
        }
        let mut eq = p.len() == n;
        let mut pre = p.len() > n && p[n] == SLASH;
        let mut i = 0;
        while i < n {
            if i < p.len() && p[i] == folder[i] {
            } else {
                eq = false;
                pre = false;
            }
            i += 1;
        }
        eq || pre
    }

    fn lower_exec(c: u8) -> u8 {
        if (65..=90).contains(&c) {
            c + 32
        } else {
            c
        }
    }

    fn contains_exec(h: &[u8], t: &[u8]) -> bool {
        if t.len() > h.len() {
            return false;
        }
        let mut i = 0;
        while i <= h.len() - t.len() {
            let mut same = true;
            for k in 0..t.len() {
                if h[i + k] != t[k] {
                    same = false;
                }
            }
            if same {
                return true;
            }
            i += 1;
        }
        false
    }

    fn hay_exec(key: &[u8], path: &[u8]) -> Vec<u8> {
        let mut h: Vec<u8> = key.iter().map(|&c| lower_exec(c)).collect();
        h.push(BAR);
        h.extend(path.iter().map(|&c| lower_exec(c)));
        h
    }

    fn squash_exec(s: &[u8]) -> Vec<u8> {
        s.iter()
            .copied()
            .filter(|&c| {
                (48..=57).contains(&c) || (65..=90).contains(&c) || (97..=122).contains(&c)
            })
            .collect()
    }

    fn no_underscore_exec(s: &[u8]) -> Vec<u8> {
        s.iter().copied().filter(|&c| c != UNDERSCORE).collect()
    }

    pub fn hard_denied_exec(key: &[u8], path: &[u8], terms: &[Vec<u8>]) -> bool {
        let h = hay_exec(key, path);
        let sq = squash_exec(&h);
        for t in terms {
            let tu = no_underscore_exec(t);
            if contains_exec(&h, t) || contains_exec(&sq, &tu) {
                return true;
            }
        }
        false
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Decision {
        Ok,
        OutOfScope,
        HardDenied,
        Denied,
    }

    pub fn decide(scope_ok: bool, hard_hit: bool, allowed: bool, deny_hit: bool) -> Decision {
        if !scope_ok {
            return Decision::OutOfScope;
        }
        if hard_hit {
            return Decision::HardDenied;
        }
        if allowed {
            return Decision::Ok;
        }
        if deny_hit {
            return Decision::Denied;
        }
        Decision::Ok
    }
}

use kernel::Decision;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

const KEY_PIECES: &[&str] = &[
    "SUDO", "sudo", "Su", "do", "S", "U", "D", "O", "ROOT", "Ro", "ot", "Login", "LOGON", "PASS",
    "pass", "Word", "WORD", "PIN", "pin", "Pin", "2", "1", "_", "-", ".", "API", "KEY", "Token",
    "GMAIL", "SMTP", "x", "shadow", "Unix", "SSH", "passwd", "Pinecone", "code", "Phrase", "ADMIN",
    "NEZ", "giuseppe", "os", "user", "account", "system", "kodeord", "Parola", "TODOIST",
];
const SEGMENTS: &[&str] = &[
    "lucina",
    "lucina-evil",
    "lucinax",
    "luci",
    "Lucina",
    "github",
    "root",
    "sudo",
    "a.b",
    "x",
    "pass-word",
    "login_x",
];
const FOLDERS: &[&str] = &[
    "/lucina",
    "/lucina/",
    "/lucina//",
    "lucina",
    "/",
    "",
    "/lucina/github",
    "/luci",
    "/root",
];

fn deny_hit(deny: &[String], key: &str) -> bool {
    let toks = tokens(key);
    let lower = key.to_lowercase();
    deny.iter()
        .any(|t| toks.iter().any(|k| k == t) || (t.len() >= 6 && lower.contains(t.as_str())))
}

fn classify(res: &Result<(), SecretError>) -> Decision {
    match res {
        Ok(()) => Decision::Ok,
        Err(SecretError::OutOfScope(_)) => Decision::OutOfScope,
        Err(SecretError::Denied(_, why)) if why.contains("login, sudo or root") => {
            Decision::HardDenied
        }
        Err(SecretError::Denied(..)) => Decision::Denied,
        Err(e) => panic!("unexpected error {e:?}"),
    }
}

fn hard_terms() -> Vec<Vec<u8>> {
    HARD_DENY.iter().map(|t| t.as_bytes().to_vec()).collect()
}

#[test]
fn random_refs_agree_with_the_kernel() {
    let terms = hard_terms();
    let mut rng = Rng(0x05ec_2e75);
    let mut counts = [0usize; 4];
    let mut parsed = 0;
    for _ in 0..60_000 {
        let mut key = String::new();
        for _ in 0..1 + rng.below(5) {
            key.push_str(rng.pick(KEY_PIECES));
        }
        let mut path = String::new();
        for i in 0..rng.below(4) {
            if i > 0 {
                path.push('/');
            }
            // Mostly under the configured folder, so every outcome is common.
            if i == 0 && rng.below(4) != 0 {
                path.push_str("lucina");
            } else {
                path.push_str(rng.pick(SEGMENTS));
            }
        }
        let project = rng.pick(&["lucina", "lucina", "lucina", "pokoin"]);
        let env = rng.pick(&["prod", "prod", "prod", "dev"]);
        let Ok(r) = SecretRef::parse(&format!("secret://{project}/{env}/{path}#{key}")) else {
            continue;
        };
        parsed += 1;
        let folder = if rng.below(2) == 0 {
            "/lucina"
        } else {
            rng.pick(FOLDERS)
        };
        let mut policy = Policy::new(Scope {
            project: "lucina".into(),
            environment: "prod".into(),
            path: folder.into(),
        });
        if rng.below(4) == 0 {
            policy.allow_names.push(r.key.clone());
        }
        if rng.below(4) == 0 {
            policy.allow_names.push("SUDO_PASSWORD".into());
        }
        if rng.below(3) == 0 {
            policy.deny.push("gmail".into());
        }

        // The pieces against the real functions.
        let in_f = kernel::in_folder_exec(r.path.as_bytes(), folder.as_bytes());
        assert_eq!(in_f, r.in_folder(folder), "in_folder {} {folder:?}", r.path);
        let hard = kernel::hard_denied_exec(r.key.as_bytes(), r.path.as_bytes(), &terms);
        assert_eq!(hard, hard_denied(&r.key, &r.path).is_some(), "hard {r}");

        // The decision.
        let scope_ok = r.project == "lucina" && r.environment == "prod" && in_f;
        let allowed = policy.allow_names.iter().any(|a| a == &r.key);
        let want = kernel::decide(scope_ok, hard, allowed, deny_hit(&policy.deny, &r.key));
        let got = classify(&policy.check(&r));
        assert_eq!(
            got, want,
            "{r} in {folder:?}, allow {:?}",
            policy.allow_names
        );
        counts[got as usize] += 1;
    }
    assert!(parsed > 50_000, "parsed {parsed}");
    // Not vacuous: every outcome many times.
    assert!(counts.iter().all(|&n| n > 1_000), "{counts:?}");
}

#[test]
fn folder_scope_edges() {
    for (path, folder, want) in [
        ("/lucina", "/lucina", true),
        ("/lucina/github", "/lucina", true),
        ("/lucina-evil", "/lucina", false),
        ("/lucinax", "/lucina", false),
        ("/luci", "/lucina", false),
        ("/", "/lucina", false),
        ("/lucina", "lucina", false),
        ("/lucina/github", "/lucina/", true),
        ("/anything", "/", true),
        ("/anything", "", true),
    ] {
        assert_eq!(
            kernel::in_folder_exec(path.as_bytes(), folder.as_bytes()),
            want
        );
        let r = SecretRef::parse(&format!("secret://lucina/prod{path}#K")).unwrap();
        assert_eq!(r.in_folder(folder), want, "{path} in {folder}");
    }
}

#[test]
fn hard_terms_beat_case_separators_and_allow_names() {
    let terms = hard_terms();
    let mut policy = Policy::new(Scope {
        project: "lucina".into(),
        environment: "prod".into(),
        path: "/lucina".into(),
    });
    for key in [
        "S-u-D_o",
        "Login.Password",
        "ssh.pass",
        "R_O_O_T",
        "x.unix.y",
        "OS-Password",
    ] {
        policy.allow_names.push(key.into());
        let r = SecretRef::parse(&format!("secret://lucina/prod/lucina#{key}")).unwrap();
        assert!(
            kernel::hard_denied_exec(key.as_bytes(), r.path.as_bytes(), &terms),
            "{key}"
        );
        assert_eq!(classify(&policy.check(&r)), Decision::HardDenied, "{key}");
    }
}

/// Known gap S1: a deny term glued to a digit is not a token of its own, so
/// these names pass although "pin" is a deny term. Flip to `Denied` when the
/// tokenizer splits digits from letters.
#[test]
fn witness_s1_pin_with_digit_passes() {
    let policy = Policy::new(Scope {
        project: "lucina".into(),
        environment: "prod".into(),
        path: "/lucina".into(),
    });
    assert!(DEFAULT_DENY.contains(&"pin"));
    for key in ["BANK_PIN2", "PIN1", "SIM_PIN0", "card-pin4"] {
        let r = SecretRef::parse(&format!("secret://lucina/prod/lucina#{key}")).unwrap();
        assert_eq!(classify(&policy.check(&r)), Decision::Ok, "{key}");
    }
    // The same names with a separator before the digit are refused.
    for key in ["BANK_PIN_2", "PIN-1"] {
        let r = SecretRef::parse(&format!("secret://lucina/prod/lucina#{key}")).unwrap();
        assert_eq!(classify(&policy.check(&r)), Decision::Denied, "{key}");
    }
}
