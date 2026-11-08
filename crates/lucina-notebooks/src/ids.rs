//! Ids (`nb` / `s` / `c` / `m` / `i` / `p` + 10 hex) and timestamps (RFC 3339, UTC, seconds).

pub fn new_id(prefix: &str) -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &hex[..10])
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A lowercase slug (`a-z0-9-`, at most `max` characters) for transformation ids.
pub fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= max {
            break;
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Whether `id` looks like one of ours (`prefix` + 10 lowercase hex).
pub fn valid(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|h| h.len() == 10 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_slugs() {
        let id = new_id("nb");
        assert!(valid(&id, "nb"), "{id}");
        assert!(!valid("nbXYZ", "nb"));
        assert_eq!(slug("Dense Summary!", 40), "dense-summary");
        assert_eq!(slug("  Ünïcode  & more ", 40), "n-code-more");
    }
}
