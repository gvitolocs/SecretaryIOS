//! One real computer-use turn, opt-in.
//!
//! It spends a Gemini-backed Hermes turn and drives the shared Cua desktop,
//! so it is `#[ignore]` by default and also wants `LUCINA_COMPUTER_USE_LIVE=1`:
//!
//! ```text
//! LUCINA_COMPUTER_USE_LIVE=1 LUCINA_COMPUTER_USE_ENABLED=true \
//!   cargo test -p lucina-computer-use --test live -- --ignored --nocapture
//! ```
//!
//! It runs `uname -a` in `local:lucina-workspace`, which proves the whole
//! chain: launcher → Hermes → Cua plugin → container → receipts.

use lucina_computer_use::{Cancel, Client, ComputerUseConfig, Status};

#[tokio::test]
#[ignore = "runs a real Hermes computer-use turn in the Cua sandbox"]
async fn uname_in_the_workspace() {
    if std::env::var("LUCINA_COMPUTER_USE_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set LUCINA_COMPUTER_USE_LIVE=1 to spend a real turn");
        return;
    }
    let cfg = ComputerUseConfig::from_lookup(&|k| std::env::var(k).ok());
    assert!(
        cfg.ready(),
        "LUCINA_COMPUTER_USE_ENABLED must be true and {} must exist",
        cfg.launcher.display()
    );
    let client = Client::new(cfg);
    let result = client
        .run("Run `uname -a` in the workspace and report the output.", &Cancel::new())
        .await
        .expect("the computer-use task runs");
    eprintln!("{}", result.to_json());
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert!(
        result.reply.to_lowercase().contains("linux"),
        "the workspace is a Linux container: {}",
        result.reply
    );
}
