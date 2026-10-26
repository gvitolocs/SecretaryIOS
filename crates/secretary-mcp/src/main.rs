use clap::Parser;
use secretary_mcp::cli::{run_admin, Cli, Cmd};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "secretary_mcp=info,rmcp=warn".into()),
                )
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
                .init();
            let cfg = secretary_mcp::Config::from_env()?;
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?
                .block_on(secretary_mcp::serve(cfg))
        }
        cmd => std::process::exit(run_admin(cmd)?),
    }
}
