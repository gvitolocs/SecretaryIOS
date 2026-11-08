//! Node `replyText`: trimmed text in Telegram HTML (`**bold**`/`__bold__` to
//! `<b>`, everything else escaped), here split at 4096 instead of cut. A part
//! whose HTML would not fit, or that Telegram refuses to parse, goes plain.
use std::sync::OnceLock;

use lucina_telegram::{split_text, ApiError, BotApi, SendOptions};
use regex::Regex;

pub const TEXT_LIMIT: usize = 4096;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Node `telegramHtml` (without its cut: the caller splits).
pub fn telegram_html(text: &str) -> String {
    static BOLD: OnceLock<Regex> = OnceLock::new();
    let re = BOLD.get_or_init(|| {
        Regex::new(r"(?s)\*\*([^\n*_].*?[^\n*_])\*\*|__([^\n*_].*?[^\n*_])__").expect("bold regex")
    });
    let mut out = String::with_capacity(text.len() + 16);
    let mut last = 0;
    for c in re.captures_iter(text) {
        let whole = c.get(0).expect("match");
        let inner = c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str());
        out.push_str(&escape(&text[last..whole.start()]));
        out.push_str("<b>");
        out.push_str(&escape(inner));
        out.push_str("</b>");
        last = whole.end();
    }
    out.push_str(&escape(&text[last..]));
    out
}

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Send `text` to `chat_id` as Node's `replyText` does. Returns the plain
/// text that was sent ("" when there was nothing to send).
pub async fn reply_text(
    bot: &BotApi,
    chat_id: &str,
    text: &str,
    disable_web_page_preview: bool,
) -> Result<String, ApiError> {
    let cleaned = text.trim();
    for part in split_text(cleaned) {
        let html = telegram_html(part);
        let plain = SendOptions {
            disable_web_page_preview,
            ..SendOptions::default()
        };
        if utf16(&html) > TEXT_LIMIT {
            bot.send_message(chat_id, part, &plain).await?;
            continue;
        }
        let rich = SendOptions {
            parse_mode: Some("HTML".into()),
            ..plain.clone()
        };
        match bot.send_message(chat_id, &html, &rich).await {
            Ok(_) => {}
            // "can't parse entities": the same text without markup.
            Err(e) if e.error_code == Some(400) || e.http_status == Some(400) => {
                tracing::warn!(error = %e, "telegram HTML rejected; sending plain text");
                bot.send_message(chat_id, part, &plain).await?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(cleaned.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_like_node() {
        assert_eq!(
            telegram_html("a < b & **c > d**"),
            "a &lt; b &amp; <b>c &gt; d</b>"
        );
        assert_eq!(telegram_html("__ab__ and **x**"), "<b>ab</b> and **x**");
        assert_eq!(telegram_html("**a\nb**"), "<b>a\nb</b>");
        assert_eq!(telegram_html("**\nab**"), "**\nab**");
        assert_eq!(telegram_html("plain"), "plain");
    }
}
