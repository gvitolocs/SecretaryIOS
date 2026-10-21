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
    assert_eq!(version["modules"], serde_json::json!(["gym"]));

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
