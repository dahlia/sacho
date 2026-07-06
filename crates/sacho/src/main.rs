use clap::Parser;

mod cli;

fn main() -> std::process::ExitCode {
    match cli::Cli::parse().run() {
        Ok(code) => code,
        Err(report) => {
            cli::render_report(report);
            std::process::ExitCode::from(2)
        }
    }
}
