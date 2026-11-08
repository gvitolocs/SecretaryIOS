//! tmux-style key tokens into terminal input (Paseo `paseo-tools.js
//! resolveTerminalKeyToken` and `protocol/terminal-key-input.js
//! encodeTerminalKeyInput`).
//!
//! A token is `[C-|M-|S-]*<key>`: a key name (Enter, Tab, Escape, Space, BSpace, Up,
//! Down, Left, Right, Home, End, Insert, Delete, PageUp, PageDown, F1-F12) or one
//! character. Anything else is sent as text. `lagent terminal send-keys` and the MCP
//! both use this encoder; Paseo's CLI has a smaller table (it types `Up` as the word),
//! which agrees with this one on every token it knows.
//!
//! Not ported: ConPTY's win32 input mode (Windows only) and the kitty keyboard
//! protocol (Paseo uses it only for modified Enter when an app turned it on).

/// The modes a terminal app can switch on that change what keys send.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputMode {
    /// DECCKM: arrows send `ESC O x` instead of `ESC [ x`.
    pub application_cursor: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
}

fn modifier_param(m: Mods) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

fn alt_prefix(seq: String, m: Mods) -> String {
    if m.alt {
        format!("\x1b{seq}")
    } else {
        seq
    }
}

fn ctrl_symbol(c: char) -> Option<char> {
    Some(match c {
        ' ' | '@' | '2' => '\x00',
        '[' | '3' => '\x1b',
        '\\' | '4' => '\x1c',
        ']' | '5' => '\x1d',
        '^' | '6' => '\x1e',
        '_' | '/' | '7' => '\x1f',
        '8' | '?' => '\x7f',
        _ => return None,
    })
}

fn ctrl_char(c: char, m: Mods) -> String {
    let upper = c.to_ascii_uppercase();
    if upper.is_ascii_uppercase() {
        return alt_prefix(char::from(upper as u8 - 64).to_string(), m);
    }
    if let Some(s) = ctrl_symbol(c) {
        return alt_prefix(s.to_string(), m);
    }
    // JavaScript `charCodeAt(0) & 0x1f` works on UTF-16 code units.
    let code = c.encode_utf16(&mut [0; 2])[0] & 0x1f;
    alt_prefix(char::from(code as u8).to_string(), m)
}

fn printable(key: &str, m: Mods) -> String {
    let c = key.chars().next().unwrap_or(' ');
    let c = if m.shift { c.to_uppercase().next().unwrap_or(c) } else { c };
    if m.ctrl {
        return ctrl_char(c, m);
    }
    alt_prefix(c.to_string(), m)
}

fn csi(final_byte: char, m: Mods) -> String {
    match modifier_param(m) {
        1 => format!("\x1b[{final_byte}"),
        p => format!("\x1b[1;{p}{final_byte}"),
    }
}

fn ss3(final_byte: char, m: Mods) -> String {
    if modifier_param(m) == 1 {
        format!("\x1bO{final_byte}")
    } else {
        csi(final_byte, m)
    }
}

fn tilde(base: u8, m: Mods) -> String {
    match modifier_param(m) {
        1 => format!("\x1b[{base}~"),
        p => format!("\x1b[{base};{p}~"),
    }
}

fn navigation(key: &str, m: Mods, mode: InputMode) -> Option<String> {
    let arrow = |b: char| if mode.application_cursor { ss3(b, m) } else { csi(b, m) };
    Some(match key {
        "ArrowUp" => arrow('A'),
        "ArrowDown" => arrow('B'),
        "ArrowRight" => arrow('C'),
        "ArrowLeft" => arrow('D'),
        "Home" => csi('H', m),
        "End" => csi('F', m),
        "Insert" => tilde(2, m),
        "Delete" => tilde(3, m),
        "PageUp" => tilde(5, m),
        "PageDown" => tilde(6, m),
        _ => return None,
    })
}

fn function(key: &str, m: Mods) -> Option<String> {
    let plain = modifier_param(m) == 1;
    Some(match key {
        "F1" if plain => "\x1bOP".into(),
        "F1" => csi('P', m),
        "F2" if plain => "\x1bOQ".into(),
        "F2" => csi('Q', m),
        "F3" if plain => "\x1bOR".into(),
        "F3" => csi('R', m),
        "F4" if plain => "\x1bOS".into(),
        "F4" => csi('S', m),
        "F5" => tilde(15, m),
        "F6" => tilde(17, m),
        "F7" => tilde(18, m),
        "F8" => tilde(19, m),
        "F9" => tilde(20, m),
        "F10" => tilde(21, m),
        "F11" => tilde(23, m),
        "F12" => tilde(24, m),
        _ => return None,
    })
}

/// `encodeTerminalKeyInput`: `""` for a key it doesn't know.
fn encode(key: &str, m: Mods, mode: InputMode) -> String {
    if key.is_empty() {
        return String::new();
    }
    // JavaScript `key.length === 1` counts UTF-16 units.
    if key.encode_utf16().count() == 1 {
        return printable(key, m);
    }
    match key {
        "Enter" => return "\r".into(),
        "Tab" if m.shift && !m.ctrl && !m.alt => return "\x1b[Z".into(),
        "Tab" => return alt_prefix("\t".into(), m),
        "Backspace" => return alt_prefix("\x7f".into(), m),
        "Escape" => return "\x1b".into(),
        _ => {}
    }
    navigation(key, m, mode).or_else(|| function(key, m)).unwrap_or_default()
}

/// One token into the bytes to type (`literal`: the text unchanged).
pub fn resolve(token: &str, literal: bool, mode: InputMode) -> String {
    if literal {
        return token.to_string();
    }
    // `^((?:[CMS]-)*)(.+)$`: the prefixes, then at least one character.
    let mut rest = token;
    let mut m = Mods::default();
    while rest.len() > 2 && matches!(&rest[..2], "C-" | "M-" | "S-") {
        match &rest[..1] {
            "C" => m.ctrl = true,
            "M" => m.alt = true,
            _ => m.shift = true,
        }
        rest = &rest[2..];
    }
    if rest.is_empty() {
        return token.to_string();
    }
    let key = match rest {
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "BSpace" => "Backspace",
        "Space" => " ",
        other => other,
    };
    let out = encode(key, m, mode);
    if out.is_empty() {
        token.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(t: &str) -> String {
        resolve(t, false, InputMode::default())
    }

    #[test]
    fn paseo_cli_tokens_agree() {
        // Paseo's CLI table (`client/dist/terminals/index.js resolveKeyToken`).
        for (t, want) in [
            ("Enter", "\r"),
            ("Tab", "\t"),
            ("Escape", "\x1b"),
            ("Space", " "),
            ("BSpace", "\x7f"),
            ("C-c", "\x03"),
            ("C-d", "\x04"),
            ("C-z", "\x1a"),
            ("C-l", "\x0c"),
            ("C-a", "\x01"),
            ("C-e", "\x05"),
            ("echo hi", "echo hi"),
        ] {
            assert_eq!(r(t), want, "{t}");
        }
    }

    #[test]
    fn modifiers_and_navigation() {
        assert_eq!(r("Up"), "\x1b[A");
        assert_eq!(resolve("Up", false, InputMode { application_cursor: true }), "\x1bOA");
        assert_eq!(r("C-M-Left"), "\x1b[1;7D");
        assert_eq!(r("S-Tab"), "\x1b[Z");
        assert_eq!(r("M-b"), "\x1bb");
        assert_eq!(r("C-r"), "\x12");
        assert_eq!(r("S-a"), "A");
        assert_eq!(r("F1"), "\x1bOP");
        assert_eq!(r("C-F5"), "\x1b[15;5~");
        assert_eq!(r("PageDown"), "\x1b[6~");
        assert_eq!(r("C-["), "\x1b");
        assert_eq!(r("C-"), "C-");
        assert_eq!(r("C-Nope"), "C-Nope");
        assert_eq!(resolve("C-c", true, InputMode::default()), "C-c");
    }
}
