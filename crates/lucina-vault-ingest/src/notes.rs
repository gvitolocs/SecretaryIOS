//! Delivering a plan into the project's folder of the vault (`<Folder>/…`):
//! claims in `Permanent/`, the event digest in `Events/`, the folder's change
//! log MOC, its hub `00 <Folder> hub.md` (and a line in `00 Home.md` when the
//! hub is new).
//!
//! - Every change is read → amend → compare-and-swap write against the vault
//!   itself, so an edit made meanwhile (the owner, the Mac sync, another agent)
//!   is never overwritten: the write is refused and the item retried.
//! - A claim whose title (normalized) names an existing note is amended, never
//!   rewritten: a source line, a dated update, missing files and links are
//!   added to its sections; the owner's text stays as it is.
//! - Every line the worker adds ends in `<!-- ingest:<key> -->`; a page that
//!   already holds the item's marker is left alone, so a replay writes nothing.
//! - The event's file name comes from the source (date + its title), not from
//!   the model, so a retry lands on the same page.

use chrono::NaiveDate;

use crate::plan::{clean_title, normalize, Claim, Plan};
use crate::queue::{Backlink, Item};
use crate::store::{version, StoreError, VaultStore};
use crate::templates::{fill, Templates};

/// Event lines kept on the hub and on the change log.
const HUB_EVENTS: usize = 60;
const MOC_EVENTS: usize = 40;
/// Characters of the source title in an event's file name.
const EVENT_NAME: usize = 80;
pub const PERMANENT_DIR: &str = "Permanent";
pub const EVENTS_DIR: &str = "Events";
pub const MOC_DIR: &str = "MOC";
pub const HOME: &str = "00 Home.md";

pub fn marker(key: &str) -> String {
    format!("<!-- ingest:{key} -->")
}

pub fn hub_name(folder: &str) -> String {
    format!("00 {folder} hub")
}

pub fn moc_name(folder: &str) -> String {
    format!("MOC {folder} change log")
}

fn bullets(v: &[String], f: impl Fn(&String) -> String) -> String {
    if v.is_empty() {
        "- —".into()
    } else {
        v.iter()
            .map(|s| format!("- {}", f(s)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn tags(item: &Item) -> String {
    let mut t = vec![item.project.clone(), item.source.as_str().to_owned()];
    t.dedup();
    t.iter()
        .map(|x| format!("  - {x}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How the notes point back to the item's source (plain text: the ops
/// folder and the repos are not in this vault).
pub fn backlink_text(b: &Backlink) -> String {
    match b {
        Backlink::Ops {
            id,
            log,
            incident,
            commit,
        } => {
            let mut s = format!("ops record `{id}` (Lucina ops `{log}`");
            if let Some(i) = incident {
                s.push_str(&format!(", `{i}`"));
            }
            s.push(')');
            if let Some(c) = commit {
                s.push_str(&format!(" · commit `{c}`"));
            }
            s
        }
        Backlink::Git {
            repo,
            path,
            branch,
            sha,
            merge,
        } => format!(
            "{} `{}` on {repo} `{branch}` (`{path}`)",
            if *merge { "merge" } else { "commit" },
            &sha[..sha.len().min(12)]
        ),
        Backlink::Session { id } => format!("Claude session report `{id}`"),
    }
}

/// The event's name from the source title: at most 80 characters, cut at a
/// word, without a dangling comma or bracket.
pub fn event_name(title: &str, fallback: &str) -> String {
    let mut t = clean_title(title);
    if t.chars().count() > EVENT_NAME {
        t = t.chars().take(EVENT_NAME).collect();
        if let Some(i) = t.rfind(' ') {
            t.truncate(i);
        }
    }
    let t = t
        .trim_end_matches([',', ';', '-', '(', '+', '&', ' ', '.'])
        .to_owned();
    if t.is_empty() {
        fallback.to_owned()
    } else {
        t
    }
}

fn stem(rel: &str) -> &str {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.strip_suffix(".md").unwrap_or(name)
}

/// Lines of one `## Section` (header excluded): `(start, end)` line indexes.
fn section(lines: &[String], name: &str) -> Option<(usize, usize)> {
    let h = format!("## {name}");
    let i = lines.iter().position(|l| l.trim_end() == h)?;
    let end = lines[i + 1..]
        .iter()
        .position(|l| l.starts_with("## ") || l.starts_with("# "))
        .map_or(lines.len(), |j| i + 1 + j);
    Some((i + 1, end))
}

/// The first of `names` that is a section of `text`, else the first name.
fn section_named<'a>(text: &str, names: &[&'a str]) -> &'a str {
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    names
        .iter()
        .copied()
        .find(|n| section(&lines, n).is_some())
        .unwrap_or(names[0])
}

/// Add `new` lines to a section (created at the end when missing). `top`:
/// newest first, right under the header; else after the section's last line.
/// A `- —` placeholder goes. `keep`: at most that many marked lines stay.
pub fn add_lines(text: &str, name: &str, new: &[String], top: bool, keep: Option<usize>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let new: Vec<String> = new
        .iter()
        .filter(|l| !text.contains(l.as_str()))
        .cloned()
        .collect();
    if new.is_empty() {
        return text.to_owned();
    }
    let (start, end) = match section(&lines, name) {
        Some(r) => r,
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.push(format!("## {name}"));
            (lines.len(), lines.len())
        }
    };
    let mut body: Vec<String> = lines[start..end].to_vec();
    body.retain(|l| l.trim() != "- —");
    while body.first().is_some_and(|l| l.trim().is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    if top {
        let mut b = new.clone();
        b.extend(body);
        body = b;
    } else {
        body.extend(new);
    }
    if let Some(k) = keep {
        let mut seen = 0;
        body.retain(|l| {
            if l.contains("<!-- ingest:") {
                seen += 1;
                seen <= k
            } else {
                true
            }
        });
    }
    let mut out: Vec<String> = lines[..start].to_vec();
    out.push(String::new());
    out.extend(body);
    if end < lines.len() {
        out.push(String::new());
    }
    out.extend(lines[end..].iter().cloned());
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// `updated: <date>` in the front matter, when the page has that field.
fn touch_updated(text: &str, date: NaiveDate) -> String {
    if !text.starts_with("---\n") {
        return text.to_owned();
    }
    let Some(end) = text[4..].find("\n---").map(|i| i + 4) else {
        return text.to_owned();
    };
    let fm: Vec<String> = text[..end]
        .lines()
        .map(|l| {
            if l.starts_with("updated:") {
                format!("updated: {date}")
            } else {
                l.to_owned()
            }
        })
        .collect();
    format!("{}{}", fm.join("\n"), &text[end..])
}

pub struct Writer<'a> {
    pub store: &'a dyn VaultStore,
    pub templates: &'a Templates,
    /// The owner's local date (stamps on lines and updates).
    pub today: NaiveDate,
}

impl Writer<'_> {
    /// Existing claims of a folder (file stems of `<folder>/Permanent/`).
    pub async fn claim_index(&self, folder: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .store
            .list(&format!("{folder}/{PERMANENT_DIR}"))
            .await?
            .iter()
            .map(|n| stem(n).to_owned())
            .collect())
    }

    /// Read, change, write back only if the page did not move meanwhile.
    /// `change` gets the current text (`None`: missing) and returns the new
    /// text, or `None` to leave it. Returns whether the page changed.
    async fn update(
        &self,
        rel: &str,
        change: impl Fn(Option<&str>) -> Option<String>,
    ) -> Result<bool, StoreError> {
        let cur = self.store.read(rel).await?;
        let Some(new) = change(cur.as_deref()) else {
            return Ok(false);
        };
        if cur.as_deref() == Some(new.as_str()) {
            return Ok(false);
        }
        let new = crate::redact::redact(&new);
        self.store
            .write(rel, &new, version(cur.as_deref()).as_deref())
            .await
    }

    /// `<folder>/Events/YYYY-MM-DD <title>.md`, or `… (n).md` when that name
    /// belongs to another item.
    async fn event_path(&self, folder: &str, item: &Item) -> Result<String, StoreError> {
        let t = event_name(&item.title, item.source.as_str());
        let base = format!("{} {t}", item.at.format("%Y-%m-%d"));
        let own = format!("ingest_key: {}", item.key);
        for n in 1..100 {
            let stem = if n == 1 {
                base.clone()
            } else {
                format!("{base} ({n})")
            };
            let p = format!("{folder}/{EVENTS_DIR}/{stem}.md");
            match self.store.read(&p).await? {
                None => return Ok(p),
                Some(text) if text.lines().any(|l| l.trim() == own) => return Ok(p),
                Some(_) => {}
            }
        }
        Err(StoreError::Other("no free event page name".into()))
    }

    /// Deliver one item's plan into `folder`. Returns the pages written or
    /// confirmed.
    pub async fn write(
        &self,
        folder: &str,
        item: &Item,
        plan: &Plan,
    ) -> Result<Vec<String>, StoreError> {
        let mark = marker(&item.key);
        let event = self.event_path(folder, item).await?;
        let event_stem = stem(&event).to_owned();
        let existing = self.claim_index(folder).await?;
        let mut pages = Vec::new();
        let mut claim_stems = Vec::new();
        for c in &plan.claims {
            let s = existing
                .iter()
                .find(|s| normalize(s) == normalize(&c.title))
                .cloned()
                .unwrap_or_else(|| clean_title(&c.title));
            let rel = format!("{folder}/{PERMANENT_DIR}/{s}.md");
            self.update(&rel, |cur| {
                Some(match cur {
                    Some(old) => self.amend(old, c, item, &event_stem, &mark),
                    None => self.new_claim(c, item, folder, &event_stem, &mark),
                })
            })
            .await?;
            claim_stems.push(s);
            pages.push(rel);
        }
        let ev = fill(
            &self.templates.event,
            &[
                ("tags", tags(item)),
                ("date", item.at.format("%Y-%m-%d").to_string()),
                ("key", item.key.clone()),
                ("title", event_stem.clone()),
                ("summary", plan.summary.clone()),
                ("outputs", bullets(&claim_stems, |s| format!("[[{s}]]"))),
                (
                    "source",
                    format!("- {} {mark}", backlink_text(&item.backlink)),
                ),
                ("hub", hub_name(folder)),
                ("moc", moc_name(folder)),
                ("folder", folder.to_owned()),
            ],
        );
        self.update(&event, |_| Some(ev.clone())).await?;
        pages.push(event.clone());
        pages.push(
            self.update_moc(folder, item, &claim_stems, &event_stem, &mark)
                .await?,
        );
        pages.extend(self.update_hub(folder, item, &event_stem, &mark).await?);
        Ok(pages)
    }

    fn source_line(&self, item: &Item, event_stem: &str, mark: &str) -> String {
        format!(
            "- {} [[{event_stem}]] · {} {mark}",
            item.at.format("%Y-%m-%d"),
            backlink_text(&item.backlink)
        )
    }

    fn new_claim(
        &self,
        c: &Claim,
        item: &Item,
        folder: &str,
        event_stem: &str,
        mark: &str,
    ) -> String {
        fill(
            &self.templates.permanent,
            &[
                ("tags", tags(item)),
                ("date", self.today.to_string()),
                ("source", backlink_text(&item.backlink).replace('`', "")),
                ("title", c.title.clone()),
                ("body", c.body.clone()),
                ("files", bullets(&c.files, |f| format!("`{f}`"))),
                (
                    "connects_to",
                    bullets(&c.connects_to, |t| format!("[[{t}]]")),
                ),
                ("enables", bullets(&c.enables, |t| format!("[[{t}]]"))),
                ("hub", hub_name(folder)),
                ("moc", moc_name(folder)),
                ("event", event_stem.to_owned()),
                ("sources", self.source_line(item, event_stem, mark)),
            ],
        )
    }

    /// Amend an existing claim (the owner's or an earlier item's).
    pub fn amend(&self, old: &str, c: &Claim, item: &Item, event_stem: &str, mark: &str) -> String {
        if old.contains(mark) {
            return old.to_owned();
        }
        let mut t = old.to_owned();
        let update = if c.update.is_empty() {
            c.body.clone()
        } else {
            c.update.clone()
        };
        let update = crate::clip(&update.replace('\n', " "), 600);
        t = add_lines(
            &t,
            "Updates",
            &[format!(
                "- {}: {update} ([[{event_stem}]]) {mark}",
                self.today
            )],
            false,
            None,
        );
        let files: Vec<String> = c.files.iter().map(|f| format!("- `{f}`")).collect();
        t = add_lines(&t, "Project files", &files, false, None);
        let links = |v: &[String]| -> Vec<String> {
            v.iter()
                .filter(|l| !old.contains(&format!("[[{l}]]")))
                .map(|l| format!("- [[{l}]]"))
                .collect()
        };
        t = add_lines(&t, "Connects to", &links(&c.connects_to), false, None);
        t = add_lines(&t, "Enables", &links(&c.enables), false, None);
        t = add_lines(&t, "Part of", &[format!("- [[{event_stem}]]")], false, None);
        t = add_lines(
            &t,
            "Sources",
            &[self.source_line(item, event_stem, mark)],
            false,
            None,
        );
        touch_updated(&t, self.today)
    }

    async fn update_moc(
        &self,
        folder: &str,
        item: &Item,
        claims: &[String],
        event_stem: &str,
        mark: &str,
    ) -> Result<String, StoreError> {
        let rel = format!("{folder}/{MOC_DIR}/{}.md", moc_name(folder));
        let fresh = fill(
            &self.templates.moc,
            &[
                ("tags", format!("  - {}", item.project)),
                ("folder", folder.to_owned()),
                ("hub", hub_name(folder)),
                ("moc", moc_name(folder)),
            ],
        );
        self.update(&rel, |cur| {
            let old = cur.map(str::to_owned).unwrap_or_else(|| fresh.clone());
            let lines: Vec<String> = claims
                .iter()
                .filter(|s| !old.contains(&format!("[[{s}]]")))
                .map(|s| format!("- [[{s}]]"))
                .collect();
            let mut t = add_lines(&old, "Claims", &lines, false, None);
            if !t.contains(mark) {
                t = add_lines(
                    &t,
                    "Events",
                    &[format!("- [[{event_stem}]] {mark}")],
                    true,
                    Some(MOC_EVENTS),
                );
            }
            Some(t)
        })
        .await?;
        Ok(rel)
    }

    /// The folder's hub (created from the template when missing, then linked
    /// from `00 Home.md`): the change log and the event, newest first.
    async fn update_hub(
        &self,
        folder: &str,
        item: &Item,
        event_stem: &str,
        mark: &str,
    ) -> Result<Vec<String>, StoreError> {
        let hub = hub_name(folder);
        let rel = format!("{folder}/{hub}.md");
        let existed = self.store.read(&rel).await?.is_some();
        let fresh = fill(
            &self.templates.hub,
            &[
                ("folder", folder.to_owned()),
                ("hub", hub.clone()),
                ("moc", moc_name(folder)),
            ],
        );
        let moc = format!("[[{}]]", moc_name(folder));
        self.update(&rel, |cur| {
            let old = cur.map(str::to_owned).unwrap_or_else(|| fresh.clone());
            let mut t = old.clone();
            if !t.contains(&moc) {
                let maps = section_named(&t, &["Maps of content", "Map", "Maps"]);
                t = add_lines(&t, maps, &[format!("- {moc}")], false, None);
            }
            if !t.contains(mark) {
                let events = section_named(&t, &["Events", "Recent events"]);
                t = add_lines(
                    &t,
                    events,
                    &[format!("- [[{event_stem}]] · {} {mark}", item.project)],
                    true,
                    Some(HUB_EVENTS),
                );
            }
            Some(t)
        })
        .await?;
        let mut pages = vec![rel];
        if !existed {
            // A new folder is linked from the vault's home page (never created here).
            let link = format!("[[{hub}]]");
            let changed = self
                .update(HOME, |cur| {
                    let old = cur?;
                    if old.contains(&link) {
                        return None;
                    }
                    let lines: Vec<String> = old.lines().map(str::to_owned).collect();
                    let table = section(&lines, "Projects")
                        .and_then(|(s, e)| lines[s..e].iter().rev().find(|l| !l.trim().is_empty()))
                        .is_some_and(|l| l.trim_start().starts_with('|'));
                    let line = if table {
                        format!("| {folder} | {link} |")
                    } else {
                        format!("- {link}")
                    };
                    Some(add_lines(old, "Projects", &[line], false, None))
                })
                .await?;
            if changed {
                pages.push(HOME.to_owned());
            }
        }
        Ok(pages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections() {
        let page = "# T\n\nbody\n\n## Sources\n\n- a\n\n## Index\n\n- i\n";
        let out = add_lines(page, "Sources", &["- b".into()], false, None);
        assert_eq!(
            out,
            "# T\n\nbody\n\n## Sources\n\n- a\n- b\n\n## Index\n\n- i\n"
        );
        assert_eq!(
            add_lines(&out, "Sources", &["- b".into()], false, None),
            out,
            "no duplicate"
        );
        let out = add_lines(page, "Updates", &["- u".into()], false, None);
        assert!(
            out.ends_with("## Index\n\n- i\n\n## Updates\n\n- u\n"),
            "{out}"
        );
        let out = add_lines(
            "## Enables\n\n- —\n",
            "Enables",
            &["- [[x]]".into()],
            false,
            None,
        );
        assert_eq!(out, "## Enables\n\n- [[x]]\n");
        let mut ev = "## Events\n".to_owned();
        for i in 0..5 {
            ev = add_lines(
                &ev,
                "Events",
                &[format!("- e{i} <!-- ingest:{i} -->")],
                true,
                Some(3),
            );
        }
        assert_eq!(
            ev,
            "## Events\n\n- e4 <!-- ingest:4 -->\n- e3 <!-- ingest:3 -->\n- e2 <!-- ingest:2 -->\n"
        );
        assert_eq!(
            section_named("## Recent events\n", &["Events", "Recent events"]),
            "Recent events"
        );
        assert_eq!(
            section_named("# x\n", &["Events", "Recent events"]),
            "Events"
        );
    }

    #[test]
    fn updated_field() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(
            touch_updated(
                "---\ntype: x\nupdated: 2026-01-01\n---\n# t\nupdated: keep\n",
                d
            ),
            "---\ntype: x\nupdated: 2026-11-01\n---\n# t\nupdated: keep\n"
        );
        assert_eq!(touch_updated("# no fm\n", d), "# no fm\n");
    }

    #[test]
    fn event_names() {
        assert_eq!(
            event_name("iOS launch perf landed on main 6826494e: incremental photo backup, endpoint race, lighter chat", "x"),
            "iOS launch perf landed on main 6826494e incremental photo backup, endpoint"
        );
        assert_eq!(event_name("short one", "x"), "short one");
        assert_eq!(event_name("::", "session"), "session");
    }

    #[test]
    fn backlinks() {
        let b = Backlink::Git {
            repo: "lucina-rs".into(),
            path: "/p".into(),
            branch: "origin/main".into(),
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            merge: true,
        };
        assert_eq!(
            backlink_text(&b),
            "merge `0123456789ab` on lucina-rs `origin/main` (`/p`)"
        );
        let b = Backlink::Ops {
            id: "abcd".into(),
            log: "Log/2026-11-01.md".into(),
            incident: Some("Incidents/2026-11-01-x.md".into()),
            commit: Some("372dec64".into()),
        };
        assert_eq!(
            backlink_text(&b),
            "ops record `abcd` (Lucina ops `Log/2026-11-01.md`, `Incidents/2026-11-01-x.md`) · commit `372dec64`"
        );
    }
}
