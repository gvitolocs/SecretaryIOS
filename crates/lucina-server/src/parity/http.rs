//! `lucina parity`: the same `/api/secretary/*` requests against a Node
//! reference instance and a Rust instance (each on its own copy of the data,
//! its own fake DeepSeek and its own fake secretary-server namespace), then
//! a structural comparison:
//!
//! - status code and the JSON body with volatile values masked (ids, ULIDs,
//!   UUIDs; timestamps cut to the minute);
//! - the prompt kinds each runtime sent to its fake DeepSeek for the step;
//! - the calls each runtime made to secretary-server (notes, calendar,
//!   avatar) for the step;
//! - SSE steps: the framing and the item events after a chat turn;
//! - at the end, the timeline lines each runtime appended.
//!
//! A step may list `allow` paths (`body.data.reply`, `deepseek`, …) whose
//! difference is expected and explained by `why`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

pub struct Side {
    pub name: &'static str,
    /// Values captured from this side's responses (`{name}` in paths/bodies).
    pub vars: std::sync::Mutex<std::collections::HashMap<String, String>>,
    pub base: String,
    pub token: String,
    pub deepseek_record: PathBuf,
    pub secretary_space: String,
    pub timeline: PathBuf,
}

pub struct Run {
    pub node: Side,
    pub rust: Side,
    pub secretary_record: PathBuf,
}

fn is_ulid(s: &str) -> bool {
    lucina_store::ulid::is_timeline_id(s)
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// ISO timestamps to the minute (`2026-10-22T16:30Z~`).
fn minute(s: &str) -> Option<String> {
    let d = lucina_store::timeutil::parse_js_date(s)?;
    let looks_iso = s.len() >= 16 && s.as_bytes().get(10) == Some(&b'T');
    looks_iso.then(|| format!("{}Z~", d.format("%Y-%m-%dT%H:%M")))
}

const VOLATILE_KEYS: &[&str] = &[
    "now",
    "ts",
    "generatedAt",
    "briefingGeneratedAt",
    "uptimeSeconds",
    "ageSeconds",
    "age_seconds",
    "createdAt",
    "updatedAt",
    "startedAt",
    "finishedAt",
    "receivedAt",
];

/// Mask volatile values for comparison.
pub fn mask(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, x) in m {
                let y = if VOLATILE_KEYS.contains(&k.as_str()) && !x.is_null() {
                    json!("<volatile>")
                } else {
                    mask(x)
                };
                out.insert(k.clone(), y);
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(mask).collect()),
        Value::String(s) if is_ulid(s) => json!("<ulid>"),
        Value::String(s) if is_uuid(s) => json!("<uuid>"),
        Value::String(s) => minute(s).map(Value::String).unwrap_or_else(|| v.clone()),
        _ => v.clone(),
    }
}

/// Paths (`a.b.0.c`) where two masked values differ.
pub fn diff_paths(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                diff_paths(
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    &p,
                    out,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                diff_paths(p, q, &format!("{path}.{i}"), out);
            }
        }
        (Value::Number(x), Value::Number(y))
            if (x.as_f64().unwrap_or(0.0) - y.as_f64().unwrap_or(0.0)).abs() < 1e-9 => {}
        _ if a == b => {}
        _ => out.push(path.to_owned()),
    }
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn deepseek_kinds(path: &Path, from: usize) -> (Vec<String>, usize) {
    let all = lines(path);
    let kinds = all
        .iter()
        .skip(from)
        .map(|l| l["kind"].as_str().unwrap_or("?").to_owned())
        .collect();
    (kinds, all.len())
}

fn secretary_calls(path: &Path, space: &str, from: usize) -> (Vec<String>, usize) {
    let all = lines(path);
    let calls = all
        .iter()
        .skip(from)
        .filter(|l| l["space"] == space)
        .map(|l| {
            format!(
                "{} {} {}",
                l["method"].as_str().unwrap_or(""),
                l["path"].as_str().unwrap_or(""),
                l["status"]
            )
        })
        .collect();
    (calls, all.len())
}

/// `{name}` placeholders from the side's captured values.
fn subst(side: &Side, v: &Value) -> Value {
    let vars = side.vars.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (k, val) in &vars {
                out = out.replace(&format!("{{{k}}}"), val);
            }
            Value::String(out)
        }
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), subst(side, x))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| subst(side, x)).collect()),
        other => other.clone(),
    }
}

fn get_path<'a>(v: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(v, |cur, k| match k.parse::<usize>() {
        Ok(i) if cur.is_array() => &cur[i],
        _ => &cur[k],
    })
}

fn capture(side: &Side, step: &Value, raw: &Value) {
    if let Some(m) = step["capture"].as_object() {
        let mut vars = side.vars.lock().unwrap_or_else(|e| e.into_inner());
        for (name, path) in m {
            if let Some(v) = get_path(raw, path.as_str().unwrap_or("")).as_str() {
                vars.insert(name.clone(), v.to_owned());
            }
        }
    }
}

async fn call(http: &reqwest::Client, side: &Side, step: &Value) -> (u16, Value) {
    let step = &subst(side, step);
    let method = reqwest::Method::from_bytes(step["method"].as_str().unwrap_or("GET").as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let path = step["path"].as_str().unwrap_or("/health");
    let mut rb = http
        .request(method, format!("{}/api/secretary{path}", side.base))
        .timeout(Duration::from_secs(90));
    if step["auth"] != false {
        rb = rb.bearer_auth(&side.token);
    }
    if let Some(raw) = step["raw"].as_str() {
        rb = rb
            .header("content-type", "application/json")
            .body(raw.to_owned());
    } else if !step["body"].is_null() {
        rb = rb.json(&step["body"]);
    }
    match rb.send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let allow = r
                .headers()
                .get("allow")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let cache = r
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let text = r.text().await.unwrap_or_default();
            let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
            let raw = json!({"body": body, "allow": allow, "cacheControl": cache});
            capture(side, step, &raw);
            (status, raw)
        }
        Err(e) => (0, json!({"error": e.to_string()})),
    }
}

/// SSE: connect, run `trigger` (a chat body) on the same side, read events
/// for `seconds`, and return the framing plus the item events (masked).
async fn sse(http: &reqwest::Client, side: &Side, step: &Value) -> Value {
    use futures_util::StreamExt;
    let step = &subst(side, step);
    let mut rb = http
        .get(format!("{}/api/secretary/timeline/stream", side.base))
        .bearer_auth(&side.token);
    if let Some(last) = step["lastEventId"].as_str() {
        rb = rb.header("last-event-id", last);
    }
    let Ok(resp) = rb.send().await else {
        return json!({"error": "connect failed"});
    };
    let status = resp.status().as_u16();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let trigger = step["trigger"].clone();
    let http2 = http.clone();
    let base = side.base.clone();
    let token = side.token.clone();
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if !trigger.is_null() {
            let _ = http2
                .post(format!("{base}/api/secretary/chat"))
                .bearer_auth(token)
                .json(&trigger)
                .send()
                .await;
        }
    });
    let until =
        tokio::time::Instant::now() + Duration::from_secs(step["seconds"].as_u64().unwrap_or(6));
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(until, stream.next()).await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
    let _ = task.await;
    let preamble = buf.split("\n\n").next().unwrap_or("").to_owned();
    let items: Vec<Value> = buf
        .split("\n\n")
        .filter(|ev| ev.starts_with("event: item"))
        .map(|ev| {
            let data = ev
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .unwrap_or("null");
            let has_id = ev.lines().any(|l| l.starts_with("id: "));
            let item: Value = serde_json::from_str(data).unwrap_or(Value::Null);
            json!({"hasId": has_id, "item": mask(&item)})
        })
        .collect();
    json!({"status": status, "contentType": ctype, "preamble": preamble, "items": items})
}

/// `a` (segments, `*` = any one segment) equals `path` or is a prefix of it.
fn path_matches(a: &str, path: &str) -> bool {
    let (want, have): (Vec<&str>, Vec<&str>) = (a.split('.').collect(), path.split('.').collect());
    want.len() <= have.len() && want.iter().zip(&have).all(|(w, h)| *w == "*" || w == h)
}

fn allowed(step: &Value, path: &str) -> bool {
    step["allow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|a| path_matches(a, path))
}

/// A `…text` difference where both sides' texts start with one of the
/// `textAllow` prefixes (expected divergences that resurface in timeline
/// listings and SSE replays).
fn text_allowed(cases: &Value, node: &Value, rust: &Value, path: &str) -> bool {
    if !path.ends_with(".text") {
        return false;
    }
    let prefixes: Vec<&str> = cases["textAllow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let (n, r) = (
        get_path(node, path).as_str().unwrap_or(""),
        get_path(rust, path).as_str().unwrap_or(""),
    );
    let ok = |t: &str| prefixes.iter().any(|p| t.starts_with(p));
    ok(n) && ok(r)
}

/// Run every step; returns the report and whether all steps matched.
pub async fn run(cases: &Value, r: &Run) -> (Value, bool) {
    let http = reqwest::Client::new();
    let mut results = Vec::new();
    let mut all_ok = true;
    let (mut nd, mut rd) = (
        lines(&r.node.deepseek_record).len(),
        lines(&r.rust.deepseek_record).len(),
    );
    let mut sc = lines(&r.secretary_record).len();
    let nt0 = lines(&r.node.timeline).len();
    let rt0 = lines(&r.rust.timeline).len();
    for step in cases["steps"].as_array().into_iter().flatten() {
        let name = step["name"].as_str().unwrap_or("?").to_owned();
        let (node, rust) = if step["kind"] == "sse" {
            (
                json!({"sse": sse(&http, &r.node, step).await}),
                json!({"sse": sse(&http, &r.rust, step).await}),
            )
        } else {
            let (ns, nb) = call(&http, &r.node, step).await;
            let (rs, rb) = call(&http, &r.rust, step).await;
            (
                json!({"status": ns, "response": mask(&nb)}),
                json!({"status": rs, "response": mask(&rb)}),
            )
        };
        if let Some(ms) = step["waitMs"].as_u64() {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let (nk, n2) = deepseek_kinds(&r.node.deepseek_record, nd);
        let (rk, r2) = deepseek_kinds(&r.rust.deepseek_record, rd);
        let (nsec, _) = secretary_calls(&r.secretary_record, &r.node.secretary_space, sc);
        let (rsec, s2) = secretary_calls(&r.secretary_record, &r.rust.secretary_space, sc);
        (nd, rd, sc) = (n2, r2, s2);
        let mut paths = Vec::new();
        diff_paths(&node, &rust, "", &mut paths);
        if nk != rk {
            paths.push("deepseek".into());
        }
        if nsec != rsec {
            paths.push("secretary".into());
        }
        let unexpected: Vec<String> = paths
            .iter()
            .filter(|p| !allowed(step, p) && !text_allowed(cases, &node, &rust, p))
            .cloned()
            .collect();
        let ok = unexpected.is_empty();
        all_ok &= ok;
        results.push(json!({
            "name": name, "ok": ok, "unexpected": unexpected, "expectedDiffs": paths.iter().filter(|p| allowed(step, p)).collect::<Vec<_>>(),
            "why": step["why"], "node": node, "rust": rust,
            "deepseek": {"node": nk, "rust": rk}, "secretary": {"node": nsec, "rust": rsec},
        }));
    }
    // Timeline lines appended by each runtime.
    let shape = |l: &Value| {
        let mut v = mask(l);
        if let Some(m) = v.as_object_mut() {
            m.remove("id");
            m.remove("ts");
            if m.contains_key("replyTo") {
                m.insert("replyTo".into(), json!("<set>"));
            }
        }
        v
    };
    let nt: Vec<Value> = lines(&r.node.timeline)
        .iter()
        .skip(nt0)
        .map(shape)
        .collect();
    let rt: Vec<Value> = lines(&r.rust.timeline)
        .iter()
        .skip(rt0)
        .map(shape)
        .collect();
    let mut tdiff = Vec::new();
    diff_paths(&json!(nt), &json!(rt), "timeline", &mut tdiff);
    let allowed_tl: Vec<String> = cases["timelineAllow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let (tn, tr) = (json!({"timeline": nt}), json!({"timeline": rt}));
    let t_unexpected: Vec<&String> = tdiff
        .iter()
        .filter(|p| {
            !allowed_tl.iter().any(|a| p.starts_with(a.as_str()))
                && !text_allowed(cases, &tn, &tr, p)
        })
        .collect();
    if nt.len() != rt.len() || !t_unexpected.is_empty() {
        all_ok = false;
    }
    let report = json!({
        "steps": results,
        "timeline": {"node": nt.len(), "rust": rt.len(), "diffs": tdiff, "unexpected": t_unexpected, "nodeLines": nt, "rustLines": rt},
        "ok": all_ok,
    });
    (report, all_ok)
}

/// One line per step for the console.
pub fn summary(report: &Value) -> String {
    let mut out = Vec::new();
    for s in report["steps"].as_array().into_iter().flatten() {
        let mark = if s["ok"] == true { "ok  " } else { "DIFF" };
        let exp = s["expectedDiffs"].as_array().map(|a| a.len()).unwrap_or(0);
        let mut line = format!("{mark} {}", s["name"].as_str().unwrap_or(""));
        if exp > 0 {
            line.push_str(&format!("  (expected: {})", s["expectedDiffs"]));
        }
        if s["ok"] != true {
            line.push_str(&format!("  unexpected: {}", s["unexpected"]));
        }
        out.push(line);
    }
    let t = &report["timeline"];
    out.push(format!(
        "timeline: node {} lines, rust {} lines, unexpected diffs {}",
        t["node"], t["rust"], t["unexpected"]
    ));
    out.join("\n")
}
