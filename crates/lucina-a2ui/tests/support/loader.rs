//! Reading a vendored suite file into cases of `serde_json::Value`.
//!
//! The rules are the guide's section "Shared harness plumbing", 1. YAML loading and plain-value
//! conversion:
//!
//! - A YAML 1.2 parser, so that an unquoted `yes`, `no`, `on` or `off` stays the string it is and
//!   `1:30` is not read as a base-60 number.
//! - Every loaded node converted to plain JSON values before the SDK sees it.
//!
//! `yaml-rust` is the parser. It resolves a plain scalar with the YAML 1.2 core schema
//! (`yaml_rust::yaml::Yaml::from_str`): only `true` and `false` become booleans, only what Rust's
//! `i64` and `f64` parsers accept become numbers, everything else stays a string. The tests in
//! `tests/harness_plumbing.rs` pin that, because the suites are only trustworthy when the words
//! they hold are the words that were written.

use std::path::PathBuf;

use serde_json::{Map, Value};
use yaml_rust::{Yaml, YamlLoader};

use super::expect::kind;

/// The vendored `conformance/` directory: the oracle's own root, checked by
/// `tests/vendored_tree.rs` against its manifest.
pub fn conformance_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("third_party/a2ui/upstream/conformance")
}

/// One case of a suite: its `<suite>#<name>` id and its JSON.
#[derive(Clone, Debug)]
pub struct Case {
    /// `<suite path relative to conformance/>#<name>`, the key the strict dispatch reports.
    pub id: String,
    /// The case object, converted from YAML.
    pub json: Value,
}

impl Case {
    /// The case's `action`, which is what the dispatcher routes on.
    pub fn action(&self) -> Option<&str> {
        self.json.get("action").and_then(Value::as_str)
    }

    /// The case's `value` for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.json.get(key)
    }

    /// The case's `key`, or a failure naming the case when it holds no such key.
    pub fn require(&self, key: &str) -> Result<&Value, String> {
        self.get(key)
            .ok_or_else(|| format!("{}: required key '{key}' is absent", self.id))
    }
}

/// Loads and converts the suite `conformance/<rel>`, one case per entry of its top-level list.
pub fn load_suite(rel: &str) -> Result<Vec<Case>, String> {
    let path = conformance_root().join(rel);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{}: cannot read suite: {error}", path.display()))?;
    let document = from_yaml(&text)?;
    cases_of(rel, &document).map_err(|problem| format!("{}: {problem}", path.display()))
}

/// The single document of a suite text, converted to JSON.
pub fn from_yaml(text: &str) -> Result<Value, String> {
    let documents = YamlLoader::load_from_str(text).map_err(|error| format!("YAML: {error}"))?;
    match documents.as_slice() {
        [document] => to_value(document),
        [] => Err("the file holds no YAML document".to_string()),
        many => Err(format!(
            "the file holds {} YAML documents; a suite is one",
            many.len()
        )),
    }
}

/// The entries of a suite's top-level list, each with its id.
fn cases_of(rel: &str, document: &Value) -> Result<Vec<Case>, String> {
    let entries = document
        .as_array()
        .ok_or_else(|| format!("expected a list of test cases, found {}", kind(document)))?;
    let mut cases = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let object = entry
            .as_object()
            .ok_or_else(|| format!("entry {index} is {}", kind(entry)))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("entry {index} has no 'name' string"))?;
        cases.push(Case {
            id: format!("{rel}#{name}"),
            json: entry.clone(),
        });
    }
    Ok(cases)
}

/// Converts a loaded YAML node to a JSON value, recursively.
pub fn to_value(yaml: &Yaml) -> Result<Value, String> {
    Ok(match yaml {
        Yaml::Null => Value::Null,
        Yaml::Boolean(value) => Value::Bool(*value),
        Yaml::Integer(value) => Value::Number((*value).into()),
        Yaml::Real(text) => number_from_str(text)?,
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Array(items) => {
            Value::Array(items.iter().map(to_value).collect::<Result<Vec<_>, _>>()?)
        }
        Yaml::Hash(entries) => {
            let mut object = Map::new();
            for (key, value) in entries {
                let name = key_name(key)?;
                object.insert(name, to_value(value)?);
            }
            Value::Object(object)
        }
        Yaml::Alias(_) => return Err("YAML aliases are not in any suite of the oracle".to_string()),
        Yaml::BadValue => return Err("the YAML parser produced no value here".to_string()),
    })
}

/// A YAML `Real` holds its text: a JSON number, which means a finite one.
fn number_from_str(text: &str) -> Result<Value, String> {
    let parsed = text
        .parse::<f64>()
        .map_err(|error| format!("'{text}' is not a number: {error}"))?;
    serde_json::Number::from_f64(parsed)
        .map(Value::Number)
        .ok_or_else(|| format!("'{text}' is not a JSON number: it is not finite"))
}

/// A JSON object key, which YAML allows to be any scalar.
fn key_name(key: &Yaml) -> Result<String, String> {
    match key {
        Yaml::String(text) => Ok(text.clone()),
        Yaml::Integer(value) => Ok(value.to_string()),
        Yaml::Real(text) => Ok(text.clone()),
        Yaml::Boolean(value) => Ok(value.to_string()),
        Yaml::Null => Ok("null".to_string()),
        other => Err(format!(
            "a mapping key cannot be {}",
            kind(&to_value(other)?)
        )),
    }
}
