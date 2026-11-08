//! The tasks bridge against a fake secretary-server (`/v1/tasks/*`), the
//! same data and expectations as Node's `tests/tasks-bridge.test.mjs`.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_server::bridges::{tasks_context, tasks_for_briefing, tasks_turn, Secretary};
use serde_json::{json, Value};

const TZ: chrono_tz::Tz = chrono_tz::Europe::Rome;
const SERVICE: &str = "svc_test_service_token_0123456789abcdef";

type Calls = Arc<Mutex<Vec<Value>>>;

fn task(id: &str, content: &str, due: Value, priority: u8) -> Value {
    json!({"id": id, "content": content, "project_name": "Lingue", "priority": priority, "labels": [], "due": due})
}

fn view() -> Value {
    json!({
        "today": "2026-10-22",
        "goal": {"daily_goal": 5, "completed_today": 1, "reached": false},
        "sync": {"status": "ok"},
        "list": {"kind": "today", "groups": [
            {"key": "overdue", "kind": "overdue", "tasks": [task("a2", "10 min anki/memrise", json!({"date": "2026-10-20", "string": "every day", "is_recurring": true, "rel_days": -2, "overdue": true}), 4)]},
            {"key": "2026-10-22", "kind": "date", "date": "2026-10-22", "tasks": [
                task("a1", "15 min shadowing", json!({"date": "2026-10-22", "time": "09:00", "string": "every day", "is_recurring": true, "rel_days": 0, "overdue": false}), 4),
                task("a3", "20min speaking", json!({"date": "2026-10-22", "string": "every day", "is_recurring": true, "rel_days": 0, "overdue": false}), 1),
            ]},
        ]},
    })
}

async fn fake(calls: Calls) -> String {
    async fn state(
        State(calls): State<Calls>,
        headers: axum::http::HeaderMap,
        Query(q): Query<std::collections::HashMap<String, String>>,
    ) -> Json<Value> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        calls
            .lock()
            .unwrap()
            .push(json!({"method": "GET", "path": "/v1/tasks/state", "query": q, "auth": auth}));
        let mut v = view();
        if q.get("view").map(String::as_str) == Some("search") {
            let needle = q.get("query").cloned().unwrap_or_default().to_lowercase();
            let hits: Vec<Value> = v["list"]["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["tasks"].as_array().unwrap().clone())
                .filter(|t| {
                    t["content"]
                        .as_str()
                        .unwrap()
                        .to_lowercase()
                        .contains(&needle)
                })
                .collect();
            v["list"] = json!({"kind": "search", "groups": [{"key": "none", "kind": "none", "tasks": hits}]});
        }
        Json(json!({"ok": true, "view": v}))
    }
    async fn command(State(calls): State<Calls>, Json(body): Json<Value>) -> Json<Value> {
        calls
            .lock()
            .unwrap()
            .push(json!({"method": "POST", "path": "/v1/tasks/command", "body": body}));
        let mut v = view();
        v["goal"] = json!({"daily_goal": 5, "completed_today": 2});
        Json(json!({"ok": true, "task_id": "tmp-1",
            "parsed": {"content": "chiamare Marco", "due_string": "domani alle 9", "is_recurring": false, "labels": []},
            "view": v}))
    }
    async fn signals() -> Json<Value> {
        Json(
            json!({"today": "2026-10-22", "completed_today": 1, "daily_goal": 5, "sync_status": "ok",
            "today_tasks": [{"content": "15 min shadowing", "time": "09:00", "days_overdue": 0}],
            "overdue": [{"content": "10 min anki/memrise", "days_overdue": 2}]}),
        )
    }
    let app = Router::new()
        .route("/v1/tasks/state", get(state))
        .route("/v1/tasks/command", post(command))
        .route("/v1/tasks/signals", get(signals))
        .with_state(calls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn secretary(url: Option<String>, dir: &tempfile::TempDir) -> Secretary {
    let token_file = dir.path().join("service.token");
    std::fs::write(&token_file, format!("{SERVICE}\n")).unwrap();
    Secretary {
        http: reqwest::Client::new(),
        url,
        token_file: Some(token_file),
    }
}

#[tokio::test]
async fn tasks_turns_like_node() {
    let calls: Calls = Arc::default();
    let url = fake(calls.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let s = secretary(Some(url), &dir);

    let list = tasks_turn(&s, "cosa devo fare oggi?", false, TZ, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list.action, "tasks");
    assert_eq!(
        list.reply,
        "You have 3 tasks today:\n- 10 min anki/memrise (2d overdue ↻)\n- 15 min shadowing (09:00 ↻)\n- 20min speaking (↻ p1)\nGoal: 1/5 done."
    );
    assert_eq!(list.action_result, "[tasks today: 3]");
    assert_eq!(list.meta.unwrap().kind.as_deref(), Some("tasks"));
    {
        let c = calls.lock().unwrap();
        assert_eq!(c[0]["auth"], format!("Bearer {SERVICE}"));
        assert_eq!(
            c[0]["query"],
            json!({"view": "today", "timezone": "Europe/Rome"})
        );
    }

    let add = tasks_turn(
        &s,
        "aggiungi task chiamare Marco domani alle 9",
        false,
        TZ,
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(add.action, "tasks_add");
    assert_eq!(
        add.reply,
        "Added \"chiamare Marco\", due domani alle 9. It shows up in Todoist shortly."
    );
    let post = calls
        .lock()
        .unwrap()
        .iter()
        .rfind(|c| c["method"] == "POST")
        .cloned()
        .unwrap();
    assert_eq!(
        post["body"]["command"],
        json!({"type": "quick_add", "text": "chiamare Marco domani alle 9"})
    );
    assert!(post["body"]["op_id"]
        .as_str()
        .unwrap()
        .starts_with("lucina-rs-"));
    assert_eq!(post["body"]["timezone"], "Europe/Rome");

    let done = tasks_turn(&s, "fatto lo shadowing", false, TZ, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.action, "tasks_complete");
    assert_eq!(
        done.reply,
        "Nice! Marked \"15 min shadowing\" done (2/5 today). It comes back at its next repeat."
    );
    let last = calls
        .lock()
        .unwrap()
        .iter()
        .rfind(|c| c["method"] == "POST")
        .cloned()
        .unwrap();
    assert_eq!(
        last["body"]["command"],
        json!({"type": "complete", "task_id": "a1"})
    );

    // "min" is a stop word; no single task has both words; not a task.
    assert!(tasks_turn(&s, "done with min", false, TZ, false)
        .await
        .is_none());
    assert!(tasks_turn(&s, "I did the anki speaking", false, TZ, false)
        .await
        .is_none());
    assert!(tasks_turn(&s, "ho fatto la spesa", false, TZ, false)
        .await
        .is_none());

    // MCP: deleting is refused, the owner gets the fixed text.
    assert_eq!(
        tasks_turn(&s, "cancella il task anki", true, TZ, false)
            .await
            .unwrap()
            .unwrap_err(),
        "tasks.delete"
    );
    let del = tasks_turn(&s, "cancella il task anki", false, TZ, false)
        .await
        .unwrap()
        .unwrap();
    // Node guessed English here (no Italian marker word); the function-word detector reads Italian, and
    // she answers in English (owner rule 2026-11-02).
    assert!(del.reply.starts_with("You delete tasks yourself"));
    let en = tasks_turn(&s, "delete the task anki", false, TZ, false)
        .await
        .unwrap()
        .unwrap();
    assert!(en.reply.starts_with("You delete tasks yourself"));
    assert!(tasks_turn(&s, "done with duolingo", true, TZ, false)
        .await
        .is_none());

    let moved = tasks_turn(
        &s,
        "sposta le attività in ritardo a domani",
        false,
        TZ,
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        moved.action_result,
        "[overdue tasks rescheduled to tomorrow]"
    );

    assert_eq!(
        tasks_context(&s, "cosa devo fare oggi?", TZ).await,
        "Giuseppe's tasks (Todoist) today 2026-10-22: 1/5 done; to do: 09:00 15 min shadowing; overdue: 10 min anki/memrise (2d late). Use these when he asks what to do; never invent tasks."
    );
    assert_eq!(tasks_context(&s, "ciao", TZ).await, "");
    assert_eq!(tasks_for_briefing(&s, TZ).await.len(), 3);
}

#[tokio::test]
async fn tasks_unconfigured_degrade() {
    let dir = tempfile::tempdir().unwrap();
    let s = secretary(None, &dir);
    let list = tasks_turn(&s, "what do I need to do today?", false, TZ, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list.reply, "I can't reach your tasks right now.");
    assert_eq!(list.action_result, "[tasks unavailable: not_configured]");
    // A completion that cannot be checked is left to the normal turn.
    assert!(tasks_turn(&s, "fatto lo shadowing", false, TZ, false)
        .await
        .is_none());
    assert_eq!(tasks_context(&s, "cosa devo fare oggi?", TZ).await, "");
    assert!(tasks_for_briefing(&s, TZ).await.is_empty());
}
