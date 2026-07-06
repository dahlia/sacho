use std::process::ExitCode;

use clap::{Parser, Subcommand};
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, Report};
use sacho::commands::{
    AddOptions, CarryOptions, CheckOptions, CompileOptions, FormatOptions, NextOptions,
    ReleaseOptions, SyncOptions, add_fragment, carry, check, compile_unreleased, format_fragments,
    plan_release, plan_sync, set_next_version,
};
use sacho::{Error, Repository};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Add {
        #[arg(long)]
        section: Option<String>,
        name: String,
    },
    Next {
        version: String,
    },
    Check {
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        fix: bool,
    },
    Fmt,
    Preview {
        #[arg(long)]
        section: Option<String>,
    },
    Sync {
        #[arg(long)]
        force: bool,
    },
    Release {
        version: Option<String>,
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        next: Option<String>,
    },
    Carry {
        version: String,
    },
    MergeDriver {
        original: String,
        current: String,
        other: String,
        path: String,
    },
}

impl Cli {
    pub fn run(self) -> Result<ExitCode, CliReport> {
        let repo = Repository::open_existing(".").map_err(CliReport::from)?;

        match self.command {
            Command::Add { section, name } => {
                let result = add_fragment(&repo, AddOptions { section, name })?;
                println!("{}", result.path.display());
                Ok(ExitCode::SUCCESS)
            }
            Command::Next { version } => {
                let result = set_next_version(&repo, NextOptions { version })?;
                println!("{}", result.path.display());
                Ok(ExitCode::SUCCESS)
            }
            Command::Check { base, fix } => {
                let report = check(&repo, CheckOptions { base, fix })?;
                if report.is_clean() {
                    Ok(ExitCode::SUCCESS)
                } else {
                    for violation in report.violations {
                        eprintln!("{}", violation.message);
                    }
                    Ok(ExitCode::from(1))
                }
            }
            Command::Fmt => {
                format_fragments(&repo, FormatOptions)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Preview { section } => {
                let compiled = compile_unreleased(&repo, CompileOptions { section })?;
                print!("{}", compiled.text);
                Ok(ExitCode::SUCCESS)
            }
            Command::Sync { force: _ } => {
                let _plan = plan_sync(&repo, SyncOptions)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Release {
                version,
                date,
                next,
            } => {
                let _plan = plan_release(
                    &repo,
                    ReleaseOptions {
                        version,
                        date,
                        next,
                    },
                )?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Carry { version } => {
                carry(&repo, CarryOptions { version })?;
                Ok(ExitCode::SUCCESS)
            }
            Command::MergeDriver {
                original: _,
                current: _,
                other: _,
                path: _,
            } => Err(Error::UnsupportedCommand {
                command: "merge-driver",
            }
            .into()),
        }
    }
}

#[derive(Debug, Diagnostic, thiserror::Error)]
#[error("{0}")]
#[diagnostic(code(sacho::error))]
pub struct CliReport(#[from] Error);

pub fn render_report(report: CliReport) {
    let mut output = String::new();
    let handler = GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor());
    if handler
        .render_report(&mut output, Report::new(report).as_ref())
        .is_ok()
    {
        eprint!("{output}");
    }
}
