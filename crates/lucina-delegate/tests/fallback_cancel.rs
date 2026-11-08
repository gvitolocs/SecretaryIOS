//! Out-of-credits backends are remembered and Mistral stands in for non-code work; a running job
//! can be cancelled. Mocked agents, a scripted Mistral model, a clock the test moves.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_delegate::agents::mistral::Mistral;
use lucina_delegate::agents::{Agent, AgentJob, AgentReply};
use lucina_delegate::config::{DelegateConfig, Isolation};
use lucina_delegate::engine::{chat_result, Card, Sink, CANCELLED};
use lucina_delegate::policy::Refusal;
use lucina_delegate::{AgentId, Delegator, JobRecord, JobStatus, Kind, Request, StartOutcome};
use lucina_llm::{ChatModel, ScriptedModel};

const OUT_OF_CREDITS: &str = "ERROR: Your workspace is out of credits";

/// Sets its flag when dropped: the agent's run future went away.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct Mock {
    id: AgentId,
    delay: Duration,
    /// Answers in order; the last one repeats.
    answers: Mutex<VecDeque<Result<String, String>>>,
    calls: AtomicUsize,
    dropped: Arc<AtomicBool>,
}

impl Mock {
    fn new(id: AgentId, delay: Duration, answers: &[Result<&str, &str>]) -> Arc<Self> {
        Arc::new(Self {
            id,
            delay,
            answers: Mutex::new(
                answers
                    .iter()
                    .map(|a| a.map(str::to_owned).map_err(str::to_owned))
                    .collect(),
            ),
            calls: AtomicUsize::new(0),
            dropped: Arc::new(AtomicBool::new(false)),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
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
    async fn run(&self, _job: &AgentJob) -> Result<AgentReply, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _flag = DropFlag(self.dropped.clone());
        tokio::time::sleep(self.delay).await;
        let answer = {
            let mut q = self.answers.lock().unwrap();
            if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q[0].clone()
            }
        };
        answer.map(|text| AgentReply {
            text,
            session: None,
            turns: 1,
            backend: "mock".into(),
        })
    }
}

#[derive(Default)]
struct Rec {
    cards: Mutex<Vec<Card>>,
}

#[async_trait::async_trait]
impl Sink for Rec {
    async fn save_note(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
        Ok("note".into())
    }
    async fn notify(&self, _: &str, _: &str) {}
    fn card(&self, card: &Card) {
        self.cards.lock().unwrap().push(card.clone());
    }
    fn message(&self, _: &str, _: &str) {}
}

struct World {
    d: Arc<Delegator>,
    sink: Arc<Rec>,
    mistral: Option<Arc<ScriptedModel>>,
    /// Seconds added to the engine's clock.
    offset: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn world(agents: &[Arc<Mock>], with_mistral: bool, isolation: Isolation) -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DelegateConfig::load(None, dir.path()).unwrap();
    cfg.sandbox_root = dir.path().join("sandboxes");
    cfg.isolation = isolation;
    cfg.inline_wait_secs = 5;
    cfg.judge = false;
    let mut list: Vec<Arc<dyn Agent>> =
        agents.iter().map(|a| a.clone() as Arc<dyn Agent>).collect();
    let mistral = with_mistral.then(|| Arc::new(ScriptedModel::new()));
    if let Some(m) = &mistral {
        list.push(Arc::new(Mistral::new(
            m.clone() as Arc<dyn ChatModel>,
            4000,
        )));
    }
    let sink = Arc::new(Rec::default());
    let offset = Arc::new(AtomicI64::new(0));
    let o = offset.clone();
    let d = Delegator::new(cfg, list, None, sink.clone(), chrono_tz::Europe::Rome).with_clock(
        Arc::new(move || chrono::Utc::now() + chrono::Duration::seconds(o.load(Ordering::SeqCst))),
    );
    World {
        d: Arc::new(d),
        sink,
        mistral,
        offset,
        _dir: dir,
    }
}

fn ask(to: AgentId, kind: Kind, task: &str) -> Request {
    Request {
        to: Some(to),
        session_id: "secretary_app".into(),
        ..Request::owner(kind, task)
    }
}

async fn finish(out: StartOutcome) -> JobRecord {
    match out {
        StartOutcome::Started(mut s) => s.wait(Duration::from_secs(10)).await.expect("finished"),
        StartOutcome::Skipped(j) => panic!("skipped: {j:?}"),
        StartOutcome::Refused(r) => panic!("refused: {r:?}"),
    }
}

fn refused(out: StartOutcome) -> Refusal {
    match out {
        StartOutcome::Refused(r) => r,
        _ => panic!("not refused"),
    }
}

#[tokio::test]
async fn out_of_credits_is_answered_by_mistral_and_remembered() {
    let chatgpt = Mock::new(
        AgentId::Chatgpt,
        Duration::ZERO,
        &[
            Err(OUT_OF_CREDITS),
            Err(OUT_OF_CREDITS),
            Ok("ChatGPT is back."),
        ],
    );
    let w = world(std::slice::from_ref(&chatgpt), true, Isolation::None);
    let m = w.mistral.clone().unwrap();
    m.push_text("Mistral's answer on Rust.")
        .push_text("Mistral again.")
        .push_text("Mistral a third time.");

    // 1. The run fails with "out of credits": Mistral answers the same job.
    let job = finish(
        w.d.start(ask(
            AgentId::Chatgpt,
            Kind::Question,
            "what is new in Rust?",
        ))
        .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(
        job.agent,
        AgentId::Chatgpt,
        "the job keeps the agent asked for"
    );
    assert_eq!(job.answered_by(), AgentId::Mistral);
    assert_eq!(job.backend, "mistral-api");
    assert_eq!(job.result.as_deref(), Some("Mistral's answer on Rust."));
    let fb = job.fallback.clone().expect("fallback");
    assert_eq!(fb.from, AgentId::Chatgpt);
    assert_eq!(fb.from_backend, "mock");
    assert_eq!(fb.reason, "out of credits");
    assert_eq!(chatgpt.calls(), 1);
    let plain = w.sink.cards.lock().unwrap().last().unwrap().plain.clone();
    assert!(plain.starts_with("I asked ChatGPT:"), "{plain}");
    assert!(
        plain.contains(
            "ChatGPT was out of credits (mock), so Mistral answered: Mistral's answer on Rust."
        ),
        "{plain}"
    );
    let v = chat_result(&job);
    assert_eq!(v["agent"], "ChatGPT");
    assert_eq!(v["answeredBy"], "Mistral");
    assert_eq!(
        v["fallback"],
        "ChatGPT was out of credits (mock), so Mistral answered"
    );
    let status = w.d.status().await;
    assert!(
        status["agents"]["chatgpt"]["health"]
            .as_str()
            .unwrap()
            .starts_with("down until "),
        "{status}"
    );
    assert_eq!(status["agents"]["mistral"]["health"], "ok");

    // 2. Remembered: the next request for ChatGPT goes to Mistral without trying ChatGPT.
    let job = finish(
        w.d.start(ask(AgentId::Chatgpt, Kind::Research, "and Go?"))
            .await,
    )
    .await;
    assert_eq!(chatgpt.calls(), 1, "ChatGPT was not called again");
    assert_eq!(job.answered_by(), AgentId::Mistral);
    assert_eq!(job.result.as_deref(), Some("Mistral again."));
    let fb = job.fallback.clone().unwrap();
    assert!(
        fb.reason.starts_with("out of credits since "),
        "{}",
        fb.reason
    );
    // Auto choice too: every preferred agent is down or missing.
    let job = finish(
        w.d.start(Request {
            session_id: "s".into(),
            ..Request::owner(Kind::Question, "and Zig?")
        })
        .await,
    )
    .await;
    assert_eq!(chatgpt.calls(), 1);
    assert_eq!(job.answered_by(), AgentId::Mistral);

    // 3. After the 6 hours ChatGPT is tried again (still out: Mistral once more).
    w.offset.store(7 * 3600, Ordering::SeqCst);
    m.push_text("Mistral, later.");
    let job = finish(
        w.d.start(ask(AgentId::Chatgpt, Kind::Question, "and C?"))
            .await,
    )
    .await;
    assert_eq!(chatgpt.calls(), 2, "tried again once the outage ran out");
    assert_eq!(job.answered_by(), AgentId::Mistral);
    assert_eq!(job.fallback.unwrap().reason, "out of credits");

    // 4. Later still it works again: its own answer, and the outage is cleared.
    w.offset.store(14 * 3600, Ordering::SeqCst);
    let job = finish(
        w.d.start(ask(AgentId::Chatgpt, Kind::Question, "and D?"))
            .await,
    )
    .await;
    assert_eq!(chatgpt.calls(), 3);
    assert_eq!(job.answered_by(), AgentId::Chatgpt);
    assert!(job.fallback.is_none());
    assert_eq!(job.result.as_deref(), Some("ChatGPT is back."));
    assert_eq!(w.d.status().await["agents"]["chatgpt"]["health"], "ok");
}

#[tokio::test]
async fn without_mistral_the_job_fails_and_the_backend_is_refused() {
    let chatgpt = Mock::new(AgentId::Chatgpt, Duration::ZERO, &[Err(OUT_OF_CREDITS)]);
    let w = world(std::slice::from_ref(&chatgpt), false, Isolation::None);
    let job = finish(
        w.d.start(ask(
            AgentId::Chatgpt,
            Kind::Question,
            "what is new in Rust?",
        ))
        .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Error);
    assert_eq!(job.error.as_deref(), Some(OUT_OF_CREDITS));
    assert_eq!(job.answered_by(), AgentId::Chatgpt);
    assert!(job.fallback.is_none());
    match refused(
        w.d.start(ask(AgentId::Chatgpt, Kind::Question, "again"))
            .await,
    ) {
        Refusal::Unavailable(AgentId::Chatgpt, why) => {
            assert!(why.starts_with("mock out of credits since "), "{why}");
            assert!(why.contains(OUT_OF_CREDITS), "{why}");
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(chatgpt.calls(), 1);
    // Named Mistral is not configured either: refused, no panic.
    assert!(matches!(
        refused(w.d.start(ask(AgentId::Mistral, Kind::Question, "x")).await),
        Refusal::Unavailable(AgentId::Mistral, _)
    ));
}

#[tokio::test]
async fn code_never_falls_back() {
    let claude = Mock::new(AgentId::Claude, Duration::ZERO, &[Err(OUT_OF_CREDITS)]);
    // Code needs the bubblewrap setting; the mock agent does not run anything.
    let w = world(std::slice::from_ref(&claude), true, Isolation::Bwrap);
    let job = finish(
        w.d.start(ask(AgentId::Claude, Kind::Code, "write a parser"))
            .await,
    )
    .await;
    assert_eq!(job.status, JobStatus::Error);
    assert_eq!(job.answered_by(), AgentId::Claude);
    assert!(job.fallback.is_none());
    assert!(matches!(
        refused(w.d.start(ask(AgentId::Claude, Kind::Code, "again")).await),
        Refusal::Unavailable(AgentId::Claude, _)
    ));
    assert!(matches!(
        refused(
            w.d.start(Request {
                session_id: "s".into(),
                ..Request::owner(Kind::Code, "auto")
            })
            .await
        ),
        Refusal::Unavailable(AgentId::Claude, _)
    ));
    assert!(
        w.mistral.unwrap().requests.lock().unwrap().is_empty(),
        "Mistral was never asked"
    );
    assert_eq!(claude.calls(), 1);
}

#[tokio::test]
async fn a_running_job_is_cancelled_and_the_agent_is_free_again() {
    let grok = Mock::new(AgentId::Grok, Duration::from_secs(60), &[Ok("too late")]);
    let w = world(std::slice::from_ref(&grok), false, Isolation::None);
    let StartOutcome::Started(mut s) = w.d.start(ask(AgentId::Grok, Kind::Research, "news")).await
    else {
        panic!("not started")
    };
    let id = s.job.id.clone();
    let sandbox = s.job.sandbox.clone().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(grok.calls(), 1);

    let t0 = Instant::now();
    let job = w.d.cancel(&id[..8], "en").await.expect("cancelled");
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert_eq!(job.status, JobStatus::Error);
    assert_eq!(job.error.as_deref(), Some(CANCELLED));
    assert_eq!(job.error.as_deref(), Some("cancelled by Giuseppe"));
    assert!(job.finished_at.is_some());
    assert!(
        grok.dropped.load(Ordering::SeqCst),
        "the agent's run was dropped"
    );
    assert!(!std::path::Path::new(&sandbox).exists(), "sandbox removed");
    assert_eq!(w.d.job(&id).unwrap().status, JobStatus::Error);
    let card = w.sink.cards.lock().unwrap().last().unwrap().plain.clone();
    assert!(card.contains("cancelled by Giuseppe"), "{card}");
    // Whoever waited on it hears it ended.
    let waited = s.wait(Duration::from_secs(1)).await.expect("told");
    assert_eq!(waited.error.as_deref(), Some(CANCELLED));

    // The claim is released: Grok takes a new job right away.
    let StartOutcome::Started(next) =
        w.d.start(ask(AgentId::Grok, Kind::Research, "more news"))
            .await
    else {
        panic!("Grok still busy")
    };
    assert!(w.d.cancel(&next.job.id, "en").await.is_ok());

    let again = w.d.cancel(&id[..8], "en").await.unwrap_err();
    assert!(again.starts_with("already error at "), "{again}");
    assert_eq!(
        w.d.cancel("ffffffff", "en").await.unwrap_err(),
        "no such job"
    );
}
