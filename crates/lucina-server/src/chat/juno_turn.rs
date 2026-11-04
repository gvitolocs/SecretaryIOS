//! Juno's tools on the JSON planner (the live default), for every owner turn
//! (Juno's Chat, the iPhone, web, Mac, S20; not MCP, guarded or consult turns)
//! once a Juno device has polled since Lucina started. In tools mode they are
//! in her registry like every other tool; the JSON planner does not know them, so:
//!
//! - a pending `juno_*` action (`delete_file`, `write_file` with `overwrite`,
//!   `wifi_connect`) is settled by this turn: yes runs it, no drops it;
//! - otherwise Jev's first-tool guess over her owner registry decides: when it
//!   names a `juno_*` tool, a tool loop (at most [`MAX_ROUNDS`]) runs with the
//!   Juno tools. In a Juno Chat session, or right after a turn that used them
//!   ([`PREVIOUS_WINDOW`]), Jev's top `juno_*` choice runs at any confidence;
//!   elsewhere from `LUCINA_JEV_TOOL_MIN`. Jev's state holds facts only: the
//!   device and the previous turn's juno_* calls and how it ended. No message
//!   text is matched; without Jev the turn goes on as itself, and with no Juno
//!   device ever seen Jev is not asked at all. An offline laptop's error
//!   reaches her through the tool, and she says so.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use lucina_llm::Message;
use lucina_store::timeline::NewItem;
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::pending::{classify_reply, Reply};
use lucina_tools::tools::juno::{device_of_session, NAMES};
use lucina_tools::{Ctx, Origin, Registry};

use super::{effects_to_finish, Finish, Turn, JUNO_SYSTEM};
use crate::api::{PLATFORM_LABEL, USER_LABEL};
use crate::jev_route::TopTool;
use crate::planner::TurnInput;
use crate::prompts::Prompts;

const GUIDE: &str = "You have Juno's tools: juno_tools lists what the laptop can do now, juno_screenshot shows its screen, juno_call runs one driver tool (apps, input, files, status, settings), juno_bash one shell command and juno_events reads what the laptop reported. A call that answers \"waiting on decision <id>\" has not run: tell the owner it waits for his Go. Report what the tools actually returned; never invent the laptop's state. If Juno is offline, say so.";

/// Jev's guess may take at most this long.
const ROUTE_BUDGET: Duration = Duration::from_secs(4);

/// Model calls of one Juno tool loop.
const MAX_ROUNDS: usize = 10;

/// A previous turn with juno_* calls counts for this long.
const PREVIOUS_WINDOW: chrono::Duration = chrono::Duration::minutes(15);

/// Jev's device fact for a Juno Chat session.
const DEVICE_FACT: &str = "the owner wrote this in Juno's Chat, on his laptop Juno";

/// The last round's note when the loop is about to end at [`MAX_ROUNDS`].
const ROUND_LIMIT_NOTE: &str = "This is the last step of this turn: no more juno_* calls can run now. Tell the owner plainly which of his requests the juno_* calls above did, and which have not been done yet.";

/// The juno_* calls of a session's last Lucina turn that had any.
#[derive(Debug, Clone, PartialEq)]
struct Previous {
    at: DateTime<Utc>,
    /// (tool, ok) in call order.
    calls: Vec<(String, bool)>,
    /// "finished", "round limit", "time limit" or "failed".
    ended: &'static str,
}

static PREVIOUS: LazyLock<Mutex<HashMap<String, Previous>>> = LazyLock::new(Default::default);

/// The session's previous turn, when it used juno_* tools in the last [`PREVIOUS_WINDOW`].
fn previous(session: &str, now: DateTime<Utc>) -> Option<Previous> {
    let map = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    map.get(session)
        .filter(|p| now - p.at <= PREVIOUS_WINDOW)
        .cloned()
}

/// Remember this turn's juno_* calls (`None` or no calls: this turn had none).
fn remember(session: &str, turn: Option<Previous>, now: DateTime<Utc>) {
    let mut map = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, p| now - p.at <= PREVIOUS_WINDOW);
    match turn.filter(|p| !p.calls.is_empty()) {
        Some(p) => {
            map.insert(session.to_owned(), p);
        }
        None => {
            map.remove(session);
        }
    }
}

/// The facts joining the message in Jev's state: nothing guessed, no hints.
fn facts(juno_session: bool, previous: Option<&Previous>) -> Value {
    let mut f = serde_json::Map::new();
    if juno_session {
        f.insert("device".into(), Value::from(DEVICE_FACT));
    }
    if let Some(p) = previous {
        let calls: Vec<Value> = p
            .calls
            .iter()
            .map(|(tool, ok)| json!({"tool": tool, "result": if *ok { "ok" } else { "error" }}))
            .collect();
        f.insert("previous_turn_tools".into(), Value::Array(calls));
        f.insert("previous_turn_ended".into(), Value::from(p.ended));
    }
    Value::Object(f)
}

/// Does Jev's top choice start the Juno loop? `open`: a Juno session, or a
/// previous juno_* turn; then any confidence will do.
fn runs(top: Option<(&str, f64)>, open: bool, min: f64) -> bool {
    top.is_some_and(|(tool, c)| wants_juno(Some(tool)) && (open || c >= min))
}

/// Ask Jev (with the facts) and decide whether the Juno loop runs.
async fn route(
    router: &dyn TopTool,
    message: &str,
    juno_session: bool,
    previous: Option<&Previous>,
    tools: &[(String, String)],
) -> bool {
    let top = tokio::time::timeout(
        ROUTE_BUDGET,
        router.first_tool_with(message, facts(juno_session, previous), tools),
    )
    .await
    .ok()
    .flatten();
    let open = juno_session || previous.is_some();
    runs(
        top.as_ref().map(|(t, c)| (t.as_str(), *c)),
        open,
        crate::jev_route::tool_min(),
    )
}

/// The loop's config: [`MAX_ROUNDS`], and the cap's note on the last round.
fn loop_config(deadline: Duration) -> AgentConfig {
    AgentConfig {
        deadline,
        max_rounds: MAX_ROUNDS,
        last_round_note: Some(ROUND_LIMIT_NOTE.to_owned()),
        ..AgentConfig::default()
    }
}

fn ended(stop: agent::Stop) -> &'static str {
    match stop {
        agent::Stop::Answered => "finished",
        agent::Stop::MaxRounds => "round limit",
        agent::Stop::Deadline => "time limit",
    }
}

/// The Juno tools of her owner registry (`None`: off).
fn registry(st: &crate::AppState) -> Option<Registry> {
    let mut reg = Registry::new();
    for n in NAMES {
        reg.register(st.registry.get(n)?.clone()).ok()?;
    }
    Some(reg)
}

/// Jev's first tool is one of Juno's.
fn wants_juno(first: Option<&str>) -> bool {
    first.is_some_and(|t| NAMES.contains(&t))
}

fn record_tool(t: &Turn<'_>, r: &ToolRecord) {
    t.st.timeline_for(&t.session).record(NewItem {
        channel: t.channel.clone(),
        role: "lucina".into(),
        text: format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" }),
        meta: Some(super::tool_meta(&t.agent_mark(), r)),
        ..Default::default()
    });
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if t.guarded || t.consult || t.mcp.is_some() || !st.juno.ever_seen() {
        return None;
    }
    let reg = registry(st)?;
    let now = Utc::now();
    if let Some(p) = st
        .pending
        .current(&t.session, now)
        .filter(|p| NAMES.contains(&p.tool.as_str()))
    {
        match classify_reply(&t.message) {
            Reply::Yes => {
                let action = st.pending.take(&p.id)?;
                let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).confirmed();
                let started = std::time::Instant::now();
                let (ok, output, summary) = agent::execute(
                    &reg,
                    &action.tool,
                    action.args.clone(),
                    &ctx,
                    agent::PER_TOOL_TIMEOUT,
                )
                .await;
                remember(
                    &t.session,
                    Some(Previous {
                        at: now,
                        calls: vec![(action.tool.clone(), ok)],
                        ended: "finished",
                    }),
                    now,
                );
                record_tool(
                    t,
                    &ToolRecord {
                        call_id: action.id.clone(),
                        name: action.tool.clone(),
                        args: action.args,
                        ok,
                        output: output.clone(),
                        summary: summary.clone(),
                        pending: None,
                        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    },
                );
                let result = format!("The owner confirmed \"{summary}\" and it has now run (ok: {ok}). Result: {output}");
                let input = TurnInput {
                    user_text: &t.message,
                    memory,
                    user_label: USER_LABEL,
                    platform: PLATFORM_LABEL,
                };
                let reply = st
                    .planner
                    .generate_skill_reply(&input, "Juno", &result, "Tell the owner what happened on his laptop in one or two sentences, in English (Danish when he writes Danish; never Italian).")
                    .await
                    .unwrap_or_else(|_| if ok { format!("Done: {summary}.") } else { format!("That did not work: {summary}.") });
                return Some(effects_to_finish(&ctx.effects(), reply));
            }
            Reply::No => {
                st.pending.take(&p.id);
                return Some(Finish::reply(
                    format!("OK, not done: {}.", p.summary),
                    "reply",
                ));
            }
            Reply::Other => {}
        }
    }
    let router: Arc<dyn TopTool> = crate::jev_route::top_tool_router()?;
    let tools: Vec<(String, String)> = st
        .registry
        .specs()
        .into_iter()
        .map(|s| (s.function.name, s.function.description))
        .collect();
    let juno_session = device_of_session(&t.session).is_some();
    let prev = previous(&t.session, now);
    if !route(
        router.as_ref(),
        &t.message,
        juno_session,
        prev.as_ref(),
        &tools,
    )
    .await
    {
        remember(&t.session, None, now);
        return None;
    }
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session)
        .with_origin(Origin::Owner)
        .owner_turn();
    let fact = match super::juno_fact(st, &t.session, false) {
        f if f.is_empty() => JUNO_SYSTEM.to_owned(),
        f => f,
    };
    let extra = crate::capabilities::subloop_extra(&format!("{fact} {GUIDE}"), "");
    let system = st.planner.prompts.agent_system(
        PLATFORM_LABEL,
        &t.message,
        memory,
        extra.trim(),
        now,
        st.cfg.time_zone,
    );
    let msgs = vec![
        Message::system(system),
        Message::user(Prompts::user_block(&t.user_label, memory, &t.message)),
    ];
    let cfg = loop_config(st.cfg.turn_deadline);
    let calls = Mutex::new(Vec::new());
    let on_tool = |r: &ToolRecord| {
        calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((r.name.clone(), r.ok));
        record_tool(t, r);
    };
    let run = agent::run(
        st.model.as_ref(),
        &reg,
        msgs,
        &ctx,
        &st.pending,
        &cfg,
        &on_tool,
    )
    .await;
    let turn = Previous {
        at: now,
        calls: calls.into_inner().unwrap_or_else(|e| e.into_inner()),
        ended: run.as_ref().map_or("failed", |out| ended(out.stop)),
    };
    remember(&t.session, Some(turn), now);
    match run {
        Ok(out) if !out.text.is_empty() => Some(effects_to_finish(&ctx.effects(), out.text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Jev's stand-in: a fixed top choice; keeps the state it was given.
    struct FakeJev {
        top: (&'static str, f64),
        seen: Mutex<Vec<Value>>,
    }

    impl FakeJev {
        fn new(tool: &'static str, confidence: f64) -> Self {
            Self {
                top: (tool, confidence),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl TopTool for FakeJev {
        async fn first_tool_with(
            &self,
            user_text: &str,
            facts: Value,
            _: &[(String, String)],
        ) -> Option<(String, f64)> {
            self.seen
                .lock()
                .unwrap()
                .push(crate::jev_route::tool_state(user_text, facts));
            Some((self.top.0.to_owned(), self.top.1))
        }
    }

    fn tools() -> Vec<(String, String)> {
        vec![("juno_call".into(), "Run one driver tool on Juno.".into())]
    }

    fn capped(now: DateTime<Utc>) -> Previous {
        Previous {
            at: now,
            calls: vec![
                ("juno_tools".into(), true),
                ("juno_call".into(), false),
                ("juno_call".into(), false),
            ],
            ended: "round limit",
        }
    }

    #[test]
    fn only_a_juno_tool_starts_the_juno_turn() {
        assert!(wants_juno(Some("juno_screenshot")));
        assert!(wants_juno(Some("juno_call")));
        assert!(!wants_juno(Some("computer")));
        assert!(!wants_juno(Some("juno_other")));
        assert!(!wants_juno(None));
    }

    #[test]
    fn open_sessions_take_any_confidence_others_the_minimum() {
        assert!(runs(Some(("juno_call", 0.1)), true, 0.7));
        assert!(!runs(Some(("juno_call", 0.69)), false, 0.7));
        assert!(runs(Some(("juno_call", 0.7)), false, 0.7));
        assert!(!runs(Some(("none", 0.99)), true, 0.7));
        assert!(!runs(Some(("computer", 0.99)), true, 0.7));
        assert!(!runs(None, true, 0.7));
    }

    #[tokio::test]
    async fn a_juno_session_runs_jevs_unsure_juno_choice() {
        // 2026-11-04: "open the gallery app … and take a screenshot" at 0.49.
        let jev = FakeJev::new("juno_call", 0.49);
        assert!(route(&jev, "open the gallery app", true, None, &tools()).await);
        assert_eq!(
            jev.seen.lock().unwrap()[0],
            json!({"message": "open the gallery app", "device": DEVICE_FACT})
        );
    }

    #[tokio::test]
    async fn elsewhere_without_a_previous_juno_turn_it_does_not() {
        let jev = FakeJev::new("juno_call", 0.49);
        assert!(!route(&jev, "open the gallery app", false, None, &tools()).await);
        assert_eq!(
            jev.seen.lock().unwrap()[0],
            json!({"message": "open the gallery app"})
        );
    }

    #[tokio::test]
    async fn go_after_a_capped_turn_runs_and_jev_sees_only_facts() {
        let now = Utc::now();
        let prev = capped(now);
        let jev = FakeJev::new("juno_call", 0.3);
        assert!(route(&jev, "go", false, Some(&prev), &tools()).await);
        assert!(route(&jev, "go", true, Some(&prev), &tools()).await);
        let seen = jev.seen.lock().unwrap();
        let calls = json!([
            {"tool": "juno_tools", "result": "ok"},
            {"tool": "juno_call", "result": "error"},
            {"tool": "juno_call", "result": "error"},
        ]);
        assert_eq!(
            seen[0],
            json!({"message": "go", "previous_turn_tools": calls, "previous_turn_ended": "round limit"})
        );
        assert_eq!(
            seen[1],
            json!({"message": "go", "device": DEVICE_FACT,
                "previous_turn_tools": calls, "previous_turn_ended": "round limit"})
        );
    }

    #[test]
    fn the_previous_turn_counts_for_fifteen_minutes_and_is_replaced() {
        let session = "test_previous_window";
        let now = Utc::now();
        remember(session, Some(capped(now)), now);
        assert_eq!(previous(session, now), Some(capped(now)));
        assert!(previous(session, now + chrono::Duration::minutes(14)).is_some());
        assert!(previous(session, now + chrono::Duration::minutes(16)).is_none());
        remember(session, None, now);
        assert!(
            previous(session, now).is_none(),
            "a turn without juno_* calls clears it"
        );
        let empty = Previous {
            calls: Vec::new(),
            ..capped(now)
        };
        remember(session, Some(empty), now);
        assert!(previous(session, now).is_none());
    }

    #[test]
    fn the_loop_has_ten_rounds_and_the_caps_note() {
        let cfg = loop_config(Duration::from_secs(90));
        assert_eq!(cfg.max_rounds, 10);
        assert_eq!(cfg.last_round_note.as_deref(), Some(ROUND_LIMIT_NOTE));
        assert_eq!(ended(agent::Stop::MaxRounds), "round limit");
        assert_eq!(ended(agent::Stop::Answered), "finished");
    }

    #[test]
    fn juno_system_forbids_claimed_laptop_actions() {
        // Both planners take the Juno fact from `juno_fact`, which starts with JUNO_SYSTEM.
        assert!(JUNO_SYSTEM
            .contains("You act on the laptop only through juno_* tool calls made in this turn."));
        assert!(JUNO_SYSTEM.contains("unless a juno_* call in this turn did it"));
    }
}
