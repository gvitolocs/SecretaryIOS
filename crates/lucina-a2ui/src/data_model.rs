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

use serde_json::{Map, Value};

use crate::error::Error;

/// The largest array index a write may address, so that a pointer in a payload cannot make the
/// model allocate an array of any size it names. The original's `MAX_ARRAY_INDEX`.
pub const MAX_ARRAY_INDEX: u64 = 10_000;

/// The path segments a pointer may never address: the prototype chain of the original's JavaScript
/// implementation. The original's `FORBIDDEN_KEYS`.
pub const FORBIDDEN_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

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

/// One registered observer: its handle, and the path it watches split into unescaped segments, so
/// that two spellings of one path are one path for the notification routing.
#[derive(Clone, Debug)]
struct Observer {
    id: WatchId,
    segments: Vec<String>,
}

/// The reactive data model of one surface.
#[derive(Clone, Debug)]
pub struct DataModel {
    /// The whole model: the original's `_data`.
    data: Value,
    /// Every registered observer, oldest first. The original keeps a dict of normalized path to
    /// list of callbacks, which is the same order seen from the other side.
    observers: Vec<Observer>,
    /// The notifications the writes since the last call produced, oldest first, one entry per
    /// observer that fired.
    pending: Vec<(WatchId, Value)>,
    next_id: u64,
}

impl DataModel {
    /// A model holding an empty object.
    pub fn new() -> DataModel {
        DataModel {
            data: Value::Object(Map::new()),
            observers: Vec::new(),
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

    /// The value at `path`, or `None` when the model holds nothing there.
    ///
    /// A path that names the root (`""`, `"/"`, or any number of empty segments) is the whole
    /// model, and a path through a primitive, through a list with a segment that is not an index,
    /// or past the end of a list is nothing.
    ///
    /// # Errors
    /// A `DataError` for a segment the model refuses to address, which is what a forbidden
    /// segment is.
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
    /// A `DataError` for a forbidden segment, a non-numeric segment on an array, an index past
    /// [`MAX_ARRAY_INDEX`], or a write that would go through a primitive. Nothing is rolled back:
    /// the original raises from the same point of its own walk, containers it already built stay.
    pub fn set(&mut self, path: &str, value: Value) -> Result<(), Error> {
        let tokens = parse_pointer(path)?;
        if value.is_null() && find(&self.data, &tokens).is_none() {
            return Ok(());
        }
        let before = self.snapshot();

        if tokens.is_empty() {
            self.data = if value.is_null() {
                Value::Object(Map::new())
            } else {
                value
            };
            self.cascade(&tokens, &before);
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
        self.cascade(&tokens, &before);
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
        self.observers.push(Observer { id, segments });
        Ok(Watch { id, initial })
    }

    /// Detaches one observer. Detaching one that is gone already does nothing.
    pub fn unsubscribe(&mut self, id: WatchId) {
        self.observers.retain(|observer| observer.id != id);
    }

    /// The notifications the writes since the last call produced, oldest first, one entry per
    /// observer that fired.
    pub fn take_notifications(&mut self) -> Vec<(WatchId, Value)> {
        std::mem::take(&mut self.pending)
    }

    /// Detaches every observer, leaving the data where it is.
    pub fn dispose(&mut self) {
        self.observers.clear();
        self.pending.clear();
    }

    /// What every path under observation holds, once per path, in the order the paths were first
    /// registered. The original snapshots the watched values before it mutates, so that a
    /// notification can be compared against what the observer really saw.
    fn snapshot(&self) -> Vec<Sighted> {
        let mut seen: Vec<Sighted> = Vec::new();
        for observer in &self.observers {
            if seen
                .iter()
                .any(|sighted| sighted.segments == observer.segments)
            {
                continue;
            }
            seen.push(Sighted {
                segments: observer.segments.clone(),
                value: find(&self.data, &observer.segments).cloned(),
            });
        }
        seen
    }

    /// The observers that have to hear about a write, because they watch the written path or a
    /// path on either side of it, and the value at their path changed.
    fn cascade(&mut self, written: &[String], before: &[Sighted]) {
        for sighted in before {
            if !speaks_for(&sighted.segments, written) {
                continue;
            }
            let now = find(&self.data, &sighted.segments).cloned();
            if now == sighted.value {
                continue;
            }
            let value = now.unwrap_or(Value::Null);
            for observer in self
                .observers
                .iter()
                .filter(|observer| observer.segments == sighted.segments)
            {
                self.pending.push((observer.id, value.clone()));
            }
        }
    }
}

impl Default for DataModel {
    /// The same as [`DataModel::new`]: an absent model is an empty object, never nothing.
    fn default() -> DataModel {
        DataModel::new()
    }
}

/// A watched path and what it held, for the notification routing.
struct Sighted {
    segments: Vec<String>,
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
/// a segment is one the model never addresses.
///
/// Empty segments are not segments, so `/foo/`, `//foo` and `/foo` are one path, as the suite's
/// `test_data_model_normalizes_empty_path_segments` states. A lone `/` or an empty path is the
/// root.
fn parse_pointer(path: &str) -> Result<Vec<String>, Error> {
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
