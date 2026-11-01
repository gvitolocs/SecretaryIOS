//! Redaction for notes: Lucina's one redactor ([`lucina_secrets::redactor`],
//! tokens, keys, credentials, high-entropy strings) plus what a note about
//! devices must not carry either: Wi-Fi PSKs, serial numbers, IMEIs, device
//! ids and MAC addresses. Applied to every item before it is queued and to
//! every model answer before it is written.

use std::sync::OnceLock;

use regex::Regex;

fn extra() -> &'static [(Regex, &'static str)] {
    static R: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    R.get_or_init(|| {
        let d = |re: &str, label| (Regex::new(re).expect("note redaction regex"), label);
        vec![
            // `psk=…`, `wpa_psk: …`, `PSK …`
            d(
                r#"(?i)\b(?:wpa[_-]?)?psk\b["']?\s*[:=]?\s*["']?[^\s"',;]{6,}"#,
                "[redacted:psk]",
            ),
            // `serial: G000…`, `serial number ABC…`, `S/N …`, `imei …`, `udid …`, `ecid …`
            d(
                r#"(?i)\b(?:serial(?:[ _-]?(?:no|number|num))?|s/n|imei|meid|udid|ecid)\b["']?\s*[:=#]?\s*["']?[A-Za-z0-9-]{6,}"#,
                "[redacted:serial]",
            ),
            // Apple UDIDs (`00008140-001225CE1A03801C`).
            d(r"\b0000[0-9A-Fa-f]{4}-[0-9A-Fa-f]{16}\b", "[redacted:serial]"),
            // Kindle serials (16 characters starting with G0 / B0).
            d(r"\b[GB]0[0-9A-Z]{14}\b", "[redacted:serial]"),
            // Upper-case UUIDs are device ids (devicectl, adb); lower-case ones
            // are ordinary record and session ids and stay.
            d(
                r"\b[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}\b",
                "[redacted:device-id]",
            ),
            // 15-digit IMEIs standing alone.
            d(r"\b\d{15}\b", "[redacted:serial]"),
            d(r"\b(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b", "[redacted:mac]"),
        ]
    })
}

/// The text with every secret replaced. Idempotent.
pub fn redact(s: &str) -> String {
    let mut out = lucina_secrets::redactor().redact(s).into_owned();
    for (re, label) in extra() {
        if re.is_match(&out) {
            out = re.replace_all(&out, *label).into_owned();
        }
    }
    out
}

/// Whether the text still holds something the redactor would remove.
pub fn has_secret(s: &str) -> bool {
    lucina_secrets::redactor().contains_secret(s) || extra().iter().any(|(re, _)| re.is_match(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_secrets_go() {
        for (input, gone) in [
            ("wifi psk=hunter2hunter2 on the AP", "hunter2hunter2"),
            ("WPA_PSK: \"correcthorse\"", "correcthorse"),
            (
                "Kindle serial: G000AB1234567890 unbricked",
                "G000AB1234567890",
            ),
            ("the board's serial number XK29384756 died", "XK29384756"),
            ("iPhone UDID 00008140-001225CE1A03801C", "001225CE1A03801C"),
            ("device FA374B43-B705-5A50-8628-A175A5CA4201", "FA374B43"),
            ("imei 356938035643809", "356938035643809"),
            ("mac 3c:22:fb:12:34:56", "3c:22:fb"),
            ("token ghp_abcdefghijklmnopqrstuvwxyz0123456789", "ghp_"),
            ("password: hunter2hunter2", "hunter2hunter2"),
        ] {
            assert!(has_secret(input), "{input}");
            let out = redact(input);
            assert!(!out.contains(gone), "{input} -> {out}");
            assert_eq!(redact(&out), out, "idempotent");
        }
    }

    #[test]
    fn ordinary_text_stays() {
        for s in [
            "lucina-ops writes the store before the vault (commit 372dec64)",
            "record 3f2a9c0e1b7d4a55 in session 0b9e4c1a-5d7f-4e2b-9c3a-1f2e3d4c5b6a",
            "kernel 5.10 boots on the PW3",
        ] {
            assert_eq!(redact(s), s);
        }
    }
}
