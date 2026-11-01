//! JNI surface for `tech.getsecretary.lucinaotp.Native` (`docs/lucina-android.md`,
//! "JNI surface").
//!
//! Thin: each function converts its arguments, runs the process-wide
//! [`Bridge`] under a lock and returns JSON. Every entry point is wrapped in
//! `catch_unwind`; a panic, a JNI failure or a call before `init` returns
//! `{"result":"error"}`. Message text and tokens are copied into buffers that
//! are wiped when dropped.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jboolean, jlong, jstring, JNI_FALSE};
use jni::JNIEnv;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::Bridge;

/// Returned on any failure.
pub const ERROR_JSON: &str = r#"{"result":"error"}"#;

/// The process's bridge; `None` until `init` succeeds.
static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);

fn json(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value).ok()
}

/// Runs `f` on the bridge, if `init` has run.
fn with_bridge<T>(f: impl FnOnce(&mut Bridge) -> T) -> Option<T> {
    let mut guard = BRIDGE.lock().unwrap_or_else(PoisonError::into_inner);
    guard.as_mut().map(f)
}

/// A Java string as a Rust string wiped on drop; `None` for null.
fn string(env: &mut JNIEnv, s: &JString) -> Option<Zeroizing<String>> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(|s| Zeroizing::new(s.into()))
}

/// Runs `f`, catching panics, and returns its JSON (or [`ERROR_JSON`]) as a
/// new Java string.
fn run(env: &mut JNIEnv, f: impl FnOnce(&mut JNIEnv) -> Option<String>) -> jstring {
    let out = catch_unwind(AssertUnwindSafe(|| f(env)))
        .ok()
        .flatten()
        .unwrap_or_else(|| ERROR_JSON.to_owned());
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    env.new_string(out)
        .map_or(std::ptr::null_mut(), |s| s.into_raw())
}

/// Implementation of `init`, shared with the host tests.
pub fn init(files_dir: &str, data_key: &[u8], legacy_token: Option<&str>) -> Option<String> {
    let mut guard = BRIDGE.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(Bridge::open(Path::new(files_dir), data_key, legacy_token).ok()?);
    }
    guard.as_ref().and_then(|b| json(&b.status()))
}

/// `init(filesDir: String, dataKey: ByteArray, legacyToken: String?): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_init<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    data_key: JByteArray<'l>,
    legacy_token: JString<'l>,
) -> jstring {
    run(&mut env, |env| {
        let dir = string(env, &files_dir)?;
        if data_key.is_null() {
            return None;
        }
        let key = Zeroizing::new(env.convert_byte_array(&data_key).ok()?);
        let legacy = string(env, &legacy_token);
        init(&dir, &key, legacy.as_deref().map(String::as_str))
    })
}

/// `onSms(sender: String, body: String, receivedAtMs: Long): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_onSms<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    sender: JString<'l>,
    body: JString<'l>,
    received_at_ms: jlong,
) -> jstring {
    run(&mut env, |env| {
        let sender = string(env, &sender).unwrap_or_default();
        let body = string(env, &body)?;
        with_bridge(|b| json(&b.on_sms(&sender, &body, received_at_ms))).flatten()
    })
}

/// `onNotification(pkg: String, title: String, text: String, postedAtMs: Long): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_onNotification<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    pkg: JString<'l>,
    title: JString<'l>,
    text: JString<'l>,
    posted_at_ms: jlong,
) -> jstring {
    run(&mut env, |env| {
        let pkg = string(env, &pkg)?;
        let title = string(env, &title).unwrap_or_default();
        let text = string(env, &text).unwrap_or_default();
        with_bridge(|b| json(&b.on_notification(&pkg, &title, &text, posted_at_ms))).flatten()
    })
}

/// `flush(): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_flush<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    run(&mut env, |_| with_bridge(|b| json(&b.flush())).flatten())
}

/// `status(): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_status<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    run(&mut env, |_| with_bridge(|b| json(&b.status())).flatten())
}

/// `configure(json: String): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_configure<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    patch: JString<'l>,
) -> jstring {
    run(&mut env, |env| {
        let patch = string(env, &patch)?;
        with_bridge(|b| b.configure(&patch).ok().and_then(|s| json(&s))).flatten()
    })
}

/// `selfTest(network: Boolean): String`
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_Native_selfTest<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    network: jboolean,
) -> jstring {
    run(&mut env, |_| {
        with_bridge(|b| json(&b.self_test(network != JNI_FALSE))).flatten()
    })
}
