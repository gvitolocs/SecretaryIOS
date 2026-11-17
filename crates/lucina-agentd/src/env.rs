//! The built environment a process agentd (or the merge/deploy executor) starts: an
//! allowlist taken from the starting process's own environment, a denylist on top, plus
//! the names a deploy recipe declares. Processes agentd starts inherit agentd's
//! environment otherwise — and a restart from a shell, or a future EnvironmentFile,
//! would hand every gate command, deploy step and supervised agent whatever secrets
//! that environment holds.

use std::ffi::OsString;

/// The names a child may keep, from the starting process's own environment.
const ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TERM",
    "TZ",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "LUCINA_AGENTD_SOCKET",
    // A socket path, not a credential: the agent socket's name is how a child talks back
    // to agentd, and the value carries no secret material.
    "SSH_AUTH_SOCK",
];

/// Allowlist prefixes: `LC_*`, the locale overrides; `BQ_*`, the bq queue's own variables.
const ALLOW_PREFIX: &[&str] = &["LC_", "BQ_"];

/// A name containing any of these (case-insensitive) is dropped even when allowlisted: it
/// looks like a credential. Today no allowlisted name matches; the list is what stops one
/// from sneaking in when the allowlist is extended.
const DENY_CONTAINS: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "COOKIE",
    "SESSION",
];

/// A name starting with any of these is dropped even when allowlisted: a cloud or AI
/// vendor's variables are credentials wholesale.
const DENY_PREFIX: &[&str] = &[
    "CLOUDFLARE_",
    "VERCEL_",
    "AWS_",
    "GITHUB_",
    "GH_",
    "OPENAI_",
    "ANTHROPIC_",
    "MISTRAL_",
    "DEEPSEEK_",
    "INFISICAL_",
];

/// The names this process passes on, built from its own environment: the allowlist, the
/// denylist on top, plus `extra` (the names a deploy recipe declares; they pass the
/// denylist — that is the point of declaring them).
pub fn scrubbed_env(extra: &[&str]) -> Vec<(OsString, OsString)> {
    scrubbed_env_from(std::env::vars_os(), extra).0
}

/// [`scrubbed_env`] built from `source` instead of this process's environment, together
/// with how many of `source`'s names did not pass — a count only, for the job log; never
/// names, never values. `source` is a parameter so tests pass a stand-in instead of
/// mutating the process environment.
pub fn scrubbed_env_from(
    source: impl IntoIterator<Item = (OsString, OsString)>,
    extra: &[&str],
) -> (Vec<(OsString, OsString)>, usize) {
    let mut out = Vec::new();
    let mut not_passed = 0usize;
    for (name, value) in source {
        let name_str = name.to_string_lossy();
        let declared = extra.iter().any(|e| name_str == *e);
        let allowlisted = ALLOW.iter().any(|a| name_str == *a)
            || ALLOW_PREFIX.iter().any(|p| name_str.starts_with(p));
        let keep = declared || (allowlisted && !denied(&name_str));
        if keep {
            out.push((name, value));
        } else {
            not_passed += 1;
        }
    }
    (out, not_passed)
}

/// Whether `name` looks like a credential: it contains a deny word (case-insensitive) or
/// starts with a vendor prefix. `BQ_*` and `SSH_AUTH_SOCK` are exempt: the queue's
/// variables are the bq queue's own bookkeeping (job, class, slot — the queue this very
/// command runs under), and the agent socket name is a path, with no secret material in it.
fn denied(name: &str) -> bool {
    if name == "SSH_AUTH_SOCK" || name.starts_with("BQ_") {
        return false;
    }
    let up = name.to_ascii_uppercase();
    DENY_CONTAINS.iter().any(|d| up.contains(d)) || DENY_PREFIX.iter().any(|p| up.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn source(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
            .collect()
    }

    fn names(built: &[(OsString, OsString)]) -> Vec<String> {
        built
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    fn value<'a>(built: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
        built
            .iter()
            .find(|(k, _)| k == OsStr::new(name))
            .map(|(_, v)| v.as_os_str())
    }

    #[test]
    fn allowlisted_names_pass_with_their_values_and_others_do_not() {
        let (built, not_passed) = scrubbed_env_from(
            source(&[
                ("PATH", "/usr/bin:/bin"),
                ("HOME", "/home/fake"),
                ("TMPDIR", "/fake/tmp"),
                ("CARGO_TARGET_DIR", "/fake/target"),
                ("LC_ALL", "C"),
                ("BQ_JOB_ID", "q-1"),
                ("FAKE_API_TOKEN", "fake-token"),
                ("CLOUDFLARE_FAKE_ID", "fake-id"),
                ("DEPLOY_WEBHOOK", "https://fake.example"),
                ("PATH_EXTRA", "x"),
            ]),
            &[],
        );
        let got = names(&built);
        for n in [
            "PATH",
            "HOME",
            "TMPDIR",
            "CARGO_TARGET_DIR",
            "LC_ALL",
            "BQ_JOB_ID",
        ] {
            assert!(got.contains(&n.to_string()), "{got:?}");
        }
        for n in [
            "FAKE_API_TOKEN",
            "CLOUDFLARE_FAKE_ID",
            "DEPLOY_WEBHOOK",
            "PATH_EXTRA",
        ] {
            assert!(!got.contains(&n.to_string()), "{got:?}");
        }
        assert_eq!(not_passed, 4);
        assert_eq!(value(&built, "TMPDIR"), Some(OsStr::new("/fake/tmp")));
    }

    #[test]
    fn the_denylist_removes_allowlisted_names_that_look_like_credentials() {
        for n in [
            "SESSION_PATH",
            "COOKIE_NOTICE",
            "MY_SECRET_DIR",
            "PASSWD_HELPER",
            "KEY_STORE",
            "CREDENTIAL_HELPER",
            "DB_PASSWORD",
            "DB_PASSWD",
            "GITHUB_TOKEN",
            "GH_SESSION",
            "AWS_SECRET",
        ] {
            assert!(denied(n), "{n}");
        }
        for n in [
            "PATH",
            "HOME",
            "TMPDIR",
            "SSH_AUTH_SOCK",
            "BQ_SESSION",
            "BQ_TOKEN",
        ] {
            assert!(!denied(n), "{n}");
        }
        // The exemptions keep the queue's own variables and the agent socket, whatever
        // their names look like.
        let (built, not_passed) = scrubbed_env_from(
            source(&[("BQ_SESSION", "s"), ("SSH_AUTH_SOCK", "/run/agent.sock")]),
            &[],
        );
        assert_eq!(
            names(&built),
            vec!["BQ_SESSION".to_string(), "SSH_AUTH_SOCK".to_string()]
        );
        assert_eq!(not_passed, 0);
    }

    #[test]
    fn a_declared_name_passes_even_though_the_denylist_would_take_it_and_only_where_declared() {
        assert!(denied("FAKE_DEPLOY_TOKEN"), "the fixture is deny-shaped");
        let src = source(&[("FAKE_DEPLOY_TOKEN", "x"), ("PATH", "/usr/bin")]);
        let (built, not_passed) = scrubbed_env_from(src.clone(), &["FAKE_DEPLOY_TOKEN"]);
        assert!(names(&built).contains(&"FAKE_DEPLOY_TOKEN".to_string()));
        assert_eq!(not_passed, 0);
        // Without the declaration it does not pass — and it is not a gate command's to have.
        let (built, not_passed) = scrubbed_env_from(src, &[]);
        assert!(!names(&built).contains(&"FAKE_DEPLOY_TOKEN".to_string()));
        assert_eq!(not_passed, 1);
    }
}
