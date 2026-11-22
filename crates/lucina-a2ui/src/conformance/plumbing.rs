//! The rules the shared harness plumbing is written to, checked by name.
//!
//! These are the guide's section "Shared harness plumbing" 1 and 2: what a suite file means once it
//! is loaded, what a key no handler declares does, how an expected error is matched and how an
//! `UNSUPPORTED` entry is counted. The suites themselves are run in `core_suites.rs`.

use super::expect::kind;
use super::loader::{from_yaml, load_suite, Case};
use super::{assert_known_keys, compare, expect_failure, expectation, run_suite, METADATA_KEYS};
use crate::Error;
use serde_json::{json, Value};

/// The guide, 1: "Older YAML 1.1 parsers convert unquoted words such as `yes`, `no`, `on`, or `off`
/// into booleans", which would corrupt the input a case states.
#[test]
fn plain_words_that_look_like_booleans_stay_strings() {
    let loaded = from_yaml("a: yes\nb: no\nc: on\nd: off\ne: True\nf: true\n").unwrap();
    assert_eq!(
        loaded["a"],
        json!("yes"),
        "an unquoted `yes` became something else"
    );
    assert_eq!(loaded["b"], json!("no"));
    assert_eq!(loaded["c"], json!("on"));
    assert_eq!(loaded["d"], json!("off"));
    assert_eq!(
        loaded["e"],
        json!("True"),
        "only the exact `true` is a boolean"
    );
    assert_eq!(loaded["f"], json!(true), "`true` itself is the boolean");
}

/// The guide, 1: a YAML 1.1 parser "parse[s] colon-separated digits as base-60 numbers, which
/// corrupts test inputs".
#[test]
fn colon_separated_digits_stay_a_string() {
    let loaded = from_yaml("time: 1:30\nlist: [1:30]\n").unwrap();
    assert_eq!(loaded["time"], json!("1:30"));
    assert_eq!(loaded["list"], json!(["1:30"]));
}

/// The guide, 1: "Float literals are written with a decimal point and a signed exponent
/// (`1.0e+308`)", so that loader and suite agree it is a number.
#[test]
fn a_written_float_is_a_float() {
    let loaded = from_yaml("big: 1.0e+308\nsmall: -2.5\nwhole: 42\n").unwrap();
    assert_eq!(loaded["big"], json!(1.0e+308));
    assert_eq!(loaded["small"], json!(-2.5));
    assert_eq!(loaded["whole"], json!(42));
    assert!(
        loaded["big"].is_number(),
        "1.0e+308 loaded as {}",
        kind(&loaded["big"])
    );
}

/// The guide, 1: "Compare JSON structures without regard to object key order."
#[test]
fn object_key_order_does_not_matter() {
    let one = from_yaml("root:\n  a: 1\n  b:\n    c: 2\n").unwrap();
    let other = from_yaml("root:\n  b:\n    c: 2\n  a: 1\n").unwrap();
    compare(&one, &other).unwrap_or_else(|problem| panic!("key order was compared: {problem}"));
    compare(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1}))
        .expect("the same entries written in another order are the same object");
    compare(
        &json!({"x": [1, {"p": 1, "q": 2}]}),
        &json!({"q": 2, "p": 1}),
    )
    .expect_err("a key the expectation holds cannot be missing from the answer");
}

/// The same value written as an integer and as a float is one value to the original: its Python
/// and TypeScript each hold one number type. See `expect::numbers_equal`.
#[test]
fn an_integer_equals_a_float_of_its_value() {
    compare(&json!(25_000), &json!(25_000.0)).unwrap();
    compare(&json!([1, 2.5]), &json!([1, 2.5])).unwrap();
    let problem = compare(&json!([1]), &json!([1.5])).unwrap_err();
    assert!(problem.contains("[0]"), "the path is not in: {problem}");
    assert!(compare(&json!(9_007_199_254_740_991_i64), &json!(1)).is_err());
}

/// The guide, 2.2.1: an unknown key fails the case, and names the kind, the key and the case.
#[test]
fn a_key_no_handler_declares_fails_the_case() {
    let case = json!({"name": "test_x", "description": "d", "features": [], "surprise": 1});
    let problem = assert_known_keys(&case, METADATA_KEYS, "case key", "core/x.yaml#test_x")
        .expect_err("an unknown key must fail the case");
    assert_eq!(problem, "Unknown case key 'surprise' in core/x.yaml#test_x");
    assert_known_keys(
        &json!({"name": "test_x"}),
        METADATA_KEYS,
        "case key",
        "core/x.yaml#test_x",
    )
    .expect("the metadata keys are always accepted");
}

/// The guide, 3: "Core suites … write both key names, so a Core runner must check both keys".
#[test]
fn both_error_expectation_key_names_are_read() {
    for key in ["expect_error", "expectError"] {
        let holder = json!({key: {"category": "DataError"}});
        let found = expectation(&holder, "core/x.yaml#test_y").unwrap();
        assert_eq!(
            found,
            Some(&json!({"category": "DataError"})),
            "{key} was not read"
        );
    }
    let both = json!({"expect_error": "a", "expectError": "b"});
    assert_eq!(
        expectation(&both, "core/x.yaml#test_y").unwrap(),
        Some(&json!("a")),
        "the snake_case name the original's harness reads first must win"
    );
    assert_eq!(expectation(&json!({}), "core/x.yaml#test_y").unwrap(), None);
    let unknown = json!({"expectError": {"category": "DataError", "rendererError": {}}});
    let problem = expectation(&unknown, "core/x.yaml#test_y").expect_err("an unchecked key");
    assert_eq!(
        problem,
        "Unknown expectError key 'rendererError' in core/x.yaml#test_y"
    );
}

/// The guide, 3: "Map each category to the narrowest class or classes that satisfy it."
#[test]
fn a_category_is_matched_through_the_narrowest_mapping() {
    use crate::ErrorCategory;
    let integrity = Error::new(ErrorCategory::IntegrityError, "Surface not found: main");
    let expression = Error::expression("Unclosed interpolation: missing '}'");
    let data = Error::data("Path '/a' is not a pointer");

    expect_failure(
        "core/x.yaml#test_z",
        &json!({"category": "ValidationError"}),
        failed(integrity),
    )
    .expect("an integrity failure satisfies a ValidationError expectation");
    expect_failure(
        "core/x.yaml#test_z",
        &json!({"category": "ParseError"}),
        failed(expression),
    )
    .expect("a template that does not parse satisfies a ParseError expectation");
    expect_failure(
        "core/x.yaml#test_z",
        &json!({"category": "ValidationError"}),
        failed(data.clone()),
    )
    .expect_err("a data failure does not satisfy a ValidationError expectation");
    expect_failure(
        "core/x.yaml#test_z",
        &json!({"category": "ExpressionError"}),
        Ok(()),
    )
    .expect_err("a call that returns a value did not fail");
    expect_failure(
        "core/x.yaml#test_z",
        &json!({"category": "NoSuchError"}),
        failed(data),
    )
    .expect_err("a category the oracle does not name is not a pass");
}

/// The guide, 3: "When a case also includes a `message` string, match it as a regular expression."
#[test]
fn an_expected_message_is_a_regular_expression() {
    let error = Error::data("Cannot set path '/a/b': segment 'a' is a primitive value.");
    expect_failure(
        "core/x.yaml#test_m",
        &json!({"message": "Cannot set.*primitive"}),
        failed(error.clone()),
    )
    .expect("a pattern no substring of the message is has to match");
    expect_failure(
        "core/x.yaml#test_m",
        &json!({"message": "^primitive"}),
        failed(error),
    )
    .expect_err("an anchored pattern that does not match is a failure");
    expect_failure(
        "core/x.yaml#test_m",
        &json!("segment 'a'"),
        Ok::<(), Error>(()),
    )
    .expect_err("a call that must fail returned a value");
}

/// A call that failed, which is what an `expect_error` case asks the SDK to do.
fn failed(error: Error) -> Result<(), Error> {
    Err(error)
}

/// A case of the oracle, loaded whole: this is what the suite tests run against.
#[test]
fn the_three_core_suites_load() {
    let counts = [
        ("core/data_model.yaml", 52),
        ("core/data_context.yaml", 11),
        ("core/expressions.yaml", 98),
    ];
    for (suite, expected) in counts {
        let cases = load_suite(suite).unwrap_or_else(|problem| panic!("{suite}: {problem}"));
        assert_eq!(
            cases.len(),
            expected,
            "{suite} loaded a different number of cases"
        );
        for case in &cases {
            assert!(
                case.id.starts_with(&format!("{suite}#")),
                "{}: odd id",
                case.id
            );
            assert!(
                case.id.starts_with(&format!("{suite}#")) && case.id.len() > suite.len() + 1,
                "{suite}: a case without a name"
            );
            assert!(case.json.is_object(), "{}: not an object", case.id);
        }
    }
    let expressions = load_suite("core/expressions.yaml").unwrap();
    let actions = expressions
        .iter()
        .map(|case| case.action().unwrap_or("").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        actions
            .iter()
            .filter(|action| *action == "parse_expression_template")
            .count(),
        92
    );
    assert_eq!(
        actions
            .iter()
            .filter(|action| *action == "validate")
            .count(),
        6
    );
}

/// The guide, 2.2.2: a matching `UNSUPPORTED` entry skips its cases, never passes them, and the
/// action itself is the key.
#[test]
fn an_unsupported_action_skips_its_cases() {
    let summary = run_suite(
        "core/expressions.yaml",
        &[],
        &[("validate", "message processor: not built here")],
    );
    assert_eq!(
        summary.skipped.len(),
        6,
        "the `validate` cases were not all skipped"
    );
    assert!(summary
        .skipped
        .iter()
        .all(|(_, reason)| reason == "message processor: not built here"));
    assert!(
        summary.unused.is_empty(),
        "the entry matched nothing: {:?}",
        summary.unused
    );
    assert_eq!(
        summary.passed, 0,
        "a skipped case must never count as passed"
    );
    assert_eq!(
        summary.failures.len(),
        92,
        "the rest of the suite still needs a handler, unknown rather than ignored"
    );
    assert!(summary.failures.iter().all(|(_, problem)| problem
        .contains("Unknown action 'parse_expression_template' in core/expressions.yaml#")));
}

/// The guide, 2.2.2: the key may also name a path of keys under the action.
#[test]
fn an_unsupported_key_path_matches_only_cases_holding_it() {
    let cases = load_suite("core/data_model.yaml").unwrap();
    let with_initial = cases
        .iter()
        .filter(|case| case.get("initial").is_some())
        .count();
    assert!(
        with_initial > 0 && with_initial < cases.len(),
        "the fixture is one-sided: every case holds `initial` or none does"
    );
    let summary = run_suite(
        "core/data_model.yaml",
        &[("data_model", always_pass)],
        &[("data_model.initial", "a key path, not an action")],
    );
    assert_eq!(summary.skipped.len(), with_initial);
    assert_eq!(summary.passed, cases.len() - with_initial);
    assert!(summary.unused.is_empty());
}

/// A handler that claims every case holds, to see what the dispatcher did with them.
fn always_pass(_case: &Case) -> Result<(), String> {
    Ok(())
}

/// The guide, 2.2.2: an entry that matches no case fails the run.
#[test]
fn an_unsupported_entry_that_matches_no_case_is_reported() {
    let summary = run_suite(
        "core/data_context.yaml",
        &[],
        &[("no_such_action", "nothing")],
    );
    assert_eq!(summary.unused, vec!["no_such_action".to_string()]);
}

#[test]
#[should_panic(expected = "matched no case")]
fn an_unsupported_entry_that_matches_no_case_fails_the_run() {
    let summary = run_suite(
        "core/data_context.yaml",
        &[("resolve_path", always_pass)],
        &[("no_such_action", "nothing")],
    );
    summary.report(11, 0);
}

/// The guide, 2.1: a suite that loads no case fails rather than passing nothing.
#[test]
#[should_panic(expected = "cannot read suite")]
fn a_missing_suite_file_is_not_an_empty_suite() {
    run_suite("core/no_such_suite.yaml", &[], &[]);
}

#[test]
fn a_case_without_an_action_fails_and_is_named() {
    let case = Case {
        id: "core/x.yaml#test_n".to_string(),
        json: Value::Object(serde_json::Map::new()),
    };
    assert_eq!(case.action(), None);
    assert_eq!(case.id, "core/x.yaml#test_n");
}
