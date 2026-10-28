//! Jev's check of the free-text items the MCP (`shopping_add`) adds.
//!
//! p1 forwards MCP adds to the ops route with a top-level `"via": "mcp"`. Each
//! add that names no object goes through [`shopping_gate::check`] first: an
//! implausible item is not added and comes back as a rejected
//! `needs_confirmation` entry with Lucina's question; a bound one gets its
//! catalog id and illustration. App batches (no `via`) are never gated.

use futures_util::future::join_all;
use lucina_grocery::{is_uuid, CatalogIndex, MAX_OPS_PER_BATCH};
use lucina_jev::Jev;
use lucina_tools::shopping_gate::{self, Gate};
use serde_json::{json, Value};

/// A batch the MCP sent (`"via": "mcp"` exactly).
pub fn is_mcp(body: &Value) -> bool {
    body["via"].as_str() == Some("mcp")
}

fn trimmed(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The typed text of an add op Jev should check: a uuid `opId`, no
/// `catalogId`, no non-UUID `itemId`, and `text` (or `name`) to read.
pub fn gated_text(op: &Value) -> Option<&str> {
    if op["type"].as_str() != Some("add") || !op["opId"].as_str().is_some_and(is_uuid) {
        return None;
    }
    if !op["catalogId"].is_null() || op["itemId"].as_str().is_some_and(|i| !is_uuid(i)) {
        return None;
    }
    trimmed(op.get("text")).or_else(|| trimmed(op.get("name")))
}

/// Apply one gate per op (`None`: not checked). Asked ops leave the batch
/// and become rejected `needs_confirmation` entries; the rest keep their order.
pub fn apply_gates(ops: &[Value], gates: Vec<Option<Gate>>) -> (Vec<Value>, Vec<Value>) {
    let mut kept = Vec::with_capacity(ops.len());
    let mut rejected = Vec::new();
    for (op, gate) in ops.iter().zip(gates) {
        let mut op = op.clone();
        match gate {
            Some(Gate::Ask {
                question,
                suggestion,
            }) => rejected.push(json!({
                "opId": op["opId"],
                "error": "needs_confirmation",
                "text": gated_text(&op),
                "question": question,
                "suggestion": suggestion,
            })),
            Some(g) => {
                g.apply_to(&mut op);
                kept.push(op);
            }
            None => kept.push(op),
        }
    }
    (kept, rejected)
}

/// Check every free-text add of `ops` with Jev, concurrently. Returns the ops
/// to apply and the gate's rejected entries. Anything but an array of at most
/// [`MAX_OPS_PER_BATCH`] ops is passed through for the store to judge.
pub async fn gate_ops(
    jev: &Jev,
    idx: &CatalogIndex,
    ops: &Value,
    lang: Option<&str>,
) -> (Value, Vec<Value>) {
    let Some(arr) = ops.as_array().filter(|a| a.len() <= MAX_OPS_PER_BATCH) else {
        return (ops.clone(), Vec::new());
    };
    let texts: Vec<Option<&str>> = arr.iter().map(gated_text).collect();
    let lang = lang.unwrap_or_else(|| {
        let all: Vec<&str> = texts.iter().flatten().copied().collect();
        lucina_grocery::message_language(&all.join(", "), None)
    });
    let gates = join_all(texts.iter().map(|t| async move {
        match t {
            Some(t) => Some(shopping_gate::check(jev, idx, t, lang).await),
            None => None,
        }
    }))
    .await;
    let bound = gates
        .iter()
        .filter(|g| {
            matches!(
                g,
                Some(Gate::Add {
                    catalog_id: Some(_),
                    ..
                })
            )
        })
        .count();
    let (kept, rejected) = apply_gates(arr, gates);
    tracing::info!(target: "lucina_jev::shopping", checked = texts.iter().flatten().count(),
        asked = rejected.len(), bound, kept = kept.len(), "mcp shopping batch gated");
    (Value::Array(kept), rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_jev::{Answer, Decision};
    use std::collections::BTreeMap;

    const ASK: &str = "00000000-0000-4000-8000-000000000001";
    const BOUND: &str = "00000000-0000-4000-8000-000000000002";
    const FALLBACK: &str = "00000000-0000-4000-8000-000000000003";
    const PURCHASE: &str = "00000000-0000-4000-8000-000000000004";
    const EXPLICIT: &str = "00000000-0000-4000-8000-000000000005";

    fn idx() -> CatalogIndex {
        CatalogIndex::build(&json!({
            "sections": [
                {"id": "household", "names": {"it": "Casa", "en": "Household"}},
                {"id": "dairy", "names": {"it": "Latticini", "en": "Dairy"}}
            ],
            "items": [
                {"id": "laundry-detergent", "names": {"it": "Detersivo per lavatrice", "en": "Laundry detergent"}, "section": "household"},
                {"id": "milk", "names": {"it": "Latte", "en": "Milk"}, "section": "dairy"}
            ]
        }))
    }

    fn ask_lavatrici() -> Gate {
        let mut a = BTreeMap::new();
        a.insert("plausible".into(), Answer::Noul(0.05));
        a.insert(
            "item_household".into(),
            Answer::Choice {
                choice: "laundry-detergent".into(),
                confidence: 0.6,
                probabilities: BTreeMap::new(),
            },
        );
        let d = Decision {
            answers: a,
            ..Decision::default()
        };
        shopping_gate::decide(&idx(), &d, "lavatrici", "it")
    }

    fn add(op_id: &str, name: &str) -> Value {
        json!({"opId": op_id, "type": "add", "name": name})
    }

    #[test]
    fn only_free_text_adds_with_a_uuid_op_id_are_checked() {
        assert_eq!(gated_text(&add(ASK, " lavatrici ")), Some("lavatrici"));
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "add", "text": "latte", "name": "x"})),
            Some("latte")
        );
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "add", "text": "  ", "name": "latte"})),
            Some("latte")
        );
        assert_eq!(gated_text(&add("not-a-uuid", "latte")), None);
        assert_eq!(gated_text(&add(ASK, "   ")), None);
        assert_eq!(gated_text(&json!({"opId": ASK, "type": "add"})), None);
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "add", "name": "latte", "catalogId": "milk"})),
            None
        );
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "add", "name": "latte", "itemId": "milk"})),
            None
        );
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "add", "name": "latte", "itemId": BOUND})),
            Some("latte"),
            "a uuid itemId is the client's own row id"
        );
        assert_eq!(
            gated_text(&json!({"opId": ASK, "type": "purchase", "name": "latte"})),
            None
        );
    }

    #[test]
    fn asked_adds_leave_the_batch_and_bound_ones_get_their_catalog_item() {
        let ops = vec![
            json!({"opId": ASK, "type": "add", "name": "lavatrici", "quantity": 1}),
            json!({"opId": BOUND, "type": "add", "name": "latte", "spec": "intero", "quantity": 2, "unit": "l"}),
            add(FALLBACK, "zzz"),
            json!({"opId": PURCHASE, "type": "purchase", "itemId": BOUND}),
            json!({"opId": EXPLICIT, "type": "add", "name": "Latte", "catalogId": "milk"}),
        ];
        let texts: Vec<Option<&str>> = ops.iter().map(gated_text).collect();
        assert_eq!(
            texts,
            [Some("lavatrici"), Some("latte"), Some("zzz"), None, None]
        );
        let gates = vec![
            Some(ask_lavatrici()),
            Some(Gate::Add {
                catalog_id: idx().find("latte").map(|i| i.id.clone()),
                section: None,
                by: "catalog",
            }),
            Some(Gate::Add {
                catalog_id: None,
                section: None,
                by: "fallback",
            }),
            None,
            None,
        ];
        let (kept, rejected) = apply_gates(&ops, gates);

        assert_eq!(rejected.len(), 1);
        let r = &rejected[0];
        assert_eq!(r["opId"], ASK);
        assert_eq!(r["error"], "needs_confirmation");
        assert_eq!(r["text"], "lavatrici");
        assert_eq!(r["suggestion"], "Detersivo per lavatrice");
        let q = r["question"].as_str().unwrap();
        assert!(q.contains("Intendevi «Detersivo per lavatrice»?"), "{q}");

        let ids: Vec<&str> = kept.iter().map(|o| o["opId"].as_str().unwrap()).collect();
        assert_eq!(ids, [BOUND, FALLBACK, PURCHASE, EXPLICIT]);
        assert_eq!(
            kept[0],
            json!({"opId": BOUND, "type": "add", "name": "latte", "spec": "intero", "quantity": 2, "unit": "l",
                   "catalogId": "milk", "icon": "svg:milk"})
        );
        assert_eq!(kept[1], ops[2]);
        assert_eq!(kept[2], ops[3]);
        assert_eq!(kept[3], ops[4]);
    }

    #[test]
    fn a_section_only_add_sets_the_section_when_the_op_has_none() {
        let g = || {
            Some(Gate::Add {
                catalog_id: None,
                section: Some("household".into()),
                by: "jev",
            })
        };
        let ops = vec![
            add(BOUND, "panni"),
            json!({"opId": FALLBACK, "type": "add", "name": "spugnette", "section": "dairy"}),
        ];
        let (kept, rejected) = apply_gates(&ops, vec![g(), g()]);
        assert!(rejected.is_empty());
        assert_eq!(kept[0]["section"], "household");
        assert!(kept[0].get("catalogId").is_none());
        assert_eq!(kept[1]["section"], "dairy");
    }

    #[test]
    fn only_batches_via_the_mcp_are_gated() {
        assert!(is_mcp(&json!({"via": "mcp", "ops": []})));
        assert!(!is_mcp(&json!({"ops": []})));
        assert!(!is_mcp(&json!({"via": "app", "ops": []})));
        assert!(!is_mcp(&json!({"via": "MCP", "ops": []})));
        assert!(!is_mcp(&json!({"via": true, "ops": []})));
    }

    #[tokio::test]
    async fn with_jev_unreachable_the_batch_is_kept_and_exact_names_are_bound() {
        let off = Jev::new(lucina_jev::Config {
            url: "http://127.0.0.1:9/v1/systemone".into(),
            model: "jev-1.14.0".into(),
            key: None,
            timeout: std::time::Duration::from_secs(1),
            enabled: true,
        });
        let ops = json!([add(BOUND, "Latte"), add(FALLBACK, "lavatrici"), {"opId": PURCHASE, "type": "purchase", "itemId": BOUND}]);
        let (kept, rejected) = gate_ops(&off, &idx(), &ops, None).await;
        assert!(rejected.is_empty());
        let kept = kept.as_array().unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0]["catalogId"], "milk");
        assert_eq!(kept[0]["icon"], "svg:milk");
        assert_eq!(kept[1], ops[1]);
        assert_eq!(kept[2], ops[2]);

        let (passed, rejected) = gate_ops(&off, &idx(), &json!("nope"), Some("it")).await;
        assert_eq!(passed, json!("nope"));
        assert!(rejected.is_empty());
    }
}
