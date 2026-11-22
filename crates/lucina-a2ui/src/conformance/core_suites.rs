//! The Core SDK suites of stage C1 that step 2a of the port answers: the reactive data model,
//! scoped path resolution, and the expression template parser.
//!
//! One test per suite file of the vendored oracle. Each prints a single summary line and fails
//! with the case names and their first difference. The plumbing that reads the files and judges the
//! answers is in the module above; what is here is the three handlers, each with the table of keys
//! it declares next to it, exactly as the guide's section "Core SDK harness" describes them.
//!
//! Protocol versions: every case of `core/data_model.yaml` (`data_model.yaml:49` and its 51
//! siblings) and of `core/data_context.yaml` (`data_context.yaml:23` and its 10 siblings) declares
//! `v0.9`, and this crate implements `1.0`. Those cases run here; they are not filtered out,
//! because in the original there is one data model and one path resolution for every version and
//! neither reads a version:
//!
//! - `python/a2ui_core/src/a2ui/core/state/data_model.py:33-351` is the whole `DataModel` class.
//!   Its constructor takes data and nothing else, and no method of it branches on a protocol
//!   version.
//! - `python/a2ui_core/src/a2ui/core/resolution/data_context.py:640-647` is
//!   `DataContext.resolve_path`, three joins and no version in sight.
//! - The TypeScript core keeps one copy of each too: `typescript/core/web_core/src/state/
//!   data-model.ts:84-495` and `typescript/core/web_core/src/resolution/data-context.ts:1737-1751`.
//!   Its `v0_8/`, `v0_9/` and `v1_0/` directories hold schemas, catalogues and message shapes, not
//!   a second data model or a second resolver — only the pre-blueprint
//!   `typescript/core/web_core/src/v0_8/data/model-processor.ts:215-227` has an old `resolvePath`
//!   of its own, and no case of these two suites is labelled v0.8.
//!
//! The version filter of the guide therefore starts with the actions that read a message
//! envelope, which is step 2b.

use super::expect::kind;
use super::loader::Case;
use super::{
    assert_known_case_keys, assert_known_keys, compare, expect_failure, expectation, run_suite,
};
use crate::data_context::ScopePath;
use crate::data_model::{DataModel, WatchId};
use crate::expression::parse_template;
use crate::expression::TemplatePart;
use crate::Error;
use serde_json::{json, Value};

const DATA_MODEL: &str = "core/data_model.yaml";
const DATA_CONTEXT: &str = "core/data_context.yaml";
const EXPRESSIONS: &str = "core/expressions.yaml";

/// The one escape this step uses: the six `validate` cases of `expressions.yaml` put a template
/// through a message envelope, and the message processor is step 2b.
const UNSUPPORTED: &[(&str, &str)] = &[("validate", "message processor: step 2b")];

#[test]
fn data_model_suite() {
    let summary = run_suite(DATA_MODEL, &[("data_model", handle_data_model)], &[]);
    summary.report(52, 0);
}

#[test]
fn data_context_suite() {
    let summary = run_suite(DATA_CONTEXT, &[("resolve_path", handle_resolve_path)], &[]);
    summary.report(11, 0);
}

#[test]
fn expressions_suite() {
    let summary = run_suite(
        EXPRESSIONS,
        &[(
            "parse_expression_template",
            handle_parse_expression_template,
        )],
        UNSUPPORTED,
    );
    summary.report(92, 6);
}

// ------------------------------------------------------------------ data_model

/// The keys the `data_model` action states, beside the metadata the runner accepts for every
/// case. The original's `_DATA_MODEL_CASE_KEYS`.
const DATA_MODEL_CASE_KEYS: &[&str] = &["action", "catalog", "expect", "initial", "steps", "watch"];

/// The keys of a `get` step. The original's `_DATA_MODEL_STEP_KEYS_BY_OP["get"]`.
const GET_STEP_KEYS: &[&str] = &[
    "op",
    "path",
    "expect",
    "expect_absent",
    "expect_type",
    "expect_error",
    "expectError",
    "expect_notified",
    "expect_values",
];

/// The keys of a `set` step, and of a `mutate_input` step, which states the same three things.
const WRITE_STEP_KEYS: &[&str] = &[
    "op",
    "path",
    "value",
    "expect_error",
    "expectError",
    "expect_notified",
    "expect_values",
];

/// The keys of a `delete` step.
const DELETE_STEP_KEYS: &[&str] = &[
    "op",
    "path",
    "expect_error",
    "expectError",
    "expect_notified",
    "expect_values",
];

/// The keys of a `dispose` step, which names no path.
const DISPOSE_STEP_KEYS: &[&str] = &[
    "op",
    "expect_error",
    "expectError",
    "expect_notified",
    "expect_values",
];

/// One of the five operations a `data_model` step states.
#[derive(Clone, Copy, Debug)]
enum Op {
    Get,
    Set,
    Delete,
    Dispose,
    MutateInput,
}

impl Op {
    fn named(name: &str) -> Option<Op> {
        Some(match name {
            "get" => Op::Get,
            "set" => Op::Set,
            "delete" => Op::Delete,
            "dispose" => Op::Dispose,
            "mutate_input" => Op::MutateInput,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Op::Get => "get",
            Op::Set => "set",
            Op::Delete => "delete",
            Op::Dispose => "dispose",
            Op::MutateInput => "mutate_input",
        }
    }

    fn step_keys(self) -> &'static [&'static str] {
        match self {
            Op::Get => GET_STEP_KEYS,
            Op::Set | Op::MutateInput => WRITE_STEP_KEYS,
            Op::Delete => DELETE_STEP_KEYS,
            Op::Dispose => DISPOSE_STEP_KEYS,
        }
    }
}

/// What one step read: a `get` answers with the value or its absence, the others answer with
/// nothing.
enum Outcome {
    Read(Option<Value>),
    Written,
}

/// One observer of a watched path: what it watches, how often it fired for the step being run,
/// and the value it last saw. The original's `_Observer` in `test_conformance.py:2332-2341`.
struct Observer {
    id: WatchId,
    /// The path as the case wrote it, which is what `expect_notified` and `expect_values` name.
    path: String,
    fired: usize,
    value: Value,
}

fn handle_data_model(case: &Case) -> Result<(), String> {
    assert_known_case_keys(case, DATA_MODEL_CASE_KEYS)?;
    let mut model = match case.get("initial") {
        Some(initial) => DataModel::with_initial(initial.clone()),
        None => DataModel::new(),
    };
    let mut observers = Vec::new();
    let watched = match case.get("watch") {
        Some(watch) => watch_paths(watch)?,
        None => Vec::new(),
    };
    for path in watched {
        let watch = model
            .subscribe(&path)
            .map_err(|error| format!("{}: watch '{path}' raised {error}", case.id))?;
        let initial = model
            .get(&path)
            .map_err(|error| format!("{}: watch '{path}' read: {error}", case.id))?
            .cloned()
            .unwrap_or(Value::Null);
        observers.push(Observer {
            id: watch.id,
            path,
            fired: 0,
            value: initial,
        });
    }

    let steps = case
        .get("steps")
        .map(|steps| {
            steps
                .as_array()
                .cloned()
                .ok_or_else(|| format!("{}: 'steps' is not a list", case.id))
        })
        .transpose()?
        .unwrap_or_default();
    if steps.is_empty() && case.get("expect").is_none() {
        return Err(format!(
            "{}: a data_model case needs 'steps' or 'expect'",
            case.id
        ));
    }

    // The value the case last handed to `set`, which `mutate_input` changes afterwards. The
    // original deep-copies the step's value for the same reason: the case must be able to prove the
    // model kept a copy of its own.
    let mut last_set: Option<Value> = None;
    for (index, step) in steps.iter().enumerate() {
        for observer in &mut observers {
            observer.fired = 0;
        }
        let named = step
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{}: steps[{index}] names no 'op'", case.id))?;
        let Some(op) = Op::named(named) else {
            return Err(format!("Unknown op '{named}' in {}", case.id));
        };
        assert_known_keys(
            step,
            op.step_keys(),
            &format!("steps[{index}] op key"),
            &case.id,
        )?;
        let expected_error = expectation(step, &case.id)?;
        let path = string_of(step, "path", &case.id, &format!("steps[{index}]"))?;

        let outcome = apply_step(&mut model, step, op, &path, &mut last_set);
        if let Some(expected) = expected_error {
            expect_failure(&case.id, expected, outcome.map(drop))?;
            let _ = model.take_notifications();
            continue;
        }
        let outcome = outcome.map_err(|error| {
            format!("{}: step {index} ({}) raised {error}", case.id, op.as_str())
        })?;
        deliver(&mut model, &mut observers);

        if let Outcome::Read(read) = outcome {
            check_get(step, &case.id, index, &path, read)?;
        }
        check_notified(step, &case.id, index, &observers)?;
        check_values(step, &case.id, index, &observers)?;
    }

    if let Some(expected) = case.get("expect") {
        let root = model
            .get("/")
            .map_err(|error| format!("{}: the root read raised {error}", case.id))?
            .cloned()
            .unwrap_or(Value::Null);
        compare(expected, &root).map_err(|problem| format!("{}: root: {problem}", case.id))?;
    }
    Ok(())
}

/// The `watch` list of a case, every entry of it a path the case wrote as a string.
fn watch_paths(watch: &Value) -> Result<Vec<String>, String> {
    let entries = watch
        .as_array()
        .ok_or_else(|| "a data_model case writes 'watch' as something but a list".to_string())?;
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "a watched path is not a string".to_string())
        })
        .collect()
}

/// Runs one step against the model.
fn apply_step(
    model: &mut DataModel,
    step: &Value,
    op: Op,
    path: &str,
    last_set: &mut Option<Value>,
) -> Result<Outcome, Error> {
    Ok(match op {
        Op::Get => Outcome::Read(model.get(path)?.cloned()),
        Op::Set => {
            let value = step.get("value").cloned().unwrap_or(Value::Null);
            *last_set = Some(value.clone());
            model.set(path, value)?;
            Outcome::Written
        }
        Op::Delete => {
            model.delete(path)?;
            Outcome::Written
        }
        Op::Dispose => {
            model.dispose();
            Outcome::Written
        }
        Op::MutateInput => {
            if let Some(held) = last_set.as_mut() {
                let value = step.get("value").cloned().unwrap_or(Value::Null);
                mutate_in_place(held, path, &value);
            }
            Outcome::Written
        }
    })
}

/// Moves every notification the step produced into the observer that received it.
fn deliver(model: &mut DataModel, observers: &mut [Observer]) {
    for (id, value) in model.take_notifications() {
        if let Some(observer) = observers.iter_mut().find(|observer| observer.id == id) {
            observer.fired += 1;
            observer.value = value;
        }
    }
}

/// The keys that make a `get` step state an expectation. The original's
/// `_DATA_MODEL_GET_EXPECT_KEYS`.
const GET_EXPECTATION_KEYS: &[&str] = &[
    "expect",
    "expect_absent",
    "expect_type",
    "expect_notified",
    "expect_values",
    "expect_error",
];

/// What a `get` step asked for: absence, a container of a kind, a whole value.
fn check_get(
    step: &Value,
    id: &str,
    index: usize,
    path: &str,
    read: Option<Value>,
) -> Result<(), String> {
    if !GET_EXPECTATION_KEYS
        .iter()
        .any(|key| step.get(*key).is_some())
    {
        return Err(format!(
            "{id}: steps[{index}] is a 'get' with no expectation"
        ));
    }
    // The suite's own words for absence: the original's `get` answers a stored `null` and a
    // missing key with the same `None`, and `data_model.yaml:38-41` says the distinction between
    // them is left out of the suite on purpose, because languages without `undefined` cannot
    // express it.
    let holds = read.as_ref().is_some_and(|value| !value.is_null());
    if step.get("expect_absent") == Some(&json!(true)) && holds {
        return Err(format!(
            "{id}: steps[{index}] expected path '{path}' to be absent, found {}",
            read.clone().unwrap_or(Value::Null)
        ));
    }
    if let Some(expected_type) = step.get("expect_type") {
        let named = expected_type.as_str().unwrap_or("");
        if named == "list" || named == "object" {
            let holds = match &read {
                Some(Value::Array(_)) => "a list".to_string(),
                Some(Value::Object(_)) => "an object".to_string(),
                Some(value) => kind(value),
                None => "nothing (the path is absent)".to_string(),
            };
            let wanted = if named == "list" {
                "a list"
            } else {
                "an object"
            };
            if holds != wanted {
                return Err(format!(
                    "{id}: steps[{index}] expected path '{path}' to be {wanted}, found {holds}"
                ));
            }
        } else {
            return Err(format!("Unknown expect_type '{named}' in {id}"));
        }
    }
    if let Some(expected) = step.get("expect") {
        let actual = read.clone().unwrap_or(Value::Null);
        compare(expected, &actual)
            .map_err(|problem| format!("{id}: steps[{index}] get '{path}': {problem}"))?;
    }
    Ok(())
}

/// `expect_notified`: the watched paths whose observers fired for this step, one entry per
/// observer. Compared sorted, as the original's harness compares it.
fn check_notified(
    step: &Value,
    id: &str,
    index: usize,
    observers: &[Observer],
) -> Result<(), String> {
    let Some(expected) = step.get("expect_notified") else {
        return Ok(());
    };
    let entries = expected
        .as_array()
        .ok_or_else(|| format!("{id}: steps[{index}] 'expect_notified' is not a list"))?;
    let mut wanted = entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{id}: an 'expect_notified' entry is not a string"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut fired: Vec<String> = Vec::new();
    for observer in observers {
        for _ in 0..observer.fired {
            fired.push(observer.path.clone());
        }
    }
    fired.sort();
    wanted.sort();
    if fired != wanted {
        return Err(format!(
            "{id}: steps[{index}] expect_notified: found {fired:?}, expected {wanted:?}"
        ));
    }
    Ok(())
}

/// `expect_values`: the value each named watched path holds after this step, as its observer saw
/// it.
fn check_values(
    step: &Value,
    id: &str,
    index: usize,
    observers: &[Observer],
) -> Result<(), String> {
    let Some(values) = step.get("expect_values") else {
        return Ok(());
    };
    let entries = values
        .as_object()
        .ok_or_else(|| format!("{id}: steps[{index}] 'expect_values' is not an object"))?;
    for (path, expected) in entries {
        let Some(observer) = observers.iter().find(|observer| &observer.path == path) else {
            return Err(format!("{id}: '{path}' in expect_values is not watched"));
        };
        compare(expected, &observer.value)
            .map_err(|problem| format!("{id}: steps[{index}] expect_values '{path}': {problem}"))?;
    }
    Ok(())
}

/// Writes into the object the case handed to `set` after the model took it, which is what the
/// suite's defensive-copy case asks the caller to do. The original's `_mutate_in_place`.
fn mutate_in_place(target: &mut Value, path: &str, value: &Value) {
    let tokens: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect();
    let (last, parents) = match tokens.split_last() {
        Some(split) => split,
        None => return,
    };
    let mut current = target;
    for token in parents {
        let next = match current {
            Value::Array(items) => token
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get_mut(index)),
            Value::Object(entries) => entries.get_mut(token.as_str()),
            _ => None,
        };
        match next {
            Some(entry) => current = entry,
            None => return,
        }
    }
    match current {
        Value::Array(items) => {
            if let Ok(index) = last.parse::<usize>() {
                while items.len() <= index {
                    items.push(Value::Null);
                }
                items[index] = value.clone();
            }
        }
        Value::Object(entries) => {
            entries.insert(last.clone(), value.clone());
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ resolve_path

/// The keys the `resolve_path` action states, beside the metadata the runner accepts for every
/// case. The original's `_RESOLVE_PATH_CASE_KEYS`.
const RESOLVE_PATH_CASE_KEYS: &[&str] = &["action", "args", "catalog", "expect"];

/// The keys of `resolve_path`'s `args`. The original's `_RESOLVE_PATH_ARGS_KEYS`, which takes both
/// spellings of the context path.
const RESOLVE_PATH_ARGS_KEYS: &[&str] = &["context_path", "contextPath", "path"];

fn handle_resolve_path(case: &Case) -> Result<(), String> {
    assert_known_case_keys(case, RESOLVE_PATH_CASE_KEYS)?;
    let expected = case.require("expect")?;
    let args = case
        .get("args")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    assert_known_keys(&args, RESOLVE_PATH_ARGS_KEYS, "args key", &case.id)?;

    let path = string_of(&args, "path", &case.id, "args")?;
    let mut context = string_of(&args, "contextPath", &case.id, "args")?;
    if context.is_empty() {
        context = string_of(&args, "context_path", &case.id, "args")?;
    }
    if context.is_empty() {
        context = "/".to_string();
    }

    let resolved = ScopePath::new(&context).resolve(&path);
    // A case may state its expectation as `{result: ...}` as well as plain.
    let expected = expected
        .as_object()
        .and_then(|object| object.get("result"))
        .unwrap_or(expected);
    compare(expected, &json!(resolved))
        .map_err(|problem| format!("{}: resolved path: {problem}", case.id))
}

// ------------------------------------------------------------------ parse_expression_template

/// The keys the `parse_expression_template` action states, beside the metadata the runner accepts
/// for every case. The original's `_PARSE_EXPRESSION_TEMPLATE_CASE_KEYS`.
const PARSE_EXPRESSION_CASE_KEYS: &[&str] =
    &["action", "expect", "expect_error", "expectError", "input"];

fn handle_parse_expression_template(case: &Case) -> Result<(), String> {
    assert_known_case_keys(case, PARSE_EXPRESSION_CASE_KEYS)?;
    let expected_error = expectation(&case.json, &case.id)?;
    if case.get("expect").is_none() && expected_error.is_none() {
        return Err(format!(
            "{}: a parse_expression_template case needs 'expect' or an expected error",
            case.id
        ));
    }
    let input = string_of(&case.json, "input", &case.id, "the case")?;
    if let Some(expected) = expected_error {
        // A case of this action that names no category expects `ParseError`: that is what the
        // original's harness defaults it to (`test_conformance.py:3360`), because a template that
        // does not parse raises the expression error the guide maps to `ParseError`.
        let expected = named_category(expected, "ParseError");
        return expect_failure(&case.id, &expected, parse_template(&input).map(drop));
    }
    let parts = parse_template(&input)
        .map_err(|error| format!("{}: the template raised {error}", case.id))?;
    compare(case.require("expect")?, &Value::Array(joined_parts(&parts)))
        .map_err(|problem| format!("{}: parsed parts: {problem}", case.id))
}

/// The same expectation with `category` filled in, when the case left it out.
fn named_category(expected: &Value, category: &str) -> Value {
    let Some(object) = expected.as_object() else {
        return expected.clone();
    };
    if object.contains_key("category") {
        return expected.clone();
    }
    let mut filled = object.clone();
    filled.insert("category".to_string(), Value::String(category.to_string()));
    Value::Object(filled)
}

/// The guide, 3: "Join adjacent literal parts and drop empty ones before comparing, so
/// implementations that split literal text differently still conform."
fn joined_parts(parts: &[TemplatePart]) -> Vec<Value> {
    let mut joined: Vec<Value> = Vec::new();
    for part in parts {
        let value = part.to_json();
        match (value, joined.last_mut()) {
            (Value::String(text), Some(Value::String(kept))) => *kept = format!("{kept}{text}"),
            (value, _) => joined.push(value),
        }
    }
    joined.retain(|value| value.as_str() != Some(""));
    joined
}

/// A case's or a step's `key` as the string it holds.
///
/// An absent key is the original's empty string; a key that holds anything but a string is a case
/// this harness cannot run, and says so rather than reading it as nothing.
fn string_of(holder: &Value, key: &str, id: &str, at: &str) -> Result<String, String> {
    match holder.get(key) {
        None => Ok(String::new()),
        Some(Value::String(found)) => Ok(found.clone()),
        Some(found) => Err(format!(
            "{id}: {at} '{key}' is not a string, it is {}",
            kind(found)
        )),
    }
}
