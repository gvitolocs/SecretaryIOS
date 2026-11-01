//! The MCP server handler (rmcp `ServerHandler`): tools, resources, prompts.
//!
//! The authenticated [`Caller`] comes from the auth middleware through the
//! HTTP request parts rmcp puts in every request context. Every tool call,
//! resource read and prompt is logged on target `secretary_mcp::audit`
//! (name, caller, duration, ok) without arguments, bodies or tokens.

use std::time::{Duration, Instant};

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation, JsonObject,
    ListPromptsResult, ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
    ProgressNotificationParam, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{json, Value};

use crate::agents;
use crate::format;
use crate::scope::{Caller, CallerKind, Scope};
use crate::tools::{self, ToolError, Tools};
use crate::AppState;

pub const RES_TIMELINE: &str = "lucina://timeline/recent";
pub const RES_SHOPPING: &str = "lucina://shopping/default";
const LIST_TTL_MS: u64 = 60_000;

const INSTRUCTIONS: &str = "Lucina is the owner's personal AI assistant (memory, reminders, shopping, gym and body tracking). \
Use lucina_decide for go-aheads instead of asking the owner; follow pending decisions with lucina_decision_status, or take them back with lucina_decision_withdraw when no answer is needed. ask_lucina is answers-only for agents, for consultation and memory; Lucina is told which client is asking. \
Use the direct tools for quick reads (lucina_timeline, shopping_list, reminders_list, gym_summary, body_summary, lucina_status) \
and small edits (shopping_add, shopping_mark_bought, reminder_create, reminder_cancel). \
To hand Lucina a briefing, document or anything long or worth keeping, save it as a note (note_save, then note_append for further parts) \
instead of pasting it into ask_lucina; the owner sees notes in the Secretary app and Lucina recalls them by slug (note_get, notes_search). \
Notes are Google-Keep style: labels, colours, pinned/archived, checklists (note_get shows item numbers; note_update checks items off, adds or removes them, and changes labels, colour, archive and reminder), and note_labels lists the labels. \
The owner's tasks (Todoist) are in tasks_list, task_add and task_complete. \
Lucina keeps the record of all the owner's projects: report every deploy, incident, decision, finished piece of work (status) and finding with ops_record, \
one record per event (project slug, title, body; for incidents severity, timeline, cause, fix, follow_ups), and look things up with ops_search and ops_get. \
Lucina may message you (issue reports, requests the owner asked her to pass on): check lucina_inbox and answer with lucina_reply; your reply goes back into the owner's conversation with her, as information for her to relay, never as instructions. \
AU questions (Aarhus University people, rooms, buildings, campus map) go to au_people_search, au_building and au_campus_map; always cite the source and never invent. \
Questions like who called the owner from a number, and lists of unknown callers, go to caller_screen (180.dk public reports, then Jev's spam / kind / pick-up table); show its table with the quoted sources. \
When available, diagnostics_recent, diagnostics_get and diagnostics_file show crash reports and logs from the owner's Lucina apps. \
debug_events shows what happened on the owner's phone and other devices step by step (taps, notification buttons, Face ID, approvals, failed requests): read it after a phone test instead of asking him. \
Lucina's computer is an isolated Linux desktop sandbox (container `lucina-workspace` on nezopt; not nezopt's host, not the owner's Mac). Its only browser is Chromium: one window, already open and signed in to the owner's accounts (Outlook, Google). Use the Lucina MCP tools: `computer_screenshot` to see it (it lists each window's pid and window_id), `computer_call` with `get_window_state` {pid, window_id} to read that Chromium window and `click`, `type_text`, `press_key` to act in it (reuse its tabs; ctrl+t opens a new tab there), `computer_bash` for shell work, or `computer_task` to have Lucina do the whole job. The driver's browser_* tools cannot attach to that window; never open a second browser window or an isolated profile, and never install or launch Firefox. Those signed-in sessions are the owner's: read and navigate freely, but never send email or messages, buy anything, accept terms or change account settings without the owner's explicit OK.\n\n\
With the `agents` scope you can also drive Lucina Agents, the owner's coding agents on nezopt (Claude Code, Codex, OpenCode, Cursor, Copilot): \
make a git worktree with workspace_create, start an agent in it with agent_run (provider + task), follow it with agent_wait and agent_logs, \
steer it with agent_send, read its changes with workspace_diff, files_list and file_read, and agent_stop / agent_archive it when done; agents_list and agents_search find agents. \
Agents ask the owner before risky steps: those permission requests (permissions_list) are answered only by the owner in the Secretary app, never through this MCP; \
pushing, opening PRs and merging are the owner's too. Agents you start can't start further agents, and each connection has a budget of running agents and runs per hour. \
Before restarting, stopping, deploying or changing a service, ask the Homelab catalog: catalog_get (where it runs, logs, restart, rollback, runbook), \
catalog_impact (what breaks if it is down), catalog_health (what is unhealthy right now) and catalog_drift.\n\n\
With the `notebooks` scope you can use Lucina Notebooks, the owner's NotebookLM-style workspaces: notebook_list and notebook_get read them, \
notebook_create and notebook_add_source (a link, text, a file on Lucina's desktop or a note) fill them, notebook_ask answers a question only from the sources \
with numbered citations (the text cites [source:<source id>-<n>]: keep those markers), notebook_search finds passages without a model, \
notebook_transform runs a saved prompt on a source and notebook_podcast makes an episode. Sources take a moment to be read after they are added; nothing here deletes anything.\n\n\
Times are in the owner's time zone (see lucina_status). Never use these tools to buy things or contact other people; ask the owner first.";

#[derive(Clone)]
pub struct LucinaMcp {
    app: AppState,
}

impl LucinaMcp {
    pub fn new(app: AppState) -> Self {
        Self { app }
    }
}

fn caller(ctx: &RequestContext<RoleServer>) -> Result<Caller, McpError> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|p| p.extensions.get::<Caller>())
        .cloned()
        .ok_or_else(|| McpError::invalid_request("unauthenticated", None))
}

fn kind(c: &Caller) -> &'static str {
    match c.kind {
        CallerKind::ServiceToken => "token",
        CallerKind::OAuth => "oauth",
    }
}

fn audit(what: &str, name: &str, c: &Caller, started: Instant, ok: bool) {
    tracing::info!(
        target: "secretary_mcp::audit",
        event = what,
        name,
        caller = %c.name,
        auth = kind(c),
        id = %c.id,
        duration_ms = started.elapsed().as_millis() as u64,
        ok,
    );
}

impl LucinaMcp {
    /// Tools of optional backends exist only when the backend is configured.
    fn offered(&self, name: &str) -> bool {
        !name.starts_with("diagnostics_") || self.app.diagnostics.is_some()
    }

    fn insufficient(&self, c: &Caller, need: Scope, name: &str) -> CallToolResult {
        let mut r = CallToolResult::error(vec![ContentBlock::text(format!(
            "This connection ({}) lacks the `{need}` scope needed for {name}. Reconnect with that permission, or issue a token with it.",
            c.name
        ))]);
        if c.kind == CallerKind::OAuth {
            // ChatGPT's tool-level auth challenge (Apps SDK auth docs).
            let challenge = format!(
                "Bearer resource_metadata=\"{}\", error=\"insufficient_scope\", scope=\"{}\"",
                self.app.cfg.resource_metadata_url(),
                c.scopes
                    .iter()
                    .map(|s| s.as_str())
                    .chain([need.as_str()])
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let mut meta = JsonObject::new();
            meta.insert("mcp/www_authenticate".into(), json!([challenge]));
            r.meta = Some(MetaObject::from(meta));
        }
        r
    }

    /// `computer_*` / `android_*` (crate::computer): image results, and progress
    /// notifications while a computer task is awaited.
    async fn run_computer(
        &self,
        name: &str,
        args: &JsonObject,
        c: &Caller,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<Vec<ContentBlock>, ToolError> {
        let t = Tools {
            app: &self.app,
            caller: c,
        };
        let progress = ctx.meta.get_progress_token();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let mut tick = move |secs: u64| {
            let _ = tx.send(secs);
        };
        let fut = t.computer(name, args, &mut tick);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => break r,
                Some(secs) = rx.recv() => {
                    if let Some(p) = progress.clone() {
                        let _ = ctx.peer.notify_progress(
                            ProgressNotificationParam::new(p, secs as f64).with_message("Lucina's computer is working…"),
                        ).await;
                    }
                }
                _ = ctx.ct.cancelled() => break Err(ToolError("cancelled".into())),
            }
        }
    }

    /// Lucina Agents (crate::agents): progress while an agent is awaited, and
    /// one audit line per call in `agents-audit.jsonl`.
    async fn run_agents(
        &self,
        name: &str,
        args: &JsonObject,
        c: &Caller,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<String, ToolError> {
        let t = Tools {
            app: &self.app,
            caller: c,
        };
        let progress = ctx.meta.get_progress_token();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let mut tick = move |secs: u64| {
            let _ = tx.send(secs);
        };
        let fut = t.agents(name, args, &mut tick);
        tokio::pin!(fut);
        let out = loop {
            tokio::select! {
                r = &mut fut => break r,
                Some(secs) = rx.recv() => {
                    if let Some(p) = progress.clone() {
                        let _ = ctx.peer.notify_progress(
                            ProgressNotificationParam::new(p, secs as f64).with_message("The coding agent is working…"),
                        ).await;
                    }
                }
                _ = ctx.ct.cancelled() => break Err(ToolError("cancelled".into())),
            }
        };
        let dir = &self.app.cfg.data_dir;
        match &out {
            Ok(o) => agents::audit(dir, name, c, args, Ok(o.target.as_deref())),
            Err(e) => agents::audit(dir, name, c, args, Err(&e.0)),
        }
        out.map(|o| o.text)
    }

    async fn run_tool(
        &self,
        name: &str,
        args: &JsonObject,
        c: &Caller,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<String, ToolError> {
        let t = Tools {
            app: &self.app,
            caller: c,
        };
        match name {
            "ask_lucina" => {
                // Keep the client informed during a long LLM turn.
                let progress = ctx.meta.get_progress_token();
                let fut = t.ask(args);
                tokio::pin!(fut);
                let mut tick = tokio::time::interval(Duration::from_secs(10));
                tick.tick().await;
                let mut n = 0.0;
                loop {
                    tokio::select! {
                        r = &mut fut => break r,
                        _ = tick.tick() => {
                            if let Some(p) = progress.clone() {
                                n += 1.0;
                                let _ = ctx.peer.notify_progress(
                                    ProgressNotificationParam::new(p, n).with_message("Lucina is thinking…"),
                                ).await;
                            }
                        }
                        _ = ctx.ct.cancelled() => break Err(ToolError("cancelled".into())),
                    }
                }
            }
            "notebook_ask" => {
                // A grounded answer can take two minutes: keep the client informed.
                let progress = ctx.meta.get_progress_token();
                let fut = t.notebook_ask(args);
                tokio::pin!(fut);
                let mut tick = tokio::time::interval(Duration::from_secs(10));
                tick.tick().await;
                let mut n = 0.0;
                loop {
                    tokio::select! {
                        r = &mut fut => break r,
                        _ = tick.tick() => {
                            if let Some(p) = progress.clone() {
                                n += 1.0;
                                let _ = ctx.peer.notify_progress(
                                    ProgressNotificationParam::new(p, n).with_message("The notebook is answering…"),
                                ).await;
                            }
                        }
                        _ = ctx.ct.cancelled() => break Err(ToolError("cancelled".into())),
                    }
                }
            }
            name if name.starts_with("notebook_") => t.notebooks(name, args).await,
            "lucina_decide" => t.decide(args).await,
            "lucina_decision_status" => t.decision_status(args).await,
            "lucina_decision_withdraw" => t.decision_withdraw(args).await,
            "lucina_job_status" => t.job_status(args).await,
            "lucina_timeline" => t.timeline(args).await,
            "shopping_list" => t.shopping_list(args).await,
            "shopping_add" => t.shopping_add(args).await,
            "shopping_mark_bought" => t.mark_bought(args).await,
            "reminders_list" => t.reminders(args).await,
            "reminder_create" => t.reminder_create(args).await,
            "reminder_cancel" => t.reminder_cancel(args).await,
            "calendar_list" => t.calendar_list(args).await,
            "calendar_create" => t.calendar_create(args).await,
            "gym_summary" => t.gym(args).await,
            "body_summary" => t.body().await,
            "lucina_status" => t.status().await,
            "note_save" => t.note_save(args).await,
            "note_append" => t.note_append(args).await,
            "note_update" => t.note_update(args).await,
            "note_get" => t.note_get(args).await,
            "notes_search" => t.notes_search(args).await,
            "note_labels" => t.note_labels().await,
            name if name.starts_with("library_") => t.library(name, args).await,
            "tasks_list" => t.tasks_list(args).await,
            "task_add" => t.task_add(args).await,
            "task_complete" => t.task_complete(args).await,
            name if name.starts_with("ops_") => t.ops(name, args).await,
            "lucina_inbox" | "lucina_reply" => t.reach(name, args).await,
            name if name.starts_with("diagnostics_") => t.diagnostics(name, args).await,
            "debug_events" => t.debug_events(args).await,
            _ => Err(ToolError(format!("unknown tool {name}"))),
        }
    }

    async fn resource_text(&self, uri: &str, c: &Caller) -> Result<String, McpError> {
        let t = Tools {
            app: &self.app,
            caller: c,
        };
        let r = match uri {
            RES_TIMELINE => t.timeline(&JsonObject::new()).await,
            RES_SHOPPING => t.shopping_list(&JsonObject::new()).await,
            _ => {
                return Err(McpError::resource_not_found(
                    "resource_not_found",
                    Some(json!({"uri": uri})),
                ))
            }
        };
        r.map_err(|e| McpError::internal_error(e.0, None))
    }

    async fn briefing(&self, date: Option<&str>) -> Result<GetPromptResult, McpError> {
        let tz = self.app.cfg.timezone;
        let mut q = Vec::new();
        if let Some(d) = date {
            if chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err() {
                return Err(McpError::invalid_params("date must be YYYY-MM-DD", None));
            }
            q.push(("date", d.to_owned()));
        }
        let text = match self.app.backend.get("/v1/agenda", &q).await {
            Ok(a) => {
                let day = a
                    .get("date")
                    .and_then(Value::as_str)
                    .unwrap_or("today")
                    .to_owned();
                let reminders = format::reminders(
                    &json!({"reminders": a.get("reminders").cloned().unwrap_or(json!([])), "timeZone": a.get("timeZone")}),
                    tz,
                );
                let briefing = a
                    .get("briefing")
                    .and_then(Value::as_str)
                    .unwrap_or("(Lucina's briefing is unavailable right now.)");
                format!(
                    "Here is my Secretary agenda for {day}.\n\nLucina's briefing:\n{briefing}\n\nReminders that day:\n{reminders}\n\n\
                     Give me my daily briefing: the plan for the day in time order, what to prepare, and anything I should not forget. Keep it short."
                )
            }
            Err(e) => {
                tracing::info!(code = %e.code, status = e.status, "agenda unavailable; briefing from reminders");
                let rem = self
                    .app
                    .backend
                    .get("/v1/reminders", &[("scope", "today".to_owned())])
                    .await
                    .map(|r| format::reminders(&r, tz))
                    .unwrap_or_else(|e| format!("(reminders unavailable: {e})"));
                format!(
                    "Lucina's agenda is not available, but here are today's reminders ({}):\n{rem}\n\n\
                     Give me a short daily briefing from these: the plan in time order and what to prepare.",
                    format::now_line(tz)
                )
            }
        };
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description("Daily briefing from Lucina's agenda"),
        )
    }
}

impl ServerHandler for LucinaMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(
            Implementation::new("lucina-mcp", crate::VERSION)
                .with_title("Lucina")
                .with_description("The owner's personal AI, Lucina, and her Secretary tools"),
        )
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let c = caller(&ctx)?;
        let tools: Vec<Tool> = tools::specs()
            .iter()
            .filter(|s| s.allowed(|x| c.has(x)) && self.offered(s.name))
            .map(|s| s.tool())
            .collect();
        Ok(ListToolsResult::with_all_items(tools)
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::spec(name)
            .filter(|s| self.offered(s.name))
            .map(|s| s.tool())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let c = caller(&ctx)?;
        let name = request.name.to_string();
        let Some(spec) = tools::spec(&name).filter(|s| self.offered(s.name)) else {
            audit("tool_call", &name, &c, started, false);
            return Err(McpError::invalid_params(
                format!("unknown tool {name}"),
                None,
            ));
        };
        if !spec.allowed(|x| c.has(x)) {
            audit("tool_call", &name, &c, started, false);
            tracing::warn!(target: "secretary_mcp::audit", event = "scope_denied", name = %name, caller = %c.name, need = %spec.scope);
            if spec.scope == Scope::Agents {
                let args = request.arguments.unwrap_or_default();
                agents::audit(
                    &self.app.cfg.data_dir,
                    &name,
                    &c,
                    &args,
                    Err("scope_denied"),
                );
            }
            return Ok(self.insufficient(&c, spec.scope, &name).into());
        }
        let args = request.arguments.unwrap_or_default();
        let out = if spec.scope == Scope::Computer {
            self.run_computer(&name, &args, &c, &ctx).await
        } else if name == "caller_screen" {
            // Caller screening (crate::callers): Lucina's table plus raw JSON.
            Tools {
                app: &self.app,
                caller: &c,
            }
            .callers(&args)
            .await
        } else if name.starts_with("au_") {
            // AU Find (crate::au): text plus raw JSON, or a map image.
            Tools {
                app: &self.app,
                caller: &c,
            }
            .au(&name, &args)
            .await
        } else if spec.scope == Scope::Agents {
            self.run_agents(&name, &args, &c, &ctx)
                .await
                .map(|text| vec![ContentBlock::text(text)])
        } else {
            self.run_tool(&name, &args, &c, &ctx)
                .await
                .map(|text| vec![ContentBlock::text(text)])
        };
        audit("tool_call", &name, &c, started, out.is_ok());
        Ok(match out {
            Ok(blocks) => CallToolResult::success(blocks),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.0)]),
        }
        .into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let c = caller(&ctx)?;
        let items = if c.has(Scope::Read) {
            vec![
                Resource::new(RES_TIMELINE, "timeline-recent")
                    .with_title("Lucina timeline (recent)")
                    .with_description(
                        "The latest 20 messages between the owner and Lucina on all channels",
                    )
                    .with_mime_type("text/plain"),
                Resource::new(RES_SHOPPING, "shopping-default")
                    .with_title("Shopping list")
                    .with_description(
                        "The default shopping list: what is needed and what was bought recently",
                    )
                    .with_mime_type("text/plain"),
            ]
        } else {
            Vec::new()
        };
        Ok(ListResourcesResult::with_all_items(items)
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let started = Instant::now();
        let c = caller(&ctx)?;
        let uri = request.uri.clone();
        if !c.has(Scope::Read) {
            audit("resource_read", &uri, &c, started, false);
            return Err(McpError::invalid_request(
                "the read scope is required",
                None,
            ));
        }
        let r = self.resource_text(&uri, &c).await;
        audit("resource_read", &uri, &c, started, r.is_ok());
        let text = r?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("text/plain")
        ])
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
        .into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let c = caller(&ctx)?;
        let items = if c.has(Scope::Read) {
            vec![Prompt::new(
                "daily_briefing",
                Some(
                    "Your day from Lucina's agenda: reminders and her briefing, ready to summarise",
                ),
                Some(vec![PromptArgument::new("date")
                    .with_description("YYYY-MM-DD (default today, in the owner's time zone)")
                    .with_required(false)]),
            )
            .with_title("Daily briefing")]
        } else {
            Vec::new()
        };
        Ok(ListPromptsResult::with_all_items(items)
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let started = Instant::now();
        let c = caller(&ctx)?;
        if request.name != "daily_briefing" {
            return Err(McpError::invalid_params(
                format!("unknown prompt {}", request.name),
                None,
            ));
        }
        if !c.has(Scope::Read) {
            audit("prompt_get", &request.name, &c, started, false);
            return Err(McpError::invalid_request(
                "the read scope is required",
                None,
            ));
        }
        let date = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("date"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_owned);
        let r = self.briefing(date.as_deref()).await;
        audit("prompt_get", "daily_briefing", &c, started, r.is_ok());
        Ok(r?.into())
    }
}
