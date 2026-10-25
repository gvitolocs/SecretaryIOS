//! Placing a call through Android Telecom, over JNI.
//!
//! This is Lucina's `phone_call` function on the phone: the in-app Call
//! button and the adb-only `CallProvider` (`content call --uri
//! content://tech.getsecretary.secretary.calls --method call|check --arg
//! <number>`) both end here. The result is the provider's JSON contract:
//!
//! | `status` | Other fields | When |
//! |---|---|---|
//! | `needs_permission` | `number`, `message` | `CALL_PHONE` is not granted |
//! | `ready` | `number`, `placed: false` | `check` (dry run) with the permission |
//! | `submitted` | `number`, `placed: true`, `message` | Telecom accepted `placeCall` |
//! | `error` | `message` | invalid number, or Telecom refused |

use jni::{
    objects::{JObject, JValue},
    JNIEnv,
};
use serde_json::{json, Value};

use crate::{number::call_number, Result};

/// `PackageManager.PERMISSION_GRANTED`.
const PERMISSION_GRANTED: i32 = 0;

/// The result for a number before Telecom is involved: `Err` for an invalid
/// number, `Ok(None)` to go on, `Ok(Some(..))` to stop here.
pub fn precheck(number: &str, granted: bool, dry_run: bool) -> Result<(String, Option<Value>)> {
    let number = call_number(number)?;
    if !granted {
        let v = json!({"status": "needs_permission", "number": number, "message": "Enable phone calls in Secretary on the S20 FE."});
        return Ok((number, Some(v)));
    }
    if dry_run {
        let v = json!({"status": "ready", "number": number, "placed": false});
        return Ok((number, Some(v)));
    }
    Ok((number, None))
}

/// The result after Telecom accepted the call.
pub fn submitted(number: &str) -> Value {
    json!({"status": "submitted", "number": number, "placed": true, "message": "Android Telecom accepted the call request; connection is not yet confirmed."})
}

/// The provider's error shape.
pub fn error(message: &str) -> Value {
    json!({"status": "error", "message": message})
}

/// Check `CALL_PHONE` and, unless `dry_run`, ask `TelecomManager.placeCall`
/// to call `number` with `context` (any `android.content.Context`).
pub fn phone_call(
    env: &mut JNIEnv,
    context: &JObject,
    number: &str,
    dry_run: bool,
) -> Result<Value> {
    let number = call_number(number)?;
    let granted = with_cleared(env, "Permission check failed.", |env| {
        let permission = env.new_string("android.permission.CALL_PHONE")?;
        env.call_method(
            context,
            "checkSelfPermission",
            "(Ljava/lang/String;)I",
            &[JValue::Object(&permission)],
        )?
        .i()
    })? == PERMISSION_GRANTED;
    let (number, done) = precheck(&number, granted, dry_run)?;
    if let Some(v) = done {
        return Ok(v);
    }
    let telecom = with_cleared(env, "Telecom unavailable.", |env| {
        let service = env.new_string("telecom")?;
        env.call_method(
            context,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&service)],
        )?
        .l()
    })?;
    if telecom.is_null() {
        return Err("Telecom unavailable.".into());
    }
    let uri = with_cleared(env, "Invalid phone URI.", |env| {
        let scheme = env.new_string("tel")?;
        let value = env.new_string(&number)?;
        env.call_static_method(
            "android/net/Uri",
            "fromParts",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;",
            &[
                JValue::Object(&scheme),
                JValue::Object(&value),
                JValue::Object(&JObject::null()),
            ],
        )?
        .l()
    })?;
    with_cleared(
        env,
        "Android refused the call. Check the SIM and phone permission.",
        |env| {
            let extras = env.new_object("android/os/Bundle", "()V", &[])?;
            env.call_method(
                &telecom,
                "placeCall",
                "(Landroid/net/Uri;Landroid/os/Bundle;)V",
                &[JValue::Object(&uri), JValue::Object(&extras)],
            )?
            .v()
        },
    )?;
    Ok(submitted(&number))
}

/// Run `f`; on any JNI error clear the pending Java exception (so the JVM is
/// usable again) and return `message`.
fn with_cleared<'l, T>(
    env: &mut JNIEnv<'l>,
    message: &str,
    f: impl FnOnce(&mut JNIEnv<'l>) -> jni::errors::Result<T>,
) -> Result<T> {
    match f(env) {
        Ok(v) => Ok(v),
        Err(_) => {
            if env.exception_check().unwrap_or(true) {
                let _ = env.exception_clear();
            }
            Err(message.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_contract_before_telecom() {
        let (n, v) = precheck("+45 66 77 87 02", false, false).unwrap();
        assert_eq!(n, "+4566778702");
        assert_eq!(
            v.unwrap(),
            json!({"status": "needs_permission", "number": "+4566778702", "message": "Enable phone calls in Secretary on the S20 FE."})
        );
        let (_, v) = precheck("+4566778702", true, true).unwrap();
        assert_eq!(
            v.unwrap(),
            json!({"status": "ready", "number": "+4566778702", "placed": false})
        );
        assert!(precheck("+4566778702", true, false).unwrap().1.is_none());
        assert!(precheck("*21*1#", true, true).is_err());
        assert_eq!(submitted("+45")["placed"], true);
        assert_eq!(error("x"), json!({"status": "error", "message": "x"}));
    }
}
