//! Link resolution the Obsidian way, purely on known vault paths: exact path,
//! then a unique path-suffix or basename match; otherwise unresolved or
//! ambiguous. Targets are cleaned lexically and can never leave the vault.
use crate::parse::{LinkKind, RawLink};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(String),
    Unresolved,
    Ambiguous(usize),
}

/// Clean a `/`-separated link target relative to `base` (a folder inside the
/// vault, `""` for the root). `None` when it would escape the root or is unsafe.
pub fn clean_path(base: &str, target: &str) -> Option<String> {
    if target.contains(['\0', '\\']) || target.chars().any(char::is_control) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    let joined;
    let full = if let Some(abs) = target.strip_prefix('/') {
        abs
    } else if base.is_empty() {
        target
    } else {
        joined = format!("{base}/{target}");
        joined.as_str()
    };
    for c in full.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}
fn base_lower(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_lowercase()
}

pub struct Resolver {
    paths: BTreeSet<String>,
    lower: BTreeMap<String, Vec<String>>,
    by_base: BTreeMap<String, Vec<String>>,
}

impl Resolver {
    pub fn new<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut r = Resolver {
            paths: BTreeSet::new(),
            lower: BTreeMap::new(),
            by_base: BTreeMap::new(),
        };
        for p in paths {
            r.paths.insert(p.to_owned());
            r.lower
                .entry(p.to_lowercase())
                .or_default()
                .push(p.to_owned());
            r.by_base
                .entry(base_lower(p))
                .or_default()
                .push(p.to_owned());
        }
        r
    }

    fn exact(&self, p: &str) -> Option<String> {
        if self.paths.contains(p) {
            return Some(p.to_owned());
        }
        match self.lower.get(&p.to_lowercase()) {
            Some(v) if v.len() == 1 => Some(v[0].clone()),
            _ => None,
        }
    }

    pub fn resolve(&self, from: &str, link: &RawLink) -> Resolution {
        let target = link.target.trim();
        if target.is_empty() {
            // `[[#Heading]]` points into the same note.
            return Resolution::Resolved(from.to_owned());
        }
        let candidates = |p: String| {
            let md = format!("{p}.md");
            if crate::walk::is_markdown(&p) {
                vec![p]
            } else {
                vec![p, md]
            }
        };
        // Markdown links are relative to the note's folder first.
        if link.kind == LinkKind::Markdown && !target.starts_with('/') {
            if let Some(p) = clean_path(parent(from), target) {
                for c in candidates(p) {
                    if let Some(hit) = self.exact(&c) {
                        return Resolution::Resolved(hit);
                    }
                }
            }
        }
        let Some(p) = clean_path("", target) else {
            return Resolution::Unresolved;
        };
        let cands = candidates(p.clone());
        for c in &cands {
            if let Some(hit) = self.exact(c) {
                return Resolution::Resolved(hit);
            }
        }
        let mut hits: BTreeSet<&String> = BTreeSet::new();
        if p.contains('/') {
            for c in &cands {
                let suffix = format!("/{}", c.to_lowercase());
                hits.extend(
                    self.paths
                        .iter()
                        .filter(|x| x.to_lowercase().ends_with(&suffix)),
                );
            }
        } else {
            for c in &cands {
                hits.extend(self.by_base.get(&c.to_lowercase()).into_iter().flatten());
            }
        }
        match hits.len() {
            0 => Resolution::Unresolved,
            1 => Resolution::Resolved(hits.into_iter().next().cloned().unwrap_or_default()),
            n => Resolution::Ambiguous(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn link(kind: LinkKind, target: &str) -> RawLink {
        RawLink {
            kind,
            target: target.into(),
            heading: None,
            alias: None,
            line: 1,
            raw: String::new(),
        }
    }
    #[test]
    fn obsidian_resolution_order() {
        let r = Resolver::new([
            "Books/Dune.md",
            "A/Index.md",
            "B/Index.md",
            "A/Notes/Deep.md",
            "img/pic.png",
        ]);
        let w = |t| r.resolve("A/Notes/Deep.md", &link(LinkKind::Wiki, t));
        assert_eq!(
            w("Books/Dune"),
            Resolution::Resolved("Books/Dune.md".into())
        );
        assert_eq!(w("dune"), Resolution::Resolved("Books/Dune.md".into()));
        assert_eq!(w("Index"), Resolution::Ambiguous(2));
        assert_eq!(w("A/Index"), Resolution::Resolved("A/Index.md".into()));
        assert_eq!(
            w("Notes/Deep"),
            Resolution::Resolved("A/Notes/Deep.md".into())
        );
        assert_eq!(w("pic.png"), Resolution::Resolved("img/pic.png".into()));
        assert_eq!(w("Missing"), Resolution::Unresolved);
        assert_eq!(w(""), Resolution::Resolved("A/Notes/Deep.md".into()));
        assert_eq!(w("../../etc/passwd"), Resolution::Unresolved);
        let m = |t| r.resolve("A/Notes/Deep.md", &link(LinkKind::Markdown, t));
        assert_eq!(m("../Index.md"), Resolution::Resolved("A/Index.md".into()));
        assert_eq!(m("/B/Index.md"), Resolution::Resolved("B/Index.md".into()));
        assert_eq!(m("../../../x.md"), Resolution::Unresolved);
    }
    #[test]
    fn clean_paths() {
        assert_eq!(clean_path("a/b", "../c.md").as_deref(), Some("a/c.md"));
        assert_eq!(clean_path("", "./x//y.md").as_deref(), Some("x/y.md"));
        assert_eq!(clean_path("a", "../../x.md"), None);
        assert_eq!(clean_path("", "a\\..\\x"), None);
        assert_eq!(clean_path("", ".."), None);
    }
    proptest! {
        #[test]
        fn cleaned_paths_never_escape(base in "[a-z./]{0,12}", target in "[a-z./\\\\]{0,24}") {
            let base = clean_path("", &base).unwrap_or_default();
            if let Some(p) = clean_path(&base, &target) {
                prop_assert!(!p.starts_with('/'));
                prop_assert!(p.split('/').all(|c| !c.is_empty() && c != "." && c != ".."));
                let root = std::path::Path::new("/vault");
                let joined = root.join(&p);
                prop_assert!(joined.starts_with(root));
                prop_assert!(joined.components().all(|c| !matches!(c, std::path::Component::ParentDir)));
            }
        }
        #[test]
        fn resolution_only_returns_known_paths(target in "[A-Za-z./ ]{0,24}") {
            let r = Resolver::new(["a/b.md", "c.md", "d/e/f.md"]);
            for kind in [LinkKind::Wiki, LinkKind::Markdown] {
                if let Resolution::Resolved(p) = r.resolve("d/e/f.md", &link(kind, &target)) {
                    prop_assert!(["a/b.md", "c.md", "d/e/f.md"].contains(&p.as_str()));
                }
            }
        }
    }
}
