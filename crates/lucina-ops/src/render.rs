//! Markdown of the vault pages: the daily log, the project pages and the
//! incident pages, and the plain text sent to Honcho.
//!
//! Ops only ever inserts (or trims) its own entries, each ending in a marker
//! `<!-- ops:<id> -->`; every other line of a page stays as it is.

use chrono::{DateTime, FixedOffset};

use crate::record::{single_line, Kind, StoredRecord};

pub const MARKER_OPEN: &str = "<!-- ops:";
/// Sections of a project page, in order.
pub const SECTIONS: [&str; 6] = [
    "Status",
    "Decisions",
    "Incidents",
    "Deploys",
    "Findings",
    "Open questions",
];
pub const OPEN_QUESTIONS: &str = "Open questions";
/// Status entries kept on a project page.
pub const STATUS_KEEP: usize = 10;
const LOG_BODY_CHARS: usize = 300;
const PAGE_BODY_CHARS: usize = 1200;
const HONCHO_CHARS: usize = 6000;

pub fn marker(id: &str) -> String {
    format!("{MARKER_OPEN}{id} -->")
}

pub fn follow_up_marker(id: &str, n: usize) -> String {
    format!("{MARKER_OPEN}{id}:f{n} -->")
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

/// `Log/2026-10-28.md`
pub fn log_path(at: &DateTime<FixedOffset>) -> String {
    format!("Log/{}.md", at.format("%Y-%m-%d"))
}

pub fn project_path(project: &str) -> String {
    format!("Projects/{project}.md")
}

/// Wikilink target of a page (`Projects/x.md` -> `Projects/x`).
pub fn link_target(rel: &str) -> &str {
    rel.strip_suffix(".md").unwrap_or(rel)
}

/// Lowercase ASCII, `-` separated, at most 60 characters.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.to_lowercase().chars() {
        let c = fold_char(c);
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(60).collect();
    s.trim_end_matches('-').to_owned()
}

/// Common Latin letters with accents to their ASCII base.
pub fn fold_char(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ä' | 'å' | 'ã' | 'æ' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'ö' | 'ø' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ß' => 's',
        c => c,
    }
}

/// `(`abc1234` on nezopt, severity high)` or empty.
fn details(r: &StoredRecord, with_severity: bool) -> String {
    let mut first = String::new();
    if let Some(c) = &r.commit {
        first.push_str(&format!("`{}`", &c[..c.len().min(7)]));
    }
    if let Some(h) = &r.host {
        if !first.is_empty() {
            first.push(' ');
        }
        first.push_str(&format!("on {h}"));
    }
    let mut parts = Vec::new();
    if !first.is_empty() {
        parts.push(first);
    }
    if with_severity {
        if let Some(s) = r.severity {
            parts.push(format!("severity {}", s.as_str()));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// The page lines of caller text: never a heading or a fence at column 0.
fn text_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| {
            let l = l.trim_end();
            if l.starts_with('#') {
                format!("\\{l}")
            } else {
                l.to_owned()
            }
        })
        .collect()
}

// ---------------------------------------------------------------- daily log

pub fn log_header(at: &DateTime<FixedOffset>) -> String {
    format!("# {}\n", at.format("%Y-%m-%d"))
}

/// The entry of `r` in its daily log (with its continuation line).
pub fn log_entry(r: &StoredRecord) -> Vec<String> {
    let first = format!(
        "- {} **{}** · [[{}|{}]] · {}{} · {} {}",
        r.at.format("%H:%M"),
        r.kind.as_str(),
        link_target(&r.paths.project),
        r.project,
        single_line(&r.title),
        details(r, false),
        r.source,
        marker(&r.id)
    );
    let mut out = vec![first];
    let mut cont = clip(&single_line(&r.body), LOG_BODY_CHARS);
    if let Some(inc) = &r.paths.incident {
        if !cont.is_empty() {
            cont.push(' ');
        }
        cont.push_str(&format!("[[{}|incident]]", link_target(inc)));
    }
    if !cont.is_empty() {
        out.push(format!("  {cont}"));
    }
    out
}

/// `HH:MM` of a top-level log entry line.
fn entry_time(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("- ")?;
    let t = rest.get(..5)?;
    let b = t.as_bytes();
    (b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit())
    .then_some(t)
}

fn is_continuation(line: &str) -> bool {
    !line.trim().is_empty() && (line.starts_with(' ') || line.starts_with('\t'))
}

/// Insert `entry` (time `hhmm`) at its chronological place in a daily log.
pub fn insert_log_entry(page: Option<&str>, header: &str, hhmm: &str, entry: &[String]) -> String {
    let text = match page {
        Some(p) if !p.trim().is_empty() => p.to_owned(),
        _ => format!("{header}\n"),
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // The end of the block of the last entry at or before `hhmm`.
    let mut insert_at = None;
    let mut first_entry = None;
    let mut i = 0;
    while i < lines.len() {
        if let Some(t) = entry_time(&lines[i]) {
            first_entry.get_or_insert(i);
            let le = t <= hhmm;
            let mut j = i + 1;
            while j < lines.len() && is_continuation(&lines[j]) {
                j += 1;
            }
            if le {
                insert_at = Some(j);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let at = match (insert_at, first_entry) {
        (Some(a), _) => a,
        (None, Some(f)) => f,
        (None, None) => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.len()
        }
    };
    for (k, l) in entry.iter().enumerate() {
        lines.insert(at + k, l.clone());
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ---------------------------------------------------------------- project page

pub fn new_project_page(project: &str) -> String {
    let mut s = format!("---\ntags: [lucina-ops]\nproject: {project}\n---\n# {project}\n\n");
    for name in SECTIONS {
        s.push_str(&format!("## {name}\n\n"));
    }
    s
}

/// `2026-10-28 14:05` of a project page entry.
fn entry_key(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("- ")?;
    let k = rest.get(..16)?;
    let b = k.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b' '
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && line.contains(MARKER_OPEN))
    .then_some(k)
}

/// The entry of `r` in its project section.
pub fn project_entry(r: &StoredRecord) -> Vec<String> {
    let mut out = vec![format!(
        "- {} · **{}**{} {}",
        r.at.format("%Y-%m-%d %H:%M"),
        single_line(&r.title),
        details(r, r.kind == Kind::Incident),
        marker(&r.id)
    )];
    for l in text_lines(&clip(&r.body, PAGE_BODY_CHARS)) {
        if !l.trim().is_empty() {
            out.push(format!("  {l}"));
        }
    }
    if let Some(inc) = &r.paths.incident {
        out.push(format!("  - [[{}|incident page]]", link_target(inc)));
    }
    for l in &r.links {
        out.push(format!("  - {}", single_line(l)));
    }
    out
}

pub fn follow_up_entry(r: &StoredRecord, n: usize, text: &str) -> String {
    let day = r.at.format("%Y-%m-%d");
    format!(
        "- [ ] {} ([[Log/{day}|{day}]]) {}",
        single_line(text),
        follow_up_marker(&r.id, n)
    )
}

/// Lines of a page and where its body starts (after the frontmatter).
fn body_start(lines: &[String]) -> usize {
    if lines.first().map(|l| l.trim_end()) == Some("---") {
        if let Some(end) = lines.iter().skip(1).position(|l| l.trim_end() == "---") {
            return end + 2;
        }
    }
    0
}

fn section_header(line: &str) -> Option<&str> {
    line.strip_prefix("## ").map(str::trim)
}

/// `(header line, end)` of section `name` (end = the next `## ` or EOF);
/// appended at the end when the owner deleted it.
fn find_or_add_section(lines: &mut Vec<String>, name: &str) -> (usize, usize) {
    let start = body_start(lines);
    let found = (start..lines.len()).find(|&i| section_header(&lines[i]) == Some(name));
    let h = match found {
        Some(h) => h,
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.push(format!("## {name}"));
            lines.push(String::new());
            lines.len() - 2
        }
    };
    let end = (h + 1..lines.len())
        .find(|&i| section_header(&lines[i]).is_some())
        .unwrap_or(lines.len());
    (h, end)
}

/// Ops entry blocks of a section: `(start, end, key)`.
fn entry_blocks(lines: &[String], from: usize, to: usize) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        if let Some(k) = entry_key(&lines[i]) {
            let mut j = i + 1;
            while j < to && is_continuation(&lines[j]) {
                j += 1;
            }
            out.push((i, j, k.to_owned()));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Insert `block` into section (`h`, `end`) at `at`, keeping one blank line
/// before the next section.
fn insert_block(
    lines: &mut Vec<String>,
    h: usize,
    end: usize,
    at: Option<usize>,
    block: &[String],
) {
    let at = match at {
        Some(a) => a,
        None => {
            // The top of the section: after the header and its blank line.
            let mut a = h + 1;
            if a < end && lines[a].trim().is_empty() {
                a += 1;
            } else {
                lines.insert(a, String::new());
                a += 1;
            }
            a
        }
    };
    for (k, l) in block.iter().enumerate() {
        lines.insert(at + k, l.clone());
    }
    let after = at + block.len();
    if after < lines.len() && section_header(&lines[after]).is_some() {
        lines.insert(after, String::new());
    }
}

fn finish(lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// What is missing for `r` on a project page.
pub struct ProjectInsert<'a> {
    pub record: &'a StoredRecord,
    /// Insert the section entry.
    pub entry: bool,
    /// Follow-ups to add to Open questions: `(n, text)` (n from 1).
    pub follow_ups: Vec<(usize, &'a str)>,
}

impl<'a> ProjectInsert<'a> {
    /// Everything of a new record.
    pub fn all(r: &'a StoredRecord) -> Self {
        Self {
            record: r,
            entry: true,
            follow_ups: r
                .follow_ups
                .iter()
                .enumerate()
                .map(|(i, t)| (i + 1, t.as_str()))
                .collect(),
        }
    }

    /// Only what the page lacks (conflict repair).
    pub fn missing(r: &'a StoredRecord, page: &str) -> Self {
        Self {
            record: r,
            entry: !page.contains(&marker(&r.id)),
            follow_ups: r
                .follow_ups
                .iter()
                .enumerate()
                .map(|(i, t)| (i + 1, t.as_str()))
                .filter(|(n, _)| !page.contains(&follow_up_marker(&r.id, *n)))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.entry && self.follow_ups.is_empty()
    }
}

/// The project page with `ins` applied (created when missing).
pub fn update_project_page(page: Option<&str>, project: &str, ins: &ProjectInsert) -> String {
    let text = match page {
        Some(p) if !p.trim().is_empty() => p.to_owned(),
        _ => new_project_page(project),
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let r = ins.record;
    if ins.entry {
        let name = r.kind.section();
        let (h, end) = find_or_add_section(&mut lines, name);
        let key = r.at.format("%Y-%m-%d %H:%M").to_string();
        let blocks = entry_blocks(&lines, h + 1, end);
        // Newest first: before the first entry not newer than this one.
        let at = blocks
            .iter()
            .find(|(_, _, k)| k.as_str() <= key.as_str())
            .map(|(s, _, _)| *s)
            .or_else(|| blocks.last().map(|(_, e, _)| *e));
        insert_block(&mut lines, h, end, at, &project_entry(r));
        if r.kind == Kind::Status {
            let (h, end) = find_or_add_section(&mut lines, name);
            let mut blocks = entry_blocks(&lines, h + 1, end);
            if blocks.len() > STATUS_KEEP {
                // Drop the oldest (by time), from the bottom up.
                blocks.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
                let mut drop: Vec<(usize, usize)> = blocks[STATUS_KEEP..]
                    .iter()
                    .map(|(s, e, _)| (*s, *e))
                    .collect();
                drop.sort_by_key(|d| std::cmp::Reverse(d.0));
                for (s, e) in drop {
                    lines.drain(s..e);
                }
            }
        }
    }
    if !ins.follow_ups.is_empty() {
        let (h, end) = find_or_add_section(&mut lines, OPEN_QUESTIONS);
        let block: Vec<String> = ins
            .follow_ups
            .iter()
            .map(|(n, t)| follow_up_entry(r, *n, t))
            .collect();
        insert_block(&mut lines, h, end, None, &block);
    }
    finish(lines)
}

// ---------------------------------------------------------------- incident page

/// `Incidents/YYYY-MM-DD-<project>-<slug>[-N].md`
pub fn incident_path(r_at: &DateTime<FixedOffset>, project: &str, title: &str, n: usize) -> String {
    let mut s = slug(title);
    if s.is_empty() {
        s = "incident".into();
    }
    let suffix = if n > 1 {
        format!("-{n}")
    } else {
        String::new()
    };
    format!(
        "Incidents/{}-{project}-{s}{suffix}.md",
        r_at.format("%Y-%m-%d")
    )
}

/// The `ops_id` in an incident page's frontmatter.
pub fn incident_owner(page: &str) -> Option<&str> {
    page.lines()
        .take_while(|l| !l.starts_with("# "))
        .find_map(|l| l.strip_prefix("ops_id: "))
        .map(str::trim)
}

pub fn incident_page(r: &StoredRecord) -> String {
    let mut s = String::from("---\ntags: [lucina-ops, incident]\n");
    s.push_str(&format!("ops_id: {}\n", r.id));
    s.push_str(&format!("project: {}\n", r.project));
    if let Some(sev) = r.severity {
        s.push_str(&format!("severity: {}\n", sev.as_str()));
    }
    s.push_str(&format!(
        "status: {}\n",
        if r.fix.is_some() { "resolved" } else { "open" }
    ));
    s.push_str(&format!("at: \"{}\"\n", r.at.to_rfc3339()));
    if let Some(c) = &r.commit {
        s.push_str(&format!("commit: \"{c}\"\n"));
    }
    if let Some(h) = &r.host {
        s.push_str(&format!("host: \"{h}\"\n"));
    }
    s.push_str(&format!("source: \"{}\"\n---\n", r.source));
    s.push_str(&format!(
        "# {}\n{}\n\n",
        single_line(&r.title),
        marker(&r.id)
    ));
    s.push_str(&format!(
        "[[{}|{}]] · [[{}|{}]]\n\n",
        link_target(&r.paths.project),
        r.project,
        link_target(&r.paths.log),
        r.at.format("%Y-%m-%d %H:%M")
    ));
    let block = |s: &mut String, title: &str, text: Option<&str>| {
        s.push_str(&format!("## {title}\n\n"));
        match text.filter(|t| !t.trim().is_empty()) {
            Some(t) => s.push_str(&text_lines(t).join("\n")),
            None => s.push_str("_unknown_"),
        }
        s.push_str("\n\n");
    };
    block(&mut s, "Summary", Some(&r.body));
    if !r.timeline.is_empty() {
        s.push_str("## Timeline\n\n");
        for t in &r.timeline {
            s.push_str(&format!("- {}\n", single_line(t)));
        }
        s.push('\n');
    }
    block(&mut s, "Cause", r.cause.as_deref());
    block(&mut s, "Fix", r.fix.as_deref());
    if !r.follow_ups.is_empty() {
        s.push_str("## Follow-ups\n\n");
        for f in &r.follow_ups {
            s.push_str(&format!("- [ ] {}\n", single_line(f)));
        }
        s.push('\n');
    }
    if !r.links.is_empty() {
        s.push_str("## Links\n\n");
        for l in &r.links {
            s.push_str(&format!("- {}\n", single_line(l)));
        }
        s.push('\n');
    }
    while s.ends_with("\n\n") {
        s.pop();
    }
    s
}

// ---------------------------------------------------------------- honcho

/// `[kind] project: title\nbody\n(cause: ..)(fix: ..)(commit .. on host ..)(links: ..)`
pub fn honcho_content(r: &StoredRecord) -> String {
    let mut s = format!("[{}] {}: {}", r.kind.as_str(), r.project, r.title);
    if !r.body.trim().is_empty() {
        s.push('\n');
        s.push_str(r.body.trim());
    }
    let mut tail = String::new();
    if let Some(c) = &r.cause {
        tail.push_str(&format!("(cause: {c})"));
    }
    if let Some(f) = &r.fix {
        tail.push_str(&format!("(fix: {f})"));
    }
    match (&r.commit, &r.host) {
        (Some(c), Some(h)) => tail.push_str(&format!("(commit {c} on host {h})")),
        (Some(c), None) => tail.push_str(&format!("(commit {c})")),
        (None, Some(h)) => tail.push_str(&format!("(host {h})")),
        (None, None) => {}
    }
    if !r.links.is_empty() {
        tail.push_str(&format!("(links: {})", r.links.join(", ")));
    }
    if !tail.is_empty() {
        s.push('\n');
        s.push_str(&tail);
    }
    clip(&s, HONCHO_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_insertion_is_chronological() {
        let h = "# 2026-10-28\n";
        let a = vec!["- 10:00 a".to_owned(), "  more a".to_owned()];
        let b = vec!["- 12:00 b".to_owned()];
        let c = vec!["- 11:00 c".to_owned()];
        let p = insert_log_entry(None, h, "10:00", &a);
        let p = insert_log_entry(Some(&p), h, "12:00", &b);
        let p = insert_log_entry(Some(&p), h, "11:00", &c);
        let p = insert_log_entry(Some(&p), h, "09:00", &["- 09:00 d".to_owned()]);
        assert_eq!(
            p,
            "# 2026-10-28\n\n- 09:00 d\n- 10:00 a\n  more a\n- 11:00 c\n- 12:00 b\n"
        );
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Pi: Pokoin sync outage!"), "pi-pokoin-sync-outage");
        assert_eq!(slug("Perché è giù?"), "perche-e-giu");
        assert_eq!(slug("???"), "");
        assert!(slug(&"x".repeat(100)).len() == 60);
    }
}
