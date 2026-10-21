use clap::Parser;

fn main() -> std::process::ExitCode {
    match secretary_server::cli::main_with(secretary_server::cli::Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("secretary-server: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
