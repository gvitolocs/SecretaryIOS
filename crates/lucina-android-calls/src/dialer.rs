//! What the phone app's own screens show: the call log, the missed-call and
//! voicemail notifications and the Jev label of a number, decided here so the
//! Kotlin shell only maps JSON onto views.
//!
//! | JNI `dialer` op | Request → response |
//! |---|---|
//! | `label` | `{"number", "lang"?, "contact"?}` → [`caller::Screen`] (remembered verdict, else the instant rules guess) |
//! | `callLog` | `{"rows": [LogIn…], "tzOffsetMin"?, "lang"?}` → `{"rows": [LogRow…]}` |
//! | `missed` | `{"count", "number"?, "name"?, "lang"?}` → `{"title", "text"}` |
//! | `voicemail` | `{"count"}` → `{"clear": bool, "title", "text"}` |
//! | `plan`, `rejected`, `allow`, `rejections`, `order` | auto-reject, see [`crate::reject`] |
//!
//! Nothing here places, answers or rejects a call, and nothing here is on the
//! emergency-call path: dialing an emergency number never reaches this module.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    caller::{self, Input, Store},
    ui, Result,
};

/// Most call-log rows formatted per request.
pub const MAX_ROWS: usize = 500;

/// `CallLog.Calls.TYPE` values.
pub mod log_type {
    pub const INCOMING: i32 = 1;
    pub const OUTGOING: i32 = 2;
    pub const MISSED: i32 = 3;
    pub const VOICEMAIL: i32 = 4;
    pub const REJECTED: i32 = 5;
    pub const BLOCKED: i32 = 6;
    pub const ANSWERED_ELSEWHERE: i32 = 7;
}

/// One `CallLog.Calls` row as Kotlin reads it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogIn {
    pub id: i64,
    pub number: String,
    #[serde(rename = "type")]
    pub kind: i32,
    pub date_ms: u64,
    pub duration_s: u64,
    /// `CACHED_NAME`: the contact's name when the call was logged.
    pub name: Option<String>,
    pub is_new: bool,
}

/// One row of the call-log screen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRow {
    pub id: i64,
    /// What to call back (empty for a private number).
    pub number: String,
    /// The contact's name, else the number as written for reading.
    pub title: String,
    /// "Missed · Today 14:05", "Outgoing · Yesterday 09:30 · 2:41".
    pub subtitle: String,
    /// `incoming`, `outgoing`, `missed`, `rejected`, `blocked`, `voicemail` or `other`.
    pub kind: &'static str,
    pub missed: bool,
    pub is_new: bool,
    /// Jev's remembered verdict for a number that is not a contact.
    pub jev: Option<String>,
    /// Jev thought the call unwanted.
    pub warn: bool,
}

fn kind_name(t: i32) -> (&'static str, &'static str) {
    match t {
        log_type::INCOMING => ("incoming", "Incoming"),
        log_type::OUTGOING => ("outgoing", "Outgoing"),
        log_type::MISSED => ("missed", "Missed"),
        log_type::VOICEMAIL => ("voicemail", "Voicemail"),
        log_type::REJECTED => ("rejected", "Declined"),
        log_type::BLOCKED => ("blocked", "Blocked"),
        log_type::ANSWERED_ELSEWHERE => ("incoming", "Answered elsewhere"),
        _ => ("other", "Call"),
    }
}

/// `3:07` or `1:02:03`, `12 s` under a minute, empty for no duration.
pub fn duration(s: u64) -> String {
    match s {
        0 => String::new(),
        1..=59 => format!("{s} s"),
        60..=3599 => format!("{}:{:02}", s / 60, s % 60),
        _ => format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
    }
}

/// "Today 14:05", "Yesterday 09:30" or "2026-10-27 18:12".
pub fn when(ms: u64, now: u64, tz_offset_min: i64) -> String {
    let full = ui::local_time(ms, tz_offset_min);
    let day = |t: u64| ((t / 1000) as i64 + tz_offset_min * 60).div_euclid(86_400);
    let hm = &full[11..];
    match day(now) - day(ms) {
        0 => format!("Today {hm}"),
        1 => format!("Yesterday {hm}"),
        _ => full,
    }
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Format the call log. A number that is not a contact gets Jev's remembered
/// verdict, if the screening ever heard one; no request is made.
pub fn call_log(
    store: &Store,
    rows: &[LogIn],
    tz_offset_min: i64,
    lang: &str,
    now: u64,
) -> Vec<LogRow> {
    rows.iter()
        .take(MAX_ROWS)
        .map(|r| {
            let (kind, word) = kind_name(r.kind);
            let number = clean(r.number.trim(), 40);
            let parsed = caller::display_of(&number);
            let name = r
                .name
                .as_deref()
                .map(|n| clean(n, 120))
                .filter(|n| !n.trim().is_empty());
            let title = match (&name, number.is_empty()) {
                (Some(n), _) => n.clone(),
                (None, true) => "Private number".to_owned(),
                (None, false) => parsed.unwrap_or_else(|| number.clone()),
            };
            let mut subtitle = format!("{word} · {}", when(r.date_ms, now, tz_offset_min));
            let d = duration(r.duration_s);
            if !d.is_empty() && matches!(r.kind, log_type::INCOMING | log_type::OUTGOING) {
                subtitle.push_str(" · ");
                subtitle.push_str(&d);
            }
            let verdict = if name.is_none() && !number.is_empty() {
                caller::cached(store, &number, lang, now)
            } else {
                None
            };
            let warn = verdict.as_ref().is_some_and(|v| v.spam >= 0.7);
            let jev = verdict.map(|v| {
                if v.source == "rules" {
                    format!("{} · quick guess", v.title)
                } else {
                    v.title
                }
            });
            LogRow {
                id: r.id,
                number,
                title,
                subtitle,
                kind,
                missed: r.kind == log_type::MISSED,
                is_new: r.is_new,
                jev,
                warn,
            }
        })
        .collect()
}

/// The missed-call notification for `count` missed calls, the last from
/// `number` (a contact `name`, if known).
pub fn missed(
    store: &Store,
    count: i64,
    number: &str,
    name: Option<&str>,
    lang: &str,
    now: u64,
) -> Value {
    let count = count.clamp(1, 999);
    let number = clean(number.trim(), 40);
    let who = match name.map(|n| clean(n, 120)).filter(|n| !n.trim().is_empty()) {
        Some(n) => n,
        None if number.is_empty() => "Private number".to_owned(),
        None => caller::display_of(&number).unwrap_or_else(|| number.clone()),
    };
    let jev = if name.is_none() && !number.is_empty() {
        caller::cached(store, &number, lang, now).map(|v| v.title)
    } else {
        None
    };
    let title = if count == 1 {
        "Missed call".to_owned()
    } else {
        format!("{count} missed calls")
    };
    let mut text = if count == 1 {
        who
    } else {
        format!("Last: {who}")
    };
    if let Some(j) = jev {
        text.push_str(" · ");
        text.push_str(&j);
    }
    json!({ "title": title, "text": text })
}

/// The voicemail notification: `count` is the number of messages, or -1 when
/// the network did not say. 0 clears the notification.
pub fn voicemail(count: i64) -> Value {
    if count == 0 {
        return json!({ "clear": true, "title": "", "text": "" });
    }
    let title = if count < 0 {
        "New voicemail".to_owned()
    } else if count == 1 {
        "1 new voicemail".to_owned()
    } else {
        format!("{} new voicemails", count.min(999))
    };
    json!({ "clear": false, "title": title, "text": "Tap to call your voicemail." })
}

/// Run a JNI `dialer` operation.
pub fn run(store: &Store, op: &str, v: &Value, now: u64) -> Result<Value> {
    let lang = v["lang"].as_str().unwrap_or("en");
    match op {
        "label" => {
            let number = v["number"].as_str().ok_or("Invalid number.")?;
            let input = Input::from_json(v);
            serde_json::to_value(caller::label(store, number, &input, lang, now))
                .map_err(|_| "Label unavailable.".into())
        }
        "callLog" => {
            let rows: Vec<LogIn> =
                serde_json::from_value(v["rows"].clone()).map_err(|_| "Invalid call log.")?;
            let tz = v["tzOffsetMin"]
                .as_i64()
                .unwrap_or(0)
                .clamp(-24 * 60, 24 * 60);
            Ok(json!({ "rows": call_log(store, &rows, tz, lang, now) }))
        }
        "missed" => Ok(missed(
            store,
            v["count"].as_i64().unwrap_or(1),
            v["number"].as_str().unwrap_or(""),
            v["name"].as_str(),
            lang,
            now,
        )),
        "voicemail" => Ok(voicemail(v["count"].as_i64().unwrap_or(-1))),
        "plan" | "rejected" | "allow" | "rejections" | "order" => {
            crate::reject::run(store, op, v, now)
        }
        _ => Err("Unknown phone operation.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::tests::app_dir;

    const NOW: u64 = 1_793_275_200_000; // 2026-10-29T12:00:00Z

    fn store(name: &str) -> Store {
        Store::open(&app_dir(name)).unwrap()
    }

    #[test]
    fn durations_and_days() {
        assert_eq!(duration(0), "");
        assert_eq!(duration(12), "12 s");
        assert_eq!(duration(161), "2:41");
        assert_eq!(duration(3723), "1:02:03");
        assert_eq!(when(NOW - 3_600_000, NOW, 0), "Today 11:00");
        assert_eq!(when(NOW - 86_400_000, NOW, 0), "Yesterday 12:00");
        assert_eq!(when(NOW - 3 * 86_400_000, NOW, 0), "2026-10-26 12:00");
        // Local midnight decides "today", not UTC's.
        assert_eq!(
            when(NOW - 3_600_000, NOW + 12 * 3_600_000, 120),
            "Yesterday 13:00"
        );
    }

    #[test]
    fn the_call_log_names_contacts_and_labels_unknown_numbers_from_the_cache() {
        let s = store("log");
        let rows = vec![
            LogIn {
                id: 1,
                number: "+4570405205".into(),
                kind: log_type::MISSED,
                date_ms: NOW - 600_000,
                duration_s: 0,
                is_new: true,
                ..Default::default()
            },
            LogIn {
                id: 2,
                number: "+4522334455".into(),
                kind: log_type::OUTGOING,
                date_ms: NOW - 90_000_000,
                duration_s: 161,
                name: Some("Mamma".into()),
                ..Default::default()
            },
            LogIn {
                id: 3,
                number: String::new(),
                kind: log_type::INCOMING,
                date_ms: NOW,
                duration_s: 5,
                ..Default::default()
            },
            LogIn {
                id: 4,
                number: "+4570405205".into(),
                kind: 99,
                date_ms: NOW,
                ..Default::default()
            },
        ];
        // Nothing remembered yet: no label.
        let before = call_log(&s, &rows, 0, "en", NOW);
        assert_eq!(before[0].title, "+45 70 40 52 05");
        assert_eq!(before[0].subtitle, "Missed · Today 11:50");
        assert!(before[0].missed && before[0].is_new);
        assert_eq!(before[0].jev, None);
        assert_eq!(before[1].title, "Mamma");
        assert_eq!(before[1].subtitle, "Outgoing · Yesterday 11:00 · 2:41");
        assert_eq!(before[2].title, "Private number");
        assert_eq!(before[2].subtitle, "Incoming · Today 12:00 · 5 s");
        assert_eq!(before[3].kind, "other");
        // The screening heard Jev's verdict for that number.
        let answer = json!({"results": [{
            "number": "+4570405205", "display": "+45 70 40 52 05", "spam": 0.93, "kind": "scam",
            "pickUp": 0.04, "label": "Likely scam (0.93)", "source": "jev", "reasons": []}]});
        struct Fake(Value);
        impl crate::net::Backend for Fake {
            fn post(&self, _: &str, _: &Value) -> Result<Value> {
                Ok(self.0.clone())
            }
        }
        caller::screen(
            &s,
            &Fake(answer),
            "+4570405205",
            &Input::default(),
            "en",
            NOW - 1000,
        );
        let after = call_log(&s, &rows, 0, "en", NOW);
        assert_eq!(after[0].jev.as_deref(), Some("Likely scam (0.93)"));
        assert!(after[0].warn);
        // A contact keeps the contact's name and no label; so does a private number.
        assert_eq!(after[1].jev, None);
        assert_eq!(after[2].jev, None);
    }

    #[test]
    fn missed_and_voicemail_notifications() {
        let s = store("notices");
        let one = missed(&s, 1, "+4570405205", None, "en", NOW);
        assert_eq!(
            one,
            json!({"title": "Missed call", "text": "+45 70 40 52 05"})
        );
        let many = missed(&s, 3, "", Some("Mamma"), "en", NOW);
        assert_eq!(
            many,
            json!({"title": "3 missed calls", "text": "Last: Mamma"})
        );
        assert_eq!(missed(&s, 0, "", None, "en", NOW)["title"], "Missed call");
        assert_eq!(missed(&s, 1, "", None, "en", NOW)["text"], "Private number");
        assert_eq!(voicemail(0)["clear"], true);
        assert_eq!(voicemail(-1)["title"], "New voicemail");
        assert_eq!(voicemail(1)["title"], "1 new voicemail");
        assert_eq!(voicemail(4)["title"], "4 new voicemails");
    }

    #[test]
    fn the_label_is_remembered_not_asked_twice() {
        let s = store("label");
        let v = json!({"number": "70405205", "lang": "en"});
        let first = run(&s, "label", &v, NOW).unwrap();
        assert_eq!(first["source"], "rules");
        let answer = json!({"results": [{
            "number": "+4570405205", "display": "+45 70 40 52 05", "spam": 0.93, "kind": "scam",
            "pickUp": 0.04, "label": "Likely scam (0.93)", "source": "jev", "reasons": []}]});
        struct Fake(Value);
        impl crate::net::Backend for Fake {
            fn post(&self, _: &str, _: &Value) -> Result<Value> {
                Ok(self.0.clone())
            }
        }
        caller::screen(&s, &Fake(answer), "70405205", &Input::default(), "en", NOW);
        let second = run(&s, "label", &v, NOW + 5000).unwrap();
        assert_eq!(second["source"], "jev");
        assert_eq!(second["title"], "Likely scam (0.93)");
        // Later the server is down: the quick guess does not replace a fresh Jev verdict ...
        struct Down;
        impl crate::net::Backend for Down {
            fn post(&self, _: &str, _: &Value) -> Result<Value> {
                Err("down".into())
            }
        }
        caller::screen(&s, &Down, "70405205", &Input::default(), "en", NOW + 10_000);
        assert_eq!(run(&s, "label", &v, NOW + 11_000).unwrap()["source"], "jev");
        // ... but a stale one is, and an old entry expires.
        caller::screen(
            &s,
            &Down,
            "70405205",
            &Input::default(),
            "en",
            NOW + 7 * 3_600_000,
        );
        assert_eq!(
            run(&s, "label", &v, NOW + 7 * 3_600_000).unwrap()["source"],
            "rules"
        );
        let far = NOW + caller::CACHE_TTL_MS + 8 * 3_600_000;
        assert_eq!(run(&s, "label", &v, far).unwrap()["source"], "rules");
    }

    #[test]
    fn bad_requests_are_refused() {
        let s = store("bad");
        assert!(run(&s, "nope", &json!({}), NOW).is_err());
        assert!(run(&s, "label", &json!({}), NOW).is_err());
        assert!(run(&s, "callLog", &json!({"rows": "x"}), NOW).is_err());
    }
}
