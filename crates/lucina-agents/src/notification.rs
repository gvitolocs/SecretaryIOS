//! Attention notification texts (Paseo `protocol/agent-attention-notification.js`):
//! the title by reason, and a 220-character plain-text preview of the last assistant
//! message (finished) or of the permission request (permission).
//!
//! JS regex differences kept in mind: `\s` is the JS whitespace set ([`jsstr`]),
//! `\d` is ASCII, and lengths and cuts are in UTF-16 code units. Multiline `^`/`$`
//! treat only `\n` as a line end (JS also treats a lone `\r`, U+2028 and U+2029).

use std::sync::OnceLock;

use regex::Regex;

use crate::jsstr;
use crate::permission::{PermissionKind, PermissionRequest};
use crate::timeline::TimelineItem;
use crate::types::AttentionReason;

pub const PREVIEW_LIMIT: usize = 220;

/// The JS `\s` class, for use inside a regex.
const WS: &str = r"\t\n\x0B\x0C\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}";

pub fn title(reason: AttentionReason) -> &'static str {
    match reason {
        AttentionReason::Permission => "Agent needs permission",
        AttentionReason::Error => "Agent needs attention",
        AttentionReason::Finished => "Agent finished",
    }
}

fn fallback_body(reason: AttentionReason) -> &'static str {
    match reason {
        AttentionReason::Permission => "Permission requested.",
        AttentionReason::Error => "Encountered an error.",
        AttentionReason::Finished => "Finished working.",
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if jsstr::len(text) <= limit {
        return text.to_string();
    }
    let cut = jsstr::slice(text, 0, limit.saturating_sub(3));
    let trimmed = jsstr::trim_end(&cut);
    if trimmed.is_empty() {
        jsstr::slice(text, 0, limit)
    } else {
        format!("{trimmed}...")
    }
}

struct Rules {
    fence_backtick: Regex,
    fence_tilde: Regex,
    image: Regex,
    link: Regex,
    heading: Regex,
    quote: Regex,
    list: Regex,
    rule: Regex,
    code: Regex,
    bold_star: Regex,
    bold_under: Regex,
    em_star: Regex,
    em_under: Regex,
    strike: Regex,
    autolink: Regex,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        let re = |p: String| Regex::new(&p).expect("static regex");
        let s = format!("[{WS}]");
        Rules {
            fence_backtick: re(format!(r"(?m)^{s}*```[^\n]*$")),
            fence_tilde: re(format!(r"(?m)^{s}*~~~[^\n]*$")),
            image: re(r"!\[([^\]]*)\]\((?:[^()\\]|\\.)*\)".into()),
            link: re(r"\[([^\]]+)\]\((?:[^()\\]|\\.)*\)".into()),
            heading: re(format!(r"(?m)^{s}{{0,3}}#{{1,6}}{s}+")),
            quote: re(format!(r"(?m)^{s}{{0,3}}>+{s}?")),
            list: re(format!(r"(?m)^{s}{{0,3}}(?:[*+-]|[0-9]+\.){s}+")),
            rule: re(format!(r"(?m)^{s}{{0,3}}(?:[-*_]{s}*){{3,}}$")),
            code: re(r"`([^`]+)`".into()),
            bold_star: re(r"\*\*([^*]+)\*\*".into()),
            bold_under: re(r"__([^_]+)__".into()),
            em_star: re(r"\*([^*\n]+)\*".into()),
            em_under: re(r"_([^_\n]+)_".into()),
            strike: re(r"~~([^~]+)~~".into()),
            autolink: re(r"<([^>\n]+)>".into()),
        }
    })
}

/// Paseo `stripMarkdownToText`.
pub fn strip_markdown(markdown: &str) -> String {
    let r = rules();
    let mut t = markdown.replace("\r\n", "\n");
    for (re, with) in [
        (&r.fence_backtick, ""),
        (&r.fence_tilde, ""),
        (&r.image, "$1"),
        (&r.link, "$1"),
        (&r.heading, ""),
        (&r.quote, ""),
        (&r.list, ""),
        (&r.rule, ""),
        (&r.code, "$1"),
        (&r.bold_star, "$1"),
        (&r.bold_under, "$1"),
        (&r.em_star, "$1"),
        (&r.em_under, "$1"),
        (&r.strike, "$1"),
        (&r.autolink, "$1"),
    ] {
        t = re.replace_all(&t, with).into_owned();
    }
    t
}

/// Paseo `buildNotificationPreview`.
pub fn preview(text: Option<&str>) -> Option<String> {
    let text = text.filter(|t| !t.is_empty())?;
    let normalized = jsstr::trim(&jsstr::collapse_spaces(&strip_markdown(text))).to_string();
    (!normalized.is_empty()).then(|| truncate(&normalized, PREVIEW_LIMIT))
}

/// Paseo `buildPermissionDetails`.
pub fn permission_details(request: &PermissionRequest) -> String {
    let title = request.title.as_deref().map(jsstr::trim).filter(|t| !t.is_empty());
    let description = request.description.as_deref().map(jsstr::trim).filter(|d| !d.is_empty());
    let mut details: Vec<&str> = Vec::new();
    if let Some(t) = title {
        details.push(t);
    }
    if let Some(d) = description.filter(|d| Some(*d) != title) {
        details.push(d);
    }
    if !details.is_empty() {
        return details.join(" - ");
    }
    if let Some(input) = &request.input {
        if let Ok(s) = serde_json::to_string(input) {
            return s;
        }
    }
    if let Some(meta) = &request.metadata {
        if let Ok(s) = serde_json::to_string(meta) {
            return s;
        }
    }
    let name = jsstr::trim(&request.name);
    if !name.is_empty() {
        return name.to_string();
    }
    match request.kind {
        PermissionKind::Tool => "tool",
        PermissionKind::Plan => "plan",
        PermissionKind::Question => "question",
        PermissionKind::Mode => "mode",
        PermissionKind::Other => "other",
    }
    .to_string()
}

/// Paseo `findLatestAssistantMessageFromTimeline`: the last run of consecutive
/// assistant chunks, joined.
pub fn latest_assistant_message(items: &[TimelineItem]) -> Option<String> {
    let mut chunks: Vec<&str> = Vec::new();
    for item in items.iter().rev() {
        match item {
            TimelineItem::AssistantMessage { text, .. } => chunks.push(text),
            _ if !chunks.is_empty() => break,
            _ => {}
        }
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.reverse();
    Some(chunks.concat())
}

/// The notification body (Paseo `buildAgentAttentionNotificationPayload`).
pub fn body(reason: AttentionReason, assistant_message: Option<&str>, permission: Option<&PermissionRequest>) -> String {
    let preview = match reason {
        AttentionReason::Finished => preview(assistant_message),
        AttentionReason::Permission => permission.and_then(|p| preview(Some(&permission_details(p)))),
        AttentionReason::Error => None,
    };
    preview.unwrap_or_else(|| fallback_body(reason).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews() {
        assert_eq!(preview(Some("## Done\n\n- **fixed** the `scroll` [anchor](http://x)")).as_deref(), Some("Done fixed the scroll anchor"));
        assert_eq!(preview(Some("   ")), None);
        let long = "word ".repeat(100);
        let p = preview(Some(&long)).unwrap();
        assert_eq!(jsstr::len(&p), 220);
        assert!(p.ends_with(" wo..."));
        assert_eq!(body(AttentionReason::Finished, None, None), "Finished working.");
    }
}
