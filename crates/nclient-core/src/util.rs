//! Port of `utility/Utility.java` (pure parts) + `java.net.URLEncoder` parity.

/// `Utility.ORIGINAL_URL`
pub const ORIGINAL_URL: &str = "nhentai.net";

/// `Utility.getHost()` — the app reads the mirror from settings; the default
/// mirror equals `Utility.ORIGINAL_URL`. Network/IO-free port takes the host
/// as a parameter where it matters; this is the default value.
pub const DEFAULT_HOST: &str = ORIGINAL_URL;

/// `Utility.getBaseUrl()`
pub fn base_url(host: &str) -> String {
    format!("https://{host}/")
}

/// `Utility.getApiBaseUrl()`
pub fn api_base_url(host: &str) -> String {
    format!("{}api/v2/", base_url(host))
}

/// Exact port of `Utility.unescapeUnicodeString` + `parseEscapedCharacter`,
/// including the original's quirks:
/// * `\uXXXX` consumes exactly 4 chars via `Character.digit`; an invalid or
///   missing digit yields `-1`, so the code point can go negative — the Java
///   writer keeps the low 16 bits (e.g. `"\\u12"` → U+11EF, `"\\uZZZZ"` → U+FFFF).
/// * An unknown escape writes the backslash AND the following char verbatim
///   (`"\\'"` stays `"\\'"`).
/// * A backslash at EOF writes `\` followed by char `0xFFFF` (Java
///   `Writer.write(-1)` is defined as writing char 0xFFFF).
/// * `\n`, `\t` handled; other letter escapes pass both chars through.
pub fn unescape_unicode_string(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '\\' {
            out.push(c);
            continue;
        }
        // parseEscapedCharacter
        let read: i32 = if i < chars.len() { chars[i] as i32 } else { -1 };
        i += 1;
        match read {
            0x75 /* 'u' */ => {
                let mut to_create: i32 = 0;
                for _ in 0..4 {
                    to_create *= 16;
                    let d = if i < chars.len() { char_digit(chars[i]) } else { -1 };
                    i += 1;
                    to_create += d;
                }
                // Writer.write(int) keeps the low 16 bits.
                let ch = (to_create & 0xFFFF) as u16 as u32;
                out.push(char::from_u32(ch).unwrap_or('\u{FFFD}'));
            }
            0x6E /* 'n' */ => out.push('\n'),
            0x74 /* 't' */ => out.push('\t'),
            other => {
                out.push('\\');
                if other != -1 {
                    out.push(char::from_u32(other as u32).unwrap_or('\u{FFFD}'));
                } else {
                    // Java Writer.write(-1) writes char 0xFFFF.
                    out.push('\u{FFFF}');
                }
            }
        }
    }
    out
}

/// `Character.digit(ch, 16)` — `-1` when not a hex digit (or EOF).
fn char_digit(c: char) -> i32 {
    match c {
        '0'..='9' => c as i32 - '0' as i32,
        'a'..='f' => c as i32 - 'a' as i32 + 10,
        'A'..='F' => c as i32 - 'A' as i32 + 10,
        _ => -1,
    }
}

/// Parity with `java.net.URLEncoder.encode(s, "UTF-8")`:
/// `a-zA-Z0-9`, `.`, `-`, `*`, `_` pass through; space becomes `+`;
/// every other byte (UTF-8) becomes `%XX` with uppercase hex.
pub fn java_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
