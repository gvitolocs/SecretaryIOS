//! The photo viewer core for the browser: the zoom, pan and swipe-down rules the iPhone runs
//! through `secretary_image_view`, as one JSON function behind the same tiny C-like ABI as
//! `secretary-player-wasm`, so the web app needs no wasm-bindgen glue (web/src/lib/imageCore.ts).
//!
//! - `alloc(len)` / `dealloc(ptr, len)`: buffers the host writes into.
//! - `image_view(ptr, len) -> u64`: one request of `secretary_image::view::handle_json`; the
//!   answer is returned as `ptr << 32 | len` and freed by the host with `dealloc`.
//! - `image_edit(src, w, h, json, json_len) -> u64`: the photo editor's live preview. `src` is an
//!   RGBA buffer of `w`×`h` pixels the host allocated and filled once (it is only read, so the
//!   host keeps it between renders), `json` an edit list (`secretary_image::edit`). The answer is a
//!   buffer `[status u32, width u32, height u32, payload]` (little endian), packed like the
//!   others: status 0 and the edited RGBA pixels, or status 1 and a UTF-8 error message (width and
//!   height 0). Free it with `dealloc`.
//! - `image_auto(src, w, h) -> u64`: auto enhance. A JSON answer `{"ok":true,"adjust":{…}}` computed
//!   from the picture's histogram.

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

/// Leaks `buf` into linear memory and packs its address and length (see [`give`]).
fn give_bytes(buf: Vec<u8>) -> u64 {
    let mut bytes = buf.into_boxed_slice();
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

/// The edited picture of `rgba` (`width`×`height`) under the edit list `json`, as the
/// `[status, width, height, payload]` buffer `image_edit` hands out.
pub fn edit(rgba: &[u8], width: u32, height: u32, json: &str) -> Vec<u8> {
    let result = secretary_image::edit::EditList::from_json(json).and_then(|list| {
        let frame = secretary_image::Frame {
            width,
            height,
            rgba: rgba.to_vec(),
            complete: true,
        };
        secretary_image::edit::render(&frame, &list)
    });
    let (status, w, h, payload) = match result {
        Ok(f) => (0u32, f.width, f.height, f.rgba),
        Err(e) => (1, 0, 0, e.0.into_bytes()),
    };
    let mut out = Vec::with_capacity(12 + payload.len());
    for v in [status, w, h] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&payload);
    out
}

/// Auto-enhance parameters for a picture, as the JSON `image_auto` answers.
pub fn auto(rgba: &[u8], width: u32, height: u32) -> String {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return serde_json::json!({ "ok": false, "error": "wrong buffer size" }).to_string();
    }
    let frame = secretary_image::Frame {
        width,
        height,
        rgba: rgba.to_vec(),
        complete: true,
    };
    let adjust = secretary_image::edit::auto_enhance(&secretary_image::edit::Histogram::of(&frame));
    serde_json::json!({ "ok": true, "adjust": adjust }).to_string()
}

#[no_mangle]
pub extern "C" fn image_edit(
    src: *const u8,
    width: u32,
    height: u32,
    json: *const u8,
    json_len: usize,
) -> u64 {
    let len = width as usize * height as usize * 4;
    give_bytes(edit(
        bytes(src, len),
        width,
        height,
        &String::from_utf8_lossy(bytes(json, json_len)),
    ))
}

#[no_mangle]
pub extern "C" fn image_auto(src: *const u8, width: u32, height: u32) -> u64 {
    give(auto(
        bytes(src, width as usize * height as usize * 4),
        width,
        height,
    ))
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

    #[test]
    fn edits_and_auto_enhance_through_the_buffer_abi() {
        let rgba: Vec<u8> = (0..4 * 3)
            .flat_map(|i| [40, 40, 40, 255 - i as u8])
            .collect();
        let out = edit(&rgba, 4, 3, r#"{"v":1,"adjust":{"exposure":1}}"#);
        let word = |i: usize| u32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!((word(0), word(1), word(2)), (0, 4, 3));
        assert_eq!(out.len(), 12 + 4 * 3 * 4);
        assert!(out[12] > 40 && out[15] == 255, "brighter, alpha kept");
        // Quarter turns swap the size.
        let turned = edit(&rgba, 4, 3, r#"{"v":1,"rotate":1}"#);
        assert_eq!(
            (
                u32::from_le_bytes(turned[4..8].try_into().unwrap()),
                u32::from_le_bytes(turned[8..12].try_into().unwrap())
            ),
            (3, 4)
        );
        // Errors come back as a status and a message, never a panic.
        for (src, w, h, json) in [
            (&rgba[..], 4, 3, "nope"),
            (&rgba[..], 4, 3, r#"{"v":7}"#),
            (&rgba[..8], 4, 3, "{}"),
        ] {
            let bad = edit(src, w, h, json);
            assert_eq!(u32::from_le_bytes(bad[0..4].try_into().unwrap()), 1);
            assert!(!bad[12..].is_empty());
        }
        let dark: Vec<u8> = (0..64).flat_map(|_| [10, 10, 10, 255]).collect();
        let answer: serde_json::Value = serde_json::from_str(&auto(&dark, 8, 8)).unwrap();
        assert_eq!(answer["ok"], true);
        assert!(answer["adjust"]["exposure"].as_f64().unwrap() > 0.5);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&auto(&dark, 8, 7)).unwrap()["ok"],
            false
        );
    }
}
