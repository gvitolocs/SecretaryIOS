//! `meshy`: 3D models with Meshy AI (crate `lucina-meshy`).
//!
//! - `plan` spends nothing: steps, clips, formats and the credit total, plus
//!   today's budget. Lucina shows it and asks "Good to go?".
//! - `text_to_3d`, `image_to_3d`, `remesh`, `texture`, `rig`, `animate`,
//!   `export` spend credits: the call becomes a **pending action** (the
//!   owner's explicit yes, in the app or with the Telegram button) and only
//!   then starts a background job. A plan over the per-plan cap or today's
//!   budget is refused before anyone is asked.
//! - `status` (a job) and `list` (the gallery) are reads.
//! - MCP callers (ChatGPT, Grok, Claude) may only `plan` and `list`.
//!
//! Registered by the server only when `LUCINA_MESHY=true`.

use std::sync::Arc;

use lucina_meshy::{Action, Args, MeshyService, Origin};
use serde_json::{json, Value};

use crate::{Ctx, Origin as TurnOrigin, SideEffect, Tool, ToolError, ToolResult};

pub struct MeshyTool {
    svc: Arc<MeshyService>,
}

impl MeshyTool {
    pub fn new(svc: Arc<MeshyService>) -> Self {
        Self { svc }
    }
}

fn spends(args: &Value) -> bool {
    args["action"]
        .as_str()
        .and_then(Action::parse)
        .is_some_and(Action::spends)
}

#[async_trait::async_trait]
impl Tool for MeshyTool {
    fn name(&self) -> &'static str {
        "meshy"
    }
    fn description(&self) -> &'static str {
        "Make 3D models with Meshy AI: from text or a photo, then remesh (poly count, quad/tri), retexture, \
         rig a humanoid, animate it with library clips (in-place walks and runs by default) and export \
         (GLB always; FBX, USDZ, OBJ, STL for 3D printing, 3MF). Models land in Giuseppe's private gallery. \
         ALWAYS call action=plan first and show the plan (steps, clips with their ids, credits, today's budget) \
         and ask whether to go ahead; spending actions then wait for his explicit yes. Jobs run in the \
         background (2-5 minutes per step) and he is notified when the model is ready; do not claim it is done."
    }
    fn parameters(&self) -> Value {
        crate::schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::External
    }
    fn needs_confirmation(&self, args: &Value) -> bool {
        // A plan over the limits is refused by `call` without asking anyone.
        spends(args)
            && Args::from_value(args)
                .ok()
                .and_then(|a| self.svc.estimate(&a).ok())
                .is_some_and(|p| self.svc.check(&p).is_ok())
    }
    fn summarize(&self, args: &Value) -> String {
        match Args::from_value(args)
            .ok()
            .and_then(|a| self.svc.estimate(&a).ok())
        {
            Some(p) if spends(args) => p.summary(),
            _ => format!("meshy {}", args["action"].as_str().unwrap_or("?")),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a = Args::from_value(&args).map_err(ToolError::new)?;
        let action = a.action().map_err(ToolError::new)?;
        if ctx.origin == TurnOrigin::Mcp && !action.mcp_allowed() {
            return Err(ToolError::new(
                "refused: via Lucina MCP, Meshy can only plan and list. Spending credits needs Giuseppe's own yes in the Secretary app or on Telegram.",
            ));
        }
        let svc = &self.svc;
        match action {
            Action::List => {
                let models = svc
                    .list(a.limit.unwrap_or(10))
                    .await
                    .map_err(ToolError::new)?;
                Ok(json!({"models": models, "count": models.len()}))
            }
            Action::Status => {
                let id = a
                    .job_id
                    .as_deref()
                    .ok_or_else(|| ToolError::new("job_id is required"))?;
                svc.job(id)
                    .ok_or_else(|| ToolError::new("no Meshy job with that id"))
            }
            Action::Plan => {
                let plan = svc.plan(&a).await.map_err(ToolError::new)?;
                let limit = svc.check(&plan).err().map(|e| e.to_string());
                Ok(json!({
                    "plan": plan.to_json(),
                    "text": plan.text(),
                    "budget": svc.budget(),
                    "withinLimits": limit.is_none(),
                    "limit": limit,
                    "spent": 0,
                    "next": if limit.is_none() {
                        "Show this plan (steps, clip ids, credits) and ask whether to go ahead. If he says yes, call meshy again with the same arguments and action set to the planned action."
                    } else {
                        "Over the limit: say so and suggest a cheaper plan (fewer clips, low_poly, no textures). Nothing was spent."
                    },
                }))
            }
            _ => {
                let plan = svc.plan(&a).await.map_err(ToolError::new)?;
                svc.check(&plan)
                    .map_err(|e| ToolError::new(e.to_string()))?;
                if !ctx.confirmed {
                    return Err(ToolError::new(
                        "not started: spending Meshy credits needs Giuseppe's explicit yes first",
                    ));
                }
                let job = svc
                    .start(
                        plan.clone(),
                        Origin {
                            channel: ctx.channel.clone(),
                            session_id: ctx.session_id.clone(),
                            reply_to: None,
                            lang: "en".into(),
                        },
                    )
                    .await
                    .map_err(ToolError::new)?;
                Ok(json!({
                    "status": "started",
                    "jobId": job.id,
                    "plan": plan.to_json(),
                    "instruction": "The job runs in the background (a few minutes per step). Say it started and that he will be notified with the model; do not claim it is done.",
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use lucina_llm::{FunctionCall, ToolCall};
    use lucina_meshy::budget::Limits;
    use lucina_meshy::key::{Secret, StaticKey};
    use lucina_meshy::mock::{self, MockConfig};
    use lucina_meshy::{MeshyConfig, MeshyService};
    use lucina_store::{JobStore, TimelineStore};
    use serde_json::json;

    use super::MeshyTool;
    use crate::pending::{PendingStore, DEFAULT_TTL};
    use crate::{Ctx, Registry, Tool, ToolEnv};

    async fn tool(dir: &std::path::Path, limits: Limits) -> (Arc<MeshyTool>, mock::MockHandle) {
        let m = mock::spawn(MockConfig {
            assets: dir.to_owned(),
            polls_to_finish: 1,
            ..MockConfig::default()
        })
        .await
        .unwrap();
        let get = |k: &str| (k == "LUCINA_MESHY").then(|| "true".to_owned());
        let mut cfg = MeshyConfig::from_lookup(&get, dir, chrono_tz::Europe::Rome, None, None);
        cfg.api_base = m.api_base();
        cfg.limits = limits;
        cfg.poll = Duration::from_millis(5);
        cfg.poll_min = Duration::from_millis(1);
        cfg.preview.enabled = false;
        let svc = MeshyService::new(
            cfg,
            Arc::new(StaticKey(Secret::new("msy_tool_test_key_000"))),
            Arc::new(JobStore::new(dir.join("jobs.json"))),
            Arc::new(TimelineStore::new(
                dir.join("timeline.jsonl"),
                1 << 20,
                true,
            )),
        );
        (Arc::new(MeshyTool::new(Arc::new(svc))), m)
    }

    fn ctx(dir: &std::path::Path) -> Ctx {
        Ctx::new(
            Arc::new(ToolEnv::new(dir, chrono_tz::Europe::Rome)),
            "web",
            "secretary_app",
        )
    }

    const KNIGHT: &str =
        r#"{"action": "text_to_3d", "prompt": "a knight", "animations": ["walk", "wave"]}"#;

    #[tokio::test]
    async fn plan_is_free_and_spending_waits_for_a_yes() {
        let dir = tempfile::tempdir().unwrap();
        let (t, m) = tool(dir.path(), Limits::new(60, 60)).await;
        let knight: serde_json::Value = serde_json::from_str(KNIGHT).unwrap();
        assert!(t.needs_confirmation(&knight));
        assert!(!t.needs_confirmation(&json!({"action": "plan", "prompt": "a knight"})));
        assert!(!t.needs_confirmation(&json!({"action": "list"})));
        assert_eq!(t.summarize(&knight), "meshy text_to_3d \"A knight\": preview 20 + refine 10 + rig 5 + animate 6 = 41 credits");
        let c = ctx(dir.path());
        let mut plan_args = knight.clone();
        plan_args["action"] = json!("plan");
        plan_args["for_action"] = json!("text_to_3d");
        let p = t.call(plan_args, &c).await.unwrap();
        assert_eq!(p["plan"]["totalCredits"], 41);
        assert_eq!(
            p["plan"]["clips"][0],
            json!({"actionId": 613, "name": "Casual Walk", "inPlace": true})
        );
        assert_eq!(p["withinLimits"], true);
        assert_eq!(p["budget"]["leftToday"], 60);
        // unconfirmed spending never reaches Meshy
        let err = t.call(knight.clone(), &c).await.unwrap_err();
        assert!(err.0.contains("explicit yes"), "{err}");
        assert_eq!(m.creates("/openapi/v2/text-to-3d"), 0);
        // confirmed: a background job starts
        let ok = t.call(knight, &ctx(dir.path()).confirmed()).await.unwrap();
        assert_eq!(ok["status"], "started");
        assert!(ok["jobId"].is_string());
    }

    #[tokio::test]
    async fn over_the_cap_is_refused_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let (t, m) = tool(dir.path(), Limits::new(60, 30)).await;
        let knight: serde_json::Value = serde_json::from_str(KNIGHT).unwrap();
        assert!(
            !t.needs_confirmation(&knight),
            "no pending action for a plan over the cap"
        );
        let err = t
            .call(knight, &ctx(dir.path()).confirmed())
            .await
            .unwrap_err();
        assert!(err.0.contains("30-credit limit"), "{err}");
        assert_eq!(m.creates("/openapi/v2/text-to-3d"), 0);
    }

    #[tokio::test]
    async fn mcp_callers_can_only_plan_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let (t, m) = tool(dir.path(), Limits::new(60, 60)).await;
        let mcp = || ctx(dir.path()).with_origin(crate::Origin::Mcp).confirmed();
        let knight: serde_json::Value = serde_json::from_str(KNIGHT).unwrap();
        for a in [knight.clone(), json!({"action": "status", "job_id": "x"})] {
            let err = t.call(a, &mcp()).await.unwrap_err();
            assert!(err.0.contains("only plan and list"), "{err}");
        }
        assert!(t
            .call(json!({"action": "plan", "prompt": "a knight"}), &mcp())
            .await
            .is_ok());
        // through the agent loop: refused, and no pending action to confirm later
        let mut reg = Registry::new();
        reg.register(t.clone()).unwrap();
        let pending = PendingStore::memory(DEFAULT_TTL);
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "meshy".into(),
                arguments: knight.to_string(),
            },
        };
        let c = ctx(dir.path()).with_origin(crate::Origin::Mcp);
        let rec = crate::agent::handle_call_for_test(&reg, &call, &c, &pending).await;
        assert!(!rec.ok);
        assert!(rec.output["error"].as_str().unwrap().contains("Lucina MCP"));
        assert!(pending.current("secretary_app", c.now).is_none());
        assert_eq!(m.creates("/openapi/v2/text-to-3d"), 0);
        // the owner's own turn gets a pending action instead
        let own = ctx(dir.path());
        let rec = crate::agent::handle_call_for_test(&reg, &call, &own, &pending).await;
        assert!(rec.pending.is_some());
        assert_eq!(rec.output["status"], "awaiting_confirmation");
        assert!(rec.output["action"]
            .as_str()
            .unwrap()
            .contains("41 credits"));
    }
}
