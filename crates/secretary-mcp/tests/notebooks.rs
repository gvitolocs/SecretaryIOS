//! Lucina Notebooks tools (scope `notebooks`) against the `/v1/notebooks` fake in
//! `common::notebooks`: scope gating, schemas and annotations, argument checks that
//! stop before the backend, request bodies, the formatted answers with their numbered
//! citations, and the errors the model can act on.

mod common;

use common::notebooks::{CHAT, NB, PODCAST, SRC};
use common::*;
use serde_json::json;

const NOTEBOOK_TOOLS: [&str; 8] = [
    "notebook_list",
    "notebook_get",
    "notebook_create",
    "notebook_add_source",
    "notebook_ask",
    "notebook_search",
    "notebook_transform",
    "notebook_podcast",
];

fn requests(h: &Harness, path: &str) -> Vec<Recorded> {
    h.mock.requests(path)
}

fn notebook_calls(h: &Harness) -> usize {
    h.mock
        .state
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|r| r.path.starts_with("/v1/notebooks"))
        .count()
}

#[tokio::test]
async fn notebook_tools_need_the_notebooks_scope() {
    let h = start().await;
    // read, chat, write, computer and agents do not imply it.
    let others = h.issue("grok", "read,chat,write,computer,agents");
    let names = h.tool_names(&others).await;
    assert!(
        !names.iter().any(|n| n.starts_with("notebook_")),
        "{names:?}"
    );
    let (err, text) = h.call(&others, "notebook_list", json!({})).await;
    assert!(err && text.contains("`notebooks` scope"), "{text}");
    let (err, _) = h
        .call(
            &others,
            "notebook_ask",
            json!({"notebook_id": NB, "question": "hi"}),
        )
        .await;
    assert!(err);
    assert_eq!(notebook_calls(&h), 0, "nothing reached the backend");

    let owner = h.issue("claude", "notebooks");
    let names = h.tool_names(&owner).await;
    assert_eq!(names, NOTEBOOK_TOOLS.map(String::from).to_vec());

    let r = h.rpc(&owner, "tools/list", json!({})).await;
    for t in r["tools"].as_array().unwrap() {
        let name = t["name"].as_str().unwrap();
        let ro = matches!(name, "notebook_list" | "notebook_get" | "notebook_search");
        assert_eq!(t["annotations"]["readOnlyHint"], ro, "{name}");
        assert_eq!(
            t["annotations"]["destructiveHint"], false,
            "{name}: nothing here deletes"
        );
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{name}");
        assert!(t["description"].as_str().unwrap().len() > 60, "{name}");
    }
    let ask = r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "notebook_ask")
        .unwrap();
    assert!(ask["description"]
        .as_str()
        .unwrap()
        .contains("[source:<source id>-<n>]"));
    assert_eq!(
        ask["inputSchema"]["required"],
        json!(["notebook_id", "question"])
    );
}

#[tokio::test]
async fn list_get_and_create() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h.call(&t, "notebook_list", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.contains("1 notebook(s)")
            && text.contains(&format!(
                "{NB} · CNS · 2 sources, 1 note, 1 chat, 1 podcast"
            )),
        "{text}"
    );
    assert!(text.contains("Course notes"), "{text}");
    let (err, _) = h
        .call(&t, "notebook_list", json!({"archived": "only"}))
        .await;
    assert!(!err);
    assert_eq!(
        requests(&h, "/v1/notebooks").last().unwrap().query,
        "archived=only"
    );
    let (err, text) = h
        .call(&t, "notebook_list", json!({"archived": "all"}))
        .await;
    assert!(err && text.contains("Invalid arguments"), "{text}");

    let (err, text) = h.call(&t, "notebook_get", json!({"notebook_id": NB})).await;
    assert!(!err, "{text}");
    assert!(text.starts_with(&format!("CNS ({NB})")), "{text}");
    for want in [
        "Sources (2):",
        &format!("- {SRC} · pdf · RM Class 2 · ready · 22 pages, 37 passages, context full"),
        "- s1111111111 · web · Wiki: Ring · extracting",
        "Notes (1):",
        "- rm-class-2-summary · RM Class 2 summary",
        "Chats (1):",
        &format!("- {CHAT} · Exam prep · 4 messages"),
        "Podcasts (1):",
        &format!("- {PODCAST} · CNS in ten minutes · ready · 5:12"),
        "[source:<source id>-<n>]",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(text.contains("updated 2026-10-29"), "{text}");
    // Not found and malformed ids.
    let (err, text) = h
        .call(&t, "notebook_get", json!({"notebook_id": "nb0000000000"}))
        .await;
    assert!(err && text.contains("Not found"), "{text}");
    let before = notebook_calls(&h);
    for id in ["", "CNS", "../etc", "s0a9be3c112", "nb4f1c09a2e7/sources"] {
        let (err, text) = h.call(&t, "notebook_get", json!({"notebook_id": id})).await;
        assert!(err && text.contains("notebook_id"), "{id}: {text}");
    }
    assert_eq!(
        notebook_calls(&h),
        before,
        "bad ids never reach the backend"
    );

    let (err, text) = h
        .call(
            &t,
            "notebook_create",
            json!({"name": "  Reti  ", "description": "Networks"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains(NB) && text.contains("Reti") && text.contains("notebook_add_source"),
        "{text}"
    );
    let post = requests(&h, "/v1/notebooks")
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(
        post.body,
        json!({"name": "Reti", "description": "Networks"})
    );
    assert_eq!(
        post.headers["authorization"],
        format!("Bearer {DEVICE_TOKEN}")
    );
    assert_eq!(
        post.headers["x-secretary-caller"], "mcp:claude",
        "Lucina logs which client asked"
    );
    for args in [
        json!({"name": ""}),
        json!({"name": "x".repeat(201)}),
        json!({}),
        json!({"name": "a", "colour": "red"}),
    ] {
        assert!(h.call(&t, "notebook_create", args).await.0);
    }
    assert_eq!(
        requests(&h, "/v1/notebooks")
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn add_source_variants_and_checks() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let path = format!("/v1/notebooks/{NB}/sources");
    for (args, body) in [
        (
            json!({"url": "https://en.wikipedia.org/wiki/Ring_(mathematics)"}),
            json!({"kind": "url", "url": "https://en.wikipedia.org/wiki/Ring_(mathematics)"}),
        ),
        (
            json!({"text": "Ring axioms", "title": "My notes"}),
            json!({"kind": "text", "text": "Ring axioms", "title": "My notes"}),
        ),
        (
            json!({"text": "No title"}),
            json!({"kind": "text", "text": "No title"}),
        ),
        (
            json!({"sandbox_path": "/home/cua/Downloads/paper.pdf"}),
            json!({"kind": "sandbox", "path": "/home/cua/Downloads/paper.pdf"}),
        ),
        (
            json!({"note_slug": "rm-class-2-summary"}),
            json!({"kind": "note", "slug": "rm-class-2-summary"}),
        ),
        (
            json!({"url": "https://example.com/a.pdf", "transformations": ["t-dense-summary"]}),
            json!({"kind": "url", "url": "https://example.com/a.pdf", "transformations": ["t-dense-summary"]}),
        ),
    ] {
        let mut a = args.clone();
        a["notebook_id"] = json!(NB);
        let (err, text) = h.call(&t, "notebook_add_source", a).await;
        assert!(!err, "{args}: {text}");
        assert!(
            text.contains("Added source s2222222222")
                && text.contains("queued")
                && text.contains("notebook_get"),
            "{text}"
        );
        assert_eq!(requests(&h, &path).last().unwrap().body, body, "{args}");
    }
    let sent = requests(&h, &path).len();
    for (args, why) in [
        (json!({}), "exactly one"),
        (json!({"url": "https://a.org", "text": "x"}), "exactly one"),
        (json!({"url": "ftp://a.org/x"}), "http(s)"),
        (json!({"url": "javascript:alert(1)"}), "http(s)"),
        (json!({"url": "https://a.org/a b"}), "http(s)"),
        (json!({"text": "   "}), "must not be empty"),
        (json!({"text": "x".repeat(1_000_001)}), "longer than"),
        (json!({"url": "https://a.org", "title": "t"}), "goes with"),
        (json!({"sandbox_path": "relative/file.pdf"}), "absolute"),
        (
            json!({"sandbox_path": "/home/cua/../../etc/passwd"}),
            "absolute",
        ),
        (json!({"note_slug": "Not A Slug"}), "slug"),
        (
            json!({"url": "https://a.org", "transformations": ["dense-summary"]}),
            "transformation_id",
        ),
        (
            json!({"url": "https://a.org", "transformations": vec!["t-a"; 11]}),
            "at most 10",
        ),
    ] {
        let mut a = args.clone();
        a["notebook_id"] = json!(NB);
        let (err, text) = h.call(&t, "notebook_add_source", a).await;
        assert!(err && text.contains(why), "{args}: {text}");
    }
    let (err, text) = h
        .call(
            &t,
            "notebook_add_source",
            json!({"notebook_id": "nb123", "url": "https://a.org"}),
        )
        .await;
    assert!(err && text.contains("notebook_id"), "{text}");
    assert_eq!(
        requests(&h, &path).len(),
        sent,
        "refused before the backend"
    );
}

#[tokio::test]
async fn ask_makes_a_chat_and_numbers_the_citations() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h
        .call(
            &t,
            "notebook_ask",
            json!({"notebook_id": NB, "question": "  What is a ring?\n", "model": "qwen"}),
        )
        .await;
    assert!(!err, "{text}");
    // A chat was made from the question, then the question posted with the model.
    let chats = requests(&h, &format!("/v1/notebooks/{NB}/chats"));
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].body, json!({"title": "What is a ring?"}));
    let msgs = requests(&h, &format!("/v1/notebooks/chats/{CHAT}/messages"));
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].body,
        json!({"text": "What is a ring?", "model": "qwen"})
    );
    assert_eq!(msgs[0].headers["x-secretary-caller"], "mcp:claude");
    // The answer keeps its inline markers and gets a numbered list under it.
    assert!(
        text.starts_with("A ring is a set with two operations [source:s0a9be3c112-14]"),
        "{text}"
    );
    for want in [
        "Citations:\n1. [source:s0a9be3c112-14] RM Class 2, page 7: \"A ring is a set R with two binary operations\"",
        "2. [source:s0a9be3c112-15] Lecture video, at 12:34: \"and more\"",
        &format!("Chat {CHAT}: pass it as chat_id"),
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // A follow-up in the same chat makes no new chat and sends no model unless given.
    let (err, text) = h
        .call(
            &t,
            "notebook_ask",
            json!({"notebook_id": NB, "question": "And a field?", "chat_id": CHAT}),
        )
        .await;
    assert!(!err && text.contains("Citations:"), "{text}");
    assert_eq!(requests(&h, &format!("/v1/notebooks/{NB}/chats")).len(), 1);
    let msgs = requests(&h, &format!("/v1/notebooks/chats/{CHAT}/messages"));
    assert_eq!(msgs.last().unwrap().body, json!({"text": "And a field?"}));
}

#[tokio::test]
async fn ask_errors_are_actionable() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h
        .call(
            &t,
            "notebook_ask",
            json!({"notebook_id": NB, "question": "this is broken"}),
        )
        .await;
    assert!(err && text.contains("not available right now"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "notebook_ask",
            json!({"notebook_id": "nb0000000000", "question": "hi"}),
        )
        .await;
    assert!(err && text.contains("Not found"), "{text}");
    let before = notebook_calls(&h);
    for (args, why) in [
        (
            json!({"notebook_id": NB, "question": ""}),
            "must not be empty",
        ),
        (
            json!({"notebook_id": NB, "question": "x".repeat(12_001)}),
            "longer than",
        ),
        (
            json!({"notebook_id": NB, "question": "hi", "chat_id": "s0a9be3c112-14"}),
            "chat_id",
        ),
        (
            json!({"notebook_id": NB, "question": "hi", "model": "gpt"}),
            "Invalid arguments",
        ),
        (json!({"question": "hi"}), "Invalid arguments"),
    ] {
        let (err, text) = h.call(&t, "notebook_ask", args.clone()).await;
        assert!(err && text.contains(why), "{args}: {text}");
    }
    assert_eq!(notebook_calls(&h), before, "refused before the backend");
}

#[tokio::test]
async fn search_in_one_notebook_or_in_all() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h
        .call(
            &t,
            "notebook_search",
            json!({"query": "ring axioms", "notebook_id": NB, "limit": 5}),
        )
        .await;
    assert!(!err, "{text}");
    let r = requests(&h, &format!("/v1/notebooks/{NB}/search"));
    assert_eq!(r.len(), 1);
    assert!(
        r[0].query.contains("q=ring+axioms") || r[0].query.contains("q=ring%20axioms"),
        "{}",
        r[0].query
    );
    assert!(r[0].query.contains("limit=5"));
    for want in [
        "2 hit(s) for \"ring axioms\"",
        "- [source:s0a9be3c112-14] RM Class 2 · score 0.83",
        "A ring is a set R with two operations",
        "- [note:rm-class-2-summary] RM Class 2 summary · score 0.50",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("· notebook "), "{text}");
    let (err, text) = h
        .call(&t, "notebook_search", json!({"query": "ring"}))
        .await;
    assert!(!err && text.contains(&format!("· notebook {NB}")), "{text}");
    assert_eq!(requests(&h, "/v1/notebooks/search").len(), 1);
    for args in [
        json!({"query": " "}),
        json!({"query": "x", "limit": 0}),
        json!({"query": "x", "limit": 51}),
        json!({"query": "x", "notebook_id": "nope"}),
    ] {
        assert!(h.call(&t, "notebook_search", args).await.0);
    }
    assert_eq!(requests(&h, "/v1/notebooks/search").len(), 1);
}

#[tokio::test]
async fn transform_starts_lists_and_rejects_citation_ids() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h
        .call(
            &t,
            "notebook_transform",
            json!({"source_id": SRC, "transformation_id": "t-dense-summary"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("Insight i5555555555")
            && text.contains("t-dense-summary")
            && text.contains(SRC)
            && text.contains("queued"),
        "{text}"
    );
    let post = requests(&h, &format!("/v1/notebooks/sources/{SRC}/insights"))
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(post.body, json!({"transformation_id": "t-dense-summary"}));
    // Without a transformation: nothing is started; the choices and the existing insights are listed.
    let (err, text) = h
        .call(&t, "notebook_transform", json!({"source_id": SRC}))
        .await;
    assert!(!err, "{text}");
    for want in [
        "- t-dense-summary · Dense Summary [applied to new sources]: A tight summary",
        "- t-key-insights · Key Insights",
        &format!("Insights on {SRC}:"),
        "Insight i4f1c09a2e7 (Dense Summary, transformation t-dense-summary) on source",
        "Rings are sets.",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert_eq!(
        requests(&h, &format!("/v1/notebooks/sources/{SRC}/insights"))
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1
    );
    let before = notebook_calls(&h);
    let (err, text) = h
        .call(
            &t,
            "notebook_transform",
            json!({"source_id": "s0a9be3c112-14", "transformation_id": "t-dense-summary"}),
        )
        .await;
    assert!(
        err && text.contains("source_id"),
        "a citation id is not a source id: {text}"
    );
    let (err, text) = h
        .call(
            &t,
            "notebook_transform",
            json!({"source_id": SRC, "transformation_id": "Dense Summary"}),
        )
        .await;
    assert!(err && text.contains("transformation_id"), "{text}");
    assert_eq!(notebook_calls(&h), before);
}

#[tokio::test]
async fn podcast_starts_and_reports() {
    let h = start().await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h.call(&t, "notebook_podcast", json!({"notebook_id": NB, "briefing": "Focus on rings", "minutes": 10, "episode_profile": "two-hosts"})).await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Started. Podcast p4f1c09a2e7")
            && text.contains("queued")
            && text.contains("Not ready yet"),
        "{text}"
    );
    let post = requests(&h, &format!("/v1/notebooks/{NB}/podcasts"))
        .pop()
        .unwrap();
    assert_eq!(
        post.body,
        json!({"briefing": "Focus on rings", "episode_profile": "two-hosts", "minutes": 10})
    );
    let (err, text) = h
        .call(&t, "notebook_podcast", json!({"podcast_id": PODCAST}))
        .await;
    assert!(!err, "{text}");
    for want in [
        "ready · 5:12",
        "1. Neurons",
        "2. Synapses",
        "Script: 1 line.",
        "audio is ready",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let before = notebook_calls(&h);
    for (args, why) in [
        (json!({}), "notebook_id"),
        (
            json!({"notebook_id": NB, "podcast_id": PODCAST}),
            "not both",
        ),
        (json!({"notebook_id": NB, "minutes": 1}), "2-60"),
        (json!({"notebook_id": NB, "minutes": 61}), "2-60"),
        (
            json!({"notebook_id": NB, "speaker_profile": "../x"}),
            "profile name",
        ),
        (
            json!({"notebook_id": NB, "briefing": "x".repeat(4001)}),
            "longer than",
        ),
        (json!({"podcast_id": "p1"}), "podcast_id"),
    ] {
        let (err, text) = h.call(&t, "notebook_podcast", args.clone()).await;
        assert!(err && text.contains(why), "{args}: {text}");
    }
    assert_eq!(notebook_calls(&h), before);
}

#[tokio::test]
async fn backend_failures_read_well() {
    let h = start_with(|cfg, _| cfg.backend_token = Some("not-the-device-token".into())).await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h.call(&t, "notebook_list", json!({})).await;
    assert!(err && text.contains("unauthorized"), "{text}");
    let h = start_with(|cfg, _| cfg.backend_token = None).await;
    let t = h.issue("claude", "notebooks");
    let (err, text) = h.call(&t, "notebook_list", json!({})).await;
    assert!(
        err && text.contains("SECRETARY_MCP_BACKEND_TOKEN_FILE"),
        "{text}"
    );
}
