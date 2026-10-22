//! `lucina parity-parsers`: the deterministic layer (tier 0, grocery, notes,
//! outfit, calendar, MCP, presence, heuristics, capability hints) on a
//! phrase corpus, compared field by field with the live Node parsers'
//! output (`tools/parity/node-parsers.mjs`, run read-only in the Node
//! container).

use chrono::{DateTime, Utc};
use lucina_intent::heuristics::{self as h, Settings};
use lucina_intent::{calendar as cal, grocery, hints, mcp, notes, outfit, presence, tier0};
use serde_json::{json, Value};

const TZ: chrono_tz::Tz = chrono_tz::Europe::Rome;

fn notes_json(n: Option<notes::NotesIntent>) -> Value {
    use notes::NotesIntent as N;
    match n {
        None => Value::Null,
        Some(N::Save {
            name,
            slug,
            title,
            content,
            more,
            lang,
        }) => {
            json!({"kind": "save", "name": name, "slug": slug, "title": title, "content": content, "more": more, "lang": lang})
        }
        Some(N::Read { name, lang }) => {
            json!({"kind": "read", "name": name, "slug": null, "title": null, "content": null, "more": null, "lang": lang})
        }
        Some(N::List { lang }) => {
            json!({"kind": "list", "name": null, "slug": null, "title": null, "content": null, "more": null, "lang": lang})
        }
        Some(N::Part {
            content,
            more,
            lang,
        }) => {
            json!({"kind": "part", "name": null, "slug": null, "title": null, "content": content, "more": more, "lang": lang})
        }
    }
}

/// The Rust results for one phrase, in the Node script's shape.
pub fn rust_case(text: &str, session: &str, now: DateTime<Utc>, s: &Settings) -> Value {
    let t0 = tier0::classify_tier0(text, "", s);
    let g = grocery::parse_grocery_intent(text);
    let cc = cal::parse_create(text, now, TZ);
    json!({
        "text": text,
        "tier0": t0.map(|c| json!({"intent": c.intent, "confidence": c.confidence, "reply_mode": c.reply_mode,
            "scope": if c.intent == "list_reminders" { json!(c.scope) } else { Value::Null }})),
        "grocery": g.map(|g| json!({
            "intent": g.intent, "rawProduct": g.raw_product, "product": g.product.map(|p| p.id),
            "store": g.store.map(|s| s.display_name), "quantity": g.quantity, "unit": g.unit,
        })),
        "notes": notes_json(notes::parse_notes_intent(text, false)),
        "notesPending": notes::parse_notes_intent(text, true).map(|n| match n {
            notes::NotesIntent::Save { .. } => "save", notes::NotesIntent::Read { .. } => "read",
            notes::NotesIntent::List { .. } => "list", notes::NotesIntent::Part { .. } => "part",
        }),
        "outfit": outfit::parse_outfit_intent(text).map(|o| json!({"outfit": o.outfit, "hair": o.hair, "lang": o.lang})),
        "calQuestion": cal::parse_question(text, now, TZ).map(|q| json!({"from": q.from, "to": q.to, "week": q.week, "lang": q.lang})),
        "calCreate": match &cc {
            None => Value::Null,
            Some(Err((e, l))) => json!({"error": e, "lang": l}),
            Some(Ok(c)) => json!({"event": c.event, "allDay": c.all_day, "lang": c.lang}),
        },
        "calConfirm": match &cc { Some(Ok(c)) => json!(cal::confirm_question(c, TZ)), _ => Value::Null },
        "mcp": mcp::turn_info(session, text).map(|m| json!({"caller": m.caller, "text": m.text})),
        "presenceQ": presence::is_presence_question(text),
        "reminderReq": h::looks_like_reminder_request(text),
        "reminderList": h::looks_like_reminder_list(text),
        "scope": h::reminder_list_scope(text),
        "complex": h::looks_like_complex(text, s),
        "recall": h::explicit_recall(text),
        "travel": h::looks_like_travel_info(text),
        "actionable": h::looks_like_actionable(text),
        "storeNote": notes::looks_like_store_note(text),
        "logInspect": tier0::looks_like_direct_log_inspect(text),
        "yesno": cal::classify_yes_no(text),
        "lang": hints::detect_language(text),
        "hints": hints::append_capability_hints("Ok, ci penso io.", text, true),
    })
}

/// Known, accepted differences: `(phrase, field, why)`. Each is a Node bug
/// the Rust port does not copy.
pub const KNOWN: &[(&str, &str, &str)] = &[(
    "sæt tandlæge i kalenderen i morgen kl. 9.30",
    "calCreate",
    "Node reads \"9.30\" as day 9 of month 30 and throws RangeError (HTTP 500); Rust asks what and when",
)];

/// Field-level differences between the Node and Rust results.
pub fn diff(node: &Value, rust: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let (Some(n), Some(r)) = (node.as_object(), rust.as_object()) else {
        return vec!["not objects".into()];
    };
    for (k, nv) in n {
        let rv = r.get(k).unwrap_or(&Value::Null);
        if !same(nv, rv) {
            out.push(format!("{k}: node={nv} rust={rv}"));
        }
    }
    out
}

/// JSON equality with numbers compared as f64 (Node prints 1 for 1.0).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            (x.as_f64().unwrap_or(f64::NAN) - y.as_f64().unwrap_or(f64::NAN)).abs() < 1e-9
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w))
        }
        _ => a == b,
    }
}

/// Run the comparison: `(cases, mismatching cases, report lines)`.
pub fn compare(input: &Value, node_out: &Value, s: &Settings) -> (usize, usize, Vec<String>) {
    let now = input["now"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let cases = input["cases"].as_array().cloned().unwrap_or_default();
    let node = node_out.as_array().cloned().unwrap_or_default();
    let mut report = Vec::new();
    let mut bad = 0;
    for (i, case) in cases.iter().enumerate() {
        let text = case["text"].as_str().unwrap_or("");
        let rust = rust_case(text, case["sessionId"].as_str().unwrap_or(""), now, s);
        let mut d = diff(node.get(i).unwrap_or(&Value::Null), &rust);
        d.retain(|line| {
            let known = KNOWN
                .iter()
                .find(|(p, f, _)| *p == text && line.starts_with(&format!("{f}:")));
            if let Some((_, f, why)) = known {
                report.push(format!("#{i} known Node bug ({f}): {why}"));
            }
            known.is_none()
        });
        if !d.is_empty() {
            bad += 1;
            report.push(format!("#{i} {text:?}"));
            for line in d {
                report.push(format!("    {line}"));
            }
        }
    }
    (cases.len(), bad, report)
}
