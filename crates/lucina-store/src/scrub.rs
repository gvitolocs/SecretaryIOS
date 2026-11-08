//! Take a secret out of what is stored: after Lucina saved it in Infisical (read back and compared),
//! the value goes from JSON, JSON lines and text files, replaced by its placeholder.
//!
//! JSON is walked, not searched as text: only string **values** change, so a short value can't break
//! a line by hitting a key. A file is rewritten only when something changed, through a temporary and
//! a rename, keeping its mode. That also breaks a hard link, on purpose: a backup snapshot shares its
//! inode with the days around it, and each path is scrubbed by itself.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde_json::Value;

/// `s` with every needle replaced, when one was there.
pub fn scrub_str(s: &str, needles: &[String], replacement: &str) -> Option<String> {
    if !needles
        .iter()
        .any(|n| !n.is_empty() && s.contains(n.as_str()))
    {
        return None;
    }
    let mut out = s.to_owned();
    for n in needles.iter().filter(|n| !n.is_empty()) {
        if out.contains(n.as_str()) {
            out = out.replace(n.as_str(), replacement);
        }
    }
    Some(out)
}

/// Every string value in `v` (never a key). True when something changed.
pub fn scrub_value(v: &mut Value, needles: &[String], replacement: &str) -> bool {
    match v {
        Value::String(s) => match scrub_str(s, needles, replacement) {
            Some(new) => {
                *s = new;
                true
            }
            None => false,
        },
        Value::Array(a) => {
            a.iter_mut()
                .map(|x| scrub_value(x, needles, replacement))
                .filter(|c| *c)
                .count()
                > 0
        }
        Value::Object(o) => {
            o.values_mut()
                .map(|x| scrub_value(x, needles, replacement))
                .filter(|c| *c)
                .count()
                > 0
        }
        _ => false,
    }
}

/// One JSON line with the needles out of its values; `None` when it held none or isn't JSON.
pub fn scrub_json_line(line: &str, needles: &[String], replacement: &str) -> Option<String> {
    if !needles
        .iter()
        .any(|n| !n.is_empty() && line.contains(n.as_str()))
    {
        return None;
    }
    let mut v: Value = serde_json::from_str(line).ok()?;
    scrub_value(&mut v, needles, replacement)
        .then(|| serde_json::to_string(&v).ok())
        .flatten()
}

/// Write `bytes` over `path` through a temporary with the same mode (a new inode).
pub fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    let mut rnd = [0u8; 6];
    let _ = getrandom::fill(&mut rnd);
    let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = path.with_file_name(format!(".{name}.{suffix}.scrub"));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `.json`, `.jsonl`, `.md`, `.txt` and `.log` files, up to this size. Anything else is not read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn kind_of(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "json" => Some("json"),
        "jsonl" => Some("jsonl"),
        "md" | "txt" | "log" => Some("text"),
        _ => None,
    }
}

/// Scrub one file. `Ok(true)`: it held a needle and was rewritten.
pub fn scrub_file(path: &Path, needles: &[String], replacement: &str) -> std::io::Result<bool> {
    let Some(kind) = kind_of(path) else {
        return Ok(false);
    };
    let meta = fs::metadata(path)?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Ok(false);
    }
    let bytes = fs::read(path)?;
    // Not UTF-8: not rewritten by this (never a lossy copy of someone's file).
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(false);
    };
    if !needles
        .iter()
        .any(|n| !n.is_empty() && text.contains(n.as_str()))
    {
        return Ok(false);
    }
    let new = match kind {
        "json" => match serde_json::from_str::<Value>(text) {
            Ok(mut v) => {
                if !scrub_value(&mut v, needles, replacement) {
                    return Ok(false);
                }
                let mut s = serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?;
                s.push('\n');
                s
            }
            Err(_) => scrub_str(text, needles, replacement).unwrap_or_default(),
        },
        "jsonl" => {
            let mut out = String::with_capacity(text.len());
            let mut changed = false;
            for line in text.split_inclusive('\n') {
                let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
                match scrub_json_line(body, needles, replacement)
                    .or_else(|| scrub_str(body, needles, replacement))
                {
                    Some(new) => {
                        changed = true;
                        out.push_str(&new);
                    }
                    None => out.push_str(body),
                }
                out.push_str(end);
            }
            if !changed {
                return Ok(false);
            }
            out
        }
        _ => scrub_str(text, needles, replacement).unwrap_or_default(),
    };
    replace_file(path, new.as_bytes())?;
    Ok(true)
}

/// Scrub a directory tree: how many files changed. Symlinks are not followed.
pub fn scrub_tree(root: &Path, needles: &[String], replacement: &str) -> std::io::Result<usize> {
    let mut changed = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && scrub_file(&entry.path(), needles, replacement)? {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V: &str = "canary-scrub-81f0";
    const P: &str = "[secret:X]";

    fn needles() -> Vec<String> {
        vec![V.to_owned()]
    }

    #[test]
    fn a_value_that_names_a_key_leaves_the_key() {
        let mut v = json!({"text": "the text is here", "other": ["text", 1]});
        assert!(scrub_value(&mut v, &["text".to_owned()], P));
        assert_eq!(
            v,
            json!({"text": "the [secret:X] is here", "other": [P, 1]})
        );
    }

    #[test]
    fn json_lines_keep_every_other_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        fs::write(
            &p,
            format!("{{\"a\":\"x\"}}\n{{\"a\":\"has {V}\"}}\nnot json {V}\n{{\"a\":\"y\"}}"),
        )
        .unwrap();
        assert!(scrub_file(&p, &needles(), P).unwrap());
        let got = fs::read_to_string(&p).unwrap();
        assert!(!got.contains(V));
        assert_eq!(got.lines().count(), 4);
        assert!(got.starts_with("{\"a\":\"x\"}\n"));
        assert!(got.ends_with("{\"a\":\"y\"}"));
    }

    #[test]
    fn a_file_without_the_value_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.json");
        fs::write(&p, "{ \"a\" : 1 }").unwrap();
        let ino = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&p).unwrap());
        assert!(!scrub_file(&p, &needles(), P).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), "{ \"a\" : 1 }");
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&p).unwrap()),
            ino
        );
    }

    #[test]
    fn hard_linked_snapshots_each_get_clean() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("day1")).unwrap();
        fs::create_dir_all(dir.path().join("day2")).unwrap();
        let (a, b) = (
            dir.path().join("day1/n.json"),
            dir.path().join("day2/n.json"),
        );
        fs::write(&a, format!("{{\"content\":\"{V}\"}}")).unwrap();
        fs::hard_link(&a, &b).unwrap();
        assert_eq!(scrub_tree(dir.path(), &needles(), P).unwrap(), 2);
        assert!(!fs::read_to_string(&a).unwrap().contains(V));
        assert!(!fs::read_to_string(&b).unwrap().contains(V));
    }

    #[test]
    fn keeps_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.txt");
        fs::write(&p, V).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(scrub_file(&p, &needles(), P).unwrap());
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), P);
    }
}
