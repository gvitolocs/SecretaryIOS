//! Fields out of the server's small, flat JSON replies (`/v1/session/qr*`).
//! Not a JSON parser: it finds `"key": value` at any depth and gives up on
//! escaped strings, which none of the fields it is used for contain.

/// The string value of the first `"key"`, unescaped strings only.
pub fn str_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = value_after(body, key)?;
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let value = &rest[..end];
    (!value.contains('\\')).then_some(value)
}

/// The non-negative integer value of the first `"key"`.
pub fn u64_field(body: &str, key: &str) -> Option<u64> {
    let rest = value_after(body, key)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The raw text of the first `"key": {…}` object value (braces balanced; strings
/// inside it must not contain braces, true for the timing objects it reads).
pub fn object_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = value_after(body, key)?;
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn value_after<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = body[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        from = end;
        let quoted = start > 0
            && body.as_bytes()[start - 1] == b'"'
            && body.as_bytes().get(end) == Some(&b'"');
        if !quoted {
            continue;
        }
        let rest = body[end + 1..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            return Some(rest.trim_start());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const QR: &str = r#"{"code":"abc-DEF_1","shortCode":"123456","pollToken":"tok_xyz","approveUrl":"https://getsecretary.tech/approve?c=abc","expiresIn": 120}"#;

    #[test]
    fn reads_fields() {
        assert_eq!(str_field(QR, "shortCode"), Some("123456"));
        assert_eq!(str_field(QR, "pollToken"), Some("tok_xyz"));
        assert_eq!(u64_field(QR, "expiresIn"), Some(120));
        assert_eq!(str_field(QR, "missing"), None);
    }

    #[test]
    fn matches_whole_keys_only() {
        let body = r#"{"xstatus":"no","status" : "approved","token":"sec_1"}"#;
        assert_eq!(str_field(body, "status"), Some("approved"));
        assert_eq!(str_field(body, "token"), Some("sec_1"));
    }

    #[test]
    fn a_key_inside_a_value_is_skipped() {
        let body = r#"{"note":"status","status":"pending"}"#;
        assert_eq!(str_field(body, "status"), Some("pending"));
    }

    #[test]
    fn reads_object_fields() {
        let done = r#"{"type":"done","ms":{"stt":120,"lucina_first":900,"x":{"y":1}},"z":2}"#;
        assert_eq!(
            object_field(done, "ms"),
            Some(r#"{"stt":120,"lucina_first":900,"x":{"y":1}}"#)
        );
        assert_eq!(object_field(done, "type"), None);
        assert_eq!(object_field(r#"{"ms":{"a":1"#, "ms"), None);
    }

    #[test]
    fn refuses_escapes_and_wrong_types() {
        assert_eq!(str_field(r#"{"a":"x\"y"}"#, "a"), None);
        assert_eq!(str_field(r#"{"a":12}"#, "a"), None);
        assert_eq!(u64_field(r#"{"a":"12"}"#, "a"), None);
    }
}
