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

use serde_json::Value;

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

/// One registered observer: what it watches, and how its path splits.
#[derive(Clone, Debug)]
struct Observer {
    id: WatchId,
    /// The watched path as unescaped segments, the original's normalized pointer: two spellings of
    /// one path are one path.
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
            data: Value::Object(serde_json::Map::new()),
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
    /// model.
    pub fn get(&self, path: &str) -> Result<Option<&Value>, Error> {
        let _ = path;
        Err(not_built())
    }

    /// Whether `path` is physically there, which tells a stored `null` from an absent key.
    pub fn has_path(&self, path: &str) -> Result<bool, Error> {
        let _ = path;
        Err(not_built())
    }

    /// Writes `value` at `path`, building the containers on the way that are not there yet.
    ///
    /// A `Value::Null` is the original's removal: it deletes the key, or empties the array element
    /// it addresses without lengthening the list. Writing at the root replaces the whole model.
    pub fn set(&mut self, path: &str, value: Value) -> Result<(), Error> {
        let _ = (path, &value);
        Err(not_built())
    }

    /// Removes what `path` holds: the original's `set(path, None)`.
    pub fn delete(&mut self, path: &str) -> Result<(), Error> {
        self.set(path, Value::Null)
    }

    /// Registers an observer of `path`, which hears about a write to that path, to any path
    /// containing it or to any path it contains, and only when the value there changed.
    pub fn subscribe(&mut self, path: &str) -> Result<Watch, Error> {
        let _ = path;
        Err(not_built())
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
}

impl Default for DataModel {
    /// The same as [`DataModel::new`]: an absent model is an empty object, never nothing.
    fn default() -> DataModel {
        DataModel::new()
    }
}

/// Whether a segment of a parsed pointer is a canonical array index: `0`, or digits with no
/// leading zero. Anything else — `01`, `-1`, `1.0` — addresses no element, which is RFC 6901
/// section 4 and the original's `NUMERIC_PATTERN` of `^(?:0|[1-9][0-9]*)$`.
fn is_index(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0')
}

/// The message of every operation this commit has not built yet: a failure in the vocabulary of
/// the port, rather than a panic the suites would only report as a crash.
const NOT_BUILT: &str = "not implemented in this commit";

fn not_built() -> Error {
    Error::state(NOT_BUILT)
}
