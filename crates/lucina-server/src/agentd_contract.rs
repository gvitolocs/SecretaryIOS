//! The contract between what Lucina sends agentd and what agentd reads: every `POST /agents` body
//! Lucina builds, deserialised with agentd's own request type, and the two answers Lucina reads by
//! key, serialised from agentd's own response types.
//!
//! LA-C049: the owner's "Spegni raffy server" (2026-11-22 10:39) died on
//! `agentd 422: … invalid type: boolean `true`, expected a string` — Lucina had sent `"act": true`
//! as a label value while agentd reads every label as a string, and no test had ever put the two
//! sides next to each other: [`crate::act`] tested against a stand-in that never serialises.
//!
//! So: the builders here are the ones the code uses, the type here is the one agentd uses, and the
//! answer tests go the other way. A rename or a retype on either side breaks this before it breaks
//! a run.

use crate::{act, config::ActConfig, flaws};
use lucina_agentd::manager::{CreateRequest, WaitResult};
use lucina_agents::types::{AgentSnapshot, AgentStatus, Capabilities};
use lucina_flaws::{Platform, Policy, Step};
use lucina_tools::tools::reach;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// agentd's real request type, read off the body Lucina really built.
fn read_by_agentd(who: &str, body: &Value) -> CreateRequest {
    match serde_json::from_value::<CreateRequest>(body.clone()) {
        Ok(req) => req,
        Err(e) => panic!(
            "{who}: agentd's CreateRequest refuses the body Lucina sent ({e})\nbody: {body}"
        ),
    }
}

/// The labels agentd read are the ones Lucina sent: equal in every key and every value, and with
/// nothing agentd cannot read as a string.
fn same_labels(who: &str, body: &Value, req: &CreateRequest) {
    assert_eq!(
        serde_json::to_value(&req.labels).unwrap(),
        body["labels"],
        "{who}: the labels agentd read are not the ones Lucina sent"
    );
}

/// The act settings: her own, every key set, and a provider that sets neither effort nor mode.
fn act_configs() -> Vec<(&'static str, ActConfig)> {
    let own = ActConfig::from_lookup(&|_| None);
    let all = ActConfig {
        effort: Some("high".into()),
        mode: Some("plan".into()),
        ..own.clone()
    };
    let neither = ActConfig {
        provider: "claude".into(),
        model: "claude-sonnet-5-5".into(),
        effort: None,
        mode: None,
        ..own.clone()
    };
    vec![
        ("her own settings", own),
        ("every key set", all),
        ("a provider with neither effort nor mode", neither),
    ]
}

#[test]
fn every_act_body_is_one_agentd_reads() {
    for (what, cfg) in act_configs() {
        let body = act::agentd_body(
            &cfg,
            "/home/nez/.local/share/lucina/act/job-1",
            "Spegni raffy server",
            "Lucina act: Spegni raffy server",
            act::labels(),
        );
        let req = read_by_agentd(what, &body);
        same_labels(what, &body, &req);
        assert_eq!(req.provider, cfg.provider, "{what}: the provider");
        assert_eq!(
            req.model.as_deref(),
            Some(cfg.model.as_str()),
            "{what}: the model"
        );
        assert_eq!(
            req.cwd,
            "/home/nez/.local/share/lucina/act/job-1",
            "{what}: the working directory"
        );
        assert_eq!(
            req.prompt.as_deref(),
            Some("Spegni raffy server"),
            "{what}: the prompt"
        );
        assert_eq!(
            req.title.as_deref(),
            Some("Lucina act: Spegni raffy server"),
            "{what}: the title"
        );
        assert_eq!(
            req.thinking_option_id, cfg.effort,
            "{what}: thinkingOptionId"
        );
        assert_eq!(req.mode_id, cfg.mode, "{what}: modeId");
        assert!(req.env.is_empty(), "{what}: act sends no env");
    }
}

#[test]
fn the_dispatchers_agent_body_is_one_agentd_reads() {
    let policy = Policy::from_lookup(|_| None, Path::new("/home/nez")).unwrap();
    let step = Step::new(
        "opencode",
        Some("opencode/deepseek-v4.1-flash-free"),
        Some("routine"),
    );
    let no_model = Step::new("claude", None, None);
    let cases = [
        (
            "a fix",
            flaws::fix_labels("fl-1", Platform::Web, "routine"),
            &step,
        ),
        ("a triage", flaws::triage_labels("fl-1", "free"), &step),
        (
            "a step with no model",
            flaws::fix_labels("fl-1", Platform::Mac, "deep"),
            &no_model,
        ),
    ];
    for (what, labels, s) in cases {
        for key in [Some("flaw:fl-1:web:1"), None] {
            let body = flaws::agent_body(
                &policy,
                s,
                "/home/nez/Projects/.worktrees/flaw-1-web-1",
                "<owner_words> emoji are boxes",
                "Flaw fl-1 on web: emoji are boxes",
                labels.clone(),
                key,
            );
            let req = read_by_agentd(what, &body);
            same_labels(what, &body, &req);
            assert_eq!(req.provider, s.provider, "{what}: the provider");
            assert_eq!(req.model, s.model, "{what}: the model");
            assert_eq!(
                req.thinking_option_id.as_deref(),
                Some(policy.effort.as_str()),
                "{what}: thinkingOptionId is the policy's effort"
            );
            assert_eq!(
                req.mode_id.as_deref(),
                Some(policy.mode.as_str()),
                "{what}: modeId is the policy's mode"
            );
            assert_eq!(
                req.prompt.as_deref(),
                Some("<owner_words> emoji are boxes"),
                "{what}: the prompt"
            );
            assert_eq!(
                req.idempotency_key,
                key.map(str::to_owned),
                "{what}: idempotencyKey"
            );
        }
    }
    // The dispatcher looks a run up by these keys, so agentd has to hand them back exactly.
    let body = flaws::agent_body(
        &policy,
        &step,
        "/w",
        "<owner_words>",
        "t",
        flaws::fix_labels("fl-1", Platform::Web, "routine"),
        None,
    );
    let req = read_by_agentd("a fix", &body);
    assert_eq!(req.labels["flaw"], "fl-1");
    assert_eq!(req.labels["platform"], "web");
    assert_eq!(req.labels["flaw_lane"], "routine");
    assert_eq!(req.labels["lucina.handoff-off"], "1");
    assert_eq!(req.labels["from"], "lucina");
    let triage_body = flaws::agent_body(
        &policy,
        &step,
        "/w",
        "<owner_words>",
        "t",
        flaws::triage_labels("fl-1", "free"),
        None,
    );
    let triage = read_by_agentd("a triage", &triage_body);
    assert_eq!(triage.labels["triage"], "1");
    assert!(
        triage.labels.get("platform").is_none(),
        "a triage names no platform"
    );
}

#[test]
fn every_reach_body_is_one_agentd_reads() {
    let mistral = vec![
        (
            "a Mistral run with no model set",
            reach::mistral_agent_body(
                "opencode",
                None,
                "/w/mistral/rt-1",
                "the report",
                "the subject",
                "rt-1",
            ),
        ),
        (
            "a Mistral run with its model set",
            reach::mistral_agent_body(
                "opencode",
                Some("opencode/deepseek-v4.1-flash-free"),
                "/w/mistral/rt-1",
                "the report",
                "the subject",
                "rt-1",
            ),
        ),
    ];
    let claude = [(
        "a Claude agent",
        reach::claude_agent_body(
            "claude",
            "claude-sonnet-5-5",
            "high",
            "bypassPermissions",
            "/w/claude/rt-2",
            "the message",
            "the subject",
            "rt-2",
        ),
    )];
    let agents = vec![
        (
            "a lucina-rs reach agents run",
            reach::agents_run_body(
                "opencode",
                None,
                "/w/agents",
                "the envelope",
                "the title",
                "rt-3",
            ),
        ),
        (
            "the same with a model set",
            reach::agents_run_body(
                "opencode",
                Some("opencode/deepseek-v4.1-flash-free"),
                "/w/agents",
                "the envelope",
                "the title",
                "rt-3",
            ),
        ),
    ];
    for (what, body) in mistral.into_iter().chain(claude).chain(agents) {
        let req = read_by_agentd(what, &body);
        same_labels(what, &body, &req);
        assert_eq!(
            req.labels["reach"],
            body["labels"]["reach"],
            "{what}: the thread label"
        );
        assert_eq!(req.labels["from"], "lucina", "{what}: who started it");
        assert_eq!(req.cwd, body["cwd"], "{what}: the working directory");
        assert_eq!(
            req.prompt.as_deref().unwrap(),
            body["prompt"].as_str().unwrap(),
            "{what}: the prompt"
        );
        assert_eq!(
            req.title.as_deref().unwrap(),
            body["title"].as_str().unwrap(),
            "{what}: the title"
        );
    }
    // `model` is sent only when the setting has one; a paid DeepSeek or Codex never gets here.
    let no_model = reach::mistral_agent_body("opencode", None, "/w", "p", "s", "rt-1");
    assert!(
        no_model.get("model").is_none(),
        "no model set, so the key is not sent"
    );
    assert_eq!(
        read_by_agentd("a Mistral run", &no_model).model,
        None,
        "and agentd reads no model"
    );
    let with_model = reach::agents_run_body("opencode", Some("m1"), "/w", "p", "s", "rt-1");
    assert_eq!(
        read_by_agentd("an agents run", &with_model)
            .model
            .as_deref(),
        Some("m1"),
        "the model the setting names"
    );
}

/// The incident itself, as agentd answered it: a label value that is not a string.
#[test]
fn a_boolean_label_is_refused_the_way_it_refused_the_owners_request() {
    let body = json!({
        "provider": "opencode",
        "model": "opencode/deepseek-v4.1-flash-free",
        "cwd": "/w",
        "prompt": "Spegni raffy server",
        "title": "Lucina act: Spegni raffy server",
        "labels": {"from": "lucina", "act": true},
    });
    let e = serde_json::from_value::<CreateRequest>(body.clone())
        .expect_err("agentd reads every label as a string: a boolean is not one (LA-C049)");
    assert!(
        e.to_string().contains("expected a string"),
        "the 422 the owner got said `expected a string`, this says {e}"
    );
    // The same body with the label as text is the run agentd starts.
    let fixed = json!({
        "provider": "opencode",
        "model": "opencode/deepseek-v4.1-flash-free",
        "cwd": "/w",
        "prompt": "Spegni raffy server",
        "title": "Lucina act: Spegni raffy server",
        "labels": {"from": "lucina", "act": "true"},
    });
    let req = read_by_agentd("the same body with the label as text", &fixed);
    assert_eq!(req.labels["act"], "true");
}

// ---------------------------------------------------------------- the answers back

/// agentd's own `GET /agents/{id}` answer, as its handler serialises it.
fn snapshot(status: AgentStatus) -> AgentSnapshot {
    AgentSnapshot {
        id: "ag-1".into(),
        provider: "opencode".into(),
        cwd: "/w".into(),
        workspace_id: None,
        model: Some("opencode/deepseek-v4.1-flash-free".into()),
        features: None,
        thinking_option_id: None,
        effective_thinking_option_id: None,
        created_at: "2026-11-22T10:39:00Z".into(),
        updated_at: "2026-11-22T10:41:00Z".into(),
        last_user_message_at: None,
        status,
        active_turn: None,
        capabilities: Capabilities::default(),
        current_mode_id: Some("build".into()),
        available_modes: Vec::new(),
        pending_permissions: Vec::new(),
        persistence: None,
        runtime_info: None,
        last_usage: None,
        last_error: None,
        title: Some("Lucina act: Spegni raffy server".into()),
        labels: BTreeMap::from([
            ("from".into(), "lucina".into()),
            ("act".into(), "true".into()),
        ]),
        requires_attention: None,
        attention_reason: None,
        attention_timestamp: None,
        archived_at: None,
        provider_unavailable: None,
        internal: None,
        gate: None,
        external: None,
        owner_device_id: None,
        host: None,
    }
}

/// Lucina keeps polling while agentd says one of these; anything else ended the run.
#[test]
fn agentds_own_status_is_the_word_act_waits_on() {
    for still in act::WORKING {
        let st = AgentStatus::parse(still).unwrap_or_else(|| {
            panic!("agentd has no status {still}: act waits on a word it never gets")
        });
        let v = serde_json::to_value(snapshot(st)).unwrap();
        assert_eq!(
            act::status_of(&v).as_deref(),
            Some(still),
            "act reads {still} off agentd's own answer"
        );
    }
    for st in AgentStatus::ALL {
        let v = serde_json::to_value(snapshot(st)).unwrap();
        assert_eq!(
            act::status_of(&v).as_deref(),
            Some(st.as_str()),
            "agentd's {st:?} is the text act reads"
        );
    }
    // An answer with no status in it is no status, as the polling code assumes.
    assert_eq!(act::status_of(&json!({"id": "ag-1"})), None);
}

#[test]
fn agentds_own_wait_answer_is_the_one_act_reads() {
    let done = WaitResult {
        status: "idle",
        error: None,
        last_message: Some("  raffy is stopped  ".into()),
        final_snapshot: snapshot(AgentStatus::Idle),
    };
    let v = serde_json::to_value(&done).unwrap();
    let a = act::answer_of(&v);
    assert_eq!(
        a.text.as_deref(),
        Some("raffy is stopped"),
        "the final message, trimmed"
    );
    assert_eq!(a.error, None);
    let failed = WaitResult {
        status: "error",
        error: Some("out of quota".into()),
        last_message: None,
        final_snapshot: snapshot(AgentStatus::Error),
    };
    let v = serde_json::to_value(&failed).unwrap();
    let a = act::answer_of(&v);
    assert_eq!(
        a.error.as_deref(),
        Some("out of quota"),
        "the error it ended on"
    );
    assert_eq!(a.text, None);
    let silent = WaitResult {
        status: "timeout",
        error: None,
        last_message: Some("   ".into()),
        final_snapshot: snapshot(AgentStatus::Running),
    };
    let v = serde_json::to_value(&silent).unwrap();
    let a = act::answer_of(&v);
    assert_eq!(
        a.text, None,
        "blank is no answer, as act reads an agent that said nothing"
    );
    assert_eq!(a.error, None);
}
