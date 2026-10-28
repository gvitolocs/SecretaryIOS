//! The fork-context attachment against Paseo 0.13.2's own `buildAgentForkContextAttachment`
//! (parity row A16), on the cases in `fixtures/gen/gen-fork-context.mjs` (regenerate with the
//! command at its top): the text, the item count and the echoed boundary, or the error
//! message. Long texts are compared by length, FNV-1a 64 and their first and last 300
//! characters.

use lucina_agents::fork_context::{build, Boundary};
use lucina_agents::jsstr;
use lucina_agents::timeline::Cursor;
use lucina_agents::TimelineRow;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    epoch: String,
    rows: Value,
    boundary_cursor: Option<Cursor>,
    boundary_message_id: Option<String>,
    agent_title: Option<String>,
    cwd: String,
    expected: Option<Expected>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expected {
    attachment: Attachment,
    item_count: usize,
    boundary_cursor: Option<Cursor>,
    boundary_message_id: Option<String>,
    text_digest: Option<Digest>,
}

#[derive(Deserialize)]
struct Attachment {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "contextKind")]
    context_kind: String,
    title: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Digest {
    len: usize,
    fnv: String,
    head: String,
    tail: String,
}

/// `{"$cat": [literal | {"$rep": [unit, n]}]}` becomes the string it stands for.
fn expand(v: Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(expand).collect()),
        Value::Object(mut o) => {
            if let Some(Value::Array(parts)) = o.remove("$cat") {
                let mut out = String::new();
                for p in parts {
                    match p {
                        Value::String(s) => out.push_str(&s),
                        Value::Object(r) => {
                            let rep = r["$rep"].as_array().expect("$rep");
                            out.push_str(&rep[0].as_str().unwrap().repeat(rep[1].as_u64().unwrap() as usize));
                        }
                        other => panic!("bad $cat part {other}"),
                    }
                }
                return Value::String(out);
            }
            Value::Object(o.into_iter().map(|(k, x)| (k, expand(x))).collect())
        }
        other => other,
    }
}

fn fnv(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[test]
fn fork_context_matches_paseo() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/fork-context.json")).expect("fork-context fixtures parse");
    assert!(cases.len() >= 35);
    let mut failures = Vec::new();
    for case in cases {
        let rows: Vec<TimelineRow> = serde_json::from_value(expand(case.rows)).expect("rows");
        let got = build(
            &rows,
            &case.epoch,
            Boundary { cursor: case.boundary_cursor.as_ref(), message_id: case.boundary_message_id.as_deref() },
            case.agent_title.as_deref(),
            Some(&case.cwd),
        );
        match (got, case.expected, case.error) {
            (Ok(got), Some(want), None) => {
                let a = &want.attachment;
                let mut problems = Vec::new();
                if (got.attachment.kind, got.attachment.mime_type, got.attachment.context_kind, got.attachment.title)
                    != (a.kind.as_str(), a.mime_type.as_str(), a.context_kind.as_str(), a.title.as_str())
                {
                    problems.push("attachment fields".to_string());
                }
                if got.item_count != want.item_count {
                    problems.push(format!("itemCount {} != {}", got.item_count, want.item_count));
                }
                if got.boundary_cursor != want.boundary_cursor || got.boundary_message_id != want.boundary_message_id {
                    problems.push(format!("boundary {:?}/{:?} != {:?}/{:?}", got.boundary_cursor, got.boundary_message_id, want.boundary_cursor, want.boundary_message_id));
                }
                let text = &got.attachment.text;
                match (&a.text, &want.text_digest) {
                    (Some(want_text), _) if text != want_text => problems.push(format!("text\n--- paseo\n{want_text}\n--- lucina\n{text}")),
                    (_, Some(d)) => {
                        let units = jsstr::len(text);
                        if units != d.len || fnv(text) != d.fnv {
                            problems.push(format!("long text: {units} units, fnv {} (paseo {} units, fnv {})", fnv(text), d.len, d.fnv));
                        }
                        if jsstr::slice(text, 0, 300) != d.head || jsstr::slice_from(text, units.saturating_sub(300)) != d.tail {
                            problems.push("long text head or tail".to_string());
                        }
                    }
                    _ => {}
                }
                if !problems.is_empty() {
                    failures.push(format!("{}: {}", case.name, problems.join("; ")));
                }
            }
            (Err(got), None, Some(want)) => {
                if got.to_string() != want {
                    failures.push(format!("{}: error {:?} != paseo {want:?}", case.name, got.to_string()));
                }
            }
            (Ok(_), None, Some(want)) => failures.push(format!("{}: Paseo refuses ({want:?}), lucina builds it", case.name)),
            (Err(got), Some(_), None) => failures.push(format!("{}: Paseo builds it, lucina refuses ({got})", case.name)),
            _ => failures.push(format!("{}: malformed case", case.name)),
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n\n{}", failures.len(), failures.join("\n\n"));
}
