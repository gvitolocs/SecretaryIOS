//! `git diff` output into Paseo's `ParsedDiffFile` list (`server/utils/checkout-git.js`).
//!
//! The input must come from `git -c core.quotePath=false diff --no-color --no-ext-diff
//! --no-textconv --find-renames --src-prefix=a/ --dst-prefix=b/`, so the owner's git
//! settings can't change the format. Files come back sorted by path; a file with more
//! than `max_lines` changed lines is `too_large` (no hunks), a binary one `binary`.

use crate::workspace::{DiffFile, DiffHunk, DiffLine};

fn strip_side(p: &str, side: &str) -> String {
    p.strip_prefix(side).unwrap_or(p).to_string()
}

/// `@@ -a[,b] +c[,d] @@` → (a, b, c, d).
fn hunk_range(header: &str) -> Option<(u64, u64, u64, u64)> {
    let inner = header.strip_prefix("@@ ")?;
    let end = inner.find(" @@")?;
    let mut parts = inner[..end].split(' ');
    let range = |s: &str, sign: char| -> Option<(u64, u64)> {
        let s = s.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = range(parts.next()?, '-')?;
    let (c, d) = range(parts.next()?, '+')?;
    Some((a, b, c, d))
}

pub fn parse(diff: &str, max_lines: usize) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut changed = 0usize;
    let finish = |f: Option<DiffFile>, changed: usize, files: &mut Vec<DiffFile>| {
        if let Some(mut f) = f {
            if f.status == "ok" && changed > max_lines {
                f.status = "too_large".into();
                f.hunks.clear();
            }
            files.push(f);
        }
    };
    for line in diff.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish(cur.take(), changed, &mut files);
            changed = 0;
            // "a/<path> b/<path>": with core.quotePath=false the paths are plain; split
            // in the middle when both sides are the same.
            let (a, b) = match rest.find(" b/") {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, rest),
            };
            cur = Some(DiffFile {
                path: strip_side(b, "b/"),
                old_path: Some(strip_side(a, "a/")),
                is_new: false,
                is_deleted: false,
                additions: 0,
                deletions: 0,
                hunks: Vec::new(),
                status: "ok".into(),
            });
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if f.hunks.is_empty() {
            if line.starts_with("new file mode") {
                f.is_new = true;
                continue;
            }
            if line.starts_with("deleted file mode") {
                f.is_deleted = true;
                continue;
            }
            if let Some(p) = line.strip_prefix("rename from ") {
                f.old_path = Some(p.to_string());
                continue;
            }
            if let Some(p) = line.strip_prefix("rename to ") {
                f.path = p.to_string();
                continue;
            }
            if line.starts_with("Binary files ") || line == "GIT binary patch" {
                f.status = "binary".into();
                continue;
            }
            if line.starts_with("--- ") || line.starts_with("+++ ") || line.starts_with("index ") || line.starts_with("similarity index") || line.starts_with("old mode") || line.starts_with("new mode") {
                continue;
            }
        }
        if let Some(after) = line.strip_prefix("@@ ") {
            if let Some((a, b, c, d)) = hunk_range(line) {
                // Paseo keeps git's ranges but not the function context after the second @@.
                let header = after.find(" @@").map(|i| &line[..i + 6]).unwrap_or(line);
                f.hunks.push(DiffHunk {
                    old_start: a,
                    old_count: b,
                    new_start: c,
                    new_count: d,
                    lines: vec![DiffLine { kind: "header".into(), content: header.to_string() }],
                });
            }
            continue;
        }
        let Some(h) = f.hunks.last_mut() else { continue };
        let (kind, content) = match line.chars().next() {
            Some('+') => ("add", &line[1..]),
            Some('-') => ("remove", &line[1..]),
            Some(' ') => ("context", &line[1..]),
            // "\ No newline at end of file", and the empty tail after the last line.
            _ => continue,
        };
        match kind {
            "add" => f.additions += 1,
            "remove" => f.deletions += 1,
            _ => {}
        }
        if kind != "context" {
            changed += 1;
        }
        h.lines.push(DiffLine { kind: kind.into(), content: content.to_string() });
    }
    finish(cur.take(), changed, &mut files);
    for f in &mut files {
        if f.old_path.as_deref() == Some(f.path.as_str()) || f.is_new || f.is_deleted {
            f.old_path = None;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@ fn x\n a\n-b\n+B\n c\n+d\n\\ No newline at end of file\ndiff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+new\ndiff --git a/bin.dat b/bin.dat\nnew file mode 100644\nBinary files /dev/null and b/bin.dat differ\ndiff --git a/old name.txt b/new name.txt\nsimilarity index 90%\nrename from old name.txt\nrename to new name.txt\n";

    #[test]
    fn parses_git_diff() {
        let files = parse(SAMPLE, 1000);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["bin.dat", "f.txt", "n.txt", "new name.txt"]);
        let f = &files[1];
        assert_eq!((f.additions, f.deletions), (2, 1));
        assert_eq!(f.hunks[0].lines[0].content, "@@ -1,3 +1,4 @@", "no function context (Paseo)");
        assert_eq!(f.hunks[0].lines.len(), 6);
        assert!(files[0].is_new && files[0].status == "binary" && files[0].hunks.is_empty());
        assert_eq!((files[2].hunks[0].old_start, files[2].hunks[0].old_count, files[2].hunks[0].new_count), (0, 0, 1));
        assert_eq!(files[3].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(parse(SAMPLE, 1)[1].status, "too_large");
    }
}
