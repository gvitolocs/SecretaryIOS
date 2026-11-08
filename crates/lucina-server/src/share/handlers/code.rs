//! Source files: language from the name, line counts, top-level symbols and
//! imports found with per-language regexes. The code itself is the text.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use regex::Regex;
use serde_json::{json, Value};

use super::Extracted;

/// Bytes read; the rest is dropped with a warning.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SYMBOLS: usize = 200;
const MAX_IMPORTS: usize = 50;

/// The language of a file name (`main.rs` → `rust`, `Dockerfile` → `dockerfile`).
pub fn language(name: &str) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") || lower == "containerfile" {
        return Some("dockerfile");
    }
    if matches!(lower.as_str(), "makefile" | "gnumakefile" | "bsdmakefile") {
        return Some("makefile");
    }
    let (_, ext) = lower.rsplit_once('.')?;
    Some(match ext {
        "rs" => "rust",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" | "tsx" => "typescript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "lua" => "lua",
        "dart" => "dart",
        "scala" | "sc" => "scala",
        "r" => "r",
        "m" => "objective-c",
        "mm" => "objective-c++",
        "pl" | "pm" => "perl",
        "ex" | "exs" => "elixir",
        "hs" => "haskell",
        "vue" => "vue",
        "svelte" => "svelte",
        "dockerfile" => "dockerfile",
        "mk" | "makefile" => "makefile",
        "gradle" => "gradle",
        "xml" => "xml",
        _ => return None,
    })
}

/// Reads a source file: counts, symbols, imports and the code itself.
pub fn read_code(path: &Path, name: &str) -> anyhow::Result<Extracted> {
    let lang = language(name).unwrap_or("text");
    let file = std::fs::File::open(path).context("cannot open the file")?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("cannot read the file")?;
    if bytes.contains(&0) {
        bail!("the file looks binary (NUL bytes), not source code");
    }
    let mut warnings = Vec::new();
    if bytes.len() as u64 > MAX_BYTES {
        bytes.truncate(MAX_BYTES as usize);
        // Do not leave half a UTF-8 sequence at the cut.
        while bytes.last().is_some_and(|b| b & 0xC0 == 0x80) {
            bytes.pop();
        }
        if bytes.last().is_some_and(|b| *b >= 0xC0) {
            bytes.pop();
        }
        warnings.push(format!(
            "Only the first {} MB were read.",
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            warnings.push("The file is not valid UTF-8; invalid bytes were replaced.".into());
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        }
    };
    let text = text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text);

    let lower = name.to_ascii_lowercase();
    let jsx = lower.ends_with(".tsx") || lower.ends_with(".jsx");
    let (lines, blank, comments) = count_lines(&text, lang);
    let symbols = find_symbols(&text, lang, jsx, name);
    let imports = find_imports(&text, lang);

    let mut out = Extracted::new("code");
    out.subtype = Some(lang.to_string());
    out.title = Some(name.to_string());
    let mut body = format!(
        "{} source, {lines} lines, {} symbols\n",
        display_lang(lang),
        symbols.len()
    );
    if !symbols.is_empty() {
        body.push_str("Symbols:\n");
        for s in &symbols {
            body.push_str(&format!("  L{} {} {}\n", s.line, s.kind, s.name));
        }
    }
    if !imports.is_empty() {
        body.push_str(&format!("Imports: {}\n", imports.join(", ")));
    }
    body.push_str(&format!("\n```{lang}\n{}\n```", text.trim_end()));
    out.text = body;
    out.structured = json!({
        "language": lang,
        "lines": lines,
        "blank_lines": blank,
        "comment_lines": comments,
        "symbols": symbols.iter().map(|s| json!({"kind": s.kind, "name": s.name, "line": s.line})).collect::<Vec<Value>>(),
        "imports": imports,
    });
    out.warnings = warnings;
    Ok(out)
}

fn display_lang(lang: &str) -> String {
    match lang {
        "javascript" => "JavaScript".into(),
        "typescript" => "TypeScript".into(),
        "csharp" => "C#".into(),
        "cpp" => "C++".into(),
        "php" | "sql" | "json" | "yaml" | "toml" | "html" | "css" | "scss" | "xml" => {
            lang.to_ascii_uppercase()
        }
        _ => {
            let mut c = lang.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

/// Comment markers: (line prefixes, block open/close pairs).
fn comment_style(
    lang: &str,
) -> (
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
) {
    const C_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
    const HTML_BLOCK: &[(&str, &str)] = &[("<!--", "-->")];
    match lang {
        "rust" | "swift" | "kotlin" | "javascript" | "typescript" | "go" | "c" | "cpp" | "java"
        | "csharp" | "dart" | "scala" | "objective-c" | "objective-c++" | "scss" | "gradle" => {
            (&["//"], C_BLOCK)
        }
        "css" => (&[], C_BLOCK),
        "php" => (&["//", "#"], C_BLOCK),
        "vue" | "svelte" => (&["//"], &[("/*", "*/"), ("<!--", "-->")]),
        "python" | "ruby" | "shell" | "perl" | "r" | "yaml" | "toml" | "elixir" | "dockerfile"
        | "makefile" => (&["#"], &[]),
        "sql" => (&["--"], C_BLOCK),
        "lua" => (&["--"], &[("--[[", "]]")]),
        "haskell" => (&["--"], &[("{-", "-}")]),
        "html" | "xml" => (&[], HTML_BLOCK),
        _ => (&[], &[]),
    }
}

/// (lines, blank lines, comment lines).
fn count_lines(text: &str, lang: &str) -> (usize, usize, usize) {
    let (prefixes, blocks) = comment_style(lang);
    let (mut lines, mut blank, mut comments) = (0, 0, 0);
    let mut open: Option<&str> = None;
    for line in text.lines() {
        lines += 1;
        let t = line.trim();
        if let Some(close) = open {
            comments += 1;
            if t.contains(close) {
                open = None;
            }
            continue;
        }
        if t.is_empty() {
            blank += 1;
            continue;
        }
        if let Some(&(o, c)) = blocks.iter().find(|(o, _)| t.starts_with(o)) {
            comments += 1;
            if !t[o.len()..].contains(c) {
                open = Some(c);
            }
            continue;
        }
        if prefixes.iter().any(|p| t.starts_with(p)) {
            comments += 1;
        }
    }
    (lines, blank, comments)
}

struct Symbol {
    kind: &'static str,
    name: String,
    line: usize,
}

/// Up to four spaces or one tab: top level and one nesting level.
const IND: &str = r"^(?:\t|[ ]{0,4})";

/// (kind, pattern with the name in group 1), tried in order per line.
fn symbol_rules(lang: &str) -> Vec<(&'static str, String)> {
    let rules: Vec<(&str, &str)> = match lang {
        "rust" => vec![
            (
                "fn",
                r#"(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:"[^"]*"\s+)?)?fn\s+([A-Za-z_]\w*)"#,
            ),
            (
                "struct",
                r"(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_]\w*)",
            ),
            ("enum", r"(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_]\w*)"),
            (
                "trait",
                r"(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:auto\s+)?trait\s+([A-Za-z_]\w*)",
            ),
            ("type", r"(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_]\w*)"),
            (
                "const",
                r"(?:pub(?:\([^)]*\))?\s+)?(?:const|static(?:\s+mut)?)\s+([A-Za-z_]\w*)\s*:",
            ),
            (
                "impl",
                r"(?:unsafe\s+)?impl\b(?:\s*<[^{]*?>)?\s+([^{]+?)\s*(?:\{|\bwhere\b|$)",
            ),
        ],
        "swift" => vec![
            (
                "func",
                r"(?:(?:@\w+(?:\([^)]*\))?|public|private|fileprivate|internal|open|final|static|class|override|mutating|nonmutating|nonisolated|required|convenience|dynamic)\s+)*func\s+([A-Za-z_]\w*|[^\s(]+)",
            ),
            (
                "class",
                r"(?:(?:@\w+(?:\([^)]*\))?|public|private|fileprivate|internal|open|final)\s+)*(?:class|actor)\s+([A-Za-z_]\w*)",
            ),
            (
                "struct",
                r"(?:(?:@\w+(?:\([^)]*\))?|public|private|fileprivate|internal)\s+)*struct\s+([A-Za-z_]\w*)",
            ),
            (
                "enum",
                r"(?:(?:@\w+(?:\([^)]*\))?|public|private|fileprivate|internal|indirect)\s+)*enum\s+([A-Za-z_]\w*)",
            ),
            (
                "interface",
                r"(?:(?:@\w+(?:\([^)]*\))?|public|private|fileprivate|internal)\s+)*protocol\s+([A-Za-z_]\w*)",
            ),
            (
                "impl",
                r"(?:(?:public|private|fileprivate|internal)\s+)*extension\s+([A-Za-z_][\w.]*)",
            ),
            (
                "type",
                r"(?:(?:public|private|fileprivate|internal)\s+)*typealias\s+([A-Za-z_]\w*)",
            ),
        ],
        "kotlin" => vec![
            (
                "fn",
                r"(?:(?:public|private|protected|internal|override|open|abstract|suspend|inline|operator|infix|tailrec|external)\s+)*fun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)",
            ),
            (
                "enum",
                r"(?:(?:public|private|protected|internal)\s+)*enum\s+class\s+([A-Za-z_]\w*)",
            ),
            (
                "interface",
                r"(?:(?:public|private|protected|internal|sealed|fun)\s+)*interface\s+([A-Za-z_]\w*)",
            ),
            (
                "class",
                r"(?:(?:public|private|protected|internal|open|abstract|sealed|data|inner|value|annotation)\s+)*(?:class|object)\s+([A-Za-z_]\w*)",
            ),
            (
                "type",
                r"(?:(?:public|private|internal)\s+)*typealias\s+([A-Za-z_]\w*)",
            ),
            (
                "const",
                r"(?:(?:public|private|internal)\s+)*const\s+val\s+([A-Za-z_]\w*)",
            ),
        ],
        "python" => vec![
            ("def", r"(?:async\s+)?def\s+([A-Za-z_]\w*)"),
            ("class", r"class\s+([A-Za-z_]\w*)"),
            ("const", r"^([A-Z][A-Z0-9_]*)\s*(?::[^=]+)?=[^=]"),
        ],
        "javascript" | "typescript" | "vue" | "svelte" => vec![
            (
                "fn",
                r"(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)",
            ),
            (
                "class",
                r"(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)",
            ),
            (
                "interface",
                r"(?:export\s+)?(?:declare\s+)?interface\s+([A-Za-z_$][\w$]*)",
            ),
            (
                "type",
                r"(?:export\s+)?(?:declare\s+)?type\s+([A-Za-z_$][\w$]*)\s*(?:<[^=]*>)?\s*=",
            ),
            (
                "enum",
                r"(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+([A-Za-z_$][\w$]*)",
            ),
            (
                "fn",
                r"(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]+)?=>|[A-Za-z_$][\w$]*\s*=>)",
            ),
            ("const", r"^export\s+const\s+([A-Za-z_$][\w$]*)"),
            ("const", r"^const\s+([A-Z][A-Z0-9_]*)\s*(?::[^=]+)?="),
        ],
        "go" => vec![
            ("func", r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)"),
            ("struct", r"^(?:type\s+|\t)([A-Za-z_]\w*)\s+struct\b"),
            ("interface", r"^(?:type\s+|\t)([A-Za-z_]\w*)\s+interface\b"),
            ("type", r"^type\s+([A-Za-z_]\w*)\s+[^(\s]"),
            ("const", r"^const\s+([A-Za-z_]\w*)"),
        ],
        "c" | "cpp" | "objective-c" | "objective-c++" => vec![
            (
                "struct",
                r"^(?:typedef\s+)?struct\s+([A-Za-z_]\w*)\s*\{?\s*$",
            ),
            (
                "class",
                r"^(?:template\s*<[^>]*>\s*)?class\s+([A-Za-z_]\w*)[^;]*$",
            ),
            (
                "enum",
                r"^(?:typedef\s+)?enum\s+(?:class\s+)?([A-Za-z_]\w*)[^;]*$",
            ),
            ("interface", r"^@(?:interface|protocol)\s+([A-Za-z_]\w*)"),
            ("impl", r"^@implementation\s+([A-Za-z_]\w*)"),
            ("const", r"^#define\s+([A-Za-z_]\w*)"),
            (
                "fn",
                r"^(?:[A-Za-z_][\w:<>,*&\s]*?[\s*&])([A-Za-z_][\w:~]*)\s*\([^;]*$",
            ),
        ],
        "java" | "csharp" | "dart" | "scala" => vec![
            (
                "interface",
                r"(?:(?:public|private|protected|internal|abstract|static|sealed|partial)\s+)*interface\s+([A-Za-z_]\w*)",
            ),
            (
                "enum",
                r"(?:(?:public|private|protected|internal|static)\s+)*enum\s+(?:class\s+)?([A-Za-z_]\w*)",
            ),
            (
                "struct",
                r"(?:(?:public|private|protected|internal|readonly|ref|partial)\s+)*struct\s+([A-Za-z_]\w*)",
            ),
            (
                "trait",
                r"(?:(?:sealed|private)\s+)*(?:trait|mixin)\s+([A-Za-z_]\w*)",
            ),
            ("impl", r"extension\s+([A-Za-z_]\w*)\s+on\b"),
            (
                "class",
                r"(?:(?:public|private|protected|internal|abstract|final|static|sealed|partial|data|case|implicit)\s+)*(?:class|record|object)\s+([A-Za-z_]\w*)",
            ),
            (
                "def",
                r"(?:(?:override|private|protected|final|implicit)\s+)*def\s+([A-Za-z_]\w*)",
            ),
            ("type", r"^(?:typedef|type)\s+([A-Za-z_]\w*)"),
        ],
        "ruby" => vec![
            ("def", r"def\s+(?:self\.)?([A-Za-z_]\w*[?!=]?)"),
            ("class", r"(?:class|module)\s+([A-Z][\w:]*)"),
        ],
        "php" => vec![
            (
                "fn",
                r"(?:(?:public|private|protected|static|abstract|final)\s+)*function\s+&?([A-Za-z_]\w*)",
            ),
            ("interface", r"interface\s+([A-Za-z_]\w*)"),
            ("trait", r"trait\s+([A-Za-z_]\w*)"),
            ("enum", r"enum\s+([A-Za-z_]\w*)"),
            (
                "class",
                r"(?:(?:abstract|final|readonly)\s+)*class\s+([A-Za-z_]\w*)",
            ),
        ],
        "shell" => vec![
            ("fn", r"^function\s+([A-Za-z_][\w-]*)"),
            ("fn", r"^([A-Za-z_][\w-]*)\s*\(\)"),
        ],
        "lua" => vec![("fn", r"^(?:local\s+)?function\s+([A-Za-z_][\w.:]*)")],
        "perl" => vec![
            ("fn", r"^sub\s+([A-Za-z_]\w*)"),
            ("class", r"^package\s+([A-Za-z_][\w:]*)"),
        ],
        "r" => vec![("fn", r"^([A-Za-z_.][\w.]*)\s*(?:<-|=)\s*function\b")],
        "elixir" => vec![
            ("class", r"defmodule\s+([A-Z][\w.]*)"),
            ("def", r"defp?\s+([a-z_]\w*[?!]?)"),
        ],
        "haskell" => vec![
            ("type", r"^(?:data|newtype|type)\s+([A-Z]\w*)"),
            ("trait", r"^class\s+(?:\([^)]*\)\s*=>\s*)?([A-Z]\w*)"),
            ("impl", r"^instance\s+(.+?)\s+where"),
            ("fn", r"^([a-z_][\w']*)\s*::"),
        ],
        "sql" => vec![
            (
                "type",
                r"(?i)^create\s+(?:or\s+replace\s+)?(?:temp(?:orary)?\s+)?(?:table|view|materialized\s+view|type)\s+(?:if\s+not\s+exists\s+)?([\w.`\x22\[\]]+)",
            ),
            (
                "fn",
                r"(?i)^create\s+(?:or\s+replace\s+)?(?:function|procedure|trigger)\s+([\w.`\x22\[\]]+)",
            ),
        ],
        "scss" => vec![
            ("fn", r"^@(?:mixin|function)\s+([\w-]+)"),
            ("const", r"^\$([\w-]+)\s*:"),
        ],
        "makefile" => vec![("fn", r"^([A-Za-z0-9_.\-/%]+)\s*:([^=]|$)")],
        _ => vec![],
    };
    rules
        .into_iter()
        .map(|(kind, pat)| {
            let pat = if pat.starts_with('^') || pat.starts_with("(?i)^") {
                pat.to_string()
            } else {
                format!("{IND}{pat}")
            };
            (kind, pat)
        })
        .collect()
}

/// Words a loose pattern can catch that are never definitions.
const NOT_NAMES: &[&str] = &[
    "if", "for", "while", "switch", "return", "else", "do", "catch", "sizeof", "func", "var",
    "let", "new", "delete", "case", "default", "when", "match", "where", "in", "of", "true",
    "false", "null",
];

fn compile(rules: Vec<(&'static str, String)>) -> Vec<(&'static str, Regex)> {
    rules
        .into_iter()
        .filter_map(|(k, p)| Regex::new(&p).ok().map(|r| (k, r)))
        .collect()
}

fn find_symbols(text: &str, lang: &str, jsx: bool, name: &str) -> Vec<Symbol> {
    let rules = compile(symbol_rules(lang));
    let mut out = Vec::new();
    if matches!(lang, "vue" | "svelte") {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
        out.push(Symbol {
            kind: "component",
            name: stem.to_string(),
            line: 1,
        });
    }
    if rules.is_empty() {
        return out;
    }
    let mut in_block_comment = false;
    for (i, line) in text.lines().enumerate() {
        if out.len() >= MAX_SYMBOLS {
            break;
        }
        let t = line.trim_start();
        if in_block_comment {
            if t.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if t.starts_with("/*") && !t.contains("*/") {
            in_block_comment = true;
            continue;
        }
        let c_like = matches!(lang, "c" | "cpp" | "objective-c" | "objective-c++");
        if t.starts_with("//") || (t.starts_with('#') && !c_like) {
            continue;
        }
        for (kind, re) in &rules {
            let Some(name) = re
                .captures(line)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim())
            else {
                continue;
            };
            if name.is_empty() || NOT_NAMES.contains(&name) {
                continue;
            }
            let kind = if jsx && *kind == "fn" && name.starts_with(|c: char| c.is_ascii_uppercase())
            {
                "component"
            } else {
                *kind
            };
            let mut name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            if name.chars().count() > 120 {
                name = name.chars().take(120).collect();
            }
            out.push(Symbol {
                kind,
                name,
                line: i + 1,
            });
            break;
        }
    }
    out
}

fn import_rules(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &[
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);",
            r"^\s*extern\s+crate\s+(\w+)",
            r"^\s*(?:pub\s+)?mod\s+(\w+)\s*;",
        ],
        "swift" => &[r"^\s*(?:@\w+\s+)?import\s+(?:class\s+|struct\s+|func\s+|enum\s+)?([\w.]+)"],
        "kotlin" | "scala" => &[r"^\s*import\s+([\w.*{}, ]+)"],
        "java" => &[r"^\s*import\s+(?:static\s+)?([\w.*]+)\s*;"],
        "python" => &[
            r"^\s*from\s+([\w.]+)\s+import\b",
            r"^\s*import\s+([\w., ]+)",
        ],
        "javascript" | "typescript" | "vue" | "svelte" => &[
            r#"^\s*import\s+(?:type\s+)?[^'"]*?\bfrom\s+['"]([^'"]+)['"]"#,
            r#"^\s*import\s+['"]([^'"]+)['"]"#,
            r#"^\s*export\s+[^'"]*?\bfrom\s+['"]([^'"]+)['"]"#,
            r#"\brequire\(\s*['"]([^'"]+)['"]\s*\)"#,
        ],
        "go" => &[r#"^\s*import\s+(?:\w+\s+)?"([^"]+)""#],
        "c" | "cpp" | "objective-c" | "objective-c++" => &[
            r#"^\s*#\s*(?:include|import)\s*[<"]([^>"]+)[>"]"#,
            r"^\s*@import\s+([\w.]+)",
        ],
        "csharp" => &[r"^\s*using\s+(?:static\s+)?([\w.]+)\s*;"],
        "dart" => &[r#"^\s*(?:import|export)\s+['"]([^'"]+)['"]"#],
        "ruby" => &[r#"^\s*require(?:_relative)?\s*\(?\s*['"]([^'"]+)['"]"#],
        "php" => &[
            r"^\s*use\s+([^;]+);",
            r#"^\s*(?:require|include)(?:_once)?\s*\(?\s*['"]([^'"]+)['"]"#,
        ],
        "shell" => &[r"^\s*(?:source|\.)\s+(\S+)"],
        "lua" => &[r#"\brequire\s*\(?\s*['"]([^'"]+)['"]"#],
        "perl" => &[r"^\s*(?:use|require)\s+([A-Za-z][\w:]*)"],
        "r" => &[r#"^\s*(?:library|require)\(\s*['"]?([\w.]+)['"]?\s*\)"#],
        "elixir" => &[r"^\s*(?:alias|import|use|require)\s+([A-Z][\w.]*)"],
        "haskell" => &[r"^\s*import\s+(?:qualified\s+)?([\w.]+)"],
        "css" | "scss" => &[r#"^\s*@(?:import|use|forward)\s+(?:url\()?['"]?([^'"\);]+)"#],
        "html" => &[
            r#"<script[^>]*\bsrc=["']([^"']+)["']"#,
            r#"<link[^>]*\bhref=["']([^"']+)["']"#,
        ],
        "dockerfile" => &[r"(?i)^\s*FROM\s+(?:--\S+\s+)*(\S+)"],
        "makefile" => &[r"^\s*-?include\s+(.+)"],
        "gradle" => &[
            r#"^\s*(?:implementation|api|compileOnly|runtimeOnly|testImplementation|androidTestImplementation|kapt|ksp|classpath)\s*\(?\s*['"]([^'"]+)['"]"#,
            r#"^\s*id\s*\(?\s*['"]([^'"]+)['"]"#,
        ],
        _ => &[],
    }
}

fn find_imports(text: &str, lang: &str) -> Vec<String> {
    let rules: Vec<Regex> = import_rules(lang)
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();
    let go_block = Regex::new(r#"^\s*(?:[\w.]+\s+)?"([^"]+)""#).ok();
    let mut out: Vec<String> = Vec::new();
    let mut in_go_imports = false;
    for line in text.lines() {
        if out.len() >= MAX_IMPORTS {
            break;
        }
        if lang == "go" {
            let t = line.trim();
            if t.starts_with("import (") {
                in_go_imports = true;
                continue;
            }
            if in_go_imports {
                if t.starts_with(')') {
                    in_go_imports = false;
                } else if let Some(c) = go_block.as_ref().and_then(|r| r.captures(line)) {
                    if let Some(m) = c.get(1) {
                        push_unique(&mut out, m.as_str());
                    }
                }
                continue;
            }
        }
        for re in &rules {
            for c in re.captures_iter(line) {
                if let Some(m) = c.get(1) {
                    push_unique(&mut out, m.as_str());
                }
            }
        }
    }
    out.truncate(MAX_IMPORTS);
    out
}

fn push_unique(out: &mut Vec<String>, s: &str) {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if !s.is_empty() && !out.contains(&s) {
        out.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, src: &str) -> Extracted {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, src).unwrap();
        read_code(&p, name).unwrap()
    }

    fn syms(out: &Extracted) -> Vec<(String, String, u64)> {
        out.structured["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["kind"].as_str().unwrap().to_string(),
                    s["name"].as_str().unwrap().to_string(),
                    s["line"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    fn has(out: &Extracted, kind: &str, name: &str) -> bool {
        syms(out).iter().any(|(k, n, _)| k == kind && n == name)
    }

    #[test]
    fn language_mapping() {
        assert_eq!(language("main.rs"), Some("rust"));
        assert_eq!(language("src/App.SWIFT"), Some("swift"));
        assert_eq!(language("build.gradle.kts"), Some("kotlin"));
        assert_eq!(language("x.mjs"), Some("javascript"));
        assert_eq!(language("x.cjs"), Some("javascript"));
        assert_eq!(language("C.tsx"), Some("typescript"));
        assert_eq!(language("C.jsx"), Some("javascript"));
        assert_eq!(language("a.hpp"), Some("cpp"));
        assert_eq!(language("a.h"), Some("c"));
        assert_eq!(language("a.cs"), Some("csharp"));
        assert_eq!(language("a.zsh"), Some("shell"));
        assert_eq!(language("a.yml"), Some("yaml"));
        assert_eq!(language("a.m"), Some("objective-c"));
        assert_eq!(language("a.exs"), Some("elixir"));
        assert_eq!(language("App.vue"), Some("vue"));
        assert_eq!(language("Dockerfile"), Some("dockerfile"));
        assert_eq!(language("dockerfile"), Some("dockerfile"));
        assert_eq!(language("Dockerfile.dev"), Some("dockerfile"));
        assert_eq!(language("app.dockerfile"), Some("dockerfile"));
        assert_eq!(language("Makefile"), Some("makefile"));
        assert_eq!(language("makefile"), Some("makefile"));
        assert_eq!(language("build.gradle"), Some("gradle"));
        assert_eq!(language("pom.xml"), Some("xml"));
        assert_eq!(language("photo.jpg"), None);
        assert_eq!(language("README"), None);
    }

    #[test]
    fn every_pattern_compiles() {
        let langs = [
            "rust",
            "swift",
            "kotlin",
            "python",
            "javascript",
            "typescript",
            "vue",
            "svelte",
            "go",
            "c",
            "cpp",
            "objective-c",
            "java",
            "csharp",
            "dart",
            "scala",
            "ruby",
            "php",
            "shell",
            "lua",
            "perl",
            "r",
            "elixir",
            "haskell",
            "sql",
            "scss",
            "css",
            "html",
            "dockerfile",
            "makefile",
            "gradle",
        ];
        for lang in langs {
            for (_, p) in symbol_rules(lang) {
                assert!(Regex::new(&p).is_ok(), "{lang}: {p}");
            }
            for p in import_rules(lang) {
                assert!(Regex::new(p).is_ok(), "{lang}: {p}");
            }
        }
    }

    #[test]
    fn rust_symbols() {
        let src = r#"//! Module docs.
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A thing.
pub struct Thing {
    pub id: u32,
}

pub(crate) enum Mode { A, B }

pub trait Speak {
    fn speak(&self) -> String;
}

impl Speak for Thing {
    fn speak(&self) -> String {
        String::new()
    }
}

impl<T: Clone> Wrapper<T> where T: Default {}

pub const LIMIT: usize = 10;
static mut COUNTER: u32 = 0;
type Map = HashMap<String, u32>;

pub async fn fetch() {}
const fn small() -> u8 { 1 }

/* block
   comment */
fn main() {
    let x = 1;
}
"#;
        let out = run("lib.rs", src);
        assert_eq!(out.kind, "code");
        assert_eq!(out.subtype.as_deref(), Some("rust"));
        let s = syms(&out);
        assert!(s.contains(&("struct".into(), "Thing".into(), 6)), "{s:?}");
        assert!(has(&out, "enum", "Mode"));
        assert!(has(&out, "trait", "Speak"));
        assert!(has(&out, "fn", "speak"));
        assert!(has(&out, "impl", "Speak for Thing"));
        assert!(has(&out, "impl", "Wrapper<T>"));
        assert!(has(&out, "const", "LIMIT"));
        assert!(has(&out, "const", "COUNTER"));
        assert!(has(&out, "type", "Map"));
        assert!(has(&out, "fn", "fetch"));
        assert!(has(&out, "fn", "small"));
        assert!(has(&out, "fn", "main"));
        assert!(!has(&out, "const", "x"));
        assert_eq!(
            out.structured["imports"],
            json!([
                "std::collections::HashMap",
                "serde::{Deserialize, Serialize}"
            ])
        );
        assert_eq!(out.structured["lines"], 35);
        assert_eq!(out.structured["comment_lines"], 4);
        assert_eq!(out.structured["blank_lines"], 8);
        assert!(out.text.starts_with("Rust source, 35 lines, "));
        assert!(out.text.contains("  L6 struct Thing\n"));
        assert!(out.text.contains("```rust\n//! Module docs."));
    }

    #[test]
    fn swift_symbols() {
        let src = r#"import SwiftUI
import Foundation

@MainActor
final class ChatModel: ObservableObject {
    @Published var text = ""
    func send() async {}
    private static func helper() {}
    class func make() -> ChatModel { ChatModel() }
}

struct ContentView: View {
    var body: some View { Text("hi") }
}

enum Tab: String { case chat, shopping }
protocol Store { func load() }
extension ContentView { func extra() {} }
typealias Handler = () -> Void
actor Cache {}
"#;
        let out = run("ContentView.swift", src);
        assert!(has(&out, "class", "ChatModel"));
        assert!(has(&out, "func", "send"));
        assert!(has(&out, "func", "helper"));
        assert!(has(&out, "func", "make"));
        assert!(has(&out, "struct", "ContentView"));
        assert!(has(&out, "enum", "Tab"));
        assert!(has(&out, "interface", "Store"));
        assert!(has(&out, "impl", "ContentView"));
        assert!(has(&out, "type", "Handler"));
        assert!(has(&out, "class", "Cache"));
        assert!(!syms(&out).iter().any(|(_, n, _)| n == "func"));
        assert_eq!(out.structured["imports"], json!(["SwiftUI", "Foundation"]));
    }

    #[test]
    fn python_symbols() {
        let src = "#!/usr/bin/env python3\n\"\"\"Doc.\"\"\"\nimport os, sys\nfrom pathlib import Path\n\nMAX_SIZE = 10\nDEBUG: bool = False\n\nclass Loader:\n    def __init__(self):\n        self.x = 1\n\n    async def load(self):\n        if MAX_SIZE == 10:\n            pass\n\ndef main():\n    # comment\n    return 0\n";
        let out = run("tool.py", src);
        let s = syms(&out);
        assert!(s.contains(&("const".into(), "MAX_SIZE".into(), 6)), "{s:?}");
        assert!(has(&out, "const", "DEBUG"));
        assert!(has(&out, "class", "Loader"));
        assert!(has(&out, "def", "__init__"));
        assert!(has(&out, "def", "load"));
        assert!(has(&out, "def", "main"));
        assert_eq!(s.len(), 6, "{s:?}");
        assert_eq!(out.structured["imports"], json!(["os, sys", "pathlib"]));
        assert_eq!(out.structured["comment_lines"], 2);
    }

    #[test]
    fn typescript_symbols() {
        let src = r#"import React, { useState } from "react";
import type { Item } from './types';
import "./styles.css";
const fs = require('fs');

export interface Props { items: Item[] }
export type Mode = "a" | "b";
export enum Color { Red, Green }
export const API_URL = "https://example.com";
const MAX = 3;

export default function App({ items }: Props) {
  const [n, setN] = useState(0);
  return <div>{n}</div>;
}

export const ItemRow = ({ item }: { item: Item }) => <li>{item.name}</li>;
const helper = async (x: number): Promise<number> => x * 2;

export class Store {
  load() {}
}
"#;
        let out = run("App.tsx", src);
        assert_eq!(out.subtype.as_deref(), Some("typescript"));
        assert!(has(&out, "interface", "Props"));
        assert!(has(&out, "type", "Mode"));
        assert!(has(&out, "enum", "Color"));
        assert!(has(&out, "const", "API_URL"));
        assert!(has(&out, "const", "MAX"));
        assert!(has(&out, "component", "App"));
        assert!(has(&out, "component", "ItemRow"));
        assert!(has(&out, "fn", "helper"));
        assert!(has(&out, "class", "Store"));
        assert!(!has(&out, "const", "n"));
        assert_eq!(
            out.structured["imports"],
            json!(["react", "./types", "./styles.css", "fs"])
        );

        // In a plain .ts file a capitalised function stays a function.
        let out = run("util.ts", "export function Parse(s: string) {}\n");
        assert!(has(&out, "fn", "Parse"));
    }

    #[test]
    fn go_symbols() {
        let src = "package main\n\nimport (\n\t\"fmt\"\n\tlog \"github.com/sirupsen/logrus\"\n)\n\nimport \"os\"\n\nconst Version = \"1.0\"\n\ntype Server struct {\n\tAddr string\n}\n\ntype Handler interface {\n\tServe()\n}\n\ntype ID int64\n\nfunc (s *Server) Start() error {\n\treturn nil\n}\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n";
        let out = run("main.go", src);
        let s = syms(&out);
        assert!(has(&out, "const", "Version"));
        assert!(has(&out, "struct", "Server"));
        assert!(has(&out, "interface", "Handler"));
        assert!(has(&out, "type", "ID"));
        assert!(has(&out, "func", "Start"));
        assert!(has(&out, "func", "main"));
        assert_eq!(s.len(), 6, "{s:?}");
        assert_eq!(
            out.structured["imports"],
            json!(["fmt", "github.com/sirupsen/logrus", "os"])
        );
    }

    #[test]
    fn vue_and_sql_and_shell() {
        let out = run("Card.vue", "<template><div/></template>\n<script>\nexport default {}\nfunction setup() {}\n</script>\n");
        assert!(has(&out, "component", "Card"));
        assert!(has(&out, "fn", "setup"));
        let out = run("schema.sql", "-- schema\nCREATE TABLE IF NOT EXISTS items (id int);\ncreate or replace function total() returns int as $$ select 1 $$;\n");
        assert!(has(&out, "type", "items"));
        assert!(has(&out, "fn", "total"));
        assert_eq!(out.structured["comment_lines"], 1);
        let out = run(
            "run.sh",
            "#!/bin/sh\nsource ./env.sh\nbuild() {\n  echo hi\n}\nfunction deploy {\n  :\n}\n",
        );
        assert!(has(&out, "fn", "build"));
        assert!(has(&out, "fn", "deploy"));
        assert_eq!(out.structured["imports"], json!(["./env.sh"]));
    }

    #[test]
    fn binary_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.rs");
        std::fs::write(&p, b"fn main() {}\n\0\0\x01\x02").unwrap();
        let err = read_code(&p, "blob.rs").unwrap_err();
        assert!(err.to_string().contains("binary"));
    }

    #[test]
    fn large_file_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.json");
        let line = "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaàà\",\n";
        let body = line.repeat(2 * 1024 * 1024 / line.len() + 10);
        std::fs::write(&p, &body).unwrap();
        let out = read_code(&p, "big.json").unwrap();
        assert_eq!(out.subtype.as_deref(), Some("json"));
        assert!(
            out.warnings.iter().any(|w| w.contains("first 2 MB")),
            "{:?}",
            out.warnings
        );
        assert!(
            !out.warnings.iter().any(|w| w.contains("UTF-8")),
            "{:?}",
            out.warnings
        );
        assert!(out.structured["symbols"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unknown_extension_is_text() {
        let out = run("notes.weird", "hello\n");
        assert_eq!(out.subtype.as_deref(), Some("text"));
        assert_eq!(out.structured["lines"], 1);
    }
}
