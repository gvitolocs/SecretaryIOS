//! Writing a plan into the vault: claims in `Permanent/`, the event digest in
//! `Events/`, the project's map in `MOC/`, the hub.
//!
//! - A claim whose title (normalized) names an existing note is amended, never
//!   rewritten: a source line, a dated update, missing files and links are
//!   added to its sections; the owner's text stays as it is.
//! - Every line the worker adds ends in `<!-- ingest:<key> -->`; a page that
//!   already holds the item's marker is left alone, so a replay writes nothing.
//! - The event's file name comes from the source (date + its title), not from
//!   the model, so a retry lands on the same page.

use chrono::NaiveDate;
use lucina_ops::{vault, Vault};

use crate::plan::{clean_title, normalize, Claim, Plan};
use crate::queue::{Backlink, Item};
use crate::templates::{fill, Templates};
use crate::IngestError;

/// Event lines kept on the hub and on a MOC.
const HUB_EVENTS: usize = 60;
const MOC_EVENTS: usize = 40;
/// Characters of the source title in an event's file name.
const EVENT_NAME: usize = 80;

pub fn marker(key: &str) -> String {
    format!("<!-- ingest:{key} -->")
}

fn page(dir: &str, stem: &str) -> String {
    format!("{dir}/{stem}.md")
}

/// The project's page in the ops folder (`[[Projects/x|x]]`).
fn project_page(project: &str) -> String {
    lucina_ops::render::link_target(&lucina_ops::render::project_path(project)).to_owned()
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
    let mut t = vec![
        "lucina".to_owned(),
        item.project.clone(),
        item.source.as_str().to_owned(),
    ];
    t.dedup();
    t.iter()
        .map(|x| format!("  - {x}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How the notes link back to the item's source.
pub fn backlink_text(b: &Backlink) -> String {
    match b {
        Backlink::Ops {
            id,
            log,
            incident,
            commit,
        } => {
            let mut s = format!(
                "ops record `{id}` · [[{}]]",
                lucina_ops::render::link_target(log)
            );
            if let Some(i) = incident {
                s.push_str(&format!(" · [[{}]]", lucina_ops::render::link_target(i)));
            }
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

/// The event's page: `Events/YYYY-MM-DD <title>.md`, or `… (n).md` when that
/// name belongs to another item.
fn event_path(v: &Vault, item: &Item) -> Result<String, IngestError> {
    let mut t = clean_title(&item.title);
    if t.chars().count() > EVENT_NAME {
        t = clean_title(&t.chars().take(EVENT_NAME).collect::<String>());
    }
    if t.is_empty() {
        t = item.source.as_str().into();
    }
    let base = format!("{} {t}", item.at.format("%Y-%m-%d"));
    let own = format!("ingest_key: {}", item.key);
    for n in 1..100 {
        let stem = if n == 1 {
            base.clone()
        } else {
            format!("{base} ({n})")
        };
        let p = page(vault::EVENTS_DIR, &stem);
        match v.read(&p)? {
            None => return Ok(p),
            Some(text) if text.lines().any(|l| l.trim() == own) => return Ok(p),
            Some(_) => {}
        }
    }
    Err(IngestError::Io("no free event page name".into()))
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
    pub vault: &'a Vault,
    pub templates: &'a Templates,
    /// The owner's local date (stamps on lines and updates).
    pub today: NaiveDate,
}

impl Writer<'_> {
    /// Existing claims: normalized title → file stem.
    pub fn claim_index(&self) -> Result<Vec<String>, IngestError> {
        Ok(self
            .vault
            .list_pages(vault::PERMANENT_DIR)?
            .iter()
            .map(|n| stem(n).to_owned())
            .collect())
    }

    /// Write one item's plan. Returns the pages it changed or confirmed.
    pub fn write(&self, item: &Item, plan: &Plan) -> Result<Vec<String>, IngestError> {
        let mark = marker(&item.key);
        let event = event_path(self.vault, item)?;
        let event_stem = stem(&event).to_owned();
        let existing = self.claim_index()?;
        let mut pages = Vec::new();
        let mut claim_stems = Vec::new();
        for c in &plan.claims {
            let found = existing
                .iter()
                .find(|s| normalize(s) == normalize(&c.title));
            let (rel, text) = match found {
                Some(s) => {
                    let rel = page(vault::PERMANENT_DIR, s);
                    let old = self.vault.read(&rel)?.unwrap_or_default();
                    (rel, self.amend(&old, c, item, &event_stem, &mark))
                }
                None => {
                    let s = clean_title(&c.title);
                    let rel = page(vault::PERMANENT_DIR, &s);
                    (rel, self.new_claim(c, item, &event_stem, &mark))
                }
            };
            self.vault.write_note(&rel, &text)?;
            claim_stems.push(stem(&rel).to_owned());
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
                ("project_page", project_page(&item.project)),
                ("project", item.project.clone()),
            ],
        );
        self.vault.write_note(&event, &ev)?;
        pages.push(event.clone());
        pages.push(self.update_moc(item, &claim_stems, &event_stem, &mark)?);
        pages.push(self.update_hub(item, &event_stem, &mark)?);
        Ok(pages)
    }

    fn source_line(&self, item: &Item, event_stem: &str, mark: &str) -> String {
        format!(
            "- {} [[{event_stem}]] · {} {mark}",
            item.at.format("%Y-%m-%d"),
            backlink_text(&item.backlink)
        )
    }

    fn new_claim(&self, c: &Claim, item: &Item, event_stem: &str, mark: &str) -> String {
        fill(
            &self.templates.permanent,
            &[
                ("tags", tags(item)),
                ("date", self.today.to_string()),
                ("source", item.key.clone()),
                ("title", c.title.clone()),
                ("body", c.body.clone()),
                ("files", bullets(&c.files, |f| format!("`{f}`"))),
                (
                    "connects_to",
                    bullets(&c.connects_to, |t| format!("[[{t}]]")),
                ),
                ("enables", bullets(&c.enables, |t| format!("[[{t}]]"))),
                ("project_page", project_page(&item.project)),
                ("project", item.project.clone()),
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

    fn update_moc(
        &self,
        item: &Item,
        claims: &[String],
        event_stem: &str,
        mark: &str,
    ) -> Result<String, IngestError> {
        let rel = page(vault::MOC_DIR, &format!("MOC {}", item.project));
        let old = match self.vault.read(&rel)? {
            Some(t) => t,
            None => fill(
                &self.templates.moc,
                &[
                    ("tags", format!("  - {}", item.project)),
                    ("project", item.project.clone()),
                    ("project_page", project_page(&item.project)),
                ],
            ),
        };
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
        self.vault.write_note(&rel, &t)?;
        Ok(rel)
    }

    fn update_hub(&self, item: &Item, event_stem: &str, mark: &str) -> Result<String, IngestError> {
        let rel = vault::HUB.to_owned();
        let old = self
            .vault
            .read(&rel)?
            .unwrap_or_else(|| self.templates.hub.clone());
        let moc = format!("[[MOC {}]]", item.project);
        let mut t = if old.contains(&moc) {
            old.clone()
        } else {
            add_lines(&old, "Projects", &[format!("- {moc}")], false, None)
        };
        if !t.contains(mark) {
            t = add_lines(
                &t,
                "Events",
                &[format!("- [[{event_stem}]] · {} {mark}", item.project)],
                true,
                Some(HUB_EVENTS),
            );
        }
        self.vault.write_note(&rel, &t)?;
        Ok(rel)
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
            "ops record `abcd` · [[Log/2026-11-01]] · [[Incidents/2026-11-01-x]] · commit `372dec64`"
        );
    }
}
