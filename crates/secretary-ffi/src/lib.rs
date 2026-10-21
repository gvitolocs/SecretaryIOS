//! Owned C strings and one mutex-protected module engine per handle.
//!
//! Swift owns handle lifetime. Every entry point catches panics so an
//! internal failure can never unwind across the C ABI.
use std::{
    cell::RefCell,
    ffi::{c_char, CStr, CString},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Mutex,
};

thread_local! {
    static OPEN_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn output(text: String) -> *mut c_char {
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{}").unwrap())
        .into_raw()
}

fn failure(error: &str) -> String {
    serde_json::json!({
        "ok": false,
        "replayed": false,
        "code": "bridge_error",
        "error": error,
    })
    .to_string()
}

/// Returns the error from the last failed `secretary_<module>_open` call on
/// this thread, as an owned C string. Free it with `secretary_string_free`.
#[no_mangle]
pub extern "C" fn secretary_open_error() -> *mut c_char {
    OPEN_ERROR.with(|s| output(s.borrow().clone()))
}

/// Frees a string returned by this library. Passing NULL is a no-op.
///
/// # Safety
/// `value` must be a non-freed pointer returned by this bridge, or NULL.
#[no_mangle]
pub unsafe extern "C" fn secretary_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(CString::from_raw(value));
    }
}

/// Returns build information as JSON: `{"version":"...","modules":[...]}`.
/// Free it with `secretary_string_free`.
#[no_mangle]
pub extern "C" fn secretary_version() -> *mut c_char {
    output(
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "modules": ["gym"],
        })
        .to_string(),
    )
}

#[doc(hidden)]
unsafe fn open_engine<E>(path: *const c_char) -> *mut Mutex<E>
where
    E: OpenModule,
{
    let result = catch_unwind(AssertUnwindSafe(|| {
        if path.is_null() {
            return Err("Missing storage path".into());
        }
        let path = CStr::from_ptr(path).to_str().map_err(|e| e.to_string())?;
        E::open(path)
    }));
    match result {
        Ok(Ok(engine)) => Box::into_raw(Box::new(Mutex::new(engine))),
        other => {
            let error = match other {
                Ok(Err(e)) => e,
                _ => "Could not initialize the Rust core".into(),
            };
            OPEN_ERROR.with(|s| *s.borrow_mut() = error);
            std::ptr::null_mut()
        }
    }
}

#[doc(hidden)]
unsafe fn request_engine<E>(handle: *mut Mutex<E>, json: *const c_char) -> *mut c_char
where
    E: RequestModule,
{
    let result = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || json.is_null() {
            return failure("Missing handle or command");
        }
        let Ok(input) = CStr::from_ptr(json).to_str() else {
            return failure("Command must be UTF-8");
        };
        let Ok(mut engine) = (*handle).lock() else {
            return failure("Core lock is unavailable");
        };
        serde_json::to_string(&engine.handle(input))
            .unwrap_or_else(|_| failure("Response could not be encoded"))
    }));
    output(result.unwrap_or_else(|_| failure("Rust core rejected an internal failure")))
}

#[doc(hidden)]
unsafe fn close_engine<E>(handle: *mut Mutex<E>) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

trait OpenModule: Sized {
    fn open(path: &str) -> std::result::Result<Self, String>;
}

trait RequestModule {
    fn handle(&mut self, json: &str) -> secretary_gym::Response;
}

impl OpenModule for secretary_gym::Engine {
    fn open(path: &str) -> std::result::Result<Self, String> {
        secretary_gym::Engine::open(path)
    }
}

impl RequestModule for secretary_gym::Engine {
    fn handle(&mut self, json: &str) -> secretary_gym::Response {
        secretary_gym::Engine::handle(self, json)
    }
}

macro_rules! module {
    ($module:ident, $open:ident, $request:ident, $close:ident, $engine:path) => {
        /// Opens the engine at `path`, creating parent directories if needed.
        /// Returns NULL on error; call `secretary_open_error`.
        ///
        /// # Safety
        /// `path` must be a valid NUL-terminated UTF-8 string for this call.
        #[no_mangle]
        pub unsafe extern "C" fn $open(path: *const c_char) -> *mut Mutex<$engine> {
            open_engine::<$engine>(path)
        }

        /// Handles one JSON request and returns an owned JSON response.
        /// Free it with `secretary_string_free`.
        ///
        /// # Safety
        /// `handle` must be live and returned by the matching open function;
        /// `json` must be a valid NUL-terminated UTF-8 string. No close may
        /// race an in-flight request.
        #[no_mangle]
        pub unsafe extern "C" fn $request(
            handle: *mut Mutex<$engine>,
            json: *const c_char,
        ) -> *mut c_char {
            request_engine(handle, json)
        }

        /// Closes the engine and releases its file lock.
        ///
        /// # Safety
        /// `handle` must be live, returned by this bridge, and have no
        /// in-flight calls.
        #[no_mangle]
        pub unsafe extern "C" fn $close(handle: *mut Mutex<$engine>) {
            close_engine(handle);
        }
    };
}

module!(
    gym,
    secretary_gym_open,
    secretary_gym_request,
    secretary_gym_close,
    secretary_gym::Engine
);

/// Alias of `secretary_open_error` for the SwiftUI port.
#[no_mangle]
pub extern "C" fn secretary_gym_open_error() -> *mut c_char {
    secretary_open_error()
}

/// Alias of `secretary_string_free` for the SwiftUI port.
///
/// # Safety
/// `value` must be a non-freed pointer returned by this bridge, or NULL.
#[no_mangle]
pub unsafe extern "C" fn secretary_gym_string_free(value: *mut c_char) {
    secretary_string_free(value)
}
