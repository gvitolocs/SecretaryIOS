//! The shared cases (tests/fixtures/shopping-split-cases.json), also run by the web and iOS twins.

use shopping_text::{fold_text, split_items, ItemPart};

#[test]
fn shared_cases() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/shopping-split-cases.json"))
        .expect("fixture");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("fixture json");
    let known: Vec<String> = doc["known"].as_array().expect("known").iter().map(|k| fold_text(k.as_str().unwrap_or_default())).collect();
    let is_known = |s: &str| known.contains(&fold_text(s));
    let cases = doc["cases"].as_array().expect("cases");
    assert!(cases.len() >= 40);
    for case in cases {
        let input = case["in"].as_str().expect("in");
        let want: Vec<ItemPart> = case["out"]
            .as_array()
            .expect("out")
            .iter()
            .map(|p| ItemPart {
                name: p["name"].as_str().expect("name").to_owned(),
                quantity: p["quantity"].as_u64().map(|q| q as u32),
                unit: p["unit"].as_str().map(str::to_owned),
            })
            .collect();
        assert_eq!(split_items(input, is_known), want, "case {input:?}");
    }
}
