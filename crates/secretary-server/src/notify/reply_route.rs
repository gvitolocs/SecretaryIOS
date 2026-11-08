//! Where a Lucina item goes: the pure rule behind "a reply goes where he asked"
//! (docs/notifications.md, "Replies and the device he asked on"). Verus kernel:
//! `verification/verus/reply_route.rs`; differential test `tests/verus_reply_route.rs`.
//!
//! | item kind \ turn | none (Lucina on her own) | agent | test | Telegram, Discord, Alexa, speaker | push device, app in front | push device, app away | other Secretary client | unknown client |
//! |---|---|---|---|---|---|---|---|---|
//! | message | now | silent | silent | silent | silent | now | grace | now |
//! | shopping | now | now | silent | silent | silent | now | grace | now |
//! | reminder | now | now | silent | now | now | now | now | now |
//! | device confirm | now | now | silent | now | now | now | now | now |
//!
//! - "now" still goes through the category switch, quiet hours and the app-in-front rule of
//!   [`super::Notifier::publish`].
//! - "grace": held for [`GRACE`]; pushed only if the asking device is gone by then
//!   ([`grace_push`]): no open timeline stream or voice socket, and no request since the reply.
//! - A push device is one that holds a push subscription itself (the iPhone app): it shows the reply
//!   when in front and needs the push when not.

use std::time::Duration;

use serde_json::Value;

/// How long a reply to another client waits for that client before it pushes.
pub const GRACE: Duration = Duration::from_secs(20);

/// What the notification is about (after `ingest::classify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Message,
    Shopping,
    Reminder,
    DeviceConfirm,
}

/// Who started the turn the item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// No turn: Lucina on her own (a reminder firing, a proactive message).
    Proactive,
    /// An agent's turn (`meta.via`, `meta.kind: "reach"`).
    Agent,
    /// Test traffic: a test source (`probe…`, `test…`), `meta.test`, or a test device.
    Test,
    /// A turn on a channel that delivers the reply itself (Telegram, Discord, Alexa, the speakers).
    Channel,
    /// The owner asked on a device that gets pushes itself; `present`: its app is in front.
    PushDevice { present: bool },
    /// The owner asked on a Secretary client that gets no pushes (the DSi, Juno, the Mac app, the web).
    OtherDevice,
    /// A reply with neither device nor source (older items): today's rule, it pushes.
    Unknown,
}

/// What to do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Silent,
    Now,
    Grace,
}

/// The rule (the table above).
pub fn route(kind: Kind, origin: Origin) -> Route {
    match origin {
        Origin::Test => Route::Silent,
        Origin::Proactive | Origin::Unknown => Route::Now,
        _ if matches!(kind, Kind::Reminder | Kind::DeviceConfirm) => Route::Now,
        Origin::Agent => {
            if kind == Kind::Message {
                Route::Silent
            } else {
                Route::Now
            }
        }
        Origin::Channel => Route::Silent,
        Origin::PushDevice { present } => {
            if present {
                Route::Silent
            } else {
                Route::Now
            }
        }
        Origin::OtherDevice => Route::Grace,
    }
}

/// After the grace period: push only when the asking client is gone. `connected`: it holds an open
/// timeline stream or voice socket; `active`: it made a request since the reply was written.
pub fn grace_push(connected: bool, active: bool) -> bool {
    !connected && !active
}

/// Sources that are test traffic (agents' probes, test runs on his devices).
pub fn is_test_source(source: &str) -> bool {
    let s = source.trim().to_ascii_lowercase();
    s.starts_with("probe") || s.starts_with("test")
}

/// Channels whose own client shows Lucina's reply (no Secretary device behind them).
pub fn delivers_itself(channel: &str) -> bool {
    matches!(channel, "telegram" | "discord" | "alexa" | "voice")
}

/// What an item says about its turn (from Lucina's marks).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    /// The item answers an owner item (top-level `replyTo`).
    pub reply: bool,
    /// `meta.fromDevice`, else `meta.device`.
    pub device: Option<String>,
    /// `meta.source`.
    pub source: Option<String>,
    /// `meta.test == true`.
    pub test: bool,
    /// `meta.via` / `kind: reach`.
    pub agent: bool,
    pub channel: String,
}

impl Turn {
    pub fn of(channel: &str, reply_to: bool, meta: &Value) -> Self {
        let text = |k: &str| {
            meta[k]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Self {
            reply: reply_to,
            device: text("fromDevice").or_else(|| text("device")),
            source: text("source"),
            test: meta["test"] == Value::Bool(true),
            agent: super::ingest::agent_chat(meta),
            channel: channel.to_owned(),
        }
    }

    /// The origin, given what the notifier knows of devices: `test_device` (flagged in devices.json),
    /// `push_device` (holds a push subscription), `present` (its app is in front).
    pub fn origin(
        &self,
        test_device: impl Fn(&str) -> bool,
        push_device: impl Fn(&str) -> bool,
        present: impl Fn(&str) -> bool,
    ) -> Origin {
        if self.agent {
            return Origin::Agent;
        }
        if self.test
            || self.source.as_deref().is_some_and(is_test_source)
            || self.device.as_deref().is_some_and(&test_device)
        {
            return Origin::Test;
        }
        if let Some(d) = self.device.as_deref() {
            return if push_device(d) {
                Origin::PushDevice { present: present(d) }
            } else {
                Origin::OtherDevice
            };
        }
        if !self.reply && self.source.is_none() {
            return Origin::Proactive;
        }
        if delivers_itself(&self.channel) {
            return Origin::Channel;
        }
        Origin::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const IPHONE: &str = "dev_6uJCAexoApw";
    const DSI: &str = "dev_1PIYOXSE72o";
    const JUNO: &str = "dev_K8bGH74HE4k";
    const PROBE: &str = "dev_T4gP5p4xAA0";

    fn origin(channel: &str, reply: bool, meta: Value, iphone_in_front: bool) -> Origin {
        Turn::of(channel, reply, &meta).origin(
            |d| d == PROBE,
            |d| d == IPHONE,
            |d| d == IPHONE && iphone_in_front,
        )
    }

    #[test]
    fn the_table() {
        use Kind::*;
        use Route::*;
        let origins = [
            Origin::Proactive,
            Origin::Agent,
            Origin::Test,
            Origin::Channel,
            Origin::PushDevice { present: true },
            Origin::PushDevice { present: false },
            Origin::OtherDevice,
            Origin::Unknown,
        ];
        let rows = [
            (Message, [Now, Silent, Silent, Silent, Silent, Now, Grace, Now]),
            (Shopping, [Now, Now, Silent, Silent, Silent, Now, Grace, Now]),
            (Reminder, [Now, Now, Silent, Now, Now, Now, Now, Now]),
            (DeviceConfirm, [Now, Now, Silent, Now, Now, Now, Now, Now]),
        ];
        for (kind, want) in rows {
            for (o, w) in origins.iter().zip(want) {
                assert_eq!(route(kind, *o), w, "{kind:?} × {o:?}");
            }
        }
        assert!(grace_push(false, false), "the client is gone: the reply must not be lost");
        assert!(!grace_push(true, false));
        assert!(!grace_push(false, true));
    }

    #[test]
    fn dsi_turn() {
        let o = origin("web", true, json!({"source": "fortuna-chat", "fromDevice": DSI}), true);
        assert_eq!(o, Origin::OtherDevice);
        assert_eq!(route(Kind::Message, o), Route::Grace);
        // A chat turn: the device rides as `meta.device`.
        let o = origin("web", true, json!({"device": DSI, "deviceName": "Fortuna (Nintendo DSi)"}), false);
        assert_eq!(o, Origin::OtherDevice);
    }

    #[test]
    fn juno_turn() {
        let o = origin("web", true, json!({"source": "juno-chat", "device": JUNO}), false);
        assert_eq!(route(Kind::Message, o), Route::Grace);
    }

    #[test]
    fn iphone_turn_in_front_and_away() {
        let meta = json!({"device": IPHONE, "source": "ios-chat"});
        assert_eq!(route(Kind::Message, origin("ios", true, meta.clone(), true)), Route::Silent);
        assert_eq!(route(Kind::Message, origin("ios", true, meta, false)), Route::Now);
    }

    #[test]
    fn probe_and_test_traffic_is_silent() {
        for meta in [
            json!({"source": "probe"}),
            json!({"source": "probe-voice"}),
            json!({"source": "Test-juno", "device": JUNO}),
            json!({"device": PROBE, "deviceName": "Voice probe (nezopt)"}),
            json!({"fromDevice": PROBE}),
            json!({"test": true, "device": IPHONE}),
        ] {
            let o = origin("web", true, meta.clone(), false);
            assert_eq!(o, Origin::Test, "{meta}");
            for k in [Kind::Message, Kind::Shopping, Kind::Reminder, Kind::DeviceConfirm] {
                assert_eq!(route(k, o), Route::Silent);
            }
        }
    }

    #[test]
    fn agent_turns() {
        let o = origin("web", true, json!({"via": "mcp", "caller": "claude", "device": DSI}), false);
        assert_eq!(o, Origin::Agent);
        assert_eq!(route(Kind::Message, o), Route::Silent);
        assert_eq!(route(Kind::Shopping, o), Route::Now);
    }

    #[test]
    fn proactive_and_reminders() {
        assert_eq!(origin("other", false, json!({"kind": "proactive"}), true), Origin::Proactive);
        assert_eq!(origin("telegram", false, json!({"kind": "reminder"}), false), Origin::Proactive);
        assert_eq!(route(Kind::Reminder, Origin::Proactive), Route::Now);
        // A reminder made in a DSi turn still pushes.
        assert_eq!(route(Kind::Reminder, Origin::OtherDevice), Route::Now);
    }

    #[test]
    fn channels_and_legacy() {
        assert_eq!(origin("telegram", true, json!(null), false), Origin::Channel);
        assert_eq!(origin("alexa", true, json!({}), false), Origin::Channel);
        assert_eq!(origin("web", true, json!({}), false), Origin::Unknown);
        assert_eq!(route(Kind::Message, Origin::Unknown), Route::Now);
        // A source without a device (a share before the gateway passed the device).
        assert_eq!(origin("web", false, json!({"source": "fortuna-chat"}), false), Origin::Unknown);
    }
}
