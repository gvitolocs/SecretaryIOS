//! Scopes and the authenticated caller.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a token may do.
///
/// - `read`: timeline, shopping list, reminders, gym/body summaries, status,
///   resources and prompts.
/// - `chat`: `ask_lucina` (a full Lucina turn; Lucina may act on it through
///   her own confirmation flow).
/// - `write`: add to / tick off the shopping list, create / cancel reminders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Chat,
    Write,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Read, Scope::Chat, Scope::Write];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Chat => "chat",
            Scope::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Scope::Read),
            "chat" => Some(Scope::Chat),
            "write" => Some(Scope::Write),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Scope::Read => "Read your timeline, shopping list, reminders and gym/body summaries",
            Scope::Chat => "Talk to Lucina (she may act through her own confirmation flow)",
            Scope::Write => "Add to / tick off the shopping list, create and cancel reminders",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of scopes, kept sorted and unique.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scopes(Vec<Scope>);

impl Scopes {
    pub fn all() -> Self {
        Self(Scope::ALL.to_vec())
    }

    pub fn of(it: impl IntoIterator<Item = Scope>) -> Self {
        let mut v: Vec<Scope> = it.into_iter().collect();
        v.sort();
        v.dedup();
        Self(v)
    }

    /// Space- or comma-separated list (`"read chat"`, `"read,chat,write"`).
    /// Unknown names are an error.
    pub fn parse_list(s: &str) -> Result<Self, String> {
        let mut out = Vec::new();
        for part in s.split([' ', ',']).filter(|p| !p.is_empty()) {
            out.push(Scope::parse(part).ok_or_else(|| format!("unknown scope {part:?}"))?);
        }
        Ok(Self::of(out))
    }

    /// Like [`Self::parse_list`] but drops unknown names (OAuth `scope`
    /// requests may carry scopes we do not offer).
    pub fn parse_lenient(s: &str) -> Self {
        Self::of(s.split([' ', ',']).filter_map(Scope::parse))
    }

    pub fn contains(&self, s: Scope) -> bool {
        self.0.contains(&s)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.0.iter().copied()
    }

    pub fn intersect(&self, other: &Scopes) -> Scopes {
        Self::of(self.iter().filter(|s| other.contains(*s)))
    }

    pub fn is_subset_of(&self, other: &Scopes) -> bool {
        self.iter().all(|s| other.contains(s))
    }

    /// Space-separated, as OAuth wants it.
    pub fn to_oauth(&self) -> String {
        self.iter().map(Scope::as_str).collect::<Vec<_>>().join(" ")
    }
}

impl fmt::Display for Scopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.iter().map(Scope::as_str).collect::<Vec<_>>().join(","))
    }
}

/// How the caller authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerKind {
    /// `mcp_…` service token from `secretary-mcp token issue`.
    ServiceToken,
    /// `mcpa_…` OAuth access token (ChatGPT connector).
    OAuth,
}

/// The authenticated client of one MCP request, put into the request
/// extensions by the auth middleware.
#[derive(Debug, Clone)]
pub struct Caller {
    /// `claude-code`, `chatgpt`, `grok`, …: shown to Lucina and in the logs.
    pub name: String,
    pub kind: CallerKind,
    pub scopes: Scopes,
    /// Token id (service token) or grant id (OAuth): for the logs.
    pub id: String,
}

impl Caller {
    pub fn has(&self, s: Scope) -> bool {
        self.scopes.contains(s)
    }
}

/// Caller names: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn valid_caller_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        let s = Scopes::parse_list("write, read read").unwrap();
        assert_eq!(s.to_string(), "read,write");
        assert_eq!(s.to_oauth(), "read write");
        assert!(Scopes::parse_list("read admin").is_err());
        assert_eq!(
            Scopes::parse_lenient("openid read offline_access").to_oauth(),
            "read"
        );
        assert!(Scopes::parse_list("read")
            .unwrap()
            .is_subset_of(&Scopes::all()));
        assert!(!Scopes::all().is_subset_of(&Scopes::parse_list("read").unwrap()));
    }

    #[test]
    fn caller_names() {
        for ok in ["claude-code", "chatgpt", "grok", "a", "x1-2"] {
            assert!(valid_caller_name(ok), "{ok}");
        }
        for bad in ["", "-x", "Claude", "a b", "x_y", &"a".repeat(33)] {
            assert!(!valid_caller_name(bad), "{bad}");
        }
    }
}
