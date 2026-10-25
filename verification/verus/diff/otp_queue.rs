//! Differential test of the phone-queue part of `verification/verus/otp_expiry.rs`
//! (`forward_gate`, `forward_queue`, `enqueue`, `flush`) against the real `Bridge`
//! (REV of `otp_run.sh`, default lucina-rs dd7ec9e with F1 write-ahead;
//! `crates/lucina-android/src/bridge.rs`), over a mock relay on 127.0.0.1.
//!
//! It needs the crate-private `on_sms_at` / `flush_at` / `Transport::plain_http_for_tests`,
//! so it is a unit-test module: `diff/otp_run.sh` installs it as
//! `crates/lucina-android/src/verus_otp_queue.rs` and appends
//! `#[cfg(test)] mod verus_otp_queue;` to that crate's `lib.rs` in the snapshot.
//! Codes and the token are synthetic.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use lucina_phone_core::proto::CODE_TTL_MS;
use serde_json::{json, Value};

use crate::bridge::{Bridge, Forward, MAX_QUEUE};
use crate::net::Transport;

const KEY: [u8; 32] = [9; 32];
const TOKEN: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

/// Plain-Rust copy of the kernel (`sat_add`, `fresh_exec`, `forward_gate`,
/// `retain_fresh`, `enqueue`, `remove_id`, `forward_queue`, `flush`).
mod kernel {
    pub const TTL: u64 = 120_000;
    pub const MAX_QUEUE: usize = 8;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Pend {
        pub id: u64,
        pub received_at: u64,
    }

    fn sat_add(a: u64, b: u64) -> u64 {
        if a > u64::MAX - b {
            u64::MAX
        } else {
            a + b
        }
    }

    pub fn fresh(p: Pend, now: u64) -> bool {
        sat_add(p.received_at, TTL) > now
    }

    pub fn forward_gate(received_at_ms: i64, now: u64) -> Option<u64> {
        let t = if received_at_ms > 0 {
            let m = received_at_ms as u64;
            if m < now {
                m
            } else {
                now
            }
        } else {
            now
        };
        if sat_add(t, TTL) <= now {
            None
        } else {
            Some(t)
        }
    }

    pub fn retain_fresh(q: &mut Vec<Pend>, now: u64) {
        let out: Vec<Pend> = q.iter().copied().filter(|p| fresh(*p, now)).collect();
        *q = out;
    }

    pub fn enqueue(q: &mut Vec<Pend>, p: Pend, now: u64) {
        retain_fresh(q, now);
        while q.len() >= MAX_QUEUE {
            q.remove(0);
        }
        q.push(p);
    }

    pub fn remove_id(q: &mut Vec<Pend>, id: u64) {
        let out: Vec<Pend> = q.iter().copied().filter(|p| p.id != id).collect();
        *q = out;
    }

    /// F1: write-ahead enqueue, then the POST; any answer but Retry removes the entry.
    pub fn forward_queue(q: &mut Vec<Pend>, p: Pend, now: u64, answered: bool) {
        enqueue(q, p, now);
        if answered {
            remove_id(q, p.id);
        }
    }

    pub fn flush(q: &mut Vec<Pend>, now: u64, retry: &[bool]) -> Vec<Pend> {
        retain_fresh(q, now);
        let mut posted = Vec::new();
        while !q.is_empty() {
            let p = q.remove(0);
            let k = posted.len();
            posted.push(p);
            if k < retry.len() && retry[k] {
                q.insert(0, p);
                break;
            }
        }
        posted
    }
}

/// Answers each POST with the next scripted status (default 200) and records the bodies.
struct Mock {
    base: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<VecDeque<u16>>>,
    server: Arc<tiny_http::Server>,
}

impl Mock {
    fn new() -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}/v1/phone/otp", server.server_addr().to_ip().unwrap());
        let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
        let script: Arc<Mutex<VecDeque<u16>>> = Arc::default();
        let (s, b, sc) = (server.clone(), bodies.clone(), script.clone());
        thread::spawn(move || {
            for mut req in s.incoming_requests() {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                b.lock().unwrap().push(serde_json::from_str(&body).unwrap_or(Value::Null));
                let status = sc.lock().unwrap().pop_front().unwrap_or(200);
                let text = if status == 200 { r#"{"accepted":true}"# } else { r#"{"error":"busy"}"# };
                let _ = req.respond(tiny_http::Response::from_string(text).with_status_code(status));
            }
        });
        Self { base, bodies, script, server }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1)
    }
}

#[test]
fn queue_matches_kernel_and_only_fresh_codes_are_sent() {
    static N: AtomicU32 = AtomicU32::new(0);
    let mut r = Rng(0x5eed_0301);
    let (mut queued, mut expired, mut posted_total, mut evicted, mut answered) = (0, 0, 0, 0, 0);
    for scenario in 0..40 {
        let dir = std::env::temp_dir().join(format!(
            "verus-otp-queue-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mock = Mock::new();
        let mut b = Bridge::open_with(&dir, &KEY, None, Transport::plain_http_for_tests(mock.base.clone())).unwrap();
        b.configure(&json!({ "token": TOKEN }).to_string()).unwrap();
        let mut kq: Vec<kernel::Pend> = Vec::new();
        let mut now: u64 = 1_700_000_000_000;
        let n = 1 + r.below(14);
        for i in 0..n {
            now += r.below(20_000);
            let received_ms: i64 = match r.below(6) {
                0 => 0,
                1 => (now + r.below(30_000)) as i64,
                2 => (now - CODE_TTL_MS - r.below(20_000)) as i64,
                _ => (now - r.below(CODE_TTL_MS + 20_000)) as i64,
            };
            let code = 100_000 + scenario * 1_000 + i;
            // Mostly offline (503: Retry, the code stays queued); sometimes accepted or paused.
            let (status, want) = match r.below(5) {
                0 => (200, Forward::Sent),
                1 => (409, Forward::Paused),
                _ => (503, Forward::Queued),
            };
            mock.script.lock().unwrap().push_back(status);
            let got = b.on_sms_at("Bank", &format!("Your verification code is {code}."), received_ms, now).result;
            match kernel::forward_gate(received_ms, now) {
                None => {
                    assert_eq!(got, Forward::Expired, "scenario {scenario} code {i}");
                    mock.script.lock().unwrap().pop_back();
                    expired += 1;
                }
                Some(t) => {
                    assert_eq!(got, want, "scenario {scenario} code {i}");
                    if kq.len() >= MAX_QUEUE {
                        evicted += 1;
                    }
                    kernel::forward_queue(&mut kq, kernel::Pend { id: code, received_at: t }, now, status != 503);
                    if status == 503 {
                        queued += 1;
                    } else {
                        answered += 1;
                    }
                }
            }
            assert_eq!(b.status().pending as usize, kq.len(), "scenario {scenario} after code {i}");
            assert!(kq.len() <= MAX_QUEUE);
        }
        // Flush later; the k-th POST answers 503 (Retry) when k < kq.len().
        let at = now + r.below(CODE_TTL_MS + 30_000);
        let k = r.below(kq.len() as u64 + 2) as usize;
        let retry: Vec<bool> = (0..MAX_QUEUE + 1).map(|i| i == k).collect();
        {
            let mut s = mock.script.lock().unwrap();
            s.clear();
            s.extend(retry.iter().map(|&x| if x { 503 } else { 200 }));
        }
        mock.bodies.lock().unwrap().clear();
        let report = b.flush_at(at);
        let posted = kernel::flush(&mut kq, at, &retry);
        let real_posted: Vec<(u64, u64)> = mock
            .bodies
            .lock()
            .unwrap()
            .iter()
            .map(|v| (v["code"].as_str().unwrap().parse().unwrap(), v["receivedAt"].as_u64().unwrap()))
            .collect();
        let kernel_posted: Vec<(u64, u64)> = posted.iter().map(|p| (p.id, p.received_at)).collect();
        assert_eq!(real_posted, kernel_posted, "scenario {scenario}: flush at {at}");
        for p in &posted {
            assert!(kernel::fresh(*p, at), "a stale code was sent");
        }
        assert_eq!(report.pending as usize, kq.len());
        assert_eq!(report.retry, !kq.is_empty());
        posted_total += posted.len();
        drop(b);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(queued > 50 && expired > 10 && posted_total > 30 && evicted > 0 && answered > 20, "{queued} {expired} {posted_total} {evicted} {answered}");
    eprintln!("queue == kernel (F1): {queued} queued, {answered} answered at once, {expired} expired at once, {evicted} evictions, {posted_total} posted by flush");
}
