use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use hongdown::{
    DashSetting, IndentWidth, LeadingSpaces, LineWidth, Options as HongdownOptions, TrailingSpaces,
    UnorderedMarker, format,
};
use indexmap::IndexSet;
use serde_yaml_ng::{Mapping, Value};
use similar::TextDiff;

use crate::changelog::{ChangelogError, replace_unreleased_region};
use crate::config::{RegionDetection, VcsPreset};
use crate::error::{Error, Result};
use crate::fragment::{
    DiscoveryWarning, FragmentWarning, discover_fragment_candidates, parse_fragment,
};
use crate::released::carry_release;
use crate::repo::Repository;
use crate::vcs::{ChangeKind, ChangedPath, CommitId, GitVcs, Vcs};

pub use crate::compile::{CompileOptions, CompiledRegion};

/// Command execution context shared by command APIs.
#[derive(Debug, Clone)]
pub struct CommandContext {
    /// Repository the command operates on.
    pub repo: Repository,
}

/// Options for creating a changelog fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOptions {
    /// Optional section identifier for repositories with sections.
    pub section: Option<String>,
    /// Fragment file stem to create.
    pub name: String,
}

/// Result of creating a changelog fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddResult {
    /// Path of the created fragment.
    pub path: PathBuf,
}

/// Options for setting the next unreleased version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextOptions {
    /// Version string to write into the next-version file.
    pub version: String,
}

/// Result of setting the next unreleased version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextResult {
    /// Path of the next-version file that was written.
    pub path: PathBuf,
}

/// Options for formatting fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatOptions;

/// Result of formatting fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatResult {
    /// Fragment paths whose contents changed.
    pub changed: Vec<PathBuf>,
}

/// Options for planning a changelog synchronization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOptions {
    /// Apply the sync even when the existing region may contain hand edits.
    pub force: bool,
}

/// Pending file write prepared by a command plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWrite {
    /// Repository-relative path that will be written.
    pub path: PathBuf,

    /// File contents before the planned write.
    pub old_contents: String,

    /// File contents after the planned write.
    pub new_contents: String,
}

/// Reason a sync plan was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncSkipReason {
    /// Materialized changelogs are disabled by configuration.
    MaterializationDisabled,

    /// The changelog already matches the compiled fragment output.
    AlreadyCurrent,
}

/// Risk that requires caller confirmation before applying a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRisk {
    /// The existing materialized region may contain hand edits.
    PossibleHandEdits,
}

/// Planned synchronization action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPlan {
    /// The changelog can be updated without confirmation.
    Apply(PendingWrite),

    /// Applying the sync may discard hand edits and needs confirmation.
    NeedsConfirmation {
        /// Pending write that would synchronize the changelog.
        pending: PendingWrite,

        /// Diff showing the content that would be replaced.
        diff: String,

        /// Reason confirmation is required.
        reason: SyncRisk,
    },

    /// No write is needed.
    Skipped(SyncSkipReason),
}

/// Result of applying a synchronization plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncResult {
    /// Whether applying the plan changed the changelog file.
    pub changed: bool,
}

/// Options for planning a release.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseOptions {
    /// Explicit version to release.
    pub version: Option<String>,
    /// Release date in `YYYY-MM-DD` form.
    pub date: Option<String>,
    /// Next unreleased version to write after releasing.
    pub next: Option<String>,
}

/// Planned release operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePlan {
    /// Version that will be released.
    pub version: String,

    /// Date line that will be written into the released section.
    pub date: ReleaseDate,

    /// Next unreleased version to write after release.
    pub next: Option<String>,

    /// Markdown for the released changelog section.
    pub released_markdown: String,

    /// Fragment files consumed by the release.
    pub consumed_fragments: Vec<PathBuf>,
}

/// Calendar date used for a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseDate {
    /// Four-digit year.
    pub year: i32,

    /// Month number in the range 1 through 12.
    pub month: u8,

    /// Day of month in the valid range for `month`.
    pub day: u8,
}

impl ReleaseDate {
    fn parse(source: &str) -> Result<Self> {
        let parts = source.split('-').collect::<Vec<_>>();
        if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
            return Err(Error::InvalidReleaseDate {
                date: source.to_owned(),
            });
        }
        let year = parts[0]
            .parse::<i32>()
            .map_err(|_| Error::InvalidReleaseDate {
                date: source.to_owned(),
            })?;
        let month = parts[1]
            .parse::<u8>()
            .map_err(|_| Error::InvalidReleaseDate {
                date: source.to_owned(),
            })?;
        let day = parts[2]
            .parse::<u8>()
            .map_err(|_| Error::InvalidReleaseDate {
                date: source.to_owned(),
            })?;
        let date = Self { year, month, day };
        if !date.is_valid() {
            return Err(Error::InvalidReleaseDate {
                date: source.to_owned(),
            });
        }
        Ok(date)
    }

    fn today_utc() -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_secs();
        civil_from_days((seconds / 86_400) as i64)
    }

    fn long_form(self) -> String {
        format!("{} {}, {}", month_name(self.month), self.day, self.year)
    }

    fn is_valid(self) -> bool {
        (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
    }
}

/// Result of applying a release plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseResult {
    /// Paths changed by the release operation.
    pub changed_paths: Vec<PathBuf>,
}

/// Options for carrying entries from a released section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryOptions {
    /// Released version to decompile back into fragments.
    pub version: String,
}

/// Result of carrying released entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CarryResult {
    /// Fragment paths written by the carry operation.
    pub written_fragments: Vec<PathBuf>,

    /// Synchronization result when materialization is enabled.
    pub sync: Option<SyncResult>,
}

/// Options for running repository checks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckOptions {
    /// Optional base revision for VCS-backed missing-fragment checks.
    pub base: Option<String>,
    /// Whether mechanically fixable violations should be repaired.
    pub fix: bool,
}

/// Report produced by repository checks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckReport {
    /// Violations found by the check command.
    pub violations: Vec<CheckViolation>,

    /// Non-fatal warnings found by the check command.
    pub warnings: Vec<CheckWarning>,

    /// Checks skipped because they do not apply to this repository.
    pub skipped: Vec<SkippedCheck>,
}

impl CheckReport {
    /// Returns the overall check status.
    pub fn status(&self) -> CheckStatus {
        if !self.violations.is_empty() {
            CheckStatus::HasViolations
        } else if !self.warnings.is_empty() {
            CheckStatus::HasWarnings
        } else {
            CheckStatus::Clean
        }
    }

    /// Returns true when the report contains no violations.
    pub fn is_clean(&self) -> bool {
        self.status() != CheckStatus::HasViolations
    }
}

/// Overall status of a check report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// No violations or warnings were found.
    Clean,

    /// Warnings were found, but no violations.
    HasWarnings,

    /// At least one violation was found.
    HasViolations,
}

/// A single check violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckViolation {
    /// Human-readable violation message.
    pub message: String,
}

/// A single check warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckWarning {
    /// Human-readable warning message.
    pub message: String,
}

/// A check that did not run because it does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCheck {
    /// Human-readable skip message.
    pub message: String,
}

/// Creates a changelog fragment.
pub fn add_fragment(repo: &Repository, options: AddOptions) -> Result<AddResult> {
    let name = validate_fragment_name(&options.name)?;
    let config = repo.config();
    let directory = if config.sections.is_empty() {
        if options.section.is_some() {
            return Err(Error::UnexpectedSection);
        }
        config.fragments.directory.clone()
    } else {
        let section_id = options.section.as_deref().ok_or(Error::MissingSection)?;
        let section = config
            .sections
            .iter()
            .find(|section| section.id == section_id)
            .ok_or_else(|| Error::UnknownSection {
                section: section_id.to_owned(),
            })?;
        config.fragments.directory.join(&section.directory)
    };
    let path = directory.join(name);
    let absolute = repo.resolve(&path);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&absolute)
        .map_err(|source| {
            if source.kind() == ErrorKind::AlreadyExists {
                Error::FragmentAlreadyExists { path: path.clone() }
            } else {
                Error::WriteFile {
                    path: absolute.clone(),
                    source,
                }
            }
        })?;
    file.write_all(b" -\n").map_err(|source| Error::WriteFile {
        path: absolute,
        source,
    })?;
    Ok(AddResult { path })
}

/// Sets the next unreleased version.
pub fn set_next_version(repo: &Repository, options: NextOptions) -> Result<NextResult> {
    let version = options.version.trim();
    if version.is_empty() {
        return Err(Error::EmptyNextVersion);
    }
    ensure_materialized_current_before_mutation(repo)?;
    let path = next_version_path(repo);
    repo.atomic_write(&path, format!("{version}\n").as_bytes())?;
    sync_after_mutation(repo)?;
    Ok(NextResult { path })
}

/// Formats all changelog fragments into normal form.
pub fn format_fragments(repo: &Repository, _options: FormatOptions) -> Result<FormatResult> {
    ensure_materialized_current_before_mutation(repo)?;
    let discovered = discover_fragment_candidates(repo)?;
    let mut changed = Vec::new();

    for candidate in discovered.candidates {
        let source = fs::read_to_string(&candidate.path).map_err(|source| Error::ReadFile {
            path: candidate.path.clone(),
            source,
        })?;
        let formatted = format_fragment_source(&source)
            .map_err(|error| with_fragment_path(error, candidate.relative_path.clone()))?;
        parse_fragment(
            candidate.relative_path.clone(),
            &formatted,
            candidate.section,
            &repo.config().links,
        )
        .map_err(|source| Error::Fragment {
            path: candidate.relative_path.clone(),
            source,
        })?;
        if source != formatted {
            repo.atomic_write(&candidate.relative_path, formatted.as_bytes())?;
            changed.push(candidate.relative_path);
        }
    }

    sync_after_mutation(repo)?;
    Ok(FormatResult { changed })
}

/// Compiles the current fragments into an unreleased changelog region.
pub fn compile_unreleased(repo: &Repository, options: CompileOptions) -> Result<CompiledRegion> {
    crate::compile::compile_unreleased(repo, options)
}

/// Plans a synchronization between fragments and the materialized changelog.
pub fn plan_sync(repo: &Repository, options: SyncOptions) -> Result<SyncPlan> {
    let config = repo.config();
    if !config.changelog.materialize {
        return Ok(SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled));
    }

    let compiled = compile_unreleased(repo, CompileOptions::default())?;
    let changelog_path = config.changelog.path.clone();
    let absolute_path = repo.resolve(&changelog_path);
    let old_contents = fs::read_to_string(&absolute_path).map_err(|source| Error::ReadFile {
        path: absolute_path,
        source,
    })?;
    let replacement = replace_unreleased_region(
        &old_contents,
        &compiled.markdown,
        config.changelog.region_detection,
        &config.changelog.unreleased_heading,
    )
    .map_err(|source| changelog_error(changelog_path.clone(), source))?;

    if old_contents == replacement.new_contents {
        return Ok(SyncPlan::Skipped(SyncSkipReason::AlreadyCurrent));
    }

    let pending = PendingWrite {
        path: changelog_path,
        old_contents,
        new_contents: replacement.new_contents,
    };
    if options.force {
        Ok(SyncPlan::Apply(pending))
    } else {
        let diff = unified_diff(&pending.old_contents, &pending.new_contents);
        Ok(SyncPlan::NeedsConfirmation {
            pending,
            diff,
            reason: SyncRisk::PossibleHandEdits,
        })
    }
}

/// Applies a previously planned synchronization.
pub fn apply_sync(repo: &Repository, plan: SyncPlan) -> Result<SyncResult> {
    let pending = match plan {
        SyncPlan::Apply(pending)
        | SyncPlan::NeedsConfirmation {
            pending,
            diff: _,
            reason: _,
        } => pending,
        SyncPlan::Skipped(_) => return Ok(SyncResult { changed: false }),
    };
    if pending.old_contents == pending.new_contents {
        return Ok(SyncResult { changed: false });
    }

    repo.atomic_write(&pending.path, pending.new_contents.as_bytes())?;
    Ok(SyncResult { changed: true })
}

/// Plans a release operation.
pub fn plan_release(repo: &Repository, options: ReleaseOptions) -> Result<ReleasePlan> {
    ensure_materialized_current_before_mutation(repo)?;

    let next_version = read_next_version(repo)?;
    let version = match (options.version.as_deref().map(str::trim), next_version) {
        (Some(""), _) => return Err(Error::MissingReleaseVersion),
        (Some(version), Some(next_version)) if version != next_version => {
            return Err(Error::ReleaseVersionMismatch {
                version: version.to_owned(),
                next_version,
            });
        }
        (Some(version), _) => version.to_owned(),
        (None, Some(next_version)) => next_version,
        (None, None) => return Err(Error::MissingReleaseVersion),
    };
    let date = match options.date {
        Some(date) => ReleaseDate::parse(&date)?,
        None => ReleaseDate::today_utc(),
    };
    let compiled = compile_unreleased(repo, CompileOptions::default())?;
    let released_markdown = released_markdown(&compiled.markdown, &version, date, repo);
    let consumed_fragments = discover_fragment_candidates(repo)?
        .candidates
        .into_iter()
        .map(|candidate| candidate.relative_path)
        .collect();

    Ok(ReleasePlan {
        version,
        date,
        next: options.next.map(|next| next.trim().to_owned()),
        released_markdown,
        consumed_fragments,
    })
}

/// Applies a previously planned release.
pub fn apply_release(repo: &Repository, plan: ReleasePlan) -> Result<ReleaseResult> {
    let config = repo.config();
    let changelog_path = config.changelog.path.clone();
    let old_changelog = match fs::read_to_string(repo.resolve(&changelog_path)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound && !config.changelog.materialize => {
            initial_changelog(&config.changelog.title)
        }
        Err(source) => {
            return Err(Error::ReadFile {
                path: repo.resolve(&changelog_path),
                source,
            });
        }
    };

    let new_changelog = if config.changelog.materialize {
        let empty_unreleased = empty_unreleased_markdown(repo, plan.next.as_deref());
        replace_region_for_release(
            &old_changelog,
            &empty_unreleased,
            &plan.released_markdown,
            config.changelog.region_detection,
            &config.changelog.unreleased_heading,
        )
        .map_err(|source| changelog_error(changelog_path.clone(), source))?
    } else {
        insert_released_section(&old_changelog, &plan.released_markdown)
    };
    repo.atomic_write(&changelog_path, new_changelog.as_bytes())?;

    for fragment in &plan.consumed_fragments {
        match fs::remove_file(repo.resolve(fragment)) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::WriteFile {
                    path: repo.resolve(fragment),
                    source,
                });
            }
        }
    }
    write_next_after_release(repo, plan.next.as_deref())?;

    let mut changed_paths = vec![changelog_path];
    changed_paths.extend(plan.consumed_fragments);
    changed_paths.push(next_version_path(repo));
    Ok(ReleaseResult { changed_paths })
}

/// Carries entries from an existing release into unreleased fragments.
pub fn carry(repo: &Repository, options: CarryOptions) -> Result<CarryResult> {
    ensure_materialized_current_before_mutation(repo)?;

    let changelog_path = repo.config().changelog.path.clone();
    let changelog =
        fs::read_to_string(repo.resolve(&changelog_path)).map_err(|source| Error::ReadFile {
            path: repo.resolve(&changelog_path),
            source,
        })?;
    let carried = carry_release(repo, &changelog, &options.version)?;
    for fragment in &carried.fragments {
        parse_fragment(
            fragment.path.clone(),
            &fragment.markdown,
            fragment.section.clone(),
            &repo.config().links,
        )
        .map_err(|source| Error::Fragment {
            path: fragment.path.clone(),
            source,
        })?;
    }

    let mut written_fragments = Vec::new();
    for fragment in carried.fragments {
        repo.atomic_write(&fragment.path, fragment.markdown.as_bytes())?;
        written_fragments.push(fragment.path);
    }

    let sync = if repo.config().changelog.materialize {
        let plan = plan_sync(repo, SyncOptions { force: true })?;
        Some(apply_sync(repo, plan)?)
    } else {
        None
    };

    Ok(CarryResult {
        written_fragments,
        sync,
    })
}

/// Checks fragments, materialized output, and missing-fragment policy.
pub fn check(repo: &Repository, options: CheckOptions) -> Result<CheckReport> {
    if options.fix {
        ensure_materialized_current_before_mutation(repo)?;
        format_fragments(repo, FormatOptions)?;
        if repo.config().changelog.materialize {
            let plan = plan_sync(repo, SyncOptions { force: true })?;
            apply_sync(repo, plan)?;
        }
        return check(
            repo,
            CheckOptions {
                fix: false,
                ..options
            },
        );
    }

    let discovered = discover_fragment_candidates(repo)?;
    let mut violations = Vec::new();
    let mut warnings = discovered
        .warnings
        .iter()
        .map(discovery_warning)
        .collect::<Vec<_>>();
    let mut skipped = Vec::new();

    for candidate in discovered.candidates {
        let source = fs::read_to_string(&candidate.path).map_err(|source| Error::ReadFile {
            path: candidate.path.clone(),
            source,
        })?;
        let formatted = format_fragment_source(&source);
        match parse_fragment(
            candidate.relative_path.clone(),
            &source,
            candidate.section,
            &repo.config().links,
        ) {
            Ok(fragment) => {
                warnings.extend(
                    fragment
                        .warnings
                        .iter()
                        .map(|warning| fragment_warning(&candidate.relative_path, warning)),
                );
                match formatted {
                    Ok(formatted) if formatted != source => violations.push(CheckViolation {
                        message: format!(
                            "{}: fragment formatting differs from normal form; run `sacho fmt`",
                            candidate.relative_path.display()
                        ),
                    }),
                    Ok(_) => {}
                    Err(error) => violations.push(CheckViolation {
                        message: format!("{}: {error}", candidate.relative_path.display()),
                    }),
                }
            }
            Err(error) => {
                violations.push(CheckViolation {
                    message: format!("{}: {error}", candidate.relative_path.display()),
                });
            }
        }
    }

    if let Some(warning) = next_file_whitespace_warning(repo)? {
        warnings.push(warning);
    }

    if violations.is_empty() {
        match plan_sync(repo, SyncOptions { force: false })? {
            SyncPlan::NeedsConfirmation { diff, .. } => {
                violations.push(CheckViolation {
                    message: format!(
                        "materialized changelog is out of sync with fragments; run `sacho sync --force`\n{diff}"
                    ),
                });
            }
            SyncPlan::Apply(_) => {
                violations.push(CheckViolation {
                    message: String::from(
                        "materialized changelog is out of sync with fragments; run `sacho sync --force`",
                    ),
                });
            }
            SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled) => {
                skipped.push(SkippedCheck {
                    message: String::from(
                        "materialized changelog consistency skipped because materialize = false",
                    ),
                });
            }
            SyncPlan::Skipped(SyncSkipReason::AlreadyCurrent) => {}
        }
    }

    run_missing_fragment_check(repo, options.base.as_deref(), &mut violations, &mut skipped)?;

    Ok(CheckReport {
        violations,
        warnings,
        skipped,
    })
}

fn run_missing_fragment_check(
    repo: &Repository,
    base: Option<&str>,
    violations: &mut Vec<CheckViolation>,
    skipped: &mut Vec<SkippedCheck>,
) -> Result<()> {
    let Some(base) = base else {
        skipped.push(SkippedCheck {
            message: String::from("missing-fragment check skipped because --base was not supplied"),
        });
        return Ok(());
    };
    if repo.config().check.paths.is_empty() {
        skipped.push(no_checked_paths_skip());
        return Ok(());
    }
    match repo.config().vcs.preset {
        VcsPreset::Git => {
            let vcs = GitVcs::new(repo.root());
            let report = missing_fragment_violations(repo, &vcs, base)?;
            violations.extend(report.violations);
            skipped.extend(report.skipped);
        }
        VcsPreset::None => skipped.push(SkippedCheck {
            message: String::from("missing-fragment check skipped because vcs.preset = \"none\""),
        }),
        VcsPreset::Jj | VcsPreset::Hg => skipped.push(SkippedCheck {
            message: format!(
                "missing-fragment check skipped because vcs.preset = {:?} is not implemented",
                repo.config().vcs.preset
            ),
        }),
    }
    Ok(())
}

fn missing_fragment_violations(
    repo: &Repository,
    vcs: &impl Vcs,
    base: &str,
) -> Result<MissingFragmentReport> {
    if repo.config().check.paths.is_empty() {
        return Ok(MissingFragmentReport {
            violations: Vec::new(),
            skipped: vec![no_checked_paths_skip()],
        });
    }
    let source_patterns = compile_glob_set(&repo.config().check.paths)?;
    let section_patterns = repo
        .config()
        .sections
        .iter()
        .map(|section| Ok((section.id.as_str(), compile_glob_set(&section.paths)?)))
        .collect::<Result<Vec<_>>>()?;
    let commits = vcs.commits(base)?;
    let final_fragments = final_fragment_paths(repo)?;
    let mut violations = Vec::new();
    let mut skipped = Vec::new();

    for commit in commits {
        let message = vcs.message(&commit)?;
        if message_exempts_changelog(&message) {
            continue;
        }
        let changed_paths = vcs.changed_paths(&commit)?;
        let mut relevant_paths = changed_paths
            .iter()
            .flat_map(policy_paths)
            .filter(|path| source_patterns.is_match(path))
            .cloned()
            .collect::<Vec<_>>();
        relevant_paths.sort();
        relevant_paths.dedup();
        if relevant_paths.is_empty() {
            continue;
        }
        let changed_fragments = changed_fragment_changes(repo, &changed_paths);
        let requirements =
            missing_fragment_requirements(repo, &commit, &relevant_paths, &section_patterns);
        skipped.extend(requirements.skipped);

        for requirement in requirements.requirements {
            if !requirement_satisfied(&requirement, &changed_fragments, &final_fragments) {
                violations.push(CheckViolation {
                    message: missing_fragment_message(&commit, &requirement),
                });
            }
        }
    }

    Ok(MissingFragmentReport {
        violations,
        skipped,
    })
}

fn no_checked_paths_skip() -> SkippedCheck {
    SkippedCheck {
        message: String::from("missing-fragment check skipped because check.paths is empty"),
    }
}

fn compile_glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|source| Error::InvalidGlob {
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| Error::InvalidGlob {
        pattern: patterns.join(", "),
        source,
    })
}

fn changed_fragment_changes(repo: &Repository, paths: &[ChangedPath]) -> Vec<FragmentChange> {
    paths
        .iter()
        .filter(|path| fragment_content_changed(path))
        .filter_map(|path| fragment_change_for_path(repo, &path.path))
        .collect()
}

fn fragment_content_changed(path: &ChangedPath) -> bool {
    match path.kind {
        ChangeKind::Added | ChangeKind::Modified => true,
        ChangeKind::Copied | ChangeKind::Renamed => {
            path.similarity.is_some_and(|value| value < 100)
        }
        ChangeKind::Deleted | ChangeKind::Other => false,
    }
}

fn policy_paths(path: &ChangedPath) -> impl Iterator<Item = &PathBuf> {
    std::iter::once(&path.path).chain(
        path.old_path
            .as_ref()
            .filter(|_| path.kind == ChangeKind::Renamed),
    )
}

fn final_fragment_paths(repo: &Repository) -> Result<IndexSet<PathBuf>> {
    Ok(discover_fragment_candidates(repo)?
        .candidates
        .iter()
        .map(|candidate| candidate.relative_path.clone())
        .collect())
}

fn fragment_change_for_path(repo: &Repository, path: &Path) -> Option<FragmentChange> {
    fragment_target_for_path(repo, path).map(|target| FragmentChange {
        path: path.to_path_buf(),
        target,
    })
}

fn fragment_target_for_path(repo: &Repository, path: &Path) -> Option<FragmentTarget> {
    let config = repo.config();
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return None;
    }
    if !path.starts_with(&config.fragments.directory) {
        return None;
    }
    if config.sections.is_empty() {
        if path.parent() != Some(config.fragments.directory.as_path()) {
            return None;
        }
        return Some(FragmentTarget::Repository);
    }
    for section in &config.sections {
        let directory = config.fragments.directory.join(&section.directory);
        if path.parent() == Some(directory.as_path()) {
            return Some(FragmentTarget::Section(section.id.clone()));
        }
    }
    if path.parent().and_then(Path::parent) == Some(config.fragments.directory.as_path()) {
        return Some(FragmentTarget::Repository);
    }
    None
}

fn requirement_satisfied(
    requirement: &MissingFragmentRequirement,
    changed_fragments: &[FragmentChange],
    final_fragments: &IndexSet<PathBuf>,
) -> bool {
    changed_fragments.iter().any(|fragment| {
        fragment_matches_requirement(fragment, &requirement.target)
            && final_fragments.contains(&fragment.path)
    })
}

fn fragment_matches_requirement(fragment: &FragmentChange, requirement: &FragmentTarget) -> bool {
    match requirement {
        FragmentTarget::Repository => true,
        FragmentTarget::Section(section) => match &fragment.target {
            FragmentTarget::Section(fragment_section) => fragment_section == section,
            FragmentTarget::Repository => false,
        },
    }
}

fn missing_fragment_requirements(
    repo: &Repository,
    _commit: &CommitId,
    paths: &[PathBuf],
    section_patterns: &[(&str, GlobSet)],
) -> MissingFragmentRequirements {
    if repo.config().sections.is_empty() {
        return MissingFragmentRequirements {
            requirements: vec![MissingFragmentRequirement::repository(
                paths.to_vec(),
                false,
            )],
            skipped: Vec::new(),
        };
    }

    let mut repository_paths = Vec::new();
    let mut section_paths = section_patterns
        .iter()
        .map(|(section, _)| ((*section).to_owned(), Vec::new()))
        .collect::<Vec<_>>();
    for path in paths {
        let matched_sections = section_patterns
            .iter()
            .enumerate()
            .filter(|(_, (_, patterns))| patterns.is_match(path))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matched_sections.is_empty() {
            repository_paths.push(path.clone());
        } else {
            for index in matched_sections {
                section_paths[index].1.push(path.clone());
            }
        }
    }

    let mut requirements = section_paths
        .into_iter()
        .filter(|(_, paths)| !paths.is_empty())
        .map(|(section, paths)| MissingFragmentRequirement {
            target: FragmentTarget::Section(section),
            paths,
            sectioned_repository: false,
        })
        .collect::<Vec<_>>();
    if !repository_paths.is_empty() {
        requirements.push(MissingFragmentRequirement::repository(
            repository_paths,
            true,
        ));
    }
    MissingFragmentRequirements {
        requirements,
        skipped: Vec::new(),
    }
}

fn missing_fragment_message(commit: &CommitId, requirement: &MissingFragmentRequirement) -> String {
    let paths = path_sample(&requirement.paths);
    let scope = match &requirement.target {
        FragmentTarget::Repository => String::from("repository"),
        FragmentTarget::Section(section) => format!("section {section:?}"),
    };
    format!(
        "{}: missing changelog fragment for {scope}; affected paths: {paths}; run `{}` or add `Changelog: none` to the commit message",
        commit.as_str(),
        requirement.suggested_command()
    )
}

fn path_sample(paths: &[PathBuf]) -> String {
    let mut sample = paths
        .iter()
        .take(3)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if paths.len() > sample.len() {
        sample.push(format!("and {} more", paths.len() - sample.len()));
    }
    sample.join(", ")
}

fn message_exempts_changelog(message: &str) -> bool {
    const SKIP_MARKERS: [&str; 4] = [
        "[changelog skip]",
        "[changes skip]",
        "[skip changelog]",
        "[skip changes]",
    ];

    let lowercase = message.to_lowercase();
    if SKIP_MARKERS.iter().any(|marker| lowercase.contains(marker)) {
        return true;
    }
    message.lines().any(|line| {
        let Some((key, value)) = line.split_once(':') else {
            return false;
        };
        key.trim().eq_ignore_ascii_case("changelog") && value.trim().eq_ignore_ascii_case("none")
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FragmentTarget {
    Repository,
    Section(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FragmentChange {
    path: PathBuf,
    target: FragmentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MissingFragmentRequirement {
    target: FragmentTarget,
    paths: Vec<PathBuf>,
    sectioned_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct MissingFragmentReport {
    violations: Vec<CheckViolation>,
    skipped: Vec<SkippedCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct MissingFragmentRequirements {
    requirements: Vec<MissingFragmentRequirement>,
    skipped: Vec<SkippedCheck>,
}

impl MissingFragmentRequirement {
    fn repository(paths: Vec<PathBuf>, sectioned_repository: bool) -> Self {
        Self {
            target: FragmentTarget::Repository,
            paths,
            sectioned_repository,
        }
    }

    fn suggested_command(&self) -> String {
        match &self.target {
            FragmentTarget::Repository if self.sectioned_repository => {
                String::from("sacho add --section <section-id> <topic-name>")
            }
            FragmentTarget::Repository => String::from("sacho add <topic-name>"),
            FragmentTarget::Section(section) => {
                format!("sacho add --section {section} <topic-name>")
            }
        }
    }
}

fn validate_fragment_name(name: &str) -> Result<String> {
    if name.is_empty() {
        return Err(Error::InvalidFragmentName {
            name: name.to_owned(),
            reason: "name must not be empty",
        });
    }
    if name.contains('/') {
        return Err(Error::InvalidFragmentName {
            name: name.to_owned(),
            reason: "name must not contain path separators",
        });
    }
    if name.contains('\\') {
        return Err(Error::InvalidFragmentName {
            name: name.to_owned(),
            reason: "name must not contain path separators",
        });
    }
    let path = Path::new(name);
    if path.components().any(|component| {
        !matches!(
            component,
            Component::Normal(_)
                if component.as_os_str() != ".." && !component.as_os_str().is_empty()
        )
    }) {
        return Err(Error::InvalidFragmentName {
            name: name.to_owned(),
            reason: "name must be a single normal path component",
        });
    }
    if name == "." {
        return Err(Error::InvalidFragmentName {
            name: name.to_owned(),
            reason: "name must not be .",
        });
    }
    if name.ends_with(".md") {
        Ok(name.to_owned())
    } else {
        Ok(format!("{name}.md"))
    }
}

fn next_version_path(repo: &Repository) -> PathBuf {
    repo.config()
        .fragments
        .directory
        .join(&repo.config().fragments.next_file)
}

fn read_next_version(repo: &Repository) -> Result<Option<String>> {
    let path = next_version_path(repo);
    let absolute = repo.resolve(&path);
    match fs::read_to_string(&absolute) {
        Ok(contents) => {
            let values = contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            match values.as_slice() {
                [] => Ok(None),
                [value] => Ok(Some((*value).to_owned())),
                _ => Err(Error::InvalidNextVersion { path: absolute }),
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::ReadFile {
            path: absolute,
            source,
        }),
    }
}

fn write_next_after_release(repo: &Repository, next: Option<&str>) -> Result<()> {
    let path = next_version_path(repo);
    let absolute = repo.resolve(&path);
    match next {
        Some(next) if !next.is_empty() => {
            repo.atomic_write(&path, format!("{}\n", next.trim()).as_bytes())?;
        }
        _ => match fs::remove_file(&absolute) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::WriteFile {
                    path: absolute,
                    source,
                });
            }
        },
    }
    Ok(())
}

fn released_markdown(
    unreleased_markdown: &str,
    version: &str,
    date: ReleaseDate,
    repo: &Repository,
) -> String {
    let release_heading = format!("Version {version}");
    let release_underline = "-".repeat(release_heading.len());
    let released_date = format!("Released on {}.", date.long_form());
    let unreleased_date = &repo.config().changelog.unreleased_heading;

    let mut lines = unreleased_markdown
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if lines.len() >= 2 {
        lines[0] = release_heading;
        lines[1] = release_underline;
    }
    let mut markdown = lines.join("\n");
    markdown.push('\n');
    markdown = markdown.replacen(unreleased_date, &released_date, 1);
    markdown
}

fn empty_unreleased_markdown(repo: &Repository, next: Option<&str>) -> String {
    let heading = next
        .map(str::trim)
        .filter(|next| !next.is_empty())
        .map_or_else(String::new, |next| format!("Version {next}"));
    let heading = if heading.is_empty() {
        String::from("Unreleased")
    } else {
        heading
    };
    let mut markdown = String::new();
    markdown.push_str(&heading);
    markdown.push('\n');
    markdown.push_str(&"-".repeat(heading.len()));
    markdown.push_str("\n\n");
    markdown.push_str(&repo.config().changelog.unreleased_heading);
    markdown.push('\n');
    markdown
}

fn replace_region_for_release(
    source: &str,
    empty_unreleased: &str,
    released: &str,
    detection: RegionDetection,
    unreleased_heading: &str,
) -> std::result::Result<String, ChangelogError> {
    match detection {
        RegionDetection::Heading => {
            let replacement = format!("{}\n\n{}", empty_unreleased.trim_end(), released);
            replace_unreleased_region(source, &replacement, detection, unreleased_heading)
                .map(|replacement| replacement.new_contents)
        }
        RegionDetection::Marker => {
            let replacement =
                replace_unreleased_region(source, empty_unreleased, detection, unreleased_heading)?;
            Ok(insert_released_after_marker_region(
                &replacement.new_contents,
                released,
            ))
        }
    }
}

fn insert_released_after_marker_region(source: &str, released: &str) -> String {
    const END_MARKER: &str = "<!-- sacho:unreleased:end -->";

    let Some(marker_index) = source.find(END_MARKER) else {
        return insert_released_section(source, released);
    };
    let after_marker = marker_index + END_MARKER.len();
    let insertion = source[after_marker..]
        .find('\n')
        .map_or(source.len(), |offset| {
            after_marker + offset.saturating_add(1)
        });

    let mut output = String::with_capacity(source.len() + released.len() + 2);
    output.push_str(&source[..insertion]);
    if !output.ends_with("\n\n") {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }
    output.push_str(released.trim_end());
    output.push_str("\n\n");
    output.push_str(source[insertion..].trim_start_matches(['\n', '\r']));
    output
}

fn insert_released_section(source: &str, released: &str) -> String {
    let insertion = insertion_index_after_title(source);
    let mut output = String::with_capacity(source.len() + released.len() + 2);
    output.push_str(&source[..insertion]);
    if !output.ends_with("\n\n") {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }
    output.push_str(released.trim_end());
    output.push_str("\n\n");
    output.push_str(source[insertion..].trim_start_matches(['\n', '\r']));
    output
}

fn insertion_index_after_title(source: &str) -> usize {
    let lines = source_lines_with_offsets(source);
    if lines.len() >= 2 && is_setext_title_underline(lines[1].text) {
        return skip_blank_lines(&lines, 2).map_or(source.len(), |index| lines[index].start);
    }
    if lines
        .first()
        .is_some_and(|line| line.text.trim_start().starts_with("# "))
    {
        return skip_blank_lines(&lines, 1).map_or(source.len(), |index| lines[index].start);
    }
    0
}

fn initial_changelog(title: &str) -> String {
    let mut changelog = String::new();
    changelog.push_str(title);
    changelog.push('\n');
    changelog.push_str(&"=".repeat(title.len()));
    changelog.push_str("\n\n");
    changelog
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    start: usize,
    text: &'a str,
}

fn source_lines_with_offsets(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for raw in source.split_inclusive('\n') {
        let end = start + raw.len();
        let text = raw.trim_end_matches(['\n', '\r']);
        lines.push(SourceLine { start, text });
        start = end;
    }
    lines
}

fn skip_blank_lines(lines: &[SourceLine<'_>], start: usize) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, line)| (!line.text.trim().is_empty()).then_some(index))
}

fn is_setext_title_underline(text: &str) -> bool {
    let text = text.trim();
    text.starts_with('=') && text.trim_matches('=').is_empty()
}

fn civil_from_days(days_since_epoch: i64) -> ReleaseDate {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    ReleaseDate {
        year: year as i32,
        month: month as u8,
        day: day as u8,
    }
}

fn month_name(month: u8) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => unreachable!("validated month"),
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn sync_after_mutation(repo: &Repository) -> Result<()> {
    if repo.config().changelog.materialize {
        let plan = plan_sync(repo, SyncOptions { force: true })?;
        apply_sync(repo, plan)?;
    }
    Ok(())
}

fn ensure_materialized_current_before_mutation(repo: &Repository) -> Result<()> {
    if !repo.config().changelog.materialize {
        return Ok(());
    }

    match plan_sync(repo, SyncOptions { force: false })? {
        SyncPlan::Skipped(_) | SyncPlan::Apply(_) => Ok(()),
        SyncPlan::NeedsConfirmation { .. } => Err(Error::SyncNeedsConfirmation {
            path: repo.config().changelog.path.clone(),
        }),
    }
}

fn format_fragment_source(source: &str) -> Result<String> {
    let parsed = parse_frontmatter_for_format(source)?;
    let body = format(parsed.body, &hongdown_options()).map_err(|source| Error::Format {
        source: Box::new(source),
    })?;
    let mut output = canonical_frontmatter(parsed.frontmatter)?;
    output.push_str(body.trim_end());
    output.push('\n');
    Ok(output)
}

struct FragmentFormatSource<'a> {
    frontmatter: Vec<FrontmatterEntry>,
    body: &'a str,
}

struct FrontmatterEntry {
    key: Value,
    value: Value,
}

fn parse_frontmatter_for_format(source: &str) -> Result<FragmentFormatSource<'_>> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(rest) = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
    else {
        if source.strip_suffix('\r').unwrap_or(source) == "---" {
            return Err(Error::Fragment {
                path: PathBuf::new(),
                source: crate::FragmentError::UnclosedFrontmatter,
            });
        }
        return Ok(FragmentFormatSource {
            frontmatter: Vec::new(),
            body: source,
        });
    };

    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let without_newline = line.strip_suffix('\n').unwrap_or(line);
        let delimiter = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        if delimiter == "---" {
            let yaml = &rest[..offset];
            let frontmatter = if yaml.trim().is_empty() {
                Vec::new()
            } else {
                let mapping: Mapping =
                    serde_yaml_ng::from_str(yaml).map_err(|source| Error::Fragment {
                        path: PathBuf::new(),
                        source: crate::FragmentError::Frontmatter { source },
                    })?;
                mapping
                    .into_iter()
                    .map(|(key, value)| FrontmatterEntry { key, value })
                    .collect()
            };
            let body_start = rest[offset..]
                .strip_prefix(line)
                .expect("line came from rest at offset");
            return Ok(FragmentFormatSource {
                frontmatter,
                body: body_start,
            });
        }
        offset += line.len();
    }

    Err(Error::Fragment {
        path: PathBuf::new(),
        source: crate::FragmentError::UnclosedFrontmatter,
    })
}

fn canonical_frontmatter(mut frontmatter: Vec<FrontmatterEntry>) -> Result<String> {
    let priority = frontmatter
        .iter()
        .position(|entry| entry.key == Value::String(String::from("priority")))
        .map(|index| frontmatter.remove(index).value)
        .unwrap_or(Value::Number(0.into()));
    let priority = match priority {
        Value::Null => 0,
        Value::Number(number) => number.as_i64().ok_or_else(frontmatter_priority_error)?,
        _ => {
            return Err(frontmatter_priority_error());
        }
    };
    if priority == 0 && frontmatter.is_empty() {
        return Ok(String::new());
    }

    let mut output = String::from("---\n");
    if priority != 0 {
        output.push_str("priority: ");
        output.push_str(&priority.to_string());
        output.push('\n');
    }
    frontmatter.sort_by_key(|entry| yaml_key_sort_text(&entry.key));
    for entry in frontmatter {
        append_yaml_entry(&mut output, &entry.key, &entry.value)?;
    }
    output.push_str("---\n");
    Ok(output)
}

fn with_fragment_path(error: Error, path: PathBuf) -> Error {
    match error {
        Error::Fragment { source, .. } => Error::Fragment { path, source },
        error => error,
    }
}

fn frontmatter_priority_error() -> Error {
    Error::Fragment {
        path: PathBuf::new(),
        source: crate::FragmentError::Frontmatter {
            source: serde_yaml_ng::from_str::<i32>("high").expect_err("invalid integer"),
        },
    }
}

fn append_yaml_entry(output: &mut String, key: &Value, value: &Value) -> Result<()> {
    let mut mapping = Mapping::new();
    mapping.insert(key.clone(), value.clone());
    let serialized = serde_yaml_ng::to_string(&mapping).map_err(|source| Error::Fragment {
        path: PathBuf::new(),
        source: crate::FragmentError::Frontmatter { source },
    })?;
    output.push_str(serialized.trim_start_matches("---\n").trim_end());
    output.push('\n');
    Ok(())
}

fn yaml_key_sort_text(key: &Value) -> String {
    match key {
        Value::String(key) => key.clone(),
        key => serde_yaml_ng::to_string(key)
            .unwrap_or_default()
            .trim_start_matches("---\n")
            .trim()
            .to_owned(),
    }
}

fn hongdown_options() -> HongdownOptions {
    HongdownOptions {
        line_width: Some(LineWidth::new(80).expect("80 is a valid line width")),
        unordered_marker: UnorderedMarker::Hyphen,
        leading_spaces: LeadingSpaces::new(1).expect("1 is valid leading spaces"),
        trailing_spaces: TrailingSpaces::new(2).expect("2 is valid trailing spaces"),
        indent_width: IndentWidth::new(4).expect("4 is a valid indent width"),
        curly_double_quotes: false,
        curly_single_quotes: false,
        ellipsis: false,
        em_dash: DashSetting::Disabled,
        ..HongdownOptions::default()
    }
}

fn discovery_warning(warning: &DiscoveryWarning) -> CheckWarning {
    match warning {
        DiscoveryWarning::UnknownSectionFragment { path, section } => CheckWarning {
            message: format!(
                "{}: fragment is under unconfigured section directory {:?}",
                path.display(),
                section
            ),
        },
    }
}

fn fragment_warning(path: &Path, warning: &FragmentWarning) -> CheckWarning {
    match warning {
        FragmentWarning::UnknownFrontmatterKey { key } => CheckWarning {
            message: format!("{}: unknown frontmatter key {:?}", path.display(), key),
        },
    }
}

fn next_file_whitespace_warning(repo: &Repository) -> Result<Option<CheckWarning>> {
    let path = next_version_path(repo);
    let absolute = repo.resolve(&path);
    let contents = match fs::read_to_string(&absolute) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::ReadFile {
                path: absolute,
                source,
            });
        }
    };
    let normalized = contents.trim();
    if normalized.is_empty() || contents == format!("{normalized}\n") {
        Ok(None)
    } else {
        Ok(Some(CheckWarning {
            message: format!(
                "{}: next-version file should be normalized to one trimmed line",
                path.display()
            ),
        }))
    }
}

fn changelog_error(path: PathBuf, source: ChangelogError) -> Error {
    match source {
        ChangelogError::RegionNotFound => Error::RegionNotFound { path },
        source => Error::Changelog { path, source },
    }
}

fn unified_diff(old_contents: &str, new_contents: &str) -> String {
    TextDiff::from_lines(old_contents, new_contents)
        .unified_diff()
        .header("current", "compiled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;
    use crate::FragmentError;
    use crate::vcs::ChangeKind;

    #[derive(Debug, Clone)]
    struct FakeCommit {
        id: CommitId,
        paths: Vec<ChangedPath>,
        message: String,
    }

    #[derive(Debug, Clone)]
    struct FakeVcs {
        commits: Vec<FakeCommit>,
    }

    struct PanicVcs;

    impl Vcs for FakeVcs {
        fn commits(&self, _base: &str) -> Result<Vec<CommitId>> {
            Ok(self
                .commits
                .iter()
                .map(|commit| commit.id.clone())
                .collect())
        }

        fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
            Ok(self
                .commits
                .iter()
                .find(|candidate| candidate.id == *commit)
                .expect("known commit")
                .paths
                .clone())
        }

        fn message(&self, commit: &CommitId) -> Result<String> {
            Ok(self
                .commits
                .iter()
                .find(|candidate| candidate.id == *commit)
                .expect("known commit")
                .message
                .clone())
        }
    }

    impl Vcs for PanicVcs {
        fn commits(&self, _base: &str) -> Result<Vec<CommitId>> {
            panic!("empty check.paths should skip commit lookup");
        }

        fn changed_paths(&self, _commit: &CommitId) -> Result<Vec<ChangedPath>> {
            panic!("empty check.paths should skip changed path lookup");
        }

        fn message(&self, _commit: &CommitId) -> Result<String> {
            panic!("empty check.paths should skip message lookup");
        }
    }

    fn fake_commit(id: &str, paths: &[&str], message: &str) -> FakeCommit {
        FakeCommit {
            id: CommitId::new(id),
            paths: paths
                .iter()
                .map(|path| ChangedPath::new(*path, ChangeKind::Modified))
                .collect(),
            message: message.to_owned(),
        }
    }

    fn fake_commit_with_paths(id: &str, paths: Vec<ChangedPath>, message: &str) -> FakeCommit {
        FakeCommit {
            id: CommitId::new(id),
            paths,
            message: message.to_owned(),
        }
    }

    fn repo_with_config(config: &str) -> (TempDir, Repository) {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), config).expect("config");
        let repo = Repository::from_root(temp.path()).expect("repo");
        (temp, repo)
    }

    fn normalize_separators(value: impl AsRef<str>) -> String {
        value.as_ref().replace('\\', "/")
    }

    #[test]
    fn check_reports_invalid_fragment_shape() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/bad.md"), "not a list\n").expect("fragment");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(!report.is_clean());
        assert_eq!(report.violations.len(), 1);
        assert!(normalize_separators(&report.violations[0].message).contains("changes.d/bad.md"));
        assert!(
            report.violations[0]
                .message
                .contains("exactly one top-level unordered list")
        );
    }

    #[test]
    fn check_report_is_clean_only_without_violations() {
        assert!(CheckReport::default().is_clean());
        assert!(
            !CheckReport {
                violations: vec![CheckViolation {
                    message: String::from("bad fragment")
                }],
                warnings: Vec::new(),
                skipped: Vec::new(),
            }
            .is_clean()
        );
    }

    #[test]
    fn carry_reports_missing_version() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.0.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let carry_error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("missing version");

        assert!(matches!(carry_error, Error::ReleasedVersionNotFound { .. }));
    }

    #[test]
    fn carry_writes_root_fragment_and_overwrites_existing_file() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/carried-from-1.1.5.md"),
            " -  Old.\n",
        )
        .expect("old fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.  [[#8]]\n\n[#8]: https://example.com/issues/8\n",
        )
        .expect("changelog");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry");

        assert_eq!(
            result.written_fragments,
            vec![PathBuf::from("changes.d/carried-from-1.1.5.md")]
        );
        assert_eq!(result.sync, None);
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/carried-from-1.1.5.md"))
                .expect("fragment"),
            " -  Fixed carry.  [[#8]]\n"
        );
    }

    #[test]
    fn carry_preserves_historical_item_formatting() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n*   Fixed historical wrapping.\n    This continuation keeps old spacing.\n",
        )
        .expect("changelog");

        carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry");

        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/carried-from-1.1.5.md"))
                .expect("fragment"),
            "*   Fixed historical wrapping.\n    This continuation keeps old spacing.\n"
        );
    }

    #[test]
    fn carry_validates_every_fragment_before_writing_any_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        );
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n### core\n\n -  Fixed core.\n\n### cli\n\n -  Fixed CLI.  [[#8]]\n",
        )
        .expect("changelog");

        let error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("unknown reference");

        assert!(matches!(
            error,
            Error::Fragment {
                source: FragmentError::UnknownReference { .. },
                ..
            }
        ));
        assert!(
            !temp
                .path()
                .join("changes.d/core/carried-from-1.1.5.md")
                .exists()
        );
        assert!(
            !temp
                .path()
                .join("changes.d/cli/carried-from-1.1.5.md")
                .exists()
        );
    }

    #[test]
    fn carry_syncs_after_writing_when_materialized() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
        )
        .expect("changelog");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains(" -  Fixed carry.\n"));
        assert!(temp.path().join("changes.d/carried-from-1.1.5.md").exists());
    }

    #[test]
    fn release_uses_next_file_and_inserts_section_without_materialization() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            title = "Project changes"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/fix.md"), " -  Fixed release.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: Some(String::from("2026-07-08")),
                next: Some(String::from("1.3.0")),
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            changelog,
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed release.\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
        assert!(!temp.path().join("changes.d/fix.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
    }

    #[test]
    fn release_leaves_empty_materialized_region() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Added release.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            changelog,
            "Project changes\n===============\n\nUnreleased\n----------\n\nTo be released.\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n"
        );
        assert!(!temp.path().join("changes.d/add.md").exists());
        assert!(!temp.path().join("changes.d/next").exists());
    }

    #[test]
    fn release_with_blank_next_removes_next_file_and_uses_unreleased_heading() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.2.0\n-------------\n\nTo be released.\n\n -  Added release.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: Some(String::from("2026-07-08")),
                next: Some(String::from("  ")),
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Unreleased\n----------\n\nTo be released.\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n"
        );
        assert!(!temp.path().join("changes.d/next").exists());
    }

    #[test]
    fn release_with_marker_detection_inserts_released_section_after_markers() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            region-detection = "marker"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(
            temp.path().join("changes.d/fix.md"),
            " -  Fixed marker release.\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Fixed marker release.\n<!-- sacho:unreleased:end -->\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            changelog,
            "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\nUnreleased\n----------\n\nTo be released.\n<!-- sacho:unreleased:end -->\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed marker release.\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
    }

    #[test]
    fn release_failure_before_changelog_write_keeps_fragments_and_next_file() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Added release.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect("plan");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("stale changelog");

        let error = apply_release(&repo, plan).expect_err("missing unreleased region");

        assert!(matches!(error, Error::RegionNotFound { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
    }

    #[test]
    fn release_ignores_consumed_fragment_that_is_already_absent() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        apply_release(
            &repo,
            ReleasePlan {
                version: String::from("1.2.0"),
                date: ReleaseDate::parse("2026-07-08").expect("date"),
                next: None,
                released_markdown: String::from(
                    "Version 1.2.0\n-------------\n\nReleased on July 8, 2026.\n",
                ),
                consumed_fragments: vec![PathBuf::from("changes.d/missing.md")],
            },
        )
        .expect("release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n"
        );
    }

    #[test]
    fn release_rejects_missing_materialized_changelog_without_deleting_fragments() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");

        let error = apply_release(
            &repo,
            ReleasePlan {
                version: String::from("1.2.0"),
                date: ReleaseDate::parse("2026-07-08").expect("date"),
                next: None,
                released_markdown: String::from(
                    "Version 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n",
                ),
                consumed_fragments: vec![PathBuf::from("changes.d/add.md")],
            },
        )
        .expect_err("missing changelog");

        assert!(matches!(error, Error::ReadFile { .. }));
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_without_materialization_creates_missing_changelog() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            title = "Project changes"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n\n"
        );
    }

    #[test]
    fn release_rejects_version_that_disagrees_with_next_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.1")),
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect_err("mismatch");

        assert!(matches!(error, Error::ReleaseVersionMismatch { .. }));
    }

    #[test]
    fn release_requires_version_when_next_file_is_empty() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: Some(String::from("2026-07-08")),
                next: None,
            },
        )
        .expect_err("missing version");

        assert!(matches!(error, Error::MissingReleaseVersion));
    }

    #[test]
    fn release_rejects_invalid_date_forms_and_calendar_dates() {
        for date in [
            "2026-07",
            "2026-7-08",
            "2026-07-8",
            "2026/07/08",
            "2026-02-29",
            "2026-00-08",
            "2026-07-00",
        ] {
            assert!(
                matches!(
                    ReleaseDate::parse(date),
                    Err(Error::InvalidReleaseDate { .. })
                ),
                "{date:?} should be invalid"
            );
        }

        assert_eq!(
            ReleaseDate::parse("2024-02-29").expect("leap day"),
            ReleaseDate {
                year: 2024,
                month: 2,
                day: 29,
            }
        );
    }

    #[test]
    fn release_date_long_form_uses_month_names() {
        let names = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        for (index, name) in names.into_iter().enumerate() {
            let date = ReleaseDate {
                year: 2026,
                month: (index + 1) as u8,
                day: 8,
            };

            assert_eq!(date.long_form(), format!("{name} 8, 2026"));
        }
    }

    #[test]
    fn release_today_utc_matches_system_day() {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_secs();
        let expected = civil_from_days((seconds / 86_400) as i64);

        assert_eq!(ReleaseDate::today_utc(), expected);
    }

    #[test]
    fn release_insertion_helpers_handle_boundary_spacing() {
        assert_eq!(
            insert_released_section("# Changelog", "Version 1.2.0\n-------------\n"),
            "# Changelog\n\nVersion 1.2.0\n-------------\n\n"
        );
        assert_eq!(
            insert_released_section(
                "Changelog\n=========\n\nVersion 1.1.0\n-------------\n",
                "Version 1.2.0\n-------------\n",
            ),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nVersion 1.1.0\n-------------\n"
        );
        assert_eq!(
            insert_released_after_marker_region(
                "Header\n<!-- sacho:unreleased:end --> trailer\nVersion 1.1.0\n",
                "Version 1.2.0\n-------------\n",
            ),
            "Header\n<!-- sacho:unreleased:end --> trailer\n\nVersion 1.2.0\n-------------\n\nVersion 1.1.0\n"
        );
    }

    #[test]
    fn release_line_scanning_helpers_classify_titles_and_blanks() {
        let lines = source_lines_with_offsets("Title\n=====\n\nBody\n");

        assert_eq!(skip_blank_lines(&lines, 2), Some(3));
        assert!(is_setext_title_underline("====="));
        assert!(!is_setext_title_underline("=====x"));
        assert_eq!(insertion_index_after_title("Plain one-line file"), 0);
    }

    #[test]
    fn release_date_helpers_handle_leap_years_and_month_lengths() {
        assert!(is_leap_year(2000));
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
        assert!(!is_leap_year(2026));

        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
    }

    #[test]
    fn civil_from_days_matches_known_utc_dates() {
        let cases = [
            (
                -1,
                ReleaseDate {
                    year: 1969,
                    month: 12,
                    day: 31,
                },
            ),
            (
                0,
                ReleaseDate {
                    year: 1970,
                    month: 1,
                    day: 1,
                },
            ),
            (
                31,
                ReleaseDate {
                    year: 1970,
                    month: 2,
                    day: 1,
                },
            ),
            (
                365,
                ReleaseDate {
                    year: 1971,
                    month: 1,
                    day: 1,
                },
            ),
            (
                789,
                ReleaseDate {
                    year: 1972,
                    month: 2,
                    day: 29,
                },
            ),
            (
                10_957,
                ReleaseDate {
                    year: 2000,
                    month: 1,
                    day: 1,
                },
            ),
            (
                11_016,
                ReleaseDate {
                    year: 2000,
                    month: 2,
                    day: 29,
                },
            ),
            (
                -719_162,
                ReleaseDate {
                    year: 1,
                    month: 1,
                    day: 1,
                },
            ),
            (
                -719_469,
                ReleaseDate {
                    year: 0,
                    month: 2,
                    day: 29,
                },
            ),
            (
                -800_000,
                ReleaseDate {
                    year: -221,
                    month: 9,
                    day: 4,
                },
            ),
            (
                -135_080,
                ReleaseDate {
                    year: 1600,
                    month: 3,
                    day: 1,
                },
            ),
            (
                -25_508,
                ReleaseDate {
                    year: 1900,
                    month: 3,
                    day: 1,
                },
            ),
            (
                20_272,
                ReleaseDate {
                    year: 2025,
                    month: 7,
                    day: 3,
                },
            ),
            (
                157_113,
                ReleaseDate {
                    year: 2400,
                    month: 2,
                    day: 29,
                },
            ),
        ];

        for (days, date) in cases {
            assert_eq!(civil_from_days(days), date, "{days}");
        }
    }

    #[test]
    fn sync_skips_when_materialization_is_disabled() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        let plan = plan_sync(&repo, SyncOptions::default()).expect("plan");

        assert!(matches!(
            plan,
            SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled)
        ));
    }

    #[test]
    fn sync_skips_when_changelog_is_current() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let plan = plan_sync(&repo, SyncOptions::default()).expect("plan");

        assert!(matches!(
            plan,
            SyncPlan::Skipped(SyncSkipReason::AlreadyCurrent)
        ));
    }

    #[test]
    fn sync_force_applies_pending_write() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let plan = plan_sync(&repo, SyncOptions { force: true }).expect("plan");
        let result = apply_sync(&repo, plan).expect("apply");

        assert!(result.changed);
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("read")
                .contains(" -  Fixed sync.\n")
        );
    }

    #[test]
    fn add_creates_fragment_in_configured_section() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "core"
            directory = "core"
            "#,
        );

        let result = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("core")),
                name: String::from("clear-function"),
            },
        )
        .expect("add");

        assert_eq!(
            result.path,
            PathBuf::from("changes.d/core/clear-function.md")
        );
        assert_eq!(
            fs::read_to_string(temp.path().join(result.path)).expect("fragment"),
            " -\n"
        );
    }

    #[test]
    fn add_requires_section_when_repository_has_sections() {
        let (_temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "core"
            directory = "core"
            "#,
        );

        let error = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function"),
            },
        )
        .expect_err("section required");

        assert!(matches!(error, Error::MissingSection));
    }

    #[test]
    fn add_creates_fragment_without_sections() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        let result = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function.md"),
            },
        )
        .expect("add");

        assert_eq!(result.path, PathBuf::from("changes.d/clear-function.md"));
        assert_eq!(
            fs::read_to_string(temp.path().join(result.path)).expect("fragment"),
            " -\n"
        );
    }

    #[test]
    fn add_rejects_section_without_section_config() {
        let (_temp, repo) = repo_with_config("");

        let error = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("core")),
                name: String::from("clear-function"),
            },
        )
        .expect_err("section is unexpected");

        assert!(matches!(error, Error::UnexpectedSection));
    }

    #[test]
    fn add_does_not_overwrite_existing_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/existing.md"), " -  Existing.\n").expect("existing");

        let error = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("existing"),
            },
        )
        .expect_err("existing file");

        assert!(matches!(error, Error::FragmentAlreadyExists { .. }));
    }

    #[test]
    fn next_rejects_empty_version_after_trimming() {
        let (_temp, repo) = repo_with_config("");

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("  "),
            },
        )
        .expect_err("empty version");

        assert!(matches!(error, Error::EmptyNextVersion));
    }

    #[test]
    fn next_updates_heading_when_materialized() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        set_next_version(
            &repo,
            NextOptions {
                version: String::from(" 1.2.0 "),
            },
        )
        .expect("next");

        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("changelog")
                .contains("Version 1.2.0\n-------------")
        );
    }

    #[test]
    fn next_refuses_to_discard_materialized_hand_edits() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\n -  Hand edited entry.\n",
        )
        .expect("changelog");

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect_err("needs explicit sync");

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert!(!temp.path().join("changes.d/next").exists());
    }

    #[test]
    fn unknown_frontmatter_key_is_warning_only() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/unknown.md"),
            "---\nowner: core\n---\n -  Added thing.\n",
        )
        .expect("fragment");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(report.is_clean());
        assert_eq!(report.status(), CheckStatus::HasWarnings);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.message.contains("unknown frontmatter key"))
        );
    }

    #[test]
    fn check_reports_formatting_mismatch() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/bad.md"), "- Added thing.\n").expect("fragment");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert_eq!(report.status(), CheckStatus::HasViolations);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.message.contains("formatting differs"))
        );
    }

    #[test]
    fn fmt_refuses_to_discard_materialized_hand_edits() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\n -  Hand edited entry.\n",
        )
        .expect("changelog");

        let error = format_fragments(&repo, FormatOptions).expect_err("needs explicit sync");

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
    }

    #[test]
    fn format_fragment_source_writes_canonical_frontmatter() {
        let formatted = format_fragment_source(
            "---\nowner: core\npriority: -2\n---\n- Added \"quotes...\" and 'apostrophes'.\n",
        )
        .expect("format");

        assert_eq!(
            formatted,
            "---\npriority: -2\nowner: core\n---\n -  Added \"quotes...\" and 'apostrophes'.\n"
        );
    }

    #[test]
    fn format_fragment_source_canonicalizes_null_priority_away() {
        let formatted =
            format_fragment_source("---\npriority: null\n---\n- Added thing.\n").expect("format");

        assert_eq!(formatted, " -  Added thing.\n");
    }

    #[test]
    fn format_fragment_source_rejects_fractional_priority() {
        let error = format_fragment_source("---\npriority: 1.5\n---\n -  Added thing.\n")
            .expect_err("fractional priority");

        assert!(matches!(
            error,
            Error::Fragment {
                source: crate::FragmentError::Frontmatter { .. },
                ..
            }
        ));
    }

    #[test]
    fn format_fragments_preserves_path_on_formatter_fragment_error() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/bad-priority.md"),
            "---\npriority: 1.5\n---\n -  Added thing.\n",
        )
        .expect("fragment");

        let error = format_fragments(&repo, FormatOptions).expect_err("format error");

        assert!(matches!(
            error,
            Error::Fragment {
                path,
                source: crate::FragmentError::Frontmatter { .. },
            } if path == PathBuf::from("changes.d/bad-priority.md")
        ));
    }

    #[test]
    fn format_fragment_source_preserves_structured_unknown_frontmatter_values() {
        let formatted = format_fragment_source(
            r#"---
tags:
- api
metadata:
  owner: docs
---
- Added thing.
"#,
        )
        .expect("format");

        assert_eq!(
            formatted,
            r#"---
metadata:
  owner: docs
tags:
- api
---
 -  Added thing.
"#
        );
        parse_fragment(
            PathBuf::from("change.md"),
            &formatted,
            None,
            &repo_with_config("").1.config().links,
        )
        .expect("formatted fragment remains parseable");
    }

    #[test]
    fn format_fragment_source_preserves_non_string_unknown_frontmatter_keys() {
        let formatted = format_fragment_source(
            r#"---
123: owner
priority: 0
---
- Added thing.
"#,
        )
        .expect("format");

        assert_eq!(
            formatted,
            r#"---
123: owner
---
 -  Added thing.
"#
        );
        parse_fragment(
            PathBuf::from("change.md"),
            &formatted,
            None,
            &repo_with_config("").1.config().links,
        )
        .expect("formatted fragment remains parseable");
    }

    #[test]
    fn format_fragment_source_rejects_unclosed_frontmatter_delimiter() {
        let error = format_fragment_source("---").expect_err("unclosed frontmatter");

        assert!(matches!(
            error,
            Error::Fragment {
                source: crate::FragmentError::UnclosedFrontmatter,
                ..
            }
        ));
    }

    #[test]
    fn check_warns_when_next_file_needs_whitespace_normalization() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "  1.2.0  \n\n").expect("next");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(report.is_clean());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.message.contains("next-version file"))
        );
    }

    #[test]
    fn check_accepts_empty_next_file_without_whitespace_warning() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "\n\n").expect("next");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(report.warnings.is_empty());
    }

    #[test]
    fn check_fix_formats_fragments_and_syncs_materialized_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\n -  Fixed issue.\n",
        )
        .expect("changelog");

        let report = check(
            &repo,
            CheckOptions {
                fix: true,
                base: None,
            },
        )
        .expect("check --fix");

        assert_eq!(report.status(), CheckStatus::Clean);
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
            " -  Fixed issue.\n"
        );
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("changelog")
                .contains(" -  Fixed issue.\n")
        );
    }

    #[test]
    fn check_reports_materialized_changelog_mismatch() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(!report.is_clean());
        assert!(report.violations.iter().any(|violation| {
            violation
                .message
                .contains("materialized changelog is out of sync")
        }));
    }

    #[test]
    fn missing_base_skips_layer_three() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(report.is_clean());
        assert!(report.skipped.iter().any(|skipped| {
            skipped
                .message
                .contains("missing-fragment check skipped because --base was not supplied")
        }));
    }

    #[test]
    fn empty_check_paths_skip_layer_three_without_vcs_lookup() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        let report =
            missing_fragment_violations(&repo, &PanicVcs, "not-a-real-base").expect("layer three");

        assert!(report.violations.is_empty());
        assert_eq!(
            report.skipped,
            vec![SkippedCheck {
                message: String::from(
                    "missing-fragment check skipped because check.paths is empty"
                )
            }]
        );
    }

    #[test]
    fn check_base_with_empty_check_paths_is_no_op_outside_git() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        let report = check(
            &repo,
            CheckOptions {
                base: Some(String::from("not-a-real-base")),
                ..CheckOptions::default()
            },
        )
        .expect("check");

        assert!(report.violations.is_empty());
        assert!(report.skipped.iter().any(|skipped| {
            skipped
                .message
                .contains("missing-fragment check skipped because check.paths is empty")
        }));
    }

    #[test]
    fn layer_three_accepts_core_change_with_core_fragment() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/*/src/**"]

            [[sections]]
            id = "@optique/core"
            directory = "core"
            paths = ["packages/core/**"]

            [[sections]]
            id = "@optique/logtape"
            directory = "logtape"
            paths = ["packages/logtape/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/core/clear-function.md"),
            " -  Added clear function.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &[
                    "packages/core/src/lib.ts",
                    "changes.d/core/clear-function.md",
                ],
                "Add clear function",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn layer_three_rejects_core_change_with_logtape_fragment() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/*/src/**"]

            [[sections]]
            id = "@optique/core"
            directory = "core"
            paths = ["packages/core/**"]

            [[sections]]
            id = "@optique/logtape"
            directory = "logtape"
            paths = ["packages/logtape/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/core/src/lib.ts", "changes.d/logtape/formatter.md"],
                "Add clear function",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .message
                .contains("section \"@optique/core\"")
        );
        assert!(
            report.violations[0]
                .message
                .contains("sacho add --section @optique/core")
        );
    }

    #[test]
    fn layer_three_ignores_docs_outside_check_paths() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit("a1", &["docs/guide.md"], "Update docs")],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn layer_three_accepts_unsectioned_source_change_with_any_fragment() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/clear-function.md"),
            " -  Added clear function.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["src/lib.rs", "changes.d/clear-function.md"],
                "Add clear function",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn layer_three_unsectioned_repository_suggestion_uses_root_add() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit("a1", &["src/lib.rs"], "Change API")],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .message
                .contains("sacho add <topic-name>")
        );
        assert!(
            !report.violations[0]
                .message
                .contains("sacho add --section <section-id> <topic-name>")
        );
    }

    #[test]
    fn layer_three_rejects_fragment_missing_from_final_tree() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![
                fake_commit(
                    "a1",
                    &["src/lib.rs", "changes.d/clear-function.md"],
                    "Add clear function",
                ),
                fake_commit_with_paths(
                    "b2",
                    vec![ChangedPath::new(
                        "changes.d/clear-function.md",
                        ChangeKind::Deleted,
                    )],
                    "Remove obsolete fragment",
                ),
            ],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.starts_with("a1:"));
        assert!(
            report.violations[0]
                .message
                .contains("missing changelog fragment")
        );
    }

    #[test]
    fn layer_three_matches_surviving_fragment_by_file_not_target() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/core/**"]

            [[sections]]
            id = "core"
            directory = "core"
            paths = ["packages/core/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/core/unrelated.md"),
            " -  Added unrelated change.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![
                fake_commit(
                    "a1",
                    &[
                        "packages/core/src/lib.ts",
                        "changes.d/core/clear-function.md",
                    ],
                    "Add clear function",
                ),
                fake_commit_with_paths(
                    "b2",
                    vec![ChangedPath::new(
                        "changes.d/core/clear-function.md",
                        ChangeKind::Deleted,
                    )],
                    "Remove obsolete fragment",
                ),
            ],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.starts_with("a1:"));
    }

    #[test]
    fn layer_three_treats_rename_out_of_checked_paths_as_relevant() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit_with_paths(
                "a1",
                vec![ChangedPath::with_old_path(
                    "internal/api.rs",
                    ChangeKind::Renamed,
                    "src/api.rs",
                )],
                "Move API internals",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.contains("src/api.rs"));
    }

    #[test]
    fn changelog_none_trailer_exempts_one_commit_only() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![
                fake_commit(
                    "a1",
                    &["src/lib.rs"],
                    "Refactor internals\n\nChangelog: none\n",
                ),
                fake_commit("b2", &["src/main.rs"], "Change CLI"),
            ],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.starts_with("b2:"));
    }

    #[test]
    fn layer_three_does_not_accept_deleted_fragment_as_coverage() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit_with_paths(
                "a1",
                vec![
                    ChangedPath::new("src/lib.rs", ChangeKind::Modified),
                    ChangedPath::new("changes.d/old-entry.md", ChangeKind::Deleted),
                ],
                "Change API and remove stale fragment",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .message
                .contains("missing changelog fragment")
        );
    }

    #[test]
    fn layer_three_does_not_accept_renamed_fragment_as_coverage() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/new-entry.md"),
            " -  Added previous behavior.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit_with_paths(
                "a1",
                vec![
                    ChangedPath::new("src/lib.rs", ChangeKind::Modified),
                    ChangedPath::with_old_path(
                        "changes.d/new-entry.md",
                        ChangeKind::Renamed,
                        "changes.d/old-entry.md",
                    ),
                ],
                "Change API and rename stale fragment",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .message
                .contains("missing changelog fragment")
        );
    }

    #[test]
    fn layer_three_accepts_edited_copied_fragment_as_coverage() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/new-entry.md"),
            " -  Added new behavior.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit_with_paths(
                "a1",
                vec![
                    ChangedPath::new("src/lib.rs", ChangeKind::Modified),
                    ChangedPath::with_old_path_and_similarity(
                        "changes.d/new-entry.md",
                        ChangeKind::Copied,
                        "changes.d/template.md",
                        85,
                    ),
                ],
                "Change API with copied fragment",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn fragment_coverage_requires_content_changes() {
        assert!(fragment_content_changed(&ChangedPath::new(
            "changes.d/added.md",
            ChangeKind::Added
        )));
        assert!(fragment_content_changed(&ChangedPath::new(
            "changes.d/modified.md",
            ChangeKind::Modified
        )));
        assert!(fragment_content_changed(
            &ChangedPath::with_old_path_and_similarity(
                "changes.d/copied.md",
                ChangeKind::Copied,
                "changes.d/template.md",
                85,
            )
        ));
        assert!(fragment_content_changed(
            &ChangedPath::with_old_path_and_similarity(
                "changes.d/renamed.md",
                ChangeKind::Renamed,
                "changes.d/old.md",
                85,
            )
        ));
        assert!(!fragment_content_changed(
            &ChangedPath::with_old_path_and_similarity(
                "changes.d/copied.md",
                ChangeKind::Copied,
                "changes.d/template.md",
                100,
            )
        ));
        assert!(!fragment_content_changed(&ChangedPath::with_old_path(
            "changes.d/copied.md",
            ChangeKind::Copied,
            "changes.d/template.md",
        )));
        assert!(!fragment_content_changed(&ChangedPath::new(
            "changes.d/deleted.md",
            ChangeKind::Deleted
        )));
        assert!(!fragment_content_changed(&ChangedPath::new(
            "changes.d/other.md",
            ChangeKind::Other
        )));
    }

    #[test]
    fn layer_three_does_not_accept_nested_unsectioned_markdown_as_fragment() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["src/lib.rs", "changes.d/nested/not-a-fragment.md"],
                "Change API",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0]
                .message
                .contains("missing changelog fragment")
        );
    }

    #[test]
    fn layer_three_does_not_accept_nested_section_markdown_as_fragment() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/core/**"]

            [[sections]]
            id = "core"
            directory = "core"
            paths = ["packages/core/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &[
                    "packages/core/src/lib.ts",
                    "changes.d/core/nested/not-a-fragment.md",
                ],
                "Change core API",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.contains("section \"core\""));
    }

    #[test]
    fn layer_three_requires_any_fragment_for_unattributed_paths_in_sectioned_repo() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/**"]

            [[sections]]
            id = "core"
            directory = "core"
            paths = ["packages/core/**"]
            "#,
        );
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/unknown/src/lib.ts"],
                "Change unknown package",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.skipped.is_empty());
        assert!(
            report.violations[0]
                .message
                .contains("sacho add --section <section-id> <topic-name>")
        );
    }

    #[test]
    fn layer_three_accepts_unknown_section_fragment_for_unattributed_paths() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/**"]

            [[sections]]
            id = "core"
            directory = "core"
            paths = ["packages/core/**"]
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/unknown")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/unknown/change.md"),
            " -  Added unknown package behavior.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/unknown/src/lib.ts", "changes.d/unknown/change.md"],
                "Change unknown package",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
    }

    #[test]
    fn path_sample_reports_only_overflow_after_three_paths() {
        let three = path_sample(&[
            PathBuf::from("src/a.rs"),
            PathBuf::from("src/b.rs"),
            PathBuf::from("src/c.rs"),
        ]);
        let four = path_sample(&[
            PathBuf::from("src/a.rs"),
            PathBuf::from("src/b.rs"),
            PathBuf::from("src/c.rs"),
            PathBuf::from("src/d.rs"),
        ]);

        assert_eq!(three, "src/a.rs, src/b.rs, src/c.rs");
        assert_eq!(four, "src/a.rs, src/b.rs, src/c.rs, and 1 more");
    }

    #[test]
    fn changelog_trailer_requires_changelog_key_and_none_value() {
        assert!(!message_exempts_changelog(
            "Change behavior\n\nChangelog: later\n"
        ));
        assert!(!message_exempts_changelog(
            "Change behavior\n\nNote: none\n"
        ));
    }

    proptest! {
        #[test]
        fn skip_markers_are_case_insensitive(marker in prop::sample::select(vec![
            "[changelog skip]",
            "[changes skip]",
            "[skip changelog]",
            "[skip changes]",
        ])) {
            let message = format!("Update docs\n\n{}", marker.to_uppercase());

            prop_assert!(message_exempts_changelog(&message));
        }

        #[test]
        fn path_attribution_is_deterministic_under_shuffled_input(shuffle in any::<bool>()) {
            let (_temp, repo) = repo_with_config(
                r#"
                [changelog]
                materialize = false

                [check]
                paths = ["packages/**"]

                [[sections]]
                id = "core"
                directory = "core"
                paths = ["packages/core/**"]

                [[sections]]
                id = "logtape"
                directory = "logtape"
                paths = ["packages/logtape/**"]
                "#,
            );
            let mut paths = vec![
                ChangedPath::new("packages/logtape/src/lib.ts", ChangeKind::Modified),
                ChangedPath::new("packages/core/src/lib.ts", ChangeKind::Modified),
            ];
            if shuffle {
                paths.reverse();
            }
            let vcs = FakeVcs {
                commits: vec![FakeCommit {
                    id: CommitId::new("a1"),
                    paths,
                    message: String::from("Change packages"),
                }],
            };

            let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");
            let messages = report
                .violations
                .iter()
                .map(|violation| violation.message.as_str())
                .collect::<Vec<_>>();

            prop_assert_eq!(messages.len(), 2);
            prop_assert!(messages[0].contains("section \"core\""));
            prop_assert!(messages[1].contains("section \"logtape\""));
        }
    }

    proptest! {
        #[test]
        fn check_collects_every_invalid_fragment(
            invalid_fragments in prop::collection::vec("[a-z]{1,12}", 1..12),
        ) {
            let (temp, repo) = repo_with_config("");
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            fs::write(temp.path().join("changes.d/valid.md"), " -  Valid change.\n")
                .expect("valid fragment");

            let expected_paths = invalid_fragments
                .iter()
                .enumerate()
                .map(|(index, word)| {
                    let relative =
                        PathBuf::from("changes.d").join(format!("{index:02}-{word}.md"));
                    fs::write(
                        temp.path().join(&relative),
                        format!("Invalid top-level paragraph {word}.\n"),
                    )
                    .expect("invalid fragment");
                    relative
                })
                .collect::<Vec<_>>();

            let report = check(&repo, CheckOptions::default()).expect("check");

            prop_assert_eq!(report.violations.len(), expected_paths.len());
            for path in expected_paths {
                let path = normalize_separators(path.display().to_string());
                prop_assert!(
                    report
                        .violations
                        .iter()
                        .any(|violation| normalize_separators(&violation.message).contains(&path)),
                    "missing violation for {path}"
                );
            }
        }
    }

    proptest! {
        #[test]
        fn fragment_filename_validator_rejects_path_traversal(
            prefix in "([A-Za-z0-9_-]{0,8}/)?",
            suffix in "(/[A-Za-z0-9_-]{0,8})?",
        ) {
            let name = format!("{prefix}..{suffix}");

            prop_assert!(validate_fragment_name(&name).is_err());
        }
    }
}
