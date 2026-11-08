//! Lucina Prompter end to end: a recorded take (`prompter-take.wav`, 26 s of
//! English) streamed in real time through the live socket, the real
//! whisper-server (small model) and the judge, with the app's tick rules.
//!
//! Two modes:
//! - by default the judge URL points nowhere, so every Jev call fails and the
//!   local matcher decides: a dead lucina-rs never stops the ticks. The matcher
//!   cannot see the last point ("skip" for "skipped", "mist row" for "missed
//!   row"), so it may stay open;
//! - with `PROMPTER_E2E_JUDGE_URL` (and `SECRETARY_API_TOKEN`) the live lucina-rs
//!   judge answers with Jev, and every spoken point must tick.
//!
//! It needs whisper.cpp and `~/models/whisper/ggml-small.bin` on nezopt, so it
//! runs only when asked:
//!
//!     PROMPTER_E2E=1 cargo test -p lucina-memo --test prompter_e2e -- --nocapture
//!     (set -a; . ~/.config/lucina-rs/lucina.env; PROMPTER_E2E=1 \
//!      PROMPTER_E2E_JUDGE_URL=http://127.0.0.1:8790/api/secretary/prompter/judge cargo test …)
use std::sync::Arc;

use lucina_memo::prompter::replay::{read_wav, replay};
use lucina_memo::{api, App, Config};

const SVC: &str = "prompter-e2e-service-token-0123456789";

/// What the speaker says (paraphrased in the WAV) and the one point he skips.
const SCRIPT: &str = "## Intro
- Lucina Prompter puts talking points on the screen
- Each point is crossed out while I talk
- It costs almost nothing per take
## How it works
- Whisper runs on my own server
- Audio stays on my machines
- Jev picks the point I just covered
- Skipped points land in a missed row
";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_take_ticks_the_spoken_points_in_order() {
    if std::env::var("PROMPTER_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set PROMPTER_E2E=1 on nezopt (needs whisper.cpp and the small model)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data").display().to_string();
    let home = std::env::var("HOME").unwrap();
    let server = std::env::var("PROMPTER_WHISPER_SERVER")
        .unwrap_or_else(|_| format!("{home}/Projects/AI/whisper.cpp/build/bin/whisper-server"));
    let judge = std::env::var("PROMPTER_E2E_JUDGE_URL").ok();
    let token =
        std::env::var("SECRETARY_API_TOKEN").unwrap_or_else(|_| "unused-judge-token".into());
    let cfg = Config::from_lookup(|k| match k {
        "HOME" => Some(home.clone()),
        "LUCINA_MEMO_DIR" => Some(data.clone()),
        "LUCINA_MEMO_OBSIDIAN_VAULT" => Some("off".into()),
        "LUCINA_PROMPTER_WHISPER_SERVER" => Some(server.clone()),
        // Not the live service's port.
        "LUCINA_PROMPTER_STT_PORT" => Some("28797".into()),
        "LUCINA_PROMPTER_JUDGE_URL" => Some(
            judge
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:9/api/secretary/prompter/judge".into()),
        ),
        "SECRETARY_API_TOKEN" => Some(token.clone()),
        _ => None,
    });
    let app: Arc<App> = App::new(
        cfg,
        Arc::new(lucina_llm::ScriptedModel::new()),
        "test".into(),
    )
    .unwrap()
    .with_tokens(None, Some(SVC));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = api::router(app.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let pcm = read_wav(include_bytes!("prompter-take.wav")).unwrap();
    let url = format!("ws://{addr}/v1/memos/prompter/live");
    let report = replay(
        &url,
        Some(SVC),
        SCRIPT,
        &pcm,
        true,
        &["Lucina".into(), "Jev".into(), "Whisper".into()],
    )
    .await;
    app.prompter.engine.shutdown().await;
    let report = report.unwrap();

    eprintln!("engine: {}", report.engine);
    for t in &report.transcript {
        eprintln!("heard: {t}");
    }
    for t in &report.ticks {
        eprintln!(
            "tick {:5} at {:6} ms, latency {:5} ms, {} p={:.2}",
            t.id, t.audio_ms, t.latency_ms, t.phase, t.p
        );
    }
    eprintln!(
        "missed: {:?}; jev calls {} failed {}",
        report.missed, report.jev_calls, report.jev_failed
    );

    let ticked: Vec<&str> = report.ticks.iter().map(|t| t.id.as_str()).collect();
    let said: &[&str] = if judge.is_some() {
        &["s0p0", "s0p1", "s1p0", "s1p1", "s1p2", "s1p3"]
    } else {
        &["s0p0", "s0p1", "s1p0", "s1p1", "s1p2"]
    };
    for id in said {
        assert!(
            ticked.contains(id),
            "{id} was said but not ticked: {ticked:?}"
        );
    }
    assert!(!ticked.contains(&"s0p2"), "the unsaid point must not tick");
    assert_eq!(
        report.missed.first().map(String::as_str),
        Some("s0p2"),
        "the skipped point goes to the missed row"
    );
    // Order: every Intro point before any How-it-works point.
    let pos = |id: &str| ticked.iter().position(|t| *t == id).unwrap();
    assert!(pos("s0p1") < pos("s1p0"));
    if judge.is_some() {
        assert!(report.jev_calls > 0, "Jev answered");
    } else {
        // Fail open: the judge was down the whole time, the matcher still ticked.
        assert!(report.jev_failed > 0);
    }
    // Speech to tick, from the end of the audio window the verdict is about.
    let mut lat: Vec<u64> = report.ticks.iter().map(|t| t.latency_ms).collect();
    lat.sort_unstable();
    let median = lat[lat.len() / 2];
    let limit = if report.engine == "gpu" { 1500 } else { 4000 };
    assert!(
        median <= limit,
        "median speech-to-tick {median} ms on {} (limit {limit})",
        report.engine
    );
}
