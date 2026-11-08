use clap::Parser;
use lucina_agentd::cli::{render, run, Cli};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let result = run(&cli).await;
    std::process::exit(render(&cli, result));
}
