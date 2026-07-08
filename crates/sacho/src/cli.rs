use std::process::ExitCode;

use clap::{Parser, Subcommand};
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, Report};
use sacho::commands::{
    AddOptions, CarryOptions, CheckOptions, CompileOptions, FormatOptions, NextOptions,
    ReleaseOptions, SyncOptions, SyncPlan, add_fragment, apply_sync, carry, check,
    compile_unreleased, format_fragments, plan_release, plan_sync, set_next_version,
};
use sacho::{Error, Repository};

#[derive(Debug, Parser)]
#[command(version, about = "Manage unreleased changelog fragments")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new changelog fragment.
    Add {
        /// Section id to place the fragment under.
        #[arg(long, help = "Section id to place the fragment under")]
        section: Option<String>,

        /// Topic-based fragment file name, without the .md extension.
        #[arg(help = "Topic-based fragment file name, without the .md extension")]
        name: String,
    },

    /// Set the next unreleased version.
    Next {
        /// Version label to write to the next-version file.
        #[arg(help = "Version label to write to the next-version file")]
        version: String,
    },

    /// Check fragments, materialized output, and missing-fragment policy.
    Check {
        /// Base revision for VCS-backed missing-fragment checks.
        #[arg(long, help = "Base revision for missing-fragment checks")]
        base: Option<String>,

        /// Repair mechanically fixable violations.
        #[arg(long, help = "Repair mechanically fixable violations")]
        fix: bool,
    },

    /// Format changelog fragments.
    Fmt,

    /// Print the compiled unreleased region.
    Preview {
        /// Section id to preview by itself.
        #[arg(long, help = "Section id to preview by itself")]
        section: Option<String>,
    },

    /// Regenerate the materialized unreleased changelog region.
    Sync {
        /// Apply the sync even when existing edits would be discarded.
        #[arg(long, help = "Apply even when existing edits would be discarded")]
        force: bool,
    },

    /// Compile fragments into a released changelog section.
    Release {
        /// Version to release; defaults to the next-version file.
        #[arg(help = "Version to release; defaults to the next-version file")]
        version: Option<String>,

        /// Release date in YYYY-MM-DD form.
        #[arg(long, help = "Release date in YYYY-MM-DD form")]
        date: Option<String>,

        /// Next unreleased version to write after release.
        #[arg(long, help = "Next unreleased version to write after release")]
        next: Option<String>,
    },

    /// Carry entries from a released section back into fragments.
    Carry {
        /// Released version whose entries should be carried.
        #[arg(help = "Released version whose entries should be carried")]
        version: String,
    },

    /// Resolve changelog merge conflicts by recompiling fragments.
    MergeDriver {
        /// Common ancestor file passed by the VCS merge driver.
        #[arg(help = "Common ancestor file passed by the VCS merge driver")]
        original: String,

        /// Current-side file passed by the VCS merge driver.
        #[arg(help = "Current-side file passed by the VCS merge driver")]
        current: String,

        /// Other-side file passed by the VCS merge driver.
        #[arg(help = "Other-side file passed by the VCS merge driver")]
        other: String,

        /// Repository path being merged.
        #[arg(help = "Repository path being merged")]
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
                for warning in &report.warnings {
                    eprintln!("warning: {}", warning.message);
                }
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
                let compiled = compile_unreleased(
                    &repo,
                    CompileOptions {
                        section,
                        include_empty_region: true,
                    },
                )?;
                print!("{}", compiled.markdown);
                Ok(ExitCode::SUCCESS)
            }
            Command::Sync { force } => {
                let plan = plan_sync(&repo, SyncOptions { force })?;
                if let SyncPlan::NeedsConfirmation { diff, .. } = &plan {
                    eprint!("{diff}");
                    eprintln!(
                        "sync may discard hand edits in the materialized changelog; rerun with --force to apply"
                    );
                    return Ok(ExitCode::from(2));
                }
                apply_sync(&repo, plan)?;
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
