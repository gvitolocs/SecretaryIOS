//! `textContentHtml` (Takeout) → markdown basics: `**bold**`, `*italic*`,
//! `<u>underline</u>`, `# H1`, `## H2`, paragraphs and line breaks, lists,
//! and `[text](href)` for http(s) links. Everything else is dropped and
//! entities are decoded. No HTML parser crate: the input is Keep's own small
//! subset (`<p>`, `<span style=…>`, `<b>`, `<i>`, `<a>`, `<br>`).

/// One thing to undo when a tag closes.
enum Act {
    /// An emphasis marker already written at `pos` (byte index in the output).
    Wrap {
        open: &'static str,
        close: &'static str,
        pos: usize,
    },
    /// An `<a>`: the link text starts at `pos`.
    Link { href: Option<String>, pos: usize },
    /// A block (`<p>`, `<div>`, `<li>`, headings): end the line.
    EndLine,
}

struct Frame {
    tag: String,
    acts: Vec<Act>,
}

const VOID: &[&str] = &["br", "hr", "img", "input", "meta", "link", "wbr"];

/// Convert Keep's `textContentHtml` to markdown. Returns the empty string
/// when nothing readable is left.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        if b[i] != b'<' {
            let end = html[i..].find('<').map_or(html.len(), |p| i + p);
            out.push_str(&decode_entities(&html[i..end]));
            i = end;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(html.len(), |p| i + p + 3);
            continue;
        }
        let next = html[i + 1..].chars().next();
        if !matches!(next, Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') {
            // A lone `<` in text.
            out.push('<');
            i += 1;
            continue;
        }
        let Some(end) = tag_end(html, i) else {
            out.push_str(&decode_entities(&html[i..]));
            break;
        };
        let inner = &html[i + 1..end];
        i = end + 1;
        if inner.starts_with('!') || inner.starts_with('?') {
            continue;
        }
        let (closing, inner) = match inner.strip_prefix('/') {
            Some(r) => (true, r),
            None => (false, inner),
        };
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        let attrs = &inner[name_end..];
        if closing {
            close_tag(&mut out, &mut stack, &name);
        } else {
            if name == "script" || name == "style" {
                // Skip the element's text.
                let close = format!("</{name}");
                let lower = html[i..].to_ascii_lowercase();
                i = lower.find(&close).map_or(html.len(), |p| {
                    html[i + p..]
                        .find('>')
                        .map_or(html.len(), |q| i + p + q + 1)
                });
                continue;
            }
            open_tag(&mut out, &mut stack, &name, attrs);
        }
    }
    while let Some(f) = stack.pop() {
        run_acts(&mut out, f.acts);
    }
    out.trim_matches(|c: char| c.is_whitespace()).to_owned()
}

/// Index of the `>` that ends the tag starting at `start`, honouring quotes.
fn tag_end(s: &str, start: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (k, &c) in s.as_bytes().iter().enumerate().skip(start + 1) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return Some(k),
            None => {}
        }
    }
    None
}

fn ensure_line_start(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn inside_wrap(stack: &[Frame], marker: &str) -> bool {
    stack.iter().any(|f| {
        f.acts
            .iter()
            .any(|a| matches!(a, Act::Wrap { open, .. } if *open == marker))
    })
}

fn open_wrap(
    out: &mut String,
    stack: &[Frame],
    acts: &mut Vec<Act>,
    open: &'static str,
    close: &'static str,
) {
    if inside_wrap(stack, open)
        || acts
            .iter()
            .any(|a| matches!(a, Act::Wrap { open: o, .. } if *o == open))
    {
        return;
    }
    acts.push(Act::Wrap {
        open,
        close,
        pos: out.len(),
    });
    out.push_str(open);
}

fn open_tag(out: &mut String, stack: &mut Vec<Frame>, name: &str, attrs: &str) {
    if name == "br" {
        out.push('\n');
        return;
    }
    if VOID.contains(&name) || (attrs.trim_end().ends_with('/') && name != "a") {
        if name == "hr" {
            ensure_line_start(out);
        }
        return;
    }
    let mut acts = Vec::new();
    match name {
        "p" | "div" | "ul" | "ol" | "h3" | "h4" | "h5" | "h6" => {
            ensure_line_start(out);
            acts.push(Act::EndLine);
        }
        "h1" => {
            ensure_line_start(out);
            out.push_str("# ");
            acts.push(Act::EndLine);
        }
        "h2" => {
            ensure_line_start(out);
            out.push_str("## ");
            acts.push(Act::EndLine);
        }
        "li" => {
            ensure_line_start(out);
            out.push_str("- ");
            acts.push(Act::EndLine);
        }
        "b" | "strong" => open_wrap(out, stack, &mut acts, "**", "**"),
        "i" | "em" => open_wrap(out, stack, &mut acts, "*", "*"),
        "u" => open_wrap(out, stack, &mut acts, "<u>", "</u>"),
        "span" => {
            let style = attr(attrs, "style")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (bold, italic, underline) = style_flags(&style);
            if bold {
                open_wrap(out, stack, &mut acts, "**", "**");
            }
            if italic {
                open_wrap(out, stack, &mut acts, "*", "*");
            }
            if underline {
                open_wrap(out, stack, &mut acts, "<u>", "</u>");
            }
        }
        "a" => acts.push(Act::Link {
            href: attr(attrs, "href"),
            pos: out.len(),
        }),
        _ => return,
    }
    stack.push(Frame {
        tag: name.to_owned(),
        acts,
    });
}

fn close_tag(out: &mut String, stack: &mut Vec<Frame>, name: &str) {
    let Some(at) = stack.iter().rposition(|f| f.tag == name) else {
        return;
    };
    while stack.len() > at {
        let f = stack.pop().expect("len > at");
        run_acts(out, f.acts);
    }
}

fn run_acts(out: &mut String, acts: Vec<Act>) {
    for act in acts.into_iter().rev() {
        match act {
            Act::EndLine => ensure_line_start(out),
            Act::Wrap { open, close, pos } => {
                let inner = out.split_off(pos + open.len());
                out.truncate(pos);
                let core = inner.trim_matches(|c: char| c.is_whitespace());
                if core.is_empty() {
                    out.push_str(&inner);
                    continue;
                }
                let lead_len =
                    inner.len() - inner.trim_start_matches(|c: char| c.is_whitespace()).len();
                let trail_start = lead_len + core.len();
                out.push_str(&inner[..lead_len]);
                out.push_str(open);
                out.push_str(core);
                out.push_str(close);
                out.push_str(&inner[trail_start..]);
            }
            Act::Link { href, pos } => {
                let text = out.split_off(pos);
                match href.as_deref().map(str::trim) {
                    Some(h)
                        if (h.starts_with("http://") || h.starts_with("https://"))
                            && !text.trim().is_empty() =>
                    {
                        let h = h.replace(' ', "%20").replace(')', "%29");
                        out.push_str(&format!("[{}]({h})", text.trim()));
                    }
                    _ => out.push_str(&text),
                }
            }
        }
    }
}

/// (bold, italic, underline) from a lowercased inline `style`.
fn style_flags(style: &str) -> (bool, bool, bool) {
    let (mut bold, mut italic, mut underline) = (false, false, false);
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "font-weight" => {
                bold = v == "bold"
                    || v == "bolder"
                    || v.parse::<u32>().map(|w| w >= 600).unwrap_or(false);
            }
            "font-style" => italic = v == "italic" || v == "oblique",
            "text-decoration" | "text-decoration-line" => underline = v.contains("underline"),
            _ => {}
        }
    }
    (bold, italic, underline)
}

/// Value of attribute `name` in the text after the tag name.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let b = attrs.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        let key = attrs[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut val = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                val = attrs[vs..i].to_owned();
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                val = attrs[vs..i].to_owned();
            }
        }
        if !key.is_empty() && key == name {
            return Some(decode_entities(&val));
        }
    }
    None
}

/// Decode `&amp;` `&lt;` `&gt;` `&quot;` `&apos;` `&nbsp;` and numeric
/// entities; anything else stays as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let semi = rest.char_indices().take(12).find(|&(_, c)| c == ';');
        let decoded = semi.and_then(|(e, _)| {
            let ent = &rest[1..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => ent.strip_prefix('#').and_then(|n| {
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse::<u32>().ok(),
                    }?;
                    char::from_u32(code).map(|c| if c == '\u{a0}' { ' ' } else { c })
                }),
            }?;
            Some((ch, e + 1))
        });
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(
            html_to_markdown("<b>bold</b> and <i>it</i> and <u>und</u>"),
            "**bold** and *it* and <u>und</u>"
        );
        assert_eq!(html_to_markdown("<strong>a</strong><em>b</em>"), "**a***b*");
        assert_eq!(
            html_to_markdown("<h1>Big</h1><h2>Mid</h2>text"),
            "# Big\n## Mid\ntext"
        );
        assert_eq!(html_to_markdown("a<br>b<br/>c"), "a\nb\nc");
    }

    #[test]
    fn paragraphs_and_blank_lines() {
        assert_eq!(
            html_to_markdown("<p>one</p><p><br></p><p>two</p>"),
            "one\n\ntwo"
        );
        assert_eq!(html_to_markdown("<p>a</p><p>b</p>"), "a\nb");
        assert_eq!(html_to_markdown("<div>a<br></div><div>b</div>"), "a\nb");
    }

    #[test]
    fn takeout_style_spans() {
        let h = r#"<p dir="ltr" style="line-height:1.38"><span style="font-size:11pt;font-weight:400;font-style:normal;text-decoration:none">plain </span><span style="font-weight:700">bold</span><span style="font-style:italic;text-decoration:underline"> x</span></p>"#;
        assert_eq!(html_to_markdown(h), "plain **bold** *<u>x</u>*");
    }

    #[test]
    fn emphasis_whitespace_moves_outside() {
        assert_eq!(html_to_markdown("a<b> x </b>y"), "a **x** y");
        assert_eq!(html_to_markdown("a<b> </b>b"), "a b");
        assert_eq!(html_to_markdown("<b>x<br></b>y"), "**x**\ny");
        assert_eq!(html_to_markdown("<b><strong>x</strong></b>"), "**x**");
    }

    #[test]
    fn links() {
        assert_eq!(
            html_to_markdown(r#"see <a href="https://example.org/a?b=1&amp;c=2">here</a>"#),
            "see [here](https://example.org/a?b=1&c=2)"
        );
        assert_eq!(
            html_to_markdown(r#"<a href="javascript:alert(1)">x</a> <a href="mailto:a@b.c">m</a>"#),
            "x m"
        );
    }

    #[test]
    fn entities_and_noise() {
        assert_eq!(
            html_to_markdown(
                "a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &#x41;&nbsp;f &bogus; 1 < 2"
            ),
            "a & b <c> \"d\" 'e' A f &bogus; 1 < 2"
        );
        assert_eq!(
            html_to_markdown("<!-- c -->x<script>alert(1)</script>y"),
            "xy"
        );
        assert_eq!(html_to_markdown("<span></span>"), "");
        assert_eq!(
            html_to_markdown("<ul><li>a</li><li>b</li></ul>"),
            "- a\n- b"
        );
    }
}
