//! `ask_lucina` when she needs longer than the caller will wait: an id instead of a failure, and the answer later.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn a_slow_answer_comes_with_an_id_and_is_not_lost() {
    let h = start().await;
    let agent = h.issue("zcode", "chat");

    let (err, text) = h.call(&agent, "ask_lucina", json!({"message": "hi"})).await;
    assert!(!err && text.contains("You said:") && text.contains("hi"), "a quick answer is as before: {text}");

    let (err, text) = h
        .call(&agent, "ask_lucina", json!({"message": "answer slowly", "wait_seconds": 0}))
        .await;
    assert!(!err, "not a failure: {text}");
    assert!(text.contains("nothing is lost") && text.contains("vision"), "{text}");
    let id = text.split_whitespace().find(|w| w.starts_with("turn_")).expect("an id").to_owned();

    let (err, later) = h
        .call(&agent, "lucina_job_status", json!({"job_id": id, "wait_seconds": 5}))
        .await;
    assert!(!err && later.contains("You said:") && later.contains("answer slowly"), "{later}");

    // Another client cannot read it.
    let other = h.issue("mistral", "chat");
    let (err, text) = h.call(&other, "lucina_job_status", json!({"job_id": id})).await;
    assert!(err && text.contains("Unknown turn"), "{text}");
}
