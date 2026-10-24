//! What the Kotlin screens show and do, decided here so the shell only maps
//! JSON onto views and framework calls.
//!
//! | JNI `ui` op | Request → response |
//! |---|---|
//! | `callView` | `{"calls": [CallIn…]}` → [`CallView`] |
//! | `dtmf` | `{"digits"}` → `{"tones": [{"tone", "delayMs"}…]}` |
//! | `route` | `{"url"}` → `{"action": "web" \| "calls" \| "external" \| "block"}` |
//!
//! The transcript list ([`record_view`]) is the journal's `view` operation.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{journal::Record, Result};

/// The site the main WebView may show.
pub const SITE_HOST: &str = "getsecretary.tech";
/// Keypad tones sent per request.
pub const MAX_TONES: usize = 30;
/// Spacing between keypad tones.
pub const TONE_SPACING_MS: u64 = 250;

/// `android.telecom.Call` states this app tells apart.
pub mod state {
    /// `Call.STATE_DIALING`.
    pub const DIALING: i32 = 1;
    /// `Call.STATE_RINGING`.
    pub const RINGING: i32 = 2;
    /// `Call.STATE_HOLDING`.
    pub const HOLDING: i32 = 3;
    /// `Call.STATE_ACTIVE`.
    pub const ACTIVE: i32 = 4;
    /// `Call.STATE_DISCONNECTED`.
    pub const DISCONNECTED: i32 = 7;
}

/// One live call, as the `InCallService` reports it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CallIn {
    /// `Call.getState()`.
    pub state: i32,
    /// `Call.Details.getHandle().getSchemeSpecificPart()`, or null.
    pub handle: Option<String>,
    /// `Call.Details.can(CAPABILITY_HOLD)`.
    pub can_hold: bool,
}

/// One row of the in-call screen.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRow {
    /// Who is calling or being called.
    pub peer: String,
    /// "Incoming", "Connected", …
    pub label: &'static str,
    /// Buttons, in order: `answer`, `decline`, `end`, `resume`, `hold`.
    pub actions: Vec<&'static str>,
}

/// The ongoing call notification.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallNotice {
    /// Which call it is about: the first ringing one, else the first.
    pub index: usize,
    /// "Incoming call" or "Phone call".
    pub title: &'static str,
    /// The peer.
    pub text: String,
    /// Ringing calls open the in-call screen full-screen.
    pub full_screen: bool,
}

/// The in-call screen and notification for the current calls.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallView {
    /// One row per call, in Telecom's order.
    pub calls: Vec<CallRow>,
    /// `None` when there are no calls: cancel the notification.
    pub notification: Option<CallNotice>,
}

/// How a call is shown, from `Call.getState()`.
pub fn state_label(state: i32) -> &'static str {
    match state {
        state::RINGING => "Incoming",
        state::ACTIVE => "Connected",
        state::DIALING => "Dialing",
        state::HOLDING => "On hold",
        state::DISCONNECTED => "Disconnected",
        _ => "Connecting",
    }
}

/// The peer of a call: its number, or "Private number" without one.
pub fn peer(handle: Option<&str>) -> String {
    let s: String = handle
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    if s.trim().is_empty() {
        "Private number".into()
    } else {
        s
    }
}

/// The in-call screen and notification for `calls`.
pub fn call_view(calls: &[CallIn]) -> CallView {
    let rows = calls
        .iter()
        .map(|c| {
            let actions = if c.state == state::RINGING {
                vec!["answer", "decline"]
            } else if c.state == state::HOLDING {
                vec!["end", "resume"]
            } else if c.can_hold {
                vec!["end", "hold"]
            } else {
                vec!["end"]
            };
            CallRow {
                peer: peer(c.handle.as_deref()),
                label: state_label(c.state),
                actions,
            }
        })
        .collect::<Vec<_>>();
    let notification = (!calls.is_empty()).then(|| {
        let index = calls
            .iter()
            .position(|c| c.state == state::RINGING)
            .unwrap_or(0);
        let ringing = calls[index].state == state::RINGING;
        CallNotice {
            index,
            title: if ringing {
                "Incoming call"
            } else {
                "Phone call"
            },
            text: rows[index].peer.clone(),
            full_screen: ringing,
        }
    });
    CallView {
        calls: rows,
        notification,
    }
}

/// Keypad tones for `digits`: the first [`MAX_TONES`] characters, keeping
/// only `0-9`, `*` and `#`, each [`TONE_SPACING_MS`] after the previous slot.
pub fn dtmf(digits: &str) -> Value {
    let tones: Vec<Value> = digits
        .chars()
        .take(MAX_TONES)
        .enumerate()
        .filter(|(_, c)| c.is_ascii_digit() || *c == '*' || *c == '#')
        .map(|(i, c)| json!({ "tone": c.to_string(), "delayMs": i as u64 * TONE_SPACING_MS }))
        .collect();
    json!({ "tones": tones })
}

/// Where a WebView navigation goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `https://getsecretary.tech/…`: stay in the WebView.
    Web,
    /// `tel:…`: open the Calls screen with that number.
    Calls,
    /// Any other `https` URL: hand it to the browser.
    External,
    /// Everything else (`http`, `intent:`, `javascript:`, `file:` …).
    Block,
}

impl Route {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Calls => "calls",
            Self::External => "external",
            Self::Block => "block",
        }
    }
}

/// Route a WebView navigation. The host is read the way a browser reads it:
/// the authority ends at `/`, `\`, `?` or `#`, user info before the last `@`
/// and a port are dropped, so `https://evil.example\@getsecretary.tech/` and
/// `https://getsecretary.tech@evil.example/` are external, not the site
/// (`android.net.Uri` reads the host of the first as getsecretary.tech, but
/// the WebView loads evil.example).
pub fn route(url: &str) -> Route {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once(':') else {
        return Route::Block;
    };
    if scheme.eq_ignore_ascii_case("tel") {
        return Route::Calls;
    }
    if !scheme.eq_ignore_ascii_case("https") {
        return Route::Block;
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return Route::Block;
    };
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    if host.eq_ignore_ascii_case(SITE_HOST) {
        Route::Web
    } else if host.is_empty() {
        Route::Block
    } else {
        Route::External
    }
}

/// A journal record as the transcript screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    /// The record id, to attach a recording to it.
    pub id: String,
    /// "+4512345678 · sim".
    pub title: String,
    /// "2026-10-24 14:03 · ended".
    pub subtitle: String,
    /// The turns as "role: text" paragraphs, or `None` without turns.
    pub transcript: Option<String>,
    /// What to show instead of a transcript.
    pub note: Option<&'static str>,
}

/// Format `r` for the transcript screen; `tz_offset_min` is the local offset
/// from UTC (`TimeZone.getOffset(now) / 60000`).
pub fn record_view(r: &Record, tz_offset_min: i64) -> RecordView {
    let transcript = (!r.turns.is_empty()).then(|| {
        let mut out = String::new();
        for t in &r.turns {
            out.push_str(&t.role);
            out.push_str(": ");
            out.push_str(&t.text);
            out.push_str("\n\n");
        }
        out.truncate(out.trim_end().len());
        out
    });
    let note = transcript.is_none().then_some(if r.kind == "sim" {
        "SIM audio is unavailable to this app. Import an existing recording for a transcript."
    } else {
        "No speech transcript was saved."
    });
    RecordView {
        id: r.id.clone(),
        title: format!("{} · {}", r.peer, r.kind),
        subtitle: format!("{} · {}", local_time(r.started_at, tz_offset_min), r.status),
        transcript,
        note,
    }
}

/// `YYYY-MM-DD HH:MM` for `ms` since the epoch, shifted by `tz_offset_min`.
pub fn local_time(ms: u64, tz_offset_min: i64) -> String {
    let secs = (ms / 1000) as i64 + tz_offset_min * 60;
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        sod / 3600,
        sod % 3600 / 60
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Run a JNI `ui` operation.
pub fn run(op: &str, v: &Value) -> Result<Value> {
    match op {
        "callView" => {
            let calls: Vec<CallIn> =
                serde_json::from_value(v["calls"].clone()).map_err(|_| "Invalid call list.")?;
            serde_json::to_value(call_view(&calls)).map_err(|_| "Invalid call list.".into())
        }
        "dtmf" => Ok(dtmf(v["digits"].as_str().unwrap_or(""))),
        "route" => Ok(json!({ "action": route(v["url"].as_str().unwrap_or("")).as_str() })),
        _ => Err("Unknown screen operation.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Turn;

    #[test]
    fn ringing_call_gets_answer_and_a_full_screen_notification() {
        let calls = [
            CallIn {
                state: state::HOLDING,
                handle: Some("+4511111111".into()),
                can_hold: true,
            },
            CallIn {
                state: state::RINGING,
                handle: None,
                can_hold: false,
            },
        ];
        let v = call_view(&calls);
        assert_eq!(v.calls[0].actions, ["end", "resume"]);
        assert_eq!(v.calls[0].label, "On hold");
        assert_eq!(v.calls[1].actions, ["answer", "decline"]);
        assert_eq!(v.calls[1].peer, "Private number");
        let n = v.notification.unwrap();
        assert_eq!(
            (n.index, n.title, n.full_screen),
            (1, "Incoming call", true)
        );
        assert_eq!(n.text, "Private number");
    }

    #[test]
    fn active_call_and_no_calls() {
        let v = call_view(&[CallIn {
            state: state::ACTIVE,
            handle: Some("+4522".into()),
            can_hold: true,
        }]);
        assert_eq!(v.calls[0].actions, ["end", "hold"]);
        assert_eq!(v.notification.as_ref().unwrap().title, "Phone call");
        assert!(!v.notification.unwrap().full_screen);
        assert_eq!(
            call_view(&[CallIn {
                state: 9,
                ..Default::default()
            }])
            .calls[0]
                .actions,
            ["end"]
        );
        assert_eq!(state_label(9), "Connecting");
        assert!(call_view(&[]).notification.is_none());
        let json = run(
            "callView",
            &json!({"calls": [{"state": 2, "handle": "+45", "canHold": false}]}),
        )
        .unwrap();
        assert_eq!(json["notification"]["fullScreen"], true);
        assert_eq!(json["calls"][0]["label"], "Incoming");
    }

    #[test]
    fn keypad_tones_keep_their_slot_and_skip_other_characters() {
        let v = dtmf("1a#");
        assert_eq!(
            v["tones"],
            json!([{"tone": "1", "delayMs": 0}, {"tone": "#", "delayMs": 500}])
        );
        assert_eq!(
            dtmf(&"5".repeat(40))["tones"].as_array().unwrap().len(),
            MAX_TONES
        );
    }

    #[test]
    fn webview_stays_on_the_site() {
        assert_eq!(route("https://getsecretary.tech/desk"), Route::Web);
        assert_eq!(route("https://getsecretary.tech:443"), Route::Web);
        assert_eq!(route("HTTPS://GetSecretary.tech/"), Route::Web);
        assert_eq!(route("tel:+4512345678"), Route::Calls);
        assert_eq!(
            route("https://example.com/getsecretary.tech"),
            Route::External
        );
        assert_eq!(
            route("https://getsecretary.tech@evil.example/"),
            Route::External
        );
        assert_eq!(
            route("https://evil.example\\@getsecretary.tech/"),
            Route::External
        );
        assert_eq!(
            route("https://getsecretary.tech\\@evil.example/"),
            Route::Web
        );
        assert_eq!(
            route("https://getsecretary.tech.evil.example/"),
            Route::External
        );
        for url in [
            "http://getsecretary.tech/",
            "javascript:alert(1)",
            "intent://x#Intent;end",
            "file:///sdcard",
            "https:///x",
            "",
        ] {
            assert_eq!(route(url), Route::Block, "{url}");
        }
    }

    #[test]
    fn transcript_view_and_local_time() {
        assert_eq!(local_time(0, 0), "1970-01-01 00:00");
        assert_eq!(local_time(1_761_314_580_000, 120), "2025-10-24 16:03");
        assert_eq!(local_time(951_782_400_000, 0), "2000-02-29 00:00");
        let mut r = Record {
            id: "a".into(),
            kind: "sim".into(),
            peer: "+4512".into(),
            started_at: 0,
            status: "ended".into(),
            turns: vec![],
            ended_at: None,
        };
        let v = record_view(&r, 60);
        assert_eq!(v.title, "+4512 · sim");
        assert_eq!(v.subtitle, "1970-01-01 01:00 · ended");
        assert!(v.transcript.is_none());
        assert!(v.note.unwrap().starts_with("SIM audio"));
        r.turns = vec![
            Turn {
                role: "user".into(),
                text: "hi".into(),
                at: 0,
            },
            Turn {
                role: "lucina".into(),
                text: "hello".into(),
                at: 0,
            },
        ];
        let v = record_view(&r, 0);
        assert_eq!(v.transcript.as_deref(), Some("user: hi\n\nlucina: hello"));
        assert!(v.note.is_none());
    }
}
