//! `tasks`: Giuseppe's tasks, synced with his Todoist by secretary-server
//! (`/v1/tasks/*`, service token).
//!
//! - `list` (today with the overdue ones, upcoming, inbox, or a search) only
//!   reads.
//! - `add` (Todoist quick add: dates and recurrence in it/en/da, `#Project`,
//!   `@label`, `p1`), `complete` and `reschedule` change only the owner's
//!   own task list and run at once (Todoist gets them within a minute).
//! - `delete` goes through the confirmation flow: it runs only after the
//!   owner's explicit yes (and stays undoable in the app for a while).
//!
//! Configuration: `LUCINA_SECRETARY_SERVER_URL` and
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (shared with `calendar`).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::calendar::server_of;
use crate::{schema_for, Ctx, SideEffect, Tool, ToolEnv, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Add,
    Complete,
    Reschedule,
    Delete,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum View {
    Today,
    Upcoming,
    Inbox,
    Search,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// list: today (default: overdue + today), upcoming (7 days), inbox, or search (with `query`).
    view: Option<View>,
    /// list search: words of the title. complete/reschedule/delete: words of the title instead of `id`.
    query: Option<String>,
    /// add: the task in Todoist quick add style, e.g. "15 min shadowing every day", "chiamare Marco domani alle 9 #Lavoro p1".
    text: Option<String>,
    /// complete/reschedule/delete: the task id from list.
    id: Option<String>,
    /// reschedule: YYYY-MM-DD or a phrase (today, tomorrow, next week, domani, i morgen). Without `id`/`query`: every overdue task.
    date: Option<String>,
}

pub struct TasksTool;

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    server_of(&ctx.env).ok_or_else(|| {
        ToolError::new("tasks are not configured (LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE)")
    })
}

async fn send(req: reqwest::RequestBuilder) -> Result<Value, ToolError> {
    let resp = req.send().await.map_err(|e| {
        ToolError::new(format!("secretary-server unreachable: {}", e.without_url()))
    })?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    // engine answers {ok:false, code, error}; the gateway {error:{code,message}}
    let message = body["error"]["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or_default();
    Err(ToolError::new(if message.is_empty() {
        format!("secretary-server answered {}", status.as_u16())
    } else {
        message.to_owned()
    }))
}

/// One task, compact for the model.
fn compact(t: &Value) -> Value {
    let mut o = json!({"id": t["id"], "content": t["content"], "project": t["project_name"]});
    if let Some(d) = t.get("due").filter(|d| d.is_object()) {
        o["due"] = d["date"].clone();
        if let Some(time) = d["time"].as_str() {
            o["time"] = json!(time);
        }
        if d["is_recurring"].as_bool() == Some(true) {
            o["repeats"] = d["string"].clone();
        }
        if d["overdue"].as_bool() == Some(true) {
            o["overdue_days"] = json!(-d["rel_days"].as_i64().unwrap_or(0));
        }
    }
    if let Some(p) = t["priority"].as_u64().filter(|p| *p < 4) {
        o["priority"] = json!(format!("p{p}"));
    }
    if t["labels"].as_array().is_some_and(|l| !l.is_empty()) {
        o["labels"] = t["labels"].clone();
    }
    o
}

fn all_tasks(view: &Value) -> Vec<&Value> {
    fn walk<'a>(t: &'a Value, out: &mut Vec<&'a Value>) {
        out.push(t);
        for c in t["children"].as_array().into_iter().flatten() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for g in view["list"]["groups"].as_array().into_iter().flatten() {
        for t in g["tasks"].as_array().into_iter().flatten() {
            walk(t, &mut out);
        }
    }
    out
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' => 'a',
            'è' | 'é' => 'e',
            'ì' | 'í' => 'i',
            'ò' | 'ó' => 'o',
            'ù' | 'ú' => 'u',
            c => c,
        })
        .collect()
}

/// `(id, content)` of tasks whose title has every word of `q`.
fn matches(view: &Value, q: &str) -> Vec<(String, String)> {
    let words: Vec<String> = fold(q).split_whitespace().map(str::to_owned).collect();
    let mut out: Vec<(String, String)> = all_tasks(view)
        .into_iter()
        .filter(|t| {
            let c = fold(t["content"].as_str().unwrap_or(""));
            !words.is_empty() && words.iter().all(|w| c.contains(w.as_str()))
        })
        .map(|t| {
            (
                t["id"].as_str().unwrap_or_default().to_owned(),
                t["content"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

impl TasksTool {
    async fn state(
        base: &str,
        token: &str,
        ctx: &Ctx,
        view: &str,
        query: Option<&str>,
    ) -> Result<Value, ToolError> {
        let mut q = vec![
            ("view", view.to_owned()),
            ("timezone", ctx.env.time_zone.name().to_owned()),
        ];
        if let Some(x) = query {
            q.push(("query", x.to_owned()));
        }
        let v = send(
            ctx.env
                .http
                .get(format!("{base}/v1/tasks/state"))
                .bearer_auth(token)
                .query(&q),
        )
        .await?;
        Ok(v["view"].clone())
    }

    async fn command(
        base: &str,
        token: &str,
        ctx: &Ctx,
        command: Value,
    ) -> Result<Value, ToolError> {
        let body = json!({
            "op_id": format!("lucina-{}", uuid::Uuid::new_v4()),
            "timezone": ctx.env.time_zone.name(),
            "command": command,
        });
        send(
            ctx.env
                .http
                .post(format!("{base}/v1/tasks/command"))
                .bearer_auth(token)
                .json(&body),
        )
        .await
    }

    /// The task an action is about: `id`, or one title match among today's
    /// and overdue tasks (then all open tasks).
    async fn target(
        base: &str,
        token: &str,
        ctx: &Ctx,
        a: &Args,
    ) -> Result<(String, String), ToolError> {
        if let Some(id) = a.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if id.len() > 128 || id.chars().any(|c| c.is_control() || c == '/') {
                return Err(ToolError::new("id must be a task id from list"));
            }
            return Ok((id.to_owned(), String::new()));
        }
        let q = a
            .query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::new("give the task id (from list) or words of its title in query")
            })?;
        let today = Self::state(base, token, ctx, "today", None).await?;
        let mut hits = matches(&today, q);
        if hits.is_empty() {
            let found = Self::state(base, token, ctx, "search", Some(q)).await?;
            hits = matches(&found, q);
        }
        match hits.len() {
            0 => Err(ToolError::new(format!("no open task matches \"{q}\""))),
            1 => Ok(hits.remove(0)),
            _ => Err(ToolError::new(format!(
                "several tasks match \"{q}\": {}; ask which one, or use its id",
                hits.iter()
                    .take(6)
                    .map(|(id, c)| format!("\"{c}\" ({id})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

#[async_trait::async_trait]
impl Tool for TasksTool {
    fn name(&self) -> &'static str {
        "tasks"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's to-do list, synced with his Todoist (his daily routines like \"15 min shadowing\", \"10 min anki\", plus one-off tasks). \
         list: today (overdue first), upcoming, inbox, or search with query. \
         add: one task in Todoist quick add style (dates and repeats in Italian, English or Danish, #Project, @label, p1-p4). \
         complete: mark done by id or by words of the title (\"fatto lo shadowing\" → query \"shadowing\"); a repeating task moves to its next date. \
         reschedule: move a task (id/query) to `date`, or with neither every overdue task. \
         delete: only after Giuseppe's explicit yes."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn needs_confirmation(&self, args: &Value) -> bool {
        args.get("action").and_then(Value::as_str) == Some("delete")
    }
    fn summarize(&self, args: &Value) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
        match s("action") {
            "delete" => format!(
                "tasks: delete {}",
                if s("id").is_empty() {
                    s("query")
                } else {
                    s("id")
                }
            ),
            "add" => format!("tasks: add \"{}\"", s("text")),
            other => format!("tasks {other}"),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (base, token) = server(ctx)?;
        match a.action {
            Action::List => {
                let view = match a.view.unwrap_or(View::Today) {
                    View::Today => "today",
                    View::Upcoming => "upcoming",
                    View::Inbox => "inbox",
                    View::Search => "search",
                };
                let q = a.query.as_deref().map(str::trim).filter(|s| !s.is_empty());
                if view == "search" && q.is_none() {
                    return Err(ToolError::new("search needs query"));
                }
                let v = Self::state(&base, &token, ctx, view, q).await?;
                let mut groups = Vec::new();
                for g in v["list"]["groups"].as_array().into_iter().flatten() {
                    let tasks: Vec<Value> = g["tasks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(60)
                        .map(compact)
                        .collect();
                    if tasks.is_empty() && view != "today" {
                        continue;
                    }
                    let name = match g["kind"].as_str() {
                        Some("overdue") => "overdue".to_owned(),
                        Some("date") => g["date"].as_str().unwrap_or("").to_owned(),
                        _ => g["title"]
                            .as_str()
                            .filter(|t| !t.is_empty())
                            .unwrap_or("tasks")
                            .to_owned(),
                    };
                    groups.push(json!({"group": name, "tasks": tasks}));
                }
                let mut out = json!({
                    "today": v["today"], "view": view, "groups": groups,
                    "daily_goal": v["goal"]["daily_goal"], "completed_today": v["goal"]["completed_today"],
                });
                match v["sync"]["status"].as_str() {
                    Some("disconnected") => {
                        out["note"] = json!(
                        "Todoist is not connected; Giuseppe can connect it in Secretary → Links."
                    )
                    }
                    Some("reconnect") => {
                        out["note"] = json!(
                        "Todoist needs reconnecting in Secretary → Links; changes wait until then."
                    )
                    }
                    _ => {}
                }
                Ok(out)
            }
            Action::Add => {
                let text = a
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.chars().count() <= 800)
                    .ok_or_else(|| ToolError::new("text is required (at most 800 characters)"))?;
                let r = Self::command(
                    &base,
                    &token,
                    ctx,
                    json!({"type": "quick_add", "text": text}),
                )
                .await?;
                let p = &r["parsed"];
                Ok(json!({
                    "added": true, "id": r["task_id"], "content": p["content"],
                    "due": p["due_date"], "due_text": p["due_string"], "repeats": p["is_recurring"],
                    "project": p["project"], "labels": p["labels"], "priority": p["priority"],
                }))
            }
            Action::Complete => {
                let (id, content) = Self::target(&base, &token, ctx, &a).await?;
                Self::command(
                    &base,
                    &token,
                    ctx,
                    json!({"type": "complete", "task_id": id}),
                )
                .await?;
                Ok(json!({"completed": true, "id": id, "content": content}))
            }
            Action::Reschedule => {
                let date = a
                    .date
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("today")
                    .to_owned();
                let has_target = a.id.as_deref().is_some_and(|s| !s.trim().is_empty())
                    || a.query.as_deref().is_some_and(|s| !s.trim().is_empty());
                if !has_target {
                    let r = Self::command(
                        &base,
                        &token,
                        ctx,
                        json!({"type": "reschedule_overdue", "date": date}),
                    )
                    .await?;
                    return Ok(
                        json!({"rescheduled": "all overdue", "date": date, "overdue_left": r["view"]["counts"]["overdue"]}),
                    );
                }
                let (id, content) = Self::target(&base, &token, ctx, &a).await?;
                Self::command(
                    &base,
                    &token,
                    ctx,
                    json!({"type": "reschedule", "task_id": id, "date": date}),
                )
                .await?;
                Ok(json!({"rescheduled": true, "id": id, "content": content, "date": date}))
            }
            Action::Delete => {
                let (id, content) = Self::target(&base, &token, ctx, &a).await?;
                Self::command(&base, &token, ctx, json!({"type": "delete", "task_id": id})).await?;
                Ok(
                    json!({"deleted": true, "id": id, "content": content, "undo": "Giuseppe can undo it in the app for a few seconds."}),
                )
            }
        }
    }
}

/// One context line with today's tasks for the chat prompt (and the daily
/// briefing), or `None` when tasks are not configured or do not answer in 3 s.
pub async fn today_context(env: &ToolEnv, now: DateTime<Utc>) -> Option<String> {
    let _ = now;
    let (base, token) = server_of(env)?;
    let req = env
        .http
        .get(format!("{base}/v1/tasks/signals"))
        .bearer_auth(&token)
        .query(&[("timezone", env.time_zone.name())]);
    let s = tokio::time::timeout(std::time::Duration::from_secs(3), send(req))
        .await
        .ok()?
        .ok()?;
    let names = |k: &str| -> Vec<String> {
        s[k].as_array()
            .into_iter()
            .flatten()
            .take(10)
            .map(|t| {
                let c = t["content"].as_str().unwrap_or("?");
                match (t["time"].as_str(), t["days_overdue"].as_i64()) {
                    (_, Some(d)) if d > 0 => format!("{c} ({d}d late)"),
                    (Some(time), _) => format!("{time} {c}"),
                    _ => c.to_owned(),
                }
            })
            .collect()
    };
    let today = names("today_tasks");
    let overdue = names("overdue");
    let mut line = format!(
        "Giuseppe's tasks today ({}): {} done of goal {}",
        s["today"].as_str().unwrap_or(""),
        s["completed_today"].as_u64().unwrap_or(0),
        s["daily_goal"].as_u64().unwrap_or(0)
    );
    if today.is_empty() && overdue.is_empty() {
        line.push_str("; nothing left for today.");
        return Some(line);
    }
    if !today.is_empty() {
        line.push_str(&format!("; to do: {}", today.join("; ")));
    }
    if !overdue.is_empty() {
        line.push_str(&format!("; overdue: {}", overdue.join("; ")));
    }
    if let Some(m) = s["load_minutes"].as_u64().filter(|m| *m > 0) {
        line.push_str(&format!("; about {m} min of work"));
    }
    line.push('.');
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    async fn env(server: &MockServer, dir: &std::path::Path) -> Arc<ToolEnv> {
        let tok = dir.join("svc.token");
        std::fs::write(&tok, "svc_testtoken\n").unwrap();
        let mut e = ToolEnv::new(dir, chrono_tz::Europe::Copenhagen);
        e.secretary_url = Some(server.uri());
        e.secretary_service_token_file = Some(tok);
        Arc::new(e)
    }

    fn view() -> Value {
        json!({"view": {"today": "2026-10-22", "goal": {"daily_goal": 5, "completed_today": 1},
            "counts": {"overdue": 0}, "sync": {"status": "ok"},
            "list": {"kind": "today", "groups": [
                {"key": "overdue", "kind": "overdue", "title": "Overdue", "tasks": [
                    {"id": "a2", "content": "10 min anki/memrise", "project_name": "Lingue", "priority": 2,
                     "labels": ["lingue"], "due": {"date": "2026-10-20", "string": "every day", "is_recurring": true, "rel_days": -2, "overdue": true}}]},
                {"key": "2026-10-22", "kind": "date", "date": "2026-10-22", "title": "", "tasks": [
                    {"id": "a1", "content": "15 min shadowing", "project_name": "Lingue", "priority": 4,
                     "due": {"date": "2026-10-22", "time": "09:00", "string": "every day", "is_recurring": true, "rel_days": 0, "overdue": false}},
                    {"id": "a3", "content": "20min speaking", "project_name": "Lingue", "priority": 4, "labels": []}]}]}}})
    }

    #[tokio::test]
    async fn list_complete_by_words_add_and_reschedule() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/tasks/state"))
            .and(header("authorization", "Bearer svc_testtoken"))
            .and(query_param("timezone", "Europe/Copenhagen"))
            .respond_with(ResponseTemplate::new(200).set_body_json(view()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/tasks/command"))
            .respond_with(|req: &Request| {
                let b: Value = serde_json::from_slice(&req.body).unwrap();
                assert!(b["op_id"].as_str().unwrap().starts_with("lucina-"));
                let parsed = json!({"content": "chiamare Marco", "due_string": "domani alle 9", "due_date": "2026-10-23T09:00", "is_recurring": false, "labels": []});
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "task_id": "tmp-1", "parsed": parsed, "command": b["command"]}))
            })
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let ctx = Ctx::new(e, "telegram", "s");
        let l = TasksTool
            .call(json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert_eq!(l["groups"][0]["group"], "overdue");
        assert_eq!(l["groups"][0]["tasks"][0]["overdue_days"], 2);
        assert_eq!(l["groups"][0]["tasks"][0]["priority"], "p2");
        assert_eq!(l["groups"][1]["tasks"][0]["repeats"], "every day");
        assert_eq!(l["groups"][1]["tasks"][0]["time"], "09:00");
        assert_eq!(l["completed_today"], 1);

        // "fatto lo shadowing"
        let c = TasksTool
            .call(json!({"action": "complete", "query": "shadowing"}), &ctx)
            .await
            .unwrap();
        assert_eq!(c["id"], "a1");
        let a = TasksTool
            .call(
                json!({"action": "add", "text": "chiamare Marco domani alle 9"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(a["due"], "2026-10-23T09:00");
        // ambiguous words list the candidates
        let err = TasksTool
            .call(json!({"action": "complete", "query": "min"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.0.contains("several tasks match"), "{}", err.0);
        let r = TasksTool
            .call(json!({"action": "reschedule"}), &ctx)
            .await
            .unwrap();
        assert_eq!(r["rescheduled"], "all overdue");

        let posts: Vec<Value> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["command"].clone())
            .collect();
        assert_eq!(posts[0], json!({"type": "complete", "task_id": "a1"}));
        assert_eq!(
            posts[1],
            json!({"type": "quick_add", "text": "chiamare Marco domani alle 9"})
        );
        assert_eq!(
            posts[2],
            json!({"type": "reschedule_overdue", "date": "today"})
        );
    }

    #[tokio::test]
    async fn delete_needs_confirmation_and_context_line() {
        assert!(TasksTool.needs_confirmation(&json!({"action": "delete", "query": "x"})));
        for a in ["list", "add", "complete", "reschedule"] {
            assert!(!TasksTool.needs_confirmation(&json!({"action": a})), "{a}");
        }
        assert_eq!(
            TasksTool.summarize(&json!({"action": "delete", "query": "speaking"})),
            "tasks: delete speaking"
        );
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/tasks/signals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "today": "2026-10-22", "completed_today": 1, "daily_goal": 5, "load_minutes": 25,
                "today_tasks": [{"content": "15 min shadowing", "time": "09:00", "days_overdue": 0}],
                "overdue": [{"content": "10 min anki/memrise", "days_overdue": 2}]})))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let line = today_context(&e, Utc::now()).await.unwrap();
        assert_eq!(line, "Giuseppe's tasks today (2026-10-22): 1 done of goal 5; to do: 09:00 15 min shadowing; overdue: 10 min anki/memrise (2d late); about 25 min of work.");
        let none = Ctx::new(
            Arc::new(ToolEnv::new(dir.path(), chrono_tz::UTC)),
            "web",
            "s",
        );
        assert!(TasksTool
            .call(json!({"action": "list"}), &none)
            .await
            .unwrap_err()
            .0
            .contains("not configured"));
    }
}
