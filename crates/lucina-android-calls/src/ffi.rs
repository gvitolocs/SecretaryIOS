//! The JNI surface: `object Native` in `tech.getsecretary.secretary`
//! (`@JvmStatic external fun`, library `lucina_calls`).
//!
//! | Kotlin | Returns |
//! |---|---|
//! | `phoneCall(context: Context, number: String, dryRun: Boolean): String` | The provider contract ([`crate::phone`]), unwrapped. |
//! | `journal(filesDir: String, op: String, json: String): String` | Envelope around [`crate::journal::Op`]. |
//! | `voiceTurn(filesDir: String, cookie: String, json: String): String` | Envelope around [`crate::net::voice_turn`]; `json` is `{"id", "text"}`. |
//! | `transcribe(filesDir: String, cookie: String, json: String, audio: ByteArray): String` | Envelope around [`crate::net::transcribe`]. |
//! | `shareUpload(filesDir: String, cookie: String, manifestPath: String): String` | Unwrapped [`crate::share::upload`] job result. |
//! | `ui(op: String, json: String): String` | Envelope around [`crate::ui::run`]. |
//! | `callerState(filesDir: String): String` | Envelope: `{settings, courier, courierLine, last, lastLine}` ([`crate::caller::Store::state`]). |
//! | `callerSettings(filesDir: String, json: String): String` | Envelope: patch `{label?, silence?, reject?}`, then the new state. |
//! | `setCourier(filesDir: String, json: String): String` | Envelope: keep Lucina Maps' `{onShift, orderAtMs}`, then the new state. |
//! | `localVerdict(filesDir: String, number: String, json: String, lang: String): String` | Envelope around [`crate::caller::Screen`], no network. `json` is `{contact?}`. |
//! | `screenCall(filesDir: String, cookie: String, number: String, json: String, lang: String): String` | The same, from the server (about 1.2 s at most), else the rules. Blocks on the network. |
//!
//! For envelope-returning calls, the envelope is `{"ok": true, "data": …}` or `{"ok": false, "error": "…"}`.
//! Every entry point catches panics, so Rust never unwinds into the JVM.
//! `journal` and `ui` are fast enough for the main thread; `voiceTurn` and
//! `transcribe` and `shareUpload` block on the network and must run on a worker thread.

use std::panic::{catch_unwind, AssertUnwindSafe};

use jni::{
    objects::{JByteArray, JClass, JObject, JString},
    sys::{jboolean, jstring},
    JNIEnv,
};
use serde_json::{json, Value};

use crate::{
    caller,
    journal::{Journal, Op},
    net::{self, Https},
    phone, ui, Result,
};

/// Largest JSON argument accepted from Kotlin.
const MAX_JSON_BYTES: usize = 64 * 1024;

/// `{"ok": true, "data": …}` or `{"ok": false, "error": …}`.
pub fn envelope(result: Result<Value>) -> String {
    match result {
        Ok(data) => json!({ "ok": true, "data": data }).to_string(),
        Err(error) => json!({ "ok": false, "error": error }).to_string(),
    }
}

fn string(env: &mut JNIEnv, s: &JString, error: &str) -> Result<String> {
    if s.is_null() {
        return Err(error.into());
    }
    env.get_string(s).map(Into::into).map_err(|_| error.into())
}

fn json_arg(env: &mut JNIEnv, s: &JString) -> Result<Value> {
    let text = string(env, s, "Invalid request.")?;
    if text.len() > MAX_JSON_BYTES {
        return Err("Request limit.".into());
    }
    serde_json::from_str(&text).map_err(|_| "Invalid request.".into())
}

/// Hand `text` to Java; null only if the JVM is out of memory.
fn give(env: &mut JNIEnv, text: String) -> jstring {
    env.new_string(text)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Run an envelope-returning entry point, catching panics.
fn guarded(env: &mut JNIEnv, f: impl FnOnce(&mut JNIEnv) -> Result<Value>) -> jstring {
    let result = catch_unwind(AssertUnwindSafe(|| f(env)))
        .unwrap_or_else(|_| Err("Call operation failed.".into()));
    give(env, envelope(result))
}

/// `Native.phoneCall`: the `CallProvider` and the Call button.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_phoneCall<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    context: JObject<'l>,
    number: JString<'l>,
    dry_run: jboolean,
) -> jstring {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let number = string(&mut env, &number, "Invalid number.")?;
        phone::phone_call(&mut env, &context, &number, dry_run != 0)
    }));
    let value = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => phone::error(&e),
        Err(_) => phone::error("Phone function failed."),
    };
    give(&mut env, value.to_string())
}

/// `Native.journal`: `begin`, `append`, `end`, `history`, `view`.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_journal<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    op: JString<'l>,
    data: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let dir = string(env, &files_dir, "Storage unavailable.")?;
        let op = Op::parse(&string(env, &op, "Invalid operation.")?)?;
        let v = json_arg(env, &data)?;
        Journal::open(&dir)?.run(op, &v)
    })
}

/// `Native.voiceTurn`: one turn of a Lucina voice conversation.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_voiceTurn<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    cookie: JString<'l>,
    data: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let journal = Journal::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let cookie = string(env, &cookie, "Sign in to Secretary first.")?;
        let v = json_arg(env, &data)?;
        let text = v["text"].as_str().ok_or("Invalid voice message.")?;
        net::voice_turn(
            &journal,
            &Https::new(&cookie),
            v["id"].as_str().unwrap_or(""),
            text,
        )
    })
}

/// `Native.transcribe`: transcribe a recording into the journal.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_transcribe<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    cookie: JString<'l>,
    data: JString<'l>,
    audio: JByteArray<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let journal = Journal::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let cookie = string(env, &cookie, "Sign in to Secretary first.")?;
        let v = json_arg(env, &data)?;
        if audio.is_null() {
            return Err("The recording is empty.".into());
        }
        let len = env
            .get_array_length(&audio)
            .map_err(|_| "Recording unavailable.")?;
        if len as usize > net::MAX_AUDIO_BYTES {
            return Err("Recording exceeds 8 MiB.".into());
        }
        let bytes = env
            .convert_byte_array(&audio)
            .map_err(|_| "Recording unavailable.")?;
        net::transcribe(&journal, &Https::new(&cookie), &v, &bytes)
    })
}

/// `Native.ui`: `callView`, `dtmf`, `route`.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_ui<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    op: JString<'l>,
    data: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let op = string(env, &op, "Invalid operation.")?;
        let v = json_arg(env, &data)?;
        ui::run(&op, &v)
    })
}

/// `Native.callerState`: settings, courier state and the last screened call.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_callerState<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let store = caller::Store::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        Ok(store.state(crate::now_ms()))
    })
}

/// `Native.callerSettings`: change the owner's switches.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_callerSettings<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    data: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let store = caller::Store::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let v = json_arg(env, &data)?;
        store.patch_settings(&v)?;
        Ok(store.state(crate::now_ms()))
    })
}

/// `Native.setCourier`: keep what Lucina Maps broadcast.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_setCourier<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    data: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let store = caller::Store::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let v = json_arg(env, &data)?;
        let now = crate::now_ms();
        store.set_courier(&v, now)?;
        Ok(store.state(now))
    })
}

/// `Native.localVerdict`: the instant verdict for a ringing number.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_localVerdict<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    number: JString<'l>,
    data: JString<'l>,
    lang: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let store = caller::Store::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let number = string(env, &number, "Invalid number.")?;
        let input = caller::Input::from_json(&json_arg(env, &data)?);
        let lang = string(env, &lang, "").unwrap_or_default();
        Ok(serde_json::to_value(caller::local(
            &store,
            &number,
            &input,
            &lang,
            crate::now_ms(),
        ))
        .unwrap_or(Value::Null))
    })
}

/// `Native.screenCall`: Jev's verdict from the server, else the rules.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_screenCall<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    cookie: JString<'l>,
    number: JString<'l>,
    data: JString<'l>,
    lang: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let store = caller::Store::open(&string(env, &files_dir, "Storage unavailable.")?)?;
        let cookie = string(env, &cookie, "").unwrap_or_default();
        let number = string(env, &number, "Invalid number.")?;
        let input = caller::Input::from_json(&json_arg(env, &data)?);
        let lang = string(env, &lang, "").unwrap_or_default();
        Ok(serde_json::to_value(caller::screen(
            &store,
            &Https::with_timeout(&cookie, caller::SCREEN_TIMEOUT),
            &number,
            &input,
            &lang,
            crate::now_ms(),
        ))
        .unwrap_or(Value::Null))
    })
}

/// `Native.shareUpload`: one streaming upload, with an unwrapped job result.
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_Native_shareUpload<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    cookie: JString<'l>,
    manifest: JString<'l>,
) -> jstring {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Value> {
        let dir = string(&mut env, &files_dir, "Storage unavailable.")?;
        let cookie = string(&mut env, &cookie, "Please sign in.")?;
        let manifest = string(&mut env, &manifest, "Share manifest unavailable.")?;
        Ok(crate::share::upload(
            &dir,
            &cookie,
            &manifest,
            &crate::share::Https(&cookie),
        ))
    }));
    let value = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            json!({"ok": false, "status": 400, "retry": false, "shareId": "", "error": e})
        }
        Err(_) => {
            json!({"ok": false, "status": 0, "retry": true, "shareId": "", "error": "Upload interrupted."})
        }
    };
    give(&mut env, value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shapes() {
        let ok: Value = serde_json::from_str(&envelope(Ok(json!({"id": "a"})))).unwrap();
        assert_eq!(ok, json!({"ok": true, "data": {"id": "a"}}));
        let err: Value = serde_json::from_str(&envelope(Err("Call not found.".into()))).unwrap();
        assert_eq!(err, json!({"ok": false, "error": "Call not found."}));
    }
}
