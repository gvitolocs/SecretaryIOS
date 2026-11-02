//! Scopes and the authenticated caller.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a token may do.
///
/// - `read`: timeline, shopping list, reminders, gym/body summaries, notes,
///   status, library records, resources and prompts.
/// - `chat`: `ask_lucina` (a full Lucina turn; Lucina may act on it through
///   her own confirmation flow).
/// - `write`: add to / tick off the shopping list, create / cancel reminders,
///   save and append to notes, generate extractive library briefs.
/// - `computer`: drive Lucina's own sandboxed computers (her Linux desktop and
///   Android sandbox): tasks, commands, clicks, screenshots. Owner tokens only:
///   it is granted on the consent page or with `add-scope`, never implied.
/// - `agents`: Lucina Agents, the owner's coding agents on nezopt (`crate::agents`):
///   run, steer, read logs and diffs, stop and archive. Separate from `write`
///   and never implied: not in an OAuth grant unless the client asks for it and
///   the owner ticks it, and off by default on the consent page.
/// - `notebooks`: Lucina Notebooks (`crate::notebooks`), the owner's source-grounded
///   notebooks: read them, add sources, ask questions, make insights and podcasts.
///   Separate from `read` and `write` because the sources are the owner's documents,
///   and never implied: explicit-only, like `agents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Chat,
    Write,
    Computer,
    Agents,
    Notebooks,
}

impl Scope {
    pub const ALL: [Scope; 6] = [Scope::Read, Scope::Chat, Scope::Write, Scope::Computer, Scope::Agents, Scope::Notebooks];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Chat => "chat",
            Scope::Write => "write",
            Scope::Computer => "computer",
            Scope::Agents => "agents",
            Scope::Notebooks => "notebooks",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Scope::Read),
            "chat" => Some(Scope::Chat),
            "write" => Some(Scope::Write),
            "computer" => Some(Scope::Computer),
            "agents" => Some(Scope::Agents),
            "notebooks" => Some(Scope::Notebooks),
            _ => None,
        }
    }

    /// Granted only when asked for by name (OAuth `scope`, `--scopes`, `add-scope`).
    pub fn explicit_only(self) -> bool {
        matches!(self, Scope::Agents | Scope::Notebooks)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Scope::Read => {
                "Read your timeline, shopping list, reminders, calendar, tasks, notes, library and gym/body summaries"
            }
            Scope::Chat => "Talk to Lucina (she may act through her own confirmation flow)",
            Scope::Computer => "Use Lucina's own sandboxed computers (her Linux desktop and Android sandbox): run tasks and commands, click and type, take screenshots",
            Scope::Write => "Add to / tick off the shopping list, create and cancel reminders, propose calendar events (you confirm them in the app), save notes, generate library briefs, add and complete tasks",
            Scope::Agents => "Run and steer your coding agents (Lucina Agents on nezopt) in worktrees: start runs, send follow-ups, read logs, diffs and files, stop and archive. It can never answer their permission requests, push or open PRs",
            Scope::Notebooks => "Use your Lucina Notebooks: read notebooks and their sources, add sources (links, text, files from Lucina's desktop, notes), ask questions answered only from the sources with citations, run transformations and make podcasts. It cannot delete anything",
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

    /// The most a caller may hold, whatever it asked for and whatever is on its grant. Mistral's connector
    /// reads and talks to Lucina: no writes and no computers until the owner decides otherwise.
    pub fn limit_for_caller(caller: &str) -> Option<Self> {
        (caller == "mistral").then(|| Self::of([Scope::Read, Scope::Chat]))
    }

    /// What an OAuth client gets when it asks for nothing in particular:
    /// every scope except the explicit-only ones (`agents`, `notebooks`).
    pub fn default_grant() -> Self {
        Self::of(Scope::ALL.into_iter().filter(|s| !s.explicit_only()))
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
    /// Delegation depth: 0 for the owner's clients (Hermes, Claude Code,
    /// ChatGPT), 1 for a token handed to an agent run. A caller at
    /// [`crate::agents::MAX_DEPTH`] cannot start agents (as in lucina-delegate).
    pub depth: u8,
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
    fn mistral_is_limited_to_read_and_chat() {
        let limit = Scopes::limit_for_caller("mistral").unwrap();
        let held = Scopes::all().intersect(&limit);
        assert!(held.contains(Scope::Read) && held.contains(Scope::Chat));
        for s in [Scope::Write, Scope::Computer, Scope::Agents, Scope::Notebooks] {
            assert!(!held.contains(s), "{s}");
        }
        assert!(Scopes::limit_for_caller("claude").is_none());
    }

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
    fn agents_is_its_own_explicit_scope() {
        let s = Scopes::parse_list("read,agents").unwrap();
        assert!(s.contains(Scope::Agents) && !s.contains(Scope::Write));
        assert_eq!(s.to_string(), "read,agents");
        assert!(!Scopes::parse_list("read,write,chat,computer").unwrap().contains(Scope::Agents));
        assert!(!Scopes::default_grant().contains(Scope::Agents));
        assert_eq!(Scopes::default_grant().to_oauth(), "read chat write computer");
        assert!(Scopes::all().contains(Scope::Agents));
        assert_eq!(Scopes::parse_lenient("openid agents").to_oauth(), "agents");
    }

    #[test]
    fn notebooks_is_its_own_explicit_scope() {
        let s = Scopes::parse_list("read,notebooks").unwrap();
        assert!(s.contains(Scope::Notebooks) && !s.contains(Scope::Write) && !s.contains(Scope::Agents));
        assert_eq!(s.to_string(), "read,notebooks");
        assert!(!Scopes::parse_list("read,write,chat,computer,agents").unwrap().contains(Scope::Notebooks));
        assert!(!Scopes::default_grant().contains(Scope::Notebooks));
        assert!(Scopes::all().contains(Scope::Notebooks));
        assert_eq!(Scopes::all().to_string(), "read,chat,write,computer,agents,notebooks");
        assert_eq!(Scopes::parse_lenient("openid notebooks agents").to_oauth(), "agents notebooks");
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
