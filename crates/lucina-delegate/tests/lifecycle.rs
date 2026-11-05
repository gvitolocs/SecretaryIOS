//! The delegation job lifecycle with mocked agents and a scripted judge.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_delegate::agents::{Agent, AgentJob, AgentReply};
use lucina_delegate::config::{Budget, DelegateConfig, Isolation};
use lucina_delegate::engine::{Card, Sink};
use lucina_delegate::intent::{Switch, SwitchIntent};
use lucina_delegate::policy::Refusal;
use lucina_delegate::{AgentId, Delegator, JobStatus, Kind, Origin, Request, StartOutcome};
use lucina_llm::{ChatModel, ScriptedModel};

struct Mock {
    id: AgentId,
    delay: Duration,
    answer: Result<String, String>,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Agent for Mock {
    fn id(&self) -> AgentId {
        self.id
    }
    fn supports(&self, kind: Kind) -> bool {
        self.id == AgentId::Claude || kind != Kind::Code
    }
    async fn backend(&self) -> Result<String, String> {
        Ok("mock".into())
    }
    async fn run(&self, job: &AgentJob) -> Result<AgentReply, String> {
        self.prompts.lock().unwrap().push(job.prompt.clone());
        tokio::time::sleep(self.delay).await;
        self.answer.clone().map(|text| AgentReply {
            text,
            session: Some("sess-1".into()),
            turns: 2,
            backend: "mock".into(),
        })
    }
}

#[derive(Default)]
struct Rec {
    cards: Mutex<Vec<Card>>,
    notes: Mutex<Vec<(String, String)>>,
    notified: Mutex<Vec<String>>,
    messages: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Sink for Rec {
    async fn save_note(&self, title: &str, content: &str, _: &[String]) -> Result<String, String> {
        self.notes
            .lock()
            .unwrap()
            .push((title.into(), content.into()));
        Ok("delegation-note".into())
    }
    async fn notify(&self, _: &str, body: &str) {
        self.notified.lock().unwrap().push(body.into());
    }
    fn card(&self, card: &Card) {
        self.cards.lock().unwrap().push(card.clone());
    }
    fn message(&self, _: &str, text: &str) {
        self.messages.lock().unwrap().push(text.into());
    }
}

struct World {
    d: Arc<Delegator>,
    sink: Arc<Rec>,
    judge: Arc<ScriptedModel>,
    prompts: Arc<Mutex<Vec<String>>>,
    dir: tempfile::TempDir,
}

const ACT: &str = r#"{"goal":"g","risk":"low","reversible":true,"why":"Lo chiedo perché serve.","decision":"act"}"#;

fn world(
    tweak: impl FnOnce(&mut DelegateConfig),
    agents: &[(AgentId, Duration, Result<&str, &str>)],
) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DelegateConfig::load(None, dir.path()).unwrap();
    cfg.sandbox_root = dir.path().join("sandboxes");
    cfg.allowed_repo_roots = vec![dir.path().join("Projects")];
    cfg.isolation = Isolation::None;
    cfg.inline_wait_secs = 5;
    cfg.note_threshold_chars = 200;
    tweak(&mut cfg);
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let agents: Vec<Arc<dyn Agent>> = agents
        .iter()
        .map(|(id, delay, a)| {
            Arc::new(Mock {
                id: *id,
                delay: *delay,
                answer: a.map(str::to_owned).map_err(str::to_owned),
                prompts: prompts.clone(),
            }) as Arc<dyn Agent>
        })
        .collect();
    let judge = Arc::new(ScriptedModel::new());
    let sink = Arc::new(Rec::default());
    let d = Arc::new(Delegator::new(
        cfg,
        agents,
        Some(judge.clone() as Arc<dyn ChatModel>),
        sink.clone(),
        chrono_tz::Europe::Rome,
    ));
    World {
        d,
        sink,
        judge,
        prompts,
        dir,
    }
}

fn req(kind: Kind, task: &str) -> Request {
    Request {
        session_id: "secretary_app".into(),
        ..Request::owner(kind, task)
    }
}

async fn finish(out: StartOutcome) -> lucina_delegate::JobRecord {
    match out {
        StartOutcome::Started(mut s) => s.wait(Duration::from_secs(10)).await.expect("finished"),
        StartOutcome::Skipped(j) => j,
        StartOutcome::Refused(r) => panic!("refused: {r:?}"),
    }
}

fn refused(out: StartOutcome) -> Refusal {
    match out {
        StartOutcome::Refused(r) => r,
        StartOutcome::Started(s) => panic!("started {}", s.job.id),
        StartOutcome::Skipped(j) => panic!("skipped {}", j.id),
    }
}

#[tokio::test]
async fn a_quick_answer_comes_back_inline_with_a_card_and_an_audit_line() {
    let w = world(
        |_| {},
        &[(
            AgentId::Grok,
            Duration::ZERO,
            Ok("Rust 1.95 è uscito ieri."),
        )],
    );
    w.judge.push_text(ACT);
    let job = finish(
        w.d.start(req(Kind::Research, "ultime notizie su Rust"))
            .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(job.agent, AgentId::Grok, "research goes to Grok");
    assert_eq!(job.result.as_deref(), Some("Rust 1.95 è uscito ieri."));
    assert_eq!(job.turns, 2);
    let prompt = w.prompts.lock().unwrap()[0].clone();
    assert!(prompt.starts_with("[Lucina delegation · job "), "{prompt}");
    assert!(prompt.contains("from Lucina") && prompt.contains("ultime notizie su Rust"));
    let cards = w.sink.cards.lock().unwrap();
    assert_eq!(cards.len(), 1, "fast jobs: only the final card");
    assert!(
        cards[0]
            .plain
            .starts_with("I asked Grok: “ultime notizie su Rust” — ✅ answer ready"),
        "{}",
        cards[0].plain
    );
    assert!(cards[0].plain.contains("Why: Lo chiedo perché serve."));
    assert!(cards[0].rich.is_some());
    assert!(
        w.sink.notified.lock().unwrap().is_empty(),
        "no push for an inline answer"
    );
    assert!(
        w.sink.notes.lock().unwrap().is_empty(),
        "short answer: no note"
    );
    // audit: agent, kind, outcome, budget, never the task or the answer
    let audit =
        std::fs::read_to_string(w.dir.path().join("lucina-rs/delegate-audit.jsonl")).unwrap();
    assert!(
        audit.contains("\"agent\":\"grok\"") && audit.contains("\"outcome\":\"done\""),
        "{audit}"
    );
    assert!(audit.contains("\"calls\":1"));
    assert!(!audit.contains("Rust"), "{audit}");
    // the sandbox of a read-only job is gone; the job is persisted
    assert!(!std::path::Path::new(job.sandbox.as_deref().unwrap()).exists());
    assert_eq!(w.d.job(&job.id[..8]).unwrap().status, JobStatus::Done);
}

#[tokio::test]
async fn long_answers_become_a_note_and_a_summary_and_slow_ones_notify() {
    let long = "Analisi. ".repeat(80);
    let w = world(
        |c| c.inline_wait_secs = 0,
        &[(
            AgentId::Claude,
            Duration::from_millis(300),
            Ok(long.as_str()),
        )],
    );
    w.judge.push_text(ACT);
    w.judge.push_text("In breve: SQLite basta.");
    let job = finish(
        w.d.start(req(Kind::Analysis, "confronta SQLite e Postgres"))
            .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(job.note_slug.as_deref(), Some("delegation-note"));
    assert_eq!(job.summary.as_deref(), Some("In breve: SQLite basta."));
    let notes = w.sink.notes.lock().unwrap();
    assert!(notes[0].0.starts_with("Claude: "));
    assert!(notes[0].1.contains(long.trim()));
    let cards = w.sink.cards.lock().unwrap();
    assert_eq!(cards.len(), 2, "a running card, then the final one");
    assert!(cards[0].plain.contains("⏳"));
    assert!(cards[1].plain.contains("/notes/delegation-note"));
    assert_eq!(
        w.sink.notified.lock().unwrap().len(),
        1,
        "slow job: one push"
    );
}

#[tokio::test]
async fn a_job_someone_else_asked_for_never_pushes() {
    let w = world(
        |c| c.inline_wait_secs = 0,
        &[(AgentId::Claude, Duration::from_millis(300), Ok("Fatto."))],
    );
    w.judge.push_text(ACT);
    let mut r = req(Kind::Analysis, "confronta SQLite e Postgres");
    r.caller = Some("some-new-provider".into());
    let job = finish(w.d.start(r).await).await;
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(job.caller.as_deref(), Some("some-new-provider"));
    assert!(!w.sink.cards.lock().unwrap().is_empty(), "the chat still has it");
    assert!(w.sink.notified.lock().unwrap().is_empty(), "no push, slow or not");
}

#[tokio::test]
async fn self_review_skip_and_revise() {
    let w = world(|_| {}, &[(AgentId::Chatgpt, Duration::ZERO, Ok("ok"))]);
    w.judge.push_text(r#"{"goal":"g","risk":"low","reversible":true,"why":"Non serve: lo so già.","decision":"skip"}"#);
    let job = finish(w.d.start(req(Kind::Question, "capitale del Perù?")).await).await;
    assert_eq!(job.status, JobStatus::Skipped);
    assert!(
        w.prompts.lock().unwrap().is_empty(),
        "skipped: no agent call"
    );
    assert!(w.sink.cards.lock().unwrap()[0]
        .plain
        .starts_with("I decided not to ask ChatGPT"));
    let status = w.d.status().await;
    assert_eq!(
        status["budget"]["totalCalls"], 0,
        "a skipped call is not counted"
    );

    w.judge.push_text(r#"{"goal":"g","risk":"low","reversible":true,"why":"Tolgo i dati personali.","decision":"revise","revised_task":"Is ibuprofen safe with coffee?"}"#);
    let job = finish(
        w.d.start(req(
            Kind::Question,
            "Giuseppe Vitolo, via Roma 3, prende ibuprofene: va bene col caffè?",
        ))
        .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Done);
    let p = w.prompts.lock().unwrap()[0].clone();
    assert!(p.contains("Is ibuprofen safe with coffee?"), "{p}");
    assert!(!p.contains("via Roma"), "minimised: {p}");
    assert_eq!(job.task, "Is ibuprofen safe with coffee?");
}

#[tokio::test]
async fn no_secret_reaches_an_agent() {
    let w = world(|_| {}, &[(AgentId::Claude, Duration::ZERO, Ok("done"))]);
    let value = "Hq7Zp2Lm9Xc4Vb8Nn3Kd6Rt1Wy5Gs0Fa";
    lucina_secrets::redactor().register("LIFECYCLE_TEST_SECRET", value);
    w.judge.push_text(ACT);
    let job = finish(
        w.d.start(req(
            Kind::Analysis,
            &format!("analizza questo log: token={value} ref secret://lucina/prod/lucina#DB_URL"),
        ))
        .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Done);
    let p = w.prompts.lock().unwrap()[0].clone();
    assert!(!p.contains(value) && !p.contains("secret://"), "{p}");
    // nor the judge model
    let judged = w.judge.requests.lock().unwrap()[0].messages[1]
        .text()
        .to_owned();
    // (the server wraps the judge model in the redacting model; here the
    // raw task reaches the scripted judge, so only the agent side is checked)
    assert!(judged.contains("analizza"));
    assert!(!job.task.contains(value), "the stored task is redacted");
}

#[tokio::test]
async fn hard_limits_refuse_before_anything_runs() {
    let w = world(
        |_| {},
        &[
            (AgentId::Grok, Duration::ZERO, Ok("x")),
            (AgentId::Claude, Duration::ZERO, Ok("x")),
        ],
    );
    let mut r = req(Kind::Research, "ultime notizie");
    r.origin = Origin::Mcp;
    r.caller = Some("chatgpt".into());
    assert_eq!(refused(w.d.start(r).await), Refusal::McpOrigin);
    let mut r = req(Kind::Research, "ultime notizie");
    r.depth = 1;
    assert_eq!(refused(w.d.start(r).await), Refusal::Depth);
    let mut r = req(Kind::Research, "ultime notizie");
    r.origin = Origin::Delegated;
    assert_eq!(refused(w.d.start(r).await), Refusal::Depth);
    assert_eq!(
        refused(
            w.d.start(req(Kind::Question, "compra le cuffie Sony"))
                .await
        ),
        Refusal::Money
    );
    let mut r = req(Kind::Code, "fix the bug");
    r.origin = Origin::Proactive;
    assert_eq!(refused(w.d.start(r).await), Refusal::ProactiveReadOnly);
    assert!(
        matches!(
            refused(w.d.start(req(Kind::Code, "fix the bug")).await),
            Refusal::Workspace(_)
        ),
        "no code without the bubblewrap sandbox"
    );
    let mut r = req(Kind::Research, "x");
    r.to = Some(AgentId::Grok);
    r.caller = Some("grok".into());
    assert_eq!(refused(w.d.start(r).await), Refusal::Loop(AgentId::Grok));
    assert!(w.prompts.lock().unwrap().is_empty());
    assert_eq!(w.judge.request_count(), 0, "refusals never reach the judge");
    let audit =
        std::fs::read_to_string(w.dir.path().join("lucina-rs/delegate-audit.jsonl")).unwrap();
    assert!(audit.contains("refused:mcp_origin") && audit.contains("refused:money"));
}

/// 2026-11-01: ChatGPT's `ask_lucina` research note went to the auto router, which picked ChatGPT
/// itself (Grok busy) and failed. The auto choice skips the caller; with only the caller left,
/// nothing runs.
#[tokio::test]
async fn the_auto_choice_never_sends_a_request_back_to_its_caller() {
    let w = world(
        |_| {},
        &[
            (AgentId::Chatgpt, Duration::ZERO, Ok("chatgpt")),
            (AgentId::Claude, Duration::ZERO, Ok("claude")),
        ],
    );
    w.judge.push_text(ACT);
    let mut r = req(Kind::Research, "research on interaction nets");
    r.caller = Some("chatgpt".into());
    assert_eq!(finish(w.d.start(r).await).await.agent, AgentId::Claude);

    let w = world(|_| {}, &[(AgentId::Chatgpt, Duration::ZERO, Ok("chatgpt"))]);
    let mut r = req(Kind::Research, "research on interaction nets");
    r.caller = Some("chatgpt".into());
    assert!(matches!(refused(w.d.start(r).await), Refusal::NoAgent(_)));
    assert!(w.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn one_job_per_agent_and_the_router_falls_back() {
    let w = world(
        |_| {},
        &[
            (AgentId::Grok, Duration::from_millis(500), Ok("grok")),
            (AgentId::Chatgpt, Duration::ZERO, Ok("chatgpt")),
        ],
    );
    w.judge.push_text(ACT);
    w.judge.push_text(ACT);
    let first = match w.d.start(req(Kind::Research, "notizie su Rust")).await {
        StartOutcome::Started(s) => s,
        _ => panic!(),
    };
    let mut r = req(Kind::Research, "notizie su Go");
    r.to = Some(AgentId::Grok);
    assert_eq!(refused(w.d.start(r).await), Refusal::Busy(AgentId::Grok));
    // auto: the next agent takes it
    let job = finish(w.d.start(req(Kind::Research, "notizie su Go")).await).await;
    assert_eq!(job.agent, AgentId::Chatgpt);
    let mut first = first;
    assert_eq!(
        first.wait(Duration::from_secs(5)).await.unwrap().agent,
        AgentId::Grok
    );
    // free again
    w.judge.push_text(ACT);
    let mut r = req(Kind::Research, "notizie su Zig");
    r.to = Some(AgentId::Grok);
    assert_eq!(finish(w.d.start(r).await).await.status, JobStatus::Done);
}

#[tokio::test]
async fn budget_caps_pause_and_tell_the_owner_once() {
    let w = world(
        |c| {
            c.budget = Budget {
                total_calls_per_day: 1,
                ..Budget::default()
            }
        },
        &[(AgentId::Chatgpt, Duration::ZERO, Ok("x"))],
    );
    w.judge.push_text(ACT);
    finish(w.d.start(req(Kind::Question, "a?")).await).await;
    assert!(matches!(
        refused(w.d.start(req(Kind::Question, "b?")).await),
        Refusal::Paused(_)
    ));
    assert!(matches!(
        refused(w.d.start(req(Kind::Question, "c?")).await),
        Refusal::Paused(_)
    ));
    assert_eq!(w.sink.messages.lock().unwrap().len(), 1, "told once");
    assert_eq!(w.sink.notified.lock().unwrap().len(), 1);
    assert!(w.sink.messages.lock().unwrap()[0].contains("paused delegation"));
}

#[tokio::test]
async fn the_owner_switches_agents_off_and_on() {
    let w = world(
        |_| {},
        &[
            (AgentId::Grok, Duration::ZERO, Ok("grok")),
            (AgentId::Chatgpt, Duration::ZERO, Ok("chatgpt")),
        ],
    );
    let reply = w.d.switch(&SwitchIntent {
        switch: Switch::Off(Some(AgentId::Grok)),
        lang: "it",
    });
    // An Italian intent is answered in English.
    assert!(reply.contains("I will not use Grok"), "{reply}");
    let mut r = req(Kind::Research, "notizie");
    r.to = Some(AgentId::Grok);
    assert_eq!(
        refused(w.d.start(r).await),
        Refusal::AgentOff(AgentId::Grok)
    );
    w.judge.push_text(ACT);
    assert_eq!(
        finish(w.d.start(req(Kind::Research, "notizie")).await)
            .await
            .agent,
        AgentId::Chatgpt
    );
    w.d.switch(&SwitchIntent {
        switch: Switch::Off(None),
        lang: "en",
    });
    assert_eq!(
        refused(w.d.start(req(Kind::Research, "news")).await),
        Refusal::Stopped
    );
    w.d.switch(&SwitchIntent {
        switch: Switch::On(None),
        lang: "en",
    });
    w.d.switch(&SwitchIntent {
        switch: Switch::On(Some(AgentId::Grok)),
        lang: "en",
    });
    w.judge.push_text(ACT);
    assert_eq!(
        finish(w.d.start(req(Kind::Research, "news")).await)
            .await
            .agent,
        AgentId::Grok
    );
    let status = w.d.switch(&SwitchIntent {
        switch: Switch::Status,
        lang: "en",
    });
    assert!(status.starts_with("Delegations today: 2/200"), "{status}");
}

#[tokio::test]
async fn failures_are_reported_and_judge_outages_fall_back() {
    let w = world(
        |_| {},
        &[(
            AgentId::Chatgpt,
            Duration::ZERO,
            Err("codex gave no answer"),
        )],
    );
    w.judge.push(Err(lucina_llm::LlmError::Timeout));
    let job = finish(w.d.start(req(Kind::Question, "a?")).await).await;
    assert_eq!(job.status, JobStatus::Error);
    assert_eq!(job.error.as_deref(), Some("codex gave no answer"));
    assert_eq!(job.judgement.unwrap().source, "fallback");
    assert!(w.sink.cards.lock().unwrap()[0].plain.contains("⚠️"));
}

#[tokio::test]
async fn the_proactive_dry_run_plans_without_running() {
    let w = world(|_| {}, &[(AgentId::Grok, Duration::ZERO, Ok("x"))]);
    w.judge.push_text(ACT);
    let mut r = req(Kind::Research, "novità sul meteo a Copenaghen");
    r.origin = Origin::Proactive;
    let (agent, j) = w.d.plan(&r).await.unwrap();
    assert_eq!(agent, AgentId::Grok);
    assert_eq!(j.why, "Lo chiedo perché serve.");
    assert!(w.prompts.lock().unwrap().is_empty());
    assert_eq!(w.d.status().await["budget"]["totalCalls"], 0);
}
