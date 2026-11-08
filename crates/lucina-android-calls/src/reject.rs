//! What the phone keeps for auto-reject: the owner's allow list, the log of what was rejected,
//! the order signal from Lucina Maps and the outbox of reports to Lucina.
//!
//! The decision itself is the shared core's [`lucina_phone_core::caller::reject_decision`], called from
//! [`crate::caller`]. Nothing here rejects a call.
//!
//! | File | Content |
//! |---|---|
//! | `caller-allowed.json` | Numbers the owner allowed ("Allow", "Allow & call back"); never rejected again. |
//! | `caller-rejected.json` | The last [`MAX_LOG`] rejections, newest last. |
//! | `caller-order.json` | The last navigation link from Lucina Maps: only used to raise the bar while an order is active. |
//! | `caller-outbox.json` | `POST /v1/callers/rejected` and `/v1/callers/allow` waiting for the network. |
//!
//! The order signal never leaves the phone: it is not in any request body.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    caller::{rfc3339, RejectOut, Store, LOCK},
    net::Backend,
    Result,
};

/// Rejections kept for the Callers screen.
pub const MAX_LOG: usize = 50;
/// Allowed numbers kept.
pub const MAX_ALLOWED: usize = 500;
/// Reports waiting in the outbox.
pub const MAX_OUTBOX: usize = 50;
/// An outbox entry is dropped after this many failed tries.
pub const MAX_TRIES: u32 = 20;
/// A navigation link counts as "an order is active" for this long.
pub const ORDER_ACTIVE_MS: u64 = 45 * 60_000;
/// Total time for one outbox request.
pub const OUTBOX_TIMEOUT: Duration = Duration::from_secs(8);
/// Server paths.
pub const REJECTED_PATH: &str = "/v1/callers/rejected";
pub const ALLOW_PATH: &str = "/v1/callers/allow";

/// One rejected call, for the Callers screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejection {
    /// The time in ms, as text: the id of the entry.
    pub id: String,
    pub at_ms: u64,
    /// E.164.
    pub number: String,
    pub line: String,
    pub spam: Option<f64>,
    pub pick_up: Option<f64>,
    pub sources: String,
    /// The owner allowed it afterwards.
    #[serde(default)]
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Order {
    order_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Pending {
    path: String,
    body: Value,
    #[serde(default)]
    tries: u32,
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

impl Store {
    // ---- allow list

    pub fn is_allowed(&self, number: &str) -> bool {
        let key = digits(number);
        !key.is_empty()
            && self
                .read::<Vec<String>>("caller-allowed.json")
                .is_some_and(|all| all.contains(&key))
    }

    /// Never reject this number again.
    pub fn allow(&self, number: &str) -> Result<()> {
        let n = lucina_phone_core::caller::parse(number);
        if !n.valid() {
            return Err("Invalid number.".into());
        }
        let key = digits(&n.e164);
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        let mut all: Vec<String> = self.read("caller-allowed.json").unwrap_or_default();
        all.retain(|a| *a != key);
        all.push(key);
        if all.len() > MAX_ALLOWED {
            let excess = all.len() - MAX_ALLOWED;
            all.drain(..excess);
        }
        self.write("caller-allowed.json", &all)
    }

    // ---- the rejection log

    /// Remember a rejection that really happened.
    pub fn log_rejection(&self, r: &RejectOut, now: u64) -> Result<Rejection> {
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        let mut all: Vec<Rejection> = self.read("caller-rejected.json").unwrap_or_default();
        let entry = Rejection {
            id: now.to_string(),
            at_ms: now,
            number: r.number.clone(),
            line: r.line.clone(),
            spam: r.spam,
            pick_up: r.pick_up,
            sources: r.sources.clone(),
            allowed: false,
        };
        all.retain(|e| e.id != entry.id);
        all.push(entry.clone());
        if all.len() > MAX_LOG {
            let excess = all.len() - MAX_LOG;
            all.drain(..excess);
        }
        self.write("caller-rejected.json", &all)?;
        Ok(entry)
    }

    /// The log, newest first.
    pub fn rejections(&self) -> Vec<Rejection> {
        let mut all: Vec<Rejection> = self.read("caller-rejected.json").unwrap_or_default();
        let allowed: Vec<String> = self.read("caller-allowed.json").unwrap_or_default();
        for e in &mut all {
            e.allowed = e.allowed || allowed.contains(&digits(&e.number));
        }
        all.reverse();
        all
    }

    // ---- the order signal (local only)

    /// Keep Lucina Maps' last navigation link.
    pub fn set_order(&self, order_at_ms: u64, now: u64) -> Result<()> {
        if order_at_ms > now.saturating_add(10 * 60_000) {
            return Err("Invalid order signal.".into());
        }
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        self.write("caller-order.json", &Order { order_at_ms })
    }

    /// A navigation link arrived within the last 45 minutes.
    pub fn order_active(&self, now: u64) -> bool {
        self.read::<Order>("caller-order.json").is_some_and(|o| {
            o.order_at_ms > 0
                && o.order_at_ms <= now.saturating_add(60_000)
                && now.saturating_sub(o.order_at_ms) <= ORDER_ACTIVE_MS
        })
    }

    // ---- the outbox

    fn enqueue(&self, path: &str, body: Value) -> Result<()> {
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        let mut all: Vec<Pending> = self.read("caller-outbox.json").unwrap_or_default();
        all.push(Pending {
            path: path.into(),
            body,
            tries: 0,
        });
        if all.len() > MAX_OUTBOX {
            let excess = all.len() - MAX_OUTBOX;
            all.drain(..excess);
        }
        self.write("caller-outbox.json", &all)
    }

    /// Tell Lucina about a rejection (queued; see [`flush`]).
    pub fn report_rejected(&self, r: &RejectOut, now: u64) -> Result<()> {
        let mut body = json!({
            "number": r.number,
            "line": r.line,
            "sources": r.sources,
            "source": "android",
            "at": rfc3339(now),
        });
        if let Some(s) = r.spam {
            body["spam"] = json!(s);
        }
        if let Some(p) = r.pick_up {
            body["pickUp"] = json!(p);
        }
        self.enqueue(REJECTED_PATH, body)
    }

    /// Tell Lucina the owner allowed a number (queued).
    pub fn report_allowed(&self, number: &str) -> Result<()> {
        self.enqueue(ALLOW_PATH, json!({ "number": number }))
    }

    /// Reports waiting for the network.
    pub fn outbox_len(&self) -> usize {
        self.read::<Vec<Pending>>("caller-outbox.json")
            .map_or(0, |v| v.len())
    }

    /// Send what is waiting; whatever fails stays for the next time (up to [`MAX_TRIES`]).
    /// Returns how many are still waiting. One flush at a time; entries queued meanwhile are kept.
    pub fn flush(&self, backend: &dyn Backend) -> usize {
        let Ok(_one_at_a_time) = FLUSH.lock() else {
            return 0;
        };
        let snapshot: Vec<Pending> = match LOCK.lock() {
            Ok(_l) => self.read("caller-outbox.json").unwrap_or_default(),
            Err(_) => return 0,
        };
        if snapshot.is_empty() {
            return 0;
        }
        let mut left = Vec::new();
        for p in &snapshot {
            if backend.post(&p.path, &p.body).is_err() {
                let mut again = p.clone();
                again.tries += 1;
                if again.tries < MAX_TRIES {
                    left.push(again);
                }
            }
        }
        let Ok(_l) = LOCK.lock() else {
            return left.len();
        };
        let now: Vec<Pending> = self.read("caller-outbox.json").unwrap_or_default();
        left.extend(now.into_iter().skip(snapshot.len()));
        let _ = self.write("caller-outbox.json", &left);
        left.len()
    }
}

static FLUSH: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run a JNI `dialer` operation about rejections.
pub fn run(store: &Store, op: &str, v: &Value, now: u64) -> Result<Value> {
    match op {
        "plan" => {
            let input = crate::caller::Input::from_json(v);
            let number = v["number"].as_str().ok_or("Invalid number.")?;
            Ok(json!({ "wait": crate::caller::must_wait(store, number, &input, now) }))
        }
        "rejected" => {
            let r: RejectOut =
                serde_json::from_value(v.clone()).map_err(|_| "Invalid rejection.")?;
            let entry = store.log_rejection(&r, now)?;
            store.report_rejected(&r, now)?;
            Ok(json!({ "id": entry.id }))
        }
        "allow" => {
            let number = v["number"].as_str().ok_or("Invalid number.")?;
            let n = lucina_phone_core::caller::parse(number);
            if !n.valid() {
                return Err("Invalid number.".into());
            }
            store.allow(&n.e164)?;
            store.report_allowed(&n.e164)?;
            Ok(json!({ "allowed": true, "number": n.e164 }))
        }
        "rejections" => Ok(json!({ "rows": store.rejections().iter().map(|e| json!({
            "id": e.id, "number": e.number, "line": e.line, "allowed": e.allowed,
            "when": crate::dialer::when(e.at_ms, now, v["tzOffsetMin"].as_i64().unwrap_or(0).clamp(-1440, 1440)),
        })).collect::<Vec<_>>() })),
        "order" => {
            store.set_order(v["orderAtMs"].as_u64().unwrap_or(0), now)?;
            Ok(json!({ "ok": true }))
        }
        _ => Err("Unknown phone operation.".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::journal::tests::app_dir;

    const NOW: u64 = 1_793_275_200_000;

    fn store(name: &str) -> Store {
        Store::open(&app_dir(name)).unwrap()
    }

    struct Fake {
        ok: bool,
        sent: RefCell<Vec<(String, Value)>>,
    }

    impl Backend for Fake {
        fn post(&self, path: &str, body: &Value) -> Result<Value> {
            self.sent.borrow_mut().push((path.into(), body.clone()));
            if self.ok {
                Ok(json!({}))
            } else {
                Err("down".into())
            }
        }
    }

    fn fake(ok: bool) -> Fake {
        Fake {
            ok,
            sent: RefCell::new(Vec::new()),
        }
    }

    fn rejected_json() -> Value {
        json!({
            "number": "+4570405205",
            "line": "Rejected likely telemarketer +45 70 40 52 05 · 180.dk 6 reports",
            "spam": 0.93,
            "pickUp": null,
            "sources": "180.dk 6 reports",
        })
    }

    #[test]
    fn a_rejection_is_logged_and_reported() {
        let s = store("log");
        let out = run(&s, "rejected", &rejected_json(), NOW).unwrap();
        assert_eq!(out["id"], NOW.to_string());
        run(&s, "rejected", &rejected_json(), NOW + 60_000).unwrap();
        let rows = run(&s, "rejections", &json!({}), NOW + 120_000).unwrap();
        let rows = rows["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], (NOW + 60_000).to_string(), "newest first");
        assert_eq!(rows[0]["allowed"], false);
        assert!(rows[0]["line"]
            .as_str()
            .unwrap()
            .starts_with("Rejected likely telemarketer"));
        // The report to Lucina: the line, the numbers and where it came from.
        let f = fake(true);
        assert_eq!(s.flush(&f), 0);
        let sent = f.sent.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].0, "/v1/callers/rejected");
        assert_eq!(
            sent[0].1,
            json!({
                "number": "+4570405205",
                "line": "Rejected likely telemarketer +45 70 40 52 05 · 180.dk 6 reports",
                "sources": "180.dk 6 reports",
                "source": "android",
                "at": "2026-10-29T12:00:00Z",
                "spam": 0.93,
            })
        );
        assert!(run(&s, "rejected", &json!({"number": 5}), NOW).is_err());
    }

    #[test]
    fn the_log_keeps_the_last_fifty() {
        let s = store("fifty");
        for i in 0..(MAX_LOG as u64 + 5) {
            run(&s, "rejected", &rejected_json(), NOW + i).unwrap();
        }
        let rows = s.rejections();
        assert_eq!(rows.len(), MAX_LOG);
        assert_eq!(rows[0].id, (NOW + MAX_LOG as u64 + 4).to_string());
    }

    #[test]
    fn allowing_a_number_is_remembered_and_reported() {
        let s = store("allow");
        run(&s, "rejected", &rejected_json(), NOW).unwrap();
        assert!(!s.is_allowed("+4570405205"));
        let out = run(&s, "allow", &json!({"number": "70405205"}), NOW).unwrap();
        assert_eq!(out, json!({"allowed": true, "number": "+4570405205"}));
        assert!(s.is_allowed("+4570405205"));
        assert!(s.rejections()[0].allowed, "the log shows it as allowed");
        assert!(run(&s, "allow", &json!({"number": "12"}), NOW).is_err());
        assert!(run(&s, "allow", &json!({}), NOW).is_err());
        let f = fake(true);
        s.flush(&f);
        let sent = f.sent.borrow();
        assert_eq!(sent.last().unwrap().0, "/v1/callers/allow");
        assert_eq!(sent.last().unwrap().1, json!({"number": "+4570405205"}));
    }

    #[test]
    fn reports_wait_for_the_network_and_are_not_lost_or_repeated() {
        let s = store("outbox");
        run(&s, "rejected", &rejected_json(), NOW).unwrap();
        // Offline: the report stays.
        let down = fake(false);
        assert_eq!(s.flush(&down), 1);
        assert_eq!(s.outbox_len(), 1);
        // Online again: it is sent once and the queue is empty.
        let up = fake(true);
        assert_eq!(s.flush(&up), 0);
        assert_eq!(up.sent.borrow().len(), 1);
        assert_eq!(s.flush(&up), 0);
        assert_eq!(up.sent.borrow().len(), 1, "nothing is sent twice");
        // A report that never gets through is dropped after MAX_TRIES.
        run(&s, "rejected", &rejected_json(), NOW).unwrap();
        for _ in 0..MAX_TRIES - 1 {
            assert_eq!(s.flush(&down), 1);
        }
        assert_eq!(s.flush(&down), 0);
        // The queue is bounded.
        for i in 0..(MAX_OUTBOX as u64 + 7) {
            run(&s, "rejected", &rejected_json(), NOW + i).unwrap();
        }
        assert_eq!(s.outbox_len(), MAX_OUTBOX);
    }

    #[test]
    fn the_allow_list_is_bounded_and_the_plan_reads_it() {
        let s = store("plan");
        let plan = |s: &Store, extra: Value| {
            let mut v = json!({"number": "70405205"});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            run(s, "plan", &v, NOW).unwrap()["wait"].clone()
        };
        assert_eq!(plan(&s, json!({})), true);
        assert_eq!(plan(&s, json!({"calledOrAnswered": true})), false);
        assert_eq!(plan(&s, json!({"unchecked": true})), false);
        run(&s, "allow", &json!({"number": "+4570405205"}), NOW).unwrap();
        assert_eq!(plan(&s, json!({})), false);
        for i in 0..(MAX_ALLOWED + 3) {
            s.allow(&format!("+4560{:06}", i)).unwrap();
        }
        let all: Vec<String> = s.read("caller-allowed.json").unwrap();
        assert_eq!(all.len(), MAX_ALLOWED);
    }

    #[test]
    fn the_order_signal_op_validates() {
        let s = store("orderop");
        run(&s, "order", &json!({"orderAtMs": NOW}), NOW).unwrap();
        assert!(s.order_active(NOW));
        assert!(run(&s, "order", &json!({"orderAtMs": NOW + 7_200_000}), NOW).is_err());
        // Missing or wrong type means "no order".
        run(&s, "order", &json!({}), NOW).unwrap();
        assert!(!s.order_active(NOW));
    }
}
