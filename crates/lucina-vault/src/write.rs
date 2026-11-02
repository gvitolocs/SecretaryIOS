//! Every write to a vault. It is refused unless the vault was opened writable.
//!
//! - **Compare and swap.** A save names the revision it read. If the file is no longer
//!   that, nothing is written and the caller gets `Conflict`; `save_conflict_copy` then
//!   keeps their text in a new note beside it, so neither side loses anything.
//! - **Atomic.** A temporary dot file in the same folder, flushed, then a rename and a
//!   flush of the folder. A power cut leaves the old file or the new one.
//! - **Byte exact.** A save writes what it was given. Nothing here reformats a note,
//!   and `rename` changes only the link it is updating.
//! - **Inside the vault.** No dot paths, no `..`, no symlinks on the way, and never `.obsidian`.
//! - **Nothing is deleted.** `trash` moves to `.trash/`, where Obsidian looks.
use crate::{
    resolve::Resolver,
    scan::Link,
    vault::{revision_of, Vault},
    Error, Result,
};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// `rel` is a clean path below the root that Obsidian would show.
fn check_rel(rel: &str) -> Result<()> {
    let bad = rel.is_empty()
        || rel.starts_with('/')
        || rel.contains(['\\', '\0'])
        || rel.chars().any(char::is_control)
        || rel.split('/').any(|c| c.is_empty() || c.starts_with('.'));
    if bad {
        return Err(Error::Invalid(format!("not a path in the vault: {rel}")));
    }
    Ok(())
}

fn no_symlink_below(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut p = root.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Error::Invalid(format!("symlink not followed: {rel}")));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(p)
}

fn sync_dir(dir: &Path) -> Result<()> {
    // Not every platform can flush a folder (a FAT partition cannot), and the rename stands anyway.
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
    Ok(())
}

/// Writes `bytes` beside `path` and renames over it, keeping its permissions. `check`
/// runs last, after the slow flush, to narrow the window for losing a change made meanwhile.
fn atomic_replace(path: &Path, bytes: &[u8], check: impl Fn() -> Result<()>) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Invalid("no folder".into()))?;
    let tmp = dir.join(format!(
        ".lucina-vault-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result: Result<()> = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        if let Ok(m) = fs::metadata(path) {
            let _ = f.set_permissions(m.permissions());
        }
        f.sync_all()?;
        drop(f);
        check()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    sync_dir(dir)
}

/// A new note's text, or a replacement for one note's link. Links in other notes are
/// rewritten to keep pointing at the moved file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    pub from: String,
    pub to: String,
    /// `(note, revision, new text)`.
    pub edits: Vec<(String, String, String)>,
}

/// The link's target with its text replaced: alias and `#subpath` stay.
fn rewritten(l: &Link, new_path: &str) -> String {
    let sub = l.subpath().map(|s| format!("#{s}")).unwrap_or_default();
    if l.wiki {
        let bang = if l.embed { "!" } else { "" };
        let alias = l
            .original
            .trim_start_matches('!')
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .split_once('|')
            .map(|(left, a)| {
                let esc = if left.ends_with('\\') { "\\" } else { "" };
                format!("{esc}|{a}")
            })
            .unwrap_or_default();
        format!("{bang}[[{new_path}{sub}{alias}]]")
    } else {
        // `[text](dest "title")`: only the destination changes.
        let open = l.original.rfind("](").unwrap_or(0);
        let inner = l.original[open + 2..l.original.len() - 1].trim_start();
        // `<a b.md>` stays in brackets with its spaces; a bare destination is percent-encoded.
        if let Some(r) = inner.strip_prefix('<') {
            let title = r.split_once('>').map_or("", |(_, t)| t);
            return format!("{}](<{new_path}{sub}>{title})", &l.original[..open]);
        }
        let title = inner
            .split_once(' ')
            .map(|(_, t)| format!(" {t}"))
            .unwrap_or_default();
        let dest = new_path.replace('%', "%25").replace(' ', "%20");
        let sub = sub.replace(' ', "%20");
        format!("{}]({dest}{sub}{title})", &l.original[..open])
    }
}

/// `text` with each link replaced on its own line, leaving every other byte alone.
fn apply_links(text: &str, body_links: &[(&Link, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut line_no = 0u32;
    while !rest.is_empty() {
        let (line, tail) = match rest.find('\n') {
            Some(i) => rest.split_at(i + 1),
            None => (rest, ""),
        };
        let mut l = line.to_owned();
        for (link, new) in body_links.iter().filter(|(k, _)| k.line == line_no) {
            if let Some(i) = l.find(&link.original) {
                l.replace_range(i..i + link.original.len(), new);
            }
        }
        out.push_str(&l);
        rest = tail;
        line_no += 1;
    }
    out
}

impl Vault {
    fn must_write(&self) -> Result<()> {
        if self.writable() {
            Ok(())
        } else {
            Err(Error::ReadOnly)
        }
    }

    /// Replaces the note if it is still at `revision`. Returns the new revision.
    pub fn save(&mut self, rel: &str, text: &str, revision: &str) -> Result<String> {
        self.must_write()?;
        check_rel(rel)?;
        let path = no_symlink_below(self.root(), rel)?;
        let current = |p: &Path| -> Result<()> {
            let on_disk = revision_of(&fs::read(p)?);
            if on_disk == revision {
                Ok(())
            } else {
                Err(Error::Conflict {
                    path: rel.to_owned(),
                    current: on_disk,
                })
            }
        };
        current(&path)?;
        atomic_replace(&path, text.as_bytes(), || current(&path))?;
        self.reread(rel)?;
        Ok(revision_of(text.as_bytes()))
    }

    /// Creates a file that must not exist yet, with folders as needed.
    pub fn create(&mut self, rel: &str, bytes: &[u8]) -> Result<String> {
        self.must_write()?;
        check_rel(rel)?;
        let path = no_symlink_below(self.root(), rel)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Exists(rel.to_owned()));
            }
            Err(e) => return Err(e.into()),
        };
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Some(dir) = path.parent() {
            sync_dir(dir)?;
        }
        self.refresh()?;
        Ok(revision_of(bytes))
    }

    /// Where a new note named `name` goes under the vault's own settings, `current` being
    /// the folder of the open note. A taken name gets ` 1`, ` 2` as in Obsidian.
    pub fn new_note_path(&self, name: &str, current: &str) -> String {
        let folder = self.settings().new_note_folder(current);
        let join = |n: &str| {
            if folder.is_empty() {
                format!("{n}.md")
            } else {
                format!("{folder}/{n}.md")
            }
        };
        let mut p = join(name);
        let mut i = 1;
        while self.root().join(&p).exists() {
            p = join(&format!("{name} {i}"));
            i += 1;
        }
        p
    }

    /// Where an attachment named `name` for the note at `note` goes.
    pub fn new_attachment_path(&self, name: &str, note: &str) -> String {
        let folder = self.settings().attachment_folder(note);
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let join = |n: String| {
            let f = if ext.is_empty() {
                n
            } else {
                format!("{n}.{ext}")
            };
            if folder.is_empty() {
                f
            } else {
                format!("{folder}/{f}")
            }
        };
        let mut p = join(stem.to_owned());
        let mut i = 1;
        while self.root().join(&p).exists() {
            p = join(format!("{stem} {i}"));
            i += 1;
        }
        p
    }

    /// Keeps text that lost a compare-and-swap, as a new note beside the original:
    /// `Note (conflict 2026-11-02 phone).md`. Real Obsidian shows it like any other note.
    pub fn save_conflict_copy(
        &mut self,
        rel: &str,
        text: &str,
        date: &str,
        device: &str,
    ) -> Result<String> {
        self.must_write()?;
        check_rel(rel)?;
        let (stem, ext) = rel.rsplit_once('.').unwrap_or((rel, "md"));
        let safe: String = device
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        let mut p = format!("{stem} (conflict {date} {safe}).{ext}");
        let mut i = 2;
        while self.root().join(&p).exists() {
            p = format!("{stem} (conflict {date} {safe} {i}).{ext}");
            i += 1;
        }
        self.create(&p, text.as_bytes())?;
        Ok(p)
    }

    /// What `rename` would do, for showing before doing. Reads the notes that link there.
    pub fn plan_rename(&self, from: &str, to: &str) -> Result<RenamePlan> {
        check_rel(from)?;
        check_rel(to)?;
        if self.root().join(to).exists() {
            return Err(Error::Exists(to.to_owned()));
        }
        // Resolve the new links against the vault as it will be.
        let after: Vec<String> = self
            .notes()
            .map(|n| n.path.clone())
            .chain(self.attachments().iter().cloned())
            .map(|p| if p == from { to.to_owned() } else { p })
            .collect();
        let resolver = Resolver::new(after.iter().map(String::as_str));
        // How a link to `path` is written after the move: a wikilink without `.md`, a Markdown
        // link with it, both the shortest that still finds the file from anywhere.
        let written = |wiki: bool, path: &str| -> String {
            let s = resolver.shortest_link(path);
            if wiki || !(path.ends_with(".md") || path.ends_with(".MD")) {
                s
            } else {
                format!("{s}.md")
            }
        };
        let moved = |p: &str| -> String {
            if p == from {
                to.to_owned()
            } else {
                p.to_owned()
            }
        };
        let mut edits = Vec::new();
        for n in self.notes() {
            let here = moved(&n.path);
            let mut hits: Vec<(&Link, String)> = Vec::new();
            for (l, d) in n.scan.links.iter().zip(&n.dest) {
                let Some(d) = d else { continue };
                if l.path().is_empty() {
                    continue;
                }
                let was = self.resolver().path(*d as usize);
                let now = moved(was);
                // A link changes if its file moved, or if moving this note made it find another
                // file. In the moved note itself, a bare name that several files share gets its path.
                let shared_name = n.path == from
                    && !l.path().contains('/')
                    && resolver.shortest_link(&now).contains('/');
                if was == from
                    || shared_name
                    || resolver.resolve(&here, l.path()) != Some(now.as_str())
                {
                    hits.push((l, rewritten(l, &written(l.wiki, &now))));
                }
            }
            let mut yaml: Vec<(&str, String)> = Vec::new();
            for ((_, l), d) in n.scan.frontmatter_links.iter().zip(&n.frontmatter_dest) {
                let Some(d) = d else { continue };
                let was = self.resolver().path(*d as usize);
                let now = moved(was);
                let target = l.split('#').next().unwrap_or("");
                if was == from || resolver.resolve(&here, target) != Some(now.as_str()) {
                    yaml.push((l, written(true, &now)));
                }
            }
            if hits.is_empty() && yaml.is_empty() {
                continue;
            }
            let note = self.read(&n.path)?;
            let mut text = apply_links(&note.text, &hits);
            for (l, new) in yaml {
                let sub = l
                    .split_once('#')
                    .map(|(_, s)| format!("#{s}"))
                    .unwrap_or_default();
                text = text.replacen(&format!("[[{l}]]"), &format!("[[{new}{sub}]]"), 1);
            }
            if text != note.text {
                edits.push((n.path.clone(), note.revision, text));
            }
        }
        Ok(RenamePlan {
            from: from.to_owned(),
            to: to.to_owned(),
            edits,
        })
    }

    /// Moves the file, then updates the links that followed it. A conflict on one note
    /// stops there: the file has moved and the notes done stay done, as in Obsidian;
    /// the returned list says which notes were updated.
    pub fn rename(&mut self, plan: &RenamePlan) -> Result<Vec<String>> {
        self.must_write()?;
        check_rel(&plan.from)?;
        check_rel(&plan.to)?;
        let from = no_symlink_below(self.root(), &plan.from)?;
        let to = no_symlink_below(self.root(), &plan.to)?;
        if to.exists() {
            return Err(Error::Exists(plan.to.clone()));
        }
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        for d in [from.parent(), to.parent()].into_iter().flatten() {
            sync_dir(d)?;
        }
        let mut done = Vec::new();
        for (note, revision, text) in &plan.edits {
            // A note that was renamed is its own edit, under its new name.
            let rel = if *note == plan.from { &plan.to } else { note };
            let path = no_symlink_below(self.root(), rel)?;
            let check = || -> Result<()> {
                let on_disk = revision_of(&fs::read(&path)?);
                if on_disk == *revision {
                    Ok(())
                } else {
                    Err(Error::Conflict {
                        path: rel.clone(),
                        current: on_disk,
                    })
                }
            };
            check()?;
            atomic_replace(&path, text.as_bytes(), check)?;
            done.push(rel.clone());
        }
        self.refresh()?;
        Ok(done)
    }

    /// Moves a file to the vault's `.trash`, keeping its folders, as Obsidian's own
    /// "move to Obsidian trash" does. Returns where it went.
    pub fn trash(&mut self, rel: &str) -> Result<String> {
        self.must_write()?;
        check_rel(rel)?;
        let from = no_symlink_below(self.root(), rel)?;
        if !from.exists() {
            return Err(Error::NotFound(rel.to_owned()));
        }
        let mut dest = format!(".trash/{rel}");
        let mut i = 1;
        while self.root().join(&dest).exists() {
            dest = format!(".trash/{rel} {i}");
            i += 1;
        }
        let to = self.root().join(&dest);
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(&from, &to)?;
        self.refresh()?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_that_are_refused() {
        for bad in [
            "",
            "/a.md",
            "../a.md",
            "a/../b.md",
            ".obsidian/app.json",
            "a/.hidden.md",
            "a\\b.md",
            "a//b.md",
            "a\n.md",
            ".trash/a.md",
        ] {
            assert!(check_rel(bad).is_err(), "{bad:?}");
        }
        for good in [
            "a.md",
            "a b/c d.md",
            "x/A claim stated as a full sentence..md",
            "café.md",
        ] {
            assert!(check_rel(good).is_ok(), "{good:?}");
        }
    }
}
