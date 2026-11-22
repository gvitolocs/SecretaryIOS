//! The reactive data model: an atomic RFC 6901 JSON Pointer store with observers.
//!
//! One store per surface. Reads, writes and deletes address it by absolute JSON Pointer; a write
//! builds the containers on its way that are not there yet, an observer registered on a path hears
//! about a write to that path, to an ancestor of it or to a descendant of it, and only when the
//! value it holds actually changed. `dispose` detaches every observer.
//!
//! The behaviour is the original's `a2ui/core/state/data_model.py`, measured by every case of
//! `conformance/core/data_model.yaml`. The interfaces are ours: no `None` path argument (Rust says
//! `&str`), no caller-owned value to defend against (`set` takes the value by moving it in), and
//! notifications the host delivers instead of callbacks the store invokes — a callback handed a
//! borrow of the model would have to borrow the model it is busy changing.
//!
//! The host drains the queue with [`DataModel::take_notifications`] right after each write, and
//! that drain is the moment a notification is handed out: an observer detached between the write
//! and the drain hears nothing, which is what the original does by re-reading its live listener
//! list as each path fires (`state/data_model.py:342`). [`DataModel::dispose`] drops whatever was
//! not yet handed out.

use std::collections::HashMap;

use serde_json::{Map, Number, Value};

use crate::error::Error;

/// The largest array index a write may address, so that a pointer in a payload cannot make the
/// model allocate an array of any size it names. The original's `MAX_ARRAY_INDEX`.
pub const MAX_ARRAY_INDEX: u64 = 10_000;

/// The path segments a pointer may never address: the prototype chain of the original's JavaScript
/// implementation. The original's `FORBIDDEN_KEYS`.
pub const FORBIDDEN_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// The cap on depth, which the original has no need for and a port cannot do without.
///
/// A pointer of any number of segments and a value of any nesting make a model the port walks,
/// clones and drops with recursion, so a payload could name a depth that overflows the stack
/// instead of being refused. 128 is the nesting limit `serde_json`'s own parser works to: a value
/// the model can hold is a value it can read back from a message, and no pointer the model
/// addresses is deeper. The same cap covers [`crate::data_context`], whose resolved pointers are
/// addressed here.
pub const MAX_DEPTH: usize = 128;

/// A handle to one observer registration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WatchId(u64);

/// What registering an observer returned: its handle, and what the path held at that moment.
#[derive(Clone, Debug)]
pub struct Watch {
    /// The handle one observer is detached with.
    pub id: WatchId,
    /// The value at the path when the observer was registered, `None` when it held nothing.
    pub initial: Option<Value>,
}

/// One path under observation and the observers registered on it, oldest first. The original's
/// value of `_listeners`: a normalized path to its list of callbacks. Keeping the observers
/// together under the path they watch is what makes a write cost the paths under observation
/// rather than the square of them.
#[derive(Clone, Debug)]
struct Watched {
    /// The path split into unescaped segments, so that two spellings of one path are one path for
    /// the notification routing.
    segments: Vec<String>,
    /// Every observer registered on it, oldest first. An empty list is a path nothing watches
    /// any more, which costs a write one test and nothing else.
    ids: Vec<WatchId>,
}

/// The reactive data model of one surface.
#[derive(Clone, Debug)]
pub struct DataModel {
    /// The whole model: the original's `_data`.
    data: Value,
    /// Every path under observation, first-registered first: the original's `_listeners`.
    watched: Vec<Watched>,
    /// Which entry of `watched` each path is, so that registering an observer on a path already
    /// watched finds it without a scan.
    paths: HashMap<Vec<String>, usize>,
    /// Which path each live observer is on. An observer detached with `unsubscribe` leaves it, so
    /// this is also the answer to "is this observer still there".
    registered: HashMap<WatchId, usize>,
    /// The notifications the writes since the last drain produced, oldest first, one entry per
    /// observer that fired. What an observer that is gone by then was told is dropped at the drain.
    pending: Vec<(WatchId, Value)>,
    next_id: u64,
}

impl DataModel {
    /// A model holding an empty object.
    pub fn new() -> DataModel {
        DataModel {
            data: Value::Object(Map::new()),
            watched: Vec::new(),
            paths: HashMap::new(),
            registered: HashMap::new(),
            pending: Vec::new(),
            next_id: 0,
        }
    }

    /// A model holding `initial`. A `Value::Null` is the original's absent argument: an empty
    /// object.
    pub fn with_initial(initial: Value) -> DataModel {
        let mut model = DataModel::new();
        if !initial.is_null() {
            model.data = initial;
        }
        model
    }

    /// The absolute pointer `path` names, hung under `context` when it is relative and the context
    /// is there: the original's static `DataModel.resolve_path`
    /// (`python/a2ui_core/src/a2ui/core/state/data_model.py:62-81`,
    /// `typescript/core/web_core/src/state/data-model.ts:305-315`, which agree line for line).
    ///
    /// This is the join a caller hands a path to, and it is *not* the scope resolution of
    /// [`crate::data_context`]: an empty context is no context rather than the root, a context of
    /// its own is taken as it stands with exactly one slash added, and `.` and the empty string are
    /// ordinary text. It answers a plain `String` because both originals do and nothing it says
    /// touches the data: what it produces is refused by [`DataModel::get`] and [`DataModel::set`]
    /// if it names more segments than [`MAX_DEPTH`].
    ///
    /// The port keeps it because the message processor of step 2b calls it: TypeScript's
    /// `MessageProcessor.resolvePath` is a public method whose body is this function
    /// (`typescript/core/web_core/src/processing/message-processor.ts:600-601`), tested at
    /// `message-processor.test.ts:1681-1687`. Python has no caller of it.
    pub fn resolve_path(path: &str, context: &str) -> String {
        if path.starts_with('/') {
            return path.to_string();
        }
        if context.is_empty() {
            return format!("/{path}");
        }
        if context.ends_with('/') {
            return format!("{context}{path}");
        }
        format!("{context}/{path}")
    }

    /// The value at `path`, or `None` when the model holds nothing there.
    ///
    /// A path that names the root (`""`, `"/"`, or any number of empty segments) is the whole
    /// model, and a path through a primitive, through a list with a segment that is not an index,
    /// or past the end of a list is nothing.
    ///
    /// # Errors
    /// A `DataError` for a segment the model refuses to address, which is what a forbidden segment
    /// is, and for a pointer with more segments than [`MAX_DEPTH`].
    pub fn get(&self, path: &str) -> Result<Option<&Value>, Error> {
        Ok(find(&self.data, &parse_pointer(path)?))
    }

    /// Whether `path` is physically there, which tells a stored `null` from an absent key.
    ///
    /// # Errors
    /// As [`DataModel::get`].
    pub fn has_path(&self, path: &str) -> Result<bool, Error> {
        Ok(find(&self.data, &parse_pointer(path)?).is_some())
    }

    /// Writes `value` at `path`, building the containers on the way that are not there yet: a list
    /// when the next segment is an index, an object otherwise.
    ///
    /// A `Value::Null` is the original's removal: it deletes the key, or empties the array element
    /// it addresses without lengthening the list. Writing at the root replaces the whole model.
    ///
    /// # Errors
    /// A `DataError` for a forbidden segment, a pointer with more segments than [`MAX_DEPTH`], a
    /// value that nests deeper than [`MAX_DEPTH`], a non-numeric segment on an array, an index past
    /// [`MAX_ARRAY_INDEX`], or a write that would go through a primitive. Nothing is rolled back:
    /// the original raises from the same point of its own walk, containers it already built stay.
    /// A refusal of a path or a value that is too deep happens before anything is built.
    pub fn set(&mut self, path: &str, value: Value) -> Result<(), Error> {
        let tokens = parse_pointer(path)?;
        if value.is_null() && find(&self.data, &tokens).is_none() {
            return Ok(());
        }
        let depth = value_depth(&value);
        if depth > MAX_DEPTH {
            return Err(Error::data(format!(
                "Cannot set path '{path}': the value nests {depth} levels, more than the {MAX_DEPTH} the model holds."
            )));
        }
        let before = self.snapshot(&tokens);

        if tokens.is_empty() {
            self.data = if value.is_null() {
                Value::Object(Map::new())
            } else {
                value
            };
            self.cascade(&before);
            return Ok(());
        }

        if self.data.is_null() {
            self.data = Value::Object(Map::new());
        } else if !self.data.is_object() && !self.data.is_array() {
            return Err(Error::data(format!(
                "Cannot set path '{path}': the data model root is a primitive value."
            )));
        }

        // The walk treats the leaf differently from the intermediates, and the leaf is the last
        // segment of a pointer that is not the root, which was answered above.
        let Some((last, parents)) = tokens.split_last() else {
            return Ok(());
        };
        let mut cursor = &mut self.data;
        for (index, token) in parents.iter().enumerate() {
            let next = parents.get(index + 1).unwrap_or(last);
            cursor = step_into(cursor, token, is_index(next), path)?;
        }
        write_leaf(cursor, last, value, path)?;
        self.cascade(&before);
        Ok(())
    }

    /// Removes what `path` holds: the original's `set(path, None)`. An absent path is left alone,
    /// so no containers appear and no observer hears anything.
    ///
    /// # Errors
    /// As [`DataModel::set`].
    pub fn delete(&mut self, path: &str) -> Result<(), Error> {
        self.set(path, Value::Null)
    }

    /// Registers an observer of `path`, which hears about a write to that path, to any path
    /// containing it or to any path it contains, and only when the value there changed.
    ///
    /// # Errors
    /// As [`DataModel::get`].
    pub fn subscribe(&mut self, path: &str) -> Result<Watch, Error> {
        let segments = parse_pointer(path)?;
        let initial = find(&self.data, &segments).cloned();
        let id = WatchId(self.next_id);
        self.next_id += 1;
        let at = match self.paths.get(&segments) {
            Some(at) => *at,
            None => {
                let at = self.watched.len();
                self.paths.insert(segments.clone(), at);
                self.watched.push(Watched {
                    segments,
                    ids: Vec::new(),
                });
                at
            }
        };
        self.watched[at].ids.push(id);
        self.registered.insert(id, at);
        Ok(Watch { id, initial })
    }

    /// Detaches one observer. Detaching one that is gone already does nothing. A notification the
    /// observer was owed and has not been handed out yet goes with it: the original re-reads its
    /// live listener list as each path fires (`state/data_model.py:342`).
    pub fn unsubscribe(&mut self, id: WatchId) {
        let Some(at) = self.registered.remove(&id) else {
            return;
        };
        self.watched[at].ids.retain(|watcher| *watcher != id);
    }

    /// The notifications the writes since the last drain produced, oldest first, one entry per
    /// observer that fired and is still subscribed. The host calls this right after each write.
    pub fn take_notifications(&mut self) -> Vec<(WatchId, Value)> {
        let DataModel {
            registered,
            pending,
            ..
        } = self;
        pending.retain(|(id, _)| registered.contains_key(id));
        std::mem::take(pending)
    }

    /// Detaches every observer, leaving the data where it is. What was not yet handed out is
    /// dropped with them.
    pub fn dispose(&mut self) {
        self.watched.clear();
        self.paths.clear();
        self.registered.clear();
        self.pending.clear();
    }

    /// What each path this write concerns holds, before the write, in the order the paths were
    /// first registered. The original snapshots the watched values before it mutates
    /// (`state/data_model.py:169`) so that a notification can be compared against what the observer
    /// really saw, and it walks its key map once per write (`:331-346`) — which is what this costs
    /// too: the paths under observation, not the observers squared.
    fn snapshot(&self, written: &[String]) -> Vec<Sighted> {
        self.watched
            .iter()
            .enumerate()
            .filter(|(_, watched)| {
                !watched.ids.is_empty() && speaks_for(&watched.segments, written)
            })
            .map(|(at, watched)| Sighted {
                at,
                value: observed(&self.data, &watched.segments),
            })
            .collect()
    }

    /// The observers that have to hear about a write: those on a path the write speaks for, and
    /// only where the value at that path changed. One entry per observer, in the order the
    /// observers registered.
    fn cascade(&mut self, before: &[Sighted]) {
        let mut handed = std::mem::take(&mut self.pending);
        for sighted in before {
            let watched = &self.watched[sighted.at];
            let now = observed(&self.data, &watched.segments);
            if same_value(&sighted.value, &now) {
                continue;
            }
            let value = now.unwrap_or(Value::Null);
            handed.extend(watched.ids.iter().map(|id| (*id, value.clone())));
        }
        self.pending = handed;
    }
}

impl Default for DataModel {
    /// The same as [`DataModel::new`]: an absent model is an empty object, never nothing.
    fn default() -> DataModel {
        DataModel::new()
    }
}

/// One path the write speaks for and what it held before the write. `at` is the path's place in
/// `watched`, which is how the routing finds the observers on it without looking for them.
struct Sighted {
    at: usize,
    value: Option<Value>,
}

/// Whether an observer of `watched` hears about a write to `written`: the same path, an ancestor
/// of it, or a descendant of it. The original's `is_relevant` in `_trigger_cascade`.
fn speaks_for(watched: &[String], written: &[String]) -> bool {
    if watched == written {
        true
    } else if watched.len() < written.len() {
        written.starts_with(watched)
    } else {
        watched.starts_with(written)
    }
}

/// The segments of a parsed JSON Pointer, escapes resolved, or a `DataError` naming the path when
/// a segment is one the model refuses to address.
///
/// Empty segments are not segments, so `/foo/`, `//foo` and `/foo` are one path, as the suite's
/// `test_data_model_normalizes_empty_path_segments` states. A lone `/` or an empty path is the
/// root. More segments than [`MAX_DEPTH`] is a pointer the model does not address at all.
fn parse_pointer(path: &str) -> Result<Vec<String>, Error> {
    let depth = depth_of(path);
    if depth > MAX_DEPTH {
        return Err(Error::data(format!(
            "Path '{path}' names {depth} segments, more than the {MAX_DEPTH} the model addresses."
        )));
    }
    let tokens: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect();
    for segment in &tokens {
        if FORBIDDEN_SEGMENTS.contains(&segment.as_str()) {
            return Err(Error::data(format!(
                "Forbidden path segment '{segment}' in path '{path}'."
            )));
        }
    }
    Ok(tokens)
}

/// The number of segments a pointer names. Empty segments are not segments, which is the same
/// count [`parse_pointer`] splits into tokens and the unit the cap on [`MAX_DEPTH`] is measured
/// in. A scope joins pointers before the model is asked to address them, and counts them here.
pub(crate) fn depth_of(pointer: &str) -> usize {
    pointer
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
}

/// The value the model holds at `segments`, or `None` when it holds nothing there. A stored
/// `Value::Null` is a value, which is what tells `has_path` from an absent path.
fn find<'a>(data: &'a Value, segments: &[String]) -> Option<&'a Value> {
    let mut current = data;
    for segment in segments {
        current = match current {
            Value::Object(entries) => entries.get(segment.as_str())?,
            Value::Array(items) => element(items, segment)?,
            _ => return None,
        };
    }
    Some(current)
}

/// The value an observer of `segments` would be told about, for the change comparison of a
/// notification.
///
/// Not `find`: the original compares through its own `get`, which answers `None` both for an absent
/// key and for a stored `null`, so a write that removes a key that already held null is no change
/// and notifies nobody. Comparing through `find` here would fire an observer over a value that
/// never differed.
fn observed(data: &Value, segments: &[String]) -> Option<Value> {
    find(data, segments)
        .filter(|value| !value.is_null())
        .cloned()
}

/// Whether what the path holds now is what the observer holds, which is what decides whether it
/// hears about the write.
///
/// Numbers are compared by value at every depth, because the original has one number type in each
/// of its languages: `state/data_model.py:341` compares the snapshot with Python's `==`, and a
/// JavaScript number holds an integer and a fraction in the one type. A port has two
/// representations where the original has one, so writing `1.0` over `1` changed nothing the
/// observer holds. A boolean is never a number, which is the TypeScript answer: Python's
/// `1 == True` is not followed, because a boolean where the observer last saw a number is a change
/// the surface has to redraw.
fn same_value(before: &Option<Value>, now: &Option<Value>) -> bool {
    match (before, now) {
        (Some(before), Some(now)) => values_equal(before, now),
        (None, None) => true,
        _ => false,
    }
}

/// The same value written another way, as far as an observer of a path is concerned.
fn values_equal(before: &Value, now: &Value) -> bool {
    match (before, now) {
        (Value::Number(before), Value::Number(now)) => numbers_equal(before, now),
        (Value::Array(before), Value::Array(now)) => {
            before.len() == now.len()
                && before
                    .iter()
                    .zip(now)
                    .all(|(before, now)| values_equal(before, now))
        }
        (Value::Object(before), Value::Object(now)) => {
            before.len() == now.len()
                && before
                    .iter()
                    .all(|(key, value)| now.get(key).is_some_and(|held| values_equal(value, held)))
        }
        (before, now) => before == now,
    }
}

/// Whether two JSON numbers are the same number, whichever way each of them is written.
///
/// `serde_json`'s own comparison answers by representation, which would make `25000` and `25000.0`
/// two values. Integers are compared exactly, within the width a machine holds; a number that is
/// not an integer on both sides is compared as a double, which is the widest thing the original's
/// two languages both agree on.
///
/// The conformance harness compares an expectation and an answer with this same rule, which is what
/// lets `expressions.yaml` hold `test_expr_exponential_notation_capital_e`,
/// `test_expr_number_with_trailing_point` and `test_expr_exponent_underflow_is_zero`.
pub(crate) fn numbers_equal(before: &Number, now: &Number) -> bool {
    match (exact_integer(before), exact_integer(now)) {
        (Some(before), Some(now)) => before == now,
        _ => before.as_f64() == now.as_f64(),
    }
}

/// The value of a number written without a fraction, which is the part both representations agree
/// on exactly.
fn exact_integer(number: &Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// The levels a value nests: none for a scalar, one more for each array or object around it.
///
/// Counted with a stack of its own rather than by recursion, so that the check that keeps the
/// stack safe is not the thing that overflows it. The walk is the same order of work as the
/// original's `copy.deepcopy` of the value it writes at the same point
/// (`state/data_model.py:172`), so it costs nothing the original was not already paying.
fn value_depth(value: &Value) -> usize {
    let mut deepest = 0;
    let mut stack = Vec::with_capacity(8);
    stack.push((value, 0usize));
    while let Some((node, level)) = stack.pop() {
        deepest = deepest.max(level);
        match node {
            Value::Array(items) => stack.extend(items.iter().map(|item| (item, level + 1))),
            Value::Object(entries) => stack.extend(entries.values().map(|item| (item, level + 1))),
            _ => {}
        }
    }
    deepest
}

/// Whether a segment is a canonical array index: `0`, or digits with no leading zero. Anything
/// else — `01`, `-1`, `1.0` — addresses no element, which is RFC 6901 section 4 and the original's
/// `NUMERIC_PATTERN` of `^(?:0|[1-9][0-9]*)$`.
fn is_index(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0')
}

/// The index a canonical numeric segment names. A segment of more digits than any machine index
/// holds saturates, which is far past [`MAX_ARRAY_INDEX`] and reads as nothing.
fn index_of(segment: &str) -> u64 {
    segment.parse::<u64>().unwrap_or(u64::MAX)
}

/// The element an index segment addresses, `None` past the end or with a segment that is not an
/// index at all.
fn element<'a>(items: &'a [Value], segment: &str) -> Option<&'a Value> {
    if !is_index(segment) {
        return None;
    }
    usize::try_from(index_of(segment))
        .ok()
        .and_then(|index| items.get(index))
}

/// One container on the way to a write, built when it is not there yet.
///
/// A `null` on the way is a placeholder the write replaces with a container, which is what
/// `test_data_model_set_through_null_placeholder_vivifies` states; a primitive is the caller's
/// value and the write stops there.
fn step_into<'a>(
    current: &'a mut Value,
    segment: &str,
    next_is_index: bool,
    path: &str,
) -> Result<&'a mut Value, Error> {
    match current {
        Value::Object(entries) => {
            let held = entries
                .entry(segment.to_string())
                .or_insert_with(|| container(next_is_index));
            refuse_primitive(held, segment, path)?;
            if held.is_null() {
                *held = container(next_is_index);
            }
            Ok(held)
        }
        Value::Array(items) => {
            refuse_index(segment, path)?;
            let index = index_of(segment);
            if index > MAX_ARRAY_INDEX {
                return Err(too_high(segment, path));
            }
            let index = index as usize;
            while items.len() <= index {
                items.push(Value::Null);
            }
            let held = &mut items[index];
            refuse_primitive(held, segment, path)?;
            if held.is_null() {
                *held = container(next_is_index);
            }
            Ok(held)
        }
        _ => Err(primitive(segment, path)),
    }
}

/// What a write makes of a missing container: a list for an index, an object for anything else.
fn container(next_is_index: bool) -> Value {
    if next_is_index {
        Value::Array(Vec::new())
    } else {
        Value::Object(Map::new())
    }
}

/// The leaf of a pointer: written, or emptied when the write is a removal.
fn write_leaf(current: &mut Value, segment: &str, value: Value, path: &str) -> Result<(), Error> {
    match current {
        Value::Object(entries) => {
            if value.is_null() {
                entries.remove(segment);
            } else {
                entries.insert(segment.to_string(), value);
            }
            Ok(())
        }
        Value::Array(items) => {
            refuse_index(segment, path)?;
            let index = index_of(segment);
            if index > MAX_ARRAY_INDEX {
                return Err(too_high(segment, path));
            }
            let index = index as usize;
            if value.is_null() {
                // Deleting an index past the end is a no-op: padding here would
                // materialise entries that were never set, which the original's
                // `test_data_model_delete_out_of_range_list_index_is_noop` states.
                if let Some(slot) = items.get_mut(index) {
                    *slot = Value::Null;
                }
            } else {
                while items.len() <= index {
                    items.push(Value::Null);
                }
                items[index] = value;
            }
            Ok(())
        }
        _ => Err(primitive(segment, path)),
    }
}

/// A segment that is not an index cannot address an array element.
fn refuse_index(segment: &str, path: &str) -> Result<(), Error> {
    if is_index(segment) {
        return Ok(());
    }
    Err(Error::data(format!(
        "Cannot use non-numeric segment '{segment}' on an array in path '{path}'."
    )))
}

/// A primitive on the way holds no key and no index, so nothing can be written under it.
fn refuse_primitive(held: &Value, segment: &str, path: &str) -> Result<(), Error> {
    if held.is_object() || held.is_array() || held.is_null() {
        return Ok(());
    }
    Err(primitive(segment, path))
}

/// The original's refusal of a write that would have to go through a value that holds nothing.
fn primitive(segment: &str, path: &str) -> Error {
    Error::data(format!(
        "Cannot set path '{path}': segment '{segment}' is a primitive value."
    ))
}

fn too_high(segment: &str, path: &str) -> Error {
    Error::data(format!(
        "Cannot set path '{path}': array index '{segment}' exceeds maximum supported index ({MAX_ARRAY_INDEX})."
    ))
}

/// What the suites cannot say about the model. `data_model.yaml` detaches observers only by
/// `dispose`, never writes one spelling of a number over another, never states a pointer or a value
/// deeper than anything it needs, and says nothing about when the host drains; its header leaves
/// the difference between an absent key and a stored `null` out on purpose, because not every
/// language can express it. A port can, later steps read it, and both reviews asked for these, so
/// they are pinned here.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::ScopePath;
    use serde_json::json;

    #[test]
    fn unsubscribing_one_observer_leaves_the_others() {
        let mut model = DataModel::with_initial(json!({"a": 1}));
        let first = model.subscribe("/a").expect("a watched path");
        let second = model.subscribe("/a").expect("a watched path");
        model.unsubscribe(first.id);
        model
            .set("/a", json!(2))
            .expect("a write to a watched path");
        let fired = model.take_notifications();
        assert_eq!(fired, vec![(second.id, json!(2))]);
    }

    /// `has_path` is the answer to "is the key there", `get` to "what is there": a stored null is a
    /// value for one and nothing for the other, which is what makes a write of `Value::Null` to an
    /// existing null a change and to an absent key a no-op.
    #[test]
    fn a_stored_null_exists_without_being_a_value() {
        let mut model = DataModel::with_initial(json!({"held": null}));
        assert!(model.has_path("/held").expect("a well formed pointer"));
        assert_eq!(model.get("/held").expect("a read"), Some(&Value::Null));
        assert!(!model.has_path("/absent").expect("a well formed pointer"));

        model.set("/held", Value::Null).expect("a removal");
        assert_eq!(
            model.get("/").expect("a read").cloned(),
            Some(json!({})),
            "the key went"
        );

        let mut watched = DataModel::with_initial(json!({"held": null}));
        let observer = watched.subscribe("/held").expect("a watched path");
        watched
            .set("/held", Value::Null)
            .expect("a removal of a stored null");
        assert!(
            watched.take_notifications().is_empty(),
            "a null written over a null changed nothing the observer holds"
        );
        assert_eq!(observer.initial, Some(Value::Null));
    }

    /// The two reviews both found the change comparison, and neither said the suite covers it:
    /// `data_model.yaml` never writes one spelling of a number over another. Both originals
    /// compare by value — Python at `state/data_model.py:341` with one number type, TypeScript the
    /// same way — so a port with two representations must not tell an observer that its `1` became
    /// `1.0`. A boolean is not a number, which is the TypeScript answer and not Python's
    /// `1 == True`.
    #[test]
    fn a_number_written_in_another_spelling_changes_nothing() {
        let mut model = DataModel::with_initial(json!({"one": 1, "list": [1]}));
        let scalar = model.subscribe("/one").expect("a watched path");
        let list = model.subscribe("/list").expect("a watched path");

        model
            .set("/one", json!(1.0))
            .expect("the same number, written as a float");
        assert!(
            model.take_notifications().is_empty(),
            "an observer of 1 was told its value became 1.0"
        );
        model
            .set("/list", json!([1.0]))
            .expect("the same list, with a float inside");
        assert!(
            model.take_notifications().is_empty(),
            "an observer of [1] was told its value became [1.0]"
        );

        model
            .set("/one", json!(true))
            .expect("a write of another kind of value");
        assert_eq!(
            model.take_notifications(),
            vec![(scalar.id, json!(true))],
            "a boolean is not the number it happens to equal in Python"
        );
        model
            .set("/one", json!(2))
            .expect("a write of another number");
        assert_eq!(
            model.take_notifications(),
            vec![(scalar.id, json!(2))],
            "a number that differs is still a change"
        );
        model
            .set("/list", json!([2]))
            .expect("a write of another list");
        assert_eq!(
            model.take_notifications(),
            vec![(list.id, json!([2]))],
            "the element spelling is what stayed the same, not the list"
        );
    }

    /// The square the reviews found: `snapshot` compared every observer against every one it had
    /// already collected, and the routing then scanned every observer once per watched path. The
    /// original keys its listeners on the path it watches (`state/data_model.py:169`) and walks
    /// that key map per write (`:331-346`), so a write costs the paths under observation plus the
    /// observers it concerns. The bound is generous by design; the quadratic form needs minutes.
    #[test]
    fn a_write_costs_the_observers_it_concerns() {
        const OBSERVERS: usize = 20_000;
        const WRITES: usize = 200;
        let mut model = DataModel::new();
        let mut ids = Vec::with_capacity(OBSERVERS);
        for index in 0..OBSERVERS {
            let path = format!("/leaf/{index}");
            ids.push(model.subscribe(&path).expect("a watched leaf").id);
        }

        let started = std::time::Instant::now();
        for (index, id) in ids.iter().enumerate().take(WRITES) {
            let path = format!("/leaf/{index}");
            model
                .set(&path, json!(index))
                .expect("a write to a watched leaf");
            assert_eq!(
                model.take_notifications(),
                vec![(*id, json!(index))],
                "write {index} was not heard by its own observer alone"
            );
        }
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(10),
            "{WRITES} writes over {OBSERVERS} watched leaves took {took:?}"
        );
    }

    /// A pointer the model refuses is a pointer that reaches nothing of it: the cap is the nesting
    /// limit `serde_json`'s own parser works to, so that no payload can leave behind a tree the
    /// port cannot clone, read back or drop without running out of stack. 128 is addressable,
    /// 129 is not, and every entry that takes a pointer says so.
    #[test]
    fn a_pointer_of_128_segments_is_addressed_and_129_is_not() {
        let mut model = DataModel::new();
        model
            .set(&pointer(128), json!("leaf"))
            .expect("the deepest pointer the model addresses");
        assert_eq!(
            model.get(&pointer(128)).expect("a read").cloned(),
            Some(json!("leaf")),
            "the deepest allowed pointer was not built"
        );

        for problem in [
            model.set(&pointer(129), json!("leaf")),
            model.get(&pointer(129)).map(drop),
            model.has_path(&pointer(129)).map(drop),
            model.delete(&pointer(129)),
            model.subscribe(&pointer(129)).map(drop),
        ] {
            assert_eq!(
                problem.expect_err("a pointer of 129 segments").category(),
                crate::ErrorCategory::DataError,
                "an over-deep pointer is a data failure, as a forbidden segment is"
            );
        }
        assert_eq!(
            model.get(&pointer(127)).expect("a read").cloned(),
            Some(json!({"s127": "leaf"})),
            "the refused write left a path behind"
        );
    }

    /// The same cap on what is written, checked without walking the value twice: 128 levels of
    /// arrays are held, 129 are refused before anything is built.
    #[test]
    fn a_value_of_128_levels_is_held_and_129_is_not() {
        let mut model = DataModel::new();
        model
            .set("/held", nested(128))
            .expect("the deepest value the model holds");
        let problem = model
            .set("/refused", nested(129))
            .expect_err("a value 129 levels deep");
        assert_eq!(problem.category(), crate::ErrorCategory::DataError);
        assert!(
            !model.has_path("/refused").expect("a well formed pointer"),
            "a refused value left a key behind"
        );
        assert_eq!(
            model.get("/held").expect("a read").cloned(),
            Some(nested(128)),
            "the deepest allowed value was not written whole"
        );
    }

    /// Cloning and dropping are the two recursions a model of the deepest value still has to
    /// survive, and nothing else in the suite walks them.
    #[test]
    fn a_model_holding_the_deepest_value_is_cloned_and_dropped() {
        let mut model = DataModel::new();
        model
            .set("/deep", nested(128))
            .expect("the deepest value the model holds");
        let kept = model.clone();
        drop(model);
        assert_eq!(
            kept.get("/deep").expect("a read").cloned(),
            Some(nested(128)),
            "the clone lost the depth"
        );
        drop(kept);
    }

    /// The original re-reads its live listener list as each path fires
    /// (`state/data_model.py:342`), so an observer that is gone by the time the notification is
    /// handed out never hears the write. Handing out is the host's drain here, which is where the
    /// check has to be.
    #[test]
    fn an_observer_gone_before_the_drain_is_not_handed_its_notification() {
        let mut model = DataModel::with_initial(json!({"a": 1}));
        let observer = model.subscribe("/a").expect("a watched path");
        model
            .set("/a", json!(2))
            .expect("a write to a watched path");
        model.unsubscribe(observer.id);
        assert!(
            model.take_notifications().is_empty(),
            "a detached observer was handed a notification"
        );
    }

    /// `dispose` detaches every observer, so what it dropped was not yet handed out goes with
    /// them.
    #[test]
    fn disposing_drops_what_was_not_handed_out() {
        let mut model = DataModel::with_initial(json!({"a": 1}));
        model.subscribe("/a").expect("a watched path");
        model
            .set("/a", json!(2))
            .expect("a write to a watched path");
        model.dispose();
        assert!(
            model.take_notifications().is_empty(),
            "a disposed model still had a notification to hand out"
        );
    }

    /// The four answers the TypeScript test of the only caller pins
    /// (`message-processor.test.ts:1681-1687`), and the three places where this join and the scope
    /// resolution of `data_context` part ways.
    #[test]
    fn the_model_join_hangs_a_path_under_the_context_it_is_handed() {
        assert_eq!(
            DataModel::resolve_path("/absolute/path", "/base"),
            "/absolute/path",
            "an absolute path ignores the context"
        );
        assert_eq!(
            DataModel::resolve_path("relative", "/base"),
            "/base/relative"
        );
        assert_eq!(
            DataModel::resolve_path("relative", "/base/"),
            "/base/relative"
        );
        assert_eq!(DataModel::resolve_path("standalone", ""), "/standalone");
        assert_eq!(DataModel::resolve_path("", ""), "/");

        // A scope resolution strips the trailing slashes of the scope and answers `.` and `""` with
        // the scope itself; this one takes the context as written and the path as text.
        assert_eq!(DataModel::resolve_path("name", "/user//"), "/user//name");
        assert_eq!(DataModel::resolve_path(".", "/user"), "/user/.");
        assert_eq!(DataModel::resolve_path("", "/user"), "/user/");
        assert_eq!(
            ScopePath::new("/user//").resolve("name").as_deref(),
            Ok("/user/name")
        );
        assert_eq!(ScopePath::new("/user").resolve(".").as_deref(), Ok("/user"));
        assert_eq!(ScopePath::new("/user").resolve("").as_deref(), Ok("/user"));
    }

    /// A pointer of `segments` segments: `/s0/s1/…`.
    fn pointer(segments: usize) -> String {
        let mut path = String::with_capacity(segments * 4);
        for index in 0..segments {
            path.push('/');
            path.push('s');
            path.push_str(&index.to_string());
        }
        path
    }

    /// A value of `levels` levels: that many arrays, each holding the next, around one leaf.
    fn nested(levels: usize) -> Value {
        let mut value = json!("leaf");
        for _ in 0..levels {
            value = Value::Array(vec![value]);
        }
        value
    }
}
