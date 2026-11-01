//! Meshy in the chat: plan → pending action with confirm buttons → "sì" →
//! background job; the Telegram confirm endpoint; MCP callers plan only.

mod common;

use std::time::Duration;

use common::{harness, script, H};
use lucina_meshy::mock::{self, MockConfig, MockHandle};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SVC: &str = "svc_meshy_chat_test_0123456789";
const MODEL: &str = "mdl_0123456789abcdef";

struct World {
    h: H,
    meshy: MockHandle,
    secretary: MockServer,
    telegram: MockServer,
}

async fn world(rules: Value) -> World {
    let fixtures = tempfile::tempdir().unwrap().keep();
    for n in [
        "knight.glb",
        "knight_walk.glb",
        "knight_anim.glb",
        "chest.glb",
        "chest_preview.glb",
    ] {
        std::fs::write(fixtures.join(n), b"glTF\x02\0\0\0\x18\0\0\0fixture-padding").unwrap();
    }
    let meshy = mock::spawn(MockConfig {
        assets: fixtures,
        polls_to_finish: 1,
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let secretary = MockServer::start().await;
    let model = json!({"id": MODEL, "title": "A knight", "files": [{"name": "model.glb"}], "animations": ["Casual Walk"], "credits": 38, "meshy": {"taskId": "t", "taskKind": "text-to-3d", "rigTaskId": "r"}});
    Mock::given(method("POST"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"model": model})))
        .mount(&secretary)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex("^/v1/models/mdl_[a-z0-9]+/files/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"model": model})))
        .mount(&secretary)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/models/{MODEL}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"model": model})))
        .mount(&secretary)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notify"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"status": "created"})))
        .mount(&secretary)
        .await;
    let telegram = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex("^/meshy/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&telegram)
        .await;
    let (surl, turl, mbase) = (secretary.uri(), telegram.uri(), meshy.api_base());
    let h = harness(script(rules), move |c| {
        let dir = c.data_dir.clone();
        let key = dir.join("meshy.key");
        let svc = dir.join("svc.token");
        let tg = dir.join("tg.token");
        std::fs::write(&key, "msy_chat_test_key_0000").unwrap();
        std::fs::write(&svc, SVC).unwrap();
        std::fs::write(&tg, "bridge-token").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        c.secretary_url = Some(surl.clone());
        c.secretary_service_token_file = Some(svc.clone());
        let m = &mut c.meshy;
        m.enabled = true;
        m.api_base = mbase;
        m.key_file = Some(key);
        m.poll = Duration::from_millis(5);
        m.poll_min = Duration::from_millis(1);
        m.preview.enabled = false;
        m.secretary_url = Some(surl);
        m.secretary_token_file = Some(svc);
        m.telegram_url = Some(turl);
        m.telegram_token_file = Some(tg);
    })
    .await;
    World {
        h,
        meshy,
        secretary,
        telegram,
    }
}

const INTENT: &str =
    "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}";

fn knight_rules() -> Value {
    let args = json!({"prompt": "a knight in silver armor", "animations": ["walk"]});
    let mut plan = args.clone();
    plan["action"] = json!("plan");
    plan["for_action"] = json!("text_to_3d");
    let mut go = args;
    go["action"] = json!("text_to_3d");
    json!({"rules": [
        {"kind": "intent", "content": INTENT},
        {"kind": "agent", "afterTool": false, "toolCalls": [{"name": "meshy", "arguments": plan}]},
        {"kind": "agent", "afterTool": true, "once": true, "toolCalls": [{"name": "meshy", "arguments": go}]},
        {"kind": "agent", "afterTool": true, "content": "Piano: anteprima 20, texture 10, rig 5, camminata sul posto (#613) 3 = 38 crediti. Procedo?"},
        {"kind": "reply", "content": "Avviato! Ti avviso quando il cavaliere è pronto."}
    ]})
}

async fn telegram_posts(w: &World, p: &str) -> Vec<Value> {
    w.telegram
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == p)
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

async fn wait_job(w: &World, id: &str) -> Value {
    for _ in 0..500 {
        let j = w.h.state.meshy.as_ref().unwrap().job(id).unwrap();
        if j["status"] == "done" || j["status"] == "error" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job did not end");
}

#[tokio::test]
async fn plan_confirm_and_job_in_the_live_json_planner() {
    let w = world(knight_rules()).await;
    let d =
        w.h.chat("Fammi un modello 3D di un cavaliere che cammina")
            .await;
    assert_eq!(d["action"], "meshy");
    assert!(d["reply"].as_str().unwrap().contains("38 crediti"), "{d}");
    assert_eq!(d["format"], "v1");
    assert_eq!(
        d["actions"][0]["payload"],
        json!({"answer": "yes", "text": "sì"})
    );
    assert_eq!(
        w.meshy.creates("/openapi/v2/text-to-3d"),
        0,
        "nothing spent before the yes"
    );
    let pending =
        w.h.state
            .pending
            .current("secretary_test", chrono::Utc::now())
            .unwrap();
    assert_eq!(pending.tool, "meshy");
    assert!(
        pending.summary.ends_with("= 38 credits"),
        "{}",
        pending.summary
    );
    let plans = telegram_posts(&w, "/meshy/plan").await;
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0]["pendingId"], json!(pending.id));
    assert!(plans[0]["text"]
        .as_str()
        .unwrap()
        .contains("Casual Walk (in place, #613)"));
    // the stored Lucina item carries the buttons
    let item =
        w.h.timeline_lines()
            .into_iter()
            .rev()
            .find(|l| l["role"] == "lucina" && l["meta"]["kind"] == "meshy_plan")
            .unwrap();
    assert_eq!(item["meta"]["actions"].as_array().unwrap().len(), 2);

    let d = w.h.chat("sì").await;
    assert_eq!(
        d["reply"],
        "Avviato! Ti avviso quando il cavaliere è pronto."
    );
    let job_id = d["lucinaItemId"].as_str().map(|_| ()).and_then(|_| {
        w.h.timeline_lines()
            .into_iter()
            .rev()
            .find_map(|l| l["meta"]["jobId"].as_str().map(str::to_owned))
    });
    let job_id = job_id.expect("job id on the reply");
    let j = wait_job(&w, &job_id).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["modelId"], MODEL);
    assert_eq!(w.meshy.creates("/openapi/v2/text-to-3d"), 2);
    assert_eq!(w.meshy.creates("/openapi/v1/rigging"), 1);
    assert_eq!(w.meshy.creates("/openapi/v1/animations"), 1);
    let done =
        w.h.timeline_lines()
            .into_iter()
            .rev()
            .find(|l| l["meta"]["blocks"].is_array())
            .unwrap();
    assert_eq!(done["meta"]["blocks"][0]["type"], "model3d");
    assert_eq!(done["meta"]["blocks"][0]["id"], MODEL);
    let notified: Vec<_> = w
        .secretary
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/v1/notify")
        .collect();
    assert_eq!(notified.len(), 1);
    assert_eq!(
        telegram_posts(&w, "/meshy/text").await.len(),
        1,
        "no preview video: the text goes to Telegram"
    );
    // a second yes is an ordinary message: the action ran once
    assert!(w
        .h
        .state
        .pending
        .current("secretary_test", chrono::Utc::now())
        .is_none());
}

#[tokio::test]
async fn telegram_buttons_confirm_once() {
    let w = world(knight_rules()).await;
    w.h.chat("Fammi un modello 3D di un cavaliere").await;
    let p =
        w.h.state
            .pending
            .current("secretary_test", chrono::Utc::now())
            .unwrap();
    let (s, b) =
        w.h.post(
            "/meshy/confirm",
            json!({"pendingId": p.id, "answer": "maybe"}),
        )
        .await;
    assert_eq!(s, 400, "{b}");
    let (s, b) =
        w.h.post(
            "/meshy/confirm",
            json!({"pendingId": p.id, "answer": "yes", "channel": "telegram"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(b["data"]["status"], "started", "{b}");
    let job = b["data"]["result"]["jobId"].as_str().unwrap().to_owned();
    let (_, again) =
        w.h.post(
            "/meshy/confirm",
            json!({"pendingId": p.id, "answer": "yes"}),
        )
        .await;
    assert_eq!(again["data"]["status"], "expired");
    assert_eq!(wait_job(&w, &job).await["status"], "done");
    assert_eq!(
        w.meshy.creates("/openapi/v2/text-to-3d"),
        2,
        "one job, not two"
    );
    // "no" on a fresh plan discards it
    let p = w.h.state.pending.create(
        "secretary_test",
        "meshy",
        json!({"action": "text_to_3d", "prompt": "a chest"}),
        "meshy text_to_3d \"A chest\": preview 20 + refine 10 = 30 credits",
        chrono::Utc::now(),
    );
    let (_, b) =
        w.h.post("/meshy/confirm", json!({"pendingId": p.id, "answer": "no"}))
            .await;
    assert_eq!(b["data"]["status"], "declined");
    assert_eq!(w.meshy.creates("/openapi/v2/text-to-3d"), 2);
}

#[tokio::test]
async fn mcp_callers_plan_but_never_spend() {
    let w = world(knight_rules()).await;
    let (s, d) = w
        .h
        .post("/chat", json!({"message": "[via Lucina MCP, from chatgpt] fammi un modello 3D di un cavaliere", "sessionId": "mcp-chatgpt", "channel": "web"}))
        .await;
    assert_eq!(s, 200, "{d}");
    // the plan ran; the spending call was refused, so nothing is pending anywhere
    assert!(w
        .h
        .state
        .pending
        .current("secretary_mcp-chatgpt", chrono::Utc::now())
        .is_none());
    assert!(
        d["data"]["actions"].is_null(),
        "no confirm buttons for an MCP caller: {d}"
    );
    assert_eq!(w.meshy.creates("/openapi/v2/text-to-3d"), 0);
    let tool_items: Vec<Value> =
        w.h.timeline_lines()
            .into_iter()
            .filter(|l| l["meta"]["tool"] == "meshy")
            .collect();
    assert_eq!(tool_items.len(), 2, "{tool_items:?}");
    assert_eq!(tool_items[0]["meta"]["ok"], true, "the plan ran");
    assert_eq!(
        tool_items[1]["meta"]["ok"], false,
        "the spending call was refused"
    );
    assert!(tool_items[1]["text"]
        .as_str()
        .unwrap()
        .contains("38 credits"));
    assert!(telegram_posts(&w, "/meshy/plan").await.is_empty());
}
