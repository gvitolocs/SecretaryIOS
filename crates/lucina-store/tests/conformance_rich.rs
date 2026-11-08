//! The shared rich-message vectors (`tests/conformance/rich/*.json`): the server's side. The web
//! (`web/src/lib/richConformance.test.ts`) and iOS (`RichMessageTests.swift`) run the same files, so
//! a client that drops a line break or an emoji fails against the same expectation.

use std::path::Path;

use lucina_store::rich::normalize;
use serde_json::Value;

fn vectors() -> Vec<(String, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/rich");
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let v = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            (p.file_name().unwrap().to_string_lossy().into_owned(), v)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_vector_holds_on_the_server() {
    let all = vectors();
    assert!(all.len() >= 4, "{} vectors", all.len());
    for (name, v) in all {
        let (m, _) = normalize(&v["input"]).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let got = m.to_value();
        let ex = &v["expect"];
        assert_eq!(got["bubbles"], ex["bubbles"], "{name}: bubbles");
        let kinds: Vec<&str> = got["blocks"].as_array().map(|b| b.iter().filter_map(|x| x["type"].as_str()).collect()).unwrap_or_default();
        let want: Vec<&str> = ex["blocks"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(kinds, want, "{name}: blocks");
        let ids: Vec<&str> = got["actions"].as_array().map(|a| a.iter().filter_map(|x| x["id"].as_str()).collect()).unwrap_or_default();
        let want: Vec<&str> = ex["actions"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(ids, want, "{name}: actions");
        assert_eq!(m.speech_text(), ex["speech"].as_str().unwrap(), "{name}: speech");
    }
}
