//! One vault as we read it against what real Obsidian decided about it.
use lucina_vault::Vault;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct Diff {
    pub checked: usize,
    pub wrong: Vec<String>,
}
impl Diff {
    fn eq<T: PartialEq + std::fmt::Debug>(&mut self, what: String, ours: T, theirs: T) {
        self.checked += 1;
        if ours != theirs {
            self.wrong
                .push(format!("{what}\n  ours   {ours:?}\n  theirs {theirs:?}"));
        }
    }
    /// Sorted lists, reported as what only one side has.
    fn lists(&mut self, what: String, ours: Vec<String>, theirs: Vec<String>) {
        self.checked += 1;
        if ours != theirs {
            let a: Vec<&String> = ours.iter().filter(|x| !theirs.contains(x)).collect();
            let b: Vec<&String> = theirs.iter().filter(|x| !ours.contains(x)).collect();
            self.wrong
                .push(format!("{what}\n  ours only   {a:?}\n  theirs only {b:?}"));
        }
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

pub fn compare(v: &Vault, g: &Value) -> Diff {
    let mut d = Diff::default();
    let mut files: Vec<String> = v
        .notes()
        .map(|n| n.path.clone())
        .chain(v.attachments().iter().cloned())
        .collect();
    files.sort();
    let mut theirs: Vec<String> = g["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect();
    theirs.sort();
    d.eq("the vault's files".into(), files, theirs);
    for (path, want) in g["notes"].as_object().unwrap() {
        let Some(n) = v.note(path) else {
            d.wrong.push(format!("{path}: note missing"));
            continue;
        };
        // Links: what, on which line, resolving where. Order is Obsidian's own business.
        let ours: Vec<String> = sorted(
            n.scan
                .links
                .iter()
                .zip(&n.dest)
                .filter(|(l, _)| !l.embed)
                .map(|(l, d)| {
                    format!(
                        "{} @{} -> {:?}",
                        l.link,
                        l.line,
                        d.map(|i| v.resolver().path(i as usize))
                    )
                })
                .collect(),
        );
        let list = |key: &str| -> Vec<String> {
            sorted(
                want[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| {
                        format!(
                            "{} @{} -> {:?}",
                            l["link"].as_str().unwrap(),
                            l["line"],
                            l["dest"].as_str()
                        )
                    })
                    .collect(),
            )
        };
        d.lists(format!("{path}: links"), ours, list("links"));
        let ours: Vec<String> = sorted(
            n.scan
                .links
                .iter()
                .zip(&n.dest)
                .filter(|(l, _)| l.embed)
                .map(|(l, d)| {
                    format!(
                        "{} @{} -> {:?}",
                        l.link,
                        l.line,
                        d.map(|i| v.resolver().path(i as usize))
                    )
                })
                .collect(),
        );
        d.lists(format!("{path}: embeds"), ours, list("embeds"));
        let ours: Vec<String> = sorted(
            n.scan
                .frontmatter_links
                .iter()
                .zip(&n.frontmatter_dest)
                .map(|((k, l), d)| {
                    format!("{k}={l} -> {:?}", d.map(|i| v.resolver().path(i as usize)))
                })
                .collect(),
        );
        let theirs: Vec<String> = sorted(
            want["frontmatterLinks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| {
                    format!(
                        "{}={} -> {:?}",
                        l["key"].as_str().unwrap(),
                        l["link"].as_str().unwrap(),
                        l["dest"].as_str()
                    )
                })
                .collect(),
        );
        d.lists(format!("{path}: frontmatter links"), ours, theirs);
        let headings: Vec<(String, u64, u64)> = n
            .scan
            .headings
            .iter()
            .map(|h| (h.text.clone(), h.level as u64, h.line as u64))
            .collect();
        let theirs: Vec<(String, u64, u64)> = want["headings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["heading"].as_str().unwrap().to_owned(),
                    h["level"].as_u64().unwrap(),
                    h["line"].as_u64().unwrap(),
                )
            })
            .collect();
        d.eq(format!("{path}: headings"), headings, theirs);
    }
    // Tag counts over the whole vault, frontmatter included.
    let ours: BTreeMap<String, u64> = v
        .tags()
        .into_iter()
        .map(|t| (t.tag.to_lowercase(), t.count as u64))
        .collect();
    let theirs: BTreeMap<String, u64> = g["tags"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, c)| {
            (
                k.trim_start_matches('#').to_lowercase(),
                c.as_u64().unwrap(),
            )
        })
        .collect();
    for (k, c) in &theirs {
        d.eq(format!("tag {k}"), ours.get(k), Some(c));
    }
    for k in ours.keys().filter(|k| !theirs.contains_key(*k)) {
        d.wrong.push(format!("tag {k} is ours only"));
    }
    d
}
