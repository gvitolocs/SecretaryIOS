//! `secretary_chat_request` through the C ABI: one test per op, plus the error paths.
use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::ptr;

fn ask(request: &Value) -> Value {
    let request = CString::new(request.to_string()).unwrap();
    take(unsafe { secretary_ffi::secretary_chat_request(request.as_ptr()) })
}

fn take(value: *mut c_char) -> Value {
    assert!(!value.is_null());
    let text = unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned();
    unsafe { secretary_ffi::secretary_string_free(value) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

#[test]
fn merge_replaces_the_local_bubble_and_orders_the_rest() {
    let answer = ask(&json!({
        "op": "merge",
        "current": [
            {"id": "local-c1", "ts_us": 100_000_000, "role": "user", "client_message_id": "c1"},
            {"id": "local-r1", "ts_us": 101_000_000, "role": "lucina", "reply_to": "local-c1"},
            {"id": "tl_1", "ts_us": 50_000_000, "role": "lucina"},
        ],
        "incoming": [
            {"id": "tl_9", "ts_us": 100_500_000, "role": "user", "client_message_id": "c1"},
        ],
    }));
    assert_eq!(answer["ids"], json!(["tl_1", "tl_9", "local-r1"]));
    assert_eq!(answer["replaced"], json!([{"local": "local-c1", "server": "tl_9"}]));
    assert_eq!(answer["reply_to_updates"], json!([{"id": "local-r1", "reply_to": "tl_9"}]));
}

#[test]
fn merge_handles_empty_input_and_a_user_before_lucina_tie() {
    assert_eq!(ask(&json!({"op": "merge", "current": [], "incoming": []})), json!({"ids": [], "replaced": [], "reply_to_updates": []}));
    let answer = ask(&json!({
        "op": "merge",
        "current": [],
        "incoming": [
            {"id": "a", "ts_us": 5, "role": "lucina"},
            {"id": "z", "ts_us": 5, "role": "user"},
        ],
    }));
    assert_eq!(answer["ids"], json!(["z", "a"]));
}

#[test]
fn sync_next_walks_a_cursor_run() {
    let a = ask(&json!({"op": "sync_next", "plan": null, "cursor": "c0", "outcome": {"kind": "started"}}));
    assert_eq!(a["action"], json!({"kind": "fetch", "after": "c0", "limit": 200}));
    let plan = a["plan"].clone();
    let b = ask(&json!({"op": "sync_next", "plan": plan, "outcome": {"kind": "page", "cursor": "c1", "has_more": true}}));
    assert_eq!(b["action"], json!({"kind": "apply", "reset": false}));
    let c = ask(&json!({"op": "sync_next", "plan": b["plan"], "outcome": {"kind": "applied"}}));
    assert_eq!(c["action"], json!({"kind": "fetch", "after": "c1", "limit": 200}));
    // the cursor expired: a fresh window, applied as a reset
    let d = ask(&json!({"op": "sync_next", "plan": c["plan"], "outcome": {"kind": "gone410"}}));
    assert_eq!(d["action"], json!({"kind": "fetch", "after": null, "limit": 50}));
    let e = ask(&json!({"op": "sync_next", "plan": d["plan"], "outcome": {"kind": "page", "cursor": "fresh", "has_more": false}}));
    assert_eq!(e["action"], json!({"kind": "apply", "reset": true}));
    let f = ask(&json!({"op": "sync_next", "plan": e["plan"], "outcome": {"kind": "applied"}}));
    assert_eq!(f["action"], json!({"kind": "done"}));
}

#[test]
fn sync_next_without_a_cursor_and_on_failure() {
    let a = ask(&json!({"op": "sync_next", "plan": null, "outcome": {"kind": "started"}}));
    assert_eq!(a["action"], json!({"kind": "fetch", "after": null, "limit": 50}));
    let b = ask(&json!({"op": "sync_next", "plan": a["plan"], "outcome": {"kind": "failed"}}));
    assert_eq!(b["action"], json!({"kind": "abort"}));
}

#[test]
fn older_returns_the_cursor_and_the_flag() {
    let answer = ask(&json!({"op": "older", "ids": ["tl_5", "local-x", "tl_3"], "stored": null, "page_len": 60, "page_size": 60}));
    assert_eq!(answer, json!({"older_cursor": "tl_3", "has_older": true}));
    let answer = ask(&json!({"op": "older", "ids": ["tl_5"], "stored": "tl_1", "page_len": 10, "page_size": 60}));
    assert_eq!(answer, json!({"older_cursor": "tl_1", "has_older": true}));
    let answer = ask(&json!({"op": "older", "ids": [], "stored": "", "page_len": 0, "page_size": 60}));
    assert_eq!(answer, json!({"older_cursor": null, "has_older": false}));
}

#[test]
fn malformed_requests_answer_with_an_error_and_never_crash() {
    for bad in ["not json", "{}", r#"{"op":"merge"}"#, r#"{"op":"nope"}"#, r#"{"op":"merge","current":[{"id":1}],"incoming":[]}"#] {
        let request = CString::new(bad).unwrap();
        let answer = take(unsafe { secretary_ffi::secretary_chat_request(request.as_ptr()) });
        assert!(answer["error"].is_string(), "{bad}: {answer}");
    }
    let answer = take(unsafe { secretary_ffi::secretary_chat_request(ptr::null()) });
    assert_eq!(answer["error"], "Missing command");
    let invalid_utf8 = [0xffu8, 0xfe, 0];
    let answer = take(unsafe { secretary_ffi::secretary_chat_request(invalid_utf8.as_ptr() as *const c_char) });
    assert_eq!(answer["error"], "Command must be UTF-8");
}

#[test]
fn the_header_declares_it_and_the_version_lists_the_module() {
    assert!(include_str!("../include/SecretaryCore.h").contains("secretary_chat_request("));
    let version = take(secretary_ffi::secretary_version());
    assert!(version["modules"].as_array().unwrap().iter().any(|m| m == "chat"));
}
