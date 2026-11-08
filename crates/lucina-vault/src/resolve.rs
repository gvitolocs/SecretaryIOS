//! Link resolution as `metadataCache.getFirstLinkpathDest` answers it, from the
//! golden file. An exact path from the vault root wins, even over a note of that
//! name beside the source. `../` and `./` count from the source's folder and stop
//! at the root. Otherwise any path ending in the target: those under the source's
//! folder come first, ancestors get no preference, then the shortest path string, then A to Z.
//! Aliases never resolve a link.
use ink_obsidian::resolve::clean_path;
use std::collections::HashMap;

pub struct Resolver {
    paths: Vec<String>,
    lower: HashMap<String, Vec<u32>>,
    by_base: HashMap<String, Vec<u32>>,
}

fn folder(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(p, _)| p)
}
fn clean_clamped(base: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|c| !c.is_empty()).collect();
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

impl Resolver {
    pub fn new<'a>(all: impl IntoIterator<Item = &'a str>) -> Self {
        let mut r = Resolver {
            paths: Vec::new(),
            lower: HashMap::new(),
            by_base: HashMap::new(),
        };
        for p in all {
            let i = r.paths.len() as u32;
            r.paths.push(p.to_owned());
            let l = p.to_lowercase();
            r.by_base
                .entry(l.rsplit('/').next().unwrap_or(&l).to_owned())
                .or_default()
                .push(i);
            r.lower.entry(l).or_default().push(i);
        }
        r
    }

    fn exact(&self, p: &str) -> Option<usize> {
        let hits = self.lower.get(&p.to_lowercase())?;
        hits.iter()
            .copied()
            .find(|i| self.paths[*i as usize] == p)
            .or_else(|| hits.first().copied())
            .map(|i| i as usize)
    }

    pub fn path(&self, i: usize) -> &str {
        &self.paths[i]
    }

    /// `target` is the link's path part. Returns an index into the known paths.
    pub fn resolve_index(&self, from: &str, target: &str) -> Option<usize> {
        if target.is_empty() {
            return self.exact(from);
        }
        if target.ends_with('/') || target.contains(['\0', '\\']) {
            return None;
        }
        // A real note can be named `x.md.md`, and `[[x.md]]` finds it when `x.md` does not exist.
        let names = |p: &str| -> Vec<String> {
            if p.to_lowercase().ends_with(".md") {
                vec![p.to_owned(), format!("{p}.md")]
            } else {
                vec![format!("{p}.md"), p.to_owned()]
            }
        };

        if target.starts_with("../") || target.starts_with("./") || target == ".." {
            let p = clean_clamped(folder(from), target)?;
            return names(&p).iter().find_map(|n| self.exact(n));
        }
        let p = clean_path("", target).or_else(|| clean_clamped("", target))?;
        for n in names(&p) {
            if let Some(hit) = self.exact(&n) {
                return Some(hit);
            }
        }
        let under = {
            let f = folder(from);
            (!f.is_empty()).then(|| format!("{}/", f.to_lowercase()))
        };
        for n in names(&p) {
            let n = n.to_lowercase();
            let base = n.rsplit('/').next().unwrap_or(&n);
            let suffix = format!("/{n}");
            let hits: Vec<usize> = self
                .by_base
                .get(base)
                .into_iter()
                .flatten()
                .map(|i| *i as usize)
                .filter(|i| self.paths[*i].to_lowercase().ends_with(&suffix))
                .collect();
            if hits.is_empty() {
                continue;
            }
            let near: Vec<usize> = match &under {
                Some(u) => hits
                    .iter()
                    .copied()
                    .filter(|i| self.paths[*i].to_lowercase().starts_with(u.as_str()))
                    .collect(),
                None => Vec::new(),
            };
            let pool = if near.is_empty() { hits } else { near };
            return pool
                .into_iter()
                .min_by_key(|i| (self.paths[*i].chars().count(), self.paths[*i].clone()));
        }
        None
    }

    pub fn resolve(&self, from: &str, target: &str) -> Option<&str> {
        self.resolve_index(from, target).map(|i| self.path(i))
    }

    /// The link text Obsidian's "shortest path" would write for `to`: the bare name
    /// when that alone finds it from anywhere, otherwise the path from the root.
    pub fn shortest_link(&self, to: &str) -> String {
        let display = to
            .strip_suffix(".md")
            .or_else(|| to.strip_suffix(".MD"))
            .unwrap_or(to);
        let base = to.rsplit('/').next().unwrap_or(to).to_lowercase();
        let unique = self.by_base.get(&base).is_none_or(|v| v.len() <= 1);
        if unique {
            display.rsplit('/').next().unwrap_or(display).to_owned()
        } else {
            display.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_beats_sibling_and_near_beats_short() {
        let r = Resolver::new([
            "Dup.md",
            "A/Dup.md",
            "A/T.md",
            "B/T.md",
            "B/C/D/S.md",
            "Y/S.md",
        ]);
        assert_eq!(r.resolve("A/x.md", "Dup"), Some("Dup.md"));
        assert_eq!(r.resolve("A/x.md", "T"), Some("A/T.md"));
        assert_eq!(r.resolve("x.md", "T"), Some("A/T.md"));
        assert_eq!(r.resolve("B/x.md", "S"), Some("B/C/D/S.md"));
        assert_eq!(r.resolve("x.md", "S"), Some("Y/S.md"));
        assert_eq!(r.resolve("B/C/D/x.md", "../T"), None);
        assert_eq!(r.resolve("A/x.md", "../Dup"), Some("Dup.md"));
        assert_eq!(r.resolve("x.md", "../Dup"), Some("Dup.md"));
        assert_eq!(r.resolve("x.md", "A/"), None);
    }
    #[test]
    fn shortest_link_needs_a_path_only_when_shared() {
        let r = Resolver::new(["Dup.md", "A/Dup.md", "A/One.md", "x/pic.png"]);
        assert_eq!(r.shortest_link("A/One.md"), "One");
        assert_eq!(r.shortest_link("A/Dup.md"), "A/Dup");
        assert_eq!(r.shortest_link("x/pic.png"), "pic.png");
    }
}
