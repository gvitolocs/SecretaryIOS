//! A `.env` file by plain parsing: no model reads it where the syntax is exact.
//!
//! Values are [`SecretValue`]s and stay in memory. What can be printed is a key that really has the
//! shape of a name, a line number, and a state. A line that doesn't parse is reported by its number
//! only, because that line may be the secret: a bare token, or the middle of a PEM block, whose
//! base64 can look like `KEY=` to a careless reader.

use zeroize::Zeroizing;

use crate::SecretValue;

/// The state of one assignment.
#[derive(Debug)]
pub enum EnvValue {
    /// The value, known exactly.
    Exact(SecretValue),
    /// `KEY=` with nothing.
    Empty,
    /// A value that names another variable (`$OTHER`, `${OTHER}`): not knowable from here.
    Reference,
    /// Syntax that parsers disagree on, or a bad escape.
    Unclear,
    /// A key that is never read or moved (`GOOGLE_PASSWORD`): its value isn't kept, not even here.
    Refused,
}

#[derive(Debug)]
pub struct Entry {
    pub key: String,
    pub value: EnvValue,
    /// 0-based, first and last line of this assignment.
    pub first_line: usize,
    pub last_line: usize,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// 1-based lines that are neither comments nor assignments. Numbers only.
    pub unclear_lines: Vec<usize>,
}

impl Parsed {
    /// The assignment that counts for `key`: the last one, as dotenv readers have it.
    pub fn last(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().rev().find(|e| e.key == key)
    }

    /// Each key once, in file order.
    pub fn keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.entries {
            if !out.contains(&e.key) {
                out.push(e.key.clone());
            }
        }
        out
    }
}

/// A name someone gave: not a fragment of a value. Mixed case with a digit is how base64 looks.
pub fn plain_name(key: &str) -> bool {
    if key.is_empty() || key.len() > 64 {
        return false;
    }
    let b = key.as_bytes();
    if !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return false;
    }
    if !b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')) {
        return false;
    }
    let (upper, lower, digit) = (
        b.iter().any(u8::is_ascii_uppercase),
        b.iter().any(u8::is_ascii_lowercase),
        b.iter().any(u8::is_ascii_digit),
    );
    // A long run with digits and no word breaks is a token (`JBSWY3DPEHPK3PXP=`), not a name.
    let run_with_digits = digit && key.len() >= 16 && !b.iter().any(|c| matches!(c, b'_' | b'.' | b'-'));
    !(upper && lower && digit) && !run_with_digits && crate::redact::detect(key).is_empty()
}

/// `$NAME` or `${` in text that a shell-minded reader would expand.
fn has_reference(v: &str) -> bool {
    let b = v.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'$'
            && (i == 0 || b[i - 1] != b'\\')
            && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'{' | b'_'))
    })
}

fn rest_is_blank(rest: &str) -> bool {
    let t = rest.trim_start();
    t.is_empty() || t.starts_with('#')
}

/// What a double-quoted body means. `None`: an escape readers disagree on.
fn unescape(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut it = body.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '$' => out.push('$'),
            _ => return None,
        }
    }
    Some(out)
}

/// Where the closing quote is, from the start of the text after the opening one.
fn closing(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            return Some(i);
        }
    }
    None
}

/// Parse. An opening quote that never closes ends the parse there: what follows may be the value's
/// own lines, so none of it is read as names.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Byte offset of each line's start.
    let mut starts = vec![0usize];
    for (i, c) in text.char_indices() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    let line_at = |offset: usize| starts.partition_point(|s| *s <= offset) - 1;
    let mut line = 0;
    while line < starts.len() {
        let begin = starts[line];
        let end = starts.get(line + 1).map_or(text.len(), |s| s - 1);
        let raw = text[begin..end].trim_end_matches('\r');
        let t = raw.trim_start();
        if t.is_empty() || t.starts_with('#') {
            line += 1;
            continue;
        }
        let t = t.strip_prefix("export ").map_or(t, str::trim_start);
        let Some((key, rest)) = t.split_once('=') else {
            out.unclear_lines.push(line + 1);
            line += 1;
            continue;
        };
        let key = key.trim_end();
        if !plain_name(key) || rest.starts_with('=') {
            out.unclear_lines.push(line + 1);
            line += 1;
            continue;
        }
        let rest = rest.trim_start_matches([' ', '\t']);
        let first_line = line;
        let mut last_line = line;
        let value = if let Some(q) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) {
            // The quote's own offset in the whole text: a quoted value may run over lines.
            let open = rest.as_ptr() as usize - text.as_ptr() as usize;
            let body = &text[open + 1..];
            let Some(close) = closing(body, q) else {
                out.unclear_lines.push(line + 1);
                break;
            };
            let inner = &body[..close];
            let after_close = open + 1 + close + 1;
            last_line = line_at(open + 1 + close);
            let line_end = starts.get(last_line + 1).map_or(text.len(), |s| s - 1);
            let trailing = text.get(after_close..line_end).unwrap_or("");
            if !rest_is_blank(trailing.trim_end_matches('\r')) {
                EnvValue::Unclear
            } else if q == '\'' {
                if inner.is_empty() {
                    EnvValue::Empty
                } else {
                    EnvValue::Exact(SecretValue::new(inner.to_owned()))
                }
            } else if has_reference(inner) {
                EnvValue::Reference
            } else {
                match unescape(inner) {
                    Some(v) if v.is_empty() => EnvValue::Empty,
                    Some(v) => EnvValue::Exact(SecretValue::new(v)),
                    None => EnvValue::Unclear,
                }
            }
        } else if rest.starts_with('`') {
            EnvValue::Unclear
        } else {
            // Unquoted: a comment starts at whitespace and `#`; a `#` inside a word is the value.
            let mut v = rest;
            let b = rest.as_bytes();
            for i in 0..b.len() {
                if b[i] == b'#' && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') {
                    v = &rest[..i];
                    break;
                }
            }
            let v = v.trim_end();
            if v.is_empty() {
                EnvValue::Empty
            } else if has_reference(v) {
                EnvValue::Reference
            } else {
                EnvValue::Exact(SecretValue::new(v.to_owned()))
            }
        };
        let value = if crate::source_path::key_refused(key.as_bytes()) { EnvValue::Refused } else { value };
        out.entries.push(Entry { key: key.to_owned(), value, first_line, last_line });
        line = last_line + 1;
    }
    out
}

/// Words that only say a value belongs here.
const PLACEHOLDERS: &[&str] = &[
    "changeme", "change_me", "change-me", "replaceme", "replace_me", "replace-me", "todo", "tbd", "fixme", "none", "null", "nil",
    "undefined", "example", "sample", "placeholder", "secret", "password", "token", "apikey", "api_key", "api-key", "xxx", "test",
    "dummy", "fake", "true", "false",
];

/// `KEY=changeme`, `<your-key>`, `your_api_key_here`, `xxxxxx`: no real value yet.
pub fn is_placeholder(v: &str) -> bool {
    let t = v.trim();
    let lower = t.to_lowercase();
    PLACEHOLDERS.contains(&lower.as_str())
        || (t.starts_with('<') && t.ends_with('>'))
        || lower.starts_with("your_")
        || lower.starts_with("your-")
        || lower.starts_with("your ")
        || lower.starts_with("replace_")
        || lower.starts_with("replace-")
        || lower.ends_with("_here")
        || lower.ends_with("-here")
        || (t.len() >= 3 && t.chars().all(|c| c == 'x' || c == 'X' || c == '*'))
}

/// By its name or its shape, this looks like a secret and not like configuration (`PORT=8080`).
pub fn looks_secret(key: &str, value: Option<&SecretValue>) -> bool {
    let upper = key.to_uppercase();
    let words: Vec<&str> = upper.split(['_', '-', '.']).filter(|w| !w.is_empty()).collect();
    if words.contains(&"PUBLIC") || words.contains(&"PUB") {
        return false;
    }
    const WORDS: &[&str] = &[
        "SECRET", "SECRETS", "TOKEN", "TOKENS", "PASSWORD", "PASSWD", "PASS", "PWD", "PASSPHRASE", "APIKEY", "AUTH", "CREDENTIAL", "CREDENTIALS",
        "PRIVATE", "DSN", "COOKIE", "SALT", "BEARER", "JWT", "SIGNATURE", "SIGNING", "KEY", "PSK",
    ];
    if words.iter().any(|w| WORDS.contains(w) || (w.len() > 3 && (w.ends_with("KEY") || w.ends_with("TOKEN") || w.ends_with("SECRET")))) {
        return true;
    }
    let Some(value) = value else { return false };
    let v = value.expose_secret();
    // A URL with a login in it (DATABASE_URL), or a known token shape.
    if let Some((_, after)) = v.split_once("://") {
        let host_part = after.split('/').next().unwrap_or("");
        if host_part.contains('@') {
            return true;
        }
    }
    !crate::redact::detect(v).is_empty()
}

/// The file's text with `keys` taken out: each assignment's lines become one comment naming where it
/// went. Everything else stays byte for byte.
pub fn without_keys(text: &str, moved: &[(String, String)]) -> Zeroizing<String> {
    let parsed = parse(text);
    let bom = text.starts_with('\u{feff}');
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(body.len());
    if bom {
        out.push('\u{feff}');
    }
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let hit = parsed.entries.iter().find(|e| e.first_line == i).filter(|e| moved.iter().any(|(k, _)| *k == e.key));
        if let Some(e) = hit {
            let (_, reference) = moved.iter().find(|(k, _)| *k == e.key).expect("just found");
            out.push_str(&format!("# {} moved to Infisical: {reference}\n", e.key));
            i = e.last_line + 1;
        } else {
            out.push_str(lines[i]);
            i += 1;
        }
    }
    Zeroizing::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY: &str = "canary-envfile-9f3kQ";

    fn exact(p: &Parsed, key: &str) -> String {
        match &p.last(key).unwrap_or_else(|| panic!("no {key}")).value {
            EnvValue::Exact(v) => v.expose_secret().to_owned(),
            other => panic!("{key}: {other:?}"),
        }
    }

    #[test]
    fn the_forms_dotenv_readers_agree_on() {
        let p = parse(
            "# a comment\n\nA=one\nexport B=two\nC = three\nD=\"four five\"\nE='six $NOT #here'\nF=seven # note\nG=has#hash\n\
             H=\"line\\nbreak\"\nI=\"quote\\\"d\" # note\nJ=\nK=\"\"\n\tL=tabbed\r\nM=url://u:p@h/db?a=b&c=d\n",
        );
        assert_eq!(exact(&p, "A"), "one");
        assert_eq!(exact(&p, "B"), "two");
        assert_eq!(exact(&p, "C"), "three");
        assert_eq!(exact(&p, "D"), "four five");
        assert_eq!(exact(&p, "E"), "six $NOT #here");
        assert_eq!(exact(&p, "F"), "seven");
        assert_eq!(exact(&p, "G"), "has#hash");
        assert_eq!(exact(&p, "H"), "line\nbreak");
        assert_eq!(exact(&p, "I"), "quote\"d");
        assert!(matches!(p.last("J").unwrap().value, EnvValue::Empty));
        assert!(matches!(p.last("K").unwrap().value, EnvValue::Empty));
        assert_eq!(exact(&p, "L"), "tabbed");
        assert_eq!(exact(&p, "M"), "url://u:p@h/db?a=b&c=d");
        assert!(p.unclear_lines.is_empty(), "{:?}", p.unclear_lines);
    }

    #[test]
    fn a_quoted_value_over_lines_and_the_key_after_it() {
        let text = "KEY=\"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nAb3=\n-----END PRIVATE KEY-----\"\nNEXT=ok\n";
        let p = parse(text);
        assert_eq!(p.keys(), vec!["KEY", "NEXT"]);
        assert!(exact(&p, "KEY").contains("MIIEvQ"));
        assert_eq!(exact(&p, "NEXT"), "ok");
        assert_eq!(p.last("KEY").unwrap().last_line, 3);
    }

    #[test]
    fn a_line_that_may_be_the_secret_is_only_a_number() {
        let p = parse(&format!(
            "OK=1\n{CANARY}\nsk-abc123def456ghi789jkl012mno345pq=\nMIIEvQIBADANBgkqhkiG9w0=\nAb3=\nKey1=x\nA==b\nJBSWY3DPEHPK3PXP=\n"
        ));
        assert_eq!(p.keys(), vec!["OK"]);
        assert_eq!(p.unclear_lines, vec![2, 3, 4, 5, 6, 7, 8]);
        assert!(!format!("{p:?}").contains(CANARY), "Debug shows no value");
    }

    #[test]
    fn a_quote_that_never_closes_stops_the_parse() {
        let p = parse("A=1\nKEY=\"-----BEGIN\nabcdEF=\nB=2\n");
        assert_eq!(p.keys(), vec!["A"]);
        assert_eq!(p.unclear_lines, vec![2]);
    }

    #[test]
    fn references_and_unclear_are_not_values() {
        let p = parse("A=$OTHER\nB=${OTHER}x\nC=\"pre$OTHER\"\nD=\"bad\\q\"\nE=\"ok\"trailing\nF=`cmd`\nG='lit$OTHER'\nH=\"esc\\$OTHER\"\nI=cost$5\n");
        assert!(matches!(p.last("A").unwrap().value, EnvValue::Reference));
        assert!(matches!(p.last("B").unwrap().value, EnvValue::Reference));
        assert!(matches!(p.last("C").unwrap().value, EnvValue::Reference));
        assert!(matches!(p.last("D").unwrap().value, EnvValue::Unclear));
        assert!(matches!(p.last("E").unwrap().value, EnvValue::Unclear));
        assert!(matches!(p.last("F").unwrap().value, EnvValue::Unclear));
        assert_eq!(exact(&p, "G"), "lit$OTHER");
        assert_eq!(exact(&p, "H"), "esc$OTHER");
        assert_eq!(exact(&p, "I"), "cost$5");
    }

    #[test]
    fn the_last_assignment_counts() {
        let p = parse("A=one\nA=two\n");
        assert_eq!(exact(&p, "A"), "two");
        assert_eq!(p.keys(), vec!["A"]);
    }

    #[test]
    fn secret_or_config() {
        let v = |s: &str| SecretValue::new(s.to_owned());
        for k in ["API_KEY", "GITHUB_TOKEN", "DB_PASSWORD", "SESSION_SECRET", "OPENAI_APIKEY", "SENTRY_DSN", "client-secret", "webhookSecret", "PRIVATE_KEY"] {
            assert!(looks_secret(k, Some(&v("x"))), "{k}");
        }
        for k in ["PORT", "DEBUG", "NODE_ENV", "NEXT_PUBLIC_API_KEY", "PUBLIC_KEY", "HOST", "MAX_RETRIES"] {
            assert!(!looks_secret(k, Some(&v("8080"))), "{k}");
        }
        assert!(looks_secret("DATABASE_URL", Some(&v("postgres://u:p@h/db"))));
        assert!(!looks_secret("DATABASE_URL", Some(&v("postgres://h/db"))));
        assert!(looks_secret("SOMETHING", Some(&v("ghp_abcdefghijklmnopqrstuvwxyz0123456789"))));
    }

    #[test]
    fn placeholders() {
        for v in ["changeme", "CHANGE_ME", "<your-key>", "your_api_key_here", "xxxxxx", "***", "secret", "TODO", "true"] {
            assert!(is_placeholder(v), "{v}");
        }
        for v in ["hunter2hunter2", "postgres://u:p@h/db", "abcxyz"] {
            assert!(!is_placeholder(v), "{v}");
        }
    }

    #[test]
    fn taking_keys_out_leaves_every_other_byte() {
        let text = "# top\nA=1 # keep\nSECRET=\"multi\nline\"\n\nexport B=2\nSECRET=again\nC='3'";
        let out = without_keys(text, &[("SECRET".to_owned(), "secret://p/e#K".to_owned())]);
        assert_eq!(
            &*out,
            "# top\nA=1 # keep\n# SECRET moved to Infisical: secret://p/e#K\n\nexport B=2\n# SECRET moved to Infisical: secret://p/e#K\nC='3'"
        );
    }
}
