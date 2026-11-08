//! Syntax highlighting for the Drive's text and code preview: syntect's grammars (pure Rust, the
//! `fancy-regex` engine), reduced to a dozen token classes the apps colour themselves, so the same
//! answer works in light and dark mode on the iPhone and the web.

use std::sync::OnceLock;

use syntect::parsing::{ParseState, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

/// Highlight at most this many lines; the rest of the text stays plain.
pub const MAX_LINES: usize = 5000;
/// Lines longer than this (minified files) are not parsed.
const MAX_LINE_BYTES: usize = 4000;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// One line: `(class, text)` runs, classes as in the API (`""` plain, `kw`, `str`, …).
pub type Line = Vec<(&'static str, String)>;

/// The grammar for `language` (a name from `doctype::language_for`), with fallbacks for the
/// languages syntect has no grammar for.
fn syntax_for(language: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let fallback = match language {
        "TypeScript" | "TypeScriptReact" | "JavaScript (Babel)" | "Dart" => "JavaScript",
        "Kotlin" | "Groovy" => "Java",
        "Swift" => "Rust",
        "TOML" | "INI" => "YAML",
        "Elixir" => "Ruby",
        "PowerShell" | "Dockerfile" => "Bourne Again Shell (bash)",
        "CMake" | "CSV" => "Plain Text",
        other => other,
    };
    set.find_syntax_by_name(language)
        .or_else(|| set.find_syntax_by_name(fallback))
        .filter(|s| s.name != "Plain Text")
}

/// The class for the innermost scopes that mean something to a reader.
fn class_of(stack: &ScopeStack) -> &'static str {
    for scope in stack.as_slice().iter().rev() {
        let s = scope.build_string();
        let c = if s.starts_with("comment") {
            "com"
        } else if s.starts_with("string") || s.starts_with("constant.character.escape") {
            "str"
        } else if s.starts_with("constant.numeric") {
            "num"
        } else if s.starts_with("constant") || s.starts_with("support.constant") {
            "const"
        } else if s.starts_with("keyword.operator") || s.starts_with("punctuation") {
            "punct"
        } else if s.starts_with("keyword") || s.starts_with("storage") {
            "kw"
        } else if s.starts_with("entity.name.function")
            || s.starts_with("support.function")
            || s.starts_with("variable.function")
        {
            "fn"
        } else if s.starts_with("entity.name.tag") {
            "tag"
        } else if s.starts_with("entity.other.attribute-name") || s.starts_with("meta.mapping.key")
        {
            "attr"
        } else if s.starts_with("entity.name")
            || s.starts_with("support.type")
            || s.starts_with("support.class")
        {
            "type"
        } else if s.starts_with("markup.heading") {
            "head"
        } else if s.starts_with("markup") {
            "em"
        } else if s.starts_with("variable") {
            "var"
        } else {
            continue;
        };
        return c;
    }
    ""
}

/// Token lines for `text`, or `None` when there is no grammar for `language`.
pub fn highlight(text: &str, language: &str) -> Option<Vec<Line>> {
    let syntax = syntax_for(language)?;
    let set = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    for (n, line) in syntect::util::LinesWithEndings::from(text).enumerate() {
        let content = line.trim_end_matches(['\n', '\r']);
        if n >= MAX_LINES || line.len() > MAX_LINE_BYTES {
            out.push(vec![("", content.to_string())]);
            if n >= MAX_LINES {
                continue;
            }
            // A minified line breaks the parser's state; start again after it.
            state = ParseState::new(syntax);
            stack = ScopeStack::new();
            continue;
        }
        let ops = match state.parse_line(line, set) {
            Ok(ops) => ops,
            Err(_) => {
                out.push(vec![("", content.to_string())]);
                continue;
            }
        };
        let mut runs: Line = Vec::new();
        let mut pos = 0;
        let push = |runs: &mut Line, class: &'static str, s: &str| {
            if s.is_empty() {
                return;
            }
            match runs.last_mut() {
                Some((c, t)) if *c == class => t.push_str(s),
                _ => runs.push((class, s.to_string())),
            }
        };
        for (at, op) in ops {
            let at = at.min(content.len());
            if at > pos {
                push(&mut runs, class_of(&stack), &content[pos..at]);
                pos = at;
            }
            let _ = apply(&mut stack, &op);
        }
        if pos < content.len() {
            push(&mut runs, class_of(&stack), &content[pos..]);
        }
        out.push(runs);
    }
    Some(out)
}

fn apply(stack: &mut ScopeStack, op: &ScopeStackOp) -> Result<(), syntect::parsing::ScopeError> {
    stack.apply(op)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_gets_keywords_strings_and_comments() {
        let lines = highlight("fn main() {\n    // hi\n    let s = \"x\";\n}\n", "Rust").unwrap();
        assert_eq!(lines.len(), 4);
        let text: String = lines[0].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, "fn main() {");
        assert!(lines[0].iter().any(|(c, t)| *c == "kw" && t == "fn"));
        assert!(lines[1].iter().any(|(c, _)| *c == "com"));
        assert!(lines[2].iter().any(|(c, t)| *c == "str" && t.contains('x')));
    }

    #[test]
    fn unknown_languages_fall_back_or_stay_plain() {
        assert!(highlight("const a: number = 1\n", "TypeScript").is_some());
        assert!(highlight("hello", "Plain Text").is_none());
        assert!(highlight("hello", "Nope").is_none());
    }

    #[test]
    fn long_lines_and_crlf_are_kept_whole() {
        let long = "x".repeat(MAX_LINE_BYTES + 10);
        let lines = highlight(&format!("a = 1\r\n{long}\nb = 2\n"), "Python").unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], vec![("", long)]);
        let first: String = lines[0].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(first, "a = 1");
    }
}
