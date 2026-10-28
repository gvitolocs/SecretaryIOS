//! Delegation through the real server on the JSON planner path (the live
//! one): explicit asks, the planner's own `delegate_task`, MCP refusal, the
//! owner's switches, the API. The agents are the real adapters pointed at
//! fakes: a fake `claude` and `codex` CLI, the xAI API on wiremock.

mod common;

use std::path::Path;

use common::{harness, script, H};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ACT: &str = r#"{"goal":"news for Giuseppe","risk":"low","reversible":true,"why":"Chiedo a Grok perché legge web e X in tempo reale.","decision":"act"}"#;

fn exe(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct World {
    h: H,
    xai: MockServer,
}

async fn world(extra_rules: Vec<Value>) -> World {
    let xai = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "Rust 1.95 è uscito ieri, con async closures stabili."}]}]
        })))
        .mount(&xai)
        .await;
    let mut rules = vec![
        json!({"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\"}"}),
        json!({"kind": "reply", "contains": "Requested by:", "content": ACT}),
    ];
    rules.extend(extra_rules);
    let s = script(
        json!({"rules": rules, "default": {"content": "RELAYED: Grok dice che Rust 1.95 è uscito."}}),
    );
    let xai_uri = xai.uri();
    let h = harness(s, move |c| {
        let d = c.data_dir.clone();
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        exe(
            &bin.join("claude"),
            &format!(
                "#!/bin/sh\ncat > {}/claude-stdin.txt\necho '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"SQLite basta per uso personale.\",\"session_id\":\"sess-xyz\",\"num_turns\":3}}'\n",
                d.display()
            ),
        );
        exe(
            &bin.join("codex"),
            &format!(
                "#!/bin/sh\necho \"$@\" > {}/codex-args.txt\nout=\"\"\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"-o\" ]; then out=\"$2\"; shift; fi; shift; done\ncat > {}/codex-stdin.txt\necho 'La capitale del Perù è Lima.' > \"$out\"\n",
                d.display(),
                d.display()
            ),
        );
        std::fs::write(d.join("codex-auth.json"), "{}").unwrap();
        std::fs::write(d.join("xai.key"), "xai-test-key-0123456789abcdef\n").unwrap();
        let toml = format!(
            r#"isolation = "none"
inline_wait_secs = 10
sandbox_root = "{d}/sandboxes"
allowed_repo_roots = ["{d}/Projects"]

[claude]
bin = "{d}/bin/claude"

[chatgpt]
backend = "codex"
codex_bin = "{d}/bin/codex"
codex_auth = "{d}/codex-auth.json"

[grok]
backend = "xai"
xai_url = "{xai_uri}"
xai_key_file = "{d}/xai.key"
"#,
            d = d.display()
        );
        std::fs::write(d.join("delegate.toml"), toml).unwrap();
        c.delegate_enabled = true;
        c.delegate_config = Some(d.join("delegate.toml"));
    })
    .await;
    World { h, xai }
}

fn delegation_cards(h: &H) -> Vec<Value> {
    h.timeline_lines()
        .into_iter()
        .filter(|l| l["meta"]["kind"] == "delegation" && l["meta"]["format"] == "v1")
        .collect()
}

#[tokio::test]
async fn chiedi_a_grok_runs_on_the_json_planner_path_with_a_card() {
    let w = world(vec![]).await;
    let secret = "Zx8Qw2Er4Ty6Ui8Op0As2Df4Gh6Jk8Lz";
    lucina_secrets::redactor().register("DELEGATION_IT_SECRET", secret);
    let d =
        w.h.chat(&format!(
            "chiedi a Grok le ultime notizie su Rust (token {secret})"
        ))
        .await;
    assert_eq!(d["action"], "delegate", "{d}");
    assert_eq!(d["reply"], "RELAYED: Grok dice che Rust 1.95 è uscito.");
    // what went to xAI: Lucina's header, web + X search, no secret
    let reqs = w.xai.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let input = body["input"].as_str().unwrap();
    assert!(input.starts_with("[Lucina delegation · job "), "{input}");
    assert!(input.contains("da Lucina") && input.contains("ultime notizie su Rust"));
    assert!(!input.contains(secret), "{input}");
    assert_eq!(
        body["tools"],
        json!([{"type": "web_search"}, {"type": "x_search"}])
    );
    assert_eq!(
        reqs[0].headers.get("authorization").unwrap(),
        "Bearer xai-test-key-0123456789abcdef"
    );
    // the timeline card
    let cards = delegation_cards(&w.h);
    assert_eq!(cards.len(), 1, "{cards:?}");
    let c = &cards[0];
    assert!(
        c["text"]
            .as_str()
            .unwrap()
            .starts_with("Ho chiesto a Grok: «le ultime notizie su Rust"),
        "{c}"
    );
    assert!(c["text"].as_str().unwrap().contains("✅ risposta pronta"));
    assert!(c["text"]
        .as_str()
        .unwrap()
        .contains("Perché: Chiedo a Grok"));
    assert_eq!(c["meta"]["tool"], "grok");
    assert_eq!(c["meta"]["ok"], true);
    assert!(!c.to_string().contains(secret));
    // the job and the status API
    let (s, st) = w.h.get("/delegations").await;
    assert_eq!(s, 200);
    assert_eq!(st["data"]["enabled"], true);
    assert_eq!(st["data"]["budget"]["agents"]["grok"]["calls"], 1);
    assert_eq!(st["data"]["agents"]["grok"]["backend"], "xai-api");
    assert_eq!(st["data"]["agents"]["chatgpt"]["backend"], "codex-cli");
    let id = st["data"]["jobs"][0]["id"].as_str().unwrap().to_owned();
    let (s, job) = w.h.get(&format!("/delegations/{}", &id[..8])).await;
    assert_eq!(s, 200);
    assert_eq!(job["data"]["status"], "done");
    assert_eq!(job["data"]["backend"], "xai-api");
    // audit: no task text
    let audit =
        std::fs::read_to_string(w.h.dir.path().join("lucina-rs/delegate-audit.jsonl")).unwrap();
    assert!(audit.contains("\"agent\":\"grok\"") && !audit.contains("Rust"));
}

#[tokio::test]
async fn claude_and_chatgpt_run_through_their_cli() {
    let w = world(vec![]).await;
    let d =
        w.h.chat("chiedi a Claude di analizzare i pro e contro di SQLite per un'app personale")
            .await;
    assert_eq!(d["action"], "delegate");
    let stdin = std::fs::read_to_string(w.h.dir.path().join("claude-stdin.txt")).unwrap();
    assert!(stdin.contains("pro e contro di SQLite"), "{stdin}");
    let d = w.h.chat("ask ChatGPT what the capital of Peru is").await;
    assert_eq!(d["action"], "delegate");
    let args = std::fs::read_to_string(w.h.dir.path().join("codex-args.txt")).unwrap();
    assert!(args.contains("exec --skip-git-repo-check --ephemeral -m gpt-6.1-sol -c model_reasoning_effort=medium -c mcp_servers={} -s read-only"), "{args}");
    let cards = delegation_cards(&w.h);
    assert_eq!(cards.len(), 2);
    assert!(cards[0]["text"].as_str().unwrap().contains("claude-cli"));
    assert!(cards[1]["text"].as_str().unwrap().contains("codex-cli"));
}

#[tokio::test]
async fn the_planner_may_decide_to_delegate_on_its_own() {
    let w = world(vec![json!({"kind": "plan", "content": "{\"action\":\"delegate_task\",\"to\":\"auto\",\"kind\":\"question\",\"task\":\"What is the capital of Peru?\",\"ack\":\"Chiedo.\"}"})]).await;
    let d =
        w.h.chat("vorrei un secondo parere: qual è la capitale del Perù?")
            .await;
    assert_eq!(d["action"], "delegate", "{d}");
    let stdin = std::fs::read_to_string(w.h.dir.path().join("codex-stdin.txt")).unwrap();
    assert!(
        stdin.contains("What is the capital of Peru?"),
        "question goes to ChatGPT: {stdin}"
    );
    // the plan prompt offered delegate_task
    let plan_req =
        w.h.fake
            .state
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|r| lucina_server::fake::kind_of(r) == "plan")
            .cloned()
            .unwrap();
    assert!(plan_req.to_string().contains("delegate_task"));
}

#[tokio::test]
async fn lucina_mcp_turns_never_delegate() {
    let w = world(vec![json!({"kind": "plan", "content": "{\"action\":\"delegate_task\",\"to\":\"grok\",\"kind\":\"research\",\"task\":\"news\"}"})]).await;
    for msg in [
        "[via Lucina MCP, from claude-code] chiedi a Grok le ultime notizie su Rust",
        "[via Lucina MCP, from chatgpt] non usare Grok",
        "[via Lucina MCP, from grok] cosa succede nel mondo?",
    ] {
        let (s, b) =
            w.h.post(
                "/chat",
                json!({"message": msg, "sessionId": "mcp-claude_code", "channel": "web"}),
            )
            .await;
        assert_eq!(s, 200, "{b}");
        // Guarded callers get the refusal; an agent's consult turn (claude-code) a plain answer
        // that names what it did not run.
        let d = &b["data"];
        assert!(
            d["action"] == "needs_confirmation" || (d["consult"] == true && d["notRun"].is_string()),
            "{msg}: {b}"
        );
    }
    assert!(
        w.xai.received_requests().await.unwrap().is_empty(),
        "no agent was called"
    );
    assert!(delegation_cards(&w.h).is_empty());
    // the owner's switch was not changed by MCP
    let (_, st) = w.h.get("/delegations").await;
    assert_eq!(st["data"]["budget"]["agents"]["grok"]["enabled"], true);
}

#[tokio::test]
async fn the_owner_switches_grok_off_and_on() {
    let w = world(vec![]).await;
    let d = w.h.chat("non usare Grok").await;
    assert_eq!(d["action"], "delegation_settings");
    assert!(d["reply"].as_str().unwrap().contains("non uso più Grok"));
    let d = w.h.chat("chiedi a Grok le ultime notizie su Rust").await;
    assert!(
        d["reply"].as_str().unwrap().contains("non usare Grok"),
        "{d}"
    );
    assert!(w.xai.received_requests().await.unwrap().is_empty());
    let d = w.h.chat("puoi usare di nuovo Grok").await;
    assert!(d["reply"].as_str().unwrap().contains("di nuovo usare Grok"));
    w.h.chat("chiedi a Grok le ultime notizie su Rust").await;
    assert_eq!(w.xai.received_requests().await.unwrap().len(), 1);
    // persisted
    let state =
        std::fs::read_to_string(w.h.dir.path().join("lucina-rs/delegate-state.json")).unwrap();
    assert!(state.contains("\"disabled\": []"), "{state}");
}

#[tokio::test]
async fn the_proactive_door_is_read_only_and_dry_by_default() {
    let w = world(vec![]).await;
    let (s, b) = w
        .h
        .post("/delegate", json!({"origin": "proactive", "task": "novità sul meteo a Copenaghen", "kind": "research"}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["dryRun"], true);
    assert_eq!(b["data"]["status"], "planned");
    assert_eq!(b["data"]["agent"], "Grok");
    assert!(
        w.xai.received_requests().await.unwrap().is_empty(),
        "dry run: nothing sent"
    );
    let (_, b) = w
        .h
        .post("/delegate", json!({"origin": "proactive", "task": "fix the failing test", "kind": "code", "dryRun": false}))
        .await;
    assert_eq!(b["data"]["code"], "proactive_read_only");
    let (s, _) =
        w.h.post("/delegate", json!({"origin": "owner", "task": "x"}))
            .await;
    assert_eq!(s, 403);
}
