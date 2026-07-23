use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use jiff::tz::TimeZone;
use jiff::{Timestamp, civil};
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme, Report};
use sacho::commands::{
    AddOptions, CarryOptions, CheckOptions, CheckReport, CompileOptions, FormatOptions,
    ImportUnreleasedOptions, ImportUnreleasedPlan, InitOptions, InitResult, MutationCleanupWarning,
    NextOptions, ReleaseDate, ReleaseOptions, ResolveLinksOptions, ResolveLinksResult, ShowOptions,
    SyncOptions, SyncPlan, add_fragment, apply_format, apply_import_unreleased, apply_merge_driver,
    apply_release, apply_resolve_links, apply_sync, carry, check, commit_message_hook,
    compile_unreleased_with_link_resolution, infer_repository_url, infer_section_paths,
    infer_section_pattern, init_repository, initialization_root, mercurial_update_hook,
    plan_format, plan_import_unreleased, plan_release, plan_release_with_link_resolution,
    plan_resolve_links, plan_sync, reference_transaction_hook, set_next_version, show,
    suggest_section_directory,
};
use sacho::link_resolution::LinkResolutionPolicy;
use sacho::merge::{MergeDriverOptions, MergeDriverResult};
use sacho::released::discover_section_candidates;
use sacho::{Error, Repository, SectionConfig, SectionPatternConfig};

const HELP_LICENSE_NOTICE: &str = "Copyright (C) 2026 Hong Minhee\n\
Sacho is free software under GNU GPLv3 only and comes with ABSOLUTELY NO WARRANTY.\n\
Run `sacho --license` for details.";
const LICENSE_NOTICE: &str = "Sacho  Copyright (C) 2026  Hong Minhee\n\
This program comes with ABSOLUTELY NO WARRANTY.\n\
This is free software, and you are welcome to redistribute it under the terms\n\
of the GNU General Public License, version 3 only. For details, see below.";
// Cargo verifies the extracted crate without access to files at the workspace
// root. Keep this path inside the crate; `mise run check` also verifies that
// this packaged copy matches the repository's top-level license.
const LICENSE_TEXT: &str = include_str!("../LICENSE");

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Manage unreleased changelog fragments",
    after_help = HELP_LICENSE_NOTICE
)]
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

        /// Install or update Git commit hooks that run Sacho checks.
        #[arg(long, help = "Install Git commit hooks")]
        install_hook: bool,

        /// Repository URL used for issue-reference links in new configuration.
        #[arg(
            long,
            value_name = "URL",
            help = "Repository URL for # links in new configuration"
        )]
        repository_url: Option<String>,

        /// Executable used by installed VCS integrations.
        #[arg(
            long,
            value_name = "PATH",
            help = "Executable used by installed VCS integrations"
        )]
        integration_executable: Option<PathBuf>,
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
        #[arg(
            long,
            conflicts_with = "staged",
            help = "Base revision for missing-fragment checks"
        )]
        base: Option<String>,

        /// Check staged Git changes for missing fragments.
        #[arg(
            long,
            conflicts_with = "base",
            help = "Check the staged Git index for missing fragments"
        )]
        staged: bool,

        /// Repair mechanically fixable violations.
        #[arg(long, help = "Repair mechanically fixable violations")]
        fix: bool,
    },

    /// Format changelog fragments.
    Fmt,

    /// Resolve and pin unpinned reference links.
    ResolveLinks,

    /// Print the compiled unreleased region.
    Preview {
        /// Section id to preview by itself.
        #[arg(long, help = "Section id to preview by itself")]
        section: Option<String>,

        /// Resolve unpinned reference links for this preview.
        #[arg(
            long,
            conflicts_with = "no_resolve_links",
            help = "Resolve unpinned reference links"
        )]
        resolve_links: bool,

        /// Do not resolve unpinned reference links.
        #[arg(
            long,
            conflicts_with = "resolve_links",
            help = "Do not resolve unpinned reference links"
        )]
        no_resolve_links: bool,

        /// Do not word-wrap the rendered Markdown.
        #[arg(long, help = "Do not word-wrap the output")]
        no_word_wrap: bool,
    },

    /// Print a released changelog section.
    Show {
        /// Released version whose section should be printed.
        #[arg(help = "Released version whose section should be printed")]
        version: String,

        /// Do not print the version heading.
        #[arg(short = 'H', long, help = "Do not print the version heading")]
        skip_heading: bool,

        /// File to receive the released section instead of standard output.
        #[arg(
            short,
            long,
            value_name = "PATH",
            help = "Write the released section to a file"
        )]
        output_file: Option<PathBuf>,

        /// Do not word-wrap the rendered Markdown.
        #[arg(long, help = "Do not word-wrap the output")]
        no_word_wrap: bool,
    },

    /// Regenerate the materialized unreleased changelog region.
    Sync {
        /// Apply the sync even when existing edits would be discarded.
        #[arg(long, help = "Apply even when existing edits would be discarded")]
        force: bool,

        /// Resolve and pin unpinned reference links before synchronizing.
        #[arg(
            long,
            conflicts_with = "no_resolve_links",
            help = "Resolve unpinned reference links"
        )]
        resolve_links: bool,

        /// Do not resolve unpinned reference links.
        #[arg(
            long,
            conflicts_with = "resolve_links",
            help = "Do not resolve unpinned reference links"
        )]
        no_resolve_links: bool,
    },

    /// Compile fragments into a released changelog section.
    Release {
        /// Version to release; defaults to the next-version file.
        #[arg(help = "Version to release; defaults to the next-version file")]
        version: Option<String>,

        /// Release date in YYYY-MM-DD form; defaults to the current local calendar date.
        #[arg(
            long,
            help = "Release date in YYYY-MM-DD form; defaults to the current local calendar date"
        )]
        date: Option<String>,

        /// Next unreleased version to write after release.
        #[arg(long, help = "Next unreleased version to write after release")]
        next: Option<String>,

        /// Allow a release without substantive changelog items.
        #[arg(long, help = "Allow a release without changelog items")]
        allow_empty: bool,

        /// Resolve unpinned reference links before releasing.
        #[arg(
            long,
            conflicts_with = "no_resolve_links",
            help = "Resolve unpinned reference links"
        )]
        resolve_links: bool,

        /// Do not resolve unpinned reference links.
        #[arg(
            long,
            conflicts_with = "resolve_links",
            help = "Do not resolve unpinned reference links"
        )]
        no_resolve_links: bool,
    },

    /// Carry entries from a released section back into fragments.
    Carry {
        /// Released version whose entries should be carried.
        #[arg(help = "Released version whose entries should be carried")]
        version: String,
    },

    /// Import the materialized unreleased region into fragments.
    ImportUnreleased {
        /// Apply even when normalized fragment output changes the region.
        #[arg(long, help = "Apply even when normalization changes the changelog")]
        force: bool,
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

    /// Print copyright and license information.
    #[command(long_flag = "license")]
    License,

    /// Run filesystem checks from Git's pre-commit hook.
    #[command(hide = true)]
    HookPreCommit,

    /// Arm the final commit check from Git's commit-msg hook.
    #[command(hide = true)]
    HookCommitMsg { message_file: PathBuf },

    /// Check the final commit from Git's reference-transaction hook.
    #[command(hide = true)]
    HookReferenceTransaction { phase: String },

    /// Synchronize the changelog after a successful Mercurial merge update.
    #[command(hide = true)]
    HookHgUpdate,
}

impl Cli {
    pub fn run(self) -> Result<ExitCode, CliReport> {
        match self.command {
            Command::Init {
                interactive,
                no_interactive,
                install_hook,
                repository_url,
                integration_executable,
            } => {
                let options = resolve_init_options(
                    interactive,
                    no_interactive,
                    install_hook,
                    repository_url,
                    integration_executable,
                )?;
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
                print_mutation_cleanup_warnings("sacho next", &result.cleanup_warnings);
                println!("{}", result.path.display());
                Ok(ExitCode::SUCCESS)
            }
            Command::Check { base, staged, fix } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                if fix {
                    let prepared = plan_format(&repo, FormatOptions)?;
                    if !confirm_sync_plan(
                        &prepared.sync,
                        Some("sacho check --fix"),
                        LinkResolutionPolicy::Configured,
                    )? {
                        return Ok(ExitCode::from(2));
                    }
                    let result = apply_format(&repo, prepared)?;
                    print_mutation_cleanup_warnings("sacho check --fix", &result.cleanup_warnings);
                }
                let report = check(
                    &repo,
                    CheckOptions {
                        base,
                        staged,
                        fix: false,
                    },
                )?;
                Ok(print_check_report(report, true))
            }
            Command::Fmt => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let plan = plan_format(&repo, FormatOptions)?;
                if !confirm_sync_plan(
                    &plan.sync,
                    Some("sacho fmt"),
                    LinkResolutionPolicy::Configured,
                )? {
                    return Ok(ExitCode::from(2));
                }
                let result = apply_format(&repo, plan)?;
                print_mutation_cleanup_warnings("sacho fmt", &result.cleanup_warnings);
                Ok(ExitCode::SUCCESS)
            }
            Command::ResolveLinks => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let plan = plan_resolve_links(&repo, ResolveLinksOptions::default())?;
                if !confirm_sync_plan(
                    &plan.sync,
                    Some("sacho resolve-links"),
                    LinkResolutionPolicy::Configured,
                )? {
                    return Ok(ExitCode::from(2));
                }
                let result = apply_resolve_links(&repo, plan)?;
                print_resolve_links_result(&repo, "sacho resolve-links", result);
                Ok(ExitCode::SUCCESS)
            }
            Command::Preview {
                section,
                resolve_links,
                no_resolve_links,
                no_word_wrap,
            } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let compiled = compile_unreleased_with_link_resolution(
                    &repo,
                    CompileOptions {
                        section,
                        include_empty_region: true,
                        word_wrap: !no_word_wrap,
                    },
                    link_resolution_policy(resolve_links, no_resolve_links),
                )?;
                print!("{}", compiled.markdown);
                Ok(ExitCode::SUCCESS)
            }
            Command::Show {
                version,
                skip_heading,
                output_file,
                no_word_wrap,
            } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let released = show(
                    &repo,
                    ShowOptions {
                        version,
                        skip_heading,
                        word_wrap: !no_word_wrap,
                    },
                )?;
                if let Some(path) = output_file {
                    std::fs::write(&path, released.markdown)
                        .map_err(|source| Error::WriteFile { path, source })?;
                } else {
                    print!("{}", released.markdown);
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::Sync {
                force,
                resolve_links,
                no_resolve_links,
            } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let policy = link_resolution_policy(resolve_links, no_resolve_links);
                if policy.is_enabled(repo.config()) {
                    let plan = plan_resolve_links(&repo, ResolveLinksOptions { force })?;
                    if !confirm_sync_plan(&plan.sync, None, policy)? {
                        return Ok(ExitCode::from(2));
                    }
                    let result = apply_resolve_links(&repo, plan)?;
                    print_resolve_links_result(&repo, "sacho sync", result);
                } else {
                    let plan = plan_sync(&repo, SyncOptions { force })?;
                    if !confirm_sync_plan(&plan, None, policy)? {
                        return Ok(ExitCode::from(2));
                    }
                    apply_sync(&repo, plan)?;
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::Release {
                version,
                date,
                next,
                allow_empty,
                resolve_links,
                no_resolve_links,
            } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let date = resolve_release_date(date)?;
                let options = ReleaseOptions {
                    version,
                    date,
                    next,
                    allow_empty,
                };
                let policy = link_resolution_policy(resolve_links, no_resolve_links);
                let plan = if policy.is_enabled(repo.config()) {
                    plan_release_with_link_resolution(&repo, options)?
                } else {
                    plan_release(&repo, options)?
                };
                apply_release(&repo, plan)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::Carry { version } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let result = carry(&repo, CarryOptions { version })?;
                print_mutation_cleanup_warnings("sacho carry", &result.cleanup_warnings);
                Ok(ExitCode::SUCCESS)
            }
            Command::ImportUnreleased { force } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let plan = plan_import_unreleased(&repo, ImportUnreleasedOptions { force })?;
                if !confirm_import_unreleased_plan(&plan)? {
                    return Ok(ExitCode::from(2));
                }
                let result = apply_import_unreleased(&repo, plan)?;
                print_mutation_cleanup_warnings(
                    "sacho import-unreleased",
                    &result.cleanup_warnings,
                );
                for path in result.written_fragments {
                    println!("{}", path.display());
                }
                Ok(ExitCode::SUCCESS)
            }
            Command::MergeDriver {
                original,
                current,
                other,
                path,
            } => run_merge_driver(original, current, other, path),
            Command::License => {
                print!("{LICENSE_NOTICE}\n\n{LICENSE_TEXT}");
                Ok(ExitCode::SUCCESS)
            }
            Command::HookPreCommit => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let report = check(&repo, CheckOptions::default())?;
                Ok(print_check_report(report, false))
            }
            Command::HookCommitMsg { message_file } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                commit_message_hook(&repo, &message_file)?;
                Ok(ExitCode::SUCCESS)
            }
            Command::HookReferenceTransaction { phase } => {
                let repo = Repository::open_existing(".").map_err(CliReport::from)?;
                let mut updates = String::new();
                io::stdin()
                    .read_to_string(&mut updates)
                    .map_err(|source| Error::ReadFile {
                        path: PathBuf::from("standard input"),
                        source,
                    })?;
                match reference_transaction_hook(&repo, &phase, &updates)? {
                    Some(report) => Ok(print_check_report(report, false)),
                    None => Ok(ExitCode::SUCCESS),
                }
            }
            Command::HookHgUpdate => {
                let parent2 = std::env::var("HG_PARENT2").ok();
                let hook_error = std::env::var("HG_ERROR").ok();
                if !mercurial_update_needs_repository(parent2.as_deref(), hook_error.as_deref()) {
                    return Ok(ExitCode::SUCCESS);
                }
                let repo = match Repository::open_existing(".") {
                    Ok(repo) => repo,
                    Err(Error::ConfigNotFound { .. }) => return Ok(ExitCode::SUCCESS),
                    Err(error) => return Err(CliReport::from(error)),
                };
                mercurial_update_hook(&repo, parent2.as_deref(), hook_error.as_deref())?;
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

fn print_mutation_cleanup_warnings(command: &str, warnings: &[MutationCleanupWarning]) {
    for warning in warnings {
        eprintln!(
            "warning: `{command}` committed successfully, but transaction cleanup failed for {}: {}",
            warning.path.display(),
            warning.message,
        );
    }
}

fn print_resolve_links_result(repo: &Repository, command: &str, result: ResolveLinksResult) {
    print_mutation_cleanup_warnings(command, &result.cleanup_warnings);
    for path in result.changed_fragments {
        println!("{}", path.display());
    }
    if result.changelog_changed {
        println!("{}", repo.config().changelog.path.display());
    }
}

fn resolve_release_date(date: Option<String>) -> Result<ReleaseDate, CliReport> {
    match date {
        Some(date) => Ok(ReleaseDate::parse(&date)?),
        None => local_release_date(),
    }
}

fn local_release_date() -> Result<ReleaseDate, CliReport> {
    let timestamp = Timestamp::try_from(SystemTime::now()).map_err(local_release_date_error)?;
    let time_zone = TimeZone::try_system().map_err(local_release_date_error)?;
    Ok(release_date_at(timestamp, &time_zone))
}

fn release_date_at(timestamp: Timestamp, time_zone: &TimeZone) -> ReleaseDate {
    let date: civil::Date = time_zone.to_datetime(timestamp).date();
    ReleaseDate {
        year: i32::from(date.year()),
        month: date.month() as u8,
        day: date.day() as u8,
    }
}

fn local_release_date_error(source: jiff::Error) -> CliReport {
    CliReport::LocalReleaseDate { source }
}

fn mercurial_update_needs_repository(parent2: Option<&str>, hook_error: Option<&str>) -> bool {
    hook_error == Some("0") && parent2.is_some_and(|parent| !parent.trim().is_empty())
}

fn print_check_report(report: CheckReport, show_skipped: bool) -> ExitCode {
    for warning in &report.warnings {
        eprintln!("warning: {}", warning.message);
    }
    if show_skipped {
        for skipped in &report.skipped {
            eprintln!("skipped: {}", skipped.message);
        }
    }
    if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        for violation in report.violations {
            eprintln!("{}", violation.message);
        }
        ExitCode::from(1)
    }
}

fn confirm_sync_plan(
    plan: &SyncPlan,
    formatting_command: Option<&str>,
    link_policy: LinkResolutionPolicy,
) -> Result<bool, CliReport> {
    let SyncPlan::NeedsConfirmation { diff, .. } = plan else {
        return Ok(true);
    };

    let interactive = sync_should_prompt(io::stdin().is_terminal(), io::stdout().is_terminal());
    if interactive {
        print!("{diff}");
        println!(
            "the synchronization shown above replaces the materialized unreleased region and may discard hand edits"
        );
    } else {
        eprint!("{diff}");
        eprintln!(
            "the synchronization shown above replaces the materialized unreleased region and may discard hand edits"
        );
        let sync_command = sync_force_command(link_policy);
        if let Some(command) = formatting_command {
            eprintln!(
                "run `{sync_command}`, then rerun `{command}` to apply all fixes non-interactively"
            );
        } else {
            eprintln!("rerun with `{sync_command}` to apply it non-interactively");
        }
        return Ok(false);
    }
    if prompt_bool("Apply this synchronization", false)? {
        Ok(true)
    } else {
        eprintln!("synchronization cancelled; the repository was not changed");
        Ok(false)
    }
}

fn sync_force_command(policy: LinkResolutionPolicy) -> &'static str {
    match policy {
        LinkResolutionPolicy::Always => "sacho sync --resolve-links --force",
        LinkResolutionPolicy::Never => "sacho sync --no-resolve-links --force",
        LinkResolutionPolicy::Configured => "sacho sync --force",
    }
}

fn link_resolution_policy(resolve_links: bool, no_resolve_links: bool) -> LinkResolutionPolicy {
    if resolve_links {
        LinkResolutionPolicy::Always
    } else if no_resolve_links {
        LinkResolutionPolicy::Never
    } else {
        LinkResolutionPolicy::Configured
    }
}

fn confirm_import_unreleased_plan(plan: &ImportUnreleasedPlan) -> Result<bool, CliReport> {
    if !plan.requires_confirmation() {
        return Ok(true);
    }
    let Some(diff) = &plan.diff else {
        return Ok(true);
    };
    let interactive = sync_should_prompt(io::stdin().is_terminal(), io::stdout().is_terminal());
    if interactive {
        print!("{diff}");
        println!("the imported fragments normalize the materialized unreleased region");
        if prompt_bool("Apply this import", false)? {
            return Ok(true);
        }
        eprintln!("import cancelled; the repository was not changed");
    } else {
        eprint!("{diff}");
        eprintln!("the imported fragments normalize the materialized unreleased region");
        eprintln!("rerun with `sacho import-unreleased --force` to apply it non-interactively");
    }
    Ok(false)
}

fn sync_should_prompt(stdin_terminal: bool, stdout_terminal: bool) -> bool {
    stdin_terminal && stdout_terminal
}

fn run_merge_driver(
    original: String,
    current: String,
    other: String,
    path: String,
) -> Result<ExitCode, CliReport> {
    let repo = Repository::open_existing(".").map_err(CliReport::from)?;
    match apply_merge_driver(
        &repo,
        MergeDriverOptions {
            ancestor: PathBuf::from(original),
            current: PathBuf::from(current),
            other: PathBuf::from(other),
            path: PathBuf::from(path),
        },
    ) {
        Ok(MergeDriverResult::Clean { hints, .. }) => {
            for hint in hints {
                eprintln!("{hint}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Ok(MergeDriverResult::Conflict { .. }) => Ok(ExitCode::from(1)),
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
    repository_url: Option<String>,
    integration_executable: Option<PathBuf>,
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
        let root = initialization_root(".");
        let (sections, section_patterns) = resolve_interactive_sections(
            &root,
            Path::new(&changelog_path),
            Path::new(&fragment_directory),
        )?;
        let repository_url = resolve_interactive_repository_url(repository_url)?;
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
            integration_executable,
            install_hook,
            append_existing_hook,
            repository_url,
            sections,
            section_patterns,
        })
    } else {
        Ok(InitOptions {
            changelog_path: None,
            fragment_directory: None,
            materialize: None,
            integration_executable,
            install_hook,
            append_existing_hook: false,
            repository_url,
            sections: Vec::new(),
            section_patterns: Vec::new(),
        })
    }
}

fn resolve_interactive_sections(
    root: &Path,
    changelog_path: &Path,
    fragment_directory: &Path,
) -> Result<(Vec<SectionConfig>, Vec<SectionPatternConfig>), CliReport> {
    if root.join(Repository::CONFIG_FILE).exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let path = root.join(changelog_path);
    if !path.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let changelog = std::fs::read_to_string(&path).map_err(|source| Error::ReadFile {
        path: path.clone(),
        source,
    })?;
    let document_title = root.file_name().and_then(|name| name.to_str()).map_or_else(
        || String::from("Changelog"),
        |name| format!("{name} changelog"),
    );
    let candidates = discover_section_candidates(&changelog, &document_title, "To be released.");
    if candidates.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    println!("Possible changelog sections:");
    for (index, candidate) in candidates.iter().enumerate() {
        let occurrence_word = if candidate.occurrences == 1 {
            "occurrence"
        } else {
            "occurrences"
        };
        let unreleased = if candidate.appears_in_unreleased {
            ", unreleased"
        } else {
            ""
        };
        println!(
            "  {}. {} ({} {}{})",
            index + 1,
            candidate.id,
            candidate.occurrences,
            occurrence_word,
            unreleased
        );
    }
    let selected = loop {
        let answer = prompt("Select sections by number, comma-separated", "")?;
        match parse_section_selection(&answer, candidates.len()) {
            Ok(selected) => break selected,
            Err(error) => eprintln!("{error}"),
        }
    };
    let selected_ids = selected
        .iter()
        .map(|index| candidates[*index].id.clone())
        .collect::<Vec<_>>();
    if let Some(pattern) = infer_section_pattern(root, &selected_ids, fragment_directory)?
        && prompt_bool(
            &format!(
                "Use inferred section pattern {} -> {}",
                pattern.source, pattern.id
            ),
            true,
        )?
    {
        return Ok((Vec::new(), vec![pattern]));
    }
    let mut sections = Vec::new();
    let mut used_directories = Vec::new();
    for index in selected {
        let candidate = &candidates[index];
        let suggested = suggest_section_directory(&candidate.id, &used_directories);
        let directory = PathBuf::from(prompt(
            &format!("Fragment directory for {}", candidate.id),
            &suggested.display().to_string(),
        )?);
        used_directories.push(directory.clone());
        let suggested_paths =
            infer_section_paths(root, &candidate.id, &directory, fragment_directory)?;
        let paths_answer = prompt(
            &format!("Paths for {} (comma-separated; '-' for none)", candidate.id),
            &suggested_paths.join(", "),
        )?;
        let paths = if paths_answer == "-" {
            Vec::new()
        } else {
            paths_answer
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect()
        };
        sections.push(SectionConfig {
            id: candidate.id.clone(),
            directory,
            paths,
        });
    }
    Ok((sections, Vec::new()))
}

fn parse_section_selection(answer: &str, candidate_count: usize) -> Result<Vec<usize>, CliReport> {
    if answer.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut selected = Vec::new();
    for part in answer.split(',') {
        let number = part.trim().parse::<usize>().map_err(|_| Error::Usage {
            message: format!("invalid section number {:?}", part.trim()),
        })?;
        if number == 0 || number > candidate_count {
            return Err(Error::Usage {
                message: format!("section number {number} is outside 1..={candidate_count}"),
            }
            .into());
        }
        let index = number - 1;
        if selected.contains(&index) {
            return Err(Error::Usage {
                message: format!("section number {number} was selected more than once"),
            }
            .into());
        }
        selected.push(index);
    }
    Ok(selected)
}

fn resolve_interactive_repository_url(
    explicit: Option<String>,
) -> Result<Option<String>, CliReport> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    let Some(inferred) = infer_repository_url(".") else {
        return prompt("Repository URL for # links", "").map(optional_prompt_value);
    };
    if prompt_bool(
        &format!("Use inferred repository URL {inferred} for # links"),
        true,
    )? {
        Ok(Some(inferred))
    } else {
        prompt("Repository URL for # links (leave blank to omit)", "").map(optional_prompt_value)
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
    for key in &result.local_hg_config_changes {
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
        && result.local_hg_config_changes.is_empty()
        && result.manual_actions_required.is_empty()
}

#[derive(Debug, Diagnostic, thiserror::Error)]
pub enum CliReport {
    #[error("{0}")]
    #[diagnostic(code(sacho::error))]
    Sacho(#[from] Error),

    #[error("could not determine the local calendar date; pass `--date YYYY-MM-DD`: {source}")]
    #[diagnostic(code(sacho::local_release_date))]
    LocalReleaseDate {
        #[source]
        source: jiff::Error,
    },
}

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
    fn release_date_at_uses_the_timezone_calendar_date_across_boundaries() {
        use jiff::Timestamp;
        use jiff::tz::{TimeZone, offset};

        let cases = [
            (
                "2026-12-31T15:30:00Z",
                TimeZone::fixed(offset(9)),
                "2027-01-01",
            ),
            (
                "2027-01-01T03:30:00Z",
                TimeZone::fixed(offset(-8)),
                "2026-12-31",
            ),
            (
                "2024-02-28T15:30:00Z",
                TimeZone::fixed(offset(9)),
                "2024-02-29",
            ),
        ];

        for (timestamp, time_zone, expected) in cases {
            let timestamp = timestamp.parse::<Timestamp>().expect("timestamp");
            assert_eq!(
                release_date_at(timestamp, &time_zone),
                expected.parse::<ReleaseDate>().expect("release date")
            );
        }
    }

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
    fn init_accepts_an_integration_executable_override() {
        let cli =
            Cli::try_parse_from(["sacho", "init", "--integration-executable", "tools/sacho-1"])
                .expect("integration executable option");

        let Command::Init {
            integration_executable,
            ..
        } = cli.command
        else {
            panic!("expected init command");
        };
        assert_eq!(integration_executable, Some(PathBuf::from("tools/sacho-1")));
    }

    #[test]
    fn link_resolution_flags_are_mutually_exclusive() {
        for command in ["preview", "sync", "release"] {
            let error =
                Cli::try_parse_from(["sacho", command, "--resolve-links", "--no-resolve-links"])
                    .expect_err("conflicting link resolution flags");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[test]
    fn link_resolution_flags_select_the_expected_policy() {
        assert_eq!(
            link_resolution_policy(true, false),
            LinkResolutionPolicy::Always
        );
        assert_eq!(
            link_resolution_policy(false, true),
            LinkResolutionPolicy::Never
        );
        assert_eq!(
            link_resolution_policy(false, false),
            LinkResolutionPolicy::Configured
        );
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
    fn sync_prompts_only_when_both_streams_are_terminals() {
        assert!(sync_should_prompt(true, true));
        assert!(!sync_should_prompt(true, false));
        assert!(!sync_should_prompt(false, true));
        assert!(!sync_should_prompt(false, false));
    }

    #[test]
    fn mercurial_update_needs_repository_only_for_successful_merges() {
        assert!(mercurial_update_needs_repository(Some("other"), Some("0")));
        assert!(!mercurial_update_needs_repository(None, Some("0")));
        assert!(!mercurial_update_needs_repository(Some(" "), Some("0")));
        assert!(!mercurial_update_needs_repository(Some("other"), Some("1")));
        assert!(!mercurial_update_needs_repository(Some("other"), None));
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
    fn section_selection_parser_preserves_order_and_rejects_bad_numbers() {
        assert_eq!(parse_section_selection("", 3).expect("empty"), Vec::new());
        assert_eq!(
            parse_section_selection("3, 1", 3).expect("selection"),
            vec![2, 0]
        );
        assert!(parse_section_selection("1,1", 3).is_err());
        assert!(parse_section_selection("4", 3).is_err());
        assert!(parse_section_selection("core", 3).is_err());
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
