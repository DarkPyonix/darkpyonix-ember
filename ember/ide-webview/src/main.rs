//! Standalone native webview host binary for Ember's VS Code Web IDE window.

mod cli;
mod opener;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Taken first, before the logger or any thread starts, and removed from the environment
    // (FR-L7, #1064: the helper's page URL carries the manager's ticket).
    let env_url = cli::take_url_env();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let env_smoke = std::env::var(cli::SMOKE_ENV).ok();
    match cli::parse_args(std::env::args(), env_smoke.as_deref(), env_url.as_deref()) {
        Ok(cli::CliAction::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(cli::CliAction::Version(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(cli::CliAction::Run(config)) => match window::run(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("ember-ide-webview: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("ember-ide-webview: {err}");
            ExitCode::from(2)
        }
    }
}
