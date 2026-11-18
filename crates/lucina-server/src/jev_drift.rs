//! The weekly JEV routing drift report (`lucina-rs jev drift-report`, docs/lucina-rust/JEV-ROUTING.md).
//!
//! - **Live traffic:** the `event="jev_first"` lines in `lucina-rs.service`'s journal
//!   ([`parse_log_line`], [`LiveStats`]): used vs fallback, reasons, intents, confidence buckets and
//!   how often Jev agrees with the tier-0 rule.
//! - **Labelled replay:** the owner's labelled turns (`<labels dir>/cal-in.jsonl`, `labels.txt`,
//!   `extras.jsonl`, 0600) through the running server's `/api/secretary/jev/route`, scored by
//!   confidence bucket, per intent and for the JEV-first policy at several thresholds ([`score`]).
//! - **Drift:** against the previous run's summary (`<data>/jev-drift/<date>.json`: numbers and
//!   intent names, never a turn's text) ([`drift`]).
//! - One ops record (`kind=status`, project `lucina`); `--recalibrate` adds per-intent JEV-first
//!   thresholds as a suggestion ([`recalibrate`]). Nothing here changes the router's settings.

use std::collections::{BTreeMap, HashSet};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::jev_route::{deterministic_intent, jev_beats_rule_from, JEV_FIRST_MIN};

/// Confidence buckets, `[lo, hi)`; the last one includes 1.0.
pub const BUCKETS: [(f64, f64); 5] = [(0.0, 0.5), (0.5, 0.6), (0.6, 0.75), (0.75, 0.9), (0.9, 1.0)];
pub const BUCKET_LABELS: [&str; 5] = [
    "[0, 0.5)",
    "[0.5, 0.6)",
    "[0.6, 0.75)",
    "[0.75, 0.9)",
    "[0.9, 1]",
];
/// JEV-first thresholds the replay scores.
pub const POLICY_THRESHOLDS: [f64; 5] = [0.6, 0.7, 0.75, 0.8, 0.9];
/// Drift: a bucket's accuracy falls this much, both runs having this many turns in it…
pub const DRIFT_ACCURACY_DROP: f64 = 0.05;
pub const DRIFT_MIN_SAMPLES: usize = 10;
/// …or the live used rate moves this much.
pub const DRIFT_USED_RATE: f64 = 0.15;
/// `--recalibrate`: intents Jev picked at least this often, and the precision to keep.
pub const RECAL_MIN_SAMPLES: usize = 10;
pub const RECAL_PRECISION: f64 = 0.9;
/// A suggested threshold must still leave this many of the intent's answers above it.
const RECAL_MIN_ABOVE: usize = 5;
const REPLAY_CONCURRENCY: usize = 4;
const REPLAY_DELAY: Duration = Duration::from_millis(250);
pub const DEFAULT_LABELS_DIR: &str = "/home/nez/Projects/Lucina/data/jev-cal";
const DEFAULT_DATA_DIR: &str = "/home/nez/Projects/Lucina/data";
/// Float comparisons at the drift limits (0.85 - 0.80 is 0.0499…).
const EPS: f64 = 1e-9;

pub fn bucket(confidence: f64) -> usize {
    BUCKETS
        .iter()
        .position(|&(_, hi)| confidence < hi)
        .unwrap_or(BUCKETS.len() - 1)
}

// ---------------------------------------------------------------- live traffic

/// One `jev_first` journal line.
#[derive(Debug, Clone, PartialEq)]
pub struct LogTurn {
    pub used: bool,
    pub reason: Option<String>,
    pub intent: Option<String>,
    pub confidence: Option<f64>,
    pub tier0: Option<String>,
    pub guard: Option<String>,
}

/// The journal keeps tracing's colours: drop the CSI sequences.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// The raw value after ` key=`: a quoted string, `Some(…)` or a bare token.
fn raw_field<'a>(fields: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!(" {key}=");
    let rest = &fields[fields.find(&pat)? + pat.len()..];
    let (mut quoted, mut escaped, mut depth) = (false, false, 0usize);
    for (i, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && !quoted && depth == 0 => return Some(&rest[..i]),
            _ => {}
        }
    }
    Some(rest)
}

/// `None` → None; `Some("x")`, `"x"`, `Some(0.6)`, `x` → the inner text.
fn field_value(raw: &str) -> Option<String> {
    if raw == "None" {
        return None;
    }
    let v = raw
        .strip_prefix("Some(")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(raw);
    let v = v
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(v);
    Some(v.replace("\\\"", "\""))
}

/// A `jev_first` line from `crate::context::classify` (used or fallback), else `None`.
pub fn parse_log_line(line: &str) -> Option<LogTurn> {
    if !line.contains("jev_first") {
        return None;
    }
    let line = strip_ansi(line);
    let at = line.find(r#"event="jev_first""#)?;
    // Only the event's own fields (a span prefix could carry its own `intent=`).
    let fields = format!(" {}", &line[at..]);
    let get = |k: &str| raw_field(&fields, k).and_then(field_value);
    let text = |k: &str| get(k).filter(|v| !v.is_empty());
    let used = match get("used")?.as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some(LogTurn {
        used,
        reason: text("reason"),
        intent: text("intent"),
        confidence: get("confidence").and_then(|c| c.parse().ok()),
        tier0: text("tier0"),
        guard: text("guard"),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveStats {
    pub turns: usize,
    pub used: usize,
    /// Why JEV-first did not route (fallback lines).
    pub reasons: BTreeMap<String, usize>,
    pub intents: BTreeMap<String, usize>,
    pub guards: BTreeMap<String, usize>,
    pub buckets: [usize; 5],
    pub no_confidence: usize,
    /// Turns with both a tier-0 rule and a Jev intent, and how many agree.
    pub tier0_compared: usize,
    pub tier0_agreed: usize,
    /// Yes/no questions answered by Jev (`event="jev_yesno"`, crate::jev_yesno); empty in older summaries.
    #[serde(default)]
    pub yesno: YesNoStats,
}

/// One `jev_yesno` journal line of a chat turn (previews from the replay endpoint are not counted).
#[derive(Debug, Clone, PartialEq)]
pub struct YesNoTurn {
    /// answer, unsure, hedge, pass.
    pub outcome: String,
    pub why: Option<String>,
    pub gate: Option<f64>,
    pub kind: Option<String>,
    pub p_yes: Option<f64>,
    pub jev_ms: Option<u64>,
}

pub fn parse_yesno_line(line: &str) -> Option<YesNoTurn> {
    if !line.contains("jev_yesno") {
        return None;
    }
    let line = strip_ansi(line);
    let at = line.find(r#"event="jev_yesno""#)?;
    let fields = format!(" {}", &line[at..]);
    let get = |k: &str| {
        raw_field(&fields, k)
            .and_then(field_value)
            .filter(|v| !v.is_empty())
    };
    if get("source").as_deref() != Some("turn") {
        return None;
    }
    Some(YesNoTurn {
        outcome: get("outcome")?,
        why: get("why"),
        gate: get("gate").and_then(|v| v.parse().ok()),
        kind: get("kind"),
        p_yes: get("p_yes").and_then(|v| v.parse().ok()),
        jev_ms: get("jev_ms").and_then(|v| v.parse().ok()),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct YesNoStats {
    /// Question-shaped owner turns that reached the yes/no path.
    pub turns: usize,
    /// Jev's gate said yes/no (≥ the gate threshold).
    pub yes_no: usize,
    pub answered: usize,
    pub answered_yes: usize,
    pub unsure: usize,
    pub hedged: usize,
    /// Why the normal path answered alone (not_yes_no, live_lookup, jev_unavailable, …).
    pub passes: BTreeMap<String, usize>,
    pub kinds: BTreeMap<String, usize>,
    /// Gate P, by [`BUCKETS`].
    pub gate_buckets: [usize; 5],
    pub jev_ms_total: u64,
    pub jev_ms_n: usize,
}

impl YesNoStats {
    pub fn add(&mut self, t: &YesNoTurn) {
        self.turns += 1;
        match t.outcome.as_str() {
            "answer" => {
                self.answered += 1;
                if t.p_yes.is_some_and(|p| p >= 0.5) {
                    self.answered_yes += 1;
                }
            }
            "unsure" => self.unsure += 1,
            "hedge" => self.hedged += 1,
            _ => {
                *self
                    .passes
                    .entry(t.why.clone().unwrap_or_else(|| "none".into()))
                    .or_default() += 1
            }
        }
        // Past the gate: every outcome but a pass before it (not question-shaped, not yes/no, Jev down).
        let past_gate = matches!(
            t.why.as_deref(),
            Some("live_lookup" | "not_in_facts" | "about_lucina")
        );
        if t.outcome != "pass" || past_gate {
            self.yes_no += 1;
            if let Some(k) = &t.kind {
                *self.kinds.entry(k.clone()).or_default() += 1;
            }
        }
        if let Some(g) = t.gate {
            self.gate_buckets[bucket(g)] += 1;
        }
        if let Some(ms) = t.jev_ms.filter(|ms| *ms > 0) {
            self.jev_ms_total += ms;
            self.jev_ms_n += 1;
        }
    }

    /// Of the yes/no questions, how many Jev answered itself.
    pub fn answered_rate(&self) -> Option<f64> {
        (self.yes_no > 0).then(|| self.answered as f64 / self.yes_no as f64)
    }

    pub fn mean_jev_ms(&self) -> Option<u64> {
        (self.jev_ms_n > 0).then(|| self.jev_ms_total / self.jev_ms_n as u64)
    }
}

impl LiveStats {
    pub fn add(&mut self, t: &LogTurn) {
        self.turns += 1;
        if t.used {
            self.used += 1;
        } else {
            *self
                .reasons
                .entry(t.reason.clone().unwrap_or_else(|| "none".into()))
                .or_default() += 1;
        }
        *self
            .intents
            .entry(t.intent.clone().unwrap_or_else(|| "none".into()))
            .or_default() += 1;
        if let Some(g) = &t.guard {
            *self.guards.entry(g.clone()).or_default() += 1;
        }
        match t.confidence {
            Some(c) => self.buckets[bucket(c)] += 1,
            None => self.no_confidence += 1,
        }
        // Every turn a tier-0 rule fired. No Jev intent does not agree.
        if let Some(rule) = &t.tier0 {
            self.tier0_compared += 1;
            if t.intent.as_ref() == Some(rule) {
                self.tier0_agreed += 1;
            }
        }
    }

    pub fn used_rate(&self) -> Option<f64> {
        (self.turns > 0).then(|| self.used as f64 / self.turns as f64)
    }

    pub fn tier0_agreement(&self) -> Option<f64> {
        (self.tier0_compared > 0).then(|| self.tier0_agreed as f64 / self.tier0_compared as f64)
    }
}

/// The last `days` days of `lucina-rs.service`'s journal (read line by line: a week of logs is big).
pub fn journal_stats(days: u32) -> anyhow::Result<LiveStats> {
    let mut child = std::process::Command::new("journalctl")
        .args([
            "--user",
            "-u",
            "lucina-rs",
            "--since",
            &format!("-{days} days"),
            "-o",
            "cat",
            "--no-pager",
        ])
        .stdout(std::process::Stdio::piped())
        // Inherit stderr: a filled stderr pipe would stall journalctl while we read stdout.
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("journalctl: {e}"))?;
    let mut out = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut stats = LiveStats::default();
    let mut buf = Vec::new();
    while out.read_until(b'\n', &mut buf)? > 0 {
        let line = String::from_utf8_lossy(&buf);
        if let Some(t) = parse_log_line(&line) {
            stats.add(&t);
        } else if let Some(t) = parse_yesno_line(&line) {
            stats.yesno.add(&t);
        }
        buf.clear();
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("journalctl exited with {status}");
    }
    Ok(stats)
}

// ---------------------------------------------------------------- labelled replay

/// A labelled turn to replay. No `Debug`: the text is the owner's and must not reach a log.
#[derive(Clone, PartialEq)]
pub struct CalTurn {
    pub text: String,
    pub recent: String,
    pub platform: &'static str,
    pub labels: Vec<String>,
}

/// `labels.txt`: `<line-no> <intent>[|<intent>…]`, line numbers of `cal-in.jsonl` from 1.
pub fn parse_labels(text: &str) -> BTreeMap<usize, Vec<String>> {
    text.lines()
        .filter_map(|l| l.trim().split_once(' '))
        .filter_map(|(n, l)| Some((n.parse().ok()?, split_labels(l))))
        .collect()
}

fn split_labels(l: &str) -> Vec<String> {
    l.split('|')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The turns to replay: `cal-in.jsonl` rows labelled by their own `label` or by `labels.txt`,
/// then the `extras.jsonl` rows not already in it (by `id`). Unlabelled and `skip` rows are left out.
fn jsonl_rows(name: &str, text: &str) -> anyhow::Result<Vec<(usize, Value)>> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        let v =
            serde_json::from_str(l).map_err(|e| anyhow::anyhow!("{name} line {}: {e}", i + 1))?;
        out.push((i + 1, v));
    }
    Ok(out)
}

pub fn cal_turns(cal_in: &str, labels: &str, extras: Option<&str>) -> anyhow::Result<Vec<CalTurn>> {
    let labels = parse_labels(labels);
    let main = jsonl_rows("cal-in.jsonl", cal_in)?;
    let ids: HashSet<&str> = main.iter().filter_map(|(_, v)| v["id"].as_str()).collect();
    let extra = match extras {
        Some(t) => jsonl_rows("extras.jsonl", t)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for (line, v) in &main {
        // A row's own label wins (the extras copied onto the end of cal-in). Otherwise the
        // line number in labels.txt, counting blank lines, from 1.
        let l = match v["label"].as_str() {
            Some(l) => split_labels(l),
            None => labels.get(line).cloned().unwrap_or_default(),
        };
        out.extend(cal_turn(v, l));
    }
    for (_, v) in extra
        .iter()
        .filter(|(_, v)| !v["id"].as_str().is_some_and(|id| ids.contains(id)))
    {
        out.extend(cal_turn(v, split_labels(v["label"].as_str().unwrap_or(""))));
    }
    Ok(out)
}

fn cal_turn(v: &Value, labels: Vec<String>) -> Option<CalTurn> {
    let text = v["text"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    if labels.is_empty() || labels[0] == "skip" {
        return None;
    }
    Some(CalTurn {
        text: text.to_owned(),
        recent: v["recent"].as_str().unwrap_or("").to_owned(),
        platform: if v["channel"] == "telegram" {
            "telegram"
        } else {
            "secretary"
        },
        labels,
    })
}

/// What `/jev/route` made of a labelled turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Routed {
    pub labels: Vec<String>,
    pub jev: Option<String>,
    pub confidence: Option<f64>,
    /// The live router's intent.
    pub routed: String,
    pub tier0: Option<String>,
}

pub fn routed_from(labels: Vec<String>, v: &Value) -> Routed {
    Routed {
        labels,
        jev: v["jev"].as_str().map(str::to_owned),
        confidence: v["jevConfidence"].as_f64(),
        routed: v["routed"]["intent"].as_str().unwrap_or("none").to_owned(),
        tier0: v["tier0"].as_str().map(str::to_owned),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    pub n: usize,
    pub right: usize,
}

impl Tally {
    pub fn add(&mut self, right: bool) {
        self.n += 1;
        self.right += usize::from(right);
    }

    pub fn rate(&self) -> Option<f64> {
        (self.n > 0).then(|| self.right as f64 / self.n as f64)
    }

    fn merge(self, o: Tally) -> Tally {
        Tally {
            n: self.n + o.n,
            right: self.right + o.right,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    pub turns: usize,
    /// Turns Jev did not answer (off, quota, timeout).
    pub jev_missing: usize,
    pub jev: Tally,
    pub buckets: [Tally; 5],
    /// Precision of each intent Jev picked at JEV-first's default threshold or above.
    pub per_intent: BTreeMap<String, Tally>,
    pub live: Tally,
    pub policy: Vec<(f64, Tally)>,
}

impl Replay {
    /// Jev's accuracy from JEV-first's default threshold (0.75) up.
    pub fn sure(&self) -> Tally {
        self.buckets[bucket(JEV_FIRST_MIN)..]
            .iter()
            .fold(Tally::default(), |a, b| a.merge(*b))
    }
}

/// A turn labelled with several intents is right on any of them.
pub fn right(labels: &[String], intent: &str) -> bool {
    labels.iter().any(|l| l == intent)
}

/// Jev's accuracy by bucket and intent, the live router's, and the JEV-first policy's at each
/// threshold (Jev's answer when sure and not `unknown`, unless a deterministic rule matched and
/// Jev's answer did not beat it — the same precedence [`crate::jev_route::jev_first`] uses).
pub fn score(rows: &[Routed]) -> Replay {
    score_from(
        rows,
        std::env::var("LUCINA_JEV_FIRST_MIN").ok().as_deref(),
        std::env::var("LUCINA_JEV_FIRST_INTENT_MIN").ok().as_deref(),
        std::env::var("LUCINA_JEV_OVER_RULE").ok().as_deref(),
        std::env::var("LUCINA_JEV_OVER_RULE_MIN").ok().as_deref(),
    )
}

/// [`score`] with the four `LUCINA_JEV_*` values as arguments, like
/// [`crate::jev_route::jev_first_from`]: a test names them instead of inheriting the environment.
pub fn score_from(
    rows: &[Routed],
    first_min: Option<&str>,
    first_intent_min: Option<&str>,
    over_rule: Option<&str>,
    over_rule_min: Option<&str>,
) -> Replay {
    let mut r = Replay {
        turns: rows.len(),
        policy: POLICY_THRESHOLDS
            .iter()
            .map(|&t| (t, Tally::default()))
            .collect(),
        ..Default::default()
    };
    for x in rows {
        r.live.add(right(&x.labels, &x.routed));
        let jev = x.jev.as_deref().zip(x.confidence);
        match jev {
            Some((j, c)) => {
                let ok = right(&x.labels, j);
                r.jev.add(ok);
                r.buckets[bucket(c)].add(ok);
                if c >= JEV_FIRST_MIN {
                    r.per_intent.entry(j.to_owned()).or_default().add(ok);
                }
            }
            None => r.jev_missing += 1,
        }
        let rule_keeps = x.tier0.as_deref().is_some_and(|t| {
            deterministic_intent(t)
                && !jev.map_or(true, |(j, c)| {
                    jev_beats_rule_from(
                        j,
                        c,
                        t,
                        over_rule,
                        over_rule_min,
                        first_min,
                        first_intent_min,
                    )
                })
        });
        // The replay does not model the MCP side-effect guard (`classify` returns plain chat there),
        // so on MCP samples the report can name Jev's intent where the server answered chat.
        for (th, t) in &mut r.policy {
            // The live rule: a sure, known intent, unless a deterministic route keeps the turn
            // (which only a sure question of Jev's own intents takes over, never one that acts).
            let pick = jev
                .filter(|&(j, c)| c >= *th && !j.is_empty() && j != "unknown" && !rule_keeps)
                .map_or(x.routed.as_str(), |(j, _)| j);
            t.add(right(&x.labels, pick));
        }
    }
    r
}

/// Per intent Jev picked at least [`RECAL_MIN_SAMPLES`] times: the lowest threshold (0.50, 0.55,
/// … 1.00) whose answers above it keep precision ≥ [`RECAL_PRECISION`], `None` when none does.
pub fn recalibrate(rows: &[Routed]) -> BTreeMap<String, Option<f64>> {
    let mut by_intent: BTreeMap<&str, Vec<(f64, bool)>> = BTreeMap::new();
    for x in rows {
        if let (Some(j), Some(c)) = (x.jev.as_deref(), x.confidence) {
            if !j.is_empty() && j != "unknown" {
                by_intent
                    .entry(j)
                    .or_default()
                    .push((c, right(&x.labels, j)));
            }
        }
    }
    by_intent
        .into_iter()
        .filter(|(_, v)| v.len() >= RECAL_MIN_SAMPLES)
        .map(|(intent, v)| {
            let th = (10..=20).map(|i| i as f64 / 20.0).find(|&t| {
                let above: Vec<bool> = v
                    .iter()
                    .filter(|(c, _)| *c >= t - EPS)
                    .map(|x| x.1)
                    .collect();
                above.len() >= RECAL_MIN_ABOVE
                    && above.iter().filter(|&&ok| ok).count() as f64
                        >= RECAL_PRECISION * above.len() as f64 - EPS
            });
            (intent.to_owned(), th)
        })
        .collect()
}

// ---------------------------------------------------------------- drift

/// One run's numbers, `<data>/jev-drift/<date>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub date: String,
    pub days: u32,
    pub live: LiveStats,
    pub replay: Option<Replay>,
}

/// What moved since `prev`: a bucket's accuracy falling ≥ 5 points (≥ 10 turns in it both times),
/// or the live used rate moving ≥ 15 points.
pub fn drift(prev: &Summary, cur: &Summary) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(p), Some(c)) = (&prev.replay, &cur.replay) {
        for (i, (a, b)) in p.buckets.iter().zip(&c.buckets).enumerate() {
            if a.n < DRIFT_MIN_SAMPLES || b.n < DRIFT_MIN_SAMPLES {
                continue;
            }
            let (ra, rb) = (a.rate().unwrap_or(0.0), b.rate().unwrap_or(0.0));
            if ra - rb >= DRIFT_ACCURACY_DROP - EPS {
                out.push(format!(
                    "accuracy in {} fell {:.0} points ({ra:.2} → {rb:.2}, {} turns)",
                    BUCKET_LABELS[i],
                    (ra - rb) * 100.0,
                    b.n
                ));
            }
        }
    }
    let (py, cy) = (&prev.live.yesno, &cur.live.yesno);
    if py.yes_no >= DRIFT_MIN_SAMPLES && cy.yes_no >= DRIFT_MIN_SAMPLES {
        if let (Some(a), Some(b)) = (py.answered_rate(), cy.answered_rate()) {
            if (b - a).abs() >= DRIFT_USED_RATE - EPS {
                out.push(format!(
                    "yes/no questions Jev answered itself moved {:+.0} points ({:.0}% → {:.0}%)",
                    (b - a) * 100.0,
                    a * 100.0,
                    b * 100.0
                ));
            }
        }
    }
    if let (Some(a), Some(b)) = (prev.live.used_rate(), cur.live.used_rate()) {
        if (b - a).abs() >= DRIFT_USED_RATE - EPS {
            out.push(format!(
                "JEV-first used rate moved {:+.0} points ({:.0}% → {:.0}%)",
                (b - a) * 100.0,
                a * 100.0,
                b * 100.0
            ));
        }
    }
    out
}

/// The latest summary in `dir` from before `date`.
pub fn previous(dir: &Path, date: &str) -> Option<Summary> {
    let latest = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.len() == "YYYY-MM-DD.json".len() && n.ends_with(".json") && n[..10] < *date)
        .max()?;
    serde_json::from_slice(&std::fs::read(dir.join(latest)).ok()?).ok()
}

// ---------------------------------------------------------------- the record

fn rate(t: &Tally) -> String {
    t.rate()
        .map_or("-".into(), |r| format!("{r:.2} ({}/{})", t.right, t.n))
}

fn counts(m: &BTreeMap<String, usize>) -> String {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if v.is_empty() {
        return "none".into();
    }
    v.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The record's title and markdown body.
pub fn render(
    cur: &Summary,
    prev: Option<&Summary>,
    flags: &[String],
    replay_note: Option<&str>,
    recal: Option<&BTreeMap<String, Option<f64>>>,
) -> (String, String) {
    let span = if cur.days == 7 {
        "week".to_owned()
    } else {
        format!("{} days", cur.days)
    };
    let verdict = match flags.len() {
        0 => "no drift".to_owned(),
        n => format!("DRIFT ({n} flag{})", if n == 1 { "" } else { "s" }),
    };
    let head = match &cur.replay {
        Some(r) => format!(
            "accuracy {} (≥0.75 bucket {})",
            r.jev.rate().map_or("-".into(), |x| format!("{x:.2}")),
            r.sure().rate().map_or("-".into(), |x| format!("{x:.2}"))
        ),
        None => format!(
            "{} live turns, JEV-first used {}, no labelled replay",
            cur.live.turns,
            cur.live
                .used_rate()
                .map_or("-".into(), |x| format!("{:.0}%", x * 100.0))
        ),
    };
    let title = format!(
        "JEV routing drift, {span} to {}: {head}, {verdict}",
        cur.date
    );

    let l = &cur.live;
    let mut b = format!(
        "**Live traffic** (journal, last {} days): {} turns, JEV-first used {} ({}), tier-0 agreement {} (of {}).\n\n",
        cur.days,
        l.turns,
        l.used_rate().map_or("-".into(), |x| format!("{:.0}%", x * 100.0)),
        l.used,
        l.tier0_agreement().map_or("-".into(), |x| format!("{:.0}%", x * 100.0)),
        l.tier0_compared,
    );
    b += "| confidence | live turns | labelled turns | JEV right |\n|---|---|---|---|\n";
    for (i, label) in BUCKET_LABELS.iter().enumerate() {
        let t = cur
            .replay
            .as_ref()
            .map(|r| r.buckets[i])
            .unwrap_or_default();
        b += &format!("| {label} | {} | {} | {} |\n", l.buckets[i], t.n, rate(&t));
    }
    if l.no_confidence > 0 {
        b += &format!("| no answer | {} | | |\n", l.no_confidence);
    }
    b += &format!("\nFallback reasons: {}.\n", counts(&l.reasons));
    if !l.guards.is_empty() {
        b += &format!("Guards: {}.\n", counts(&l.guards));
    }
    b += &format!("Intents: {}.\n", counts(&l.intents));

    let y = &l.yesno;
    b += &format!(
        "\n**Yes/no questions** (`jev_yesno`, Jev gate ≥ {:.2}, answer ≥ {:.2}): {} question-shaped turns, {} yes/no; Jev answered {} ({}; yes {}, no {}), unsure {}, hedged {}; mean Jev latency {}.\n",
        crate::jev_yesno::GATE_MIN,
        crate::jev_yesno::ANSWER_MIN,
        y.turns,
        y.yes_no,
        y.answered,
        y.answered_rate().map_or("-".into(), |x| format!("{:.0}%", x * 100.0)),
        y.answered_yes,
        y.answered - y.answered_yes,
        y.unsure,
        y.hedged,
        y.mean_jev_ms().map_or("-".into(), |ms| format!("{ms} ms")),
    );
    if y.turns > 0 {
        b += &format!(
            "Kinds: {}. Normal path alone: {}. Gate P by bucket: {}.\n",
            counts(&y.kinds),
            counts(&y.passes),
            BUCKET_LABELS
                .iter()
                .zip(y.gate_buckets)
                .map(|(l, n)| format!("{l} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    match (&cur.replay, replay_note) {
        (Some(r), _) => {
            b += &format!(
                "\n**Labelled replay** ({} turns, Jev unanswered {}): Jev {}, live router {}.\n\n",
                r.turns,
                r.jev_missing,
                rate(&r.jev),
                rate(&r.live)
            );
            b += "| policy | right |\n|---|---|\n";
            b += &format!("| live router | {} |\n", rate(&r.live));
            for (th, t) in &r.policy {
                b += &format!("| JEV-first ≥ {th:.2} | {} |\n", rate(t));
            }
            b += "\n| Jev intent (≥ 0.75) | precision |\n|---|---|\n";
            for (k, t) in &r.per_intent {
                b += &format!("| {k} | {} |\n", rate(t));
            }
        }
        (None, note) => {
            b += &format!("\n**Labelled replay:** {}.\n", note.unwrap_or("skipped"));
        }
    }

    match prev {
        None => b += "\n**Drift:** no earlier report to compare with.\n",
        Some(p) if flags.is_empty() => b += &format!("\n**Drift** (vs {}): none.\n", p.date),
        Some(p) => {
            b += &format!("\n**Drift** (vs {}):\n", p.date);
            for f in flags {
                b += &format!("- {f}\n");
            }
        }
    }

    if let Some(recal) = recal {
        b += &format!(
            "\n**Recalibration suggestion** (not applied): per intent Jev picked ≥ {RECAL_MIN_SAMPLES} times, the lowest threshold on a 0.05 grid (0.50–1.00) that keeps precision ≥ {RECAL_PRECISION} with at least {RECAL_MIN_ABOVE} answers still above it. Set `LUCINA_JEV_FIRST_INTENT_MIN` (it overrides `LUCINA_JEV_FIRST_MIN`).\n\n"
        );
        let set: Vec<String> = recal
            .iter()
            .filter_map(|(k, t)| t.map(|t| format!("{k}={t:.2}")))
            .collect();
        if set.is_empty() {
            b += "No intent reaches it.\n";
        } else {
            b += &format!("`LUCINA_JEV_FIRST_INTENT_MIN={}`\n", set.join(","));
        }
        let none: Vec<&str> = recal
            .iter()
            .filter(|(_, t)| t.is_none())
            .map(|(k, _)| k.as_str())
            .collect();
        if !none.is_empty() {
            b += &format!(
                "\nNo threshold keeps it for: {} (leave them to the rules).\n",
                none.join(", ")
            );
        }
    }
    (title, b)
}

// ---------------------------------------------------------------- the command

pub struct Args {
    pub days: u32,
    pub labels_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub recalibrate: bool,
}

async fn replay(
    c: &crate::decide::cli::Client,
    turns: Vec<CalTurn>,
) -> anyhow::Result<Vec<Routed>> {
    use futures_util::{StreamExt as _, TryStreamExt as _};
    let total = turns.len();
    futures_util::stream::iter(turns.into_iter().enumerate())
        .map(|(i, t)| async move {
            tokio::time::sleep(REPLAY_DELAY).await;
            let body = json!({"text": t.text, "recent": t.recent, "platform": t.platform});
            let v = c.post("jev/route", &body).await.map_err(|e| {
                anyhow::anyhow!("labelled replay stopped at turn {}/{total}: {e}", i + 1)
            })?;
            Ok::<_, anyhow::Error>(routed_from(t.labels, &v))
        })
        .buffer_unordered(REPLAY_CONCURRENCY)
        .try_collect()
        .await
}

fn read_cal(dir: &Path) -> anyhow::Result<Vec<CalTurn>> {
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .map_err(|e| anyhow::anyhow!("{}: {e}", dir.join(name).display()))
    };
    let extras = dir.join("extras.jsonl");
    let extras = extras.exists().then(|| read("extras.jsonl")).transpose()?;
    cal_turns(
        &read("cal-in.jsonl")?,
        &read("labels.txt")?,
        extras.as_deref(),
    )
}

fn write_summary(dir: &Path, date: &str, cur: &Summary) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{date}.json"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(&serde_json::to_vec_pretty(cur)?)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// `lucina-rs jev drift-report`: journal, replay, drift, one ops record (or print it on `--dry-run`).
pub async fn run(a: Args) -> anyhow::Result<()> {
    use crate::decide::cli::{setting, Client};
    if a.days == 0 {
        anyhow::bail!("--days must be at least 1");
    }
    let date = chrono::Local::now().date_naive().to_string();
    let days = a.days;
    let live = tokio::task::spawn_blocking(move || journal_stats(days)).await??;

    let labels_dir = a
        .labels_dir
        .or_else(|| setting("LUCINA_JEV_CAL_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LABELS_DIR));
    let (rows, note) = if labels_dir.is_dir() {
        let turns = read_cal(&labels_dir)?;
        (Some(replay(&Client::from_env()?, turns).await?), None)
    } else {
        (
            None,
            Some(format!(
                "no labelled set at {}, skipped",
                labels_dir.display()
            )),
        )
    };

    let dir = setting("LUCINA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
        .join("jev-drift");
    let prev = previous(&dir, &date);
    let cur = Summary {
        date: date.clone(),
        days,
        live,
        replay: rows.as_deref().map(score),
    };
    let flags = prev.as_ref().map(|p| drift(p, &cur)).unwrap_or_default();
    let recal = match (&rows, a.recalibrate) {
        (Some(rows), true) => Some(recalibrate(rows)),
        _ => None,
    };
    let (title, body) = render(&cur, prev.as_ref(), &flags, note.as_deref(), recal.as_ref());
    // The recalibration run shares the weekly run's day: its own key, or it would be a duplicate.
    let key = if a.recalibrate {
        "jev-recal"
    } else {
        "jev-drift"
    };
    let record = json!({
        "kind": "status",
        "project": "lucina",
        "title": title,
        "body": body,
        "dedup_key": format!("{key}-{date}"),
    });
    if a.dry_run {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }
    write_summary(&dir, &date, &cur)?;
    let ops = crate::ops::open_cli(None, None, None)?;
    let r = ops
        .record(serde_json::from_value(record)?, "jev-drift")
        .await?;
    println!("{}", serde_json::to_string(&r)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_no_lines_are_counted_from_chat_turns_only() {
        let line = |source: &str, outcome: &str, why: &str, p_yes: &str| {
            format!("2026-10-29T12:00:00Z  INFO lucina_jev::route: jev_yesno event=\"jev_yesno\" source=\"{source}\" outcome=\"{outcome}\" why=\"{why}\" gate=Some(0.93) kind=Some(\"personal\") sensitive=None p_yes={p_yes} grounded=Some(0.9) facts=12 jev_ms=410 total_ms=1300")
        };
        let t = parse_yesno_line(&line("turn", "answer", "", "Some(0.97)")).unwrap();
        assert_eq!(t.outcome, "answer");
        assert_eq!(t.why, None);
        assert_eq!(t.gate, Some(0.93));
        assert_eq!(t.kind.as_deref(), Some("personal"));
        assert_eq!(t.p_yes, Some(0.97));
        assert_eq!(t.jev_ms, Some(410));
        assert!(parse_yesno_line(&line("preview", "answer", "", "Some(0.97)")).is_none());
        assert!(
            parse_log_line(&line("turn", "answer", "", "None")).is_none(),
            "not a jev_first line"
        );

        let mut y = YesNoStats::default();
        y.add(&t);
        y.add(&parse_yesno_line(&line("turn", "answer", "", "Some(0.1)")).unwrap());
        y.add(&parse_yesno_line(&line("turn", "unsure", "", "Some(0.5)")).unwrap());
        y.add(&parse_yesno_line(&line("turn", "pass", "not_yes_no", "None")).unwrap());
        y.add(&parse_yesno_line(&line("turn", "pass", "live_lookup", "Some(0.5)")).unwrap());
        assert_eq!(
            (y.turns, y.yes_no, y.answered, y.answered_yes, y.unsure),
            (5, 4, 2, 1, 1)
        );
        assert_eq!(y.passes.get("not_yes_no"), Some(&1));
        assert_eq!(y.answered_rate(), Some(0.5));
        assert_eq!(y.mean_jev_ms(), Some(410));
        assert_eq!(y.gate_buckets[4], 5);

        // Older summaries have no yes/no section.
        let old: LiveStats = serde_json::from_value(serde_json::json!({"turns": 3, "used": 1, "reasons": {}, "intents": {},
            "guards": {}, "buckets": [0, 0, 0, 0, 0], "no_confidence": 0, "tier0_compared": 0, "tier0_agreed": 0}))
            .unwrap();
        assert_eq!(old.yesno, YesNoStats::default());

        let mut a = Summary {
            date: "2026-10-22".into(),
            days: 7,
            live: LiveStats::default(),
            replay: None,
        };
        let mut b = a.clone();
        b.date = "2026-10-29".into();
        a.live.yesno = YesNoStats {
            yes_no: 20,
            answered: 16,
            ..YesNoStats::default()
        };
        b.live.yesno = YesNoStats {
            yes_no: 20,
            answered: 10,
            ..YesNoStats::default()
        };
        let flags = drift(&a, &b);
        assert!(
            flags
                .iter()
                .any(|f| f.starts_with("yes/no questions Jev answered itself moved -30 points")),
            "{flags:?}"
        );
        let (_, body) = render(&b, Some(&a), &flags, None, None);
        assert!(body.contains("**Yes/no questions**"), "{body}");
    }

    const USED: &str = "\u{1b}[2m2026-10-28T18:01:29.483962Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m \u{1b}[2mlucina_jev::route\u{1b}[0m\u{1b}[2m:\u{1b}[0m jev_first \u{1b}[3mevent\u{1b}[0m\u{1b}[2m=\u{1b}[0m\"jev_first\" \u{1b}[3mused\u{1b}[0m\u{1b}[2m=\u{1b}[0mtrue \u{1b}[3mintent\u{1b}[0m\u{1b}[2m=\u{1b}[0mtemporal_query \u{1b}[3mconfidence\u{1b}[0m\u{1b}[2m=\u{1b}[0m0.77 \u{1b}[3mtier0\u{1b}[0m\u{1b}[2m=\u{1b}[0mSome(\"chat\") \u{1b}[3mguard\u{1b}[0m\u{1b}[2m=\u{1b}[0m\"\"";

    fn labels(l: &str) -> Vec<String> {
        split_labels(l)
    }

    fn routed(l: &str, jev: Option<(&str, f64)>, live: &str, tier0: Option<&str>) -> Routed {
        Routed {
            labels: labels(l),
            jev: jev.map(|j| j.0.to_owned()),
            confidence: jev.map(|j| j.1),
            routed: live.to_owned(),
            tier0: tier0.map(str::to_owned),
        }
    }

    /// `score_from` with the policy the replay tests judge named here, not inherited from the
    /// process environment: the override over a deterministic route on, and Jev trusted from `0.9`
    /// up. The other two values are what the code defaults them to (`None`).
    fn scored(rows: &[Routed]) -> Replay {
        score_from(rows, Some("0.9"), None, Some("1"), None)
    }

    #[test]
    fn jev_parses_used_and_fallback_lines() {
        let t = parse_log_line(USED).expect("used line");
        assert_eq!(
            t,
            LogTurn {
                used: true,
                reason: None,
                intent: Some("temporal_query".into()),
                confidence: Some(0.77),
                tier0: Some("chat".into()),
                guard: None,
            }
        );
        let t = parse_log_line(r#"2026-10-28T18:01:29Z  INFO lucina_jev::route: jev_first fallback event="jev_first" used=false reason="below_threshold" intent=Some("computer_task") confidence=Some(0.62) tier0=None"#).unwrap();
        assert!(!t.used);
        assert_eq!(t.reason.as_deref(), Some("below_threshold"));
        assert_eq!(t.intent.as_deref(), Some("computer_task"));
        assert_eq!(t.confidence, Some(0.62));
        assert_eq!(t.tier0, None);
        let t = parse_log_line(r#"INFO lucina_jev::route: jev_first fallback event="jev_first" used=false reason="jev_unavailable" intent=None confidence=None tier0=Some("cursor_task")"#).unwrap();
        assert_eq!((t.intent, t.confidence), (None, None));
        assert_eq!(t.tier0.as_deref(), Some("cursor_task"));
        let t = parse_log_line(r#"jev_first event="jev_first" used=true intent=computer_task confidence=1.0 tier0=None guard="side_effect_on_mcp_turn:chat""#).unwrap();
        assert_eq!(t.guard.as_deref(), Some("side_effect_on_mcp_turn:chat"));
        // A span field before the event does not count, and other lines are not turns.
        let t = parse_log_line(r#"turn{intent=x}: jev_first event="jev_first" used=true intent=chat confidence=0.9 tier0=None guard="""#).unwrap();
        assert_eq!(t.intent.as_deref(), Some("chat"));
        assert_eq!(
            parse_log_line(r#"INFO lucina_jev::route: jev intent intent="chat" confidence=0.9"#),
            None
        );
        assert_eq!(
            parse_log_line(r#"jev_first event="jev_first" intent=chat"#),
            None,
            "no used="
        );
    }

    #[test]
    fn jev_buckets_split_at_their_lower_edges() {
        assert_eq!(bucket(0.0), 0);
        assert_eq!(bucket(0.4999), 0);
        assert_eq!(bucket(0.5), 1);
        assert_eq!(bucket(0.6), 2);
        assert_eq!(bucket(0.7499), 2);
        assert_eq!(bucket(0.75), 3);
        assert_eq!(bucket(0.9), 4);
        assert_eq!(bucket(1.0), 4);
    }

    #[test]
    fn jev_live_stats_count_reasons_buckets_and_agreement() {
        let mut s = LiveStats::default();
        for l in [
            USED,
            r#"jev_first fallback event="jev_first" used=false reason="below_threshold" intent=Some("computer_task") confidence=Some(0.62) tier0=None"#,
            r#"jev_first fallback event="jev_first" used=false reason="jev_unavailable" intent=None confidence=None tier0=Some("chat")"#,
            r#"jev_first event="jev_first" used=true intent=chat confidence=0.95 tier0=Some("chat") guard="""#,
        ] {
            s.add(&parse_log_line(l).unwrap());
        }
        assert_eq!((s.turns, s.used), (4, 2));
        assert_eq!(s.used_rate(), Some(0.5));
        assert_eq!(s.reasons["below_threshold"], 1);
        assert_eq!(s.reasons["jev_unavailable"], 1);
        assert_eq!(s.intents["none"], 1);
        assert_eq!(s.buckets, [0, 0, 1, 1, 1]);
        assert_eq!(s.no_confidence, 1);
        // The unanswered turn still counts: tier-0 fired and Jev did not agree.
        assert_eq!((s.tier0_compared, s.tier0_agreed), (3, 1));
    }

    #[test]
    fn jev_cal_turns_take_labels_from_the_file_or_the_row() {
        let cal_in = [
            r#"{"id":"a","channel":"telegram","text":"t1","recent":"r"}"#,
            "",
            r#"{"id":"b","channel":"web","text":"t2","recent":""}"#,
            r#"{"id":"c","channel":"secretary","text":"t3","recent":""}"#,
            r#"{"id":"x","channel":"ios","text":"t4","recent":"","label":"chat|web_crawl","set":"misroute"}"#,
        ]
        .join("\n");
        let extras = [
            r#"{"id":"x","text":"t4","label":"chat","set":"misroute"}"#,
            r#"{"id":"y","channel":"telegram","text":"t5","label":"cursor_task","set":"misroute"}"#,
        ]
        .join("\n");
        // Line 3 is t2 (a blank line does not renumber). t4's own labels win; x is not replayed twice.
        let t = cal_turns(&cal_in, "1 chat|web_crawl\n3 skip\n", Some(&extras)).unwrap();
        let got: Vec<(&str, &str, Vec<String>)> = t
            .iter()
            .map(|t| (t.text.as_str(), t.platform, t.labels.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("t1", "telegram", labels("chat|web_crawl")),
                ("t4", "secretary", labels("chat|web_crawl")),
                ("t5", "telegram", labels("cursor_task")),
            ]
        );
        let err = cal_turns("{not json", "", None).err().unwrap().to_string();
        assert!(err.starts_with("cal-in.jsonl line 1"), "{err}");
    }

    #[test]
    fn jev_score_counts_any_label_and_applies_the_policy() {
        let rows = [
            routed("chat|web_crawl", Some(("web_crawl", 0.95)), "chat", None),
            routed(
                "chat",
                Some(("social_greeting", 0.8)),
                "chat",
                Some("social_greeting"),
            ),
            routed(
                "computer_task",
                Some(("chat", 0.95)),
                "computer_task",
                Some("computer_task"),
            ),
            routed("chat", Some(("unknown", 0.99)), "chat", None),
            routed(
                "schedule_reminder",
                Some(("schedule_reminder", 0.65)),
                "chat",
                None,
            ),
            routed("chat", None, "chat", None),
        ];
        let r = scored(&rows);
        assert_eq!(r.turns, 6);
        assert_eq!(r.jev_missing, 1);
        assert_eq!(r.jev, Tally { n: 5, right: 2 });
        assert_eq!(r.buckets[2], Tally { n: 1, right: 1 });
        assert_eq!(r.buckets[3], Tally { n: 1, right: 0 });
        assert_eq!(r.buckets[4], Tally { n: 3, right: 1 });
        assert_eq!(r.sure(), Tally { n: 4, right: 1 });
        assert_eq!(r.per_intent["web_crawl"], Tally { n: 1, right: 1 });
        assert_eq!(r.per_intent["chat"], Tally { n: 1, right: 0 });
        assert_eq!(r.live, Tally { n: 6, right: 5 });
        // ≥ 0.6: Jev's wrong greeting and the right reminder; the computer rule and `unknown` keep the router.
        assert_eq!(r.policy[0], (0.6, Tally { n: 6, right: 5 }));
        // ≥ 0.8: the greeting still wins (wrong), the reminder goes back to the router (wrong).
        assert_eq!(r.policy[3], (0.8, Tally { n: 6, right: 4 }));
        assert_eq!(r.policy[4], (0.9, Tally { n: 6, right: 5 }));
    }

    #[test]
    fn jev_policy_keeps_the_router_for_an_empty_intent() {
        let r = scored(&[routed("chat", Some(("", 0.99)), "chat", None)]);
        assert_eq!(r.policy[0], (0.6, Tally { n: 1, right: 1 }));
    }

    #[test]
    fn jev_policy_replays_the_over_rule_precedence() {
        // A sure answer of Jev's own intents takes the turn from the grocery rule it disagrees
        // with, at every threshold; an unsure one leaves it with the rule.
        let over = routed(
            "grocery.shopping_list.query",
            Some(("grocery.shopping_list.query", 0.99)),
            "grocery.expense.query",
            Some("grocery.expense.query"),
        );
        let r = scored(&[over]);
        for (th, t) in r.policy {
            assert_eq!((th, t), (th, Tally { n: 1, right: 1 }), "{th}");
        }
        let kept = routed(
            "grocery.shopping_list.query",
            Some(("grocery.shopping_list.query", 0.85)),
            "grocery.expense.query",
            Some("grocery.expense.query"),
        );
        let r = scored(&[kept]);
        for (th, t) in r.policy {
            assert_eq!((th, t), (th, Tally { n: 1, right: 0 }), "{th}");
        }
    }

    /// TURN-01: the replay scores the same precedence the router uses. An answer that acts never
    /// takes the turn from a deterministic route, so even at `0.99` the rule keeps it — and a sample
    /// whose true label is that acting intent is wrong at every threshold, while Jev's own answer is
    /// still judged right on its own.
    #[test]
    fn an_acting_jev_intent_leaves_the_turn_to_the_rule_at_every_threshold() {
        let row = routed(
            "grocery.shopping_list.add",
            Some(("grocery.shopping_list.add", 0.99)),
            "grocery.expense.query",
            Some("grocery.expense.query"),
        );
        let r = scored(&[row]);
        assert_eq!(
            r.jev,
            Tally { n: 1, right: 1 },
            "Jev's intent was not the acting one it was scored as"
        );
        for (th, t) in r.policy {
            assert_eq!((th, t), (th, Tally { n: 1, right: 0 }), "{th}");
        }
    }

    #[test]
    fn jev_recalibrate_finds_the_lowest_threshold_keeping_precision() {
        let mut rows = Vec::new();
        // chat: wrong below 0.7, right from 0.7 up (12 answers).
        for i in 0..4 {
            rows.push(routed(
                "web_crawl",
                Some(("chat", 0.5 + i as f64 * 0.04)),
                "chat",
                None,
            ));
        }
        for i in 0..8 {
            rows.push(routed(
                "chat",
                Some(("chat", 0.7 + i as f64 * 0.03)),
                "chat",
                None,
            ));
        }
        // cursor_task: always wrong; social_greeting: too few answers.
        for _ in 0..10 {
            rows.push(routed("chat", Some(("cursor_task", 0.99)), "chat", None));
        }
        for _ in 0..3 {
            rows.push(routed(
                "social_greeting",
                Some(("social_greeting", 0.9)),
                "chat",
                None,
            ));
        }
        let r = recalibrate(&rows);
        assert_eq!(r.len(), 2);
        assert_eq!(r["chat"], Some(0.65));
        assert_eq!(r["cursor_task"], None);
    }

    fn summary(date: &str, used: usize, buckets: [Tally; 5]) -> Summary {
        Summary {
            date: date.into(),
            days: 7,
            live: LiveStats {
                turns: 100,
                used,
                ..Default::default()
            },
            replay: Some(Replay {
                buckets,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn jev_drift_flags_accuracy_drops_and_used_rate_moves() {
        let t = |n, right| Tally { n, right };
        let z = Tally::default();
        let prev = summary(
            "2026-10-28",
            60,
            [t(20, 10), t(9, 9), t(20, 18), t(40, 36), t(50, 45)],
        );
        // 10 points of used-rate, a 9-sample bucket, and drops under 5 points: no flag.
        let same = summary(
            "2026-11-04",
            70,
            [t(20, 10), t(9, 0), t(20, 18), t(40, 35), t(50, 45)],
        );
        assert!(drift(&prev, &same).is_empty(), "{:?}", drift(&prev, &same));
        let worse = summary(
            "2026-11-04",
            75,
            [t(20, 9), z, t(10, 8), t(40, 36), t(50, 45)],
        );
        let f = drift(&prev, &worse);
        assert_eq!(f.len(), 3, "{f:?}");
        assert!(
            f[0].starts_with("accuracy in [0, 0.5) fell 5 points"),
            "{f:?}"
        );
        assert!(
            f[1].starts_with("accuracy in [0.6, 0.75) fell 10 points"),
            "{f:?}"
        );
        assert!(
            f[2].starts_with("JEV-first used rate moved +15 points"),
            "{f:?}"
        );
        let no_replay = Summary {
            replay: None,
            ..worse.clone()
        };
        assert_eq!(drift(&prev, &no_replay).len(), 1);
    }

    #[test]
    fn jev_previous_summary_is_the_latest_before_today_and_render_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = |n, right| Tally { n, right };
        for d in ["2026-10-21", "2026-10-28", "2026-11-04"] {
            let s = summary(d, 50, [t(10, 5); 5]);
            std::fs::write(
                dir.path().join(format!("{d}.json")),
                serde_json::to_vec(&s).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let prev = previous(dir.path(), "2026-11-04").unwrap();
        assert_eq!(prev.date, "2026-10-28");
        assert!(previous(dir.path(), "2026-10-21").is_none());

        let mut cur = summary(
            "2026-11-04",
            50,
            [t(10, 5), t(0, 0), t(10, 6), t(20, 18), t(20, 20)],
        );
        cur.replay.as_mut().unwrap().jev = t(60, 49);
        let recal = BTreeMap::from([
            ("chat".to_owned(), Some(0.65)),
            ("cursor_task".to_owned(), None),
        ]);
        let (title, body) = render(&cur, Some(&prev), &[], None, Some(&recal));
        assert_eq!(
            title,
            "JEV routing drift, week to 2026-11-04: accuracy 0.82 (≥0.75 bucket 0.95), no drift"
        );
        assert!(body.contains("**Drift** (vs 2026-10-28): none."), "{body}");
        assert!(
            body.contains("| [0.9, 1] | 0 | 20 | 1.00 (20/20) |"),
            "{body}"
        );
        assert!(
            body.contains("`LUCINA_JEV_FIRST_INTENT_MIN=chat=0.65`"),
            "{body}"
        );
        assert!(
            body.contains("No threshold keeps it for: cursor_task"),
            "{body}"
        );
        let (title, body) = render(
            &Summary {
                replay: None,
                ..cur
            },
            None,
            &["x".into()],
            Some("no labelled set at /d, skipped"),
            None,
        );
        assert_eq!(title, "JEV routing drift, week to 2026-11-04: 100 live turns, JEV-first used 50%, no labelled replay, DRIFT (1 flag)");
        assert!(
            body.contains("**Labelled replay:** no labelled set at /d, skipped."),
            "{body}"
        );
    }
}
