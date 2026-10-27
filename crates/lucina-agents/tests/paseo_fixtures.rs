//! lucina-agents against Paseo 0.13.2's own code: the activity curator, titles and
//! state buckets on the cases in `fixtures/gen/gen.mjs`, whose expected outputs
//! were produced by running Paseo's `activity-curator.js`, `create-agent-title.js`
//! and `agent-state-bucket.js` (regenerate with the command at the top of gen.mjs).

use lucina_agents::bucket::{state_bucket, status_priority, Bucket, BucketInput};
use lucina_agents::curator::{curate_activity, summarize_activity, CurateOptions};
use lucina_agents::title::resolve_titles;
use lucina_agents::{AgentStatus, AttentionReason, TimelineItem};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CuratorCase {
    name: String,
    limit: Option<usize>,
    items: Vec<TimelineItem>,
    options: Option<Options>,
    expected: Option<Expected>,
    expected_curate: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    label_assistant_messages: bool,
    include_kinds: Vec<String>,
    include_external_tool_input: bool,
    max_items: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expected {
    update_count: usize,
    content: String,
}

fn kind(name: &str) -> &'static str {
    match name {
        "user_message" => "user_message",
        "assistant_message" => "assistant_message",
        "tool_call" => "tool_call",
        other => panic!("unexpected kind {other}"),
    }
}

#[test]
fn curator_matches_paseo() {
    let cases: Vec<CuratorCase> =
        serde_json::from_str(include_str!("fixtures/curator.json")).expect("curator fixtures parse");
    assert!(cases.len() >= 60);
    let mut failures = Vec::new();
    for case in &cases {
        if let Some(expected) = &case.expected {
            let got = summarize_activity(&case.items, case.limit);
            if got.content != expected.content || got.update_count != expected.update_count {
                failures.push(format!(
                    "{} (limit {:?})\n--- paseo\n{}\n--- lucina\n{}",
                    case.name, case.limit, expected.content, got.content
                ));
            }
        }
        if let (Some(opts), Some(expected)) = (&case.options, &case.expected_curate) {
            let options = CurateOptions {
                max_items: opts.max_items,
                label_assistant_messages: opts.label_assistant_messages,
                include_kinds: Some(opts.include_kinds.iter().map(|k| kind(k)).collect()),
                include_external_tool_input: Some(opts.include_external_tool_input),
            };
            let got = curate_activity(&case.items, &options);
            if &got != expected {
                failures.push(format!("{}\n--- paseo\n{expected}\n--- lucina\n{got}", case.name));
            }
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n\n{}", failures.len(), failures.join("\n\n"));
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TitleCase {
    config_title: Option<String>,
    initial_prompt: Option<String>,
    expected: TitleExpected,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TitleExpected {
    explicit_title: Option<String>,
    provisional_title: Option<String>,
}

#[test]
fn titles_match_paseo() {
    let cases: Vec<TitleCase> = serde_json::from_str(include_str!("fixtures/titles.json")).unwrap();
    for case in cases {
        let (explicit, provisional) =
            resolve_titles(case.config_title.as_deref(), case.initial_prompt.as_deref());
        assert_eq!(explicit, case.expected.explicit_title, "{:?}", case.initial_prompt);
        assert_eq!(provisional, case.expected.provisional_title, "{:?}", case.initial_prompt);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BucketCase {
    input: BucketCaseInput,
    bucket: Bucket,
    priority: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BucketCaseInput {
    status: AgentStatus,
    pending_permission_count: usize,
    requires_attention: bool,
    attention_reason: Option<AttentionReason>,
}

#[test]
fn buckets_match_paseo() {
    let cases: Vec<BucketCase> = serde_json::from_str(include_str!("fixtures/buckets.json")).unwrap();
    assert_eq!(cases.len(), 80);
    for case in cases {
        let input = BucketInput {
            status: case.input.status,
            pending_permission_count: case.input.pending_permission_count,
            requires_attention: case.input.requires_attention,
            attention_reason: case.input.attention_reason,
        };
        assert_eq!(state_bucket(input), case.bucket, "{input:?}");
        assert_eq!(status_priority(input), case.priority, "{input:?}");
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttentionCase {
    reason: AttentionReason,
    assistant_message: Option<String>,
    permission_request: Option<lucina_agents::PermissionRequest>,
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct LatestCase {
    items: Vec<TimelineItem>,
    expected: Option<String>,
}

#[derive(Deserialize)]
struct AttentionFixtures {
    payloads: Vec<AttentionCase>,
    latest: Vec<LatestCase>,
}

#[test]
fn attention_notifications_match_paseo() {
    use lucina_agents::notification::{body, latest_assistant_message, title};
    let f: AttentionFixtures = serde_json::from_str(include_str!("fixtures/attention.json")).expect("attention fixtures parse");
    assert!(f.payloads.len() >= 30);
    let mut failures = Vec::new();
    for c in &f.payloads {
        let got = (title(c.reason).to_string(), body(c.reason, c.assistant_message.as_deref(), c.permission_request.as_ref()));
        if got != (c.title.clone(), c.body.clone()) {
            failures.push(format!("{:?} {:?} {:?}\n--- paseo\n{}\n--- lucina\n{}", c.reason, c.assistant_message, c.permission_request.as_ref().map(|p| &p.id), c.body, got.1));
        }
    }
    for c in &f.latest {
        if latest_assistant_message(&c.items) != c.expected {
            failures.push(format!("latest of {:?}", c.items));
        }
    }
    assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), f.payloads.len() + f.latest.len(), failures.join("\n\n"));
}
