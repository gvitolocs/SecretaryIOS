//! Schema-guided coercion of tool arguments.
//!
//! Models emit `"42"` for an integer, `"true"` for a boolean, a JSON-encoded
//! string for an array or object (also nested inside containers), or a bare
//! value where an array is expected. Before [`crate::Registry::validate`]
//! refuses such a call, the arguments are repaired against the tool's
//! parameters schema here. Ported from Hermes Agent's
//! `tools/arg_coercion.py` (MIT, Copyright (c) 2025 Nous Research):
//! coercion is conservative — the original value is kept whenever a repair is
//! not unambiguous.

use serde_json::Value;

/// Coerce the top-level properties of an object `args` against the tool's
/// `parameters` schema. Only keys with a property schema are visited; a
/// non-object `args` is returned unchanged. Pure: no panics on any input.
pub fn coerce_args(parameters: &Value, args: Value) -> Value {
    let Some(props) = parameters.get("properties").and_then(Value::as_object) else {
        return args;
    };
    let Some(obj) = args.as_object() else {
        return args;
    };
    let mut out = obj.clone();
    for (key, value) in obj.iter() {
        let Some(prop) = props.get(key).and_then(|p| resolve_ref(p, parameters)) else {
            continue;
        };
        let expected = prop.get("type").and_then(Value::as_str);
        let new = match value {
            Value::String(s) => {
                if expected == Some("array") {
                    // A bare value for an array schema: a JSON-encoded array
                    // (or a nullable "null") is used as is, anything else is
                    // wrapped in a one-element list.
                    match coerce_value(s, prop, parameters) {
                        Some(c) => Some(normalize_container(&c, prop, parameters)),
                        None => Some(normalize(
                            &Value::Array(vec![value.clone()]),
                            prop,
                            parameters,
                        )),
                    }
                } else {
                    coerce_value(s, prop, parameters)
                        .map(|c| normalize_container(&c, prop, parameters))
                }
            }
            Value::Array(_) if expected == Some("array") => {
                Some(normalize(value, prop, parameters))
            }
            Value::Object(_) if expected == Some("object") => {
                Some(normalize(value, prop, parameters))
            }
            v if expected == Some("array") && !v.is_null() => Some(Value::Array(vec![v.clone()])),
            _ => None,
        };
        if let Some(new) = new {
            out.insert(key.clone(), new);
        }
    }
    Value::Object(out)
}

/// Coerce the string `s` to the schema's expected type; `None` keeps the
/// original (Hermes `_coerce_value`). A `"null"` string becomes null when the
/// schema allows null; `type` lists and anyOf/oneOf branches are tried in
/// order, the first change wins.
fn coerce_value(s: &str, schema: &Value, root: &Value) -> Option<Value> {
    if allows_null(schema, root) && s.trim().eq_ignore_ascii_case("null") {
        return Some(Value::Null);
    }
    for t in type_candidates(schema, root) {
        let coerced = match t.as_str() {
            "integer" => coerce_number(s, true),
            "number" => coerce_number(s, false),
            "boolean" => coerce_boolean(s),
            "array" => coerce_json(s, true),
            "object" => coerce_json(s, false),
            "null" if s.trim().eq_ignore_ascii_case("null") => Some(Value::Null),
            _ => None,
        };
        if coerced.is_some() {
            return coerced;
        }
    }
    None
}

/// The expected types of a schema, in order: `type` (a string or a list), then
/// the `type` of each anyOf/oneOf branch.
fn type_candidates(schema: &Value, root: &Value) -> Vec<String> {
    fn push(t: &Value, out: &mut Vec<String>) {
        match t {
            Value::String(s) => out.push(s.clone()),
            Value::Array(list) => {
                out.extend(list.iter().filter_map(|v| v.as_str().map(str::to_owned)))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(t) = schema.get("type") {
        push(t, &mut out);
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(Value::as_array) {
            for b in branches {
                if let Some(b) = resolve_ref(b, root) {
                    if let Some(t) = b.get("type") {
                        push(t, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// True when the schema explicitly permits null: `type` "null" or a list
/// containing it, `nullable: true`, or an anyOf/oneOf `{"type": "null"}`
/// branch (Hermes `_schema_allows_null`).
fn allows_null(schema: &Value, root: &Value) -> bool {
    let Some(s) = resolve_ref(schema, root) else {
        return false;
    };
    if let Some(t) = s.get("type") {
        match t {
            Value::String(x) if x == "null" => return true,
            Value::Array(list) if list.iter().any(|v| v.as_str() == Some("null")) => return true,
            _ => {}
        }
    }
    if s.get("nullable").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    ["anyOf", "oneOf"].iter().any(|key| {
        s.get(key)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches.iter().any(|b| {
                    resolve_ref(b, root)
                        .and_then(|b| b.get("type"))
                        .and_then(Value::as_str)
                        == Some("null")
                })
            })
    })
}

/// Parse `s` as a number; `None` on failure, on non-finite values (NaN/inf are
/// not JSON), or on a fraction when `integer_only`. Whole numbers keep the
/// integer form (`"5"` -> 5) (Hermes `_coerce_number`).
fn coerce_number(s: &str, integer_only: bool) -> Option<Value> {
    let f: f64 = s.trim().parse().ok()?;
    if !f.is_finite() {
        return None;
    }
    if f == f.trunc() {
        const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0; // 2^63
        const U64_LIMIT: f64 = 18_446_744_073_709_551_616.0; // 2^64
        if (-I64_LIMIT..I64_LIMIT).contains(&f) {
            return Some(Value::from(f as i64));
        }
        if (0.0..U64_LIMIT).contains(&f) {
            return Some(Value::from(f as u64));
        }
        return serde_json::Number::from_f64(f).map(Value::Number);
    }
    if integer_only {
        None
    } else {
        serde_json::Number::from_f64(f).map(Value::Number)
    }
}

/// `"true"`/`"false"` (trimmed, case-insensitive) to a bool (Hermes
/// `_coerce_boolean`).
fn coerce_boolean(s: &str) -> Option<Value> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// json-parse `s` when the schema expects an array/object (Hermes
/// `_coerce_json`); the original string on any mismatch.
fn coerce_json(s: &str, want_array: bool) -> Option<Value> {
    let parsed: Value = serde_json::from_str(s).ok()?;
    match (&parsed, want_array) {
        (Value::Array(_), true) | (Value::Object(_), false) => Some(parsed),
        _ => None,
    }
}

/// Recursively parse JSON-encoded strings where the schema expects an array or
/// object (Hermes `_normalize_json_strings_for_schema`). Strings whose schema
/// is `string` stay strings even when they look like JSON.
fn normalize(value: &Value, schema: &Value, root: &Value) -> Value {
    let Some(schema) = resolve_ref(schema, root) else {
        return value.clone();
    };
    let mut value = value.clone();
    if let Value::String(s) = &value {
        let trimmed = s.trim();
        let expects_array = accepts_kind(schema, "array", root, 0);
        let expects_object = accepts_kind(schema, "object", root, 0);
        if (expects_array && trimmed.starts_with('['))
            || (expects_object && trimmed.starts_with('{'))
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                if (parsed.is_array() && expects_array) || (parsed.is_object() && expects_object) {
                    value = parsed;
                }
            }
        }
    }
    match &value {
        Value::Array(items) => {
            let Some(items_schema) = schema.get("items").and_then(|i| resolve_ref(i, root)) else {
                return value;
            };
            Value::Array(
                items
                    .iter()
                    .map(|item| normalize(item, items_schema, root))
                    .collect(),
            )
        }
        Value::Object(map) => {
            let Some(props) = schema.get("properties").and_then(Value::as_object) else {
                return value;
            };
            let mut out = map.clone();
            for (k, prop_schema) in props {
                if let Some(v) = map.get(k) {
                    if let Some(ps) = resolve_ref(prop_schema, root) {
                        out.insert(k.clone(), normalize(v, ps, root));
                    }
                }
            }
            Value::Object(out)
        }
        _ => value,
    }
}

/// Normalize a freshly coerced value when it became a container.
fn normalize_container(value: &Value, prop: &Value, root: &Value) -> Value {
    match value {
        Value::Array(_) | Value::Object(_) => normalize(value, prop, root),
        _ => value.clone(),
    }
}

/// True when `schema` permits the JSON type `kind` via `type` or an
/// anyOf/oneOf/allOf branch (Hermes `_schema_accepts_kind`).
fn accepts_kind(schema: &Value, kind: &str, root: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    let Some(schema) = resolve_ref(schema, root) else {
        return false;
    };
    if let Some(t) = schema.get("type") {
        match t {
            Value::String(s) if s == kind => return true,
            Value::Array(list) if list.iter().any(|v| v.as_str() == Some(kind)) => return true,
            _ => {}
        }
    }
    ["anyOf", "oneOf", "allOf"].iter().any(|key| {
        schema
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches
                    .iter()
                    .any(|b| accepts_kind(b, kind, root, depth + 1))
            })
    })
}

/// Follow a document-local `$ref` (`#/$defs/X`, `#/definitions/X`, any `#/…`
/// JSON pointer) against `root`. `None`: not an object schema, or a `$ref`
/// that does not resolve to an object — the caller leaves such values alone.
fn resolve_ref<'a>(schema: &'a Value, root: &'a Value) -> Option<&'a Value> {
    let obj = schema.as_object()?;
    let Some(r) = obj.get("$ref").and_then(Value::as_str) else {
        return Some(schema);
    };
    if !r.starts_with("#/") {
        return None;
    }
    let mut target = root;
    for token in r[2..].split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        target = match target {
            Value::Object(map) => map.get(&token)?,
            Value::Array(list) => list.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    target.as_object().map(|_| target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn coerce(props: Value, args: Value) -> Value {
        coerce_args(&json!({"type": "object", "properties": props}), args)
    }

    #[test]
    fn integer_and_number_strings() {
        assert_eq!(
            coerce(json!({"n": {"type": "integer"}}), json!({"n": "42"})),
            json!({"n": 42})
        );
        assert_eq!(
            coerce(json!({"n": {"type": "integer"}}), json!({"n": "4.5"})),
            json!({"n": "4.5"})
        );
        assert_eq!(
            coerce(json!({"n": {"type": "integer"}}), json!({"n": "-7"})),
            json!({"n": -7})
        );
        assert_eq!(
            coerce(json!({"n": {"type": "number"}}), json!({"n": "4.5"})),
            json!({"n": 4.5})
        );
        // whole numbers keep the integer form
        assert_eq!(
            coerce(json!({"n": {"type": "number"}}), json!({"n": "5"})),
            json!({"n": 5})
        );
    }

    #[test]
    fn non_finite_numbers_stay_strings() {
        for s in ["NaN", "nan", "inf", "-inf", "Infinity"] {
            assert_eq!(
                coerce(json!({"n": {"type": "number"}}), json!({"n": s})),
                json!({"n": s})
            );
            assert_eq!(
                coerce(json!({"n": {"type": "integer"}}), json!({"n": s})),
                json!({"n": s})
            );
        }
    }

    #[test]
    fn boolean_strings() {
        assert_eq!(
            coerce(json!({"b": {"type": "boolean"}}), json!({"b": "TRUE"})),
            json!({"b": true})
        );
        assert_eq!(
            coerce(json!({"b": {"type": "boolean"}}), json!({"b": " False "})),
            json!({"b": false})
        );
        assert_eq!(
            coerce(json!({"b": {"type": "boolean"}}), json!({"b": "yes"})),
            json!({"b": "yes"})
        );
    }

    #[test]
    fn array_values() {
        let p = json!({"a": {"type": "array"}});
        assert_eq!(
            coerce(p.clone(), json!({"a": "[1,2]"})),
            json!({"a": [1, 2]})
        );
        assert_eq!(coerce(p.clone(), json!({"a": "x"})), json!({"a": ["x"]}));
        assert_eq!(coerce(p.clone(), json!({"a": 3})), json!({"a": [3]}));
        assert_eq!(coerce(p.clone(), json!({"a": null})), json!({"a": null}));
        assert_eq!(
            coerce(p.clone(), json!({"a": [1, "x"]})),
            json!({"a": [1, "x"]})
        );
        // a nullable array takes "null" as null, not ["null"]
        let pn = json!({"a": {"type": ["array", "null"]}});
        assert_eq!(coerce(pn, json!({"a": "null"})), json!({"a": null}));
    }

    #[test]
    fn object_values() {
        assert_eq!(
            coerce(json!({"o": {"type": "object"}}), json!({"o": "{\"a\":1}"})),
            json!({"o": {"a": 1}})
        );
        assert_eq!(
            coerce(json!({"o": {"type": "object"}}), json!({"o": "x"})),
            json!({"o": "x"})
        );
    }

    #[test]
    fn null_strings() {
        assert_eq!(
            coerce(
                json!({"s": {"type": ["string", "null"]}}),
                json!({"s": "null"})
            ),
            json!({"s": null})
        );
        assert_eq!(
            coerce(json!({"s": {"type": "string"}}), json!({"s": "null"})),
            json!({"s": "null"})
        );
        assert_eq!(
            coerce(
                json!({"s": {"type": "string", "nullable": true}}),
                json!({"s": "NULL"})
            ),
            json!({"s": null})
        );
        assert_eq!(
            coerce(
                json!({"s": {"anyOf": [{"type": "string"}, {"type": "null"}]}}),
                json!({"s": "null"})
            ),
            json!({"s": null})
        );
    }

    #[test]
    fn type_lists_try_each_in_order() {
        assert_eq!(
            coerce(
                json!({"n": {"type": ["integer", "string"]}}),
                json!({"n": "7"})
            ),
            json!({"n": 7})
        );
        assert_eq!(
            coerce(
                json!({"n": {"type": ["integer", "string"]}}),
                json!({"n": "x"})
            ),
            json!({"n": "x"})
        );
    }

    #[test]
    fn nested_containers_parse_json_strings_only_where_expected() {
        let props = json!({"rows": {
            "type": "array",
            "items": {"type": "object", "properties": {
                "tags": {"type": "array"},
                "name": {"type": "string"}
            }}
        }});
        assert_eq!(
            coerce(props, json!({"rows": [{"tags": "[\"a\"]", "name": "[1]"}]})),
            json!({"rows": [{"tags": ["a"], "name": "[1]"}]})
        );
    }

    #[test]
    fn local_refs_are_resolved() {
        let parameters = json!({
            "type": "object",
            "properties": {"n": {"$ref": "#/$defs/N"}, "m": {"$ref": "#/definitions/M"}},
            "$defs": {"N": {"type": "integer"}},
            "definitions": {"M": {"type": "boolean"}}
        });
        assert_eq!(
            coerce_args(&parameters, json!({"n": "42", "m": "true"})),
            json!({"n": 42, "m": true})
        );
        // an unknown ref leaves the value alone
        let parameters = json!({
            "type": "object",
            "properties": {"n": {"$ref": "#/$defs/Missing"}}
        });
        assert_eq!(
            coerce_args(&parameters, json!({"n": "42"})),
            json!({"n": "42"})
        );
    }

    #[test]
    fn non_object_args_and_unknown_keys_are_unchanged() {
        let parameters = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        for args in [json!([1, 2]), json!("x"), json!(null), json!(42), json!({})] {
            assert_eq!(coerce_args(&parameters, args.clone()), args);
        }
        assert_eq!(
            coerce_args(&parameters, json!({"n": "1", "other": "x"})),
            json!({"n": 1, "other": "x"})
        );
        // no properties at all
        assert_eq!(
            coerce_args(&json!({"type": "object"}), json!({"n": "1"})),
            json!({"n": "1"})
        );
    }
}
