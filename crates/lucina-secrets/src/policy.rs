//! What Lucina may resolve: one folder, no login/sudo/root passwords.
//!
//! - **Scope**: the ref's project alias, environment and folder must be the
//!   configured ones (Infisical enforces the same with a read-only machine
//!   identity on that folder; this is the client-side copy).
//! - **Hard deny** ([`HARD_DENY`], not configurable): any name or path that
//!   looks like a login, sudo or root password. Lucina never gets, stores or
//!   uses those, even if someone puts one in her folder.
//! - **Deny terms** ([`DEFAULT_DENY`], configurable): personal passwords and
//!   PINs in general. A service password a tool really needs can be allowed
//!   by exact name (`policy.allow_names`), never one that hits the hard list.

use crate::{SecretError, SecretRef};

/// Substrings (of the lowercased name and path) that are always refused.
pub const HARD_DENY: &[&str] = &[
    "sudo",
    "root",
    "login",
    "logon",
    "passwd",
    "shadow",
    "unix",
    "sshpass",
    "ssh_pass",
    "user_password",
    "account_password",
    "os_password",
    "system_password",
    "admin_password",
    "nez_password",
    "giuseppe_password",
    "loginpassword",
    "sudopassword",
];

/// Name tokens refused unless allowed by exact name (configurable).
pub const DEFAULT_DENY: &[&str] = &[
    "password",
    "passwords",
    "pwd",
    "passphrase",
    "pin",
    "pincode",
    "passcode",
    "adgangskode",
    "kodeord",
    "parola",
];

#[derive(Debug, Clone)]
pub struct Scope {
    pub project: String,
    pub environment: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub scope: Scope,
    pub deny: Vec<String>,
    pub allow_names: Vec<String>,
    pub min_value_len: usize,
}

/// Lowercased tokens of a name (`GitHubToken` -> `git`, `hub`, `token`;
/// `SUDO_PASS` -> `sudo`, `pass`).
pub fn tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Why a name/path is hard-denied, if it is.
pub fn hard_denied(key: &str, path: &str) -> Option<&'static str> {
    let hay = format!("{}|{}", key.to_lowercase(), path.to_lowercase());
    let squashed: String = hay.chars().filter(|c| c.is_alphanumeric()).collect();
    HARD_DENY
        .iter()
        .find(|t| hay.contains(*t) || squashed.contains(&t.replace('_', "")))
        .copied()
}

impl Policy {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            deny: DEFAULT_DENY.iter().map(|s| (*s).to_owned()).collect(),
            allow_names: Vec::new(),
            min_value_len: 8,
        }
    }

    /// Scope, then the hard list, then the deny terms.
    pub fn check(&self, r: &SecretRef) -> Result<(), SecretError> {
        if r.project != self.scope.project
            || r.environment != self.scope.environment
            || !r.in_folder(&self.scope.path)
        {
            return Err(SecretError::OutOfScope(r.to_string()));
        }
        if let Some(t) = hard_denied(&r.key, &r.path) {
            return Err(SecretError::Denied(
                r.to_string(),
                format!(
                    "looks like a login, sudo or root password (\"{t}\"); Lucina never uses those"
                ),
            ));
        }
        if self.allow_names.iter().any(|a| a == &r.key) {
            return Ok(());
        }
        let toks = tokens(&r.key);
        let lower = r.key.to_lowercase();
        if let Some(t) = self
            .deny
            .iter()
            .find(|t| toks.iter().any(|k| k == *t) || (t.len() >= 6 && lower.contains(t.as_str())))
        {
            return Err(SecretError::Denied(
                r.to_string(),
                format!("names a personal password or PIN (\"{t}\"); allow it by exact name in policy.allow_names if a tool needs it"),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Policy {
        Policy::new(Scope {
            project: "lucina".into(),
            environment: "prod".into(),
            path: "/lucina".into(),
        })
    }

    fn r(s: &str) -> SecretRef {
        SecretRef::parse(s).unwrap()
    }

    #[test]
    fn scope_is_one_folder() {
        let p = p();
        assert!(p
            .check(&r("secret://lucina/prod/lucina#TODOIST_TOKEN"))
            .is_ok());
        assert!(p
            .check(&r("secret://lucina/prod/lucina/github#GITHUB_TOKEN"))
            .is_ok());
        for out in [
            "secret://lucina/dev/lucina#TODOIST_TOKEN",
            "secret://pokoin/prod/lucina#TODOIST_TOKEN",
            "secret://lucina/prod/#TODOIST_TOKEN",
            "secret://lucina/prod/owner#GMAIL_TOKEN",
            "secret://lucina/prod/lucinax#T_KEY",
        ] {
            assert!(
                matches!(p.check(&r(out)), Err(SecretError::OutOfScope(_))),
                "{out}"
            );
        }
    }

    #[test]
    fn login_sudo_root_passwords_are_always_refused() {
        let mut p = p();
        p.allow_names = vec!["SUDO_PASSWORD".into(), "ROOT_PW".into()];
        for name in [
            "SUDO_PASSWORD",
            "sudoPass",
            "ROOT_PW",
            "NEZ_LOGIN",
            "LoginPassword",
            "UNIX_PASSWORD",
            "nez-passwd",
            "NEZ_PASSWORD",
            "OS_PASSWORD",
            "ssh_pass_nezopt",
        ] {
            let res = p.check(&r(&format!("secret://lucina/prod/lucina#{name}")));
            assert!(
                matches!(res, Err(SecretError::Denied(..))),
                "{name}: {res:?}"
            );
        }
        // in the path too
        let res = p.check(&r("secret://lucina/prod/lucina/sudo#VALUE_X"));
        assert!(matches!(res, Err(SecretError::Denied(..))));
    }

    #[test]
    fn deny_terms_are_configurable() {
        let mut p = p();
        let smtp = r("secret://lucina/prod/lucina#SMTP_PASSWORD");
        assert!(matches!(p.check(&smtp), Err(SecretError::Denied(..))));
        assert!(p.check(&r("secret://lucina/prod/lucina#BANK_PIN")).is_err());
        assert!(p
            .check(&r("secret://lucina/prod/lucina#PINECONE_API_KEY"))
            .is_ok());
        p.allow_names = vec!["SMTP_PASSWORD".into()];
        assert!(p.check(&smtp).is_ok());
        p.deny.push("gmail".into());
        assert!(p
            .check(&r("secret://lucina/prod/lucina#GMAIL_APP_TOKEN"))
            .is_err());
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokens("GitHubToken"), vec!["git", "hub", "token"]);
        assert_eq!(tokens("SUDO_PASS"), vec!["sudo", "pass"]);
        assert_eq!(tokens("api-key2"), vec!["api", "key2"]);
    }
}
