use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::ptr;

const EXPORTED: &[&str] = &[
    "secretary_open_error",
    "secretary_string_free",
    "secretary_version",
    "secretary_gym_open",
    "secretary_gym_request",
    "secretary_gym_close",
    "secretary_gym_open_error",
    "secretary_gym_string_free",
    "secretary_body_open",
    "secretary_body_request",
    "secretary_body_close",
];

unsafe fn take(value: *mut c_char) -> String {
    assert!(!value.is_null());
    let text = CStr::from_ptr(value).to_string_lossy().into_owned();
    secretary_ffi::secretary_string_free(value);
    text
}

unsafe fn take_gym(value: *mut c_char) -> String {
    assert!(!value.is_null());
    let text = CStr::from_ptr(value).to_string_lossy().into_owned();
    secretary_ffi::secretary_gym_string_free(value);
    text
}

#[test]
fn gym_request_roundtrip_through_the_c_abi() {
    let dir = tempfile::tempdir().unwrap();
    let path = CString::new(dir.path().join("gym.json").to_str().unwrap()).unwrap();
    let handle = unsafe { secretary_ffi::secretary_gym_open(path.as_ptr()) };
    assert!(!handle.is_null());

    let request = CString::new(r#"{"command":{"type":"snapshot"}}"#).unwrap();
    let response = unsafe { secretary_ffi::secretary_gym_request(handle, request.as_ptr()) };
    let json: Value = serde_json::from_str(&unsafe { take(response) }).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["view"]["summary"]["workouts"], 0);

    let version: Value =
        serde_json::from_str(&unsafe { take(secretary_ffi::secretary_version()) }).unwrap();
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(version["modules"], serde_json::json!(["gym", "body", "player", "image", "chat"]));

    unsafe { secretary_ffi::secretary_gym_close(handle) };
}

#[test]
fn null_handle_or_json_returns_bridge_error_json() {
    let request = CString::new("{}").unwrap();
    let response =
        unsafe { secretary_ffi::secretary_gym_request(ptr::null_mut(), request.as_ptr()) };
    let json: Value = serde_json::from_str(&unsafe { take_gym(response) }).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["code"], "bridge_error");

    let response = unsafe { secretary_ffi::secretary_gym_request(ptr::null_mut(), ptr::null()) };
    let json: Value = serde_json::from_str(&unsafe { take_gym(response) }).unwrap();
    assert_eq!(json["code"], "bridge_error");
}

#[test]
fn second_open_of_the_same_path_fails_with_an_explanation() {
    let dir = tempfile::tempdir().unwrap();
    let path = CString::new(dir.path().join("gym.json").to_str().unwrap()).unwrap();
    let first = unsafe { secretary_ffi::secretary_gym_open(path.as_ptr()) };
    assert!(!first.is_null());

    let second = unsafe { secretary_ffi::secretary_gym_open(path.as_ptr()) };
    assert!(second.is_null());
    let error = unsafe { take_gym(secretary_ffi::secretary_gym_open_error()) };
    assert!(error.contains("already open"), "{error}");

    unsafe { secretary_ffi::secretary_gym_close(first) };
    let reopened = unsafe { secretary_ffi::secretary_gym_open(path.as_ptr()) };
    assert!(!reopened.is_null());
    unsafe { secretary_ffi::secretary_gym_close(reopened) };
}

#[test]
fn header_declares_every_exported_symbol() {
    let header = include_str!("../include/SecretaryCore.h");
    for name in EXPORTED {
        assert!(
            header.contains(&format!("{name}(")),
            "SecretaryCore.h is missing {name}"
        );
    }
}

#[test]
fn body_module_uses_the_same_open_request_close_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let path = CString::new(dir.path().join("body.json").to_str().unwrap()).unwrap();
    let handle = unsafe { secretary_ffi::secretary_body_open(path.as_ptr()) };
    assert!(!handle.is_null());
    let log = CString::new(
        r#"{"op_id":"ffi-1","expected_revision":0,"command":{"type":"log_entry","weight":"80.5"}}"#,
    )
    .unwrap();
    let json: Value = serde_json::from_str(&unsafe {
        take(secretary_ffi::secretary_body_request(handle, log.as_ptr()))
    })
    .unwrap();
    assert_eq!(json["ok"], true, "{json}");
    assert_eq!(json["view"]["entries"][0]["weight_grams"], 80_500);
    assert_eq!(json["view"]["stats"]["avg7_grams"], 80_500);
    let again: Value = serde_json::from_str(&unsafe {
        take(secretary_ffi::secretary_body_request(handle, log.as_ptr()))
    })
    .unwrap();
    assert_eq!(again["replayed"], true);

    let second = unsafe { secretary_ffi::secretary_body_open(path.as_ptr()) };
    assert!(second.is_null());
    let error = unsafe { take(secretary_ffi::secretary_open_error()) };
    assert!(error.contains("already open"), "{error}");
    unsafe { secretary_ffi::secretary_body_close(handle) };
    let reopened = unsafe { secretary_ffi::secretary_body_open(path.as_ptr()) };
    assert!(!reopened.is_null());
    unsafe { secretary_ffi::secretary_body_close(reopened) };
}

#[test]
fn player_queue_and_resume_through_the_c_abi() {
    let dir = tempfile::tempdir().unwrap();
    let path = CString::new(dir.path().join("player.json").to_str().unwrap()).unwrap();
    let handle = unsafe { secretary_ffi::secretary_player_open(path.as_ptr()) };
    assert!(!handle.is_null());
    let ask = |json: &str| -> Value {
        let request = CString::new(json).unwrap();
        let response = unsafe { secretary_ffi::secretary_player_request(handle, request.as_ptr()) };
        serde_json::from_str(&unsafe { take(response) }).unwrap()
    };
    let json = ask(r#"{"cmd":"queue_set","items":[{"id":"v1","kind":"video","title":"Film"}]}"#);
    assert_eq!(json["queue"]["item"]["id"], "v1");
    let json = ask(r#"{"cmd":"position","id":"v1","position_ms":120000,"duration_ms":5400000,"playing":false}"#);
    assert_eq!(json["sync"], true);
    assert_eq!(ask(r#"{"cmd":"resume","id":"v1"}"#)["position_ms"], 118000);
    unsafe { secretary_ffi::secretary_player_close(handle) };
}

#[test]
fn image_decoder_thumbhash_and_view_through_the_c_abi() {
    use std::ffi::CString;
    let mut png = Vec::new();
    image::DynamicImage::new_rgb8(40, 20)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    unsafe {
        let d = secretary_ffi::image::secretary_image_decoder_new();
        assert_eq!(secretary_ffi::image::secretary_image_decoder_feed(d, png.as_ptr(), 10, false), 0);
        assert_eq!(secretary_ffi::image::secretary_image_decoder_feed(d, png[10..].as_ptr(), png.len() - 10, true), 1);
        let (mut w, mut h, mut px, mut done) = (0u32, 0u32, std::ptr::null::<u8>(), false);
        assert_eq!(secretary_ffi::image::secretary_image_decoder_frame(d, &mut w, &mut h, &mut px, &mut done), 1);
        assert_eq!((w, h, done), (40, 20, true));
        assert!(!px.is_null());
        secretary_ffi::image::secretary_image_decoder_free(d);
        // Null decoder: an error code, never a crash.
        assert!(secretary_ffi::image::secretary_image_decoder_feed(std::ptr::null_mut(), png.as_ptr(), 1, false) < 0);
        let msg = take(secretary_ffi::image::secretary_image_last_error());
        assert!(msg.contains("null"), "{msg}");

        let rgba = vec![200u8; 16 * 8 * 4];
        let hash = thumbhash::rgba_to_thumb_hash(16, 8, &rgba);
        let (mut buf, mut len) = (std::ptr::null_mut::<u8>(), 0usize);
        assert_eq!(secretary_ffi::image::secretary_image_thumbhash(hash.as_ptr(), hash.len(), &mut w, &mut h, &mut buf, &mut len), 1);
        assert_eq!(len, (w * h * 4) as usize);
        secretary_ffi::image::secretary_image_buffer_free(buf, len);

        let req = CString::new(r#"{"new":[1440,960,390,844],"gesture":{"op":"double_tap","x":195,"y":422}}"#).unwrap();
        let answer: serde_json::Value = serde_json::from_str(&take(secretary_ffi::image::secretary_image_view(req.as_ptr()))).unwrap();
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["view"]["scale"], 2.5);

        // Swipe down on a fitted photo: released past 12% of the height closes the viewer.
        let req = CString::new(r#"{"new":[1440,960,390,844],"drag":{"dx":4,"dy":150,"predicted_dy":160,"ended":true}}"#).unwrap();
        let answer: serde_json::Value = serde_json::from_str(&take(secretary_ffi::image::secretary_image_view(req.as_ptr()))).unwrap();
        assert_eq!(answer["drag"]["axis"], "out");
        assert_eq!(answer["drag"]["end"], "dismiss");
        assert!(answer["drag"]["out"]["backdrop"].as_f64().unwrap() < 1.0);
    }
}
