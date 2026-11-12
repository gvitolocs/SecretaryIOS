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
//!   ([`PREVIOUS_WINDOW`]), Jev's top `juno_*` choice runs at any confidence,
//!   and any other top tool of her owner registry (not "none") runs the loop
//!   with the Juno tools plus that one (`notes`), so she can pick the right one;
//!   elsewhere a `juno_*` choice from `LUCINA_JEV_TOOL_MIN`. The loop's system
//!   text lists the laptop's tools and intents from its last answers. Jev's state holds facts only: the
//!   device and the previous turn's juno_* calls and how it ended. No message
//!   text is matched; without Jev the turn goes on as itself, and with no Juno
//!   device ever seen Jev is not asked at all. An offline laptop's error
//!   reaches her through the tool, and she says so.
//! - when no loop runs in such a session, the JSON planner's system text
//!   (`TURN_SYSTEM`) gets [`JUNO_SYSTEM`] and [`NO_TOOL_RULE`]: no narrated actions.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use lucina_llm::Message;
use lucina_store::timeline::NewItem;
use lucina_tools::agent::{self, AgentConfig, AgentOutcome, ToolRecord};
use lucina_tools::carry;
use lucina_tools::pending::{classify_reply, Reply};
use lucina_tools::tools::juno::{device_of_session, fetch_lists, known_lists, NAMES};
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
const MAX_ROUNDS: usize = 16;

/// Fetching the laptop's tool and intent lists at the loop's start takes at most this long.
const LISTS_BUDGET: Duration = Duration::from_secs(8);

/// After the laptop's lists in the loop's system text.
const LISTS_NOTE: &str = "The laptop's tools and intents are listed above; call juno_tools or intents only if you need a schema that is not shown.";

/// The planner path's rule in a Juno session (or after a juno_* turn) when no tool loop ran.
pub const NO_TOOL_RULE: &str = "No tool ran in this turn. Do not say you are doing, sending, opening, creating or playing anything now or next; say plainly that nothing was done yet and what you need (for example a go-ahead).";

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
            // The carried loop never outlives this record (juno-lucina-tools' rule).
            carry::forget(session);
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

/// The Juno loop Jev's top choice starts.
#[derive(Debug, Clone, PartialEq)]
enum Choice {
    /// The juno_* tools.
    Juno,
    /// The juno_* tools and this tool of her owner registry.
    JunoWith(String),
}

/// Does Jev's top choice start the Juno loop? `open`: a Juno session, or a
/// previous juno_* turn; then any confidence will do, and any other tool of
/// `tools` (not "none") joins the juno_* ones.
fn runs(
    top: Option<(&str, f64)>,
    open: bool,
    min: f64,
    tools: &[(String, String)],
) -> Option<Choice> {
    let (tool, c) = top?;
    if wants_juno(Some(tool)) {
        return (open || c >= min).then_some(Choice::Juno);
    }
    (open && tools.iter().any(|(n, _)| n == tool)).then(|| Choice::JunoWith(tool.to_owned()))
}

/// Ask Jev (with the facts) and decide whether, and with what, the Juno loop runs.
async fn route(
    router: &dyn TopTool,
    message: &str,
    juno_session: bool,
    previous: Option<&Previous>,
    tools: &[(String, String)],
) -> Option<Choice> {
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
        tools,
    )
}

/// The planner path's system text for a session in the Juno window: a Juno
/// Chat session or one whose last turn used juno_* tools (call before
/// [`try_turn`], which closes the window when no loop runs). `None` elsewhere.
pub(super) fn planner_system(session: &str) -> Option<String> {
    if device_of_session(session).is_some() {
        return Some(format!("{JUNO_SYSTEM} {NO_TOOL_RULE}"));
    }
    previous(session, Utc::now()).map(|_| NO_TOOL_RULE.to_owned())
}

/// The loop's extra system text: the Juno fact, the guide, and the laptop's
/// lists from its last answers, when known.
fn loop_extra(fact: &str, lists: Option<&str>) -> String {
    match lists {
        Some(l) => format!("{fact} {GUIDE}\n\n{l}\n\n{LISTS_NOTE}"),
        None => format!("{fact} {GUIDE}"),
    }
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

/// The Juno tools of `owner`, her owner registry (`None`: off), and `with` when it has it.
fn registry(owner: &Registry, with: Option<&str>) -> Option<Registry> {
    let mut reg = Registry::new();
    for n in NAMES {
        reg.register(owner.get(n)?.clone()).ok()?;
    }
    if let Some(tool) = with.and_then(|w| owner.get(w)) {
        reg.register(tool.clone()).ok()?;
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
    let reg = registry(&st.registry, None)?;
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
                    device: "",
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
    let Some(choice) = route(
        router.as_ref(),
        &t.message,
        juno_session,
        prev.as_ref(),
        &tools,
    )
    .await
    else {
        remember(&t.session, None, now);
        return None;
    };
    let reg = match &choice {
        Choice::Juno => reg,
        Choice::JunoWith(tool) => registry(&st.registry, Some(tool))?,
    };
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session)
        .with_origin(Origin::Owner)
        .owner_turn();
    let fact = match super::juno_fact(st, &t.session, false) {
        f if f.is_empty() => JUNO_SYSTEM.to_owned(),
        f => f,
    };
    fetch_lists(&ctx, LISTS_BUDGET).await;
    let lists = known_lists(&t.session);
    let extra = crate::capabilities::subloop_extra(&loop_extra(&fact, lists.as_deref()), "");
    let system = st.planner.prompts.agent_system(
        PLATFORM_LABEL,
        &t.message,
        memory,
        extra.trim(),
        now,
        st.cfg.time_zone,
    );
    let msgs = loop_messages(
        &t.session,
        system,
        Prompts::user_block(&t.user_label, memory, &t.message),
    );
    let opening = msgs.len();
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
    keep_loop(&t.session, &t.message, opening, &run);
    match run {
        Ok(out) if !out.text.is_empty() => Some(effects_to_finish(&ctx.effects(), out.text)),
        _ => None,
    }
}

/// The loop's opening messages: the system text, then this session's previous
/// Juno loop when it ended within [`carry::TTL`] (its calls and results as the
/// model saw them; Hermes, LucinaCode and OpenBot replay a turn's tools the same
/// way), then the owner's message.
fn loop_messages(session: &str, system: String, user: String) -> Vec<Message> {
    let mut msgs = vec![Message::system(system)];
    // Only while the previous-turn record ([`PREVIOUS_WINDOW`]) still holds that turn.
    if previous(session, Utc::now()).is_some() {
        msgs.extend(carry::previous(session));
    }
    msgs.push(Message::user(user));
    msgs
}

/// Keep what this loop added (after its `opening` messages) for the session's
/// next Juno loop; a failed loop leaves nothing to carry.
fn keep_loop(
    session: &str,
    message: &str,
    opening: usize,
    run: &Result<AgentOutcome, lucina_llm::LlmError>,
) {
    match run {
        Ok(out) => carry::remember(
            session,
            message,
            out.messages.get(opening..).unwrap_or_default(),
        ),
        Err(_) => carry::forget(session),
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

    fn owner_tools() -> Vec<(String, String)> {
        vec![
            ("juno_call".into(), "Run one driver tool on Juno.".into()),
            ("notes".into(), "The owner's notes.".into()),
        ]
    }

    #[test]
    fn open_sessions_take_any_confidence_others_the_minimum() {
        let t = owner_tools();
        assert_eq!(
            runs(Some(("juno_call", 0.1)), true, 0.7, &t),
            Some(Choice::Juno)
        );
        assert_eq!(runs(Some(("juno_call", 0.69)), false, 0.7, &t), None);
        assert_eq!(
            runs(Some(("juno_call", 0.7)), false, 0.7, &t),
            Some(Choice::Juno)
        );
        assert_eq!(runs(Some(("none", 0.99)), true, 0.7, &t), None);
        assert_eq!(
            runs(Some(("computer", 0.99)), true, 0.7, &t),
            None,
            "not in her registry"
        );
        assert_eq!(
            runs(Some(("notes", 0.99)), false, 0.7, &t),
            None,
            "only in the Juno window"
        );
        assert_eq!(runs(None, true, 0.7, &t), None);
    }

    /// A tool of her owner registry by name only.
    struct Named(&'static str);

    #[async_trait::async_trait]
    impl lucina_tools::Tool for Named {
        fn name(&self) -> &'static str {
            self.0
        }
        fn description(&self) -> &'static str {
            "a test tool"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn side_effect(&self) -> lucina_tools::SideEffect {
            lucina_tools::SideEffect::None
        }
        async fn call(&self, _: Value, _: &Ctx) -> lucina_tools::ToolResult {
            Ok(json!({}))
        }
    }

    fn owner_registry() -> Registry {
        let mut reg = Registry::new();
        for n in NAMES.iter().copied().chain(["notes", "computer"]) {
            reg.register(Arc::new(Named(n))).unwrap();
        }
        reg
    }

    #[tokio::test]
    async fn a_juno_session_with_jevs_top_notes_runs_the_loop_with_juno_and_notes() {
        // 2026-11-04, gen 31: "go ahead, create the note titled Juno test …" → notes at 0.41, no loop.
        let jev = FakeJev::new("notes", 0.41);
        let choice = route(
            &jev,
            "go ahead, create the note",
            true,
            None,
            &owner_tools(),
        )
        .await;
        assert_eq!(choice, Some(Choice::JunoWith("notes".into())));
        let reg = registry(&owner_registry(), Some("notes")).unwrap();
        let mut names: Vec<String> = reg.specs().into_iter().map(|s| s.function.name).collect();
        names.sort_unstable();
        let mut want: Vec<&str> = NAMES.iter().copied().chain(["notes"]).collect();
        want.sort_unstable();
        assert_eq!(names, want);
        assert_eq!(
            registry(&owner_registry(), None).unwrap().specs().len(),
            NAMES.len()
        );
    }

    #[tokio::test]
    async fn a_juno_session_with_jevs_top_none_runs_no_loop() {
        let jev = FakeJev::new("none", 0.9);
        assert_eq!(
            route(&jev, "thanks", true, None, &owner_tools()).await,
            None
        );
    }

    #[test]
    fn juno_planner_rule_is_for_juno_sessions_and_the_juno_window_only() {
        let juno = planner_system("secretary_juno-dev_K8bGH74HE4k").unwrap();
        assert!(
            juno.starts_with(JUNO_SYSTEM) && juno.ends_with(NO_TOOL_RULE),
            "{juno}"
        );
        assert!(planner_system("secretary_web_test_rule").is_none());
        let now = Utc::now();
        remember("secretary_web_test_rule", Some(capped(now)), now);
        assert_eq!(
            planner_system("secretary_web_test_rule").as_deref(),
            Some(NO_TOOL_RULE)
        );
        remember("secretary_web_test_rule", None, now);
    }

    #[tokio::test]
    async fn the_json_planner_gets_the_juno_no_tool_rule_in_a_juno_session_only() {
        use crate::planner::{Planner, TurnInput, TURN_SYSTEM};
        let model = Arc::new(lucina_llm::ScriptedModel::new());
        model.push_text(r#"{"action":"reply","text":"Nothing was done yet."}"#);
        model.push_text(r#"{"action":"reply","text":"Hi."}"#);
        let planner = Planner {
            model: model.clone(),
            prompts: Prompts::minimal("Lucina"),
        };
        let input = TurnInput {
            user_text: "go ahead, create the note titled Juno test",
            memory: "",
            user_label: "Giuseppe",
            platform: "secretary",
            device: "",
        };
        for session in ["secretary_juno-dev_K8bGH74HE4k", "secretary_web_plain"] {
            match planner_system(session) {
                Some(rule) => {
                    TURN_SYSTEM
                        .scope(
                            super::super::with_juno_rule(&rule),
                            planner.plan_response(&input),
                        )
                        .await
                }
                None => planner.plan_response(&input).await,
            }
            .unwrap();
        }
        let reqs = model.requests.lock().unwrap().clone();
        let juno = reqs[0].messages[0].text();
        let web = reqs[1].messages[0].text();
        assert!(
            juno.contains(NO_TOOL_RULE) && juno.contains(JUNO_SYSTEM),
            "{juno}"
        );
        assert!(!web.contains(NO_TOOL_RULE) && !web.contains(JUNO_SYSTEM));
    }

    /// The bridge's stand-in for the lists: answers `tools` and `intents`.
    struct Lists;

    #[async_trait::async_trait]
    impl lucina_tools::tools::juno::JunoLink for Lists {
        async fn call(
            &self,
            _: Option<String>,
            tool: &str,
            _: Value,
            _: Duration,
            _: &lucina_tools::tools::juno::CallOpts,
        ) -> Result<Value, String> {
            Ok(match tool {
                "tools" => json!({"text": "Tools on this laptop now:", "tools": [
                    {"name": "intent", "description": "Run an app intent.", "available": true},
                    {"name": "ui_tree", "description": "The screen's elements.", "available": true},
                ]}),
                _ => json!({"text": "0 intents"}),
            })
        }
        fn status(&self) -> Value {
            json!({"devices": []})
        }
        async fn ask(&self, _: lucina_tools::tools::juno::Ask) -> Result<String, String> {
            Err("no decisions here".into())
        }
        fn decided(&self, _: &str, _: &str) -> Result<(), String> {
            Err("no decisions here".into())
        }
        fn events(&self, _: Option<&str>, _: &lucina_tools::tools::juno::EventQuery) -> Value {
            json!({"events": []})
        }
    }

    #[tokio::test]
    async fn the_juno_loop_text_lists_the_laptops_tools_after_one_tools_answer() {
        let session = "secretary_juno-dev_loop_lists";
        assert_eq!(
            loop_extra(JUNO_SYSTEM, known_lists(session).as_deref()),
            format!("{JUNO_SYSTEM} {GUIDE}")
        );
        let mut env =
            lucina_tools::ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        env.juno = Some(Arc::new(Lists));
        let ctx = Ctx::new(Arc::new(env), "ios", session);
        lucina_tools::Tool::call(&lucina_tools::tools::juno::JunoToolsTool, json!({}), &ctx)
            .await
            .unwrap();
        let text = loop_extra(JUNO_SYSTEM, known_lists(session).as_deref());
        assert!(
            text.contains("intent: Run an app intent.\nui_tree: The screen's elements."),
            "{text}"
        );
        assert!(text.ends_with(LISTS_NOTE));
        assert_eq!(text.matches("ui_tree").count(), 1, "listed once");
    }

    #[tokio::test]
    async fn a_juno_session_runs_jevs_unsure_juno_choice() {
        // 2026-11-04: "open the gallery app … and take a screenshot" at 0.49.
        let jev = FakeJev::new("juno_call", 0.49);
        assert_eq!(
            route(&jev, "open the gallery app", true, None, &tools()).await,
            Some(Choice::Juno)
        );
        assert_eq!(
            jev.seen.lock().unwrap()[0],
            json!({"message": "open the gallery app", "device": DEVICE_FACT})
        );
    }

    #[tokio::test]
    async fn elsewhere_without_a_previous_juno_turn_it_does_not() {
        let jev = FakeJev::new("juno_call", 0.49);
        assert!(route(&jev, "open the gallery app", false, None, &tools())
            .await
            .is_none());
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
        assert!(route(&jev, "go", false, Some(&prev), &tools())
            .await
            .is_some());
        assert!(route(&jev, "go", true, Some(&prev), &tools())
            .await
            .is_some());
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
    fn the_lists_prefetch_waits_at_most_eight_seconds() {
        assert_eq!(LISTS_BUDGET, Duration::from_secs(8));
    }

    #[test]
    fn the_juno_loop_has_sixteen_rounds_and_the_caps_note() {
        let cfg = loop_config(Duration::from_secs(90));
        assert_eq!(cfg.max_rounds, 16);
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

    #[tokio::test]
    async fn go_ahead_after_a_capped_turn_sees_the_previous_call_and_result() {
        use lucina_llm::Role;
        let session = "secretary_juno-dev_carry_test";
        let mut env =
            lucina_tools::ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        env.juno = Some(Arc::new(Lists));
        let ctx = Ctx::new(Arc::new(env), "ios", session).with_origin(Origin::Owner);
        let mut reg = Registry::new();
        reg.register(Arc::new(lucina_tools::tools::juno::JunoCallTool))
            .unwrap();
        let model = lucina_llm::ScriptedModel::new();
        model.push_calls(&[("juno_call", json!({"tool": "intents"}))]);
        model.push_text("The rounds ran out: nothing was made yet.");
        let cfg = AgentConfig {
            max_rounds: 1,
            ..AgentConfig::default()
        };
        let pending =
            lucina_tools::pending::PendingStore::memory(lucina_tools::pending::DEFAULT_TTL);
        let msgs = loop_messages(session, "sys".into(), "make a note".into());
        let opening = msgs.len();
        let run = agent::run(&model, &reg, msgs, &ctx, &pending, &cfg, &|_| {}).await;
        assert_eq!(run.as_ref().unwrap().stop, agent::Stop::MaxRounds);
        remember(session, Some(capped(Utc::now())), Utc::now());
        keep_loop(session, "make a note", opening, &run);

        let next = loop_messages(session, "sys".into(), "go ahead".into());
        assert_eq!(next[0].role, Role::System);
        assert_eq!(next.last().unwrap().text(), "go ahead");
        assert_eq!(
            next[1].text(),
            "make a note",
            "the earlier request comes first"
        );
        let call = next
            .iter()
            .position(|m| {
                m.tool_calls
                    .iter()
                    .flatten()
                    .any(|c| c.function.name == "juno_call")
            })
            .expect("the previous juno_call is carried");
        assert!(
            next[call + 1].role == Role::Tool && next[call + 1].text().contains("0 intents"),
            "{:?}",
            next[call + 1]
        );
        assert!(next.iter().any(|m| m.text().contains("The rounds ran out")));
        // Only this session's loop: another session starts clean.
        assert_eq!(
            loop_messages("secretary_juno-dev_other", "sys".into(), "go".into()).len(),
            2
        );
        // A failed loop leaves nothing to carry.
        keep_loop(session, "x", 2, &Err(lucina_llm::LlmError::Timeout));
        assert_eq!(loop_messages(session, "sys".into(), "go".into()).len(), 2);
    }

    /// The laptop's stand-in for one intent: a note made, with its undo id.
    struct MadeNote;

    #[async_trait::async_trait]
    impl lucina_tools::tools::juno::JunoLink for MadeNote {
        async fn call(
            &self,
            _: Option<String>,
            tool: &str,
            _: Value,
            _: Duration,
            _: &lucina_tools::tools::juno::CallOpts,
        ) -> Result<Value, String> {
            Ok(match tool {
                "intent" => json!({"text": "Note made.", "action": "act_42", "undoable": true}),
                _ => json!({"text": "ok"}),
            })
        }
        fn status(&self) -> Value {
            json!({"devices": []})
        }
        async fn ask(&self, _: lucina_tools::tools::juno::Ask) -> Result<String, String> {
            Err("no decisions here".into())
        }
        fn decided(&self, _: &str, _: &str) -> Result<(), String> {
            Err("no decisions here".into())
        }
        fn events(&self, _: Option<&str>, _: &lucina_tools::tools::juno::EventQuery) -> Value {
            json!({"events": []})
        }
    }

    #[tokio::test]
    async fn go_ahead_sees_the_previous_intents_action_id_only_while_the_record_holds() {
        let session = "secretary_juno-dev_carry_action";
        let mut env =
            lucina_tools::ToolEnv::new(std::env::temp_dir(), chrono_tz::Europe::Copenhagen);
        env.juno = Some(Arc::new(MadeNote));
        let ctx = Ctx::new(Arc::new(env), "ios", session).with_origin(Origin::Owner);
        let mut reg = Registry::new();
        reg.register(Arc::new(lucina_tools::tools::juno::JunoCallTool))
            .unwrap();
        let model = lucina_llm::ScriptedModel::new();
        model.push_calls(&[(
            "juno_call",
            json!({"tool": "intent", "arguments": {"name": "notes.create", "args": {"title": "Juno test", "body": "hi"}}}),
        )]);
        model.push_text("Made the note; the rounds ran out before the undo.");
        let cfg = AgentConfig {
            max_rounds: 1,
            ..AgentConfig::default()
        };
        let pending =
            lucina_tools::pending::PendingStore::memory(lucina_tools::pending::DEFAULT_TTL);
        let msgs = loop_messages(session, "sys".into(), "make a note, then undo it".into());
        let opening = msgs.len();
        let run = agent::run(&model, &reg, msgs, &ctx, &pending, &cfg, &|_| {}).await;
        assert_eq!(run.as_ref().unwrap().stop, agent::Stop::MaxRounds);
        remember(session, Some(capped(Utc::now())), Utc::now());
        keep_loop(session, "make a note, then undo it", opening, &run);

        let next = loop_messages(session, "sys".into(), "go ahead".into());
        assert!(
            next.iter()
                .any(|m| m.role == lucina_llm::Role::Tool && m.text().contains("act_42")),
            "the undo id reaches the go-ahead turn: {next:?}"
        );
        // The record cleared (a turn without a loop): nothing is carried.
        remember(session, None, Utc::now());
        assert_eq!(
            loop_messages(session, "sys".into(), "go ahead".into()).len(),
            2
        );
    }
}
