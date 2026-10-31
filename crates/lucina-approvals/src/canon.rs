//! Canonical JSON, the payload hash and shell quoting for display.
//!
//! The payload hash binds an approval to the exact thing that will run or be
//! revealed: `sha256:` + hex of the canonical JSON (object keys sorted,
//! no whitespace). For a root plan the value is the list of argv arrays.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Compact JSON with object keys sorted (stable across serde_json features).
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write(&o[k], out);
            }
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// `sha256:<64 hex>` of `bytes`.
pub fn sha256_tag(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::from("sha256:");
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The payload hash of a JSON value.
pub fn payload_hash(v: &Value) -> String {
    sha256_tag(canonical_json(v).as_bytes())
}

/// The payload hash of a root plan: the canonical JSON list of argv arrays.
pub fn plan_hash(plan: &[Vec<String>]) -> String {
    payload_hash(&serde_json::json!(plan))
}

/// One argument as a POSIX shell word (for display only; nothing is ever run through a shell).
pub fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if safe {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// An argv as one shell-quoted line.
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_sorts_keys_and_is_compact() {
        let v = json!({"b": [1, {"z": null, "a": "x y"}], "a": true});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":true,"b":[1,{"a":"x y","z":null}]}"#
        );
    }

    #[test]
    fn plan_hash_depends_on_every_argument_and_order() {
        let a = vec![vec!["mkdir".to_owned(), "-p".into(), "/a b".into()]];
        let b = vec![vec![
            "mkdir".to_owned(),
            "-p".into(),
            "/a".into(),
            "b".into(),
        ]];
        assert_ne!(plan_hash(&a), plan_hash(&b));
        let two = vec![vec!["x".to_owned()], vec!["y".to_owned()]];
        let rev = vec![vec!["y".to_owned()], vec!["x".to_owned()]];
        assert_ne!(plan_hash(&two), plan_hash(&rev));
        assert!(plan_hash(&a).starts_with("sha256:") && plan_hash(&a).len() == 7 + 64);
        // Known value: sha256 of [["a"]]
        assert_eq!(
            plan_hash(&[vec!["a".to_owned()]]),
            sha256_tag(br#"[["a"]]"#)
        );
    }

    #[test]
    fn shell_quote_keeps_spaces_in_one_word() {
        assert_eq!(shell_quote("systemctl"), "systemctl");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$(x)"), "'$(x)'");
        assert_eq!(
            shell_line(&["install".into(), "-m".into(), "644".into(), "/a b/c".into()]),
            "install -m 644 '/a b/c'"
        );
    }
}
