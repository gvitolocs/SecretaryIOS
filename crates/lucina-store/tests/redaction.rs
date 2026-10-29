//! The stores are sinks: the timeline (file and live stream), recent turns
//! and job results never hold a known secret or a token shape.

use lucina_store::jobs::{Job, JobStore};
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{NewItem, TimelineStore, DEFAULT_MAX_BYTES};

const VALUE: &str = "store-sink-Secret-4242xyz";
const TOKEN: &str = "sk-abcdefghijklmnopqrstu12345";

fn job(id: &str) -> Job {
    serde_json::from_value(serde_json::json!({
        "id": id, "action": "cursor_task", "status": "queued", "sessionId": "s",
        "createdAt": "2026-10-22T10:00:00.000Z", "updatedAt": "2026-10-22T10:00:00.000Z"
    }))
    .unwrap()
}

#[test]
fn timeline_recent_and_jobs_are_redacted() {
    lucina_secrets::redactor().register("STORE_TEST_KEY", VALUE);
    let dir = tempfile::tempdir().unwrap();

    let tl = TimelineStore::new(dir.path().join("timeline.jsonl"), DEFAULT_MAX_BYTES, true);
    let mut live = tl.subscribe();
    let item = tl
        .append(NewItem {
            channel: "web".into(),
            role: "lucina".into(),
            text: format!("the key is {VALUE} and {TOKEN}"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        item.text,
        "the key is [secret:STORE_TEST_KEY] and [secret:token]"
    );
    let streamed = live.try_recv().unwrap();
    assert!(!format!("{streamed:?}").contains(VALUE));

    let recent = RecentChat::new(Some(dir.path().join("recent-chat.json")));
    recent.append(
        "s",
        "assistant",
        &format!("echo {VALUE}"),
        chrono::Utc::now(),
    );
    assert!(recent
        .render("s", chrono_tz::Europe::Rome)
        .contains("[secret:STORE_TEST_KEY]"));

    let jobs = JobStore::new(dir.path().join("jobs.json"));
    let j = jobs.insert(job("j1")).unwrap();
    assert_eq!(j.id, "j1");
    let done = jobs
        .update("j1", |j| {
            j.result = Some(format!("harness printed {VALUE}"));
            j.error = Some(format!("Authorization: Bearer {TOKEN}"));
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        done.result.as_deref(),
        Some("harness printed [secret:STORE_TEST_KEY]")
    );
    assert!(!done.error.unwrap().contains(TOKEN));

    for f in ["timeline.jsonl", "recent-chat.json", "jobs.json"] {
        let text = std::fs::read_to_string(dir.path().join(f)).unwrap();
        assert!(
            !text.contains(VALUE) && !text.contains(TOKEN),
            "{f}: {text}"
        );
    }
}
