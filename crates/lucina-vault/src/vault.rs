//! The open vault: every note's links resolved, tags counted, backlinks ready. It holds
//! no note text. Text is read when asked for, so a large vault costs little memory.
use crate::{
    resolve::Resolver,
    scan::{scan, Link, Scan},
    settings::Settings,
    Error, Result,
};
use ink_obsidian::{
    config::VaultConfig,
    walk::{read_note, walk, Read},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Options {
    /// Off by default: a vault is read-only until the caller says otherwise.
    pub writable: bool,
    pub max_note_bytes: u64,
    pub max_files: usize,
}
impl Default for Options {
    fn default() -> Self {
        Options {
            writable: false,
            max_note_bytes: 32 * 1024 * 1024,
            max_files: 500_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Note {
    pub path: String,
    pub mtime_ns: u64,
    pub size: u64,
    pub scan: Scan,
    /// Per `scan.links`: the path it resolves to.
    pub dest: Vec<Option<u32>>,
    pub frontmatter_dest: Vec<Option<u32>>,
}
impl Note {
    /// What Obsidian shows: the file name, not the first heading.
    pub fn name(&self) -> &str {
        note_name(&self.path)
    }
}
pub fn note_name(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.rsplit_once('.')
        .filter(|(_, e)| e.eq_ignore_ascii_case("md"))
        .map_or(base, |(s, _)| s)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backlink {
    pub from: String,
    pub line: u32,
    pub link: String,
    pub embed: bool,
    pub original: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub notes: usize,
    pub attachments: usize,
    pub links: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub tags: usize,
    pub skipped: usize,
}

pub struct NoteText {
    pub text: String,
    /// sha256 of the bytes on disk. Hand it back to a write.
    pub revision: String,
}

pub struct Vault {
    pub(crate) root: PathBuf,
    pub(crate) opts: Options,
    pub(crate) settings: Settings,
    notes: BTreeMap<String, Note>,
    attachments: Vec<String>,
    pub(crate) resolver: Resolver,
    warnings: Vec<String>,
    skipped: usize,
}

pub(crate) fn revision_of(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn config(root: &Path, opts: &Options) -> VaultConfig {
    let mut c = VaultConfig::new(root);
    // Obsidian shows every file except dot files and dot folders.
    c.exclude = vec![".*/".into(), ".*".into()];
    c.max_file_bytes = opts.max_note_bytes;
    c.max_files = opts.max_files;
    c
}

impl Vault {
    pub fn open(root: impl AsRef<Path>, opts: Options) -> Result<Vault> {
        let mut v = Vault {
            root: std::fs::canonicalize(root.as_ref())?,
            opts,
            settings: Settings::default(),
            notes: BTreeMap::new(),
            attachments: Vec::new(),
            resolver: Resolver::new([]),
            warnings: Vec::new(),
            skipped: 0,
        };
        v.refresh()?;
        Ok(v)
    }

    /// Reads what changed on disk since the last look (by mtime and size) and
    /// resolves every link again. Returns how many notes were read.
    pub fn refresh(&mut self) -> Result<usize> {
        let cfg = config(&self.root, &self.opts);
        let w = walk(&cfg).map_err(ink_err)?;
        self.settings = Settings::load(&self.root);
        self.warnings = w.warnings.clone();
        self.skipped = w.skipped;
        let mut old = std::mem::take(&mut self.notes);
        let mut read = 0;
        for e in &w.notes {
            if let Some(n) = old
                .remove(&e.rel)
                .filter(|n| n.mtime_ns == e.mtime_ns && n.size == e.size)
            {
                self.notes.insert(e.rel.clone(), n);
                continue;
            }
            let text =
                match read_note(&w.root, &e.rel, self.opts.max_note_bytes).map_err(ink_err)? {
                    Read::Text(t) => t,
                    Read::Skipped(why) => {
                        self.warnings.push(why);
                        self.skipped += 1;
                        continue;
                    }
                };
            read += 1;
            self.notes.insert(
                e.rel.clone(),
                Note {
                    path: e.rel.clone(),
                    mtime_ns: e.mtime_ns,
                    size: e.size,
                    scan: scan(&text),
                    dest: Vec::new(),
                    frontmatter_dest: Vec::new(),
                },
            );
        }
        self.attachments = w.attachments;
        self.resolver = Resolver::new(
            self.notes
                .keys()
                .map(String::as_str)
                .chain(self.attachments.iter().map(String::as_str)),
        );
        for n in self.notes.values_mut() {
            n.dest = n
                .scan
                .links
                .iter()
                .map(|l| {
                    self.resolver
                        .resolve_index(&n.path, l.path())
                        .map(|i| i as u32)
                })
                .collect();
            n.frontmatter_dest = n
                .scan
                .frontmatter_links
                .iter()
                .map(|(_, l)| {
                    let p = l.split('#').next().unwrap_or("");
                    self.resolver.resolve_index(&n.path, p).map(|i| i as u32)
                })
                .collect();
        }
        Ok(read)
    }

    /// One note changed and the set of files did not: no other note's links can change,
    /// so a save need not walk the vault again.
    pub(crate) fn reread(&mut self, rel: &str) -> Result<()> {
        let bytes = self.read(rel)?;
        let meta = std::fs::metadata(self.root.join(rel))?;
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let s = scan(&bytes.text);
        let dest = s
            .links
            .iter()
            .map(|l| self.resolver.resolve_index(rel, l.path()).map(|i| i as u32))
            .collect();
        let frontmatter_dest = s
            .frontmatter_links
            .iter()
            .map(|(_, l)| {
                let p = l.split('#').next().unwrap_or("");
                self.resolver.resolve_index(rel, p).map(|i| i as u32)
            })
            .collect();
        if let Some(n) = self.notes.get_mut(rel) {
            n.mtime_ns = mtime_ns;
            n.size = meta.len();
            n.scan = s;
            n.dest = dest;
            n.frontmatter_dest = frontmatter_dest;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn writable(&self) -> bool {
        self.opts.writable
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.notes.values()
    }
    pub fn note(&self, path: &str) -> Option<&Note> {
        self.notes.get(path)
    }
    pub fn attachments(&self) -> &[String] {
        &self.attachments
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    pub fn resolver(&self) -> &Resolver {
        &self.resolver
    }

    /// Target of a link written in the note at `from`.
    pub fn resolve(&self, from: &str, link: &str) -> Option<&str> {
        self.resolver
            .resolve(from, link.split('#').next().unwrap_or(""))
    }

    pub fn read(&self, path: &str) -> Result<NoteText> {
        if !self.notes.contains_key(path) {
            return Err(Error::NotFound(path.to_owned()));
        }
        match read_note(&self.root, path, self.opts.max_note_bytes).map_err(ink_err)? {
            Read::Text(text) => {
                let revision = revision_of(text.as_bytes());
                Ok(NoteText { text, revision })
            }
            Read::Skipped(why) => Err(Error::Invalid(why)),
        }
    }

    /// A safe absolute path for a file the vault knows: no symlink anywhere below the
    /// root and never outside it. For showing attachments; there is no write through this.
    pub fn file_path(&self, rel: &str) -> Result<PathBuf> {
        if !self.notes.contains_key(rel)
            && self
                .attachments
                .binary_search_by(|a| a.as_str().cmp(rel))
                .is_err()
        {
            return Err(Error::NotFound(rel.to_owned()));
        }
        let mut p = self.root.clone();
        for c in rel.split('/') {
            p.push(c);
            if std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
                return Err(Error::Invalid(format!("symlink not followed: {rel}")));
            }
        }
        Ok(p)
    }

    /// Links from other notes to `path`, in path order. A note's links to itself are left out.
    pub fn backlinks(&self, path: &str) -> Vec<Backlink> {
        let mut out = Vec::new();
        for n in self.notes.values() {
            if n.path == path {
                continue;
            }
            for (l, d) in n.scan.links.iter().zip(&n.dest) {
                if d.is_some_and(|d| self.resolver.path(d as usize) == path) {
                    out.push(Backlink {
                        from: n.path.clone(),
                        line: l.line,
                        link: l.link.clone(),
                        embed: l.embed,
                        original: l.original.clone(),
                    });
                }
            }
            for ((_, l), d) in n.scan.frontmatter_links.iter().zip(&n.frontmatter_dest) {
                if d.is_some_and(|d| self.resolver.path(d as usize) == path) {
                    out.push(Backlink {
                        from: n.path.clone(),
                        line: 0,
                        link: l.clone(),
                        embed: false,
                        original: format!("[[{l}]]"),
                    });
                }
            }
        }
        out
    }

    /// Links nothing answers, with how many notes' links want them.
    pub fn unresolved(&self) -> Vec<(String, u32)> {
        let mut m: BTreeMap<String, u32> = BTreeMap::new();
        for n in self.notes.values() {
            for (l, d) in n.scan.links.iter().zip(&n.dest) {
                if d.is_none() {
                    *m.entry(l.path().to_owned()).or_default() += 1;
                }
            }
        }
        m.into_iter().collect()
    }

    /// `metadataCache.getTags()`: every use counts for the tag and each parent. Case folds
    /// together, keeping the first spelling seen. A trailing `/` is dropped.
    pub fn tags(&self) -> Vec<TagCount> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut out: Vec<TagCount> = Vec::new();
        let mut bump = |t: &str| {
            let k = t.to_lowercase();
            let i = *seen.entry(k).or_insert_with(|| {
                out.push(TagCount {
                    tag: t.to_owned(),
                    count: 0,
                });
                out.len() - 1
            });
            out[i].count += 1;
        };
        for n in self.notes.values() {
            for t in &n.scan.tags {
                let t = t.strip_suffix('/').unwrap_or(t);
                for (i, c) in t.char_indices() {
                    if c == '/' && i > 0 && t[..i].chars().any(|c| !c.is_ascii_digit()) {
                        bump(&t[..i]);
                    }
                }
                if !t.is_empty() {
                    bump(t);
                }
            }
        }
        out.sort_by_cached_key(|t| t.tag.to_lowercase());
        out
    }

    pub fn notes_with_tag(&self, tag: &str) -> Vec<&Note> {
        let tag = tag.trim_start_matches('#').to_lowercase();
        let prefix = format!("{tag}/");
        self.notes
            .values()
            .filter(|n| {
                n.scan.tags.iter().any(|t| {
                    let t = t.to_lowercase();
                    t.trim_end_matches('/') == tag || t.starts_with(&prefix)
                })
            })
            .collect()
    }

    /// Folders that hold something, including the folders above them, A to Z without case.
    pub fn folders(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        for p in self.notes.keys().chain(self.attachments.iter()) {
            for (i, c) in p.char_indices() {
                if c == '/' {
                    out.insert(p[..i].to_owned());
                }
            }
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort_by_cached_key(|f| f.to_lowercase());
        v
    }

    pub fn counts(&self) -> Counts {
        let mut links = 0;
        let mut resolved = 0;
        for n in self.notes.values() {
            links += n.dest.len();
            resolved += n.dest.iter().filter(|d| d.is_some()).count();
        }
        Counts {
            notes: self.notes.len(),
            attachments: self.attachments.len(),
            links,
            resolved,
            unresolved: links - resolved,
            tags: self.tags().len(),
            skipped: self.skipped,
        }
    }

    /// Links in the note at `path` with what each resolves to.
    pub fn outgoing(&self, path: &str) -> Vec<(&Link, Option<&str>)> {
        self.notes.get(path).map_or_else(Vec::new, |n| {
            n.scan
                .links
                .iter()
                .zip(&n.dest)
                .map(|(l, d)| (l, d.map(|d| self.resolver.path(d as usize))))
                .collect()
        })
    }
}

pub(crate) fn ink_err(e: ink_obsidian::Error) -> Error {
    match e {
        ink_obsidian::Error::Invalid(s) => Error::Invalid(s),
        ink_obsidian::Error::Io(e) => Error::Io(e),
    }
}
