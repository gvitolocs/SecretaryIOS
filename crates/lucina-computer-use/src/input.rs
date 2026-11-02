//! One driver call from an agent on her desktop (`computer_call`, the `computer` tool's `tool`
//! action), made so nobody has to know the driver's rules:
//!
//! - Input is delivered in the foreground: Chromium refuses the driver's default
//!   (`background_unavailable`), which every caller found out one refused click at a time.
//! - A pixel action the driver wants grounded on a read gets that read, once.
//! - **Keys need a proven focus.** The driver reports no focused element on Linux, so a typed word lands
//!   wherever focus happens to be. In Outlook's message list letters and Return are live shortcuts: on
//!   2026-11-02 a refused click followed by `type_text` + `press_key` opened a mail. Text and action keys
//!   are refused unless the last pointer action in this flow worked, or the call places focus itself
//!   (`element_token`, `x`/`y`). A failed click closes the gate at once, so a run stops there.
//!
//! What this cannot see: a click that works but lands on something that takes no text. Only the
//! owner's input and the Jev engine (which stops on any failed step) bypass this module.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::sandbox::text_of;
use crate::{Error, Sandbox, SandboxId};

/// Input tools that take `delivery_mode`.
const INPUT_TOOLS: [&str; 5] = ["click", "scroll", "type_text", "press_key", "hotkey"];

/// How long a working click counts as proof that focus is still there.
const FOCUS_PROOF: Duration = Duration::from_secs(120);

fn focus_proved_at() -> &'static Mutex<Option<Instant>> {
    static AT: Mutex<Option<Instant>> = Mutex::new(None);
    &AT
}

/// What a call means for the keyboard gate.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Keys {
    /// Not keyboard input.
    None,
    /// Safe wherever focus is (browser chrome, Escape, paging).
    Free,
    /// As [`Keys::Free`], and it leaves focus in a known place (the address bar).
    PlacesFocus,
    /// Goes wherever focus is: needs proof.
    NeedsFocus,
}

fn names_a_place(args: &Value) -> bool {
    args.get("element_token").is_some() || args.get("x").is_some()
}

fn lower_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn chord(keys: &[String]) -> Keys {
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    match refs.as_slice() {
        ["ctrl", "l"] | ["ctrl", "t"] => Keys::PlacesFocus,
        ["ctrl", "tab"] | ["ctrl", "shift", "tab"] | ["alt", "left"] | ["alt", "right"]
        | ["ctrl", "r"] | ["escape"] | ["page_up"] | ["pageup"] | ["page_down"] | ["pagedown"] => {
            Keys::Free
        }
        _ => Keys::NeedsFocus,
    }
}

fn classify(tool: &str, args: &Value) -> Keys {
    match tool {
        "type_text" | "press_key" | "hotkey" if names_a_place(args) => Keys::PlacesFocus,
        "type_text" => Keys::NeedsFocus,
        "press_key" => {
            let mut keys = lower_list(args.get("modifiers"));
            keys.push(
                args.get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_ascii_lowercase(),
            );
            chord(&keys)
        }
        "hotkey" => chord(&lower_list(args.get("keys"))),
        _ => Keys::None,
    }
}

fn failed(r: &Result<Value, Error>) -> bool {
    r.as_ref().map_or(true, |v| v["isError"] == true)
}

fn says(r: &Result<Value, Error>, what: &str) -> bool {
    match r {
        Ok(v) => v["isError"] == true && text_of(v).contains(what),
        Err(e) => e.to_string().contains(what),
    }
}

/// The driver wants this pixel action grounded on a read of the window.
fn wants_a_read(r: &Result<Value, Error>, args: &Value) -> bool {
    args.get("x").is_some() && args.get("pid").is_some() && says(r, "call get_window_state")
}

/// One driver tool for an agent. Other sandboxes (Android) go straight through.
pub async fn agent_tool(sb: &Sandbox, id: SandboxId, tool: &str, args: Value) -> Result<Value, Error> {
    if id != SandboxId::Workspace {
        return sb.call_tool(tool, args).await;
    }
    run(tool, args, |t: String, a: Value| async move { sb.call_tool(&t, a).await }).await
}

/// [`agent_tool`] over any driver (tests use a scripted one).
async fn run<F, Fut>(tool: &str, mut args: Value, call: F) -> Result<Value, Error>
where
    F: Fn(String, Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    let keys = classify(tool, &args);
    let proved = focus_proved_at()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|t| t.elapsed() < FOCUS_PROOF);
    if keys == Keys::NeedsFocus && !proved {
        return Err(Error::FocusUnverified);
    }
    if INPUT_TOOLS.contains(&tool) && args.is_object() && args.get("delivery_mode").is_none() {
        args["delivery_mode"] = json!("foreground");
    }
    let mut r = call(tool.to_owned(), args.clone()).await;
    if wants_a_read(&r, &args) {
        let read = json!({
            "pid": args["pid"], "window_id": args.get("window_id").cloned().unwrap_or(Value::Null),
            "include_accessibility_tree": false,
        });
        if !failed(&call("get_window_state".to_owned(), read).await) {
            r = call(tool.to_owned(), args.clone()).await;
        }
    }
    // An explicit `background` that Chromium refused; a click on something without on-screen bounds.
    if INPUT_TOOLS.contains(&tool) {
        let retry = if says(&r, "background_unavailable") {
            Some("foreground")
        } else if tool == "click" && says(&r, "no usable on-screen bounds") {
            Some("background")
        } else {
            None
        };
        if let Some(mode) = retry {
            args["delivery_mode"] = json!(mode);
            r = call(tool.to_owned(), args).await;
        }
    }
    let mut proved = focus_proved_at().lock().unwrap_or_else(|e| e.into_inner());
    let ok = !failed(&r);
    match (tool, keys) {
        ("click", _) => *proved = ok.then(Instant::now),
        (_, Keys::PlacesFocus) => *proved = ok.then(Instant::now),
        // Return and the like can change the page under the focus.
        (_, Keys::NeedsFocus) if tool != "type_text" => *proved = None,
        (_, Keys::NeedsFocus) if !ok => *proved = None,
        _ => {}
    }
    drop(proved);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Scripted driver: every call is recorded; `refuse` names tools that fail.
    #[derive(Clone, Default)]
    struct Driver {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        refuse: Arc<Mutex<Vec<(&'static str, &'static str)>>>,
    }

    impl Driver {
        async fn call(&self, tool: String, args: Value) -> Result<Value, Error> {
            self.calls.lock().unwrap().push((tool.clone(), args));
            let mut refuse = self.refuse.lock().unwrap();
            if let Some(i) = refuse.iter().position(|(t, _)| *t == tool) {
                let (_, why) = refuse.remove(i);
                return Ok(json!({"isError": true, "content": [{"type": "text", "text": why}]}));
            }
            Ok(json!({"content": [{"type": "text", "text": "ok"}]}))
        }
        fn tools(&self) -> Vec<String> {
            self.calls.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
        }
    }

    async fn go(d: &Driver, tool: &str, args: Value) -> Result<Value, Error> {
        run(tool, args, |t, a| {
            let d = d.clone();
            async move { d.call(t, a).await }
        })
        .await
    }

    /// The gate is one process-wide state, like the desktop: a single test walks through it.
    #[tokio::test]
    async fn keys_need_a_proven_focus_and_input_goes_to_the_foreground() {
        let d = Driver::default();
        *focus_proved_at().lock().unwrap() = None;

        // 2026-11-02: no click yet, or a click that failed: nothing is typed into Outlook's list.
        let e = go(&d, "type_text", json!({"text": "Therese"})).await.unwrap_err();
        assert_eq!(e.code(), "focus_unverified");
        let e = go(&d, "press_key", json!({"key": "Return"})).await.unwrap_err();
        assert_eq!(e.code(), "focus_unverified");
        assert!(d.tools().is_empty(), "refused before the driver");

        d.refuse.lock().unwrap().push(("click", "boom"));
        assert!(failed(&go(&d, "click", json!({"pid": 1, "x": 5, "y": 5})).await));
        assert!(go(&d, "type_text", json!({"text": "Therese"})).await.is_err());

        // A click that works opens the gate; input is in the foreground without anyone asking.
        assert!(!failed(&go(&d, "click", json!({"pid": 1, "x": 5, "y": 5})).await));
        assert_eq!(d.calls.lock().unwrap().last().unwrap().1["delivery_mode"], "foreground");
        assert!(!failed(&go(&d, "type_text", json!({"text": "Therese"})).await));
        // Return may leave the field: the next key needs a new click.
        assert!(!failed(&go(&d, "press_key", json!({"key": "Return"})).await));
        assert!(go(&d, "press_key", json!({"key": "Return"})).await.is_err());

        // A later failed click closes it again, whatever worked before.
        assert!(!failed(&go(&d, "click", json!({"pid": 1, "x": 5, "y": 5})).await));
        d.refuse.lock().unwrap().push(("click", "boom"));
        assert!(failed(&go(&d, "click", json!({"pid": 1, "x": 5, "y": 5})).await));
        assert!(go(&d, "type_text", json!({"text": "x"})).await.is_err());

        // Calls that place focus themselves, and browser chrome, need no proof.
        assert!(!failed(&go(&d, "press_key", json!({"key": "Escape"})).await));
        assert!(!failed(&go(&d, "hotkey", json!({"keys": ["ctrl", "l"]})).await));
        assert!(!failed(&go(&d, "type_text", json!({"text": "example.com"})).await));
        *focus_proved_at().lock().unwrap() = None;
        assert!(!failed(&go(&d, "type_text", json!({"text": "a", "x": 1, "y": 2, "pid": 1})).await));
        *focus_proved_at().lock().unwrap() = None;
        assert!(!failed(&go(&d, "type_text", json!({"text": "a", "element_token": "s00000001:3"})).await));
        *focus_proved_at().lock().unwrap() = None;

        // A pixel click the driver wants grounded gets one read and one retry.
        let d = Driver::default();
        d.refuse.lock().unwrap().push(("click", "no capture: call get_window_state(pid=1) first"));
        assert!(!failed(&go(&d, "click", json!({"pid": 1, "window_id": 2, "x": 5, "y": 5})).await));
        assert_eq!(d.tools(), ["click", "get_window_state", "click"]);

        // Chromium's refusal of an explicit background is answered with the foreground.
        let d = Driver::default();
        d.refuse.lock().unwrap().push(("click", "{\"code\":\"background_unavailable\"}"));
        assert!(!failed(&go(&d, "click", json!({"pid": 1, "x": 5, "y": 5, "delivery_mode": "background"})).await));
        assert_eq!(d.calls.lock().unwrap().last().unwrap().1["delivery_mode"], "foreground");
        *focus_proved_at().lock().unwrap() = None;
    }

    #[test]
    fn what_the_gate_lets_through() {
        for (tool, args) in [
            ("hotkey", json!({"keys": ["ctrl", "a"]})),
            ("hotkey", json!({"keys": ["ctrl", "v"]})),
            ("press_key", json!({"key": "Delete"})),
            ("press_key", json!({"key": "r"})),
            ("press_key", json!({"key": "Return", "modifiers": ["ctrl"]})),
            ("type_text", json!({"text": "hi"})),
        ] {
            assert_eq!(classify(tool, &args), Keys::NeedsFocus, "{tool} {args}");
        }
        assert_eq!(classify("press_key", &json!({"key": "Escape"})), Keys::Free);
        assert_eq!(classify("hotkey", &json!({"keys": ["ctrl", "tab"]})), Keys::Free);
        assert_eq!(classify("hotkey", &json!({"keys": ["ctrl", "t"]})), Keys::PlacesFocus);
        assert_eq!(classify("get_window_state", &json!({})), Keys::None);
        assert_eq!(classify("scroll", &json!({})), Keys::None);
    }
}
