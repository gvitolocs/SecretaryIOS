//! Read the Node Lucina's env files for the few names the engine needs
//! ([`crate::config::ENV_NAMES`] plus `url_env` health targets), so the
//! systemd unit does not have to load every secret Lucina has.
//!
//! Format: `KEY=VALUE` lines (`export ` prefix, `#` comments and matching
//! quotes tolerated). Later files win, as with docker compose `env_file`;
//! the process environment wins over all files.

use std::collections::HashMap;
use std::path::Path;

/// Parse one env file, keeping only `wanted` names.
pub fn parse(text: &str, wanted: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if !wanted.iter().any(|w| w == k) {
            continue;
        }
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        out.insert(k.to_owned(), v.to_owned());
    }
    out
}

/// Every wanted value from `files` (missing files are skipped, with a warning).
pub fn load(files: &[impl AsRef<Path>], wanted: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for f in files {
        let f = f.as_ref();
        match std::fs::read_to_string(f) {
            Ok(text) => out.extend(parse(&text, wanted)),
            Err(e) => tracing::warn!(file = %f.display(), error = %e, "env file not read"),
        }
    }
    out
}

/// A lookup: the process environment, then the files' values.
pub fn lookup(files: HashMap<String, String>) -> impl Fn(&str) -> Option<String> + Clone {
    move |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| files.get(k).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_quotes_comments() {
        let wanted = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
        let m = parse(
            "# x\nA=1\nexport B=\"two words\"\nSECRET=nope\nC='3'\nbroken\n",
            &wanted,
        );
        assert_eq!(m.get("A").unwrap(), "1");
        assert_eq!(m.get("B").unwrap(), "two words");
        assert_eq!(m.get("C").unwrap(), "3");
        assert!(!m.contains_key("SECRET"));
    }
}
