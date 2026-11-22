//! Comparing what the SDK returned with what the case expects.
//!
//! Two rules of the guide are implemented here: values are compared as JSON structures without
//! regard to object key order, and an expected error is matched through the narrowest category
//! mapping with its `message` taken as a regular expression.

use lucina_a2ui::{Error, ErrorCategory};
use serde_json::{Number, Value};

/// The keys of an `expectError` / `expect_error` object this harness checks.
///
/// The oracle's schema admits `code`, `path`, `surfaceId`, `functionCallId`, `details` and
/// `rendererError` too. They are not in the table because this harness does not check them: an
/// expectation naming one fails the case as an unknown key rather than passing without a check,
/// and the step that builds the message processor adds a key to the table together with the check
/// of it.
pub const EXPECT_ERROR_KEYS: &[&str] = &["category", "message"];

/// The failure expectation of a case or step, read from `expect_error` and then `expectError` in
/// the order the original's harness reads them.
///
/// `None` when neither key is there, which is the same as "must not fail".
pub fn expectation<'a>(holder: &'a Value, id: &str) -> Result<Option<&'a Value>, String> {
    let Some(expected) = holder
        .get("expect_error")
        .or_else(|| holder.get("expectError"))
    else {
        return Ok(None);
    };
    match expected {
        Value::Object(_) => {
            crate::support::assert_known_keys(expected, EXPECT_ERROR_KEYS, "expectError key", id)
        }
        Value::String(_) => Ok(()),
        _ => Err(format!(
            "{id}: expect_error is neither an object nor a string, it is {}",
            kind(expected)
        )),
    }?;
    Ok(Some(expected))
}

/// Checks a call the case expects to fail: it must fail, in the named category, and with a message
/// the named regular expression matches.
pub fn expect_failure<T>(
    id: &str,
    expected: &Value,
    result: Result<T, Error>,
) -> Result<(), String> {
    let error = match result {
        Ok(_) => {
            return Err(format!(
                "{id}: expected {}, but the call returned a value",
                expectation_words(expected)
            ))
        }
        Err(error) => error,
    };
    let (category, message) = match expected {
        Value::Object(object) => (
            object
                .get("category")
                .map(|name| {
                    name.as_str()
                        .ok_or_else(|| format!("{id}: expectError.category is not a string"))
                })
                .transpose()?,
            object.get("message").and_then(Value::as_str),
        ),
        Value::String(text) => (None, Some(text.as_str())),
        _ => (None, None),
    };
    if let Some(name) = category {
        let wanted = ErrorCategory::from_name(name)
            .ok_or_else(|| format!("{id}: {name} is not a category of the oracle's ExpectError"))?;
        if !error.category().satisfies(wanted) {
            return Err(format!(
                "{id}: expected {name}, found {}",
                error.category().as_str()
            ));
        }
    }
    if let Some(pattern) = message.filter(|pattern| !pattern.is_empty()) {
        let expression = regex::Regex::new(pattern)
            .map_err(|problem| format!("{id}: expectError.message is not a regex: {problem}"))?;
        if !expression.is_match(error.message()) {
            return Err(format!(
                "{id}: expected a message matching /{pattern}/, found \"{}\"",
                error.message()
            ));
        }
    }
    Ok(())
}

/// What an expectation asks for, for a message about a call that did not fail.
fn expectation_words(expected: &Value) -> String {
    match expected {
        Value::Object(object) => match object.get("category").and_then(Value::as_str) {
            Some(name) => format!("a {name} failure"),
            None => "a failure".to_string(),
        },
        _ => "a failure".to_string(),
    }
}

/// What kind of JSON value this is.
pub fn kind(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(_) => "a string".to_string(),
        Value::Array(_) => "a list".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

/// Whether an expected value and an actual one are the same, with the first difference as the
/// message when they are not.
pub fn compare(expected: &Value, actual: &Value) -> Result<(), String> {
    match first_difference(expected, actual, "") {
        None => Ok(()),
        Some(problem) => Err(problem),
    }
}

/// The first difference between an expected and an actual value, located in the expectation, or
/// `None` when the two are the same.
pub fn first_difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Number(one), Value::Number(other)) if numbers_equal(one, other) => None,
        (Value::Array(one), Value::Array(other)) => {
            if one.len() != other.len() {
                return Some(located(
                    path,
                    &format!("a list of {} entries", one.len()),
                    &format!("a list of {} entries", other.len()),
                ));
            }
            (0..one.len()).find_map(|index| {
                first_difference(&one[index], &other[index], &format!("{path}[{index}]"))
            })
        }
        (Value::Object(one), Value::Object(other)) => {
            if let Some(absent) = one.keys().find(|key| !other.contains_key(*key)) {
                return Some(located(
                    &format!("{path}/{absent}"),
                    &format!("{}", one[absent]),
                    "no such key",
                ));
            }
            if let Some(extra) = other.keys().find(|key| !one.contains_key(*key)) {
                return Some(located(
                    &format!("{path}/{extra}"),
                    "no such key",
                    &format!("{}", other[extra]),
                ));
            }
            one.iter().find_map(|(key, value)| {
                first_difference(value, other.get(key)?, &format!("{path}/{key}"))
            })
        }
        (Value::Bool(one), Value::Bool(other)) if one == other => None,
        (Value::String(one), Value::String(other)) if one == other => None,
        (Value::Null, Value::Null) => None,
        (one, other) => Some(located(path, &one.to_string(), &other.to_string())),
    }
}

/// Numbers are compared by value, not by how each side happens to be written.
///
/// The original's Python and TypeScript each hold one number type, so a `${2.5E+4}` that evaluates
/// to `25000.0` satisfies an expectation written `25000`; `expressions.yaml` relies on this in
/// `test_expr_exponential_notation_capital_e`, `test_expr_number_with_trailing_point` and
/// `test_expr_exponent_underflow_is_zero`. A port has two representations where the original has
/// one, which is why the comparison, not the parser, is where they are made equal.
fn numbers_equal(one: &Number, other: &Number) -> bool {
    match (exact_integer(one), exact_integer(other)) {
        (Some(one), Some(other)) => one == other,
        _ => one.as_f64() == other.as_f64(),
    }
}

/// The value of a number written without a fraction, which is the part the two representations
/// agree on exactly.
fn exact_integer(number: &Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn located(path: &str, expected: &str, actual: &str) -> String {
    if path.is_empty() {
        format!("expected {expected}, found {actual}")
    } else {
        format!("at {path}: expected {expected}, found {actual}")
    }
}
