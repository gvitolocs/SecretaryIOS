//! The player core for the browser: the same engine and JSON protocol the
//! iOS app reaches through `secretary-ffi`, behind a tiny C-like ABI so the
//! web app needs no wasm-bindgen glue (web/src/lib/playerCore.ts).
//!
//! - `alloc(len)` / `dealloc(ptr, len)`: buffers the host writes into.
//! - `player_init(ptr, len)`: starts the engine from a saved state (len 0 = empty).
//! - `player_request(ptr, len, now_ms) -> u64`: one `{"cmd": ...}` request; the
//!   answer is returned as `ptr << 32 | len` and freed by the host with `dealloc`.
//! - `player_state() -> u64`: the whole state, for localStorage.
//!
//! The clock comes from the host: `SystemTime` does not exist on wasm32.
use secretary_player::Engine;
use std::sync::Mutex;

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the host passes a buffer it got from `alloc(len)` and filled.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// Starts the engine from a saved state ("" = empty).
pub fn init(state: &str) {
    let engine = if state.trim().is_empty() { Engine::in_memory() } else { Engine::from_state_json(state) };
    *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine);
}

/// One request at the host's clock.
pub fn request(json: &str, now_ms: f64) -> String {
    let mut guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let engine = guard.get_or_insert_with(Engine::in_memory);
    engine.handle_at(json, now_ms.max(0.0) as u64).to_string()
}

pub fn state() -> String {
    let guard = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map_or_else(|| "{}".to_string(), Engine::state_json)
}

/// Leaks `text` into linear memory and packs its address and length. Only
/// meaningful on wasm32, where addresses fit in 32 bits.
fn give(text: String) -> u64 {
    let mut bytes = text.into_bytes().into_boxed_slice();
    let (ptr, len) = (bytes.as_mut_ptr() as u64, bytes.len() as u64);
    std::mem::forget(bytes);
    (ptr << 32) | len
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = vec![0u8; len].into_boxed_slice();
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr` must come from `alloc(len)` or a `player_*` answer of length `len`.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

#[no_mangle]
pub extern "C" fn player_init(ptr: *const u8, len: usize) {
    init(&String::from_utf8_lossy(bytes(ptr, len)));
}

#[no_mangle]
pub extern "C" fn player_request(ptr: *const u8, len: usize, now_ms: f64) -> u64 {
    give(request(&String::from_utf8_lossy(bytes(ptr, len)), now_ms))
}

#[no_mangle]
pub extern "C" fn player_state() -> u64 {
    give(state())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The packed-pointer ABI is exercised by web/src/lib/playerCore.test.ts against the
    // built .wasm; here the same calls run natively.
    #[test]
    fn requests_and_state_survive_a_reload() {
        init("");
        let ask = |json: &str, now: f64| -> serde_json::Value { serde_json::from_str(&request(json, now)).unwrap() };
        assert_eq!(ask(r#"{"cmd":"position","id":"a","position_ms":600000,"duration_ms":3600000,"playing":false}"#, 5_000.0)["sync"], true);
        let saved = state();
        init(&saved);
        assert_eq!(ask(r#"{"cmd":"resume","id":"a"}"#, 6_000.0)["position_ms"], 598_000);
        assert_eq!(ask(r#"{"cmd":"nope"}"#, 0.0)["code"], "invalid_command");
    }
}
