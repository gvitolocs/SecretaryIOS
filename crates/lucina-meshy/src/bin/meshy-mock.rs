//! `meshy-mock --bind 127.0.0.1:8797 --assets <fixtures>`: a local Meshy API
//! for end-to-end runs (Meshy retired its test-mode key). Any `msy_…` key
//! without "invalid" in it is accepted. See `lucina_meshy::mock`.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8797")]
    bind: std::net::SocketAddr,
    #[arg(long, default_value = "fixtures")]
    assets: PathBuf,
    /// Polls until a task succeeds.
    #[arg(long, default_value_t = 3)]
    polls: u32,
    /// Retry-After seconds on running tasks (0: none).
    #[arg(long, default_value_t = 1)]
    retry_after: u64,
    #[arg(long, default_value_t = 1000)]
    balance: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let h = lucina_meshy::mock::spawn(lucina_meshy::mock::MockConfig {
        bind: cli.bind,
        assets: cli.assets,
        polls_to_finish: cli.polls.max(1),
        retry_after: cli.retry_after,
        balance: cli.balance,
    })
    .await?;
    println!("meshy mock at {} (api base {})", h.base_url, h.api_base());
    tokio::signal::ctrl_c().await?;
    Ok(())
}
