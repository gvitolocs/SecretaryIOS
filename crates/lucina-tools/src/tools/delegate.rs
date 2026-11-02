//! `ask_claude`, `ask_grok`, `ask_chatgpt` and the router `delegate`:
//! Lucina hands work to another AI agent on her own (crate
//! `lucina-delegate`). No owner confirmation: her self-review decides, the
//! hard limits hold (no secrets out, no money, depth 1, never from Lucina
//! MCP, code only in a sandbox). The tool waits a few seconds for a quick
//! answer; otherwise the answer arrives as a timeline card.

use lucina_delegate::engine::chat_result;
use lucina_delegate::{AgentId, Kind, Origin, Request, StartOutcome};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// The delegation tools (never offered on Lucina MCP turns).
pub const DELEGATION_TOOLS: &[&str] = &["ask_claude", "ask_grok", "ask_chatgpt", "delegate"];

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum KindArg {
    Question,
    Research,
    Analysis,
    Code,
}

impl From<KindArg> for Kind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Question => Kind::Question,
            KindArg::Research => Kind::Research,
            KindArg::Analysis => Kind::Analysis,
            KindArg::Code => Kind::Code,
        }
    }
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ToArg {
    Auto,
    Claude,
    Grok,
    Chatgpt,
}

#[derive(Deserialize, JsonSchema)]
struct AskArgs {
    /// The task, self-contained, with only the details the agent needs (no
    /// private data beyond that, never passwords or secrets). In English
    /// (or Danish for a Danish task); the answer comes back in English or Danish.
    task: String,
    /// question (default), research, analysis or code.
    kind: Option<KindArg>,
    /// Code only: a repo under /home/nez/Projects to work on (as a sandbox
    /// clone on branch lucina/<job>; never main).
    workspace: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct DelegateArgs {
    /// auto (default: Grok for live web and X/news research, Claude for code
    /// and long analysis, ChatGPT for general second opinions), or an agent.
    to: Option<ToArg>,
    /// The task, self-contained and minimal (no secrets, no private data
    /// the agent does not need).
    task: String,
    /// question, research, analysis or code.
    kind: KindArg,
    /// Code only: a repo under /home/nez/Projects (worked on as a sandbox clone).
    workspace: Option<String>,
}

async fn run(
    ctx: &Ctx,
    to: Option<AgentId>,
    kind: Kind,
    task: String,
    workspace: Option<String>,
) -> ToolResult {
    // Hard limit: outside agents calling Lucina never make her delegate.
    if ctx.origin == crate::Origin::Mcp {
        return Err(ToolError::new(
            "refused: requests that come through Lucina MCP cannot start a delegation",
        ));
    }
    let Some(d) = ctx.env.delegator.clone() else {
        return Err(ToolError::new(
            "delegation is not enabled on this host (LUCINA_DELEGATE)",
        ));
    };
    let req = Request {
        to,
        kind,
        task,
        workspace: workspace.filter(|w| !w.trim().is_empty()),
        origin: Origin::Owner,
        depth: 0,
        caller: None,
        session_id: ctx.session_id.clone(),
        channel: ctx.channel.clone(),
        lang: None,
        resume_job: None,
    };
    let lang = lucina_delegate::types::guess_lang(&req.task);
    // Leave the turn time to answer: wait at most 20 s here.
    let wait = d.inline_wait().min(std::time::Duration::from_secs(20));
    match d.start(req).await {
        StartOutcome::Refused(r) => Ok(json!({
            "status": "refused",
            "code": r.code(),
            "reason": r.message(lang),
            "instruction": "Tell the owner in one sentence why it was not delegated; answer yourself if you can."
        })),
        StartOutcome::Skipped(job) => {
            let mut v = chat_result(&job);
            v["instruction"] =
                json!("You decided not to delegate (see why). Answer yourself if you can.");
            Ok(v)
        }
        StartOutcome::Started(mut s) => match s.wait(wait).await {
            Some(job) => {
                let mut v = chat_result(&job);
                v["instruction"] = json!(if job.note_slug.is_some() {
                    "Relay the answer briefly, say which agent gave it, and that the full answer is in Notes."
                } else {
                    "Relay the answer, saying which agent gave it."
                });
                Ok(v)
            }
            None => {
                let mut v = chat_result(&s.job);
                v["status"] = json!("running");
                v["instruction"] = json!("Still working: tell the owner you asked this agent and the answer will appear in the chat when ready. Do not invent the answer.");
                Ok(v)
            }
        },
    }
}

fn kind_or(k: Option<KindArg>, default: Kind) -> Kind {
    k.map(Kind::from).unwrap_or(default)
}

fn summarize(name: &str, args: &Value) -> String {
    let task: String = args["task"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    format!("{name} {task}").trim().to_owned()
}

macro_rules! ask_tool {
    ($ty:ident, $name:literal, $agent:expr, $default_kind:expr, $desc:literal) => {
        pub struct $ty;

        #[async_trait::async_trait]
        impl Tool for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn description(&self) -> &'static str {
                $desc
            }
            fn parameters(&self) -> Value {
                schema_for::<AskArgs>()
            }
            fn side_effect(&self) -> SideEffect {
                SideEffect::Delegate
            }
            fn summarize(&self, args: &Value) -> String {
                summarize($name, args)
            }
            async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
                let a: AskArgs = super::args(args)?;
                run(
                    ctx,
                    Some($agent),
                    kind_or(a.kind, $default_kind),
                    a.task,
                    a.workspace,
                )
                .await
            }
        }
    };
}

ask_tool!(
    AskClaudeTool,
    "ask_claude",
    AgentId::Claude,
    Kind::Analysis,
    "Hand a task to Claude (Claude Code, Opus) on your own: code (in an isolated sandbox, or a sandbox clone of a repo on branch lucina/<job>, never main) and long, careful analysis. Decide yourself; no confirmation is needed. Send only what the task needs."
);
ask_tool!(
    AskGrokTool,
    "ask_grok",
    AgentId::Grok,
    Kind::Research,
    "Hand a question to Grok on your own: live web and X (Twitter) research, current news, what people say right now. Decide yourself; no confirmation is needed. Send only what the task needs."
);
ask_tool!(
    AskChatgptTool,
    "ask_chatgpt",
    AgentId::Chatgpt,
    Kind::Question,
    "Ask ChatGPT on your own for a general answer or a second opinion (it can search the web for research). Decide yourself; no confirmation is needed. Send only what the task needs."
);

pub struct DelegateTool;

#[async_trait::async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &'static str {
        "delegate"
    }
    fn description(&self) -> &'static str {
        "Delegate work to the best other AI agent, on your own judgement: Grok for live web and X/news research, Claude for code and long analysis, ChatGPT for general second opinions (to: auto picks). Use it when another agent does it better than you (fresh facts you do not have, code, deep analysis). No confirmation is needed; your self-review runs before the call and goes on the timeline card. Never for purchases or money, never with secrets."
    }
    fn parameters(&self) -> Value {
        schema_for::<DelegateArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Delegate
    }
    fn summarize(&self, args: &Value) -> String {
        summarize("delegate", args)
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: DelegateArgs = super::args(args)?;
        let to = match a.to.unwrap_or(ToArg::Auto) {
            ToArg::Auto => None,
            ToArg::Claude => Some(AgentId::Claude),
            ToArg::Grok => Some(AgentId::Grok),
            ToArg::Chatgpt => Some(AgentId::Chatgpt),
        };
        run(ctx, to, a.kind.into(), a.task, a.workspace).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use lucina_delegate::agents::{Agent, AgentJob, AgentReply};
    use lucina_delegate::engine::NullSink;
    use lucina_delegate::{DelegateConfig, Delegator};
    use serde_json::json;

    use super::*;
    use crate::agent::{call_gate, CallGate};
    use crate::{Registry, ToolEnv, ToolSelection, NO_MCP_TOOLS};

    struct Echo(Arc<Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl Agent for Echo {
        fn id(&self) -> AgentId {
            AgentId::Grok
        }
        fn supports(&self, k: Kind) -> bool {
            k != Kind::Code
        }
        async fn backend(&self) -> Result<String, String> {
            Ok("mock".into())
        }
        async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
            self.0.lock().unwrap().push(job.prompt.clone());
            Ok(AgentReply {
                text: "Rust 1.95 è uscito.".into(),
                session: None,
                turns: 1,
                backend: "mock".into(),
            })
        }
    }

    fn ctx(dir: &std::path::Path, origin: crate::Origin) -> (Ctx, Arc<Mutex<Vec<String>>>) {
        let mut cfg = DelegateConfig::load(None, dir).unwrap();
        cfg.sandbox_root = dir.join("sb");
        cfg.isolation = lucina_delegate::config::Isolation::None;
        cfg.judge = false;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let d = Delegator::new(
            cfg,
            vec![Arc::new(Echo(seen.clone()))],
            None,
            Arc::new(NullSink),
            chrono_tz::Europe::Rome,
        );
        let mut env = ToolEnv::new(dir, chrono_tz::Europe::Rome);
        env.delegator = Some(Arc::new(d));
        (
            Ctx::new(Arc::new(env), "web", "s1").with_origin(origin),
            seen,
        )
    }

    #[tokio::test]
    async fn asks_without_confirmation_and_relays_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (c, seen) = ctx(dir.path(), crate::Origin::Owner);
        let out = AskGrokTool
            .call(json!({"task": "ultime notizie su Rust"}), &c)
            .await
            .unwrap();
        assert_eq!(out["status"], "done");
        assert_eq!(out["agent"], "Grok");
        assert_eq!(out["answer"], "Rust 1.95 è uscito.");
        assert_eq!(seen.lock().unwrap().len(), 1);
        // no pending action: delegation is Lucina's own decision
        let reg = Registry::builtin(&ToolSelection::All);
        for name in DELEGATION_TOOLS {
            assert_eq!(reg.get(name).unwrap().side_effect(), SideEffect::Delegate);
            assert_eq!(
                call_gate(
                    &reg,
                    name,
                    &json!({"task": "x", "kind": "question"}),
                    crate::Origin::Owner
                ),
                CallGate::Execute,
                "{name}"
            );
        }
        let out = DelegateTool
            .call(
                json!({"task": "compra le cuffie Sony", "kind": "question"}),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(out["status"], "refused");
        assert_eq!(out["code"], "money");
    }

    #[tokio::test]
    async fn never_from_lucina_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let (c, seen) = ctx(dir.path(), crate::Origin::Mcp);
        let err = DelegateTool
            .call(json!({"task": "news", "kind": "research"}), &c)
            .await
            .unwrap_err();
        assert!(err.0.contains("Lucina MCP"));
        assert!(seen.lock().unwrap().is_empty());
        let mcp = Registry::builtin(&ToolSelection::All).without(NO_MCP_TOOLS);
        for name in DELEGATION_TOOLS {
            assert!(mcp.get(name).is_none(), "{name} offered on MCP turns");
        }
        let _ = Duration::ZERO;
    }
}
