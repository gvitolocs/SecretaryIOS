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

/// The image viewer core (secretary-image): its own binary entry points, not a JSON engine.
pub mod image;

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
            "modules": MODULES,
        })
        .to_string(),
    )
}

/// One feature engine behind the C ABI: open a file, answer JSON with JSON.
trait Module: Sized {
    fn open(path: &str) -> std::result::Result<Self, String>;
    fn request(&mut self, json: &str) -> String;
}

unsafe fn open_engine<E: Module>(path: *const c_char) -> *mut Mutex<E> {
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

unsafe fn request_engine<E: Module>(handle: *mut Mutex<E>, json: *const c_char) -> *mut c_char {
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
        engine.request(input)
    }));
    output(result.unwrap_or_else(|_| failure("Rust core rejected an internal failure")))
}

unsafe fn close_engine<E>(handle: *mut Mutex<E>) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Exposes one engine type as `secretary_<module>_{open,request,close}`.
/// The engine needs `open(path) -> Result<Self, String>` and
/// `handle(&mut self, &str) -> impl Serialize`. Adding a module is one
/// invocation plus its lines in `SecretaryCore.h` and `MODULES`.
macro_rules! module {
    ($engine:ty, $open:ident, $request:ident, $close:ident) => {
        impl Module for $engine {
            fn open(path: &str) -> std::result::Result<Self, String> {
                <$engine>::open(path)
            }
            fn request(&mut self, json: &str) -> String {
                serde_json::to_string(&<$engine>::handle(self, json))
                    .unwrap_or_else(|_| failure("Response could not be encoded"))
            }
        }

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

/// The chat engine's pure kernel (secretary-chat): merge, sync cursor loop, pagination. One JSON request in, one
/// JSON response out, no handle and no state: see `secretary_chat::request` and `docs/server-api.md`.
///
/// # Safety
/// `json` must be a valid NUL-terminated UTF-8 string for this call, or NULL (answered with an error).
#[no_mangle]
pub unsafe extern "C" fn secretary_chat_request(json: *const c_char) -> *mut c_char {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if json.is_null() {
            return chat_error("Missing command");
        }
        match CStr::from_ptr(json).to_str() {
            Ok(input) => secretary_chat::handle(input),
            Err(_) => chat_error("Command must be UTF-8"),
        }
    }));
    output(result.unwrap_or_else(|_| chat_error("Rust core rejected an internal failure")))
}

fn chat_error(error: &str) -> String {
    serde_json::json!({ "error": error }).to_string()
}

/// An Obsidian vault (lucina-vault): `path` is the vault's folder. Requests and answers are its protocol: docs/lucina-vault.md.
/// Opens read-only; `{"op":"set_writable","writable":true}` says otherwise.
pub struct VaultEngine(lucina_vault::protocol::Session);

impl VaultEngine {
    fn open(path: &str) -> std::result::Result<Self, String> {
        lucina_vault::protocol::Session::open(path)
            .map(VaultEngine)
            .map_err(|e| e.to_string())
    }
    fn handle(&mut self, json: &str) -> serde_json::Value {
        serde_json::from_str(&self.0.request(json)).unwrap_or(serde_json::Value::Null)
    }
}

module!(
    VaultEngine,
    secretary_vault_open,
    secretary_vault_request,
    secretary_vault_close
);

/// Module names reported by `secretary_version`.
pub const MODULES: &[&str] = &["gym", "body", "player", "image", "chat", "vault"];

module!(
    secretary_gym::Engine,
    secretary_gym_open,
    secretary_gym_request,
    secretary_gym_close
);
module!(
    secretary_body::Engine,
    secretary_body_open,
    secretary_body_request,
    secretary_body_close
);
module!(
    secretary_player::Engine,
    secretary_player_open,
    secretary_player_request,
    secretary_player_close
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
