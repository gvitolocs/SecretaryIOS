//! Per-turn result and summary math.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Everything one turn measured. `t` maps event names to milliseconds from
/// t0 (the `end` frame for ws turns, the moment the request starts for share).
#[derive(Debug, Clone)]
pub struct TurnResult {
    pub mode: &'static str, // "ws" | "share"
    pub purpose: Option<String>,
    pub codec: Option<String>,
    /// TCP + TLS + upgrade (ws only).
    pub connect_ms: Option<u64>,
    /// Duration of the audio sent (ms).
    pub speech_ms: u64,
    pub t: BTreeMap<String, u64>,
    /// The server's `done.ms` object, as received.
    pub server_ms: Option<Value>,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub transcript_chars: u32,
    pub reply_chars: u32,
    /// Error code (`no_speech`, `timeout`, …).
    pub error: Option<String>,
    /// ws only: the server's `listening` echoed a `codec` key (section 10 server).
    pub server_section10: Option<bool>,
    /// Texts are kept but never printed except with --show-text.
    pub transcript: Option<String>,
    pub reply: Option<String>,
    /// Reply audio (ws only): s16le mono at `audio_rate`.
    pub audio: Vec<u8>,
    pub audio_rate: u32,
}

impl TurnResult {
    pub fn json(&self) -> Value {
        let mut t = serde_json::Map::new();
        for (k, v) in &self.t {
            t.insert(k.clone(), json!(v));
        }
        let mut v = json!({
            "mode": self.mode,
            "purpose": self.purpose,
            "codec": self.codec,
            "connect_ms": self.connect_ms,
            "speech_ms": self.speech_ms,
            "t": t,
            "server_ms": self.server_ms.clone().unwrap_or(Value::Null),
            "up_bytes": self.up_bytes,
            "down_bytes": self.down_bytes,
            "transcript_chars": self.transcript_chars,
            "reply_chars": self.reply_chars,
            "server_section10": self.server_section10,
        });
        if let Some(err) = &self.error {
            v["error"] = json!(err);
        }
        v
    }
}

/// (min, median, max) over the turns for each `t` key (keys present in at
/// least one turn).
pub fn summary(turns: &[TurnResult]) -> BTreeMap<String, (u64, u64, u64)> {
    let mut out = BTreeMap::new();
    for key in turns.iter().flat_map(|r| r.t.keys()).collect::<BTreeSet<&String>>() {
        let vals: Vec<u64> = turns.iter().filter_map(|r| r.t.get(key)).copied().collect();
        let mut sorted = vals.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2
        };
        out.insert(key.clone(), (sorted.first().copied().unwrap(), median, sorted.last().copied().unwrap()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(t: &[(&str, u64)]) -> TurnResult {
        TurnResult {
            mode: "ws",
            purpose: None,
            codec: None,
            connect_ms: None,
            speech_ms: 0,
            t: t.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            server_ms: None,
            up_bytes: 0,
            down_bytes: 0,
            transcript_chars: 0,
            reply_chars: 0,
            error: None,
            server_section10: None,
            transcript: None,
            reply: None,
            audio: Vec::new(),
            audio_rate: 0,
        }
    }

    #[test]
    fn summary_min_median_max() {
        let turns = vec![
            turn(&[("a", 10), ("b", 100)]),
            turn(&[("a", 20), ("b", 300)]),
            turn(&[("a", 30), ("b", 200)]),
        ];
        let s = summary(&turns);
        assert_eq!(s["a"], (10, 20, 30));
        assert_eq!(s["b"], (100, 200, 300));
    }

    #[test]
    fn summary_even_count_and_missing_keys() {
        let turns = vec![turn(&[("a", 10)]), turn(&[("a", 20), ("c", 5)]), turn(&[("c", 7)])];
        let s = summary(&turns);
        assert_eq!(s["a"], (10, 15, 20));
        assert_eq!(s["c"], (5, 6, 7));
    }

    #[test]
    fn json_shape() {
        let mut r = turn(&[("done", 5)]);
        r.error = Some("no_speech".into());
        let v = r.json();
        assert_eq!(v["mode"], "ws");
        assert_eq!(v["t"]["done"], 5);
        assert_eq!(v["error"], "no_speech");
    }
}
