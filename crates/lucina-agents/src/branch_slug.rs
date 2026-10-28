//! Branch slugs (Paseo `protocol/branch-slug.js`): `slugify` for worktree and branch
//! names made from a prompt or a title, `validate` for names a caller passes.

pub const MAX_SLUG_LENGTH: usize = 50;

/// Paseo `slugify`: lowercase, every run of other characters becomes `-`, no `-` at
/// either end, at most 50 characters (cut at the last `-` when that is past 25).
pub fn slugify(input: &str) -> String {
    let lower = input.to_lowercase();
    let mut slug = String::new();
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c);
            gap = false;
        } else {
            gap = true;
        }
    }
    if slug.len() <= MAX_SLUG_LENGTH {
        return slug;
    }
    let truncated = &slug[..MAX_SLUG_LENGTH];
    match truncated.rfind('-') {
        Some(i) if i > MAX_SLUG_LENGTH / 2 => truncated[..i].to_string(),
        _ => truncated.trim_end_matches('-').to_string(),
    }
}

/// Paseo `validateBranchSlug`; the error texts are Paseo's.
pub fn validate(slug: &str) -> Result<(), &'static str> {
    if slug.is_empty() {
        return Err("Branch name cannot be empty");
    }
    if slug.chars().count() > 100 {
        return Err("Branch name too long (max 100 characters)");
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/') {
        return Err("Branch name must contain only lowercase letters, numbers, hyphens, and forward slashes");
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err("Branch name cannot start or end with a hyphen");
    }
    if slug.contains("--") {
        return Err("Branch name cannot have consecutive hyphens");
    }
    Ok(())
}

/// A git ref name that git itself accepts (`git check-ref-format --branch`, the
/// rules that matter for names agentd makes or is given).
pub fn valid_git_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name != "@"
        && !name.split('/').any(|part| part.is_empty() || part.starts_with('.'))
        && !name.chars().any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the login redirect bug!"), "fix-the-login-redirect-bug");
        assert_eq!(slugify("  --Ünïcode__x  "), "n-code-x");
        assert_eq!(validate("feat/x"), Ok(()));
        assert!(validate("Feat").is_err());
        assert!(validate("a--b").is_err());
        assert!(valid_git_branch("agent/fix-login"));
        assert!(!valid_git_branch("a..b"));
        assert!(!valid_git_branch("x.lock"));
        assert!(!valid_git_branch("a/.b"));
    }
}
