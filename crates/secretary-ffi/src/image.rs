//! The image viewer core (secretary-image) behind the C ABI: a progressive decoder fed with the
//! bytes as they arrive, ThumbHash placeholders, and the zoom/pan view as JSON.
//!
//! Return codes: `>= 0` ok (`1` = something new, `0` = nothing yet), `< 0` error; the message of
//! the last error on this thread comes from `secretary_image_last_error`.

use std::{
    cell::RefCell,
    ffi::{c_char, CStr},
    panic::{catch_unwind, AssertUnwindSafe},
};

use secretary_image::view::{Gesture, View};
use secretary_image::Decoder;

thread_local! {
    static IMAGE_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

const ERR_NULL: i32 = -1;
const ERR_DECODE: i32 = -2;
const ERR_PANIC: i32 = -3;

fn fail(code: i32, message: impl Into<String>) -> i32 {
    let m = message.into();
    IMAGE_ERROR.with(|s| *s.borrow_mut() = m);
    code
}

fn guarded(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| fail(ERR_PANIC, "panic in the image core"))
}

/// The message of the last failed `secretary_image_*` call on this thread.
/// Free it with `secretary_string_free`.
#[no_mangle]
pub extern "C" fn secretary_image_last_error() -> *mut c_char {
    IMAGE_ERROR.with(|s| super::output(s.borrow().clone()))
}

/// A new decoder for one picture. Free it with `secretary_image_decoder_free`.
#[no_mangle]
pub extern "C" fn secretary_image_decoder_new() -> *mut Decoder {
    catch_unwind(|| Box::into_raw(Box::new(Decoder::new()))).unwrap_or(std::ptr::null_mut())
}

/// Feeds the next `len` bytes; `done` marks the last chunk. Returns 1 when a better frame is
/// ready (read it with `secretary_image_decoder_frame`), 0 when not yet, < 0 on error.
///
/// # Safety
/// `decoder` comes from `secretary_image_decoder_new` and is not freed; `bytes` points to `len`
/// readable bytes (or is NULL with `len == 0`).
#[no_mangle]
pub unsafe extern "C" fn secretary_image_decoder_feed(
    decoder: *mut Decoder,
    bytes: *const u8,
    len: usize,
    done: bool,
) -> i32 {
    guarded(|| {
        let Some(d) = decoder.as_mut() else { return fail(ERR_NULL, "null decoder") };
        if bytes.is_null() && len > 0 {
            return fail(ERR_NULL, "null bytes");
        }
        let chunk = if len == 0 { &[][..] } else { std::slice::from_raw_parts(bytes, len) };
        match d.feed(chunk, done) {
            Ok(better) => better as i32,
            Err(e) => fail(ERR_DECODE, e.to_string()),
        }
    })
}

/// The best frame so far: RGBA8, `width * height * 4` bytes, valid until the next feed or free.
/// Returns 1 with a frame, 0 when there is none yet.
///
/// # Safety
/// `decoder` is live; the out pointers are writable (any may be NULL to skip it).
#[no_mangle]
pub unsafe extern "C" fn secretary_image_decoder_frame(
    decoder: *const Decoder,
    width: *mut u32,
    height: *mut u32,
    rgba: *mut *const u8,
    complete: *mut bool,
) -> i32 {
    guarded(|| {
        let Some(d) = decoder.as_ref() else { return fail(ERR_NULL, "null decoder") };
        let Some(f) = d.frame() else { return 0 };
        if !width.is_null() {
            *width = f.width;
        }
        if !height.is_null() {
            *height = f.height;
        }
        if !rgba.is_null() {
            *rgba = f.rgba.as_ptr();
        }
        if !complete.is_null() {
            *complete = f.complete;
        }
        1
    })
}

/// The picture's size as soon as its header is in (before any frame). Returns 1 when known.
///
/// # Safety
/// As `secretary_image_decoder_frame`.
#[no_mangle]
pub unsafe extern "C" fn secretary_image_decoder_size(decoder: *const Decoder, width: *mut u32, height: *mut u32) -> i32 {
    guarded(|| {
        let Some(d) = decoder.as_ref() else { return fail(ERR_NULL, "null decoder") };
        let Some((w, h)) = d.dimensions() else { return 0 };
        if !width.is_null() {
            *width = w;
        }
        if !height.is_null() {
            *height = h;
        }
        1
    })
}

/// Frees a decoder. NULL is a no-op.
///
/// # Safety
/// `decoder` comes from `secretary_image_decoder_new` and is not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn secretary_image_decoder_free(decoder: *mut Decoder) {
    if !decoder.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(decoder))));
    }
}

/// ThumbHash → a small RGBA picture (at most 32 px), in a buffer owned by the caller: free it
/// with `secretary_image_buffer_free(ptr, len)`. Returns 1 on success.
///
/// # Safety
/// `hash` points to `len` bytes; the out pointers are writable.
#[no_mangle]
pub unsafe extern "C" fn secretary_image_thumbhash(
    hash: *const u8,
    len: usize,
    width: *mut u32,
    height: *mut u32,
    rgba: *mut *mut u8,
    rgba_len: *mut usize,
) -> i32 {
    guarded(|| {
        if hash.is_null() || width.is_null() || height.is_null() || rgba.is_null() || rgba_len.is_null() {
            return fail(ERR_NULL, "null argument");
        }
        match secretary_image::placeholder(std::slice::from_raw_parts(hash, len)) {
            Ok(f) => {
                *width = f.width;
                *height = f.height;
                let boxed = f.rgba.into_boxed_slice();
                *rgba_len = boxed.len();
                *rgba = Box::into_raw(boxed) as *mut u8;
                1
            }
            Err(e) => fail(ERR_DECODE, e.to_string()),
        }
    })
}

/// Frees a buffer from `secretary_image_thumbhash`. NULL is a no-op.
///
/// # Safety
/// `ptr`/`len` are exactly what `secretary_image_thumbhash` returned.
#[no_mangle]
pub unsafe extern "C" fn secretary_image_buffer_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

#[derive(serde::Deserialize)]
struct ViewRequest {
    /// The current view, or `new` for a fresh one.
    view: Option<View>,
    new: Option<[f32; 4]>,
    gesture: Option<Gesture>,
    #[serde(default = "default_preview_px")]
    preview_px: f32,
    #[serde(default = "default_screen_scale")]
    screen_scale: f32,
}

fn default_preview_px() -> f32 {
    1440.0
}
fn default_screen_scale() -> f32 {
    3.0
}

/// Zoom and pan, JSON in and out:
/// `{"new":[image_w,image_h,viewport_w,viewport_h]}` or `{"view":{...},"gesture":{"op":"pinch",
/// "factor":1.2,"x":..,"y":..}}` → `{"ok":true,"view":{...},"rect":{x,y,w,h},"wants_original":bool}`.
/// Free the answer with `secretary_string_free`.
///
/// # Safety
/// `json` is a NUL-terminated UTF-8 string or NULL.
#[no_mangle]
pub unsafe extern "C" fn secretary_image_view(json: *const c_char) -> *mut c_char {
    let answer = catch_unwind(AssertUnwindSafe(|| {
        if json.is_null() {
            return serde_json::json!({"ok": false, "error": "null request"});
        }
        let req: ViewRequest = match CStr::from_ptr(json).to_str().map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(s).map_err(|e| e.to_string())) {
            Ok(r) => r,
            Err(e) => return serde_json::json!({"ok": false, "error": e}),
        };
        let view = match (req.view, req.new) {
            (Some(v), _) => v,
            (None, Some([iw, ih, vw, vh])) => View::new(iw, ih, vw, vh),
            (None, None) => return serde_json::json!({"ok": false, "error": "view or new is required"}),
        };
        let view = match req.gesture {
            Some(g) => view.apply(g),
            None => view,
        };
        serde_json::json!({
            "ok": true,
            "view": view,
            "rect": view.rect(),
            "wants_original": view.wants_original(req.preview_px, req.screen_scale),
        })
    }))
    .unwrap_or_else(|_| serde_json::json!({"ok": false, "error": "panic in the image core"}));
    super::output(answer.to_string())
}
