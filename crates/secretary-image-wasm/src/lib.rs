//! The photo viewer core for the browser: the zoom, pan and swipe-down rules the iPhone runs
//! through `secretary_image_view`, as one JSON function behind the same tiny C-like ABI as
//! `secretary-player-wasm`, so the web app needs no wasm-bindgen glue (web/src/lib/imageCore.ts).
//!
//! - `alloc(len)` / `dealloc(ptr, len)`: buffers the host writes into.
//! - `image_view(ptr, len) -> u64`: one request of `secretary_image::view::handle_json`; the
//!   answer is returned as `ptr << 32 | len` and freed by the host with `dealloc`.

fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the host passes a buffer it got from `alloc(len)` and filled.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// One view request (JSON in, JSON out).
pub fn view(json: &str) -> String {
    secretary_image::view::handle_json(json).to_string()
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
/// `ptr` must come from `alloc(len)` or an `image_view` answer of length `len`.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

#[no_mangle]
pub extern "C" fn image_view(ptr: *const u8, len: usize) -> u64 {
    give(view(&String::from_utf8_lossy(bytes(ptr, len))))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The packed-pointer ABI is exercised by web/src/lib/imageCore.test.ts against the built
    // .wasm; here the same calls run natively.
    #[test]
    fn answers_like_the_iphone_core() {
        let ask = |json: &str| -> serde_json::Value { serde_json::from_str(&view(json)).unwrap() };
        let fitted = ask(r#"{"new":[1440,960,390,844]}"#);
        assert_eq!(fitted["rect"]["w"], 390.0);
        let zoomed = ask(&format!(
            r#"{{"view":{},"gesture":{{"op":"double_tap","x":195,"y":422}}}}"#,
            fitted["view"]
        ));
        assert_eq!(zoomed["view"]["scale"], 2.5);
        let out = ask(&format!(
            r#"{{"view":{},"drag":{{"dx":0,"dy":200,"predicted_dy":220,"ended":true}}}}"#,
            fitted["view"]
        ));
        assert_eq!(
            (out["drag"]["axis"].as_str(), out["drag"]["end"].as_str()),
            (Some("out"), Some("dismiss"))
        );
        assert_eq!(ask("{}")["ok"], false);
        assert_eq!(ask("not json")["ok"], false);
    }
}
