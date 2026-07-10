use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, Report};
use sacho::commands::{
    AddOptions, CarryOptions, CheckOptions, CompileOptions, FormatOptions, InitOptions, InitResult,
    NextOptions, ReleaseOptions, SyncOptions, SyncPlan, add_fragment, apply_release, apply_sync,
    carry, check, compile_unreleased, format_fragments, init_repository, plan_release, plan_sync,
    set_next_version,
};
use sacho::merge::{MergeDriverOptions, MergeDriverResult, merge_driver};
use sacho::{Error, Repository};

#[derive(Debug, Parser)]
#[command(version, about = "Manage unreleased changelog fragments")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Bootstrap Sacho configuration and repository integration.
    Init {
        /// Ask setup questions even when auto-detection would not.
        #[arg(long, help = "Ask setup questions")]
        interactive: bool,

        /// Do not ask setup questions.
        #[arg(long, help = "Do not ask setup questions")]
        no_interactive: bool,

        /// Install or update a pre-commit hook that runs sacho check.
        #[arg(long, help = "Install a pre-commit hook")]
        install_hook: bool,
    },

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
        match self.command {
            Command::Init {
                interactive,
                no_interactive,
                install_hook,
            } => {
                let options = resolve_init_options(interactive, no_interactive, install_hook)?;
                let result = init_repository(".", options)?;
                print_init_result(&result);
                Ok(ExitCode::SUCCESS)
            }
            Command::Add { section, name } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let result = add_fragment(&repo, AddOptions { section, name })?;
                println!("{}", result.path.display());
                Ok(ExitCode::SUCCESS)
            }
            Command::Next { version } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let result = set_next_version(&repo, NextOptions { version })?;
                println!("{}", result.path.display());
                Ok(ExitCode::SUCCESS)
            }
            Command::Check { base, fix } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let report = check(&repo, CheckOptions { base, fix })?;
                for warning in &report.warnings {
                    eprintln!("warning: {}", warning.message);
                }
                for skipped in &report.skipped {
                    eprintln!("skipped: {}", skipped.message);
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
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                format_fragments(&repo, FormatOptions)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Preview { section } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
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
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
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
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let plan = plan_release(
                    &repo,
                    ReleaseOptions {
                        version,
                        date,
                        next,
                    },
                )?;
                apply_release(&repo, plan)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Carry { version } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                carry(&repo, CarryOptions { version })?;
                Ok(ExitCode::SUCCESS)
            }
            Command::MergeDriver {
                original,
                current,
                other,
                path,
            } => run_merge_driver(original, current, other, path),
        }
    }
}

fn run_merge_driver(
    original: String,
    current: String,
    other: String,
    path: String,
) -> Result<ExitCode, CliReport> {
    let repo = Repository::open_existing(".").map_err(CliReport::from)?;
    let current_path = PathBuf::from(&current);
    match merge_driver(
        &repo,
        MergeDriverOptions {
            ancestor: PathBuf::from(original),
            current: current_path.clone(),
            other: PathBuf::from(other),
            path: PathBuf::from(path),
        },
    ) {
        Ok(MergeDriverResult::Clean { output, hints }) => {
            std::fs::write(&current_path, output).map_err(|source| {
                CliReport::from(Error::WriteFile {
                    path: current_path.clone(),
                    source,
                })
            })?;
            for hint in hints {
                eprintln!("{hint}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Ok(MergeDriverResult::Conflict {
            output_with_markers,
            reason: _,
        }) => {
            std::fs::write(&current_path, output_with_markers).map_err(|source| {
                CliReport::from(Error::WriteFile {
                    path: current_path.clone(),
                    source,
                })
            })?;
            Ok(ExitCode::from(1))
        }
        Err(error) => {
            eprintln!("{error}");
            Ok(ExitCode::from(1))
        }
    }
}

fn resolve_init_options(
    interactive: bool,
    no_interactive: bool,
    install_hook: bool,
) -> Result<InitOptions, CliReport> {
    let should_prompt = init_should_prompt(
        interactive,
        no_interactive,
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
    )?;

    if should_prompt {
        let changelog_path = prompt("Changelog path", "CHANGES.md")?;
        let fragment_directory = prompt("Fragment directory", "changes.d")?;
        let materialize = prompt_bool("Materialize unreleased changelog", true)?;
        let repository_url = prompt("Repository URL for # links", "")?;
        let install_hook = if install_hook {
            true
        } else {
            prompt_bool("Install pre-commit hook", false)?
        };
        let append_existing_hook = if install_hook {
            prompt_bool("Append to existing unmarked hook", false)?
        } else {
            false
        };
        Ok(InitOptions {
            changelog_path: Some(PathBuf::from(changelog_path)),
            fragment_directory: Some(PathBuf::from(fragment_directory)),
            materialize: Some(materialize),
            install_hook,
            append_existing_hook,
            repository_url: optional_prompt_value(repository_url),
        })
    } else {
        Ok(InitOptions {
            changelog_path: None,
            fragment_directory: None,
            materialize: None,
            install_hook,
            append_existing_hook: false,
            repository_url: None,
        })
    }
}

fn init_should_prompt(
    interactive: bool,
    no_interactive: bool,
    stdin_terminal: bool,
    stdout_terminal: bool,
) -> Result<bool, CliReport> {
    if interactive && no_interactive {
        return Err(Error::Usage {
            message: "--interactive and --no-interactive cannot be used together".to_owned(),
        }
        .into());
    }
    let auto_interactive = stdin_terminal && stdout_terminal;
    if interactive {
        if !auto_interactive {
            return Err(Error::TerminalRequired {
                message: "--interactive requires terminal stdin and stdout".to_owned(),
            }
            .into());
        }
        Ok(true)
    } else if no_interactive {
        Ok(false)
    } else {
        Ok(auto_interactive)
    }
}

fn prompt(label: &str, default: &str) -> Result<String, CliReport> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    io::stdout().flush().map_err(|source| Error::WriteFile {
        path: "<stdout>".into(),
        source,
    })?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|source| Error::ReadFile {
            path: "<stdin>".into(),
            source,
        })?;
    let answer = answer.trim();
    if answer.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(answer.to_owned())
    }
}

fn prompt_bool(label: &str, default: bool) -> Result<bool, CliReport> {
    let default_text = if default { "Y/n" } else { "y/N" };
    loop {
        print!("{label} [{default_text}]: ");
        io::stdout().flush().map_err(|source| Error::WriteFile {
            path: "<stdout>".into(),
            source,
        })?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|source| Error::ReadFile {
                path: "<stdin>".into(),
                source,
            })?;
        let answer = answer.trim();
        if answer.is_empty() {
            return Ok(default);
        }
        let Some(value) = parse_bool_answer(answer, default) else {
            eprintln!("answer yes or no");
            continue;
        };
        return Ok(value);
    }
}

fn optional_prompt_value(value: String) -> Option<String> {
    match value.trim() {
        "" => None,
        _ => Some(value),
    }
}

fn parse_bool_answer(answer: &str, default: bool) -> Option<bool> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Some(default);
    }
    match answer.to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

fn print_init_result(result: &InitResult) {
    for path in &result.created_files {
        println!("created {}", path.display());
    }
    for path in &result.modified_files {
        println!("modified {}", path.display());
    }
    for key in &result.local_git_config_changes {
        println!("configured {key}");
    }
    for action in &result.manual_actions_required {
        println!("manual {action}");
    }
    if init_result_has_no_actions(result) {
        println!("already initialized");
    }
}

fn init_result_has_no_actions(result: &InitResult) -> bool {
    result.created_files.is_empty()
        && result.modified_files.is_empty()
        && result.local_git_config_changes.is_empty()
        && result.manual_actions_required.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_prompt_mode_rejects_conflicting_flags() {
        let error = init_should_prompt(true, true, true, true).expect_err("conflict");

        assert!(error.to_string().contains("cannot be used together"));
    }

    #[test]
    fn init_prompt_mode_requires_both_terminals_for_interactive_override() {
        assert!(init_should_prompt(true, false, true, false).is_err());
        assert!(init_should_prompt(true, false, false, true).is_err());
        assert!(init_should_prompt(true, false, true, true).expect("prompt"));
    }

    #[test]
    fn init_prompt_mode_auto_detects_only_when_both_streams_are_terminals() {
        assert!(init_should_prompt(false, false, true, true).expect("prompt"));
        assert!(!init_should_prompt(false, false, true, false).expect("no prompt"));
        assert!(!init_should_prompt(false, false, false, true).expect("no prompt"));
        assert!(!init_should_prompt(false, true, true, true).expect("disabled"));
    }

    #[test]
    fn bool_prompt_parser_accepts_yes_no_and_default() {
        assert_eq!(parse_bool_answer("", true), Some(true));
        assert_eq!(parse_bool_answer(" ", false), Some(false));
        assert_eq!(parse_bool_answer("y", false), Some(true));
        assert_eq!(parse_bool_answer("yes", false), Some(true));
        assert_eq!(parse_bool_answer("N", true), Some(false));
        assert_eq!(parse_bool_answer("no", true), Some(false));
        assert_eq!(parse_bool_answer("maybe", true), None);
    }

    #[test]
    fn optional_prompt_value_drops_blank_answers() {
        assert_eq!(optional_prompt_value(String::from("   ")), None);
        assert_eq!(
            optional_prompt_value(String::from("https://example.com/repo")),
            Some(String::from("https://example.com/repo"))
        );
    }

    #[test]
    fn init_result_no_actions_checks_all_reported_action_groups() {
        assert!(init_result_has_no_actions(&InitResult::default()));

        let mut result = InitResult::default();
        result.created_files.push(PathBuf::from("sacho.toml"));
        assert!(!init_result_has_no_actions(&result));

        let mut result = InitResult::default();
        result.modified_files.push(PathBuf::from(".gitattributes"));
        assert!(!init_result_has_no_actions(&result));

        let mut result = InitResult::default();
        result
            .local_git_config_changes
            .push(String::from("merge.sacho.driver"));
        assert!(!init_result_has_no_actions(&result));

        let mut result = InitResult::default();
        result
            .manual_actions_required
            .push(String::from("configure merge driver"));
        assert!(!init_result_has_no_actions(&result));
    }
}
