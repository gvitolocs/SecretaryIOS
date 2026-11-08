//! Timeline ingestion: the server keeps one event stream open to Lucina's
//! `/api/secretary/timeline/stream` (with its own Lucina token, independent
//! of any client) and turns Lucina's items into notifications:
//!
//! | item | category |
//! |---|---|
//! | `role: lucina`, `meta.kind: "reminder"` | `reminder` |
//! | `role: lucina`, `meta.kind: "device_command"` that needs a tap (`meta.command.requiresConfirmation`, else by type) | `device_confirm` |
//! | `role: lucina`, `meta.kind: "shopping"` (Lucina changed a list) | `shopping` |
//! | `role: lucina`, `meta.kind: "decision"` | none: Lucina's own `POST /v1/notify` carries the route (`/decisions/<id>`) and the options |
//! | `role: lucina`, agent chat ([`agent_chat`]) | none: it stays readable in the chat, the owner gets no alert |
//! | any other `role: lucina` item | `lucina_message` |
//!
//! Owner items, backfilled history and items older than
//! [`super::NotifyConfig::stale`] are skipped. The last handled id is saved
//! (`notify-state.json`) and sent as `Last-Event-ID` on reconnect, so a
//! restart neither loses nor repeats items; every item is also
//! deduplicated by `tl:<id>` against the inbox. Reconnects back off 1 s,
//! 2 s, 5 s, 10 s, 30 s.

use std::time::Duration;

use futures_util::StreamExt;
use secretary_core::models::{Role, TimelineItem};
use serde_json::{json, Value};

use super::model::{clip, Category, NewNotification};
use crate::AppState;

pub const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
/// Longest notification body taken from a message.
pub const BODY_CHARS: usize = 240;

/// One Server-Sent Event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub id: Option<String>,
    pub data: String,
}

/// Incremental SSE parser (`\n`, `\r\n` or `\r` line ends; `:` comments).
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    cur: SseEvent,
    has_data: bool,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find(['\n', '\r']) {
            let line = self.buf[..pos].to_owned();
            let mut skip = pos + 1;
            if self.buf.as_bytes()[pos] == b'\r' {
                if pos + 1 >= self.buf.len() {
                    // Wait: a `\n` may follow in the next chunk.
                    break;
                }
                if self.buf.as_bytes()[pos + 1] == b'\n' {
                    skip += 1;
                }
            }
            self.buf.drain(..skip);
            self.line(&line, &mut out);
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            let ev = std::mem::take(&mut self.cur);
            if self.has_data {
                out.push(SseEvent {
                    event: if ev.event.is_empty() {
                        "message".into()
                    } else {
                        ev.event
                    },
                    ..ev
                });
            }
            self.has_data = false;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.cur.event = value.to_owned(),
            "id" => self.cur.id = Some(value.to_owned()),
            "data" => {
                if self.has_data {
                    self.cur.data.push('\n');
                }
                self.cur.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
    }
}

/// Epoch ms from a timeline `ts` (number, numeric string or ISO 8601).
pub fn ts_ms(v: &secretary_core::models::Scalar) -> Option<i64> {
    let text = v.to_text();
    if let Ok(n) = text.parse::<f64>() {
        let n = n as i64;
        return Some(if n < 1_000_000_000_000 { n * 1000 } else { n });
    }
    time::OffsetDateTime::parse(&text, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// Whether a device command type needs the owner's tap (same rule as the
/// iOS app: deletes and anything beyond alarms, timers and reminders).
pub fn command_needs_tap(kind: &str) -> bool {
    !matches!(
        kind,
        "alarm.set" | "timer.start" | "reminder.create" | "reminder.complete"
    )
}

fn attachment_body(item: &TimelineItem) -> String {
    let kinds: Vec<String> = item
        .attachments
        .as_ref()
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x["kind"].as_str().map(|k| format!("[{k}]")))
                .collect()
        })
        .unwrap_or_default();
    kinds.join(" ")
}

/// Lucina talking with an agent, not to the owner: a turn that came in through an agent link
/// (`meta.via`, whichever provider or caller) or an agent's reply to her message (`meta.kind:
/// "reach"`). What the text says does not matter: agents name the owner all the time. An agent
/// that needs him uses `lucina_decide` or an approval; each of those has its own push.
pub fn agent_chat(meta: &Value) -> bool {
    meta["via"].as_str().is_some_and(|v| !v.is_empty()) || meta["kind"] == "reach"
}

/// The notification for a timeline item, if any.
pub fn classify(item: &TimelineItem, now_ms: i64, stale_ms: i64) -> Option<NewNotification> {
    if item.role != Role::Lucina || item.backfilled == Some(true) {
        return None;
    }
    if let Some(ts) = ts_ms(&item.ts) {
        if now_ms - ts > stale_ms {
            return None;
        }
    }
    let meta = item.meta.clone().unwrap_or(Value::Null);
    let kind = meta["kind"].as_str().unwrap_or("");
    let mut body = clip(&item.text, BODY_CHARS);
    if body.is_empty() {
        body = attachment_body(item);
    }
    let (category, data) = match kind {
        // A decision: her own push opens it (`/decisions/<id>`, the options as buttons). A notification from the
        // item would win the shared `tl:` key with route `/` (2026-10-28: the owner's tap opened the chat).
        "decision" => return None,
        // A tool-trace line ("🔧 media search … → ok") is part of a turn, not a message: her reply
        // to his own turn notifies; an agent's turn notifies nothing (2026-11-03: 19 trace pushes).
        "tool" => return None,
        "reminder" => (Category::Reminder, None),
        "device_command" => {
            let command = &meta["command"];
            let ctype = command["type"].as_str().unwrap_or("");
            // Lucina says so (`requiresConfirmation`); else the app's rule by type.
            let needs_tap = command["requiresConfirmation"]
                .as_bool()
                .unwrap_or(ctype.is_empty() || command_needs_tap(ctype));
            if !needs_tap {
                return None;
            }
            (
                Category::DeviceConfirm,
                Some(json!({"commandId": command["id"], "type": ctype})),
            )
        }
        "shopping" => (
            Category::Shopping,
            meta.get("listId").map(|l| json!({"listId": l})),
        ),
        _ if agent_chat(&meta) => return None,
        _ => (Category::LucinaMessage, None),
    };
    if body.is_empty() {
        return None;
    }
    let mut n = NewNotification::new(category, body);
    n.channel = Some(String::from(item.channel.clone()));
    n.timeline_id = Some(item.id.to_text());
    n.route = Some(category.default_route().to_owned());
    n.data = data;
    Some(n)
}

/// The ingestion loop; returns on shutdown.
pub async fn run(st: AppState) {
    let Some(lucina) = st.lucina.clone() else {
        return;
    };
    let notifier = st.notify.clone();
    let mut shutdown = st.shutdown.subscribe();
    let mut failures = 0usize;
    tracing::info!("notification ingester started");
    loop {
        if *shutdown.borrow() {
            break;
        }
        let last = notifier.state.read(|s| s.last_event_id.clone());
        let opened = tokio::select! {
            r = lucina.timeline_stream(last.as_deref()) => r,
            () = super::stopped(&mut shutdown) => break,
        };
        match opened {
            Ok(resp) => {
                notifier.lucina_link(true).await;
                failures = 0;
                let mut parser = SseParser::default();
                let mut body = resp.bytes_stream();
                loop {
                    let chunk = tokio::select! {
                        c = body.next() => c,
                        () = super::stopped(&mut shutdown) => return,
                    };
                    let Some(Ok(bytes)) = chunk else {
                        break;
                    };
                    for ev in parser.push(&bytes) {
                        if ev.event != "item" {
                            continue;
                        }
                        match serde_json::from_str::<TimelineItem>(&ev.data) {
                            Ok(item) => notifier.ingest(&item, ev.id).await,
                            Err(e) => {
                                tracing::warn!(error = %e, "unreadable timeline item; skipped")
                            }
                        }
                    }
                }
                tracing::info!("timeline stream for notifications ended; reconnecting");
            }
            Err(e) => {
                notifier.lucina_link(false).await;
                tracing::info!(error = %e, "timeline stream for notifications unavailable");
                failures += 1;
            }
        }
        let wait = BACKOFF[failures.min(BACKOFF.len() - 1)];
        tokio::select! {
            () = tokio::time::sleep(wait) => {},
            () = super::stopped(&mut shutdown) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(v: Value) -> TimelineItem {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn sse_parser_handles_split_chunks_and_comments() {
        let mut p = SseParser::default();
        let mut out = p.push(b": ping\n\nevent: item\nid: 01");
        assert!(out.is_empty());
        out.extend(p.push(b"A\ndata: {\"a\":\r\n"));
        out.extend(p.push(b"data: 1}\r\n\r"));
        out.extend(p.push(b"\ndata: plain\n\n"));
        assert_eq!(
            out,
            vec![
                SseEvent {
                    event: "item".into(),
                    id: Some("01A".into()),
                    data: "{\"a\":\n1}".into()
                },
                SseEvent {
                    event: "message".into(),
                    id: None,
                    data: "plain".into()
                },
            ]
        );
        // `retry:` and an event without data are ignored.
        assert!(p.push(b"retry: 5000\nevent: x\n\n").is_empty());
    }

    #[test]
    fn timestamps() {
        use secretary_core::models::Scalar;
        assert_eq!(
            ts_ms(&Scalar::from("2026-10-22T07:30:00.000Z")),
            Some(1_792_654_200_000)
        );
        assert_eq!(ts_ms(&Scalar::from("1761048000")), Some(1_761_048_000_000));
        assert_eq!(
            ts_ms(&Scalar::from("1761048000123")),
            Some(1_761_048_000_123)
        );
        assert_eq!(ts_ms(&Scalar::from("ieri")), None);
    }

    #[test]
    fn agent_chat_makes_no_notification() {
        let now = 1_792_654_200_000;
        let base = json!({"id": "01J", "ts": "2026-10-22T07:30:00.000Z", "channel": "web",
                          "role": "lucina", "text": "Giuseppe should look at this"});
        let quiet = |meta: Value| {
            let mut v = base.clone();
            v["meta"] = meta;
            classify(&item(v), now, 3_600_000).is_none()
        };
        // Any caller, any link, with or without a conversation; naming him changes nothing.
        assert!(quiet(json!({"via": "mcp", "caller": "claude-code"})));
        assert!(quiet(json!({"via": "mcp", "caller": "some-new-provider", "conversation": "report"})));
        assert!(quiet(json!({"via": "mcp"})));
        assert!(quiet(json!({"via": "another-link", "caller": "x"})));
        assert!(quiet(json!({"kind": "tool", "via": "mcp", "caller": "claude"})));
        assert!(quiet(json!({"kind": "reach", "tool": "reach_agents", "ok": true})));
        // A tool-trace line never notifies, marked or not (the 2026-11-03 photo-search burst had no via).
        assert!(quiet(json!({"kind": "tool", "tool": "media", "ok": true})));
        // A save an agent asked for: the receipt stays in the chat.
        assert!(quiet(json!({"kind": "secret_receipt", "via": "agent"})));

        // The same words to him, and what he has to act on, still notify.
        assert!(!quiet(json!({"device": "dev_1", "deviceName": "iPhone"})));
        assert!(!quiet(json!({"kind": "secret_receipt"})));
        assert!(!quiet(json!({"kind": "proactive"})));
        assert!(!quiet(json!({"via": ""})));
        // An agent's turn can still make something that is his: a reminder, a tap, a list change.
        assert!(!quiet(json!({"kind": "reminder", "via": "mcp"})));
        assert!(!quiet(json!({"kind": "shopping", "via": "mcp", "listId": "L1"})));
        assert!(!quiet(json!({"kind": "device_command", "via": "mcp", "command": {"id": "c1", "type": "alarm.delete"}})));
    }

    #[test]
    fn classification() {
        let now = 1_792_654_200_000;
        let base = json!({"id": "01J", "ts": "2026-10-22T07:30:00.000Z", "channel": "telegram",
                          "role": "lucina", "text": "Ciao Giuseppe"});
        let n = classify(&item(base.clone()), now, 3_600_000).unwrap();
        assert_eq!(n.category, Some(Category::LucinaMessage));
        assert_eq!(n.channel.as_deref(), Some("telegram"));
        assert_eq!(n.timeline_id.as_deref(), Some("01J"));
        assert_eq!(n.keys(), vec!["tl:01J".to_owned()]);

        let mut v = base.clone();
        v["meta"] = json!({"kind": "reminder", "isDm": true});
        assert_eq!(
            classify(&item(v), now, 3_600_000).unwrap().category,
            Some(Category::Reminder)
        );

        let mut v = base.clone();
        v["meta"] = json!({"kind": "decision", "decisionId": "dc-0123456789ab"});
        assert!(classify(&item(v), now, 3_600_000).is_none(), "Lucina pushes decisions herself");

        let mut v = base.clone();
        v["meta"] = json!({"kind": "shopping", "listId": "L1"});
        let s = classify(&item(v), now, 3_600_000).unwrap();
        assert_eq!(s.category, Some(Category::Shopping));
        assert_eq!(s.route.as_deref(), Some("/shopping"));
        assert_eq!(s.data, Some(json!({"listId": "L1"})));

        let mut v = base.clone();
        v["meta"] = json!({"kind": "device_command", "command": {"id": "c1", "type": "alarm.delete", "args": {}}});
        let d = classify(&item(v), now, 3_600_000).unwrap();
        assert_eq!(d.category, Some(Category::DeviceConfirm));
        assert_eq!(
            d.data,
            Some(json!({"commandId": "c1", "type": "alarm.delete"}))
        );
        let mut v = base.clone();
        v["meta"] = json!({"kind": "device_command", "command": {"id": "c2", "type": "alarm.set"}});
        assert!(
            classify(&item(v), now, 3_600_000).is_none(),
            "runs without a tap"
        );
        let mut v = base.clone();
        v["meta"] = json!({"kind": "device_command", "command": {"id": "c3", "type": "alarm.set", "requiresConfirmation": true}});
        assert!(
            classify(&item(v), now, 3_600_000).is_some(),
            "Lucina's flag wins"
        );
        let mut v = base.clone();
        v["meta"] = json!({"kind": "device_command", "command": {"id": "c4", "type": "calendar.create", "requiresConfirmation": false}});
        assert!(classify(&item(v), now, 3_600_000).is_none());

        let mut v = base.clone();
        v["role"] = json!("user");
        assert!(classify(&item(v), now, 3_600_000).is_none());
        let mut v = base.clone();
        v["backfilled"] = json!(true);
        assert!(classify(&item(v), now, 3_600_000).is_none());
        assert!(
            classify(&item(base.clone()), now + 3_600_001, 3_600_000).is_none(),
            "stale"
        );

        let mut v = base;
        v["text"] = json!("");
        v["attachments"] = json!([{"kind": "image"}]);
        assert_eq!(classify(&item(v), now, 3_600_000).unwrap().body, "[image]");
    }
}
