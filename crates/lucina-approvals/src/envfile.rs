//! Reading lucina-rs's systemd `EnvironmentFile`s (`KEY=value`, `#` comments,
//! optional single or double quotes) so `lucina-root` uses the same server
//! URL, service token path and pinned key as the service. Values are never
//! printed.

use std::collections::HashMap;
use std::path::Path;

/// Parse one file's text.
pub fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let v = v.trim();
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        out.insert(k.to_owned(), v.to_owned());
    }
    out
}

/// Several files, later ones winning (like repeated `EnvironmentFile=` lines).
/// Missing or unreadable files are skipped.
pub fn load(files: &[&Path]) -> HashMap<String, String> {
    let mut all = HashMap::new();
    for f in files {
        if let Ok(t) = std::fs::read_to_string(f) {
            all.extend(parse(&t));
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_style_lines() {
        let m = parse("# c\nA=1\nexport B = \"two words\"\nC='x'\n\nbad line\nD=\n=x\n");
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "two words");
        assert_eq!(m["C"], "x");
        assert_eq!(m["D"], "");
        assert_eq!(m.len(), 4);
    }
}
