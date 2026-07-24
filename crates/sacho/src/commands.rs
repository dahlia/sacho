use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use globset::{Glob, GlobSet, GlobSetBuilder};
use indexmap::IndexSet;
use serde_yaml_ng::{Mapping, Value};
use similar::TextDiff;

use crate::changelog::{
    BEGIN_MARKER, ChangelogError, END_MARKER, ReleasedSection, find_unreleased_region,
    marker_region_contents, replace_unreleased_region, set_hongdown_separator_before,
    set_trailing_newline_count,
};
use crate::compile::{
    VersionLabel, compile_parsed_fragments, validate_resolved_link_consistency,
    version_label_from_contents,
};
use crate::config::{
    Config, ReferenceSigil, RegionDetection, SectionConfig, UrlTemplate, VcsPreset,
};
use crate::error::{Error, MutationCommand, Result};
use crate::fragment::{
    DiscoveryWarning, Fragment, FragmentWarning, compare_fragment_paths,
    discover_fragment_candidates, parse_fragment,
};
use crate::link_resolution::{LinkResolutionPolicy, ReferenceUrlResolver, is_http_reference_url};
use crate::markdown::format_markdown;
use crate::merge::{MergeDriverOptions, MergeDriverResult, merge_driver};
use crate::released::{
    carry_release, has_released_sections, import_unreleased_region, insertion_title_span,
    render_released_section,
};
#[cfg(test)]
use crate::repo::MUTATION_LOCK_FILE;
use crate::repo::{
    PathValidationCache, PreparedAtomicWrite, Repository,
    filesystem_path_identity as repo_path_identity, filesystem_paths_overlap, move_path_if_absent,
    mutation_lock_path, normalize_repository_config_paths,
    validate_configured_paths_against_reserved_with_cache, validate_pattern_section_directories,
    validate_repository_config_paths_with_cache,
};
use crate::section::{SectionResolver, has_sections};
use crate::section_pattern::{SectionPattern, SectionPatternConfig};
use crate::vcs::{ChangeKind, ChangedPath, CommitId, GitVcs, HgVcs, JjVcs, Vcs};

pub use crate::compile::{CompileOptions, CompiledRegion};

/// Command execution context shared by command APIs.
#[derive(Debug, Clone)]
pub struct CommandContext {
    /// Repository the command operates on.
    pub repo: Repository,
}

/// Options for bootstrapping Sacho in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitOptions {
    /// Changelog path for newly generated configuration.
    pub changelog_path: Option<PathBuf>,

    /// Fragment directory for newly generated configuration.
    pub fragment_directory: Option<PathBuf>,

    /// Materialization policy for newly generated configuration.
    pub materialize: Option<bool>,

    /// Executable used by installed VCS integrations.
    ///
    /// The current process executable is used when this is absent.
    pub integration_executable: Option<PathBuf>,

    /// Whether to install or update Sacho's Git commit hook blocks.
    pub install_hook: bool,

    /// Whether an existing unmarked hook may receive a marked Sacho block.
    pub append_existing_hook: bool,

    /// Repository URL used for `links."#"` in newly generated configuration.
    pub repository_url: Option<String>,

    /// Sections selected while creating new configuration.
    pub sections: Vec<SectionConfig>,

    /// Section patterns selected while creating new configuration.
    pub section_patterns: Vec<SectionPatternConfig>,
}

/// Result of bootstrapping Sacho in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitResult {
    /// Repository-relative files created by the operation.
    pub created_files: Vec<PathBuf>,

    /// Repository-relative files modified by the operation.
    pub modified_files: Vec<PathBuf>,

    /// Repository-relative files that already existed and were left alone.
    pub skipped_existing_files: Vec<PathBuf>,

    /// Local Git configuration keys written by the operation.
    pub local_git_config_changes: Vec<String>,

    /// Local Mercurial configuration keys written by the operation.
    pub local_hg_config_changes: Vec<String>,

    /// Manual actions the user still needs to perform.
    pub manual_actions_required: Vec<String>,
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

    /// Synchronization result when materialization is enabled.
    pub sync: Option<SyncResult>,
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

    /// Synchronization result when materialization is enabled.
    pub sync: Option<SyncResult>,

    /// Non-fatal failures while removing committed transaction claims.
    pub cleanup_warnings: Vec<MutationCleanupWarning>,
}

/// Options for formatting fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatOptions;

/// Result of formatting fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatResult {
    /// Fragment paths whose contents changed.
    pub changed: Vec<PathBuf>,

    /// Non-fatal failures while removing committed transaction claims.
    pub cleanup_warnings: Vec<MutationCleanupWarning>,
}

/// Warning emitted after a repository mutation committed successfully.
///
/// The repository already contains the complete final state when this warning
/// is returned.  It describes a retained private claim that could not be
/// removed and may require manual cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationCleanupWarning {
    /// Repository-relative participant associated with the retained claim.
    pub path: PathBuf,

    /// Cleanup failure, including the private path retained for recovery.
    pub message: String,
}

/// Planned fragment formatting and materialized changelog synchronization.
///
/// Planning is read-only.  Callers may inspect [`Self::sync`] to obtain any
/// required user confirmation before passing the plan to [`apply_format`].
/// The former `prepare_check_fix` compatibility wrapper is intentionally not
/// available because its old mutation-before-confirmation contract cannot be
/// preserved safely; use [`plan_format`] and [`apply_format`] together:
///
/// ```compile_fail
/// use sacho::commands::prepare_check_fix;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatPlan {
    /// Fragment paths that will be rewritten when the plan is applied.
    pub formatting: FormatResult,

    /// Materialized changelog synchronization required after formatting.
    pub sync: SyncPlan,

    fragments: Vec<FormatFragmentSnapshot>,
    next_file: Option<FormatFileSnapshot>,
    changelog: Option<FormatFileSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatFragmentSnapshot {
    path: PathBuf,
    before: String,
    after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedFragmentSnapshot {
    path: PathBuf,
    before: String,
    after: String,
    parsed_before: Fragment,
    parsed_after: Fragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatFileSnapshot {
    path: PathBuf,
    contents: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryMutationPlan {
    command: MutationCommand,
    participants: Vec<ReleaseFileChange>,
    fragment_paths_before: Vec<PathBuf>,
    fragment_paths_after: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RepositoryMutationOutcome {
    cleanup_warnings: Vec<MutationCleanupWarning>,
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

/// Planned resolution of unpinned reference links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveLinksPlan {
    /// Fragment paths whose frontmatter will change.
    pub changed_fragments: Vec<PathBuf>,

    /// Materialized changelog synchronization produced by the resolved links.
    pub sync: SyncPlan,

    mutation: RepositoryMutationPlan,
}

/// Options for resolving and pinning reference links.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveLinksOptions {
    /// Apply materialized synchronization even when it may discard hand edits.
    pub force: bool,
}

/// Result of resolving and pinning reference links.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolveLinksResult {
    /// Fragment paths whose frontmatter changed.
    pub changed_fragments: Vec<PathBuf>,

    /// Whether the materialized changelog changed.
    pub changelog_changed: bool,

    /// Non-fatal failures while removing committed transaction claims.
    pub cleanup_warnings: Vec<MutationCleanupWarning>,
}

/// Options for planning a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOptions {
    /// Explicit version to release.
    pub version: Option<String>,
    /// Calendar date to record for the release.
    pub date: ReleaseDate,
    /// Next unreleased version to write after releasing.
    pub next: Option<String>,
    /// Allow a release whose compiled changelog has no substantive items.
    pub allow_empty: bool,
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

    /// Planned changelog replacement.
    pub changelog: ReleaseFileChange,

    /// Planned next-version file replacement or removal.
    pub next_file: ReleaseFileChange,

    /// Fragment files and exact contents consumed by the release.
    pub consumed_fragments: Vec<ReleaseFragment>,
}

/// Exact state of a file participating in a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseFileState {
    /// The path does not exist.
    Missing,

    /// The path is a regular UTF-8 file with these exact contents.
    Present(String),

    /// The path is a regular non-UTF-8 file with these exact bytes.
    Raw(Vec<u8>),
}

impl ReleaseFileState {
    fn contents(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present(contents) => Some(contents.as_bytes()),
            Self::Raw(contents) => Some(contents),
        }
    }
}

/// Before and after state of a file changed by a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFileChange {
    /// Repository-relative path of the file.
    pub path: PathBuf,

    /// File state observed while planning.
    pub before: ReleaseFileState,

    /// File state required after a successful release.
    pub after: ReleaseFileState,
}

/// Fragment snapshot consumed by a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFragment {
    /// Repository-relative fragment path.
    pub path: PathBuf,

    /// Exact contents observed while planning.
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseFragmentSource {
    path: PathBuf,
    contents: Vec<u8>,
    section: Option<String>,
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
    /// Parses a calendar date in strict `YYYY-MM-DD` form.
    pub fn parse(source: &str) -> Result<Self> {
        let parts = source.split('-').collect::<Vec<_>>();
        if parts.len() != 3
            || parts[0].len() != 4
            || parts[1].len() != 2
            || parts[2].len() != 2
            || parts
                .iter()
                .any(|part| !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
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

    fn long_form(self) -> String {
        format!("{} {}, {}", month_name(self.month), self.day, self.year)
    }

    fn is_valid(self) -> bool {
        (0..=9_999).contains(&self.year)
            && (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
    }
}

impl FromStr for ReleaseDate {
    type Err = Error;

    fn from_str(source: &str) -> Result<Self> {
        Self::parse(source)
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

/// Options for showing a released changelog section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowOptions {
    /// Released version whose section should be returned.
    pub version: String,

    /// Whether to omit the released version heading from the returned Markdown.
    pub skip_heading: bool,

    /// Whether to wrap returned Markdown at Sacho's canonical line width.
    pub word_wrap: bool,
}

/// Result of carrying released entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CarryResult {
    /// Fragment paths written by the carry operation.
    pub written_fragments: Vec<PathBuf>,

    /// Synchronization result when materialization is enabled.
    pub sync: Option<SyncResult>,

    /// Non-fatal failures while removing committed transaction claims.
    pub cleanup_warnings: Vec<MutationCleanupWarning>,
}

/// Options for importing the materialized unreleased changelog into fragments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportUnreleasedOptions {
    /// Bypass confirmation when normalization changes the materialized region.
    pub force: bool,
}

/// Planned import of the materialized unreleased changelog.
///
/// Planning is read-only. If [`Self::requires_confirmation`] returns true, a
/// caller should show [`Self::diff`] and obtain confirmation before applying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportUnreleasedPlan {
    /// Version inferred from the materialized heading.
    pub inferred_version: Option<String>,

    /// Fragment paths that will be created.
    pub written_fragments: Vec<PathBuf>,

    /// Normalization diff that requires confirmation, unless forced.
    pub diff: Option<String>,

    force: bool,
    mutation: RepositoryMutationPlan,
}

impl ImportUnreleasedPlan {
    /// Reports whether applying the plan needs caller confirmation.
    pub fn requires_confirmation(&self) -> bool {
        self.diff.is_some() && !self.force
    }
}

/// Result of importing a materialized unreleased changelog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportUnreleasedResult {
    /// Fragment paths written by the import.
    pub written_fragments: Vec<PathBuf>,

    /// Version inferred from the materialized heading.
    pub inferred_version: Option<String>,

    /// Non-fatal failures while removing committed transaction claims.
    pub cleanup_warnings: Vec<MutationCleanupWarning>,
}

/// Options for running repository checks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckOptions {
    /// Optional base revision for VCS-backed missing-fragment checks.
    pub base: Option<String>,
    /// Whether Layer 3 should inspect the staged Git index.
    ///
    /// This mode is mutually exclusive with [`Self::base`].
    pub staged: bool,
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

/// Bootstraps Sacho configuration and repository integration.
pub fn init_repository(root: impl AsRef<Path>, options: InitOptions) -> Result<InitResult> {
    let root = init_root(root.as_ref());
    let config_path = root.join(Repository::CONFIG_FILE);
    let lock_path = mutation_lock_path(&root);
    let (_, mut preflight_config) = load_init_config(&root, &config_path, &options)?;
    validate_init_config_paths(&root, &mut preflight_config, &lock_path)?;
    let lock = acquire_mutation_lock_at_path(lock_path)?;
    let (created_config, mut config) = load_init_config(&root, &config_path, &options)?;
    validate_init_config_paths(&root, &mut config, &lock.path)?;
    let integration_executable = resolve_integration_executable(&options)?;
    let mut result = InitResult::default();
    let fragment_dir = root.join(&config.fragments.directory);
    let fragment_dir_exists = fragment_dir.exists();
    if fragment_dir_exists && !fragment_dir.is_dir() {
        return Err(Error::InitConflict {
            message: format!(
                "{} exists but is not a directory",
                config.fragments.directory.display()
            ),
        });
    }
    let changelog_path = root.join(&config.changelog.path);
    let changelog_exists = changelog_path.exists();
    if changelog_exists && !changelog_path.is_file() {
        return Err(Error::InitConflict {
            message: format!(
                "{} exists but is not a file",
                config.changelog.path.display()
            ),
        });
    }
    validate_init_changelog_path(&config_path, &changelog_path, &config.changelog.path)?;
    if created_config {
        fs::write(&config_path, render_init_config(&config)).map_err(|source| {
            Error::WriteFile {
                path: config_path.clone(),
                source,
            }
        })?;
        result
            .created_files
            .push(PathBuf::from(Repository::CONFIG_FILE));
    } else {
        result
            .skipped_existing_files
            .push(PathBuf::from(Repository::CONFIG_FILE));
    }
    validate_init_changelog_path(&config_path, &changelog_path, &config.changelog.path)?;
    if fragment_dir_exists {
        result
            .skipped_existing_files
            .push(config.fragments.directory.clone());
    } else {
        fs::create_dir_all(&fragment_dir).map_err(|source| Error::CreateDirectory {
            path: fragment_dir,
            source,
        })?;
        result
            .created_files
            .push(config.fragments.directory.clone());
    }

    let changelog_created = create_initial_changelog_if_absent(
        &config_path,
        &changelog_path,
        &config.changelog.path,
        &config,
    )?;
    if changelog_created {
        result.created_files.push(config.changelog.path.clone());
    } else {
        result
            .skipped_existing_files
            .push(config.changelog.path.clone());
    }

    match config.vcs.preset {
        VcsPreset::Git => {
            apply_git_integration(&root, &config, &integration_executable, &mut result)?;
        }
        VcsPreset::Hg => {
            apply_hg_integration(&root, &config, &integration_executable, &mut result)?;
        }
        VcsPreset::Jj => result.manual_actions_required.push(String::from(
            "Jujutsu does not support per-path merge drivers; after resolving concurrent fragment changes, run `sacho sync --force`",
        )),
        VcsPreset::None => {}
    }
    if options.install_hook {
        if config.vcs.preset == VcsPreset::Git && is_git_repository(&root) {
            install_commit_hooks(
                &root,
                &integration_executable,
                options.append_existing_hook,
                &mut result,
            )?;
        } else if config.vcs.preset == VcsPreset::Git {
            result.manual_actions_required.push(String::from(
                "commit hooks not installed: automatic installation requires a Git repository",
            ));
        } else {
            result.manual_actions_required.push(format!(
                "commit hooks not installed: staged missing-fragment checks require vcs.preset = \"git\", but this repository uses vcs.preset = {:?}",
                toml_vcs_preset(config.vcs.preset)
            ));
        }
    }

    Ok(result)
}

fn resolve_integration_executable(options: &InitOptions) -> Result<String> {
    let path = match &options.integration_executable {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|source| Error::CurrentExecutable { source })?,
    };
    let Some(executable) = path.to_str() else {
        return Err(Error::InvalidIntegrationExecutable {
            path,
            reason: "path is not valid UTF-8",
        });
    };
    if executable.is_empty() {
        return Err(Error::InvalidIntegrationExecutable {
            path,
            reason: "path must not be empty",
        });
    }
    if executable.trim() != executable {
        return Err(Error::InvalidIntegrationExecutable {
            path,
            reason: "path must not begin or end with whitespace",
        });
    }
    if executable.contains(['\n', '\r']) {
        return Err(Error::InvalidIntegrationExecutable {
            path,
            reason: "path must not contain a newline",
        });
    }
    Ok(executable.to_owned())
}

/// Infers a sanitized repository web URL from local VCS configuration.
///
/// Missing, ambiguous, unsupported, and malformed remote configuration all
/// return `None` without turning VCS inspection failures into diagnostics.
pub fn infer_repository_url(start: impl AsRef<Path>) -> Option<String> {
    let root = init_root(start.as_ref());
    let preset =
        repository_preset(&root).or_else(|| is_git_repository(&root).then_some(VcsPreset::Git))?;
    crate::repository_url::infer(&root, preset).map(|url| url.web_url)
}

/// Resolves the repository root used by [`init_repository`].
pub fn initialization_root(start: impl AsRef<Path>) -> PathBuf {
    init_root(start.as_ref())
}

/// Suggests a safe, unused fragment subdirectory for a section identifier.
pub fn suggest_section_directory(id: &str, used: &[PathBuf]) -> PathBuf {
    suggest_section_directory_avoiding(id, used, &[])
}

/// Suggests a safe fragment subdirectory outside used and reserved paths.
pub fn suggest_section_directory_avoiding(
    id: &str,
    used: &[PathBuf],
    reserved: &[PathBuf],
) -> PathBuf {
    let stem = id.rsplit('/').next().unwrap_or(id);
    let mut slug = String::new();
    let mut separator = false;
    for character in stem.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("section");
    }

    for suffix in 1..=used.len() + reserved.len() + 1 {
        let candidate = if suffix == 1 {
            PathBuf::from(&slug)
        } else {
            PathBuf::from(format!("{slug}-{suffix}"))
        };
        if !used.iter().chain(reserved).any(|path| path == &candidate) {
            return candidate;
        }
    }
    unreachable!("one more candidate than excluded paths must produce an unused path")
}

/// Finds repository directories whose basename resembles a section.
///
/// Returned values are repository-relative recursive glob patterns. VCS
/// metadata, dependency caches, build output, virtual environments, symlinked
/// directories, and the configured fragment tree are not traversed.
pub fn infer_section_paths(
    root: impl AsRef<Path>,
    id: &str,
    section_directory: &Path,
    fragment_directory: &Path,
) -> Result<Vec<String>> {
    const SKIPPED_DIRECTORIES: &[&str] = &[".git", ".hg", ".jj", ".venv", "node_modules", "target"];

    fn visit(
        root: &Path,
        relative: &Path,
        fragment_directory: &Path,
        wanted: &[String],
        matches: &mut Vec<String>,
    ) -> Result<()> {
        let absolute = root.join(relative);
        let entries = match fs::read_dir(&absolute) {
            Ok(entries) => entries,
            Err(_) if !relative.as_os_str().is_empty() => return Ok(()),
            Err(source) => {
                return Err(Error::ReadFile {
                    path: absolute,
                    source,
                });
            }
        };
        let mut entries = entries
            .filter_map(std::result::Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let path = relative.join(entry.file_name());
            if path == fragment_directory {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if SKIPPED_DIRECTORIES
                .iter()
                .any(|skipped| name.eq_ignore_ascii_case(skipped))
            {
                continue;
            }
            if wanted.iter().any(|wanted| wanted == &name)
                && let Some(path) = path.to_str()
            {
                matches.push(format!("{}/**", path.replace('\\', "/")));
            }
            visit(root, &path, fragment_directory, wanted, matches)?;
        }
        Ok(())
    }

    let id_stem = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
    let directory_stem = section_directory
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let wanted = vec![id_stem, directory_stem];
    let mut matches = Vec::new();
    visit(
        root.as_ref(),
        Path::new(""),
        fragment_directory,
        &wanted,
        &mut matches,
    )?;
    matches.sort();
    matches.dedup();
    Ok(matches)
}

/// Infers one reversible section pattern from selected changelog section ids.
///
/// Inference succeeds only when at least two ids share the same prefix and
/// their final components name sibling directories under exactly one common
/// repository path. Every selected section's rendered fragment directory must
/// also pass the runtime path-safety checks for patterned sections.
pub fn infer_section_pattern(
    root: impl AsRef<Path>,
    ids: &[String],
    fragment_directory: &Path,
) -> Result<Option<SectionPatternConfig>> {
    if ids.len() < 2 {
        return Ok(None);
    }

    let capture_pattern =
        SectionPattern::from_str("{name}").expect("the built-in single-capture pattern is valid");
    let mut id_prefix = None::<&str>;
    let mut common_parents = None::<BTreeSet<String>>;
    let mut capture_values = BTreeSet::new();
    for id in ids {
        let stem = id.rsplit('/').next().unwrap_or(id);
        if capture_pattern.captures(stem).is_none() || !capture_values.insert(stem) {
            return Ok(None);
        }
        let prefix = &id[..id.len() - stem.len()];
        match id_prefix {
            Some(expected) if expected != prefix => return Ok(None),
            None => id_prefix = Some(prefix),
            _ => {}
        }

        let parents = infer_section_paths(root.as_ref(), id, Path::new(stem), fragment_directory)?
            .into_iter()
            .filter_map(|path| path.strip_suffix("/**").map(str::to_owned))
            .filter_map(|path| {
                let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
                (name == stem).then(|| parent.to_owned())
            })
            .collect::<BTreeSet<_>>();
        common_parents = Some(match common_parents {
            Some(common) => common.intersection(&parents).cloned().collect(),
            None => parents,
        });
    }

    let mut parents = common_parents.unwrap_or_default().into_iter();
    let Some(parent) = parents.next() else {
        return Ok(None);
    };
    if parents.next().is_some() {
        return Ok(None);
    }
    let parent = escape_section_pattern_literal(&parent);
    let prefix = escape_section_pattern_literal(id_prefix.unwrap_or_default());
    let source = if parent.is_empty() {
        capture_pattern.to_string()
    } else {
        format!("{parent}/{capture_pattern}")
    };
    let id = format!("{prefix}{capture_pattern}");
    let Ok(source) = SectionPattern::from_str(&source) else {
        return Ok(None);
    };
    let Ok(id) = SectionPattern::from_str(&id) else {
        return Ok(None);
    };
    let pattern = SectionPatternConfig {
        source,
        id,
        directory: capture_pattern,
        paths: None,
    };
    if pattern.validate().is_err() {
        return Ok(None);
    }
    if !inferred_section_pattern_is_safe(root.as_ref(), ids, fragment_directory, &pattern) {
        return Ok(None);
    }
    Ok(Some(pattern))
}

fn inferred_section_pattern_is_safe(
    root: &Path,
    ids: &[String],
    fragment_directory: &Path,
    pattern: &SectionPatternConfig,
) -> bool {
    let default_config = Config::parse("").expect("default config parses");
    let mut directories = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(captures) = pattern.id.captures(id) else {
            return false;
        };
        let Ok(directory) = pattern.directory.render(&captures) else {
            return false;
        };
        directories.push(PathBuf::from(directory));
    }
    let lock_path = mutation_lock_path(root);
    validate_pattern_section_directories(
        root,
        fragment_directory,
        &default_config.fragments.next_file,
        &directories,
        &lock_path,
    )
    .is_ok()
}

fn escape_section_pattern_literal(value: &str) -> String {
    value.replace('{', "{{").replace('}', "}}")
}

fn load_init_config(
    root: &Path,
    config_path: &Path,
    options: &InitOptions,
) -> Result<(bool, Config)> {
    let created = !config_path.exists();
    let config = if created {
        let config = default_init_config(root, options)?;
        config.validate().map_err(|source| Error::Config {
            path: config_path.to_path_buf(),
            source: Box::new(source),
        })?;
        config
    } else {
        let contents = fs::read_to_string(config_path).map_err(|source| Error::ReadFile {
            path: config_path.to_path_buf(),
            source,
        })?;
        Config::parse(&contents).map_err(|source| Error::Config {
            path: config_path.to_path_buf(),
            source: Box::new(source),
        })?
    };
    Ok((created, config))
}

fn validate_init_config_paths(root: &Path, config: &mut Config, lock_path: &Path) -> Result<()> {
    let fragment_dir = root.join(&config.fragments.directory);
    if fragment_dir.exists() && !fragment_dir.is_dir() {
        return Err(Error::InitConflict {
            message: format!(
                "{} exists but is not a directory",
                config.fragments.directory.display()
            ),
        });
    }
    let changelog_path = root.join(&config.changelog.path);
    if changelog_path.exists() && !changelog_path.is_file() {
        return Err(Error::InitConflict {
            message: format!(
                "{} exists but is not a file",
                config.changelog.path.display()
            ),
        });
    }
    let mut validation_cache = PathValidationCache::default();
    let configured =
        validate_repository_config_paths_with_cache(root, config, &mut validation_cache).map_err(
            |source| Error::Config {
                path: root.join(Repository::CONFIG_FILE),
                source: Box::new(source),
            },
        )?;
    validate_configured_paths_against_reserved_with_cache(
        &configured,
        "repository mutation lock",
        lock_path,
        &mut validation_cache,
    )
    .map_err(|source| Error::Config {
        path: root.join(Repository::CONFIG_FILE),
        source: Box::new(source),
    })?;
    for (marker, key, path) in [
        (
            ".git",
            "potential Git repository mutation lock",
            root.join(".git/sacho.lock"),
        ),
        (
            ".jj",
            "potential Jujutsu repository mutation lock",
            root.join(".jj/sacho.lock"),
        ),
        (
            ".hg",
            "potential Mercurial repository mutation lock",
            root.join(".hg/sacho.lock"),
        ),
    ] {
        let marker = root.join(marker);
        if marker.exists() && !marker.is_dir() {
            continue;
        }
        validate_configured_paths_against_reserved_with_cache(
            &configured,
            key,
            &path,
            &mut validation_cache,
        )
        .map_err(|source| Error::Config {
            path: root.join(Repository::CONFIG_FILE),
            source: Box::new(source),
        })?;
    }
    normalize_repository_config_paths(root, config, &configured);
    Ok(())
}

/// Creates a changelog fragment.
pub fn add_fragment(repo: &Repository, options: AddOptions) -> Result<AddResult> {
    let name = validate_fragment_name(&options.name)?;
    let config = repo.config();
    let (directory, patterned_directory) = if !has_sections(config) {
        if options.section.is_some() {
            return Err(Error::UnexpectedSection);
        }
        (config.fragments.directory.clone(), None)
    } else {
        let section_id = options.section.as_deref().ok_or(Error::MissingSection)?;
        let resolver = SectionResolver::from_config(config)?;
        let section = resolver
            .resolve_id(section_id)
            .map_err(|error| Error::SectionPattern {
                message: error.to_string(),
            })?
            .ok_or_else(|| Error::UnknownSection {
                section: section_id.to_owned(),
            })?;
        let patterned_directory = section.pattern_index.map(|_| section.directory.clone());
        (
            config.fragments.directory.join(&section.directory),
            patterned_directory,
        )
    };
    let path = directory.join(&name);
    let _lock = acquire_mutation_lock(repo)?;
    let absolute = match patterned_directory {
        Some(directory) => repo
            .validate_pattern_section_directory(&directory)?
            .join(&name),
        None => repo.resolve(&path),
    };
    ensure_materialized_current_before_mutation(repo)?;
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
    if let Err(source) = file.write_all(b" -\n") {
        drop(file);
        fs::remove_file(&absolute).map_err(|cleanup_source| Error::WriteFile {
            path: absolute.clone(),
            source: cleanup_source,
        })?;
        return Err(Error::WriteFile {
            path: absolute,
            source,
        });
    }
    drop(file);

    let sync = match sync_after_mutation(repo) {
        Ok(sync) => sync,
        Err(error) => {
            fs::remove_file(&absolute).map_err(|source| Error::WriteFile {
                path: absolute,
                source,
            })?;
            return Err(error);
        }
    };

    Ok(AddResult { path, sync })
}

/// Sets the next unreleased version and synchronizes materialized output.
///
/// The next-version file, every fragment snapshot, and the changelog form one
/// transaction.  An error returned after applying begins includes any rollback
/// failures; otherwise every applied participant was restored.  Cleanup that
/// fails only after commit is reported through [`NextResult::cleanup_warnings`].
pub fn set_next_version(repo: &Repository, options: NextOptions) -> Result<NextResult> {
    let version = options.version.trim();
    if version.is_empty() {
        return Err(Error::EmptyNextVersion);
    }
    let _lock = acquire_mutation_lock(repo)?;
    let (plan, path, sync) = plan_next_mutation(repo, version)?;
    let outcome = apply_repository_mutation(repo, &plan)?;
    Ok(NextResult {
        path,
        sync,
        cleanup_warnings: outcome.cleanup_warnings,
    })
}

fn plan_next_mutation(
    repo: &Repository,
    version: &str,
) -> Result<(RepositoryMutationPlan, PathBuf, Option<SyncResult>)> {
    let next_path = next_version_path(repo);
    let next_contents = format!("{version}\n");
    let next_label = version_label_from_contents(&repo.resolve(&next_path), Some(&next_contents))?;
    let fragment_sources = snapshot_release_fragment_sources(repo)?;
    let fragment_paths = fragment_sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    let next_before = read_release_file_state(repo, &next_path)?;
    let next_after = ReleaseFileState::Present(next_contents);
    let mut participants = vec![ReleaseFileChange {
        path: next_path.clone(),
        before: next_before.clone(),
        after: next_after,
    }];
    participants.extend(fragment_sources.iter().map(|source| {
        let state = release_file_state_from_bytes(source.contents.clone());
        ReleaseFileChange {
            path: source.path.clone(),
            before: state.clone(),
            after: state,
        }
    }));

    let sync = if repo.config().changelog.materialize {
        let parsed = parse_release_fragment_sources(repo, &fragment_sources)?;
        let next_contents = release_file_state_utf8(repo, &next_path, &next_before)?;
        let before_label = version_label_from_contents(&repo.resolve(&next_path), next_contents)?;
        let compiled_before = compile_parsed_fragments(
            repo,
            CompileOptions::default(),
            before_label,
            parsed.clone(),
        )?;
        let changelog_path = repo.config().changelog.path.clone();
        let changelog_before = read_release_file_state(repo, &changelog_path)?;
        if matches!(changelog_before, ReleaseFileState::Missing) {
            return Err(Error::ReadFile {
                path: repo.resolve(&changelog_path),
                source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
            });
        }
        ensure_materialized_release_snapshot_current(
            repo,
            &changelog_path,
            &changelog_before,
            &compiled_before,
        )?;
        let compiled_after =
            compile_parsed_fragments(repo, CompileOptions::default(), next_label, parsed)?;
        let old_changelog = release_file_state_utf8(repo, &changelog_path, &changelog_before)?
            .expect("materialized next planning requires an existing changelog");
        let (new_contents, _) = synchronize_materialized_changelog(
            repo,
            old_changelog,
            &compiled_after,
            &changelog_path,
        )?;
        let changed = new_contents != *old_changelog;
        participants.push(ReleaseFileChange {
            path: changelog_path,
            before: changelog_before,
            after: ReleaseFileState::Present(new_contents),
        });
        Some(SyncResult { changed })
    } else {
        None
    };

    Ok((
        RepositoryMutationPlan {
            command: MutationCommand::Next,
            participants,
            fragment_paths_before: fragment_paths.clone(),
            fragment_paths_after: fragment_paths,
        },
        next_path,
        sync,
    ))
}

/// Formats all changelog fragments into normal form as one transaction.
///
/// See [`apply_format`] for the rollback and committed-cleanup guarantees.
pub fn format_fragments(repo: &Repository, options: FormatOptions) -> Result<FormatResult> {
    let plan = plan_format(repo, options)?;
    if matches!(plan.sync, SyncPlan::NeedsConfirmation { .. }) {
        return Err(Error::SyncNeedsConfirmation {
            path: repo.config().changelog.path.clone(),
        });
    }
    apply_format(repo, plan)
}

/// Plans fragment formatting and materialized changelog synchronization.
///
/// This function does not mutate the repository.  When [`FormatPlan::sync`]
/// requires confirmation, callers must obtain it before calling
/// [`apply_format`].
pub fn plan_format(repo: &Repository, _options: FormatOptions) -> Result<FormatPlan> {
    let discovered = discover_fragment_candidates(repo)?;
    let mut changed = Vec::new();
    let mut snapshots = Vec::with_capacity(discovered.candidates.len());
    let mut parsed = Vec::with_capacity(discovered.candidates.len());

    for candidate in discovered.candidates {
        let source = fs::read_to_string(&candidate.path).map_err(|source| Error::ReadFile {
            path: candidate.path.clone(),
            source,
        })?;
        let formatted = format_fragment_source(&source)
            .map_err(|error| with_fragment_path(error, candidate.relative_path.clone()))?;
        let fragment = parse_fragment(
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
            changed.push(candidate.relative_path.clone());
        }
        snapshots.push(FormatFragmentSnapshot {
            path: candidate.relative_path,
            before: source,
            after: formatted,
        });
        parsed.push(fragment);
    }

    let formatting = FormatResult {
        changed,
        cleanup_warnings: Vec::new(),
    };
    if !repo.config().changelog.materialize {
        return Ok(FormatPlan {
            formatting,
            sync: SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled),
            fragments: snapshots,
            next_file: None,
            changelog: None,
        });
    }

    let next_path = next_version_path(repo);
    let next_file = read_format_file_snapshot(repo, &next_path)?;
    let version_label =
        version_label_from_contents(&repo.resolve(&next_path), next_file.contents.as_deref())?;
    let compiled =
        compile_parsed_fragments(repo, CompileOptions::default(), version_label, parsed)?;
    let changelog_path = repo.config().changelog.path.clone();
    let changelog = read_format_file_snapshot(repo, &changelog_path)?;
    let old_contents = changelog.contents.clone().ok_or_else(|| Error::ReadFile {
        path: repo.resolve(&changelog_path),
        source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
    })?;
    let sync =
        plan_sync_from_contents(repo, &compiled, old_contents, SyncOptions { force: false })?;

    Ok(FormatPlan {
        formatting,
        sync,
        fragments: snapshots,
        next_file: Some(next_file),
        changelog: Some(changelog),
    })
}

/// Applies a previously planned fragment formatting operation.
///
/// The complete fragment set, next-version file, and materialized changelog are
/// revalidated under the repository mutation lock before the first write.  All
/// replacements use atomic conditional claims, and an apply failure rolls back
/// replacements that were already installed.  Concurrent edits encountered
/// during rollback are preserved and included in the returned error.  Once all
/// participants have committed, claim-removal failures are returned as
/// [`FormatResult::cleanup_warnings`] instead of turning success into an error.
pub fn apply_format(repo: &Repository, plan: FormatPlan) -> Result<FormatResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let mutation = format_mutation_plan(&plan);
    let outcome = apply_repository_mutation(repo, &mutation)?;
    let mut result = plan.formatting;
    result.cleanup_warnings = outcome.cleanup_warnings;
    Ok(result)
}

fn format_mutation_plan(plan: &FormatPlan) -> RepositoryMutationPlan {
    let mut participants = plan
        .fragments
        .iter()
        .map(|fragment| ReleaseFileChange {
            path: fragment.path.clone(),
            before: ReleaseFileState::Present(fragment.before.clone()),
            after: ReleaseFileState::Present(fragment.after.clone()),
        })
        .collect::<Vec<_>>();
    if let Some(next_file) = &plan.next_file {
        let state = release_state_from_optional_contents(next_file.contents.clone());
        participants.push(ReleaseFileChange {
            path: next_file.path.clone(),
            before: state.clone(),
            after: state,
        });
    }
    if let Some(changelog) = &plan.changelog {
        let before = release_state_from_optional_contents(changelog.contents.clone());
        let after = format_sync_pending(&plan.sync).map_or_else(
            || before.clone(),
            |pending| ReleaseFileState::Present(pending.new_contents.clone()),
        );
        participants.push(ReleaseFileChange {
            path: changelog.path.clone(),
            before,
            after,
        });
    }
    let fragment_paths = plan
        .fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    RepositoryMutationPlan {
        command: MutationCommand::Format,
        participants,
        fragment_paths_before: fragment_paths.clone(),
        fragment_paths_after: fragment_paths,
    }
}

fn format_sync_pending(sync: &SyncPlan) -> Option<&PendingWrite> {
    match sync {
        SyncPlan::Apply(pending) | SyncPlan::NeedsConfirmation { pending, .. } => Some(pending),
        SyncPlan::Skipped(_) => None,
    }
}

fn read_format_file_snapshot(repo: &Repository, path: &Path) -> Result<FormatFileSnapshot> {
    let absolute = repo.resolve(path);
    let contents = match fs::read_to_string(&absolute) {
        Ok(contents) => Some(contents),
        Err(source) if source.kind() == ErrorKind::NotFound => None,
        Err(source) => {
            return Err(Error::ReadFile {
                path: absolute,
                source,
            });
        }
    };
    Ok(FormatFileSnapshot {
        path: path.to_path_buf(),
        contents,
    })
}

/// Compiles the current fragments into an unreleased changelog region.
pub fn compile_unreleased(repo: &Repository, options: CompileOptions) -> Result<CompiledRegion> {
    crate::compile::compile_unreleased(repo, options)
}

/// Compiles the current fragments, optionally resolving unpinned links in memory.
///
/// This function never writes fragment or changelog files.
pub fn compile_unreleased_with_link_resolution(
    repo: &Repository,
    options: CompileOptions,
    policy: LinkResolutionPolicy,
) -> Result<CompiledRegion> {
    if !policy.is_enabled(repo.config()) {
        return compile_unreleased(repo, options);
    }
    let snapshots = prepare_resolved_fragments(repo, options.section.as_deref())?;
    let version_label = current_version_label(repo)?;
    compile_parsed_fragments(
        repo,
        options,
        version_label,
        snapshots
            .into_iter()
            .map(|snapshot| snapshot.parsed_after)
            .collect(),
    )
}

/// Plans link resolution and any materialized changelog synchronization.
///
/// Planning completes every network request but does not write repository files.
pub fn plan_resolve_links(
    repo: &Repository,
    options: ResolveLinksOptions,
) -> Result<ResolveLinksPlan> {
    plan_resolve_links_with_snapshot_hook(repo, options, || {})
}

fn plan_resolve_links_with_snapshot_hook(
    repo: &Repository,
    options: ResolveLinksOptions,
    after_next_snapshot: impl FnOnce(),
) -> Result<ResolveLinksPlan> {
    let snapshots = prepare_resolved_fragments(repo, None)?;
    let changed_fragments = snapshots
        .iter()
        .filter(|snapshot| snapshot.before != snapshot.after)
        .map(|snapshot| snapshot.path.clone())
        .collect::<Vec<_>>();
    let fragment_paths = snapshots
        .iter()
        .map(|snapshot| snapshot.path.clone())
        .collect::<Vec<_>>();
    let mut participants = snapshots
        .iter()
        .map(|snapshot| ReleaseFileChange {
            path: snapshot.path.clone(),
            before: ReleaseFileState::Present(snapshot.before.clone()),
            after: ReleaseFileState::Present(snapshot.after.clone()),
        })
        .collect::<Vec<_>>();

    let sync = if repo.config().changelog.materialize {
        let next_path = next_version_path(repo);
        let next_file = read_format_file_snapshot(repo, &next_path)?;
        let version_label =
            version_label_from_contents(&repo.resolve(&next_path), next_file.contents.as_deref())?;
        after_next_snapshot();
        let compiled_before = compile_parsed_fragments(
            repo,
            CompileOptions::default(),
            version_label.clone(),
            snapshots
                .iter()
                .map(|snapshot| snapshot.parsed_before.clone())
                .collect(),
        )?;
        let compiled_after = compile_parsed_fragments(
            repo,
            CompileOptions::default(),
            version_label,
            snapshots
                .iter()
                .map(|snapshot| snapshot.parsed_after.clone())
                .collect(),
        )?;
        let next_state = release_state_from_optional_contents(next_file.contents);
        participants.push(ReleaseFileChange {
            path: next_path,
            before: next_state.clone(),
            after: next_state,
        });

        let changelog_path = repo.config().changelog.path.clone();
        let changelog = read_format_file_snapshot(repo, &changelog_path)?;
        let old_contents = changelog.contents.ok_or_else(|| Error::ReadFile {
            path: repo.resolve(&changelog_path),
            source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
        })?;
        let (expected_before, _) = synchronize_materialized_changelog(
            repo,
            &old_contents,
            &compiled_before,
            &changelog_path,
        )?;
        let generated_change_only = expected_before == old_contents;
        let sync = plan_sync_from_contents(
            repo,
            &compiled_after,
            old_contents.clone(),
            SyncOptions {
                force: options.force || generated_change_only,
            },
        )?;
        let after = format_sync_pending(&sync).map_or_else(
            || ReleaseFileState::Present(old_contents.clone()),
            |pending| ReleaseFileState::Present(pending.new_contents.clone()),
        );
        participants.push(ReleaseFileChange {
            path: changelog_path,
            before: ReleaseFileState::Present(old_contents),
            after,
        });
        sync
    } else {
        SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled)
    };

    Ok(ResolveLinksPlan {
        changed_fragments,
        sync,
        mutation: RepositoryMutationPlan {
            command: MutationCommand::ResolveLinks,
            participants,
            fragment_paths_before: fragment_paths.clone(),
            fragment_paths_after: fragment_paths,
        },
    })
}

/// Applies a previously planned reference-link resolution.
pub fn apply_resolve_links(
    repo: &Repository,
    plan: ResolveLinksPlan,
) -> Result<ResolveLinksResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let changelog_changed = format_sync_pending(&plan.sync)
        .is_some_and(|pending| pending.old_contents != pending.new_contents);
    let changed_fragments = plan.changed_fragments;
    let outcome = apply_repository_mutation(repo, &plan.mutation)?;
    Ok(ResolveLinksResult {
        changed_fragments,
        changelog_changed,
        cleanup_warnings: outcome.cleanup_warnings,
    })
}

fn current_version_label(repo: &Repository) -> Result<VersionLabel> {
    let path = next_version_path(repo);
    let snapshot = read_format_file_snapshot(repo, &path)?;
    version_label_from_contents(&repo.resolve(&path), snapshot.contents.as_deref())
}

fn prepare_resolved_fragments(
    repo: &Repository,
    resolve_section: Option<&str>,
) -> Result<Vec<ResolvedFragmentSnapshot>> {
    let discovered = discover_fragment_candidates(repo)?;
    let mut parsed = Vec::with_capacity(discovered.candidates.len());
    let mut targets = BTreeMap::<String, (String, u64)>::new();
    let mut resolved = BTreeMap::<String, String>::new();

    for candidate in discovered.candidates {
        let should_resolve =
            resolve_section.is_none_or(|section| candidate.section.as_deref() == Some(section));
        let source = fs::read_to_string(&candidate.path).map_err(|source| Error::ReadFile {
            path: candidate.path.clone(),
            source,
        })?;
        let fragment = parse_fragment(
            candidate.relative_path.clone(),
            &source,
            candidate.section.clone(),
            &repo.config().links,
        )
        .map_err(|source| Error::Fragment {
            path: candidate.relative_path.clone(),
            source,
        })?;
        let used = fragment
            .items
            .iter()
            .flat_map(|item| item.references.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        for reference in &used {
            if should_resolve {
                targets
                    .entry(reference.label.clone())
                    .or_insert_with(|| (reference.sigil.clone(), reference.number));
            }
            if let Some(url) = fragment.links.get(&reference.label) {
                if let Some(existing) = resolved.get(&reference.label)
                    && existing != url
                {
                    return Err(Error::ConflictingResolvedLinks {
                        label: reference.label.clone(),
                        first: existing.clone(),
                        second: url.clone(),
                    });
                }
                resolved.insert(reference.label.clone(), url.clone());
            }
        }
        parsed.push((
            candidate.relative_path,
            candidate.section,
            source,
            fragment,
            used,
            should_resolve,
        ));
    }

    let resolver = ReferenceUrlResolver::new();
    for (label, (sigil, number)) in &targets {
        if resolved.contains_key(label) {
            continue;
        }
        let template = repo
            .config()
            .links
            .iter()
            .find(|(configured, _)| configured.as_str() == sigil)
            .map(|(_, template)| template)
            .expect("parsed references always retain a configured sigil");
        let source_url = template.as_str().replace("{n}", &number.to_string());
        if !is_http_reference_url(&source_url) {
            continue;
        }
        resolved.insert(label.clone(), resolver.resolve(label, &source_url)?);
    }

    parsed
        .into_iter()
        .map(
            |(path, section, before, parsed_before, used, should_resolve)| {
                if !should_resolve {
                    return Ok(ResolvedFragmentSnapshot {
                        path,
                        after: before.clone(),
                        before,
                        parsed_after: parsed_before.clone(),
                        parsed_before,
                    });
                }
                let links = used
                    .iter()
                    .filter_map(|reference| {
                        resolved
                            .get(&reference.label)
                            .map(|url| (reference.label.clone(), url.clone()))
                    })
                    .collect::<BTreeMap<_, _>>();
                let after = source_with_resolved_links(&before, &links)
                    .map_err(|error| with_fragment_path(error, path.clone()))?;
                let parsed_after =
                    parse_fragment(path.clone(), &after, section, &repo.config().links).map_err(
                        |source| Error::Fragment {
                            path: path.clone(),
                            source,
                        },
                    )?;
                Ok(ResolvedFragmentSnapshot {
                    path,
                    before,
                    after,
                    parsed_before,
                    parsed_after,
                })
            },
        )
        .collect()
}

fn source_with_resolved_links(source: &str, links: &BTreeMap<String, String>) -> Result<String> {
    let mut parsed = parse_frontmatter_for_format(source)?;
    parsed
        .frontmatter
        .retain(|entry| entry.key != Value::String(String::from("links")));
    if !links.is_empty() {
        let mut mapping = Mapping::new();
        for (label, url) in links {
            mapping.insert(Value::String(label.clone()), Value::String(url.clone()));
        }
        parsed.frontmatter.push(FrontmatterEntry {
            key: Value::String(String::from("links")),
            value: Value::Mapping(mapping),
        });
    }
    let mut output = canonical_frontmatter(parsed.frontmatter)?;
    output.push_str(parsed.body);
    Ok(output)
}

/// Reads one released section from the configured changelog.
pub fn show(repo: &Repository, options: ShowOptions) -> Result<ReleasedSection> {
    let config = &repo.config().changelog;
    let changelog_path = config.path.clone();
    let absolute_path = repo.resolve(&changelog_path);
    let changelog = fs::read_to_string(&absolute_path).map_err(|source| Error::ReadFile {
        path: absolute_path,
        source,
    })?;
    let unreleased_region = if config.materialize {
        match find_unreleased_region(
            &changelog,
            config.region_detection,
            &config.unreleased_heading,
        ) {
            Ok(region) => Some(region),
            Err(ChangelogError::RegionNotFound) => None,
            Err(source) => return Err(changelog_error(changelog_path, source)),
        }
    } else {
        None
    };
    render_released_section(
        &changelog,
        &options.version,
        unreleased_region,
        options.skip_heading,
        options.word_wrap,
    )?
    .ok_or(Error::ReleasedVersionNotFound {
        version: options.version,
    })
}

/// Plans a synchronization between fragments and the materialized changelog.
pub fn plan_sync(repo: &Repository, options: SyncOptions) -> Result<SyncPlan> {
    plan_sync_unlocked(repo, options)
}

fn plan_sync_unlocked(repo: &Repository, options: SyncOptions) -> Result<SyncPlan> {
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
    plan_sync_from_contents(repo, &compiled, old_contents, options)
}

fn plan_sync_from_contents(
    repo: &Repository,
    compiled: &CompiledRegion,
    old_contents: String,
    options: SyncOptions,
) -> Result<SyncPlan> {
    let changelog_path = repo.config().changelog.path.clone();
    let (new_contents, safe_to_apply) =
        synchronize_materialized_changelog(repo, &old_contents, compiled, &changelog_path)?;

    if old_contents == new_contents {
        return Ok(SyncPlan::Skipped(SyncSkipReason::AlreadyCurrent));
    }

    let pending = PendingWrite {
        path: changelog_path,
        old_contents,
        new_contents,
    };
    if options.force || safe_to_apply {
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

fn synchronize_materialized_changelog(
    repo: &Repository,
    source: &str,
    compiled: &CompiledRegion,
    changelog_path: &Path,
) -> Result<(String, bool)> {
    let config = &repo.config().changelog;
    if !compiled.is_active()
        && let Ok(region) =
            find_unreleased_region(source, config.region_detection, &config.unreleased_heading)
        && source[region.start..region.end].trim().is_empty()
    {
        return Ok((source.to_owned(), true));
    }
    match replace_unreleased_region(
        source,
        &compiled.markdown,
        config.region_detection,
        &config.unreleased_heading,
    ) {
        Ok(replacement) => Ok((replacement.new_contents, false)),
        Err(ChangelogError::RegionNotFound)
            if !compiled.is_active() && config.region_detection == RegionDetection::Heading =>
        {
            Ok((source.to_owned(), true))
        }
        Err(ChangelogError::RegionNotFound)
            if compiled.is_active() && config.region_detection == RegionDetection::Heading =>
        {
            Ok((
                insert_unreleased_region(source, &compiled.markdown, &config.title),
                true,
            ))
        }
        Err(source) => Err(changelog_error(changelog_path.to_path_buf(), source)),
    }
}

/// Applies a previously planned synchronization.
pub fn apply_sync(repo: &Repository, plan: SyncPlan) -> Result<SyncResult> {
    let _lock = acquire_mutation_lock(repo)?;
    apply_sync_unlocked(repo, plan)
}

fn apply_sync_unlocked(repo: &Repository, plan: SyncPlan) -> Result<SyncResult> {
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

    let absolute_path = repo.resolve(&pending.path);
    let current_contents =
        fs::read_to_string(&absolute_path).map_err(|source| Error::ReadFile {
            path: absolute_path,
            source,
        })?;
    if current_contents != pending.old_contents {
        return Err(Error::StaleSyncPlan { path: pending.path });
    }

    repo.atomic_write(&pending.path, pending.new_contents.as_bytes())?;
    Ok(SyncResult { changed: true })
}

/// Plans a release operation.
pub fn plan_release(repo: &Repository, options: ReleaseOptions) -> Result<ReleasePlan> {
    plan_release_with_resolved_fragments(repo, options, None)
}

/// Plans a release whose unpinned reference links are resolved in memory.
///
/// The consumed fragment files are not rewritten before they are removed by
/// [`apply_release`].
pub fn plan_release_with_link_resolution(
    repo: &Repository,
    options: ReleaseOptions,
) -> Result<ReleasePlan> {
    let resolved = prepare_resolved_fragments(repo, None)?;
    plan_release_with_resolved_fragments(repo, options, Some(resolved))
}

fn plan_release_with_resolved_fragments(
    repo: &Repository,
    options: ReleaseOptions,
    resolved: Option<Vec<ResolvedFragmentSnapshot>>,
) -> Result<ReleasePlan> {
    let _lock = acquire_mutation_lock(repo)?;

    let changelog_path = repo.config().changelog.path.clone();
    release_failpoint(ReleaseApplyStage::PlanSnapshot, &changelog_path, repo)?;
    let changelog_before = read_release_file_state(repo, &changelog_path)?;
    if repo.config().changelog.materialize && matches!(changelog_before, ReleaseFileState::Missing)
    {
        return Err(Error::ReadFile {
            path: repo.resolve(&changelog_path),
            source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
        });
    }
    let next_path = next_version_path(repo);
    let next_before = read_release_file_state(repo, &next_path)?;
    let next_version = release_next_version(repo, &next_path, &next_before)?;
    let version = match (
        options.version.as_deref().map(str::trim),
        next_version.as_deref(),
    ) {
        (Some(""), _) => return Err(Error::MissingReleaseVersion),
        (Some(version), Some(next_version)) if version != next_version => {
            return Err(Error::ReleaseVersionMismatch {
                version: version.to_owned(),
                next_version: next_version.to_owned(),
            });
        }
        (Some(version), _) => version.to_owned(),
        (None, Some(next_version)) => next_version.to_owned(),
        (None, None) => return Err(Error::MissingReleaseVersion),
    };
    let date = options.date;
    if !date.is_valid() {
        return Err(Error::InvalidReleaseDate {
            date: format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
        });
    }
    let (consumed_fragments, parsed_fragments_before) = snapshot_release_fragments(repo)?;
    let parsed_fragments = if let Some(resolved) = resolved {
        if consumed_fragments.len() != resolved.len() {
            return Err(Error::StaleReleasePlan {
                path: resolved
                    .first()
                    .map(|planned| planned.path.clone())
                    .unwrap_or_else(|| repo.config().fragments.directory.clone()),
            });
        }
        for (actual, planned) in consumed_fragments.iter().zip(&resolved) {
            if actual.path != planned.path {
                return Err(Error::StaleReleasePlan {
                    path: planned.path.clone(),
                });
            }
            if actual.contents != planned.before {
                return Err(Error::StaleReleasePlan {
                    path: planned.path.clone(),
                });
            }
        }
        resolved
            .into_iter()
            .map(|snapshot| snapshot.parsed_after)
            .collect()
    } else {
        parsed_fragments_before.clone()
    };
    let version_label = next_version
        .as_ref()
        .map_or(VersionLabel::Unreleased, |version| {
            VersionLabel::Version(version.clone())
        });
    let compiled_before = compile_parsed_fragments(
        repo,
        CompileOptions::default(),
        version_label.clone(),
        parsed_fragments_before,
    )?;
    ensure_materialized_release_snapshot_current(
        repo,
        &changelog_path,
        &changelog_before,
        &compiled_before,
    )?;
    let compiled = compile_parsed_fragments(
        repo,
        CompileOptions::default(),
        version_label,
        parsed_fragments,
    )?;
    let old_changelog = release_file_state_utf8(repo, &changelog_path, &changelog_before)?
        .map_or_else(
            || initial_changelog(&repo.config().changelog.title),
            ToOwned::to_owned,
        );
    let unreleased_region = if repo.config().changelog.materialize {
        match find_unreleased_region(
            &old_changelog,
            repo.config().changelog.region_detection,
            &repo.config().changelog.unreleased_heading,
        ) {
            Ok(region) => Some(region),
            Err(ChangelogError::RegionNotFound) => None,
            Err(source) => return Err(changelog_error(changelog_path.clone(), source)),
        }
    } else {
        None
    };
    let is_initial_release = consumed_fragments.is_empty()
        && !has_released_sections(
            &old_changelog,
            unreleased_region,
            &repo.config().changelog.title,
        );
    if compiled.substantive_item_count == 0 && !options.allow_empty && !is_initial_release {
        return Err(Error::EmptyRelease);
    }
    let released_markdown = released_markdown(&compiled.markdown, &version, date, repo);
    let next = options
        .next
        .map(|next| next.trim().to_owned())
        .filter(|next| !next.is_empty());
    let new_changelog = if repo.config().changelog.materialize {
        let next_unreleased = next
            .as_deref()
            .map(|next| empty_unreleased_markdown(repo, next));
        if unreleased_region.is_some() {
            replace_region_for_release(
                &old_changelog,
                next_unreleased.as_deref(),
                &released_markdown,
                repo.config().changelog.region_detection,
                &repo.config().changelog.unreleased_heading,
                &repo.config().changelog.title,
            )
            .map_err(|source| changelog_error(changelog_path.clone(), source))?
        } else {
            let released = insert_released_section(
                &old_changelog,
                &released_markdown,
                &repo.config().changelog.title,
            );
            match next_unreleased {
                Some(unreleased) => {
                    insert_unreleased_region(&released, &unreleased, &repo.config().changelog.title)
                }
                None => released,
            }
        }
    } else {
        insert_released_section(
            &old_changelog,
            &released_markdown,
            &repo.config().changelog.title,
        )
    };
    let next_after = next
        .as_deref()
        .filter(|next| !next.is_empty())
        .map_or(ReleaseFileState::Missing, |next| {
            ReleaseFileState::Present(format!("{next}\n"))
        });

    let plan = ReleasePlan {
        version,
        date,
        next,
        released_markdown,
        changelog: ReleaseFileChange {
            path: changelog_path,
            before: changelog_before,
            after: ReleaseFileState::Present(new_changelog),
        },
        next_file: ReleaseFileChange {
            path: next_path,
            before: next_before,
            after: next_after,
        },
        consumed_fragments,
    };
    validate_distinct_release_paths(repo, &plan)?;
    Ok(plan)
}

fn ensure_materialized_release_snapshot_current(
    repo: &Repository,
    changelog_path: &Path,
    changelog: &ReleaseFileState,
    compiled: &CompiledRegion,
) -> Result<()> {
    if !repo.config().changelog.materialize {
        return Ok(());
    }
    let contents = release_file_state_utf8(repo, changelog_path, changelog)?
        .expect("materialized release planning rejects a missing changelog");
    let (new_contents, _) =
        synchronize_materialized_changelog(repo, contents, compiled, changelog_path)?;
    if *contents == new_contents {
        Ok(())
    } else {
        Err(Error::SyncNeedsConfirmation {
            path: changelog_path.to_path_buf(),
        })
    }
}

/// Applies a previously planned release.
pub fn apply_release(repo: &Repository, plan: ReleasePlan) -> Result<ReleaseResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let participant_identities = validate_distinct_release_paths(repo, &plan)?;
    validate_release_plan(repo, &plan)?;
    probe_release_move_support(repo, &plan, &participant_identities)?;

    let prepared_changelog =
        prepare_release_change(repo, &plan.changelog, &participant_identities)?;
    let prepared_next = prepare_release_change(repo, &plan.next_file, &participant_identities)?;
    let mut applied = Vec::new();

    let changelog_outcome = match apply_release_change(
        repo,
        &plan.changelog,
        prepared_changelog,
        &participant_identities,
    ) {
        Ok(outcome) => outcome,
        Err(error) => return Err(release_apply_error(error, Vec::new())),
    };
    applied.push(AppliedReleaseChange {
        path: plan.changelog.path.clone(),
        before: plan.changelog.before.clone(),
        after: plan.changelog.after.clone(),
        original: changelog_outcome.original,
        created_directories: changelog_outcome.created_directories,
    });

    let next_outcome = match apply_release_change(
        repo,
        &plan.next_file,
        prepared_next,
        &participant_identities,
    ) {
        Ok(outcome) => outcome,
        Err(error) => return Err(rollback_release(repo, error, applied)),
    };
    let unchanged_participants: &[ReleaseFileChange] =
        if plan.next_file.before == plan.next_file.after {
            std::slice::from_ref(&plan.next_file)
        } else {
            &[]
        };
    if unchanged_participants.is_empty() {
        applied.push(AppliedReleaseChange {
            path: plan.next_file.path.clone(),
            before: plan.next_file.before.clone(),
            after: plan.next_file.after.clone(),
            original: next_outcome.original,
            created_directories: next_outcome.created_directories,
        });
    }

    for fragment in &plan.consumed_fragments {
        let original = match remove_release_fragment(repo, fragment, &participant_identities) {
            Ok(original) => original,
            Err(error) => return Err(rollback_release(repo, error, applied)),
        };
        applied.push(AppliedReleaseChange {
            path: fragment.path.clone(),
            before: ReleaseFileState::Present(fragment.contents.clone()),
            after: ReleaseFileState::Missing,
            original: Some(original),
            created_directories: Vec::new(),
        });
    }

    if let Err(failure) = commit_release_claims(repo, &mut applied, unchanged_participants) {
        return Err(match failure {
            ReleaseClaimCommitFailure::BeforeCommit(error) => {
                rollback_release(repo, error, applied)
            }
            ReleaseClaimCommitFailure::AfterCommit(failures) => Error::ReleaseCleanup { failures },
        });
    }

    let mut changed_paths = vec![plan.changelog.path, plan.next_file.path];
    changed_paths.extend(
        plan.consumed_fragments
            .into_iter()
            .map(|fragment| fragment.path),
    );
    Ok(ReleaseResult { changed_paths })
}

/// Carries entries from an existing release into unreleased fragments.
///
/// All carried fragments and materialized output are committed together.  An
/// apply error restores earlier writes in reverse order without overwriting a
/// concurrent edit; failures after commit are returned through
/// [`CarryResult::cleanup_warnings`].
pub fn carry(repo: &Repository, options: CarryOptions) -> Result<CarryResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let (plan, written_fragments, sync) = plan_carry_mutation(repo, &options.version)?;
    let outcome = apply_repository_mutation(repo, &plan)?;
    Ok(CarryResult {
        written_fragments,
        sync,
        cleanup_warnings: outcome.cleanup_warnings,
    })
}

/// Plans importing the materialized unreleased region into fragments.
///
/// The plan refuses repositories that already contain Markdown fragments and
/// validates every generated fragment before returning. No file is changed
/// until [`apply_import_unreleased`] is called.
pub fn plan_import_unreleased(
    repo: &Repository,
    options: ImportUnreleasedOptions,
) -> Result<ImportUnreleasedPlan> {
    if !repo.config().changelog.materialize {
        return Err(Error::UnreleasedImportRequiresMaterialization);
    }
    let before_sources = snapshot_release_fragment_sources(repo)?;
    if !before_sources.is_empty() {
        return Err(Error::UnreleasedImportExistingFragments);
    }

    let changelog_path = repo.config().changelog.path.clone();
    let changelog_before = read_release_file_state(repo, &changelog_path)?;
    let changelog =
        release_file_state_utf8(repo, &changelog_path, &changelog_before)?.ok_or_else(|| {
            Error::ReadFile {
                path: repo.resolve(&changelog_path),
                source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
            }
        })?;
    let region = find_unreleased_region(
        changelog,
        repo.config().changelog.region_detection,
        &repo.config().changelog.unreleased_heading,
    )
    .map_err(|source| changelog_error(changelog_path.clone(), source))?;
    let mut imported = import_unreleased_region(repo, &changelog[region.start..region.end])?;
    for fragment in &mut imported.fragments {
        fragment.markdown = format_fragment_source(&fragment.markdown)
            .map_err(|error| with_fragment_path(error, fragment.path.clone()))?;
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

    let next_path = next_version_path(repo);
    let next_before = read_release_file_state(repo, &next_path)?;
    let existing_version = release_next_version(repo, &next_path, &next_before)?;
    if let Some(actual) = &existing_version
        && imported.version.as_ref() != Some(actual)
    {
        return Err(Error::UnreleasedImportNextMismatch {
            expected: imported
                .version
                .clone()
                .unwrap_or_else(|| String::from("Unreleased")),
            actual: actual.clone(),
        });
    }
    let next_after = match (&imported.version, existing_version) {
        (Some(version), None) => ReleaseFileState::Present(format!("{version}\n")),
        _ => next_before.clone(),
    };

    let after_sources = imported
        .fragments
        .iter()
        .map(|fragment| ReleaseFragmentSource {
            path: fragment.path.clone(),
            contents: fragment.markdown.as_bytes().to_vec(),
            section: fragment.section.clone(),
        })
        .collect::<Vec<_>>();
    let parsed_after = parse_release_fragment_sources(repo, &after_sources)?;
    let next_contents = release_file_state_utf8(repo, &next_path, &next_after)?;
    let version_label = version_label_from_contents(&repo.resolve(&next_path), next_contents)?;
    let compiled =
        compile_parsed_fragments(repo, CompileOptions::default(), version_label, parsed_after)?;
    let (normalized_changelog, _) =
        synchronize_materialized_changelog(repo, changelog, &compiled, &changelog_path)?;
    let changelog_after = ReleaseFileState::Present(normalized_changelog.clone());
    let diff =
        (changelog != normalized_changelog).then(|| unified_diff(changelog, &normalized_changelog));

    let written_fragments = imported
        .fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    let mut participants = after_sources
        .iter()
        .map(|source| ReleaseFileChange {
            path: source.path.clone(),
            before: ReleaseFileState::Missing,
            after: release_file_state_from_bytes(source.contents.clone()),
        })
        .collect::<Vec<_>>();
    participants.push(ReleaseFileChange {
        path: next_path,
        before: next_before,
        after: next_after,
    });
    participants.push(ReleaseFileChange {
        path: changelog_path,
        before: changelog_before,
        after: changelog_after,
    });

    Ok(ImportUnreleasedPlan {
        inferred_version: imported.version,
        written_fragments: written_fragments.clone(),
        diff,
        force: options.force,
        mutation: RepositoryMutationPlan {
            command: MutationCommand::ImportUnreleased,
            participants,
            fragment_paths_before: Vec::new(),
            fragment_paths_after: written_fragments,
        },
    })
}

/// Applies a previously planned unreleased import transaction.
///
/// Every generated fragment, the next-version file, and the materialized
/// changelog are committed together. Stale inputs and concurrent fragment
/// additions are rejected before the first write.
pub fn apply_import_unreleased(
    repo: &Repository,
    plan: ImportUnreleasedPlan,
) -> Result<ImportUnreleasedResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let outcome = apply_repository_mutation(repo, &plan.mutation)?;
    Ok(ImportUnreleasedResult {
        written_fragments: plan.written_fragments,
        inferred_version: plan.inferred_version,
        cleanup_warnings: outcome.cleanup_warnings,
    })
}

fn plan_carry_mutation(
    repo: &Repository,
    version: &str,
) -> Result<(RepositoryMutationPlan, Vec<PathBuf>, Option<SyncResult>)> {
    let changelog_path = repo.config().changelog.path.clone();
    let changelog_before = read_release_file_state(repo, &changelog_path)?;
    let changelog =
        release_file_state_utf8(repo, &changelog_path, &changelog_before)?.ok_or_else(|| {
            Error::ReadFile {
                path: repo.resolve(&changelog_path),
                source: std::io::Error::new(ErrorKind::NotFound, "changelog does not exist"),
            }
        })?;
    let mut carried = carry_release(repo, changelog, version)?;
    for fragment in &mut carried.fragments {
        fragment.markdown = format_fragment_source(&fragment.markdown)
            .map_err(|error| with_fragment_path(error, fragment.path.clone()))?;
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
    let written_fragments = carried
        .fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    let before_sources = snapshot_release_fragment_sources(repo)?;
    let parsed_before = repo
        .config()
        .changelog
        .materialize
        .then(|| parse_release_fragment_sources(repo, &before_sources))
        .transpose()?;
    let mut after_sources = before_sources.clone();
    for carried_fragment in &carried.fragments {
        if let Some(existing) = after_sources
            .iter_mut()
            .find(|source| source.path == carried_fragment.path)
        {
            existing.contents = carried_fragment.markdown.as_bytes().to_vec();
            existing.section.clone_from(&carried_fragment.section);
        } else {
            after_sources.push(ReleaseFragmentSource {
                path: carried_fragment.path.clone(),
                contents: carried_fragment.markdown.as_bytes().to_vec(),
                section: carried_fragment.section.clone(),
            });
        }
    }
    after_sources.sort_by(|left, right| compare_fragment_paths(&left.path, &right.path));
    let parsed_after = repo
        .config()
        .changelog
        .materialize
        .then(|| parse_release_fragment_sources(repo, &after_sources))
        .transpose()?;
    if parsed_after.is_none() {
        validate_parseable_fragment_pin_consistency(repo, &after_sources)?;
    }
    let next_path = next_version_path(repo);
    let next_state = read_release_file_state(repo, &next_path)?;
    let mut participants = Vec::with_capacity(after_sources.len() + 2);
    for source in &after_sources {
        let before = before_sources
            .iter()
            .find(|before| before.path == source.path)
            .map_or(ReleaseFileState::Missing, |before| {
                release_file_state_from_bytes(before.contents.clone())
            });
        participants.push(ReleaseFileChange {
            path: source.path.clone(),
            before,
            after: release_file_state_from_bytes(source.contents.clone()),
        });
    }
    participants.push(ReleaseFileChange {
        path: next_path.clone(),
        before: next_state.clone(),
        after: next_state.clone(),
    });

    let changelog_after = if repo.config().changelog.materialize {
        let next_contents = release_file_state_utf8(repo, &next_path, &next_state)?;
        let version_label = version_label_from_contents(&repo.resolve(&next_path), next_contents)?;
        let compiled_before = compile_parsed_fragments(
            repo,
            CompileOptions::default(),
            version_label.clone(),
            parsed_before.expect("materialized carry parses the previous fragment snapshot"),
        )?;
        ensure_materialized_release_snapshot_current(
            repo,
            &changelog_path,
            &changelog_before,
            &compiled_before,
        )?;
        let compiled_after = compile_parsed_fragments(
            repo,
            CompileOptions::default(),
            version_label,
            parsed_after.expect("materialized carry parses the resulting fragment snapshot"),
        )?;
        let (new_contents, _) =
            synchronize_materialized_changelog(repo, changelog, &compiled_after, &changelog_path)?;
        ReleaseFileState::Present(new_contents)
    } else {
        changelog_before.clone()
    };
    let sync = repo.config().changelog.materialize.then(|| SyncResult {
        changed: changelog_after != changelog_before,
    });
    participants.push(ReleaseFileChange {
        path: changelog_path,
        before: changelog_before,
        after: changelog_after,
    });

    let before_paths = before_sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    let after_paths = after_sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    Ok((
        RepositoryMutationPlan {
            command: MutationCommand::Carry,
            participants,
            fragment_paths_before: before_paths,
            fragment_paths_after: after_paths,
        },
        written_fragments,
        sync,
    ))
}

/// Runs the changelog merge driver and writes its result to the current-side file.
///
/// The repository mutation lock is held while fragments are compiled and until
/// the merged output has been written.
pub fn apply_merge_driver(
    repo: &Repository,
    options: MergeDriverOptions,
) -> Result<MergeDriverResult> {
    let _lock = acquire_mutation_lock(repo)?;
    let current = options.current.clone();
    let result = merge_driver(repo, options)?;
    let output = match &result {
        MergeDriverResult::Clean { output, .. } => output,
        MergeDriverResult::Conflict {
            output_with_markers,
            ..
        } => output_with_markers,
    };
    fs::write(&current, output).map_err(|source| Error::WriteFile {
        path: current,
        source,
    })?;
    Ok(result)
}

/// Checks fragments, materialized output, and missing-fragment policy.
pub fn check(repo: &Repository, options: CheckOptions) -> Result<CheckReport> {
    if options.base.is_some() && options.staged {
        return Err(Error::Usage {
            message: String::from("--base and --staged cannot be used together"),
        });
    }
    if options.fix {
        let prepared = plan_format(repo, FormatOptions)?;
        if matches!(&prepared.sync, SyncPlan::NeedsConfirmation { .. }) {
            return Err(Error::SyncNeedsConfirmation {
                path: repo.config().changelog.path.clone(),
            });
        }
        let formatting = apply_format(repo, prepared)?;
        let mut report = check(
            repo,
            CheckOptions {
                fix: false,
                ..options
            },
        )?;
        report.warnings.extend(
            formatting
                .cleanup_warnings
                .into_iter()
                .map(check_fix_cleanup_warning),
        );
        return Ok(report);
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
        // Validate repository-wide resolved-link consistency even when
        // materialization is disabled. Compilation never performs network
        // resolution on the check path.
        compile_unreleased(repo, CompileOptions::default())?;
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

    run_missing_fragment_check(repo, &options, &mut violations, &mut skipped)?;

    Ok(CheckReport {
        violations,
        warnings,
        skipped,
    })
}

/// Arms the final commit check from Git's `commit-msg` hook.
pub fn commit_message_hook(repo: &Repository, _message_path: &Path) -> Result<()> {
    let vcs = GitVcs::configured(repo.root(), &repo.config().vcs);
    let state = CommitHookState {
        head: vcs.head()?,
        tree: vcs.index_tree()?,
    };
    let path = commit_hook_state_path(repo.root())?;
    fs::write(&path, state.encode()).map_err(|source| Error::WriteFile { path, source })
}

/// Checks a commit before Git completes its reference transaction.
pub fn reference_transaction_hook(
    repo: &Repository,
    phase: &str,
    updates: &str,
) -> Result<Option<CheckReport>> {
    if phase != "prepared" {
        return Ok(None);
    }
    let state_path = commit_hook_state_path(repo.root())?;
    let state = match fs::read_to_string(&state_path) {
        Ok(state) => CommitHookState::decode(&state)?,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::ReadFile {
                path: state_path,
                source,
            });
        }
    };
    fs::remove_file(&state_path).map_err(|source| Error::WriteFile {
        path: state_path,
        source,
    })?;
    let vcs = GitVcs::configured(repo.root(), &repo.config().vcs);
    let Some(commit) = reference_transaction_commit(&vcs, &state, updates)? else {
        return Ok(None);
    };
    let missing = commit_missing_fragment_violations(repo, &vcs, &CommitId::new(commit))?;
    Ok(Some(CheckReport {
        violations: missing.violations,
        warnings: Vec::new(),
        skipped: missing.skipped,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitHookState {
    head: Option<String>,
    tree: String,
}

impl CommitHookState {
    fn encode(&self) -> String {
        format!(
            "head {}\ntree {}\n",
            self.head.as_deref().unwrap_or("-"),
            self.tree
        )
    }

    fn decode(value: &str) -> Result<Self> {
        let mut lines = value.lines();
        let head = lines
            .next()
            .and_then(|line| line.strip_prefix("head "))
            .ok_or_else(|| Error::Usage {
                message: String::from("invalid Sacho commit-hook state"),
            })?;
        let tree = lines
            .next()
            .and_then(|line| line.strip_prefix("tree "))
            .filter(|tree| !tree.is_empty())
            .ok_or_else(|| Error::Usage {
                message: String::from("invalid Sacho commit-hook state"),
            })?;
        if lines.next().is_some() {
            return Err(Error::Usage {
                message: String::from("invalid Sacho commit-hook state"),
            });
        }
        Ok(Self {
            head: (head != "-").then(|| head.to_owned()),
            tree: tree.to_owned(),
        })
    }
}

fn reference_transaction_commit(
    vcs: &GitVcs,
    state: &CommitHookState,
    updates: &str,
) -> Result<Option<String>> {
    for line in updates.lines() {
        let mut fields = line.split_whitespace();
        let (Some(old), Some(new), Some(reference), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !matches_commit_head(old, state.head.as_deref())
            || !(reference == "HEAD" || reference.starts_with("refs/heads/"))
        {
            continue;
        }
        if vcs.commit_tree(new).is_ok_and(|tree| tree == state.tree) {
            return Ok(Some(new.to_owned()));
        }
    }
    Ok(None)
}

fn matches_commit_head(old: &str, head: Option<&str>) -> bool {
    match head {
        Some(head) => old == head,
        None => !old.is_empty() && old.bytes().all(|byte| byte == b'0'),
    }
}

fn commit_hook_state_path(root: &Path) -> Result<PathBuf> {
    let path = git_output(root, ["rev-parse", "--git-path", "sacho-commit-state"])?;
    Ok(resolve_git_path(root, path.trim()))
}

fn run_missing_fragment_check(
    repo: &Repository,
    options: &CheckOptions,
    violations: &mut Vec<CheckViolation>,
    skipped: &mut Vec<SkippedCheck>,
) -> Result<()> {
    if options.base.is_none() && !options.staged {
        skipped.push(SkippedCheck {
            message: String::from(
                "missing-fragment check skipped because neither --base nor --staged was supplied",
            ),
        });
        return Ok(());
    }
    if repo.config().check.paths.is_empty() {
        skipped.push(no_checked_paths_skip());
        return Ok(());
    }
    match repo.config().vcs.preset {
        VcsPreset::Git => {
            let vcs = GitVcs::configured(repo.root(), &repo.config().vcs);
            let report = if options.staged {
                staged_missing_fragment_violations(repo, &vcs)?
            } else {
                missing_fragment_violations(
                    repo,
                    &vcs,
                    options.base.as_deref().expect("base mode has a revision"),
                )?
            };
            violations.extend(report.violations);
            skipped.extend(report.skipped);
        }
        VcsPreset::Jj => {
            if options.staged {
                return Err(Error::Usage {
                    message: String::from("--staged is supported only with vcs.preset = \"git\""),
                });
            }
            let vcs = JjVcs::with_fragment_directory(
                repo.root(),
                &repo.config().vcs,
                &repo.config().fragments.directory,
            );
            let report = missing_fragment_violations(
                repo,
                &vcs,
                options.base.as_deref().expect("base mode has a revision"),
            )?;
            violations.extend(report.violations);
            skipped.extend(report.skipped);
        }
        VcsPreset::Hg => {
            if options.staged {
                return Err(Error::Usage {
                    message: String::from("--staged is supported only with vcs.preset = \"git\""),
                });
            }
            let section_patterns = repo
                .config()
                .section_patterns
                .iter()
                .map(|pattern| pattern.directory.clone())
                .collect::<Vec<_>>();
            let vcs = HgVcs::with_patterned_fragment_layout(
                repo.root(),
                &repo.config().vcs,
                &repo.config().fragments.directory,
                repo.config()
                    .sections
                    .iter()
                    .map(|section| &section.directory),
                &section_patterns,
            );
            let report = missing_fragment_violations(
                repo,
                &vcs,
                options.base.as_deref().expect("base mode has a revision"),
            )?;
            violations.extend(report.violations);
            skipped.extend(report.skipped);
        }
        VcsPreset::None => skipped.push(SkippedCheck {
            message: String::from("missing-fragment check skipped because vcs.preset = \"none\""),
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
        let report = missing_fragment_violations_for_changes(
            repo,
            commit.as_str(),
            &changed_paths,
            &final_fragments,
            &source_patterns,
            MissingFragmentMode::Commit,
        )?;
        violations.extend(report.violations);
        skipped.extend(report.skipped);
    }

    Ok(MissingFragmentReport {
        violations,
        skipped,
    })
}

fn staged_missing_fragment_violations(
    repo: &Repository,
    vcs: &GitVcs,
) -> Result<MissingFragmentReport> {
    if repo.config().check.paths.is_empty() {
        return Ok(MissingFragmentReport {
            violations: Vec::new(),
            skipped: vec![no_checked_paths_skip()],
        });
    }
    let source_patterns = compile_glob_set(&repo.config().check.paths)?;
    let changed_paths = vcs.staged_paths()?;
    let staged_fragments = surviving_fragment_paths(repo, &changed_paths)?;

    missing_fragment_violations_for_changes(
        repo,
        "staged changes",
        &changed_paths,
        &staged_fragments,
        &source_patterns,
        MissingFragmentMode::Staged,
    )
}

fn commit_missing_fragment_violations(
    repo: &Repository,
    vcs: &impl Vcs,
    commit: &CommitId,
) -> Result<MissingFragmentReport> {
    if repo.config().check.paths.is_empty() {
        return Ok(MissingFragmentReport {
            violations: Vec::new(),
            skipped: vec![no_checked_paths_skip()],
        });
    }
    if message_exempts_changelog(&vcs.message(commit)?) {
        return Ok(MissingFragmentReport::default());
    }
    let source_patterns = compile_glob_set(&repo.config().check.paths)?;
    let changed_paths = vcs.changed_paths(commit)?;
    let surviving_fragments = surviving_fragment_paths(repo, &changed_paths)?;
    missing_fragment_violations_for_changes(
        repo,
        commit.as_str(),
        &changed_paths,
        &surviving_fragments,
        &source_patterns,
        MissingFragmentMode::Commit,
    )
}

fn missing_fragment_violations_for_changes(
    repo: &Repository,
    subject: &str,
    changed_paths: &[ChangedPath],
    final_fragments: &IndexSet<PathBuf>,
    source_patterns: &GlobSet,
    mode: MissingFragmentMode,
) -> Result<MissingFragmentReport> {
    let mut relevant_paths = changed_paths
        .iter()
        .flat_map(policy_paths)
        .filter(|path| source_patterns.is_match(path))
        .cloned()
        .collect::<Vec<_>>();
    relevant_paths.sort();
    relevant_paths.dedup();
    if relevant_paths.is_empty() {
        return Ok(MissingFragmentReport::default());
    }
    let changed_fragments = changed_fragment_changes(repo, changed_paths)?;
    let requirements = missing_fragment_requirements(repo, &relevant_paths)?;
    let violations = requirements
        .requirements
        .iter()
        .filter(|requirement| {
            !requirement_satisfied(requirement, &changed_fragments, final_fragments)
        })
        .map(|requirement| CheckViolation {
            message: missing_fragment_message(subject, requirement, mode),
        })
        .collect();
    Ok(MissingFragmentReport {
        violations,
        skipped: requirements.skipped,
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

fn changed_fragment_changes(
    repo: &Repository,
    paths: &[ChangedPath],
) -> Result<Vec<FragmentChange>> {
    let resolver = SectionResolver::from_config(repo.config())?;
    paths
        .iter()
        .filter(|path| fragment_content_changed(path))
        .map(|path| fragment_change_for_path(repo, &resolver, &path.path))
        .filter_map(Result::transpose)
        .collect()
}

fn surviving_fragment_paths(repo: &Repository, paths: &[ChangedPath]) -> Result<IndexSet<PathBuf>> {
    let resolver = SectionResolver::from_config(repo.config())?;
    paths
        .iter()
        .filter(|path| path.kind.path_survives())
        .map(|path| {
            fragment_target_for_path(repo, &resolver, &path.path)
                .map(|target| target.map(|_| path.path.clone()))
        })
        .filter_map(Result::transpose)
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
    std::iter::once(&path.path).chain(&path.rename_origins)
}

fn final_fragment_paths(repo: &Repository) -> Result<IndexSet<PathBuf>> {
    Ok(discover_fragment_candidates(repo)?
        .candidates
        .iter()
        .map(|candidate| candidate.relative_path.clone())
        .collect())
}

fn fragment_change_for_path(
    repo: &Repository,
    resolver: &SectionResolver<'_>,
    path: &Path,
) -> Result<Option<FragmentChange>> {
    Ok(
        fragment_target_for_path(repo, resolver, path)?.map(|target| FragmentChange {
            path: path.to_path_buf(),
            target,
        }),
    )
}

fn fragment_target_for_path(
    repo: &Repository,
    resolver: &SectionResolver<'_>,
    path: &Path,
) -> Result<Option<FragmentTarget>> {
    let config = repo.config();
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return Ok(None);
    }
    if !path.starts_with(&config.fragments.directory) {
        return Ok(None);
    }
    if !has_sections(config) {
        if path.parent() != Some(config.fragments.directory.as_path()) {
            return Ok(None);
        }
        return Ok(Some(FragmentTarget::Repository));
    }
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let Ok(relative) = parent.strip_prefix(&config.fragments.directory) else {
        return Ok(None);
    };
    if let Some(section) =
        resolver
            .resolve_directory(relative)
            .map_err(|error| Error::SectionPattern {
                message: error.to_string(),
            })?
    {
        return Ok(Some(FragmentTarget::Section(section.id)));
    }
    if path.parent().and_then(Path::parent) == Some(config.fragments.directory.as_path()) {
        return Ok(Some(FragmentTarget::Repository));
    }
    Ok(None)
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
    paths: &[PathBuf],
) -> Result<MissingFragmentRequirements> {
    if !has_sections(repo.config()) {
        return Ok(MissingFragmentRequirements {
            requirements: vec![MissingFragmentRequirement::repository(
                paths.to_vec(),
                false,
            )],
            skipped: Vec::new(),
        });
    }

    let resolver = SectionResolver::from_config(repo.config())?;
    let mut repository_paths = Vec::new();
    let mut section_paths = BTreeMap::<String, Vec<PathBuf>>::new();
    for path in paths {
        let matched_sections =
            resolver
                .resolve_source_path(path)
                .map_err(|error| Error::SectionPattern {
                    message: error.to_string(),
                })?;
        if matched_sections.is_empty() {
            repository_paths.push(path.clone());
        } else {
            for section in matched_sections {
                if section.pattern_index.is_some() && !section_paths.contains_key(&section.id) {
                    repo.validate_pattern_section_directory(&section.directory)?;
                }
                section_paths
                    .entry(section.id)
                    .or_default()
                    .push(path.clone());
            }
        }
    }

    let present = section_paths.keys().cloned().collect::<BTreeSet<_>>();
    let ordered = resolver
        .ordered_present(&present)
        .map_err(|error| Error::SectionPattern {
            message: error.to_string(),
        })?;
    let mut requirements = ordered
        .into_iter()
        .map(|section| MissingFragmentRequirement {
            paths: section_paths
                .remove(&section)
                .expect("ordered section came from grouped paths"),
            target: FragmentTarget::Section(section),
            sectioned_repository: false,
        })
        .collect::<Vec<_>>();
    if !repository_paths.is_empty() {
        requirements.push(MissingFragmentRequirement::repository(
            repository_paths,
            true,
        ));
    }
    Ok(MissingFragmentRequirements {
        requirements,
        skipped: Vec::new(),
    })
}

fn missing_fragment_message(
    subject: &str,
    requirement: &MissingFragmentRequirement,
    mode: MissingFragmentMode,
) -> String {
    let paths = path_sample(&requirement.paths);
    let scope = match &requirement.target {
        FragmentTarget::Repository => String::from("repository"),
        FragmentTarget::Section(section) => format!("section {section:?}"),
    };
    let remedy = match mode {
        MissingFragmentMode::Commit => format!(
            "run `{}` or add `Changelog: none` to the commit message",
            requirement.suggested_command()
        ),
        MissingFragmentMode::Staged => format!(
            "run `{}` and stage the fragment; commit-message escape hatches apply in the installed commit hook and in `sacho check --base`",
            requirement.suggested_command()
        ),
    };
    format!("{subject}: missing changelog fragment for {scope}; affected paths: {paths}; {remedy}")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissingFragmentMode {
    Commit,
    Staged,
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

fn read_release_file_state(repo: &Repository, path: &Path) -> Result<ReleaseFileState> {
    let absolute = repo.resolve(path);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(source) if error_has_kind(&source, ErrorKind::NotFound) => {
            return Ok(ReleaseFileState::Missing);
        }
        Err(source) => {
            return Err(Error::ReadFile {
                path: absolute,
                source,
            });
        }
    };
    if !metadata.file_type().is_file() {
        return Err(Error::ReleasePathConflict { path: absolute });
    }
    fs::read(&absolute)
        .map(release_file_state_from_bytes)
        .map_err(|source| Error::ReadFile {
            path: absolute,
            source,
        })
}

fn release_file_state_from_bytes(contents: Vec<u8>) -> ReleaseFileState {
    match String::from_utf8(contents) {
        Ok(contents) => ReleaseFileState::Present(contents),
        Err(error) => ReleaseFileState::Raw(error.into_bytes()),
    }
}

fn release_file_state_utf8<'a>(
    repo: &Repository,
    path: &Path,
    state: &'a ReleaseFileState,
) -> Result<Option<&'a str>> {
    match state {
        ReleaseFileState::Missing => Ok(None),
        ReleaseFileState::Present(contents) => Ok(Some(contents)),
        ReleaseFileState::Raw(_) => Err(Error::ReadFile {
            path: repo.resolve(path),
            source: std::io::Error::new(ErrorKind::InvalidData, "file is not valid UTF-8"),
        }),
    }
}

fn release_next_version(
    repo: &Repository,
    path: &Path,
    state: &ReleaseFileState,
) -> Result<Option<String>> {
    let Some(contents) = release_file_state_utf8(repo, path, state)? else {
        return Ok(None);
    };
    let values = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some((*value).to_owned())),
        _ => Err(Error::InvalidNextVersion {
            path: repo.resolve(path),
        }),
    }
}

fn snapshot_release_fragments(repo: &Repository) -> Result<(Vec<ReleaseFragment>, Vec<Fragment>)> {
    let sources = snapshot_release_fragment_sources(repo)?;
    let parsed = parse_release_fragment_sources(repo, &sources)?;
    let snapshots = sources
        .into_iter()
        .map(|source| {
            let contents = String::from_utf8(source.contents).map_err(|error| Error::ReadFile {
                path: repo.resolve(&source.path),
                source: std::io::Error::new(ErrorKind::InvalidData, error),
            })?;
            Ok(ReleaseFragment {
                path: source.path,
                contents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((snapshots, parsed))
}

fn snapshot_release_fragment_sources(repo: &Repository) -> Result<Vec<ReleaseFragmentSource>> {
    let candidates = discover_fragment_candidates(repo)?.candidates;
    let mut sources = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let state = read_release_file_state(repo, &candidate.relative_path)?;
        let contents = match state {
            ReleaseFileState::Present(contents) => contents.into_bytes(),
            ReleaseFileState::Raw(contents) => contents,
            ReleaseFileState::Missing => {
                return Err(Error::StaleReleasePlan {
                    path: candidate.relative_path,
                });
            }
        };
        sources.push(ReleaseFragmentSource {
            path: candidate.relative_path,
            contents,
            section: candidate.section,
        });
    }
    Ok(sources)
}

fn parse_release_fragment_sources(
    repo: &Repository,
    sources: &[ReleaseFragmentSource],
) -> Result<Vec<Fragment>> {
    sources
        .iter()
        .map(|fragment_source| {
            let contents = std::str::from_utf8(&fragment_source.contents).map_err(|error| {
                Error::ReadFile {
                    path: repo.resolve(&fragment_source.path),
                    source: std::io::Error::new(ErrorKind::InvalidData, error),
                }
            })?;
            parse_fragment(
                fragment_source.path.clone(),
                contents,
                fragment_source.section.clone(),
                &repo.config().links,
            )
            .map_err(|source| Error::Fragment {
                path: fragment_source.path.clone(),
                source,
            })
        })
        .collect()
}

fn validate_parseable_fragment_pin_consistency(
    repo: &Repository,
    sources: &[ReleaseFragmentSource],
) -> Result<()> {
    let fragments = sources
        .iter()
        .filter_map(|source| {
            let contents = std::str::from_utf8(&source.contents).ok()?;
            parse_fragment(
                source.path.clone(),
                contents,
                source.section.clone(),
                &repo.config().links,
            )
            .ok()
        })
        .collect::<Vec<_>>();
    validate_resolved_link_consistency(&fragments)
}

struct MutationLock {
    file: fs::File,
    path: PathBuf,
}

impl Drop for MutationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_mutation_lock(repo: &Repository) -> Result<MutationLock> {
    let path = mutation_lock_path(repo.root());
    validate_configured_mutation_paths_at_root(repo.root(), repo.config(), &path)?;
    let lock = acquire_mutation_lock_at_path(path)?;
    validate_configured_mutation_paths(repo, &lock.path)?;
    Ok(lock)
}

#[cfg(test)]
fn acquire_mutation_lock_at_root(root: &Path) -> Result<MutationLock> {
    acquire_mutation_lock_at_path(mutation_lock_path(root))
}

fn acquire_mutation_lock_at_path(path: PathBuf) -> Result<MutationLock> {
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == ErrorKind::NotFound => {
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(source) if mutation_lock_was_created_concurrently(&source) => {
                    fs::File::open(&path).map_err(|source| Error::ReadFile {
                        path: path.clone(),
                        source,
                    })?
                }
                Err(source) => {
                    return Err(Error::WriteFile {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        Err(source) => {
            return Err(Error::ReadFile {
                path: path.clone(),
                source,
            });
        }
    };
    file.try_lock().map_err(|source| {
        let source: std::io::Error = source.into();
        if source.kind() == ErrorKind::WouldBlock {
            Error::ReleaseLocked
        } else {
            Error::ReadFile {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    Ok(MutationLock { file, path })
}

fn validate_configured_mutation_paths(repo: &Repository, lock_path: &Path) -> Result<()> {
    validate_configured_mutation_paths_at_root(repo.root(), repo.config(), lock_path)
}

fn validate_configured_mutation_paths_at_root(
    root: &Path,
    config: &Config,
    lock_path: &Path,
) -> Result<()> {
    let mut validation_cache = PathValidationCache::default();
    let configured =
        validate_repository_config_paths_with_cache(root, config, &mut validation_cache).map_err(
            |source| Error::Config {
                path: root.join(Repository::CONFIG_FILE),
                source: Box::new(source),
            },
        )?;
    validate_configured_paths_against_reserved_with_cache(
        &configured,
        "repository mutation lock",
        lock_path,
        &mut validation_cache,
    )
    .map_err(|source| Error::Config {
        path: root.join(Repository::CONFIG_FILE),
        source: Box::new(source),
    })
}

fn parse_git_root_output(output: &str) -> Option<PathBuf> {
    let path = output.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn mutation_lock_was_created_concurrently(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::AlreadyExists
}

fn validate_distinct_release_paths(repo: &Repository, plan: &ReleasePlan) -> Result<Vec<PathBuf>> {
    let paths = std::iter::once(&plan.changelog.path)
        .chain(std::iter::once(&plan.next_file.path))
        .chain(
            plan.consumed_fragments
                .iter()
                .map(|fragment| &fragment.path),
        );
    let mut identities = paths
        .map(|path| Ok((path.clone(), release_path_identity(repo, path)?)))
        .collect::<Result<Vec<_>>>()?;
    let identity_paths = identities
        .iter()
        .map(|(_, identity)| identity.clone())
        .collect::<Vec<_>>();
    let mut case_sensitivity = Vec::<(PathBuf, bool)>::new();
    let sensitivities = identity_paths
        .iter()
        .map(|identity| {
            release_path_case_sensitivity(identity, &identity_paths, &mut case_sensitivity)
        })
        .collect::<Result<Vec<_>>>()?;
    for index in 0..identities.len() {
        let (path, identity) = &identities[index];
        if let Some((other, _)) = identities[..index]
            .iter()
            .enumerate()
            .find(|(other_index, (_, other_identity))| {
                release_paths_overlap(
                    identity,
                    other_identity,
                    release_path_pair_case_sensitive(
                        sensitivities[index],
                        sensitivities[*other_index],
                    ),
                )
            })
            .map(|(_, identity)| identity)
        {
            return Err(Error::ReleasePathOverlap {
                first: other.clone(),
                second: path.clone(),
            });
        }
    }
    Ok(identities.drain(..).map(|(_, identity)| identity).collect())
}

fn release_path_case_sensitivity(
    identity: &Path,
    participant_identities: &[PathBuf],
    cache: &mut Vec<(PathBuf, bool)>,
) -> Result<bool> {
    let directory = nearest_existing_directory(identity).ok_or_else(|| Error::ReadFile {
        path: identity.to_path_buf(),
        source: std::io::Error::new(
            ErrorKind::NotFound,
            "release path has no existing directory ancestor",
        ),
    })?;
    if let Some((_, case_sensitive)) = cache.iter().find(|(cached, _)| *cached == directory) {
        return Ok(*case_sensitive);
    }
    let case_sensitive = probe_directory_case_sensitivity(&directory, participant_identities)?;
    cache.push((directory, case_sensitive));
    Ok(case_sensitive)
}

fn release_path_pair_case_sensitive(first: bool, second: bool) -> bool {
    first && second
}

fn probe_directory_case_sensitivity(
    parent: &Path,
    participant_identities: &[PathBuf],
) -> Result<bool> {
    let directory = reserve_release_probe_directory(parent, participant_identities)?;
    let mixed_case = directory.join("Case-Sensitivity-Aa");
    let alternate_case = directory.join("case-sensitivity-aA");
    if let Err(source) = fs::create_dir(&mixed_case) {
        let _ = fs::remove_dir(&directory);
        return Err(Error::CreateDirectory {
            path: mixed_case,
            source,
        });
    }
    let case_sensitive = match fs::symlink_metadata(&alternate_case) {
        Ok(_) => false,
        Err(source) if source.kind() == ErrorKind::NotFound => true,
        Err(source) => {
            let _ = fs::remove_dir(&mixed_case);
            let _ = fs::remove_dir(&directory);
            return Err(Error::ReadFile {
                path: alternate_case,
                source,
            });
        }
    };
    fs::remove_dir(&mixed_case).map_err(|source| Error::RemoveDirectory {
        path: mixed_case,
        source,
    })?;
    fs::remove_dir(&directory).map_err(|source| Error::RemoveDirectory {
        path: directory,
        source,
    })?;
    Ok(case_sensitive)
}

fn release_paths_overlap(first: &Path, second: &Path, case_sensitive: bool) -> bool {
    filesystem_paths_overlap(first, second, case_sensitive)
}

fn release_path_identity(repo: &Repository, path: &Path) -> Result<PathBuf> {
    filesystem_path_identity(&repo.resolve(path))
}

fn filesystem_path_identity(path: &Path) -> Result<PathBuf> {
    repo_path_identity(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn probe_release_move_support(
    repo: &Repository,
    plan: &ReleasePlan,
    participant_identities: &[PathBuf],
) -> Result<()> {
    let paths = std::iter::once(plan.changelog.path.as_path())
        .chain(std::iter::once(plan.next_file.path.as_path()))
        .chain(
            plan.consumed_fragments
                .iter()
                .map(|fragment| fragment.path.as_path()),
        )
        .collect::<Vec<_>>();
    probe_transaction_move_support(repo, &paths, participant_identities)
}

fn probe_transaction_move_support(
    repo: &Repository,
    paths: &[&Path],
    participant_identities: &[PathBuf],
) -> Result<()> {
    debug_assert_eq!(paths.len(), participant_identities.len());
    let mut probed_directories = IndexSet::new();
    for (path, identity) in paths.iter().copied().zip(participant_identities) {
        let directory = nearest_existing_directory(identity).ok_or_else(|| Error::ReadFile {
            path: identity.clone(),
            source: std::io::Error::new(
                ErrorKind::NotFound,
                "release path has no existing directory ancestor",
            ),
        })?;
        if !probed_directories.insert(directory.clone()) {
            continue;
        }
        probe_directory_move(repo, path, &directory, participant_identities)?;
    }
    Ok(())
}

fn nearest_existing_directory(identity: &Path) -> Option<PathBuf> {
    let mut candidate = identity.parent();
    while let Some(path) = candidate {
        if let Ok(canonical) = fs::canonicalize(path)
            && canonical.is_dir()
        {
            return Some(canonical);
        }
        candidate = path.parent();
    }
    None
}

fn probe_directory_move(
    repo: &Repository,
    path: &Path,
    parent: &Path,
    participant_identities: &[PathBuf],
) -> Result<()> {
    let directory = reserve_release_probe_directory(parent, participant_identities)?;
    let source = directory.join("source");
    let destination = directory.join("destination");
    if let Err(source_error) = fs::create_dir(&source) {
        let _ = fs::remove_dir(&directory);
        return Err(Error::CreateDirectory {
            path: source,
            source: source_error,
        });
    }
    let probe_result = release_failpoint(ReleaseApplyStage::Probe, path, repo).and_then(|()| {
        move_path_if_absent(&source, &destination).map_err(|_| Error::ReleaseTransactionUnsupported)
    });
    let moved = destination.exists();
    let cleanup_path = if moved { &destination } else { &source };
    let cleanup_result = fs::remove_dir(cleanup_path)
        .and_then(|()| fs::remove_dir(&directory))
        .map_err(|source| Error::RemoveDirectory {
            path: cleanup_path.to_path_buf(),
            source,
        });
    match (probe_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(_), Ok(())) => Err(Error::ReleaseTransactionUnsupported),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(release_apply_error(error, vec![cleanup.to_string()])),
    }
}

fn reserve_release_probe_directory(
    parent: &Path,
    participant_identities: &[PathBuf],
) -> Result<PathBuf> {
    loop {
        let sequence = RELEASE_CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(".sacho-probe-{}-{sequence}", std::process::id());
        let directory = parent.join(&name);
        if release_claim_path_conflicts(&directory, participant_identities) {
            continue;
        }
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(source) if error_has_kind(&source, ErrorKind::AlreadyExists) => continue,
            Err(source) => {
                return Err(Error::CreateDirectory {
                    path: directory,
                    source,
                });
            }
        }
    }
}

fn validate_release_plan(repo: &Repository, plan: &ReleasePlan) -> Result<()> {
    validate_release_state(repo, &plan.changelog.path, &plan.changelog.before)?;
    validate_release_state(repo, &plan.next_file.path, &plan.next_file.before)?;
    for fragment in &plan.consumed_fragments {
        validate_release_state(
            repo,
            &fragment.path,
            &ReleaseFileState::Present(fragment.contents.clone()),
        )?;
    }
    let planned_paths = plan
        .consumed_fragments
        .iter()
        .map(|fragment| fragment.path.clone())
        .collect::<Vec<_>>();
    validate_release_fragment_paths(repo, &planned_paths)
}

fn validate_release_fragment_paths(repo: &Repository, expected: &[PathBuf]) -> Result<()> {
    let current_paths = discover_fragment_candidates(repo)?
        .candidates
        .into_iter()
        .map(|candidate| candidate.relative_path)
        .collect::<Vec<_>>();
    if let Some(path) = differing_fragment_path(expected, &current_paths) {
        return Err(Error::StaleReleasePlan { path });
    }
    Ok(())
}

fn differing_fragment_path(planned: &[PathBuf], current: &[PathBuf]) -> Option<PathBuf> {
    if planned == current {
        return None;
    }
    current
        .iter()
        .find(|path| !planned.contains(path))
        .or_else(|| planned.iter().find(|path| !current.contains(path)))
        .or_else(|| {
            planned
                .iter()
                .zip(current)
                .find_map(|(planned, current)| (planned != current).then_some(current))
        })
        .cloned()
}

fn release_state_from_optional_contents(contents: Option<String>) -> ReleaseFileState {
    contents.map_or(ReleaseFileState::Missing, ReleaseFileState::Present)
}

fn apply_repository_mutation(
    repo: &Repository,
    plan: &RepositoryMutationPlan,
) -> Result<RepositoryMutationOutcome> {
    validate_repository_mutation(repo, plan)?;
    let participant_identities = validate_distinct_mutation_paths(repo, plan)?;
    let changed = plan
        .participants
        .iter()
        .enumerate()
        .filter(|(_, participant)| participant.before != participant.after)
        .collect::<Vec<_>>();
    let changed_paths = changed
        .iter()
        .map(|(_, participant)| participant.path.as_path())
        .collect::<Vec<_>>();
    let changed_identities = changed
        .iter()
        .map(|(index, _)| participant_identities[*index].clone())
        .collect::<Vec<_>>();
    probe_transaction_move_support(repo, &changed_paths, &changed_identities)
        .map_err(|error| mutation_transaction_error(plan.command, error))?;
    let mut prepared = VecDeque::with_capacity(changed.len());
    for (_, change) in &changed {
        match prepare_release_change(repo, change, &participant_identities) {
            Ok(write) => prepared.push_back(write),
            Err(error) => {
                discard_prepared_mutation_writes(&mut prepared);
                return Err(mutation_transaction_error(plan.command, error));
            }
        }
    }
    let mut applied = Vec::with_capacity(changed.len());
    for (_, change) in changed {
        let mut write = prepared
            .pop_front()
            .expect("every changed mutation participant has a prepared write");
        let created_directories = write
            .as_mut()
            .map(PreparedAtomicWrite::take_created_directories)
            .unwrap_or_default();
        let mut outcome = match apply_release_change(repo, change, write, &participant_identities) {
            Ok(outcome) => outcome,
            Err(error) => {
                discard_prepared_mutation_writes(&mut prepared);
                let cleanup_failures = remove_release_created_directories(created_directories)
                    .err()
                    .map(|error| mutation_transaction_error(plan.command, error).to_string())
                    .into_iter()
                    .collect();
                let error = rollback_repository_mutation(repo, plan.command, error, applied);
                return Err(mutation_apply_error(plan.command, error, cleanup_failures));
            }
        };
        outcome.created_directories.extend(created_directories);
        applied.push(AppliedReleaseChange {
            path: change.path.clone(),
            before: change.before.clone(),
            after: change.after.clone(),
            original: outcome.original,
            created_directories: outcome.created_directories,
        });
    }
    let cleanup_warnings = match commit_repository_mutation(repo, plan, &mut applied) {
        Ok(warnings) => warnings,
        Err(error) => {
            return Err(rollback_repository_mutation(
                repo,
                plan.command,
                error,
                applied,
            ));
        }
    };
    Ok(RepositoryMutationOutcome { cleanup_warnings })
}

fn discard_prepared_mutation_writes(prepared: &mut VecDeque<Option<PreparedAtomicWrite>>) {
    while let Some(write) = prepared.pop_back() {
        drop(write);
    }
}

fn validate_repository_mutation(repo: &Repository, plan: &RepositoryMutationPlan) -> Result<()> {
    for participant in &plan.participants {
        validate_mutation_state(repo, plan.command, &participant.path, &participant.before)?;
    }
    validate_mutation_fragment_paths(repo, plan.command, &plan.fragment_paths_before)
}

fn validate_mutation_state(
    repo: &Repository,
    command: MutationCommand,
    path: &Path,
    expected: &ReleaseFileState,
) -> Result<()> {
    match read_release_file_state(repo, path) {
        Ok(current) if current == *expected => Ok(()),
        Ok(_) => Err(Error::StaleMutationPlan {
            command,
            path: path.to_path_buf(),
        }),
        Err(error) if mutation_state_error_is_stale(&error) => Err(Error::StaleMutationPlan {
            command,
            path: path.to_path_buf(),
        }),
        Err(error) => Err(error),
    }
}

fn mutation_state_error_is_stale(error: &Error) -> bool {
    match error {
        Error::ReleasePathConflict { .. } => true,
        Error::ReadFile { source, .. } => [
            ErrorKind::InvalidData,
            ErrorKind::NotFound,
            ErrorKind::IsADirectory,
        ]
        .into_iter()
        .any(|kind| error_has_kind(source, kind)),
        _ => false,
    }
}

fn validate_mutation_fragment_paths(
    repo: &Repository,
    command: MutationCommand,
    expected: &[PathBuf],
) -> Result<()> {
    let current = discover_fragment_candidates(repo)?
        .candidates
        .into_iter()
        .map(|candidate| candidate.relative_path)
        .collect::<Vec<_>>();
    if let Some(path) = differing_fragment_path(expected, &current) {
        Err(Error::StaleMutationPlan { command, path })
    } else {
        Ok(())
    }
}

fn validate_distinct_mutation_paths(
    repo: &Repository,
    plan: &RepositoryMutationPlan,
) -> Result<Vec<PathBuf>> {
    let mut identities = plan
        .participants
        .iter()
        .map(|participant| {
            Ok((
                participant.path.clone(),
                release_path_identity(repo, &participant.path)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let identity_paths = identities
        .iter()
        .map(|(_, identity)| identity.clone())
        .collect::<Vec<_>>();
    let mut writable_case_sensitivity = Vec::<(PathBuf, bool)>::new();
    let mut sensitivities = vec![None; identity_paths.len()];
    for (index, identity) in identity_paths.iter().enumerate() {
        if plan.participants[index].before != plan.participants[index].after {
            sensitivities[index] = Some(release_path_case_sensitivity(
                identity,
                &identity_paths,
                &mut writable_case_sensitivity,
            )?);
        }
    }
    let mut read_only_case_sensitivity = PathValidationCache::default();
    for (index, identity) in identity_paths.iter().enumerate() {
        if sensitivities[index].is_none() {
            sensitivities[index] = Some(
                read_only_case_sensitivity
                    .filesystem_path_case_sensitive(identity)
                    .map_err(|source| Error::ReadFile {
                        path: identity.clone(),
                        source,
                    })?,
            );
        }
    }
    let sensitivities = sensitivities
        .into_iter()
        .map(|case_sensitive| {
            case_sensitive.expect("every mutation participant has a case-sensitivity result")
        })
        .collect::<Vec<_>>();
    for index in 0..identities.len() {
        let (path, identity) = &identities[index];
        if let Some((other, _)) = identities[..index]
            .iter()
            .enumerate()
            .find(|(other_index, (_, other_identity))| {
                release_paths_overlap(
                    identity,
                    other_identity,
                    release_path_pair_case_sensitive(
                        sensitivities[index],
                        sensitivities[*other_index],
                    ),
                )
            })
            .map(|(_, identity)| identity)
        {
            return Err(Error::MutationPathOverlap {
                command: plan.command,
                first: other.clone(),
                second: path.clone(),
            });
        }
    }
    Ok(identities.drain(..).map(|(_, identity)| identity).collect())
}

fn commit_repository_mutation(
    repo: &Repository,
    plan: &RepositoryMutationPlan,
    applied: &mut [AppliedReleaseChange],
) -> Result<Vec<MutationCleanupWarning>> {
    for change in applied.iter() {
        if let Some(original) = &change.original {
            original
                .preflight_discard(repo, &change.path)
                .map_err(|error| mutation_transaction_error(plan.command, error))?;
        }
    }
    for change in applied.iter() {
        if let Some(original) = &change.original {
            original
                .validate_retained_state(&change.path, &change.before)
                .map_err(|error| mutation_transaction_error(plan.command, error))?;
        }
    }
    for participant in &plan.participants {
        validate_mutation_state(repo, plan.command, &participant.path, &participant.after)?;
    }
    validate_mutation_fragment_paths(repo, plan.command, &plan.fragment_paths_after)?;

    let mut warnings = Vec::new();
    for change in applied.iter_mut() {
        let Some(original) = change.original.take() else {
            continue;
        };
        if let Err(failure) = original.discard_after_commit(repo, &change.path) {
            warnings.push(MutationCleanupWarning {
                path: change.path.clone(),
                message: claim_discard_error(failure).to_string(),
            });
        }
    }
    Ok(warnings)
}

fn rollback_repository_mutation(
    repo: &Repository,
    command: MutationCommand,
    error: Error,
    applied: Vec<AppliedReleaseChange>,
) -> Error {
    let error = mutation_transaction_error(command, error);
    let rollback_failures = applied
        .into_iter()
        .rev()
        .filter_map(|change| {
            rollback_applied_release_change(repo, change)
                .err()
                .map(|error| mutation_transaction_error(command, error).to_string())
        })
        .collect::<Vec<_>>();
    mutation_apply_error(command, error, rollback_failures)
}

fn mutation_apply_error(
    command: MutationCommand,
    error: Error,
    rollback_failures: Vec<String>,
) -> Error {
    match error {
        Error::MutationApply {
            command: _,
            cause,
            rollback_failures: mut existing_failures,
        } => {
            existing_failures.extend(rollback_failures);
            Error::MutationApply {
                command,
                cause,
                rollback_failures: existing_failures,
            }
        }
        error => Error::MutationApply {
            command,
            cause: error.to_string(),
            rollback_failures,
        },
    }
}

fn mutation_transaction_error(command: MutationCommand, error: Error) -> Error {
    match error {
        Error::StaleReleasePlan { path } | Error::ReleasePathConflict { path } => {
            Error::StaleMutationPlan { command, path }
        }
        Error::ReleaseRollbackConflict { path } => {
            Error::MutationRollbackConflict { command, path }
        }
        Error::ReleaseApply {
            cause,
            rollback_failures,
        } => Error::MutationApply {
            command,
            cause,
            rollback_failures,
        },
        Error::ReleaseTransactionUnsupported => Error::MutationTransactionUnsupported { command },
        error => error,
    }
}

fn validate_release_state(
    repo: &Repository,
    path: &Path,
    expected: &ReleaseFileState,
) -> Result<()> {
    match read_release_file_state(repo, path) {
        Ok(current) if current == *expected => Ok(()),
        Ok(_) | Err(Error::ReleasePathConflict { .. }) => Err(Error::StaleReleasePlan {
            path: path.to_path_buf(),
        }),
        Err(error) => Err(error),
    }
}

fn prepare_release_change(
    repo: &Repository,
    change: &ReleaseFileChange,
    participant_identities: &[PathBuf],
) -> Result<Option<PreparedAtomicWrite>> {
    release_failpoint(ReleaseApplyStage::Prepare, &change.path, repo)?;
    if change.before == change.after {
        return Ok(None);
    }
    match change.after.contents() {
        Some(contents) => repo
            .prepare_atomic_write_avoiding(&change.path, contents, participant_identities)
            .map(Some),
        None => Ok(None),
    }
}

fn apply_release_change(
    repo: &Repository,
    change: &ReleaseFileChange,
    prepared: Option<PreparedAtomicWrite>,
    participant_identities: &[PathBuf],
) -> Result<AppliedReleaseOutcome> {
    if change.before == change.after {
        validate_release_state(repo, &change.path, &change.before)?;
        return Ok(AppliedReleaseOutcome::default());
    }
    release_failpoint(ReleaseApplyStage::Apply, &change.path, repo)?;
    match change.before.contents() {
        Some(expected) => {
            let claimed = claim_release_file(repo, &change.path, expected, participant_identities)?;
            if let Err(error) =
                release_failpoint(ReleaseApplyStage::ApplyClaimed, &change.path, repo)
            {
                return Err(restore_claim_after_error(claimed, &change.path, error));
            }
            match prepared {
                Some(prepared) => {
                    if change.after.contents().is_none() {
                        unreachable!("a prepared release change always writes a file");
                    }
                    install_over_claimed_file(repo, &change.path, prepared, claimed).map(
                        |original| AppliedReleaseOutcome {
                            original: Some(original),
                            created_directories: Vec::new(),
                        },
                    )
                }
                None => finish_claimed_deletion(repo, &change.path, claimed).map(|original| {
                    AppliedReleaseOutcome {
                        original: Some(original),
                        created_directories: Vec::new(),
                    }
                }),
            }
        }
        None => {
            match prepared {
                Some(prepared) => install_into_missing_path(repo, &change.path, prepared).map(
                    |created_directories| AppliedReleaseOutcome {
                        original: None,
                        created_directories,
                    },
                ),
                None => Ok(AppliedReleaseOutcome::default()),
            }
        }
    }
}

#[derive(Default)]
struct AppliedReleaseOutcome {
    original: Option<ClaimedReleaseFile>,
    created_directories: Vec<PathBuf>,
}

fn remove_release_fragment(
    repo: &Repository,
    fragment: &ReleaseFragment,
    participant_identities: &[PathBuf],
) -> Result<ClaimedReleaseFile> {
    release_failpoint(ReleaseApplyStage::Apply, &fragment.path, repo)?;
    let claimed = claim_release_file(
        repo,
        &fragment.path,
        fragment.contents.as_bytes(),
        participant_identities,
    )?;
    if let Err(error) = release_failpoint(ReleaseApplyStage::ApplyClaimed, &fragment.path, repo) {
        return Err(restore_claim_after_error(claimed, &fragment.path, error));
    }
    finish_claimed_deletion(repo, &fragment.path, claimed)
}

static RELEASE_CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ClaimedReleaseFile {
    directory: PathBuf,
    file: PathBuf,
    destination: PathBuf,
}

enum ClaimDiscardFailure {
    Retained {
        claimed: ClaimedReleaseFile,
        error: Box<Error>,
    },
    Cleaned(Box<Error>),
}

impl ClaimedReleaseFile {
    fn preflight_discard(&self, repo: &Repository, path: &Path) -> Result<()> {
        release_failpoint(ReleaseApplyStage::DiscardClaimed, path, repo)?;
        release_failpoint(ReleaseApplyStage::DiscardClaimDirectory, path, repo)?;
        #[cfg(test)]
        inject_release_claim_interference(path, &self.file);
        Ok(())
    }

    fn validate_retained_state(&self, path: &Path, expected: &ReleaseFileState) -> Result<()> {
        let expected = expected
            .contents()
            .expect("a retained release original was present before applying");
        let matches = match fs::symlink_metadata(&self.file) {
            Ok(metadata) if metadata.file_type().is_file() => match fs::read(&self.file) {
                Ok(contents) => contents == *expected,
                Err(source) => {
                    return Err(Error::ReadFile {
                        path: self.file.clone(),
                        source,
                    });
                }
            },
            Ok(_) => false,
            Err(source) => {
                return Err(Error::ReadFile {
                    path: self.file.clone(),
                    source,
                });
            }
        };
        if matches {
            Ok(())
        } else {
            Err(Error::StaleReleasePlan {
                path: path.to_path_buf(),
            })
        }
    }

    fn discard(
        self,
        repo: &Repository,
        path: &Path,
    ) -> std::result::Result<(), ClaimDiscardFailure> {
        if let Err(error) = release_failpoint(ReleaseApplyStage::DiscardClaimed, path, repo) {
            return Err(ClaimDiscardFailure::Retained {
                claimed: self,
                error: Box::new(error),
            });
        }
        if let Err(source) = fs::remove_file(&self.file) {
            let error = Error::RemoveFile {
                path: self.file.clone(),
                source,
            };
            return Err(ClaimDiscardFailure::Retained {
                claimed: self,
                error: Box::new(error),
            });
        }
        if let Err(error) = release_failpoint(ReleaseApplyStage::DiscardClaimDirectory, path, repo)
        {
            return Err(ClaimDiscardFailure::Cleaned(Box::new(error)));
        }
        fs::remove_dir(&self.directory).map_err(|source| {
            ClaimDiscardFailure::Cleaned(Box::new(Error::RemoveDirectory {
                path: self.directory,
                source,
            }))
        })
    }

    fn discard_after_commit(
        self,
        repo: &Repository,
        path: &Path,
    ) -> std::result::Result<(), ClaimDiscardFailure> {
        if let Err(error) =
            release_failpoint(ReleaseApplyStage::CommittedDiscardClaimed, path, repo)
        {
            return Err(ClaimDiscardFailure::Retained {
                claimed: self,
                error: Box::new(error),
            });
        }
        self.discard(repo, path)
    }

    fn restore(self, path: &Path) -> Result<()> {
        match move_path_if_absent(&self.file, &self.destination) {
            Ok(()) => fs::remove_dir(&self.directory).map_err(|source| Error::RemoveDirectory {
                path: self.directory,
                source,
            }),
            Err(source) if error_has_kind(&source, ErrorKind::AlreadyExists) => {
                Err(Error::ReleaseRollbackConflict {
                    path: path.to_path_buf(),
                })
            }
            Err(source) => Err(Error::WriteFile {
                path: self.destination,
                source,
            }),
        }
    }
}

fn restore_claim_after_error(claimed: ClaimedReleaseFile, path: &Path, error: Error) -> Error {
    match claimed.restore(path) {
        Ok(()) => error,
        Err(restore_error) => release_apply_error(error, vec![restore_error.to_string()]),
    }
}

fn claim_release_file(
    repo: &Repository,
    path: &Path,
    expected: &[u8],
    participant_identities: &[PathBuf],
) -> Result<ClaimedReleaseFile> {
    let destination = repo.resolve(path);
    let directory = reserve_release_claim_directory(&destination, participant_identities)?;
    let file = directory.join("claimed");
    if let Err(source) = fs::rename(&destination, &file) {
        let _ = fs::remove_dir(&directory);
        return if error_has_kind(&source, ErrorKind::NotFound) {
            Err(Error::StaleReleasePlan {
                path: path.to_path_buf(),
            })
        } else {
            Err(Error::WriteFile {
                path: destination,
                source,
            })
        };
    }
    let claimed = ClaimedReleaseFile {
        directory,
        file,
        destination,
    };
    let matches = match fs::symlink_metadata(&claimed.file) {
        Ok(metadata) if metadata.file_type().is_file() => match fs::read(&claimed.file) {
            Ok(contents) => contents == expected,
            Err(source) => {
                let error = Error::ReadFile {
                    path: claimed.file.clone(),
                    source,
                };
                return Err(restore_claim_after_error(claimed, path, error));
            }
        },
        Ok(_) => false,
        Err(source) => {
            let error = Error::ReadFile {
                path: claimed.file.clone(),
                source,
            };
            return Err(restore_claim_after_error(claimed, path, error));
        }
    };
    if matches {
        Ok(claimed)
    } else {
        let error = Error::StaleReleasePlan {
            path: path.to_path_buf(),
        };
        Err(restore_claim_after_error(claimed, path, error))
    }
}

fn reserve_release_claim_directory(
    destination: &Path,
    participant_identities: &[PathBuf],
) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .expect("resolved repository paths always have a parent");
    let canonical_parent = fs::canonicalize(parent).map_err(|source| Error::ReadFile {
        path: parent.to_path_buf(),
        source,
    })?;
    loop {
        let sequence = RELEASE_CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(".sacho-claim-{}-{sequence}", std::process::id());
        let directory = parent.join(&name);
        let identity = canonical_parent.join(name);
        if release_claim_path_conflicts(&identity, participant_identities) {
            continue;
        }
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(source) if error_has_kind(&source, ErrorKind::AlreadyExists) => continue,
            Err(source) => {
                return Err(Error::CreateDirectory {
                    path: directory,
                    source,
                });
            }
        }
    }
}

fn install_over_claimed_file(
    repo: &Repository,
    path: &Path,
    prepared: PreparedAtomicWrite,
    claimed: ClaimedReleaseFile,
) -> Result<ClaimedReleaseFile> {
    match prepared.commit_if_absent() {
        Ok(()) => Ok(claimed),
        Err(source) if error_has_kind(&source, ErrorKind::AlreadyExists) => {
            let error = Error::StaleReleasePlan {
                path: path.to_path_buf(),
            };
            Err(retain_claim_after_collision(path, claimed, error))
        }
        Err(source) => {
            let error = Error::WriteFile {
                path: repo.resolve(path),
                source,
            };
            Err(restore_claim_after_error(claimed, path, error))
        }
    }
}

fn claim_discard_error(failure: ClaimDiscardFailure) -> Error {
    match failure {
        ClaimDiscardFailure::Retained { claimed, error } => {
            drop(claimed);
            *error
        }
        ClaimDiscardFailure::Cleaned(error) => *error,
    }
}

fn retain_claim_after_collision(path: &Path, claimed: ClaimedReleaseFile, error: Error) -> Error {
    drop(claimed);
    release_apply_error(
        error,
        vec![
            Error::ReleaseRollbackConflict {
                path: path.to_path_buf(),
            }
            .to_string(),
        ],
    )
}

fn install_into_missing_path(
    repo: &Repository,
    path: &Path,
    prepared: PreparedAtomicWrite,
) -> Result<Vec<PathBuf>> {
    prepared
        .commit_if_absent_with_created_directories()
        .map_err(|source| {
            if error_has_kind(&source, ErrorKind::AlreadyExists) {
                Error::StaleReleasePlan {
                    path: path.to_path_buf(),
                }
            } else {
                Error::WriteFile {
                    path: repo.resolve(path),
                    source,
                }
            }
        })
}

fn error_has_kind(error: &std::io::Error, expected: ErrorKind) -> bool {
    error.kind() == expected
}

fn release_claim_path_conflicts(identity: &Path, participant_identities: &[PathBuf]) -> bool {
    participant_identities
        .iter()
        .any(|participant| participant.starts_with(identity))
}

fn finish_claimed_deletion(
    repo: &Repository,
    path: &Path,
    claimed: ClaimedReleaseFile,
) -> Result<ClaimedReleaseFile> {
    match fs::symlink_metadata(repo.resolve(path)) {
        Err(source) if error_has_kind(&source, ErrorKind::NotFound) => Ok(claimed),
        Ok(_) => {
            let error = Error::StaleReleasePlan {
                path: path.to_path_buf(),
            };
            Err(retain_claim_after_collision(path, claimed, error))
        }
        Err(source) => {
            let error = Error::ReadFile {
                path: repo.resolve(path),
                source,
            };
            Err(restore_claim_after_error(claimed, path, error))
        }
    }
}

fn restore_release_state(
    repo: &Repository,
    path: &Path,
    before: &ReleaseFileState,
    applied_after: &ReleaseFileState,
) -> Result<()> {
    release_failpoint(ReleaseApplyStage::Rollback, path, repo)?;
    match applied_after.contents() {
        Some(expected) => restore_over_applied_file(repo, path, before, expected),
        None => restore_into_missing_path(repo, path, before),
    }
}

fn restore_over_applied_file(
    repo: &Repository,
    path: &Path,
    before: &ReleaseFileState,
    expected: &[u8],
) -> Result<()> {
    let absolute = repo.resolve(path);
    let directory = reserve_release_claim_directory(&absolute, &[])?;
    let guard = directory.join("claimed");
    if let Err(source) = fs::rename(&absolute, &guard) {
        let _ = fs::remove_dir(&directory);
        return Err(if error_has_kind(&source, ErrorKind::NotFound) {
            Error::ReleaseRollbackConflict {
                path: path.to_path_buf(),
            }
        } else {
            Error::WriteFile {
                path: absolute.clone(),
                source,
            }
        });
    }
    let claimed_file = ClaimedReleaseFile {
        directory,
        file: guard.clone(),
        destination: absolute.clone(),
    };
    let metadata = match fs::symlink_metadata(&guard) {
        Ok(metadata) => metadata,
        Err(source) => {
            let error = Error::ReadFile {
                path: guard.clone(),
                source,
            };
            claimed_file.restore(path)?;
            return Err(error);
        }
    };
    if !metadata.file_type().is_file() {
        claimed_file.restore(path)?;
        return Err(Error::ReleaseRollbackConflict {
            path: path.to_path_buf(),
        });
    }
    let claimed_contents = match fs::read(&guard) {
        Ok(claimed) => claimed,
        Err(source) => {
            let error = Error::ReadFile {
                path: guard.clone(),
                source,
            };
            claimed_file.restore(path)?;
            return Err(error);
        }
    };
    if claimed_contents != expected {
        claimed_file.restore(path)?;
        return Err(Error::ReleaseRollbackConflict {
            path: path.to_path_buf(),
        });
    }

    if let Err(error) = release_failpoint(ReleaseApplyStage::RollbackClaimed, path, repo) {
        preserve_applied_claim(repo, path, claimed_file)?;
        return Err(error);
    }
    let result = match before.contents() {
        Some(contents) => install_rollback_file_if_absent(repo, path, contents),
        None => match fs::symlink_metadata(&absolute) {
            Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(Error::ReleaseRollbackConflict {
                path: path.to_path_buf(),
            }),
            Err(source) => Err(Error::ReadFile {
                path: absolute.clone(),
                source,
            }),
        },
    };
    match result {
        Ok(()) => claimed_file
            .discard(repo, path)
            .map_err(claim_discard_error),
        Err(error) => {
            preserve_applied_claim(repo, path, claimed_file)?;
            Err(error)
        }
    }
}

fn preserve_applied_claim(
    repo: &Repository,
    path: &Path,
    claimed: ClaimedReleaseFile,
) -> Result<()> {
    match move_path_if_absent(&claimed.file, &claimed.destination) {
        Ok(()) => fs::remove_dir(&claimed.directory).map_err(|source| Error::RemoveDirectory {
            path: claimed.directory,
            source,
        }),
        Err(source) if error_has_kind(&source, ErrorKind::AlreadyExists) => {
            claimed.discard(repo, path).map_err(claim_discard_error)
        }
        Err(source) => Err(Error::WriteFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn restore_into_missing_path(
    repo: &Repository,
    path: &Path,
    before: &ReleaseFileState,
) -> Result<()> {
    release_failpoint(ReleaseApplyStage::RollbackClaimed, path, repo)?;
    match before.contents() {
        Some(contents) => install_rollback_file_if_absent(repo, path, contents),
        None => Ok(()),
    }
}

fn install_rollback_file_if_absent(repo: &Repository, path: &Path, contents: &[u8]) -> Result<()> {
    repo.prepare_atomic_write(path, contents)?
        .commit_if_absent()
        .map_err(|source| {
            if source.kind() == ErrorKind::AlreadyExists {
                Error::ReleaseRollbackConflict {
                    path: path.to_path_buf(),
                }
            } else {
                Error::WriteFile {
                    path: repo.resolve(path),
                    source,
                }
            }
        })
}

enum ReleaseClaimCommitFailure {
    BeforeCommit(Error),
    AfterCommit(Vec<String>),
}

fn commit_release_claims(
    repo: &Repository,
    applied: &mut [AppliedReleaseChange],
    unchanged_participants: &[ReleaseFileChange],
) -> std::result::Result<(), ReleaseClaimCommitFailure> {
    for change in applied.iter() {
        if let Some(original) = &change.original {
            original
                .preflight_discard(repo, &change.path)
                .map_err(ReleaseClaimCommitFailure::BeforeCommit)?;
        }
    }
    for change in applied.iter() {
        if let Some(original) = &change.original {
            original
                .validate_retained_state(&change.path, &change.before)
                .map_err(ReleaseClaimCommitFailure::BeforeCommit)?;
        }
    }
    for change in applied.iter() {
        validate_release_state(repo, &change.path, &change.after)
            .map_err(ReleaseClaimCommitFailure::BeforeCommit)?;
    }
    for change in unchanged_participants {
        validate_release_state(repo, &change.path, &change.after)
            .map_err(ReleaseClaimCommitFailure::BeforeCommit)?;
    }
    validate_release_fragment_paths(repo, &[]).map_err(ReleaseClaimCommitFailure::BeforeCommit)?;
    let mut failures = Vec::new();
    for change in applied.iter_mut() {
        let Some(original) = change.original.take() else {
            continue;
        };
        match original.discard_after_commit(repo, &change.path) {
            Ok(()) => {}
            Err(failure) => failures.push(format!(
                "{}: {}",
                change.path.display(),
                claim_discard_error(failure)
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ReleaseClaimCommitFailure::AfterCommit(failures))
    }
}

struct AppliedReleaseChange {
    path: PathBuf,
    before: ReleaseFileState,
    after: ReleaseFileState,
    original: Option<ClaimedReleaseFile>,
    created_directories: Vec<PathBuf>,
}

fn rollback_release(repo: &Repository, error: Error, applied: Vec<AppliedReleaseChange>) -> Error {
    let rollback_failures = applied
        .into_iter()
        .rev()
        .filter_map(|change| {
            rollback_applied_release_change(repo, change)
                .err()
                .map(|error| error.to_string())
        })
        .collect();
    release_apply_error(error, rollback_failures)
}

fn rollback_applied_release_change(repo: &Repository, change: AppliedReleaseChange) -> Result<()> {
    match change.original {
        Some(original) => {
            restore_retained_release_original(repo, &change.path, &change.after, original)
        }
        None => restore_release_state(repo, &change.path, &change.before, &change.after),
    }?;
    remove_release_created_directories(change.created_directories)
}

fn remove_release_created_directories(directories: Vec<PathBuf>) -> Result<()> {
    for directory in directories.into_iter().rev() {
        fs::remove_dir(&directory).map_err(|source| Error::RemoveDirectory {
            path: directory,
            source,
        })?;
    }
    Ok(())
}

fn restore_retained_release_original(
    repo: &Repository,
    path: &Path,
    applied_after: &ReleaseFileState,
    original: ClaimedReleaseFile,
) -> Result<()> {
    release_failpoint(ReleaseApplyStage::Rollback, path, repo)?;
    match applied_after.contents() {
        Some(expected) => {
            let applied = claim_release_file(repo, path, expected, &[]).map_err(|error| {
                if matches!(
                    error,
                    Error::StaleReleasePlan { .. } | Error::ReleasePathConflict { .. }
                ) {
                    Error::ReleaseRollbackConflict {
                        path: path.to_path_buf(),
                    }
                } else {
                    error
                }
            })?;
            if let Err(error) = release_failpoint(ReleaseApplyStage::RollbackClaimed, path, repo) {
                preserve_applied_claim(repo, path, applied)?;
                return Err(error);
            }
            match original.restore(path) {
                Ok(()) => applied.discard(repo, path).map_err(claim_discard_error),
                Err(error) => {
                    preserve_applied_claim(repo, path, applied)?;
                    Err(error)
                }
            }
        }
        None => {
            match fs::symlink_metadata(repo.resolve(path)) {
                Err(source) if source.kind() == ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(Error::ReleaseRollbackConflict {
                        path: path.to_path_buf(),
                    });
                }
                Err(source) => {
                    return Err(Error::ReadFile {
                        path: repo.resolve(path),
                        source,
                    });
                }
            }
            release_failpoint(ReleaseApplyStage::RollbackClaimed, path, repo)?;
            original.restore(path)
        }
    }
}

fn release_apply_error(error: Error, rollback_failures: Vec<String>) -> Error {
    match error {
        Error::ReleaseApply {
            cause,
            rollback_failures: mut existing_failures,
        } => {
            existing_failures.extend(rollback_failures);
            Error::ReleaseApply {
                cause,
                rollback_failures: existing_failures,
            }
        }
        error => Error::ReleaseApply {
            cause: error.to_string(),
            rollback_failures,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseApplyStage {
    PlanSnapshot,
    Probe,
    Prepare,
    Apply,
    ApplyClaimed,
    DiscardClaimed,
    DiscardClaimDirectory,
    CommittedDiscardClaimed,
    Rollback,
    RollbackClaimed,
}

#[cfg(not(test))]
fn release_failpoint(_stage: ReleaseApplyStage, _path: &Path, _repo: &Repository) -> Result<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static RELEASE_FAILPOINTS: std::cell::RefCell<Vec<(ReleaseApplyStage, PathBuf)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static RELEASE_INTERFERENCES: std::cell::RefCell<Vec<(ReleaseApplyStage, PathBuf, PathBuf, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static RELEASE_CLAIM_INTERFERENCES: std::cell::RefCell<Vec<(PathBuf, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn release_failpoint(stage: ReleaseApplyStage, path: &Path, repo: &Repository) -> Result<()> {
    RELEASE_INTERFERENCES.with(|interferences| {
        let mut interferences = interferences.borrow_mut();
        if let Some(index) = interferences
            .iter()
            .position(|interference| interference.0 == stage && interference.1 == path)
        {
            let (_, _, target, contents) = interferences.remove(index);
            fs::write(repo.resolve(target), contents).expect("inject concurrent release edit");
        }
    });
    let should_fail = RELEASE_FAILPOINTS.with(|failpoints| {
        let mut failpoints = failpoints.borrow_mut();
        failpoints
            .iter()
            .position(|expected| expected == &(stage, path.to_path_buf()))
            .map(|index| failpoints.remove(index))
            .is_some()
    });
    if should_fail {
        Err(Error::WriteFile {
            path: repo.resolve(path),
            source: std::io::Error::other("injected release I/O failure"),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn set_release_failpoint(stage: ReleaseApplyStage, path: impl Into<PathBuf>) {
    RELEASE_FAILPOINTS.with(|failpoints| failpoints.borrow_mut().push((stage, path.into())));
}

#[cfg(test)]
fn set_release_interference(
    stage: ReleaseApplyStage,
    trigger: impl Into<PathBuf>,
    target: impl Into<PathBuf>,
    contents: impl Into<String>,
) {
    RELEASE_INTERFERENCES.with(|interferences| {
        interferences
            .borrow_mut()
            .push((stage, trigger.into(), target.into(), contents.into()));
    });
}

#[cfg(test)]
fn set_release_claim_interference(path: impl Into<PathBuf>, contents: impl Into<String>) {
    RELEASE_CLAIM_INTERFERENCES.with(|interferences| {
        interferences
            .borrow_mut()
            .push((path.into(), contents.into()));
    });
}

#[cfg(test)]
fn inject_release_claim_interference(path: &Path, claimed: &Path) {
    RELEASE_CLAIM_INTERFERENCES.with(|interferences| {
        let mut interferences = interferences.borrow_mut();
        if let Some(index) = interferences
            .iter()
            .position(|(expected, _)| expected == path)
        {
            let (_, contents) = interferences.remove(index);
            fs::write(claimed, contents).expect("inject concurrent retained-original edit");
        }
    });
}

#[cfg(test)]
fn clear_release_failpoint() {
    RELEASE_FAILPOINTS.with(|failpoints| failpoints.borrow_mut().clear());
    RELEASE_INTERFERENCES.with(|interferences| interferences.borrow_mut().clear());
    RELEASE_CLAIM_INTERFERENCES.with(|interferences| interferences.borrow_mut().clear());
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

fn empty_unreleased_markdown(repo: &Repository, next: &str) -> String {
    let heading = format!("Version {next}");
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
    next_unreleased: Option<&str>,
    released: &str,
    detection: RegionDetection,
    unreleased_heading: &str,
    document_title: &str,
) -> std::result::Result<String, ChangelogError> {
    match detection {
        RegionDetection::Heading => {
            let replacement = next_unreleased.map_or_else(
                || released.to_owned(),
                |unreleased| format!("{}\n\n\n{}", unreleased.trim_end(), released),
            );
            replace_unreleased_region(source, &replacement, detection, unreleased_heading)
                .map(|replacement| replacement.new_contents)
        }
        RegionDetection::Marker => {
            let replacement = if let Some(unreleased) = next_unreleased {
                replace_unreleased_region(source, unreleased, detection, unreleased_heading)?
                    .new_contents
            } else {
                remove_unreleased_region(source, detection, unreleased_heading)?
            };
            Ok(insert_released_after_marker_region(
                &replacement,
                released,
                document_title,
            ))
        }
    }
}

fn remove_unreleased_region(
    source: &str,
    detection: RegionDetection,
    unreleased_heading: &str,
) -> std::result::Result<String, ChangelogError> {
    let span = find_unreleased_region(source, detection, unreleased_heading)?;
    let mut output = String::with_capacity(source.len() - (span.end - span.start));
    output.push_str(&source[..span.start]);
    if detection == RegionDetection::Marker {
        output.push('\n');
    }
    output.push_str(&source[span.end..]);
    Ok(output)
}

fn insert_released_after_marker_region(
    source: &str,
    released: &str,
    document_title: &str,
) -> String {
    const END_MARKER: &str = "<!-- sacho:unreleased:end -->";

    let Some(marker_index) = source.find(END_MARKER) else {
        return insert_released_section(source, released, document_title);
    };
    let after_marker = marker_index + END_MARKER.len();
    let insertion = source[after_marker..]
        .find('\n')
        .map_or(source.len(), |offset| {
            after_marker + offset.saturating_add(1)
        });

    let mut output = String::with_capacity(source.len() + released.len() + 2);
    output.push_str(&source[..insertion]);
    set_hongdown_separator_before(&mut output, released);
    output.push_str(released.trim_end());
    let suffix = source[insertion..].trim_start_matches(['\n', '\r']);
    if suffix.is_empty() {
        output.push('\n');
    } else {
        set_hongdown_separator_before(&mut output, suffix);
        output.push_str(suffix);
    }
    output
}

fn insert_released_section(source: &str, released: &str, document_title: &str) -> String {
    let insertion = insertion_index_after_title(source, document_title);
    let mut output = String::with_capacity(source.len() + released.len() + 2);
    output.push_str(&source[..insertion]);
    if !output.is_empty() {
        set_trailing_newline_count(&mut output, 2);
    }
    output.push_str(released.trim_end());
    let suffix = source[insertion..].trim_start_matches(['\n', '\r']);
    if suffix.is_empty() {
        output.push('\n');
    } else {
        set_hongdown_separator_before(&mut output, suffix);
        output.push_str(suffix);
    }
    output
}

fn insert_unreleased_region(source: &str, unreleased: &str, document_title: &str) -> String {
    let insertion = insertion_index_after_title(source, document_title);
    let mut output = String::with_capacity(source.len() + unreleased.len() + 2);
    output.push_str(&source[..insertion]);
    if !output.is_empty() {
        set_trailing_newline_count(&mut output, 2);
    }
    output.push_str(unreleased.trim_end());
    let suffix = source[insertion..].trim_start_matches(['\n', '\r']);
    if suffix.is_empty() {
        output.push('\n');
    } else {
        set_hongdown_separator_before(&mut output, suffix);
        output.push_str(suffix);
    }
    output
}

fn insertion_index_after_title(source: &str, document_title: &str) -> usize {
    let lines = source_lines_with_offsets(source);
    let Some(title) = insertion_title_span(source, document_title) else {
        return 0;
    };
    let after_title = lines
        .iter()
        .position(|line| line.start >= title.end)
        .unwrap_or(lines.len());
    skip_blank_lines(&lines, after_title).map_or(source.len(), |index| lines[index].start)
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

fn sync_after_mutation(repo: &Repository) -> Result<Option<SyncResult>> {
    if repo.config().changelog.materialize {
        let plan = plan_sync_unlocked(repo, SyncOptions { force: true })?;
        return apply_sync_unlocked(repo, plan).map(Some);
    }
    Ok(None)
}

fn ensure_materialized_current_before_mutation(repo: &Repository) -> Result<()> {
    if !repo.config().changelog.materialize {
        return Ok(());
    }

    match plan_sync_unlocked(repo, SyncOptions { force: false })? {
        SyncPlan::Skipped(_) | SyncPlan::Apply(_) => Ok(()),
        SyncPlan::NeedsConfirmation { .. } => Err(Error::SyncNeedsConfirmation {
            path: repo.config().changelog.path.clone(),
        }),
    }
}

fn format_fragment_source(source: &str) -> Result<String> {
    let parsed = parse_frontmatter_for_format(source)?;
    let body = format_markdown(parsed.body)?;
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

    for entry in &mut frontmatter {
        if entry.key == Value::String(String::from("links"))
            && let Value::Mapping(mapping) = &entry.value
        {
            let mut values = mapping.clone().into_iter().collect::<Vec<_>>();
            values.sort_by_key(|(key, _)| yaml_key_sort_text(key));
            entry.value = Value::Mapping(values.into_iter().collect());
        }
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

fn check_fix_cleanup_warning(warning: MutationCleanupWarning) -> CheckWarning {
    CheckWarning {
        message: format!(
            "sacho check --fix committed successfully, but transaction cleanup failed for {}: {}",
            warning.path.display(),
            warning.message,
        ),
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

fn init_root(start: &Path) -> PathBuf {
    let absolute = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(start))
            .unwrap_or_else(|_| start.to_path_buf())
    };
    let start = fs::canonicalize(&absolute).unwrap_or_else(|_| normalize_absolute_path(&absolute));
    let marker = start.ancestors().find_map(|candidate| {
        repository_preset(candidate).map(|preset| (candidate.to_path_buf(), preset))
    });
    if let Some((root, VcsPreset::Jj | VcsPreset::Hg)) = &marker {
        return root.clone();
    }
    if let Some(root) = git_output(&start, ["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|output| parse_git_root_output(&output))
    {
        return root;
    }
    marker.map_or(start, |(root, _)| root)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn default_init_config(root: &Path, options: &InitOptions) -> Result<Config> {
    let mut config = Config::parse("").expect("default config parses");
    if let Some(name) = root.file_name().and_then(|name| name.to_str()) {
        config.changelog.title = format!("{name} changelog");
    }
    if let Some(path) = &options.changelog_path {
        config.changelog.path = path.clone();
    }
    if let Some(directory) = &options.fragment_directory {
        config.fragments.directory = directory.clone();
    }
    if let Some(materialize) = options.materialize {
        config.changelog.materialize = materialize;
    }
    config.sections.clone_from(&options.sections);
    config
        .section_patterns
        .clone_from(&options.section_patterns);
    for section in &config.sections {
        compile_glob_set(&section.paths)?;
    }
    config.vcs.preset = repository_preset(root)
        .or_else(|| is_git_repository(root).then_some(VcsPreset::Git))
        .unwrap_or(VcsPreset::None);
    if let Some(url) = options.repository_url.as_deref().map(str::trim)
        && !url.is_empty()
    {
        let url =
            crate::repository_url::normalize_explicit(url).ok_or(Error::InvalidRepositoryUrl)?;
        config.links.insert(
            ReferenceSigil::new("#"),
            UrlTemplate::new(url.issue_template),
        );
    }
    Ok(config)
}

fn render_init_config(config: &Config) -> String {
    let mut output = String::new();
    output.push_str("[changelog]\n");
    output.push_str(&format!(
        "path = {}\n",
        toml_basic_string(&config.changelog.path.display().to_string())
    ));
    output.push_str(&format!(
        "title = {}\n",
        toml_basic_string(&config.changelog.title)
    ));
    output.push_str(&format!(
        "unreleased-heading = {}\n",
        toml_basic_string(&config.changelog.unreleased_heading)
    ));
    output.push_str(&format!("materialize = {}\n", config.changelog.materialize));
    output.push_str(&format!(
        "region-detection = {}\n\n",
        toml_basic_string(toml_vcs_region(config.changelog.region_detection))
    ));
    output.push_str("[fragments]\n");
    output.push_str(&format!(
        "directory = {}\n",
        toml_basic_string(&config.fragments.directory.display().to_string())
    ));
    output.push_str(&format!(
        "next-file = {}\n\n",
        toml_basic_string(&config.fragments.next_file.display().to_string())
    ));
    if !config.links.is_empty() {
        output.push_str("[links]\n");
        for (sigil, template) in &config.links {
            output.push_str(&format!(
                "{} = {}\n",
                toml_basic_string(sigil.as_str()),
                toml_basic_string(template.as_str())
            ));
        }
        output.push('\n');
    }
    output.push_str("[vcs]\n");
    output.push_str(&format!(
        "preset = {}\n\n",
        toml_basic_string(toml_vcs_preset(config.vcs.preset))
    ));
    output.push_str("[check]\n");
    output.push_str("paths = []\n");
    for section in &config.sections {
        output.push_str("\n[[sections]]\n");
        output.push_str(&format!("id = {}\n", toml_basic_string(&section.id)));
        output.push_str(&format!(
            "directory = {}\n",
            toml_basic_string(&section.directory.display().to_string())
        ));
        output.push_str(&format!("paths = {}\n", toml_string_array(&section.paths)));
    }
    for pattern in &config.section_patterns {
        output.push_str("\n[[section-patterns]]\n");
        output.push_str(&format!(
            "source = {}\n",
            toml_basic_string(&pattern.source.to_string())
        ));
        output.push_str(&format!(
            "id = {}\n",
            toml_basic_string(&pattern.id.to_string())
        ));
        output.push_str(&format!(
            "directory = {}\n",
            toml_basic_string(&pattern.directory.to_string())
        ));
        if let Some(paths) = &pattern.paths {
            output.push_str(&format!(
                "paths = {}\n",
                toml_string_array(&paths.iter().map(ToString::to_string).collect::<Vec<_>>())
            ));
        }
    }
    output
}

fn toml_basic_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '\"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{000c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            control if control <= '\u{001f}' || control == '\u{007f}' => {
                output.push_str(&format!("\\u{:04X}", control as u32));
            }
            character => output.push(character),
        }
    }
    output.push('\"');
    output
}

fn toml_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| toml_basic_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn validate_init_changelog_path(
    config_path: &Path,
    changelog_path: &Path,
    configured_changelog_path: &Path,
) -> Result<()> {
    let config_identity = filesystem_path_identity(config_path)?;
    let changelog_identity = filesystem_path_identity(changelog_path)?;
    if release_paths_overlap(&config_identity, &changelog_identity, true) {
        return Err(Error::InitConflict {
            message: format!(
                "changelog path {} overlaps {}",
                configured_changelog_path.display(),
                Repository::CONFIG_FILE
            ),
        });
    }
    Ok(())
}

fn create_initial_changelog_if_absent(
    config_path: &Path,
    changelog_path: &Path,
    configured_changelog_path: &Path,
    config: &Config,
) -> Result<bool> {
    if let Some(parent) = changelog_path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(changelog_path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == ErrorKind::AlreadyExists => {
            validate_init_changelog_path(config_path, changelog_path, configured_changelog_path)?;
            if changelog_path.is_file() {
                return Ok(false);
            }
            return Err(Error::InitConflict {
                message: format!(
                    "{} exists but is not a file",
                    configured_changelog_path.display()
                ),
            });
        }
        Err(source) => {
            return Err(Error::WriteFile {
                path: changelog_path.to_path_buf(),
                source,
            });
        }
    };
    file.write_all(initial_changelog_for_config(config).as_bytes())
        .map_err(|source| Error::WriteFile {
            path: changelog_path.to_path_buf(),
            source,
        })?;
    Ok(true)
}

fn toml_vcs_region(region: RegionDetection) -> &'static str {
    match region {
        RegionDetection::Heading => "heading",
        RegionDetection::Marker => "marker",
    }
}

fn toml_vcs_preset(preset: VcsPreset) -> &'static str {
    match preset {
        VcsPreset::Git => "git",
        VcsPreset::Jj => "jj",
        VcsPreset::Hg => "hg",
        VcsPreset::None => "none",
    }
}

fn initial_changelog_for_config(config: &Config) -> String {
    if config.changelog.materialize {
        initial_materialized_changelog(config)
    } else {
        initial_changelog(&config.changelog.title)
    }
}

fn initial_materialized_changelog(config: &Config) -> String {
    let mut changelog = initial_changelog(&config.changelog.title);
    let mut unreleased = String::from("Unreleased\n----------\n\n");
    unreleased.push_str(&config.changelog.unreleased_heading);
    unreleased.push('\n');
    if config.changelog.region_detection == RegionDetection::Marker {
        changelog.push_str(BEGIN_MARKER);
        changelog.push('\n');
        changelog.push_str(&marker_region_contents(&unreleased));
        changelog.push_str(END_MARKER);
        changelog.push('\n');
    } else {
        changelog.push_str(&unreleased);
    }
    changelog
}

fn apply_git_integration(
    root: &Path,
    config: &Config,
    integration_executable: &str,
    result: &mut InitResult,
) -> Result<()> {
    if config.vcs.preset != VcsPreset::Git || !is_git_repository(root) {
        return Ok(());
    }

    let changelog_attr = format!("{} merge=sacho", git_attr_path(&config.changelog.path));
    let next_path = config.fragments.directory.join(&config.fragments.next_file);
    let next_attr = format!("{} merge=ours", git_attr_path(&next_path));
    let attributes_path = root.join(".gitattributes");
    let attributes_existed = attributes_path.exists();
    let old = match fs::read_to_string(&attributes_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::ReadFile {
                path: attributes_path,
                source,
            });
        }
    };
    let edited = edit_gitattributes(&old, &[changelog_attr.as_str(), next_attr.as_str()])?;
    if old != edited {
        fs::write(root.join(".gitattributes"), edited).map_err(|source| Error::WriteFile {
            path: root.join(".gitattributes"),
            source,
        })?;
        if attributes_existed {
            result.modified_files.push(PathBuf::from(".gitattributes"));
        } else {
            result.created_files.push(PathBuf::from(".gitattributes"));
        }
    } else if attributes_existed {
        result
            .skipped_existing_files
            .push(PathBuf::from(".gitattributes"));
    }

    set_git_config(
        root,
        "merge.sacho.name",
        "Sacho changelog merge driver",
        result,
    )?;
    set_git_config(
        root,
        "merge.sacho.driver",
        &format!(
            "{} merge-driver %O %A %B %P",
            shell_quote(integration_executable)
        ),
        result,
    )?;
    if git_config_get(root, "merge.ours.driver").is_err() {
        set_git_config(root, "merge.ours.driver", "true", result)?;
    }
    Ok(())
}

const HG_INTEGRATION_BEGIN: &str = "# sacho integration begin";
const HG_INTEGRATION_END: &str = "# sacho integration end";

fn apply_hg_integration(
    root: &Path,
    config: &Config,
    integration_executable: &str,
    result: &mut InitResult,
) -> Result<()> {
    let hg_dir = root.join(".hg");
    if !hg_dir.is_dir() {
        result.manual_actions_required.push(String::from(
            "Mercurial integration not installed: .hg directory was not found",
        ));
        return Ok(());
    }
    let Some(next) = hg_config_path(&config.fragments.directory.join(&config.fragments.next_file))
    else {
        result.manual_actions_required.push(String::from(
            "Mercurial integration not installed: the next-version path must not contain newlines or `=`, or end in whitespace",
        ));
        return Ok(());
    };
    let changelog = if config.changelog.materialize {
        let Some(changelog) = hg_config_path(&config.changelog.path) else {
            result.manual_actions_required.push(String::from(
                "Mercurial integration not installed: the changelog path must not contain newlines or `=`, or end in whitespace",
            ));
            return Ok(());
        };
        Some(changelog)
    } else {
        None
    };
    let hook_command = format!("{} hook-hg-update", shell_quote(integration_executable));
    let block = if let Some(changelog) = &changelog {
        format!(
            "{HG_INTEGRATION_BEGIN}\n[merge-patterns]\nfilepath:{changelog} = sacho\nfilepath:{next} = :local\n\n[merge-tools]\nsacho.executable = {integration_executable}\nsacho.args = merge-driver $base $output $other $output\nsacho.premerge = false\nsacho.priority = -100\n\n[hooks]\nupdate.sacho = {hook_command}\n{HG_INTEGRATION_END}\n"
        )
    } else {
        format!(
            "{HG_INTEGRATION_BEGIN}\n[merge-patterns]\nfilepath:{next} = :local\n\n[hooks]\nupdate.sacho = {hook_command}\n{HG_INTEGRATION_END}\n"
        )
    };
    let hgrc = hg_dir.join("hgrc");
    let old = match fs::read_to_string(&hgrc) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(source) => return Err(Error::ReadFile { path: hgrc, source }),
    };
    let edited = match edit_hgrc(
        &old,
        &block,
        changelog.as_deref(),
        &next,
        integration_executable,
    ) {
        Ok(edited) => edited,
        Err(reason) => {
            result.manual_actions_required.push(format!(
                "Mercurial integration not installed: {reason}; add this block to .hg/hgrc manually:\n{block}"
            ));
            return Ok(());
        }
    };
    if edited != old {
        fs::write(&hgrc, edited).map_err(|source| Error::WriteFile { path: hgrc, source })?;
        if let Some(changelog) = changelog {
            result.local_hg_config_changes.extend([
                format!("merge-patterns.filepath:{changelog}"),
                format!("merge-patterns.filepath:{next}"),
                String::from("merge-tools.sacho"),
                String::from("hooks.update.sacho"),
            ]);
        } else {
            result.local_hg_config_changes.extend([
                format!("merge-patterns.filepath:{next}"),
                String::from("hooks.update.sacho"),
            ]);
        }
    }
    Ok(())
}

fn hg_config_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    (!path.contains(['\n', '\r', '=']) && path.trim_end() == path).then_some(path)
}

fn edit_hgrc(
    source: &str,
    block: &str,
    changelog: Option<&str>,
    next: &str,
    integration_executable: &str,
) -> std::result::Result<String, String> {
    match (
        source.find(HG_INTEGRATION_BEGIN),
        source.find(HG_INTEGRATION_END),
    ) {
        (Some(begin), Some(end)) if begin <= end => {
            let end = end + HG_INTEGRATION_END.len();
            let mut output = String::new();
            output.push_str(&source[..begin]);
            output.push_str(block.trim_end());
            output.push_str(&source[end..]);
            if !output.ends_with('\n') {
                output.push('\n');
            }
            validate_hgrc_integration_settings(&output, changelog, next, integration_executable)?;
            Ok(output)
        }
        (None, None) => {
            validate_hgrc_integration_settings(source, changelog, next, integration_executable)?;
            let mut output = source.to_owned();
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(block);
            Ok(output)
        }
        _ => Err(String::from("existing Sacho marker block is malformed")),
    }
}

fn validate_hgrc_integration_settings(
    source: &str,
    changelog: Option<&str>,
    next: &str,
    integration_executable: &str,
) -> std::result::Result<(), String> {
    let hook_command = format!("{} hook-hg-update", shell_quote(integration_executable));
    let mut settings = Vec::new();
    if let Some(changelog) = changelog {
        settings.extend([
            ("merge-patterns", format!("filepath:{changelog}"), "sacho"),
            (
                "merge-tools",
                String::from("sacho.executable"),
                integration_executable,
            ),
            (
                "merge-tools",
                String::from("sacho.args"),
                "merge-driver $base $output $other $output",
            ),
            ("merge-tools", String::from("sacho.premerge"), "false"),
            ("merge-tools", String::from("sacho.priority"), "-100"),
        ]);
    }
    settings.extend([
        ("merge-patterns", format!("filepath:{next}"), ":local"),
        ("hooks", String::from("update.sacho"), hook_command.as_str()),
    ]);
    for (section, key, value) in settings {
        if let Some(existing) = hgrc_value(source, section, &key)
            && existing != value
        {
            return Err(format!(
                "[{section}] {key} already has incompatible value {existing:?}"
            ));
        }
    }
    Ok(())
}

fn hgrc_value<'a>(source: &'a str, wanted_section: &str, wanted_key: &str) -> Option<&'a str> {
    let mut section = "";
    let mut found = None;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
        } else if section == wanted_section
            && !line.starts_with(['#', ';'])
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == wanted_key
        {
            found = Some(value.trim());
        }
    }
    found
}

fn repository_preset(root: &Path) -> Option<VcsPreset> {
    if root.join(".jj").is_dir() {
        Some(VcsPreset::Jj)
    } else if root.join(".hg").is_dir() {
        Some(VcsPreset::Hg)
    } else if root.join(".git").exists() {
        Some(VcsPreset::Git)
    } else {
        None
    }
}

/// Applies the post-merge synchronization requested by Mercurial's update hook.
///
/// The hook is a no-op for ordinary updates and failed or unresolved merges.
pub fn mercurial_update_hook(
    repo: &Repository,
    parent2: Option<&str>,
    hook_error: Option<&str>,
) -> Result<bool> {
    if hook_error != Some("0") || parent2.is_none_or(|parent| parent.trim().is_empty()) {
        return Ok(false);
    }
    let plan = plan_sync(repo, SyncOptions { force: true })?;
    match plan {
        SyncPlan::Apply(plan) => {
            apply_sync(repo, SyncPlan::Apply(plan))?;
            Ok(true)
        }
        SyncPlan::Skipped(_) | SyncPlan::NeedsConfirmation { .. } => Ok(false),
    }
}

fn edit_gitattributes(source: &str, required: &[&str]) -> Result<String> {
    let mut output = source.to_owned();
    for required_line in required {
        let (path, expected_merge) = parse_required_attribute(required_line);
        let mut found = false;
        for line in source.lines() {
            let trimmed = line.trim();
            let Some((candidate_path, attributes)) = parse_gitattributes_line(trimmed) else {
                continue;
            };
            if candidate_path != path {
                continue;
            }
            found = true;
            if !attributes.contains(&expected_merge) {
                return Err(Error::InitConflict {
                    message: format!("{path} already has an incompatible merge attribute"),
                });
            }
        }
        if !found {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(required_line);
            output.push('\n');
        }
    }
    Ok(output)
}

fn parse_required_attribute(line: &str) -> (&str, &str) {
    let (path, attributes) =
        parse_gitattributes_line(line).expect("required attributes include a path");
    let merge = attributes
        .first()
        .copied()
        .expect("required attributes include merge attr");
    (path, merge)
}

fn parse_gitattributes_line(line: &str) -> Option<(&str, Vec<&str>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let path_end = gitattributes_path_end(line)?;
    let path = &line[..path_end];
    let attributes = line[path_end..].split_whitespace().collect::<Vec<_>>();
    Some((path, attributes))
}

fn gitattributes_path_end(line: &str) -> Option<usize> {
    if line.starts_with('"') {
        let mut escaped = false;
        for (index, character) in line.char_indices().skip(1) {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Some(index + character.len_utf8());
            }
        }
        None
    } else {
        line.find(char::is_whitespace).or(Some(line.len()))
    }
}

fn install_commit_hooks(
    root: &Path,
    integration_executable: &str,
    append_existing_hook: bool,
    result: &mut InitResult,
) -> Result<()> {
    let executable = shell_quote(integration_executable);
    install_git_hook(
        root,
        "pre-commit",
        &format!(
            "# sacho pre-commit begin\n{executable} hook-pre-commit\n# sacho pre-commit end\n"
        ),
        append_existing_hook,
        result,
    )?;
    install_git_hook(
        root,
        "commit-msg",
        &format!(
            "# sacho commit-msg begin\n{executable} hook-commit-msg \"$1\"\n# sacho commit-msg end\n"
        ),
        append_existing_hook,
        result,
    )?;
    install_git_hook(
        root,
        "reference-transaction",
        &format!(
            "# sacho reference-transaction begin\nif test \"$1\" = prepared\nthen\n    sacho_state=$(git rev-parse --git-path sacho-commit-state) || exit $?\n    if test -f \"$sacho_state\"\n    then\n        {executable} hook-reference-transaction \"$1\"\n    fi\nfi\n# sacho reference-transaction end\n"
        ),
        append_existing_hook,
        result,
    )
}

#[cfg(test)]
fn install_pre_commit_hook(
    root: &Path,
    append_existing_hook: bool,
    result: &mut InitResult,
) -> Result<()> {
    install_git_hook(
        root,
        "pre-commit",
        "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n",
        append_existing_hook,
        result,
    )
}

fn install_git_hook(
    root: &Path,
    hook_name: &str,
    block: &str,
    append_existing_hook: bool,
    result: &mut InitResult,
) -> Result<()> {
    let hook_path = git_hook_path(root, hook_name)?;
    let reported_path = report_path(root, &hook_path);
    let old = match fs::read_to_string(&hook_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = hook_path.parent() {
                fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&hook_path, format!("#!/bin/sh\n{block}")).map_err(|source| {
                Error::WriteFile {
                    path: hook_path.clone(),
                    source,
                }
            })?;
            make_executable(&hook_path)?;
            result.created_files.push(reported_path);
            return Ok(());
        }
        Err(source) => {
            return Err(Error::ReadFile {
                path: hook_path,
                source,
            });
        }
    };
    let edited = edit_marked_hook(&old, block, append_existing_hook).ok_or_else(|| {
        Error::HookNeedsManualInstall {
            path: reported_path.clone(),
        }
    })?;
    if old != edited {
        fs::write(&hook_path, edited).map_err(|source| Error::WriteFile {
            path: hook_path.clone(),
            source,
        })?;
        make_executable(&hook_path)?;
        result.modified_files.push(reported_path);
    } else {
        result.skipped_existing_files.push(reported_path);
    }
    Ok(())
}

fn git_hook_path(root: &Path, hook_name: &str) -> Result<PathBuf> {
    if let Ok(path) = git_output(root, ["config", "--path", "--get", "core.hooksPath"]) {
        let path = path.trim();
        if !path.is_empty() {
            return Ok(resolve_git_path(root, path).join(hook_name));
        }
    }

    let common_dir = git_output(root, ["rev-parse", "--git-common-dir"])?;
    Ok(resolve_git_path(root, common_dir.trim())
        .join("hooks")
        .join(hook_name))
}

fn resolve_git_path(root: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn report_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(|source| Error::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn edit_pre_commit_hook(source: &str, block: &str, append_existing: bool) -> Option<String> {
    edit_marked_hook(source, block, append_existing)
}

fn edit_marked_hook(source: &str, block: &str, append_existing: bool) -> Option<String> {
    let begin = block.lines().next()?;
    let end = block.lines().next_back()?;
    match (source.find(begin), source.find(end)) {
        (Some(begin_index), Some(end_index)) if begin_index <= end_index => {
            let end_index = end_index + end.len();
            let mut output = String::new();
            output.push_str(&source[..begin_index]);
            output.push_str(block.trim_end());
            output.push_str(&source[end_index..]);
            if !output.ends_with('\n') {
                output.push('\n');
            }
            Some(output)
        }
        (None, None) if append_existing => {
            let mut output = source.to_owned();
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(block);
            Some(output)
        }
        _ => None,
    }
}

fn set_git_config(root: &Path, key: &str, value: &str, result: &mut InitResult) -> Result<()> {
    if git_config_get(root, key).is_ok_and(|current| current.trim() == value) {
        return Ok(());
    }
    git(root, ["config", key, value])?;
    result.local_git_config_changes.push(key.to_owned());
    Ok(())
}

fn git_config_get(root: &Path, key: &str) -> Result<String> {
    git_output(root, ["config", "--get", key])
}

fn is_git_repository(root: &Path) -> bool {
    git(root, ["rev-parse", "--git-dir"]).is_ok()
}

fn git_attr_path(path: &Path) -> String {
    let path = path
        .iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    quote_git_attr_path(&path)
}

fn quote_git_attr_path(path: &str) -> String {
    if path.starts_with('#') || path.chars().any(char::is_whitespace) {
        let mut quoted = String::from("\"");
        for character in path.chars() {
            if matches!(character, '"' | '\\') {
                quoted.push('\\');
            }
            quoted.push(character);
        }
        quoted.push('"');
        quoted
    } else {
        path.to_owned()
    }
}

fn shell_quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character))
    {
        return argument.to_owned();
    }

    format!("'{}'", argument.replace('\'', "'\"'\"'"))
}

fn git<I, S>(root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_output(root, args).map(|_| ())
}

fn git_output<I, S>(root: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let command = command_line("git", &args);
    let output = Command::new("git")
        .args(&args)
        .current_dir(root)
        .output()
        .map_err(|source| Error::VcsCommandIo {
            command: command.clone(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::VcsCommandFailed {
            command,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn command_line<S>(program: &str, args: &[S]) -> String
where
    S: AsRef<OsStr>,
{
    let mut output = String::from(program);
    for arg in args {
        output.push(' ');
        output.push_str(&arg.as_ref().to_string_lossy());
    }
    output
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
    use std::io::{BufRead, BufReader, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::thread;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;
    use crate::FragmentError;
    use crate::vcs::{ChangeKind, CommitId};

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

    fn init_test_git_repository(root: &Path) {
        git(root, ["init"]).expect("git init");
        git(root, ["config", "commit.gpgSign", "false"]).expect("disable test commit signing");
        git(root, ["config", "tag.gpgSign", "false"]).expect("disable test tag signing");
    }

    fn repo_with_config(config: &str) -> (TempDir, Repository) {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), config).expect("config");
        let repo = Repository::from_root(temp.path()).expect("repo");
        (temp, repo)
    }

    struct TestHttpServer {
        base: String,
        address: SocketAddr,
        response_count: usize,
        handle: Option<thread::JoinHandle<Vec<String>>>,
    }

    impl TestHttpServer {
        fn spawn(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let address = listener.local_addr().expect("address");
            let base = format!("http://{address}");
            let response_count = responses.len();
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                for response in responses {
                    let (mut stream, _) = listener.accept().expect("connection");
                    requests.push(
                        BufReader::new(&stream)
                            .lines()
                            .next()
                            .expect("request line")
                            .expect("request contents"),
                    );
                    stream.write_all(response.as_bytes()).expect("response");
                }
                requests
            });
            Self {
                base,
                address,
                response_count,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> Vec<String> {
            wake_http_server(self.address, self.response_count);
            self.handle.take().expect("handle").join().expect("server")
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            let Some(handle) = self.handle.take() else {
                return;
            };
            wake_http_server(self.address, self.response_count);
            handle.join().expect("server");
        }
    }

    fn wake_http_server(address: SocketAddr, attempts: usize) {
        for _ in 0..attempts {
            let Ok(mut stream) = TcpStream::connect(address) else {
                break;
            };
            stream
                .write_all(b"HEAD /test-server-shutdown HTTP/1.1\r\n\r\n")
                .expect("shutdown request");
        }
    }

    fn http_response(status: &str, location: Option<&str>) -> String {
        let mut output = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n");
        if let Some(location) = location {
            output.push_str("Location: ");
            output.push_str(location);
            output.push_str("\r\n");
        }
        output.push_str("\r\n");
        output
    }

    fn release_date() -> ReleaseDate {
        ReleaseDate::parse("2026-07-08").expect("release date")
    }

    fn normalize_separators(value: impl AsRef<str>) -> String {
        value.as_ref().replace('\\', "/")
    }

    fn assert_no_release_artifacts(root: &Path) {
        fn collect(directory: &Path, artifacts: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(directory).expect("repository entries") {
                let entry = entry.expect("repository entry");
                let path = entry.path();
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".sacho-claim-")
                    || name.to_string_lossy().starts_with(".sacho-probe-")
                {
                    artifacts.push(path);
                } else if entry.file_type().expect("entry type").is_dir() {
                    collect(&path, artifacts);
                }
            }
        }

        let mut artifacts = Vec::new();
        collect(root, &mut artifacts);
        assert!(
            artifacts.is_empty(),
            "unexpected release transaction artifacts: {artifacts:?}"
        );
    }

    fn retained_release_claim_contents(root: &Path) -> Vec<String> {
        fn collect(directory: &Path, claims: &mut Vec<String>) {
            for entry in fs::read_dir(directory).expect("repository entries") {
                let entry = entry.expect("repository entry");
                let path = entry.path();
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".sacho-claim-") {
                    claims.push(fs::read_to_string(path.join("claimed")).expect("retained claim"));
                } else if entry.file_type().expect("entry type").is_dir() {
                    collect(&path, claims);
                }
            }
        }

        let mut claims = Vec::new();
        collect(root, &mut claims);
        claims.sort();
        claims
    }

    fn assert_mutation_locked<T>(result: Result<T>) {
        assert!(matches!(result, Err(Error::ReleaseLocked)));
    }

    #[test]
    fn init_config_omits_blank_repository_url() {
        let config = default_init_config(
            Path::new("project"),
            &InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: Some(String::from("   ")),
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("blank repository URL");

        assert!(config.links.is_empty());
        assert!(!render_init_config(&config).contains("[links]"));
    }

    #[test]
    fn init_config_renders_selected_sections() {
        let options = InitOptions {
            sections: vec![SectionConfig {
                id: String::from("👨‍👩‍👧 core"),
                directory: PathBuf::from("core"),
                paths: vec![String::from("packages/core/**")],
            }],
            ..InitOptions::default()
        };
        let config = default_init_config(Path::new("project"), &options).expect("init config");
        let rendered = render_init_config(&config);
        let reparsed = Config::parse(&rendered).expect("rendered config");

        assert_eq!(reparsed.sections, options.sections);
        assert!(rendered.contains("[[sections]]"));
        assert!(rendered.contains("id = \"👨‍👩‍👧 core\""));
        assert!(!rendered.contains("\\u{"));
        assert!(rendered.contains("directory = \"core\""));
        assert!(rendered.contains("paths = [\"packages/core/**\"]"));
    }

    #[test]
    fn init_config_renders_selected_section_patterns() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
        fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");
        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("pattern inference");
        let options = InitOptions {
            section_patterns: pattern.into_iter().collect(),
            ..InitOptions::default()
        };
        let config = default_init_config(temp.path(), &options).expect("init config");
        let rendered = render_init_config(&config);
        let reparsed = Config::parse(&rendered).expect("rendered config");

        assert_eq!(reparsed.section_patterns, options.section_patterns);
        assert!(rendered.contains("[[section-patterns]]"));
    }

    #[test]
    fn init_config_escapes_remaining_ascii_control_characters() {
        assert_eq!(toml_basic_string("\u{001f}\u{007f}"), "\"\\u001F\\u007F\"");
    }

    #[test]
    fn suggests_safe_unique_section_directories() {
        let mut used = Vec::new();

        let first = suggest_section_directory("@example/Core tools", &used);
        used.push(first.clone());
        let second = suggest_section_directory("core-tools", &used);

        assert_eq!(first, PathBuf::from("core-tools"));
        assert_eq!(second, PathBuf::from("core-tools-2"));
    }

    #[test]
    fn infers_section_paths_by_directory_name_and_skips_generated_trees() {
        let temp = TempDir::new().expect("tempdir");
        for path in [
            "packages/core",
            "examples/core",
            "target/core",
            "node_modules/core",
            ".git/core",
            "changes.d/core",
        ] {
            fs::create_dir_all(temp.path().join(path)).expect("directory");
        }

        let paths = infer_section_paths(
            temp.path(),
            "@example/core",
            Path::new("core"),
            Path::new("changes.d"),
        )
        .expect("path suggestions");

        assert_eq!(
            paths,
            vec![
                String::from("examples/core/**"),
                String::from("packages/core/**")
            ]
        );
    }

    #[test]
    fn section_path_inference_matches_distinct_id_and_directory_stems() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("id directory");
        fs::create_dir_all(temp.path().join("crates/cli")).expect("section directory");

        let paths = infer_section_paths(
            temp.path(),
            "@example/core",
            Path::new("cli"),
            Path::new("changes.d"),
        )
        .expect("path suggestions");

        assert_eq!(paths, vec!["crates/cli/**", "packages/core/**"]);
    }

    #[test]
    fn infers_a_section_pattern_from_selected_sibling_packages() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
        fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");

        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("pattern inference")
        .expect("one pattern");

        assert_eq!(pattern.source.to_string(), "packages/{name}");
        assert_eq!(pattern.id.to_string(), "@example/{name}");
        assert_eq!(pattern.directory.to_string(), "{name}");
        assert_eq!(pattern.paths, None);
    }

    #[test]
    fn infers_a_section_pattern_from_selected_root_directories() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join("core")).expect("core package");
        fs::create_dir(temp.path().join("cli")).expect("cli package");

        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("pattern inference")
        .expect("one pattern");

        assert_eq!(pattern.source.to_string(), "{name}");
        assert_eq!(pattern.id.to_string(), "@example/{name}");
        assert_eq!(pattern.directory.to_string(), "{name}");
        assert_eq!(pattern.paths, None);
    }

    #[test]
    fn section_pattern_inference_rejects_a_directory_overlapping_the_next_file() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
        fs::create_dir_all(temp.path().join("packages/next")).expect("next package");

        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/next".to_owned()],
            Path::new("changes.d"),
        )
        .expect("best-effort inference");

        assert_eq!(pattern, None);
    }

    #[cfg(unix)]
    #[test]
    fn section_pattern_inference_checks_the_exact_nondefault_fragment_directory() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
        fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");
        let fragments = temp.path().join("generated/changes");
        fs::create_dir_all(fragments.join("shared")).expect("shared fragments");
        symlink("shared", fragments.join("core")).expect("section alias");

        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/cli".to_owned()],
            Path::new("generated/changes"),
        )
        .expect("best-effort inference");

        assert_eq!(pattern, None);
    }

    #[test]
    fn does_not_infer_a_section_pattern_from_ambiguous_roots() {
        let temp = TempDir::new().expect("tempdir");
        for path in [
            "packages/core",
            "packages/cli",
            "examples/core",
            "examples/cli",
        ] {
            fs::create_dir_all(temp.path().join(path)).expect("package");
        }

        let pattern = infer_section_pattern(
            temp.path(),
            &["@example/core".to_owned(), "@example/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("pattern inference");

        assert_eq!(pattern, None);
    }

    #[test]
    fn section_pattern_inference_rejects_duplicates_and_escapes_literal_braces() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("groups/{legacy}/core")).expect("core package");
        fs::create_dir_all(temp.path().join("groups/{legacy}/cli")).expect("cli package");

        let duplicate = infer_section_pattern(
            temp.path(),
            &["team{old}/core".to_owned(), "team{old}/core".to_owned()],
            Path::new("changes.d"),
        )
        .expect("duplicate inference");
        let pattern = infer_section_pattern(
            temp.path(),
            &["team{old}/core".to_owned(), "team{old}/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("pattern inference")
        .expect("one pattern");

        assert_eq!(duplicate, None);
        assert_eq!(pattern.source.to_string(), "groups/{{legacy}}/{name}");
        assert_eq!(pattern.id.to_string(), "team{{old}}/{name}");
    }

    #[test]
    fn section_pattern_inference_falls_back_for_an_invalid_id_template() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
        fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");

        let pattern = infer_section_pattern(
            temp.path(),
            &["/packages/core".to_owned(), "/packages/cli".to_owned()],
            Path::new("changes.d"),
        )
        .expect("best-effort inference");

        assert_eq!(pattern, None);
    }

    #[test]
    fn section_path_inference_reports_an_unreadable_root() {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("not-a-directory");
        fs::write(&root, "file").expect("root file");

        let error = infer_section_paths(&root, "core", Path::new("core"), Path::new("changes.d"))
            .expect_err("root read error");

        assert!(matches!(error, Error::ReadFile { path, .. } if path == root));
    }

    #[cfg(unix)]
    #[test]
    fn section_path_inference_skips_unreadable_subdirectories() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("packages/core")).expect("package directory");
        let unreadable = temp.path().join("private");
        fs::create_dir(&unreadable).expect("private directory");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
            .expect("remove permissions");

        let result = infer_section_paths(
            temp.path(),
            "core",
            Path::new("core"),
            Path::new("changes.d"),
        );

        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700))
            .expect("restore permissions");
        assert_eq!(result.expect("best-effort scan"), vec!["packages/core/**"]);
    }

    #[test]
    fn colocated_jujutsu_repository_wins_over_git_detection() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join(".git")).expect("git marker");
        fs::create_dir(temp.path().join(".jj")).expect("jj marker");

        assert_eq!(repository_preset(temp.path()), Some(VcsPreset::Jj));
        assert_eq!(
            default_init_config(temp.path(), &InitOptions::default())
                .expect("default init config")
                .vcs
                .preset,
            VcsPreset::Jj
        );
    }

    #[test]
    fn repository_preset_requires_directories_for_nongit_markers() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join(".git"), "gitdir: elsewhere\n").expect("git marker");
        fs::write(temp.path().join(".jj"), "ordinary file\n").expect("jj file");
        fs::write(temp.path().join(".hg"), "ordinary file\n").expect("hg file");

        assert_eq!(repository_preset(temp.path()), Some(VcsPreset::Git));
    }

    #[test]
    fn non_git_mutation_locks_live_in_vcs_metadata() {
        let jj = TempDir::new().expect("jj tempdir");
        fs::create_dir(jj.path().join(".jj")).expect("jj marker");
        let jj_lock = acquire_mutation_lock_at_root(jj.path()).expect("jj lock");
        assert_eq!(jj_lock.path, jj.path().join(".jj/sacho.lock"));
        assert!(!jj.path().join(MUTATION_LOCK_FILE).exists());

        let hg = TempDir::new().expect("hg tempdir");
        fs::create_dir(hg.path().join(".hg")).expect("hg marker");
        let hg_lock = acquire_mutation_lock_at_root(hg.path()).expect("hg lock");
        assert_eq!(hg_lock.path, hg.path().join(".hg/sacho.lock"));
        assert!(!hg.path().join(MUTATION_LOCK_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn mutation_uses_the_resolved_path_after_a_configured_symlink_is_retargeted() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        fs::create_dir_all(temp.path().join("safe/changes.d")).expect("safe fragments");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\nmaterialize = false\n[fragments]\ndirectory = \"linked\"\n",
        )
        .expect("config");
        symlink("safe/changes.d", temp.path().join("linked")).expect("safe symlink");
        let repo = Repository::from_root(temp.path()).expect("repository");

        fs::remove_file(temp.path().join("linked")).expect("remove safe symlink");
        symlink(outside.path(), temp.path().join("linked")).expect("external symlink");

        add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("topic"),
            },
        )
        .expect("resolved fragment path");

        assert!(temp.path().join("safe/changes.d/topic.md").is_file());
        assert!(!outside.path().join("topic.md").exists());
        assert!(temp.path().join(MUTATION_LOCK_FILE).is_file());
    }

    #[test]
    fn init_discovers_the_marker_root_from_a_nested_directory() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join(".git")).expect("git marker");
        let nested = temp.path().join("one/two");
        fs::create_dir_all(&nested).expect("nested directory");

        assert_eq!(
            init_root(&nested),
            fs::canonicalize(temp.path()).expect("canonical repository root")
        );
    }

    #[test]
    fn init_ignores_nongit_marker_files_in_nested_directories() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join(".git")).expect("git marker");
        let nested = temp.path().join("one/two");
        fs::create_dir_all(&nested).expect("nested directory");
        fs::write(temp.path().join("one/.jj"), "ordinary file\n").expect("jj file");
        fs::write(nested.join(".hg"), "ordinary file\n").expect("hg file");

        assert_eq!(
            init_root(&nested),
            fs::canonicalize(temp.path()).expect("canonical repository root")
        );
    }

    #[test]
    fn init_normalizes_parent_components_before_discovering_markers() {
        let temp = TempDir::new().expect("tempdir");
        let sibling_a = temp.path().join("a");
        let sibling_b = temp.path().join("b");
        fs::create_dir_all(sibling_a.join(".git")).expect("Git marker");
        fs::create_dir(&sibling_b).expect("sibling directory");

        assert_eq!(
            init_root(&sibling_a.join("../b")),
            fs::canonicalize(sibling_b).expect("canonical sibling")
        );
    }

    #[test]
    fn init_installs_idempotent_mercurial_merge_integration() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join(".hg")).expect("hg marker");

        let options = InitOptions {
            integration_executable: Some(PathBuf::from("sacho")),
            ..InitOptions::default()
        };
        let first = init_repository(temp.path(), options.clone()).expect("first init");
        let second = init_repository(temp.path(), options).expect("second init");
        let hgrc = fs::read_to_string(temp.path().join(".hg/hgrc")).expect("hgrc");

        assert_eq!(hgrc.matches(HG_INTEGRATION_BEGIN).count(), 1);
        assert!(hgrc.contains("filepath:CHANGES.md = sacho"));
        assert!(hgrc.contains("filepath:changes.d/next = :local"));
        assert!(hgrc.contains("update.sacho = sacho hook-hg-update"));
        assert!(!first.local_hg_config_changes.is_empty());
        assert!(second.local_hg_config_changes.is_empty());
    }

    #[test]
    fn init_uses_the_current_executable_for_git_integration() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());

        init_repository(temp.path(), InitOptions::default()).expect("init");

        let driver = git_config_get(temp.path(), "merge.sacho.driver").expect("merge driver");
        let executable = std::env::current_exe().expect("current executable");
        assert!(
            driver.contains(&executable.to_string_lossy().into_owned()),
            "driver {driver:?} does not contain {executable:?}"
        );
    }

    #[test]
    fn init_uses_an_explicit_executable_for_git_integration_and_hooks() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());

        init_repository(
            temp.path(),
            InitOptions {
                integration_executable: Some(PathBuf::from("tools/Sacho driver")),
                install_hook: true,
                ..InitOptions::default()
            },
        )
        .expect("init");

        assert_eq!(
            git_config_get(temp.path(), "merge.sacho.driver")
                .expect("merge driver")
                .trim(),
            "'tools/Sacho driver' merge-driver %O %A %B %P"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join(".git/hooks/pre-commit")).expect("pre-commit hook"),
            "#!/bin/sh\n# sacho pre-commit begin\n'tools/Sacho driver' hook-pre-commit\n# sacho pre-commit end\n"
        );
        assert!(
            fs::read_to_string(temp.path().join(".git/hooks/commit-msg"))
                .expect("commit-msg hook")
                .contains("'tools/Sacho driver' hook-commit-msg \"$1\"")
        );
        assert!(
            fs::read_to_string(temp.path().join(".git/hooks/reference-transaction"))
                .expect("reference-transaction hook")
                .contains("'tools/Sacho driver' hook-reference-transaction \"$1\"")
        );
    }

    #[test]
    fn integration_executable_rejects_empty_and_multiline_paths() {
        for path in ["", " tools/sacho", "tools/sacho ", "tools/sacho\nother"] {
            let error = resolve_integration_executable(&InitOptions {
                integration_executable: Some(PathBuf::from(path)),
                ..InitOptions::default()
            })
            .expect_err("invalid integration executable");

            assert!(matches!(error, Error::InvalidIntegrationExecutable { .. }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn integration_executable_rejects_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let error = resolve_integration_executable(&InitOptions {
            integration_executable: Some(PathBuf::from(OsString::from_vec(vec![0x80]))),
            ..InitOptions::default()
        })
        .expect_err("non-UTF-8 integration executable");

        assert!(matches!(error, Error::InvalidIntegrationExecutable { .. }));
    }

    #[test]
    fn init_skips_the_mercurial_changelog_driver_in_fragments_only_mode() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join(".hg")).expect("hg marker");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\nmaterialize = false\n\n[vcs]\npreset = \"hg\"\n",
        )
        .expect("config");

        let result = init_repository(
            temp.path(),
            InitOptions {
                integration_executable: Some(PathBuf::from("sacho")),
                ..InitOptions::default()
            },
        )
        .expect("init");
        let hgrc = fs::read_to_string(temp.path().join(".hg/hgrc")).expect("hgrc");

        assert!(!hgrc.contains("filepath:CHANGES.md = sacho"));
        assert!(!hgrc.contains("[merge-tools]"));
        assert!(!hgrc.contains("sacho.executable"));
        assert!(hgrc.contains("filepath:changes.d/next = :local"));
        assert!(hgrc.contains("update.sacho = sacho hook-hg-update"));
        assert_eq!(
            result.local_hg_config_changes,
            vec![
                String::from("merge-patterns.filepath:changes.d/next"),
                String::from("hooks.update.sacho"),
            ]
        );
    }

    #[test]
    fn mercurial_integration_preserves_conflicting_unmarked_config() {
        let source = "[hooks]\nupdate.sacho = other-command\n";
        let block = format!(
            "{HG_INTEGRATION_BEGIN}\n[hooks]\nupdate.sacho = sacho hook-hg-update\n{HG_INTEGRATION_END}\n"
        );

        let error = edit_hgrc(
            source,
            &block,
            Some("CHANGES.md"),
            "changes.d/next",
            "sacho",
        )
        .expect_err("conflicting hook must not be overwritten");

        assert!(error.contains("incompatible value"));
        assert_eq!(source, "[hooks]\nupdate.sacho = other-command\n");
    }

    #[test]
    fn mercurial_integration_rejects_incompatible_merge_tool_controls() {
        let block = format!(
            "{HG_INTEGRATION_BEGIN}\n[merge-tools]\nsacho.premerge = false\nsacho.priority = -100\n{HG_INTEGRATION_END}\n"
        );
        for (key, value) in [("sacho.premerge", "true"), ("sacho.priority", "0")] {
            let source = format!("[merge-tools]\n{key} = {value}\n");

            let error = edit_hgrc(
                &source,
                &block,
                Some("CHANGES.md"),
                "changes.d/next",
                "sacho",
            )
            .expect_err("incompatible merge-tool setting must not be overwritten");

            assert!(error.contains(key), "{error}");
            assert!(error.contains("incompatible value"), "{error}");
        }
    }

    #[test]
    fn mercurial_integration_rejects_effective_settings_after_its_block() {
        let block = format!(
            "{HG_INTEGRATION_BEGIN}\n[hooks]\nupdate.sacho = sacho hook-hg-update\n{HG_INTEGRATION_END}\n"
        );
        let source = format!("{block}update.sacho = false\n");

        let error = edit_hgrc(
            &source,
            &block,
            Some("CHANGES.md"),
            "changes.d/next",
            "sacho",
        )
        .expect_err("later conflicting hook must remain visible");

        assert!(error.contains("update.sacho"), "{error}");
        assert!(error.contains("incompatible value"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn mercurial_config_paths_preserve_literal_backslashes() {
        assert_eq!(
            hg_config_path(Path::new(r"changes\directory/next")),
            Some(String::from(r"changes\directory/next"))
        );
    }

    #[test]
    fn mercurial_config_paths_reject_trailing_whitespace() {
        for path in ["CHANGES.md ", "changes.d/next\t"] {
            assert_eq!(hg_config_path(Path::new(path)), None, "{path:?}");
        }
    }

    #[test]
    fn hgrc_edit_rejects_missing_and_reversed_markers() {
        let block = format!("{HG_INTEGRATION_BEGIN}\nvalue\n{HG_INTEGRATION_END}\n");
        for source in [
            format!("{HG_INTEGRATION_BEGIN}\n"),
            format!("{HG_INTEGRATION_END}\n"),
            format!("{HG_INTEGRATION_END}\n{HG_INTEGRATION_BEGIN}\n"),
        ] {
            assert_eq!(
                edit_hgrc(
                    &source,
                    &block,
                    Some("CHANGES.md"),
                    "changes.d/next",
                    "sacho",
                ),
                Err(String::from("existing Sacho marker block is malformed"))
            );
        }
    }

    #[test]
    fn hgrc_edit_appends_with_canonical_spacing() {
        let block = format!("{HG_INTEGRATION_BEGIN}\nvalue\n{HG_INTEGRATION_END}\n");

        assert_eq!(
            edit_hgrc("", &block, Some("CHANGES.md"), "changes.d/next", "sacho",),
            Ok(block.clone())
        );
        assert_eq!(
            edit_hgrc(
                "[ui]\n",
                &block,
                Some("CHANGES.md"),
                "changes.d/next",
                "sacho",
            ),
            Ok(format!("[ui]\n\n{block}"))
        );
        assert_eq!(
            edit_hgrc(
                "[ui]",
                &block,
                Some("CHANGES.md"),
                "changes.d/next",
                "sacho",
            ),
            Ok(format!("[ui]\n\n{block}"))
        );
    }

    #[test]
    fn hgrc_edit_replaces_an_existing_marked_block() {
        let source =
            format!("[ui]\n{HG_INTEGRATION_BEGIN}\nold\n{HG_INTEGRATION_END}\n[extensions]\n");
        let block = format!("{HG_INTEGRATION_BEGIN}\nnew\n{HG_INTEGRATION_END}\n");

        assert_eq!(
            edit_hgrc(
                &source,
                &block,
                Some("CHANGES.md"),
                "changes.d/next",
                "sacho",
            ),
            Ok(format!("[ui]\n{block}[extensions]\n"))
        );
    }

    #[test]
    fn hgrc_value_requires_complete_section_headers_and_exact_keys() {
        assert_eq!(
            hgrc_value(
                "[hooksX\nupdate.sacho = wrong\n[hooks]\n# update.sacho = commented\n; update.sacho = commented\nupdate.other = other\nupdate.sacho = expected\n",
                "hooks",
                "update.sacho",
            ),
            Some("expected")
        );
        assert_eq!(
            hgrc_value("[hooksX\nupdate.sacho = wrong\n", "hooks", "update.sacho",),
            None
        );
    }

    #[test]
    fn mercurial_update_hook_syncs_only_successful_merges() {
        let (temp, repo) =
            repo_with_config("[vcs]\npreset = \"hg\"\n[changelog]\nmaterialize = true\n");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(
            temp.path().join("changes.d/feature.md"),
            " -  Added a feature.\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            initial_materialized_changelog(repo.config()),
        )
        .expect("initial changelog");

        assert!(!mercurial_update_hook(&repo, None, Some("0")).expect("ordinary update"));
        assert!(!mercurial_update_hook(&repo, Some("other"), Some("1")).expect("failed merge"));
        assert!(mercurial_update_hook(&repo, Some("other"), Some("0")).expect("merge sync"));
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("synced changelog")
                .contains("Added a feature.")
        );
    }

    #[test]
    fn init_creates_configured_changelog_path() {
        let temp = TempDir::new().expect("tempdir");

        let result = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(PathBuf::from("docs/changes.md")),
                fragment_directory: None,
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        assert!(
            result
                .created_files
                .contains(&PathBuf::from("docs/changes.md"))
        );
        assert!(
            fs::read_to_string(temp.path().join("docs/changes.md"))
                .expect("changelog")
                .contains("To be released.")
        );
    }

    #[test]
    fn init_acquires_the_repository_lock_before_creating_configuration() {
        let temp = TempDir::new().expect("tempdir");
        let _lock = acquire_mutation_lock_at_root(temp.path()).expect("bootstrap lock");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(PathBuf::from("docs/changes.md")),
                fragment_directory: Some(PathBuf::from("fragments")),
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("concurrent init lock");

        assert!(matches!(error, Error::ReleaseLocked));
        assert!(!temp.path().join("sacho.toml").exists());
        assert!(!temp.path().join("docs/changes.md").exists());
        assert!(!temp.path().join("fragments").exists());
    }

    #[test]
    fn init_rejects_the_mutation_lock_as_the_changelog_path() {
        let temp = TempDir::new().expect("tempdir");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(PathBuf::from(MUTATION_LOCK_FILE)),
                fragment_directory: Some(PathBuf::from("fragments")),
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("mutation lock changelog");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
        assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
        assert!(!temp.path().join("fragments").exists());
        assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
    }

    #[test]
    fn init_rejects_the_git_mutation_lock_as_the_changelog_path() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());
        let lock_path = PathBuf::from(".git/sacho.lock");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(lock_path.clone()),
                fragment_directory: Some(PathBuf::from("fragments")),
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("Git mutation lock changelog");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
        assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
        assert!(!temp.path().join("fragments").exists());
        assert!(!temp.path().join(lock_path).exists());
    }

    #[test]
    fn init_rejects_fragment_directories_that_would_contain_a_future_vcs_lock() {
        for marker in [".git", ".jj", ".hg"] {
            let temp = TempDir::new().expect("tempdir");

            let error = init_repository(
                temp.path(),
                InitOptions {
                    changelog_path: None,
                    fragment_directory: Some(PathBuf::from(marker)),
                    materialize: None,
                    integration_executable: None,
                    install_hook: false,
                    append_existing_hook: false,
                    repository_url: None,
                    sections: Vec::new(),
                    section_patterns: Vec::new(),
                },
            )
            .expect_err("future VCS lock parent");

            assert!(matches!(
                error,
                Error::Config { source, .. }
                    if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
            ));
            assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
            assert!(!temp.path().join(marker).exists());
            assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
        }
    }

    #[test]
    fn init_rejects_inactive_git_lock_parent_in_a_colocated_repository() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());
        fs::create_dir(temp.path().join(".jj")).expect("Jujutsu metadata directory");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: Some(PathBuf::from(".git")),
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("inactive Git lock parent");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
        assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
        assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
        assert!(!temp.path().join(".jj/sacho.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn init_uses_resolved_paths_for_git_merge_attributes() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());
        fs::create_dir(temp.path().join("docs")).expect("docs directory");
        fs::write(
            temp.path().join("docs/CHANGES.md"),
            "Changelog\n=========\n",
        )
        .expect("changelog");
        symlink("docs/CHANGES.md", temp.path().join("alias.md")).expect("changelog alias");
        fs::write(
            temp.path().join(Repository::CONFIG_FILE),
            "[changelog]\npath = \"alias.md\"\n",
        )
        .expect("config");

        init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        let attributes =
            fs::read_to_string(temp.path().join(".gitattributes")).expect("Git attributes");
        assert!(attributes.contains("docs/CHANGES.md merge=sacho"));
        assert!(!attributes.contains("alias.md merge=sacho"));
        assert!(
            fs::read_to_string(temp.path().join(Repository::CONFIG_FILE))
                .expect("config")
                .contains("path = \"alias.md\"")
        );
    }

    #[test]
    fn init_rejects_the_configuration_file_as_the_changelog_path() {
        let temp = TempDir::new().expect("tempdir");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(PathBuf::from(Repository::CONFIG_FILE)),
                fragment_directory: Some(PathBuf::from("fragments")),
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("configuration changelog alias");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
        assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
        assert!(!temp.path().join("fragments").exists());
    }

    #[cfg(unix)]
    #[test]
    fn init_does_not_overwrite_configuration_through_a_changelog_symlink_alias() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let alias = PathBuf::from("SACHO.TOML");
        symlink(Repository::CONFIG_FILE, temp.path().join(&alias)).expect("dangling alias");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(alias.clone()),
                fragment_directory: Some(PathBuf::from("fragments")),
                materialize: Some(true),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("configuration changelog alias");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathResolution { .. })
        ));
        assert!(!temp.path().join(Repository::CONFIG_FILE).exists());
        assert_eq!(
            fs::read_link(temp.path().join(alias)).expect("changelog alias"),
            PathBuf::from(Repository::CONFIG_FILE)
        );
    }

    #[test]
    fn initial_changelog_creation_preserves_a_concurrently_created_file() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join(Repository::CONFIG_FILE);
        let changelog_path = temp.path().join("CHANGES.md");
        let config = Config::parse("").expect("default config");
        fs::write(&config_path, render_init_config(&config)).expect("configuration");
        fs::write(&changelog_path, "Concurrent changelog.\n").expect("concurrent changelog");

        let created = create_initial_changelog_if_absent(
            &config_path,
            &changelog_path,
            &config.changelog.path,
            &config,
        )
        .expect("preserve concurrent changelog");

        assert!(!created);
        assert_eq!(
            fs::read_to_string(changelog_path).expect("changelog"),
            "Concurrent changelog.\n"
        );
    }

    #[test]
    fn mutation_lock_creation_race_matches_only_an_existing_lock_file() {
        assert!(mutation_lock_was_created_concurrently(
            &std::io::Error::from(ErrorKind::AlreadyExists)
        ));
        assert!(!mutation_lock_was_created_concurrently(
            &std::io::Error::from(ErrorKind::PermissionDenied)
        ));
    }

    #[test]
    fn init_reports_manual_hook_action_for_non_git_preset() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\nmaterialize = false\n\n[vcs]\npreset = \"none\"\n",
        )
        .expect("config");

        let result = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: true,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        assert_eq!(result.manual_actions_required.len(), 1);
        assert!(result.manual_actions_required[0].contains("vcs.preset = \"git\""));
    }

    #[test]
    fn init_reports_manual_hook_action_outside_git_repository() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\nmaterialize = false\n",
        )
        .expect("config");

        let result = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: true,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        assert_eq!(result.manual_actions_required.len(), 1);
        assert!(result.manual_actions_required[0].contains("requires a Git repository"));
    }

    #[test]
    fn init_non_materialized_changelog_has_no_unreleased_region() {
        let temp = TempDir::new().expect("tempdir");

        init_repository(
            temp.path(),
            InitOptions {
                changelog_path: Some(PathBuf::from("CHANGES.md")),
                fragment_directory: None,
                materialize: Some(false),
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains("changelog"));
        assert!(!changelog.contains("Unreleased"));
        assert!(!changelog.contains("To be released."));
    }

    #[test]
    fn init_marker_mode_changelog_contains_markers() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            region-detection = "marker"
            "#,
        );
        init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect("init");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains("<!-- sacho:unreleased:begin -->"));
        assert!(changelog.contains("<!-- sacho:unreleased:end -->"));
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn init_rejects_fragment_directory_path_that_is_not_directory() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("changes.d"), "not a directory\n").expect("file");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("fragment path conflict");

        assert!(matches!(error, Error::InitConflict { .. }));
        assert!(error.to_string().contains("not a directory"));
    }

    #[test]
    fn init_rejects_changelog_path_that_is_not_file() {
        let temp = TempDir::new().expect("tempdir");
        fs::create_dir(temp.path().join("CHANGES.md")).expect("directory");

        let error = init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        )
        .expect_err("changelog path conflict");

        assert!(matches!(error, Error::InitConflict { .. }));
        assert!(error.to_string().contains("not a file"));
    }

    #[test]
    fn gitattributes_edit_preserves_unrelated_lines() {
        let edited = edit_gitattributes(
            "*.md text\n",
            &["CHANGES.md merge=sacho", "changes.d/next merge=ours"],
        )
        .expect("edit");

        assert_eq!(
            edited,
            "*.md text\nCHANGES.md merge=sacho\nchanges.d/next merge=ours\n"
        );
    }

    #[test]
    fn gitattributes_quotes_paths_with_spaces() {
        let edited = edit_gitattributes(
            "",
            &[
                r#""docs/change log.md" merge=sacho"#,
                r#""changes dir/next" merge=ours"#,
            ],
        )
        .expect("edit");

        assert_eq!(
            edited,
            "\"docs/change log.md\" merge=sacho\n\"changes dir/next\" merge=ours\n"
        );
        assert_eq!(
            quote_git_attr_path("docs/change log.md"),
            "\"docs/change log.md\""
        );
    }

    #[test]
    fn shell_arguments_are_quoted_only_when_needed() {
        assert_eq!(shell_quote("tools/sacho-1"), "tools/sacho-1");
        assert_eq!(shell_quote("tools/Sacho driver"), "'tools/Sacho driver'");
        assert_eq!(
            shell_quote("tools/Sacho's driver"),
            "'tools/Sacho'\"'\"'s driver'"
        );
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn gitattributes_quotes_paths_that_would_be_comments() {
        let edited = edit_gitattributes("", &[r##""#changes.md" merge=sacho"##]).expect("edit");

        assert_eq!(edited, "\"#changes.md\" merge=sacho\n");
        assert_eq!(quote_git_attr_path("#changes.md"), "\"#changes.md\"");
    }

    #[test]
    fn gitattributes_parser_ignores_empty_and_comment_lines() {
        assert_eq!(parse_gitattributes_line(""), None);
        assert_eq!(parse_gitattributes_line("   "), None);
        assert_eq!(parse_gitattributes_line("# CHANGES.md merge=union"), None);
    }

    #[test]
    fn gitattributes_detects_existing_quoted_conflict() {
        let error = edit_gitattributes(
            r#""docs/change log.md" merge=union
"#,
            &[r#""docs/change log.md" merge=sacho"#],
        )
        .expect_err("conflict");

        assert!(matches!(error, Error::InitConflict { .. }));
    }

    #[test]
    fn hook_marker_replacement_preserves_unrelated_content() {
        let source = "#!/bin/sh\necho before\n# sacho pre-commit begin\nold\n# sacho pre-commit end\necho after\n";
        let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";

        let edited = edit_pre_commit_hook(source, block, false).expect("edit");

        assert_eq!(
            edited,
            "#!/bin/sh\necho before\n# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\necho after\n"
        );
    }

    #[test]
    fn hook_edit_rejects_unmarked_hook_when_append_is_false() {
        let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";

        assert_eq!(edit_pre_commit_hook("#!/bin/sh\n", block, false), None);
    }

    #[test]
    fn hook_edit_rejects_reversed_marker_order() {
        let source = "# sacho pre-commit end\nold\n# sacho pre-commit begin\n";
        let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";

        assert_eq!(edit_pre_commit_hook(source, block, false), None);
    }

    #[test]
    fn hook_append_separates_block_from_existing_content() {
        let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";

        let edited = edit_pre_commit_hook("#!/bin/sh", block, true).expect("append");

        assert_eq!(
            edited,
            "#!/bin/sh\n# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n"
        );
    }

    #[test]
    fn commit_hook_state_rejects_malformed_content() {
        for value in [
            "",
            "head -\n",
            "tree abc\n",
            "head -\ntree \n",
            "head -\ntree abc\nextra\n",
        ] {
            assert!(CommitHookState::decode(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn reference_transaction_matches_the_armed_commit() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());
        git(temp.path(), ["config", "user.email", "test@example.com"]).expect("configure email");
        git(temp.path(), ["config", "user.name", "Test User"]).expect("configure name");
        fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        git(temp.path(), ["add", "README.md"]).expect("stage readme");
        git(temp.path(), ["commit", "-m", "Initial"]).expect("initial commit");
        let vcs = GitVcs::new(temp.path());
        let head = vcs.head().expect("head lookup").expect("head");
        let tree = vcs.index_tree().expect("index tree");
        let commit = git_output(
            temp.path(),
            ["commit-tree", &tree, "-p", &head, "-m", "Candidate"],
        )
        .expect("candidate commit");
        let commit = commit.trim();
        let state = CommitHookState {
            head: Some(head.clone()),
            tree,
        };

        let selected = reference_transaction_commit(
            &vcs,
            &state,
            &format!("{head} {commit} refs/heads/main\n"),
        )
        .expect("select commit");

        assert_eq!(selected.as_deref(), Some(commit));
        assert_eq!(
            reference_transaction_commit(&vcs, &state, &format!("{head} {commit} HEAD\n"))
                .expect("select detached HEAD commit")
                .as_deref(),
            Some(commit)
        );
        assert_eq!(
            reference_transaction_commit(
                &vcs,
                &state,
                &format!("{} {commit} refs/heads/main\n", "0".repeat(head.len())),
            )
            .expect("ignore other update"),
            None
        );
        assert_eq!(
            reference_transaction_commit(&vcs, &state, &format!("{head} {commit} refs/tags/v1\n"),)
                .expect("ignore tag update"),
            None
        );
    }

    #[test]
    fn reference_transaction_ignores_non_prepared_phases_without_consuming_state() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        init_test_git_repository(temp.path());
        git(temp.path(), ["add", "sacho.toml"]).expect("stage config");
        commit_message_hook(&repo, Path::new("COMMIT_EDITMSG")).expect("arm hook");
        let state_path = commit_hook_state_path(repo.root()).expect("state path");

        let report =
            reference_transaction_hook(&repo, "preparing", "").expect("ignore preparing phase");

        assert_eq!(report, None);
        assert!(state_path.is_file());
    }

    #[test]
    fn reference_transaction_propagates_state_read_errors() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        init_test_git_repository(temp.path());
        let state_path = commit_hook_state_path(repo.root()).expect("state path");
        fs::create_dir(&state_path).expect("state directory");

        let error =
            reference_transaction_hook(&repo, "prepared", "").expect_err("state read should fail");

        assert!(matches!(error, Error::ReadFile { path, .. } if path == state_path));
    }

    #[test]
    fn unborn_head_matches_only_a_nonempty_zero_object_id() {
        assert!(matches_commit_head(
            "0000000000000000000000000000000000000000",
            None
        ));
        assert!(!matches_commit_head("", None));
        assert!(!matches_commit_head(
            "1000000000000000000000000000000000000000",
            None
        ));
    }

    #[test]
    fn hook_install_updates_existing_marker_block() {
        let temp = TempDir::new().expect("tempdir");
        init_test_git_repository(temp.path());
        fs::write(
            temp.path().join(".git/hooks/pre-commit"),
            "#!/bin/sh\n# sacho pre-commit begin\nold\n# sacho pre-commit end\n",
        )
        .expect("hook");
        let mut result = InitResult::default();

        install_pre_commit_hook(temp.path(), false, &mut result).expect("install");

        assert_eq!(
            fs::read_to_string(temp.path().join(".git/hooks/pre-commit")).expect("hook"),
            "#!/bin/sh\n# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n"
        );
        assert_eq!(
            result.modified_files,
            vec![PathBuf::from(".git/hooks/pre-commit")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn make_executable_preserves_existing_executable_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("hook");
        fs::write(&path, "#!/bin/sh\n").expect("hook");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("permissions");

        make_executable(&path).expect("executable");

        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o755
        );
    }

    proptest! {
        #[test]
        fn gitattributes_edit_is_idempotent(prefix in "([A-Za-z0-9_.*-]+ text\n){0,5}") {
            let once = edit_gitattributes(
                &prefix,
                &["CHANGES.md merge=sacho", "changes.d/next merge=ours"],
            ).expect("first edit");
            let twice = edit_gitattributes(
                &once,
                &["CHANGES.md merge=sacho", "changes.d/next merge=ours"],
            ).expect("second edit");

            prop_assert_eq!(once, twice);
        }

        #[test]
        fn hook_marker_replacement_is_idempotent(prefix in "([A-Za-z0-9_ -]+\n){0,5}", suffix in "([A-Za-z0-9_ -]+\n){0,5}") {
            let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";
            let source = format!("{prefix}# sacho pre-commit begin\nold command\n# sacho pre-commit end\n{suffix}");
            let once = edit_pre_commit_hook(&source, block, false).expect("first edit");
            let twice = edit_pre_commit_hook(&once, block, false).expect("second edit");

            prop_assert_eq!(once, twice);
        }

        #[test]
        fn hook_append_preserves_existing_content(source in "([A-Za-z0-9_ -]+\n){1,5}") {
            let block = "# sacho pre-commit begin\nsacho hook-pre-commit\n# sacho pre-commit end\n";
            let edited = edit_pre_commit_hook(&source, block, true).expect("append");

            prop_assert!(edited.starts_with(&source));
            prop_assert!(edited.contains(block));
        }

        #[test]
        fn commit_hook_state_round_trips(
            head in prop::option::of("[0-9a-f]{40}"),
            tree in "[0-9a-f]{40}",
        ) {
            let state = CommitHookState { head, tree };

            prop_assert_eq!(CommitHookState::decode(&state.encode()).expect("decode"), state);
        }

        #[test]
        fn mutation_failure_at_any_apply_position_restores_every_target(
            target_count in 1usize..7,
            failure_seed in any::<usize>(),
        ) {
            let failure_index = failure_seed % target_count;
            let (temp, repo) = repo_with_config(
                "[changelog]\nmaterialize = false\n",
            );
            fs::create_dir_all(temp.path().join("targets")).expect("targets dir");
            let participants = (0..target_count)
                .map(|index| {
                    let path = PathBuf::from(format!("targets/{index}.txt"));
                    fs::write(repo.resolve(&path), format!("before {index}\n"))
                        .expect("target");
                    ReleaseFileChange {
                        path,
                        before: ReleaseFileState::Present(format!("before {index}\n")),
                        after: ReleaseFileState::Present(format!("after {index}\n")),
                    }
                })
                .collect::<Vec<_>>();
            let failure_path = participants[failure_index].path.clone();
            let plan = RepositoryMutationPlan {
                command: MutationCommand::Format,
                participants,
                fragment_paths_before: Vec::new(),
                fragment_paths_after: Vec::new(),
            };
            let _lock = acquire_mutation_lock(&repo).expect("mutation lock");
            set_release_failpoint(ReleaseApplyStage::Apply, failure_path);

            let error = apply_repository_mutation(&repo, &plan)
                .expect_err("injected apply failure");
            clear_release_failpoint();

            let rolled_back = matches!(
                error,
                Error::MutationApply {
                    command: MutationCommand::Format,
                    rollback_failures,
                    ..
                } if rollback_failures.is_empty()
            );
            prop_assert!(rolled_back);
            for index in 0..target_count {
                prop_assert_eq!(
                    fs::read_to_string(temp.path().join(format!("targets/{index}.txt")))
                        .expect("restored target"),
                    format!("before {index}\n"),
                );
            }
            assert_no_release_artifacts(temp.path());
        }
    }

    #[test]
    fn mutation_prepare_failure_removes_shared_new_directories() {
        let (temp, repo) = repo_with_config("[changelog]\nmaterialize = false\n");
        let participants = ["nested/deep/first.txt", "nested/second.txt", "failure.txt"]
            .into_iter()
            .map(|path| ReleaseFileChange {
                path: PathBuf::from(path),
                before: ReleaseFileState::Missing,
                after: ReleaseFileState::Present(format!("{path}\n")),
            })
            .collect();
        let plan = RepositoryMutationPlan {
            command: MutationCommand::Carry,
            participants,
            fragment_paths_before: Vec::new(),
            fragment_paths_after: Vec::new(),
        };
        let _lock = acquire_mutation_lock(&repo).expect("mutation lock");
        set_release_failpoint(ReleaseApplyStage::Prepare, PathBuf::from("failure.txt"));

        let error = apply_repository_mutation(&repo, &plan).expect_err("prepare failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::WriteFile { .. }));
        assert!(!temp.path().join("nested").exists());
        assert!(!temp.path().join("failure.txt").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn mutation_apply_failure_discards_remaining_prepared_writes_before_rollback() {
        let (temp, repo) = repo_with_config("[changelog]\nmaterialize = false\n");
        let participants = [
            "nested/deep/first.txt",
            "nested/second.txt",
            "remaining.txt",
        ]
        .into_iter()
        .map(|path| ReleaseFileChange {
            path: PathBuf::from(path),
            before: ReleaseFileState::Missing,
            after: ReleaseFileState::Present(format!("{path}\n")),
        })
        .collect();
        let plan = RepositoryMutationPlan {
            command: MutationCommand::Carry,
            participants,
            fragment_paths_before: Vec::new(),
            fragment_paths_after: Vec::new(),
        };
        let _lock = acquire_mutation_lock(&repo).expect("mutation lock");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("nested/second.txt"));

        let error = apply_repository_mutation(&repo, &plan).expect_err("apply failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert!(!temp.path().join("nested").exists());
        assert!(!temp.path().join("remaining.txt").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn mutation_apply_failure_removes_current_descendants_before_rollback() {
        let (temp, repo) = repo_with_config("[changelog]\nmaterialize = false\n");
        let participants = [
            "nested/first.txt",
            "nested/deep/second.txt",
            "remaining.txt",
        ]
        .into_iter()
        .map(|path| ReleaseFileChange {
            path: PathBuf::from(path),
            before: ReleaseFileState::Missing,
            after: ReleaseFileState::Present(format!("{path}\n")),
        })
        .collect();
        let plan = RepositoryMutationPlan {
            command: MutationCommand::Carry,
            participants,
            fragment_paths_before: Vec::new(),
            fragment_paths_after: Vec::new(),
        };
        let _lock = acquire_mutation_lock(&repo).expect("mutation lock");
        set_release_failpoint(
            ReleaseApplyStage::Apply,
            PathBuf::from("nested/deep/second.txt"),
        );

        let error = apply_repository_mutation(&repo, &plan).expect_err("apply failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert!(!temp.path().join("nested").exists());
        assert!(!temp.path().join("remaining.txt").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn first_mutation_apply_failure_retries_shared_directory_cleanup() {
        let (temp, repo) = repo_with_config("[changelog]\nmaterialize = false\n");
        let participants = [
            "nested/deep/first.txt",
            "nested/second.txt",
            "remaining.txt",
        ]
        .into_iter()
        .map(|path| ReleaseFileChange {
            path: PathBuf::from(path),
            before: ReleaseFileState::Missing,
            after: ReleaseFileState::Present(format!("{path}\n")),
        })
        .collect();
        let plan = RepositoryMutationPlan {
            command: MutationCommand::Carry,
            participants,
            fragment_paths_before: Vec::new(),
            fragment_paths_after: Vec::new(),
        };
        let _lock = acquire_mutation_lock(&repo).expect("mutation lock");
        set_release_failpoint(
            ReleaseApplyStage::Apply,
            PathBuf::from("nested/deep/first.txt"),
        );

        let error = apply_repository_mutation(&repo, &plan).expect_err("first apply failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert!(!temp.path().join("nested").exists());
        assert!(!temp.path().join("remaining.txt").exists());
        assert_no_release_artifacts(temp.path());
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
    fn import_unreleased_plans_before_writing_and_preserves_released_bytes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        let released = "Version 2.3.0\n-------------\n\nReleased on July 1, 2026.\n\n -  Shipped the previous release.\n";
        fs::write(
            temp.path().join("CHANGES.md"),
            format!(
                "Version 2.4.0\n-------------\n\nTo be released.\n\n- Added import support.\n\n{released}"
            ),
        )
        .expect("changelog");

        let plan =
            plan_import_unreleased(&repo, ImportUnreleasedOptions { force: false }).expect("plan");

        assert!(plan.requires_confirmation());
        assert_eq!(plan.inferred_version.as_deref(), Some("2.4.0"));
        assert!(
            !temp
                .path()
                .join("changes.d/imported-unreleased.md")
                .exists()
        );
        assert!(!temp.path().join("changes.d/next").exists());

        let plan = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect("forced plan");
        let result = apply_import_unreleased(&repo, plan).expect("apply");

        assert_eq!(
            result.written_fragments,
            vec![PathBuf::from("changes.d/imported-unreleased.md")]
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "2.4.0\n"
        );
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.ends_with(released));
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn import_unreleased_rejects_existing_fragments_without_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(temp.path().join("changes.d/existing.md"), " -  Existing.\n")
            .expect("existing fragment");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\n -  Added import support.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        let error = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect_err("existing fragment");

        assert!(matches!(error, Error::UnreleasedImportExistingFragments));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
        assert!(
            !temp
                .path()
                .join("changes.d/imported-unreleased.md")
                .exists()
        );
    }

    #[test]
    fn import_unreleased_requires_materialized_changelog() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );

        let error = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect_err("materialized changelog required");

        assert!(matches!(
            error,
            Error::UnreleasedImportRequiresMaterialization
        ));
    }

    #[test]
    fn import_unreleased_rejects_mismatched_next_version_without_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(temp.path().join("changes.d/next"), "2.5.0\n").expect("next");
        let changelog =
            "Version 2.4.0\n-------------\n\nTo be released.\n\n -  Added import support.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        let error = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect_err("mismatched next version");

        assert!(matches!(
            error,
            Error::UnreleasedImportNextMismatch {
                expected,
                actual
            } if expected == "2.4.0" && actual == "2.5.0"
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "2.5.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn import_unreleased_accepts_matching_next_version() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(temp.path().join("changes.d/next"), "2.4.0\n").expect("next");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 2.4.0\n-------------\n\nTo be released.\n\n -  Added import support.\n",
        )
        .expect("changelog");

        let plan = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect("matching next version");
        let result = apply_import_unreleased(&repo, plan).expect("apply");

        assert_eq!(result.inferred_version.as_deref(), Some("2.4.0"));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "2.4.0\n"
        );
    }

    #[test]
    fn import_unreleased_without_version_keeps_next_file_absent() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\n- Added import support.\n",
        )
        .expect("changelog");

        let plan = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect("version-less plan");

        assert!(plan.diff.is_some());
        assert!(!plan.requires_confirmation());
        let result = apply_import_unreleased(&repo, plan).expect("apply");
        assert_eq!(result.inferred_version, None);
        assert!(!temp.path().join("changes.d/next").exists());
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
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.  [[#8](https://example.com/pull/8)]\n\n[#8]: https://example.com/pull/8\n",
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
            "---\nlinks:\n  '#8': https://example.com/pull/8\n---\n -  Fixed carry.  [[#8]]\n"
        );
    }

    #[test]
    fn import_unreleased_preserves_resolved_reference_destinations() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 2.4.0\n-------------\n\nTo be released.\n\n -  Fixed import.  [[#9](https://example.com/discussions/9)]\n\n[#9]: https://example.com/discussions/9\n",
        )
        .expect("changelog");

        let plan = plan_import_unreleased(&repo, ImportUnreleasedOptions { force: true })
            .expect("import plan");
        apply_import_unreleased(&repo, plan).expect("apply import");

        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/imported-unreleased.md"))
                .expect("fragment"),
            "---\nlinks:\n  '#9': https://example.com/discussions/9\n---\n -  Fixed import.  [[#9]]\n"
        );
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains("[#9]: https://example.com/discussions/9"));
        assert!(!changelog.contains("[#9]: https://example.com/issues/9"));
    }

    #[test]
    fn carry_overwrites_a_malformed_existing_target_before_parsing() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/carried-from-1.1.5.md"),
            "not a fragment\n",
        )
        .expect("malformed carried target");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
        )
        .expect("changelog");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry repairs malformed target");

        assert_eq!(
            result.written_fragments,
            vec![PathBuf::from("changes.d/carried-from-1.1.5.md")]
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/carried-from-1.1.5.md"))
                .expect("repaired fragment"),
            " -  Fixed carry.\n"
        );
    }

    #[test]
    fn carry_overwrites_an_invalid_utf8_existing_target() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/carried-from-1.1.5.md"), [0xff])
            .expect("invalid UTF-8 carried target");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
        )
        .expect("changelog");

        carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry replaces invalid UTF-8 target");

        assert_eq!(
            fs::read(temp.path().join("changes.d/carried-from-1.1.5.md"))
                .expect("carried fragment"),
            b" -  Fixed carry.\n"
        );
    }

    #[test]
    fn carry_ignores_an_unrelated_malformed_fragment_without_materialization() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let unrelated = temp.path().join("changes.d/unrelated.md");
        fs::write(&unrelated, "not a fragment\n").expect("malformed unrelated fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
        )
        .expect("changelog");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry ignores unrelated malformed fragment");

        assert_eq!(result.sync, None);
        assert_eq!(
            fs::read_to_string(unrelated).expect("unrelated fragment"),
            "not a fragment\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/carried-from-1.1.5.md"))
                .expect("carried fragment"),
            " -  Fixed carry.\n"
        );
    }

    #[test]
    fn fragments_only_carry_rejects_a_pin_conflicting_with_an_existing_fragment() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let existing = temp.path().join("changes.d/existing.md");
        let existing_source = "\
---
links:
  '#1': https://example.com/issues/1
---
 -  Existing change.  [[#1]]
";
        fs::write(&existing, existing_source).expect("existing fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Carried change.  [[#1](https://example.com/pull/1)]\n\n[#1]: https://example.com/pull/1\n",
        )
        .expect("changelog");

        let error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("conflicting pins");

        assert!(matches!(
            error,
            Error::ConflictingResolvedLinks { ref label, ref first, ref second }
                if label == "#1"
                    && first == "https://example.com/pull/1"
                    && second == "https://example.com/issues/1"
        ));
        assert_eq!(
            fs::read_to_string(existing).expect("existing fragment"),
            existing_source
        );
        assert!(!temp.path().join("changes.d/carried-from-1.1.5.md").exists());
    }

    // APFS rejects the deliberately non-UTF-8 path before carry can inspect it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn carry_accepts_existing_non_utf8_fragments_in_discovery_order() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        let fragments = temp.path().join("changes.d");
        fs::create_dir_all(&fragments).expect("fragments dir");
        fs::write(
            fragments.join(OsString::from_vec(vec![0x80, b'.', b'm', b'd'])),
            " -  Invalid UTF-8 filename.\n",
        )
        .expect("non-UTF-8 fragment");
        fs::write(fragments.join("é.md"), " -  Unicode filename.\n").expect("Unicode fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
        )
        .expect("changelog");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry with non-UTF-8 fragment names");

        assert_eq!(
            result.written_fragments,
            vec![PathBuf::from("changes.d/carried-from-1.1.5.md")]
        );
        assert!(temp.path().join("changes.d/é.md").exists());
        assert!(
            temp.path()
                .join("changes.d")
                .join(OsString::from_vec(vec![0x80, b'.', b'm', b'd']))
                .exists()
        );
    }

    #[test]
    fn carry_normalizes_historical_item_formatting() {
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
            " -  Fixed historical wrapping.\n    This continuation keeps old spacing.\n"
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
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
            "Unreleased\n----------\n\nTo be released.\n\n\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
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
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn carry_starts_materialized_region_after_closed_release() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Changelog\n=========\n\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n",
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
        assert!(changelog.starts_with(
            "Changelog\n=========\n\nUnreleased\n----------\n\nTo be released.\n\n -  Fixed carry.\n"
        ));
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn carry_materialization_preserves_sections_for_existing_and_carried_fragments() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("cli dir");
        fs::write(
            temp.path().join("changes.d/core/existing.md"),
            " -  Existing core.\n",
        )
        .expect("core fragment");
        fs::write(
            temp.path().join("changes.d/cli/existing.md"),
            " -  Existing CLI.\n",
        )
        .expect("cli fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n### core\n\n -  Carried core.\n\n### cli\n\n -  Carried CLI.\n",
        )
        .expect("changelog");
        let sync = plan_sync(&repo, SyncOptions { force: true }).expect("sync plan");
        apply_sync(&repo, sync).expect("initial sync");

        let result = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect("carry");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        let core_heading = changelog.find("### core").expect("core heading");
        let cli_heading = changelog.find("### cli").expect("cli heading");
        assert!(
            changelog[core_heading..cli_heading].contains(" -  Carried core.\n")
                && changelog[core_heading..cli_heading].contains(" -  Existing core.\n")
        );
        assert!(changelog[cli_heading..].contains(" -  Carried CLI.\n"));
        assert!(changelog[cli_heading..].contains(" -  Existing CLI.\n"));
    }

    #[test]
    fn carry_nth_write_failure_restores_earlier_fragments_without_materialization() {
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
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::write(
            temp.path().join("changes.d/core/carried-from-1.1.5.md"),
            " -  Original core.\n",
        )
        .expect("old core");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n### core\n\n -  Fixed core.\n\n### cli\n\n -  Fixed CLI.\n",
        )
        .expect("changelog");
        set_release_failpoint(
            ReleaseApplyStage::Apply,
            PathBuf::from("changes.d/core/carried-from-1.1.5.md"),
        );

        let error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("second fragment write failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/core/carried-from-1.1.5.md"))
                .expect("restored core"),
            " -  Original core.\n"
        );
        assert!(
            !temp
                .path()
                .join("changes.d/cli/carried-from-1.1.5.md")
                .exists()
        );
    }

    #[test]
    fn carry_sync_failure_restores_every_fragment_and_the_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\n\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("CHANGES.md"));

        let error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("sync write failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert!(!temp.path().join("changes.d/carried-from-1.1.5.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn carry_rolls_back_when_a_new_fragment_appears_during_apply() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\n\nVersion 1.1.5\n-------------\n\nReleased on July 1, 2026.\n\n -  Fixed carry.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        set_release_interference(
            ReleaseApplyStage::Apply,
            "CHANGES.md",
            "changes.d/late.md",
            " -  Added concurrently.\n",
        );

        let error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.5"),
            },
        )
        .expect_err("late fragment");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Carry,
                ..
            }
        ));
        assert!(!temp.path().join("changes.d/carried-from-1.1.5.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/late.md")).expect("late fragment"),
            " -  Added concurrently.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
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
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            changelog,
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed release.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
        assert!(!temp.path().join("changes.d/fix.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("post-release check")
                .is_clean()
        );
    }

    #[test]
    fn release_closes_materialized_region_without_next_version() {
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
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            changelog,
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n"
        );
        assert!(!temp.path().join("changes.d/add.md").exists());
        assert!(!temp.path().join("changes.d/next").exists());
        assert!(
            check(&repo, CheckOptions::default())
                .expect("post-release check")
                .is_clean()
        );
    }

    #[test]
    fn release_rejects_a_hand_edit_in_the_retained_changelog_snapshot() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Added release.\n",
        )
        .expect("changelog");
        let hand_edited = "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Hand-edited release.\n";
        set_release_interference(
            ReleaseApplyStage::PlanSnapshot,
            PathBuf::from("CHANGES.md"),
            PathBuf::from("CHANGES.md"),
            hand_edited,
        );

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("retained hand edit");
        clear_release_failpoint();

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            hand_edited
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_rejects_empty_non_initial_release_without_override() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");
        let sync = plan_sync(&repo, SyncOptions { force: true }).expect("sync plan");
        apply_sync(&repo, sync).expect("sync");
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect_err("empty release");

        assert_eq!(
            error.to_string(),
            "no changelog entries to release; add a fragment or pass --allow-empty if this is intentional"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
    }

    #[test]
    fn release_accepts_initial_release_without_fragments() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.0.0\n-------------\n\nTo be released.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.1.0")),
                allow_empty: false,
            },
        )
        .expect("initial release");
        apply_release(&repo, plan).expect("apply initial release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nTo be released.\n\n\nVersion 1.0.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.1.0\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("post-release check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn release_accepts_initial_release_without_fragments_or_changelog() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.0.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("initial release");
        apply_release(&repo, plan).expect("apply initial release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n\nVersion 1.0.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
    }

    #[test]
    fn release_ignores_version_prefixed_atx_document_title() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            title = "Version history"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "<!-- Project release history. -->\n\n# Version history\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.0.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("initial release");
        apply_release(&repo, plan).expect("apply initial release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "<!-- Project release history. -->\n\n# Version history\n\nVersion 1.0.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
    }

    #[test]
    fn release_rejects_empty_release_after_first_line_h1_version_section() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "# Version 1.0.0\n\nReleased on July 20, 2026.\n",
        )
        .expect("changelog");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.1.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("empty non-initial release");

        assert!(matches!(error, Error::EmptyRelease));
    }

    #[test]
    fn release_rejects_empty_scaffold_fragment_without_changes() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/scaffold.md"), " -  \n").expect("fragment");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("empty release");

        assert_eq!(
            error.to_string(),
            "no changelog entries to release; add a fragment or pass --allow-empty if this is intentional"
        );
        assert!(!temp.path().join("CHANGES.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/scaffold.md")).expect("fragment"),
            " -  \n"
        );
    }

    #[test]
    fn release_allow_empty_consumes_scaffold_fragments() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/scaffold.md"), " -  \n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: true,
            },
        )
        .expect("intentional empty release");
        apply_release(&repo, plan).expect("apply empty release");

        assert!(!temp.path().join("changes.d/scaffold.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
    }

    #[test]
    fn release_resolves_links_in_memory_before_consuming_fragments() {
        let server = TestHttpServer::spawn(vec![
            http_response("302 Found", Some("/pull/3")),
            http_response("200 OK", None),
        ]);
        let (temp, repo) = repo_with_config(&format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "0.2.0\n").expect("next");
        fs::write(
            temp.path().join("changes.d/release.md"),
            " -  Fixed release links.  [[#3]]\n",
        )
        .expect("fragment");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");

        let plan = plan_release_with_link_resolution(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("release plan");
        assert!(
            plan.released_markdown
                .contains(&format!("[#3]: {}/pull/3", server.base))
        );

        apply_release(&repo, plan).expect("release");

        assert!(!temp.path().join("changes.d/release.md").exists());
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("changelog")
                .contains(&format!("[#3]: {}/pull/3", server.base))
        );
        assert_eq!(
            server.finish(),
            vec!["HEAD /issues/3 HTTP/1.1", "HEAD /pull/3 HTTP/1.1"]
        );
    }

    #[test]
    fn resolving_release_rejects_a_fragment_changed_after_resolution() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "0.2.0\n").expect("next");
        let fragment = temp.path().join("changes.d/release.md");
        fs::write(
            &fragment,
            "---\nlinks:\n  '#3': https://example.com/pull/3\n---\n -  Before.  [[#3]]\n",
        )
        .expect("fragment");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        let resolved = prepare_resolved_fragments(&repo, None).expect("resolved snapshots");
        fs::write(
            &fragment,
            "---\nlinks:\n  '#3': https://example.com/pull/3\n---\n -  After.  [[#3]]\n",
        )
        .expect("concurrent edit");

        let error = plan_release_with_resolved_fragments(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
            Some(resolved),
        )
        .expect_err("stale fragment");

        assert!(matches!(
            error,
            Error::StaleReleasePlan { ref path }
                if path == Path::new("changes.d/release.md")
        ));
    }

    #[test]
    fn release_rejects_empty_release_from_closed_materialized_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("empty release");

        assert!(matches!(error, Error::EmptyRelease));
    }

    #[test]
    fn release_allows_intentional_empty_release_from_closed_materialized_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: true,
            },
        )
        .expect("intentional empty release");
        apply_release(&repo, plan).expect("apply release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn release_opens_next_version_from_closed_materialized_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: true,
            },
        )
        .expect("intentional empty release");
        apply_release(&repo, plan).expect("apply release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.3.0\n-------------\n\nTo be released.\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn release_does_not_treat_missing_markers_as_a_closed_region() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            region-detection = "marker"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.1.0\n-------------\n",
        )
        .expect("changelog");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: true,
            },
        )
        .expect_err("missing markers");

        assert!(matches!(error, Error::RegionNotFound { .. }));
    }

    #[test]
    fn release_rejects_fragment_containing_only_html_comment() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment = " -  <!-- TODO: write this -->\n";
        fs::write(temp.path().join("changes.d/scaffold.md"), fragment).expect("fragment");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("comment-only release");

        assert_eq!(
            error.to_string(),
            "no changelog entries to release; add a fragment or pass --allow-empty if this is intentional"
        );
        assert!(!temp.path().join("CHANGES.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/scaffold.md")).expect("fragment"),
            fragment
        );
    }

    #[test]
    fn release_accepts_substantive_non_text_content() {
        for fragment in [
            " -  ![](release.png)\n",
            " -  <img src=\"release.png\" alt=\"Release diagram\">\n",
            " -  <span hidden></span>\n",
            " -  <script>void 0</script>\n",
            " -  <div style=\"display: none\">Placeholder.</div>\n",
            " -  <div>&nbsp;</div>\n",
            " -  [](/release-artifact)\n",
        ] {
            let (temp, repo) = repo_with_config(
                r#"
                [changelog]
                materialize = false
                "#,
            );
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            fs::write(temp.path().join("changes.d/content.md"), fragment).expect("fragment");

            let plan = plan_release(
                &repo,
                ReleaseOptions {
                    version: Some(String::from("1.2.0")),
                    date: release_date(),
                    next: None,
                    allow_empty: false,
                },
            )
            .expect("substantive content release");

            assert!(plan.released_markdown.contains(fragment.trim_end()));
        }
    }

    #[test]
    fn release_rejects_repository_when_every_section_is_empty() {
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
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("cli dir");
        fs::write(temp.path().join("changes.d/core/scaffold.md"), " -  \n").expect("core fragment");
        fs::write(temp.path().join("changes.d/cli/scaffold.md"), " -  \n").expect("cli fragment");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("empty release");

        assert_eq!(
            error.to_string(),
            "no changelog entries to release; add a fragment or pass --allow-empty if this is intentional"
        );
        assert!(!temp.path().join("CHANGES.md").exists());
        assert!(temp.path().join("changes.d/core/scaffold.md").exists());
        assert!(temp.path().join("changes.d/cli/scaffold.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
    }

    #[test]
    fn release_with_blank_next_removes_next_file_and_closes_materialized_region() {
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
                date: release_date(),
                next: Some(String::from("  ")),
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Version 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n"
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
            "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\n\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Fixed marker release.\n\n<!-- sacho:unreleased:end -->\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        let expected = "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\n\n<!-- sacho:unreleased:end -->\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed marker release.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n";
        assert!(matches!(
            &plan.changelog.after,
            ReleaseFileState::Present(changelog) if changelog == expected
        ));
        apply_release(&repo, plan).expect("release");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(changelog, expected);
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("post-release check")
                .is_clean()
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
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 1, 2026.\n",
        )
        .expect("stale changelog");

        let error = apply_release(&repo, plan).expect_err("stale changelog");

        assert!(matches!(error, Error::StaleReleasePlan { .. }));
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
    fn release_prepare_failure_changes_nothing() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Prepare, PathBuf::from("changes.d/next"));

        let error = apply_release(&repo, plan).expect_err("prepare failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::WriteFile { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn release_move_probe_failure_changes_nothing() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Probe, PathBuf::from("CHANGES.md"));

        let error = apply_release(&repo, plan).expect_err("move probe failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseTransactionUnsupported));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn release_changelog_prepare_or_replace_failure_changes_nothing() {
        for stage in [ReleaseApplyStage::Prepare, ReleaseApplyStage::Apply] {
            let (temp, repo) = repo_with_config(
                r#"
                [changelog]
                materialize = false
                "#,
            );
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            let old_changelog = "Changelog\n=========\n";
            fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
            fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
            fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n")
                .expect("fragment");
            let plan = plan_release(
                &repo,
                ReleaseOptions {
                    version: None,
                    date: release_date(),
                    next: Some(String::from("1.3.0")),
                    allow_empty: false,
                },
            )
            .expect("plan");
            set_release_failpoint(stage, PathBuf::from("CHANGES.md"));

            apply_release(&repo, plan).expect_err("changelog failure");
            clear_release_failpoint();

            assert_eq!(
                fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
                old_changelog
            );
            assert_eq!(
                fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
                "1.2.0\n"
            );
            assert!(temp.path().join("changes.d/add.md").exists());
        }
    }

    #[test]
    fn release_does_not_replace_a_changelog_edited_after_validation() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::Apply,
            PathBuf::from("CHANGES.md"),
            PathBuf::from("CHANGES.md"),
            "Concurrent changelog edit.\n",
        );

        let error = apply_release(&repo, plan).expect_err("concurrent changelog edit");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Concurrent changelog edit.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_failure_removes_directories_created_while_preparing_writes() {
        for stage in [ReleaseApplyStage::Prepare, ReleaseApplyStage::Apply] {
            let (temp, repo) = repo_with_config(
                r#"
                [changelog]
                path = "generated/changelog/CHANGES.md"
                materialize = false
                "#,
            );
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
            fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n")
                .expect("fragment");
            let plan = plan_release(
                &repo,
                ReleaseOptions {
                    version: None,
                    date: release_date(),
                    next: Some(String::from("1.3.0")),
                    allow_empty: false,
                },
            )
            .expect("plan");
            set_release_failpoint(stage, PathBuf::from("changes.d/next"));

            apply_release(&repo, plan).expect_err("release failure");
            clear_release_failpoint();

            assert!(!temp.path().join("generated").exists(), "stage: {stage:?}");
            assert_eq!(
                fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
                "1.2.0\n"
            );
            assert!(temp.path().join("changes.d/add.md").exists());
        }
    }

    #[test]
    fn release_does_not_delete_a_fragment_edited_after_validation() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::Apply,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/add.md"),
            " -  Concurrent fragment edit.\n",
        );

        let error = apply_release(&repo, plan).expect_err("concurrent fragment edit");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Concurrent fragment edit.\n"
        );
    }

    #[test]
    fn release_does_not_replace_an_edit_created_after_claiming_a_path() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::ApplyClaimed,
            PathBuf::from("CHANGES.md"),
            PathBuf::from("CHANGES.md"),
            "Edit created after claim.\n",
        );
        set_release_failpoint(ReleaseApplyStage::ApplyClaimed, PathBuf::from("CHANGES.md"));

        let error = apply_release(&repo, plan).expect_err("edit after claim");
        clear_release_failpoint();

        match error {
            Error::ReleaseApply {
                cause,
                rollback_failures,
            } => {
                assert!(cause.contains("injected release I/O failure"));
                assert_eq!(rollback_failures.len(), 1);
                assert!(rollback_failures[0].contains("changed after the release wrote it"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Edit created after claim.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_retains_original_when_replacement_destination_is_recreated() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::ApplyClaimed,
            PathBuf::from("CHANGES.md"),
            PathBuf::from("CHANGES.md"),
            "Concurrent changelog.\n",
        );

        let error = apply_release(&repo, plan).expect_err("replacement collision");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::ReleaseApply {
                rollback_failures,
                ..
            } if rollback_failures.iter().any(|failure| failure.contains("changed after the release wrote it"))
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Concurrent changelog.\n"
        );
        assert_eq!(
            retained_release_claim_contents(temp.path()),
            [old_changelog]
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_retains_original_when_deletion_destination_is_recreated() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        let old_fragment = " -  Added release.\n";
        fs::write(temp.path().join("changes.d/add.md"), old_fragment).expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::ApplyClaimed,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/add.md"),
            " -  Concurrent fragment.\n",
        );

        let error = apply_release(&repo, plan).expect_err("deletion collision");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::ReleaseApply {
                rollback_failures,
                ..
            } if rollback_failures.iter().any(|failure| failure.contains("changed after the release wrote it"))
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Concurrent fragment.\n"
        );
        assert_eq!(retained_release_claim_contents(temp.path()), [old_fragment]);
    }

    #[test]
    fn release_retains_fragment_apply_error_when_claim_restoration_fails() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::ApplyClaimed,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/add.md"),
            " -  Concurrent fragment edit.\n",
        );
        set_release_failpoint(
            ReleaseApplyStage::ApplyClaimed,
            PathBuf::from("changes.d/add.md"),
        );

        let error = apply_release(&repo, plan).expect_err("fragment restore collision");
        clear_release_failpoint();

        match error {
            Error::ReleaseApply {
                cause,
                rollback_failures,
            } => {
                assert!(cause.contains("injected release I/O failure"));
                assert_eq!(rollback_failures.len(), 1);
                assert!(rollback_failures[0].contains("changed after the release wrote it"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Concurrent fragment edit.\n"
        );
    }

    #[test]
    fn release_does_not_replace_a_missing_path_created_during_apply() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::Apply,
            PathBuf::from("changes.d/next"),
            PathBuf::from("changes.d/next"),
            "Concurrent next version.\n",
        );

        let error = apply_release(&repo, plan).expect_err("concurrent next creation");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "Concurrent next version.\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_claim_failure_restores_the_claimed_path() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::ApplyClaimed, PathBuf::from("CHANGES.md"));

        apply_release(&repo, plan).expect_err("failure after claim");
        clear_release_failpoint();

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_next_apply_failure_rolls_back_changelog_and_keeps_fragments() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));

        let error = apply_release(&repo, plan).expect_err("next apply failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::ReleaseApply {
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_fragment_delete_failure_restores_every_applied_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/a.md"), " -  Added A.\n").expect("fragment a");
        fs::write(temp.path().join("changes.d/b.md"), " -  Added B.\n").expect("fragment b");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/b.md"));

        let error = apply_release(&repo, plan).expect_err("fragment delete failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/a.md")).expect("fragment a"),
            " -  Added A.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b.md")).expect("fragment b"),
            " -  Added B.\n"
        );
    }

    #[test]
    fn release_changelog_discard_failure_restores_every_applied_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(
            ReleaseApplyStage::DiscardClaimed,
            PathBuf::from("CHANGES.md"),
        );

        let error = apply_release(&repo, plan).expect_err("changelog discard failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn release_fragment_discard_failure_restores_every_applied_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(
            ReleaseApplyStage::DiscardClaimed,
            PathBuf::from("changes.d/add.md"),
        );

        let error = apply_release(&repo, plan).expect_err("fragment discard failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn release_reports_post_commit_claim_cleanup_without_rolling_back() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(
            ReleaseApplyStage::CommittedDiscardClaimed,
            PathBuf::from("changes.d/next"),
        );

        let error = apply_release(&repo, plan).expect_err("committed cleanup failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseCleanup { .. }));
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("released changelog")
                .contains("Added release.")
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
        assert!(!temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_revalidates_applied_changelog_before_committing_claims() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("CHANGES.md"),
            "Concurrent changelog edit.\n",
        );

        let error = apply_release(&repo, plan).expect_err("changed applied changelog");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Concurrent changelog edit.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
    }

    #[test]
    fn release_revalidates_unchanged_next_file_before_committing_claims() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        assert_eq!(plan.next_file.before, ReleaseFileState::Missing);
        assert_eq!(plan.next_file.after, ReleaseFileState::Missing);
        set_release_interference(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/next"),
            "1.3.0\n",
        );

        let error = apply_release(&repo, plan).expect_err("concurrently created next file");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
    }

    #[test]
    fn release_revalidates_fragment_absence_before_committing_claims() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/add.md"),
            " -  Concurrent fragment.\n",
        );

        let error = apply_release(&repo, plan).expect_err("recreated fragment");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Concurrent fragment.\n"
        );
    }

    #[test]
    fn release_revalidates_retained_originals_before_committing_claims() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_claim_interference(
            PathBuf::from("CHANGES.md"),
            "Concurrent edit through retained inode.\n",
        );

        let error = apply_release(&repo, plan).expect_err("changed retained original");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Concurrent edit through retained inode.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
    }

    #[test]
    fn release_rechecks_for_new_fragments_before_committing_claims() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/add.md"),
            PathBuf::from("changes.d/concurrent.md"),
            " -  Concurrent fragment.\n",
        );

        let error = apply_release(&repo, plan).expect_err("new fragment");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/concurrent.md"))
                .expect("concurrent fragment"),
            " -  Concurrent fragment.\n"
        );
    }

    #[test]
    fn release_next_claim_directory_cleanup_failure_restores_the_deleted_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/next"),
        );

        let error = apply_release(&repo, plan).expect_err("next claim cleanup failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_fragment_claim_directory_cleanup_failure_restores_the_deleted_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let old_changelog = "Changelog\n=========\n";
        fs::write(temp.path().join("CHANGES.md"), old_changelog).expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(
            ReleaseApplyStage::DiscardClaimDirectory,
            PathBuf::from("changes.d/add.md"),
        );

        let error = apply_release(&repo, plan).expect_err("fragment claim cleanup failure");
        clear_release_failpoint();

        assert!(matches!(error, Error::ReleaseApply { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
    }

    #[test]
    fn materialized_release_failure_restores_exact_changelog_bytes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let old_changelog = format!(
            "Project changes\n===============\n\n{}\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n\nHistorical  spacing.\n",
            compiled.markdown
        );
        fs::write(temp.path().join("CHANGES.md"), &old_changelog).expect("changelog");
        apply_sync(
            &repo,
            plan_sync(&repo, SyncOptions { force: true }).expect("sync plan"),
        )
        .expect("sync");
        let old_changelog =
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("synced changelog");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));

        apply_release(&repo, plan).expect_err("next apply failure");
        clear_release_failpoint();

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            old_changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn release_rollback_restores_the_original_changelog_inode_and_mode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let changelog_path = temp.path().join("CHANGES.md");
        let changelog_alias = temp.path().join("CHANGES.alias");
        fs::write(&changelog_path, "Changelog\n=========\n").expect("changelog");
        let mut permissions = fs::metadata(&changelog_path)
            .expect("changelog metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&changelog_path, permissions).expect("executable changelog");
        fs::hard_link(&changelog_path, &changelog_alias).expect("changelog hard link");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let before = fs::metadata(&changelog_path).expect("before metadata");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));

        apply_release(&repo, plan).expect_err("next apply failure");
        clear_release_failpoint();

        let after = fs::metadata(&changelog_path).expect("restored metadata");
        let alias = fs::metadata(&changelog_alias).expect("alias metadata");
        assert_eq!(after.dev(), before.dev());
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.ino(), alias.ino());
        assert_eq!(after.mode(), before.mode());
        assert_eq!(after.nlink(), before.nlink());
    }

    #[test]
    fn release_reports_original_and_rollback_failures() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));
        set_release_failpoint(ReleaseApplyStage::Rollback, PathBuf::from("CHANGES.md"));

        let error = apply_release(&repo, plan).expect_err("apply and rollback failure");
        clear_release_failpoint();

        match error {
            Error::ReleaseApply {
                cause,
                rollback_failures,
            } => {
                assert!(cause.contains("changes.d/next"));
                assert_eq!(rollback_failures.len(), 1);
                assert!(rollback_failures[0].contains("CHANGES.md"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn release_rollback_preserves_concurrent_edits_and_reports_conflict() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::Apply,
            PathBuf::from("changes.d/next"),
            PathBuf::from("CHANGES.md"),
            "Concurrent changelog edit.\n",
        );
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));

        let error = apply_release(&repo, plan).expect_err("next apply failure");
        clear_release_failpoint();

        match error {
            Error::ReleaseApply {
                rollback_failures, ..
            } => {
                assert_eq!(rollback_failures.len(), 1);
                assert!(rollback_failures[0].contains("changed after the release wrote it"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Concurrent changelog edit.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_rollback_does_not_clobber_edit_created_after_claiming_applied_file() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("CHANGES.md"), "Changelog\n=========\n").expect("changelog");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        set_release_interference(
            ReleaseApplyStage::RollbackClaimed,
            PathBuf::from("CHANGES.md"),
            PathBuf::from("CHANGES.md"),
            "Edit created during rollback.\n",
        );
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("changes.d/next"));

        let error = apply_release(&repo, plan).expect_err("next apply failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::ReleaseApply {
                rollback_failures,
                ..
            } if rollback_failures.len() == 1
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("concurrent changelog"),
            "Edit created during rollback.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_rejects_overlapping_changelog_and_next_paths() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [changelog]
            path = "changes.d/next"
            materialize = false
            "#,
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("overlapping paths");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
    }

    #[test]
    fn release_rejects_ancestor_overlap_between_missing_participants() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let mut plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        plan.changelog.path = PathBuf::from("meta/ancestor/CHANGES.md");
        plan.next_file.path = PathBuf::from("meta/ancestor");

        let error = apply_release(&repo, plan).expect_err("ancestor overlap");

        assert!(matches!(
            error,
            Error::ReleasePathOverlap { first, second }
                if first == Path::new("meta/ancestor/CHANGES.md")
                    && second == Path::new("meta/ancestor")
        ));
        assert!(!temp.path().join("meta/ancestor").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/add.md")).expect("fragment"),
            " -  Added release.\n"
        );
    }

    #[test]
    fn release_apply_revalidates_distinct_paths() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let mut plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        plan.next_file.path = plan.changelog.path.clone();

        let error = apply_release(&repo, plan).expect_err("overlapping applied paths");

        assert!(matches!(error, Error::ReleasePathOverlap { .. }));
        assert!(!temp.path().join("CHANGES.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_temporary_paths_do_not_collide_with_participants() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            path = "meta/out"
            materialize = false

            [fragments]
            directory = "meta/fragments"
            next-file = "out.tmp"
            "#,
        );
        fs::create_dir_all(temp.path().join("meta/fragments")).expect("fragments dir");
        fs::write(temp.path().join("meta/fragments/out.tmp"), "1.2.0\n").expect("next");
        fs::write(
            temp.path().join("meta/fragments/add.md"),
            " -  Added release.\n",
        )
        .expect("fragment");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert!(
            fs::read_to_string(temp.path().join("meta/out"))
                .expect("changelog")
                .contains("Added release.")
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("meta/fragments/out.tmp")).expect("next"),
            "1.3.0\n"
        );
        assert!(!temp.path().join("meta/fragments/add.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn release_accepts_a_read_only_configuration_file() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        let config_path = temp.path().join("sacho.toml");
        let mut permissions = fs::metadata(&config_path)
            .expect("config metadata")
            .permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&config_path, permissions).expect("read-only config");

        apply_release(&repo, plan).expect("release");

        assert!(temp.path().join("CHANGES.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.3.0\n"
        );
        assert!(!temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_apply_rejects_a_concurrent_repository_release_lock() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: Some(String::from("1.3.0")),
                allow_empty: false,
            },
        )
        .expect("plan");
        let _lock = acquire_mutation_lock_at_root(temp.path()).expect("hold release lock");

        let error = apply_release(&repo, plan).expect_err("concurrent release lock");

        assert!(matches!(error, Error::ReleaseLocked));
        assert!(!temp.path().join("CHANGES.md").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn every_repository_mutation_honors_the_shared_lock() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let _lock = acquire_mutation_lock_at_root(temp.path()).expect("hold mutation lock");

        assert_mutation_locked(init_repository(
            temp.path(),
            InitOptions {
                changelog_path: None,
                fragment_directory: None,
                materialize: None,
                integration_executable: None,
                install_hook: false,
                append_existing_hook: false,
                repository_url: None,
                sections: Vec::new(),
                section_patterns: Vec::new(),
            },
        ));
        assert_mutation_locked(add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("locked"),
            },
        ));
        assert_mutation_locked(set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        ));
        let format_plan = plan_format(&repo, FormatOptions).expect("format plan without mutation");
        assert_mutation_locked(apply_format(&repo, format_plan));
        assert_mutation_locked(apply_sync(
            &repo,
            SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled),
        ));
        assert_mutation_locked(plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        ));
        assert_mutation_locked(carry(
            &repo,
            CarryOptions {
                version: String::from("1.1.0"),
            },
        ));

        assert!(!temp.path().join("changes.d/locked.md").exists());
        assert!(!temp.path().join("changes.d/next").exists());
    }

    #[test]
    fn release_path_identity_rejects_non_directory_ancestors() {
        let (temp, repo) = repo_with_config("");
        fs::write(temp.path().join("not-a-directory"), "file\n").expect("file");

        let error = release_path_identity(&repo, Path::new("not-a-directory/child"))
            .expect_err("non-directory ancestor");

        assert!(
            matches!(error, Error::ReadFile { source, .. } if source.kind() == ErrorKind::NotADirectory)
        );
    }

    #[test]
    fn mutation_lock_identity_does_not_prefix_a_relative_root_twice() {
        let temp = TempDir::new_in(".").expect("relative tempdir");
        let current = std::env::current_dir().expect("current directory");
        let root = temp
            .path()
            .strip_prefix(&current)
            .expect("tempdir below current directory")
            .to_path_buf();
        assert!(root.is_relative());
        let path = root.join(MUTATION_LOCK_FILE);
        let mut config = Config::parse("").expect("config");
        config.changelog.path = PathBuf::from(MUTATION_LOCK_FILE);

        let error = validate_configured_mutation_paths_at_root(&root, &config, &path)
            .expect_err("relative mutation lock alias");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
    }

    #[test]
    fn release_path_identity_cancels_a_missing_component_before_parent() {
        let (temp, repo) = repo_with_config("");

        let identity = release_path_identity(&repo, Path::new("nested/../CHANGES.md"))
            .expect("missing component followed by parent");

        assert_eq!(
            identity,
            fs::canonicalize(temp.path())
                .expect("repository root")
                .join("CHANGES.md")
        );
    }

    #[test]
    fn release_path_overlap_respects_filesystem_case_sensitivity() {
        let upper = Path::new("/repo/Meta/next");
        let lower = Path::new("/repo/meta/next");
        let lower_child = Path::new("/repo/meta/next/child");

        assert!(!release_paths_overlap(upper, lower, true));
        assert!(release_paths_overlap(upper, lower, false));
        assert!(release_paths_overlap(upper, lower_child, false));
    }

    #[test]
    fn release_path_pair_is_case_sensitive_only_when_both_filesystems_are() {
        assert!(release_path_pair_case_sensitive(true, true));
        assert!(!release_path_pair_case_sensitive(true, false));
        assert!(!release_path_pair_case_sensitive(false, true));
        assert!(!release_path_pair_case_sensitive(false, false));
    }

    #[test]
    fn release_path_case_sensitivity_uses_only_the_matching_cached_directory() {
        let temp = TempDir::new().expect("temporary directory");
        let directory = fs::canonicalize(temp.path()).expect("canonical temporary directory");
        let identity = directory.join("missing");
        let unrelated = directory.join("unrelated");
        let mut cache = vec![(unrelated, true), (directory.clone(), false)];

        assert!(
            !release_path_case_sensitivity(&identity, std::slice::from_ref(&identity), &mut cache,)
                .expect("cached case sensitivity")
        );
    }

    #[cfg(unix)]
    #[test]
    fn release_path_identity_resolves_symlinks_before_parent_components() {
        use std::os::unix::fs::symlink;

        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("actual/nested")).expect("actual directory");
        fs::write(temp.path().join("actual/shared"), "shared\n").expect("shared file");
        symlink(
            temp.path().join("actual/nested"),
            temp.path().join("linked"),
        )
        .expect("directory symlink");

        let through_symlink =
            release_path_identity(&repo, Path::new("linked/../shared")).expect("symlink path");
        let direct = release_path_identity(&repo, Path::new("actual/shared")).expect("direct path");

        assert_eq!(through_symlink, direct);
    }

    #[test]
    fn release_claim_error_kind_matching_is_exact() {
        let not_found = std::io::Error::from(ErrorKind::NotFound);
        let already_exists = std::io::Error::from(ErrorKind::AlreadyExists);

        assert!(error_has_kind(&not_found, ErrorKind::NotFound));
        assert!(!error_has_kind(&not_found, ErrorKind::AlreadyExists));
        assert!(error_has_kind(&already_exists, ErrorKind::AlreadyExists));
        assert!(!error_has_kind(&already_exists, ErrorKind::InvalidData));
    }

    #[test]
    fn mutation_state_error_classification_preserves_genuine_read_failures() {
        let read_error = |kind| Error::ReadFile {
            path: PathBuf::from("changes.d/change.md"),
            source: std::io::Error::from(kind),
        };

        for kind in [
            ErrorKind::InvalidData,
            ErrorKind::NotFound,
            ErrorKind::IsADirectory,
        ] {
            assert!(mutation_state_error_is_stale(&read_error(kind)));
        }
        assert!(!mutation_state_error_is_stale(&read_error(
            ErrorKind::PermissionDenied
        )));
        assert!(mutation_state_error_is_stale(&Error::ReleasePathConflict {
            path: PathBuf::from("changes.d/change.md"),
        }));
    }

    #[test]
    fn release_claim_paths_avoid_participants_and_their_ancestors() {
        let claim = Path::new("/repo/.sacho-claim-1");
        let exact = vec![claim.to_path_buf()];
        let nested = vec![claim.join("claimed")];
        let unrelated = vec![PathBuf::from("/repo/changes.d/add.md")];
        let containing = vec![PathBuf::from("/repo")];

        assert!(release_claim_path_conflicts(claim, &exact));
        assert!(release_claim_path_conflicts(claim, &nested));
        assert!(!release_claim_path_conflicts(claim, &unrelated));
        assert!(!release_claim_path_conflicts(claim, &containing));
        assert!(!release_claim_path_conflicts(claim, &[]));
    }

    #[test]
    fn release_rollback_classifies_a_missing_applied_file_as_conflict() {
        let (_temp, repo) = repo_with_config("");

        let error = restore_release_state(
            &repo,
            Path::new("CHANGES.md"),
            &ReleaseFileState::Present(String::from("before\n")),
            &ReleaseFileState::Present(String::from("applied\n")),
        )
        .expect_err("missing applied file");

        assert!(matches!(error, Error::ReleaseRollbackConflict { .. }));
    }

    #[test]
    fn release_rollback_can_remove_an_applied_file_conditionally() {
        let (temp, repo) = repo_with_config("");
        fs::write(temp.path().join("created-by-release"), "applied\n").expect("applied file");

        restore_release_state(
            &repo,
            Path::new("created-by-release"),
            &ReleaseFileState::Missing,
            &ReleaseFileState::Present(String::from("applied\n")),
        )
        .expect("rollback removal");

        assert!(!temp.path().join("created-by-release").exists());
    }

    #[test]
    fn release_rollback_discards_guard_when_concurrent_destination_exists() {
        let (temp, repo) = repo_with_config("");
        let absolute = temp.path().join("CHANGES.md");
        let directory = temp.path().join(".sacho-claim-test");
        fs::create_dir(&directory).expect("claim directory");
        let guard = directory.join("claimed");
        fs::write(&absolute, "concurrent\n").expect("concurrent destination");
        fs::write(&guard, "applied\n").expect("guard");
        let claimed = ClaimedReleaseFile {
            directory,
            file: guard.clone(),
            destination: absolute.clone(),
        };

        preserve_applied_claim(&repo, Path::new("CHANGES.md"), claimed)
            .expect("preserve concurrent destination");

        assert_eq!(
            fs::read_to_string(absolute).expect("destination"),
            "concurrent\n"
        );
        assert!(!guard.exists());
    }

    #[test]
    fn release_rollback_classifies_conditional_install_collision_as_conflict() {
        let (temp, repo) = repo_with_config("");
        fs::write(temp.path().join("CHANGES.md"), "concurrent\n").expect("destination");

        let error = install_rollback_file_if_absent(&repo, Path::new("CHANGES.md"), b"before\n")
            .expect_err("conditional install collision");

        assert!(matches!(error, Error::ReleaseRollbackConflict { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("destination"),
            "concurrent\n"
        );
    }

    #[test]
    fn release_rollback_classifies_claim_restore_collision_as_conflict() {
        let (temp, _repo) = repo_with_config("");
        let absolute = temp.path().join("CHANGES.md");
        let directory = temp.path().join(".sacho-claim-test");
        fs::create_dir(&directory).expect("claim directory");
        let guard = directory.join("claimed");
        fs::write(&absolute, "new concurrent path\n").expect("destination");
        fs::write(&guard, "claimed concurrent edit\n").expect("guard");
        let claimed = ClaimedReleaseFile {
            directory,
            file: guard.clone(),
            destination: absolute.clone(),
        };

        let error = claimed
            .restore(Path::new("CHANGES.md"))
            .expect_err("claim restore collision");

        assert!(matches!(error, Error::ReleaseRollbackConflict { .. }));
        assert_eq!(
            fs::read_to_string(absolute).expect("destination"),
            "new concurrent path\n"
        );
        assert_eq!(
            fs::read_to_string(guard).expect("guard"),
            "claimed concurrent edit\n"
        );
    }

    #[test]
    fn release_rollback_restores_a_concurrent_directory_replacement() {
        let (temp, repo) = repo_with_config("");
        let path = temp.path().join("CHANGES.md");
        fs::create_dir(&path).expect("concurrent directory");
        fs::write(path.join("entry"), "concurrent\n").expect("directory entry");

        let error = restore_release_state(
            &repo,
            Path::new("CHANGES.md"),
            &ReleaseFileState::Present(String::from("before\n")),
            &ReleaseFileState::Present(String::from("applied\n")),
        )
        .expect_err("concurrent directory");

        assert!(matches!(error, Error::ReleaseRollbackConflict { .. }));
        assert!(path.is_dir());
        assert_eq!(
            fs::read_to_string(path.join("entry")).expect("directory entry"),
            "concurrent\n"
        );
    }

    #[test]
    fn release_claim_directories_are_uniquely_reserved() {
        let (temp, _repo) = repo_with_config("");
        let destination = temp.path().join("CHANGES.md");

        let first = reserve_release_claim_directory(&destination, &[]).expect("first claim");
        let second = reserve_release_claim_directory(&destination, &[]).expect("second claim");

        assert_ne!(first, second);
        assert!(first.is_dir());
        assert!(second.is_dir());
        fs::remove_dir(first).expect("remove first claim");
        fs::remove_dir(second).expect("remove second claim");
    }

    #[test]
    fn release_compiles_the_exact_fragment_snapshots_it_retains() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment = temp.path().join("changes.d/change.md");
        fs::write(&fragment, " -  Original entry.\n").expect("fragment");
        let (snapshots, parsed) = snapshot_release_fragments(&repo).expect("snapshots");
        fs::write(&fragment, " -  Concurrent replacement.\n").expect("concurrent edit");

        let compiled = compile_parsed_fragments(
            &repo,
            CompileOptions::default(),
            VersionLabel::Unreleased,
            parsed,
        )
        .expect("compile snapshots");

        assert_eq!(snapshots[0].contents, " -  Original entry.\n");
        assert!(compiled.markdown.contains("Original entry."));
        assert!(!compiled.markdown.contains("Concurrent replacement."));
    }

    #[test]
    fn release_rejects_fragment_added_after_planning() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        fs::write(
            temp.path().join("changes.d/planned.md"),
            " -  Planned entry.\n",
        )
        .expect("planned fragment");
        let changelog = compile_unreleased(&repo, CompileOptions::default())
            .expect("compile")
            .markdown;
        fs::write(temp.path().join("CHANGES.md"), &changelog).expect("changelog");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        fs::write(temp.path().join("changes.d/late.md"), " -  Late entry.\n")
            .expect("late fragment");

        let error = apply_release(&repo, plan).expect_err("stale fragment set");

        assert!(
            matches!(error, Error::StaleReleasePlan { path } if path == Path::new("changes.d/late.md"))
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(temp.path().join("changes.d/planned.md").exists());
        assert!(temp.path().join("changes.d/late.md").exists());
    }

    #[test]
    fn release_fragment_set_difference_reports_additions_removals_and_reordering() {
        let a = PathBuf::from("changes.d/a.md");
        let b = PathBuf::from("changes.d/b.md");

        assert_eq!(
            differing_fragment_path(std::slice::from_ref(&a), std::slice::from_ref(&a)),
            None
        );
        assert_eq!(
            differing_fragment_path(std::slice::from_ref(&a), &[a.clone(), b.clone()]),
            Some(b.clone())
        );
        assert_eq!(
            differing_fragment_path(&[a.clone(), b.clone()], std::slice::from_ref(&a)),
            Some(b.clone())
        );
        assert_eq!(
            differing_fragment_path(&[a.clone(), b.clone()], &[b.clone(), a]),
            Some(b)
        );
    }

    #[test]
    fn release_rollback_accepts_an_already_missing_path() {
        let (_temp, repo) = repo_with_config("");

        restore_release_state(
            &repo,
            Path::new("changes.d/already-missing"),
            &ReleaseFileState::Missing,
            &ReleaseFileState::Missing,
        )
        .expect("missing state is already restored");
    }

    proptest! {
        #[test]
        fn release_path_identity_cancels_generated_missing_components(
            missing in "[a-z]{1,12}",
            target in "[a-z]{1,12}",
        ) {
            let (temp, repo) = repo_with_config("");
            let path = PathBuf::from(format!("missing-{missing}/../target-{target}"));

            let identity = release_path_identity(&repo, &path)
                .expect("generated missing component followed by parent");

            prop_assert_eq!(
                identity,
                fs::canonicalize(temp.path())
                    .expect("repository root")
                    .join(format!("target-{target}"))
            );
        }

        #[test]
        fn release_path_identity_keeps_existing_names_below_a_missing_component(
            missing in "[a-z]{1,12}",
            existing in "[a-z]{1,12}",
        ) {
            let (temp, repo) = repo_with_config("");
            let existing = format!("existing-{existing}");
            fs::write(temp.path().join(&existing), "root file\n").expect("existing root file");
            let path = PathBuf::from(format!("missing-{missing}/{existing}"));

            let identity = release_path_identity(&repo, &path)
                .expect("existing name below missing component");

            prop_assert_eq!(
                identity,
                fs::canonicalize(temp.path())
                    .expect("repository root")
                    .join(format!("missing-{missing}"))
                    .join(existing)
            );
        }

        #[test]
        fn release_plan_rejects_any_participant_changed_after_planning(
            participant in 0usize..3,
            replacement in "[A-Za-z]{1,20}\\n",
        ) {
            let (temp, repo) = repo_with_config(
                r#"
                [changelog]
                materialize = false
                "#,
            );
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            let changelog = temp.path().join("CHANGES.md");
            let next = temp.path().join("changes.d/next");
            let fragment = temp.path().join("changes.d/add.md");
            fs::write(&changelog, "Changelog\n=========\n").expect("changelog");
            fs::write(&next, "1.2.0\n").expect("next");
            fs::write(&fragment, " -  Added release.\n").expect("fragment");
            let plan = plan_release(
                &repo,
                ReleaseOptions {
                    version: None,
                    date: release_date(),
                    next: Some(String::from("1.3.0")),
                    allow_empty: false,
                },
            )
            .expect("plan");
            let changed = [&changelog, &next, &fragment][participant];
            fs::write(changed, &replacement).expect("external change");

            let error = apply_release(&repo, plan).expect_err("stale plan");

            let stale = matches!(error, Error::StaleReleasePlan { .. });
            prop_assert!(stale);
            prop_assert_eq!(
                fs::read_to_string(changed).expect("changed file"),
                replacement
            );
        }
    }

    #[test]
    fn release_rejects_consumed_fragment_that_disappeared_after_planning() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment = temp.path().join("changes.d/missing.md");
        fs::write(&fragment, " -  Fixed release.\n").expect("fragment");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        fs::remove_file(fragment).expect("remove fragment");

        let error = apply_release(&repo, plan).expect_err("stale fragment");

        assert!(matches!(error, Error::StaleReleasePlan { .. }));
        assert!(!temp.path().join("CHANGES.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn release_rejects_symlinked_transaction_paths_during_planning() {
        use std::os::unix::fs::symlink;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("real-next"), "1.2.0\n").expect("real next");
        symlink(
            temp.path().join("real-next"),
            temp.path().join("changes.d/next"),
        )
        .expect("next symlink");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");

        let error = plan_release(
            &repo,
            ReleaseOptions {
                version: None,
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect_err("symlink conflict");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOutsideBoundary { .. })
        ));
        assert!(!temp.path().join("CHANGES.md").exists());
        assert!(temp.path().join("changes.d/add.md").exists());
    }

    #[test]
    fn release_rejects_missing_materialized_changelog_without_deleting_fragments() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment = temp.path().join("changes.d/add.md");
        fs::write(&fragment, " -  Added release.\n").expect("fragment");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n").expect("next");
        let changelog = temp.path().join("CHANGES.md");
        fs::write(
            &changelog,
            "Version 1.2.0\n-------------\n\nTo be released.\n\n -  Added release.\n",
        )
        .expect("changelog");
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        fs::remove_file(changelog).expect("remove changelog");

        let error = apply_release(&repo, plan).expect_err("stale changelog");

        assert!(matches!(error, Error::StaleReleasePlan { .. }));
        assert!(fragment.exists());
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
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Project changes\n===============\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Added release.\n"
        );
    }

    #[test]
    fn release_with_relative_repository_root_creates_missing_changelog() {
        let temp = TempDir::new_in(".").expect("relative tempdir");
        let current = std::env::current_dir().expect("current directory");
        let root = temp
            .path()
            .strip_prefix(&current)
            .expect("tempdir below current directory")
            .to_path_buf();
        assert!(root.is_relative());
        fs::write(
            temp.path().join(Repository::CONFIG_FILE),
            r#"
            [changelog]
            materialize = false
            title = "Project changes"
            "#,
        )
        .expect("config");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/add.md"), " -  Added release.\n").expect("fragment");
        let repo = Repository::from_root(&root).expect("relative repository");

        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some(String::from("1.2.0")),
                date: release_date(),
                next: None,
                allow_empty: false,
            },
        )
        .expect("plan");
        apply_release(&repo, plan).expect("release");

        assert!(temp.path().join("CHANGES.md").is_file());
        assert!(!temp.path().join("changes.d/add.md").exists());
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
                date: release_date(),
                next: None,
                allow_empty: false,
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
                date: release_date(),
                next: None,
                allow_empty: false,
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
            "-001-01-01",
            "+001-01-01",
            "2026-+1-01",
            "2026-01-+1",
            "10000-01-01",
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
        assert_eq!(
            ReleaseDate::parse("0000-01-01")
                .expect("minimum four-digit year")
                .year,
            0
        );
        assert_eq!(
            ReleaseDate::parse("9999-12-31")
                .expect("maximum four-digit year")
                .year,
            9999
        );
    }

    #[test]
    fn release_plan_rejects_an_invalid_constructed_date() {
        let (temp, repo) = repo_with_config("[changelog]\nmaterialize = false\n");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fix.md"), " -  Fixed release.\n").expect("fragment");

        for date in [
            ReleaseDate {
                year: -1,
                month: 1,
                day: 1,
            },
            ReleaseDate {
                year: 10_000,
                month: 1,
                day: 1,
            },
            ReleaseDate {
                year: 2026,
                month: 13,
                day: 8,
            },
        ] {
            let error = plan_release(
                &repo,
                ReleaseOptions {
                    version: Some(String::from("1.2.0")),
                    date,
                    next: None,
                    allow_empty: false,
                },
            )
            .expect_err("invalid date");

            assert!(matches!(error, Error::InvalidReleaseDate { .. }));
        }
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
    fn release_insertion_helpers_handle_boundary_spacing() {
        let marker_release_source = "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\n\n\nVersion 1.2.0\n-------------\n\nTo be released.\n\n -  Fixed marker release.\n\n<!-- sacho:unreleased:end -->\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n";
        assert_eq!(
            replace_region_for_release(
                marker_release_source,
                None,
                "Version 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed marker release.\n",
                RegionDetection::Marker,
                "To be released.",
                "Project changes",
            )
            .expect("replace marker release"),
            "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\n\n<!-- sacho:unreleased:end -->\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed marker release.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n"
        );
        assert_eq!(
            remove_unreleased_region(
                "<!-- sacho:unreleased:begin -->\n\n\nUnreleased\n----------\n\nTo be released.\n\n<!-- sacho:unreleased:end -->\n",
                RegionDetection::Marker,
                "To be released.",
            )
            .expect("remove marker region"),
            "<!-- sacho:unreleased:begin -->\n\n<!-- sacho:unreleased:end -->\n"
        );
        assert_eq!(
            insert_released_section("# Changelog", "Version 1.2.0\n-------------\n", "Changelog",),
            "# Changelog\n\nVersion 1.2.0\n-------------\n"
        );
        assert_eq!(
            insert_released_section(
                "Changelog\n=========\n\nVersion 1.1.0\n-------------\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\n\nVersion 1.1.0\n-------------\n"
        );
        assert_eq!(
            insert_released_section(
                "Changelog\n=========\n\n### Version 1.1.0\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\n### Version 1.1.0\n"
        );
        assert_eq!(
            insert_released_section(
                "Changelog\n=========\n\nIntro paragraph.\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nIntro paragraph.\n"
        );
        assert_eq!(
            insert_released_after_marker_region(
                "Header\n<!-- sacho:unreleased:end --> trailer\nVersion 1.1.0\n-------------\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "Header\n<!-- sacho:unreleased:end --> trailer\n\n\nVersion 1.2.0\n-------------\n\n\nVersion 1.1.0\n-------------\n"
        );
        assert_eq!(
            insert_released_after_marker_region(
                "Header\n<!-- sacho:unreleased:end --> trailer\n### Version 1.1.0\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "Header\n<!-- sacho:unreleased:end --> trailer\n\n\nVersion 1.2.0\n-------------\n\n### Version 1.1.0\n"
        );
        assert_eq!(
            insert_released_after_marker_region(
                "<!-- sacho:unreleased:begin -->\n<!-- sacho:unreleased:end -->\n",
                "Version 1.2.0\n-------------\n",
                "Changelog",
            ),
            "<!-- sacho:unreleased:begin -->\n<!-- sacho:unreleased:end -->\n\n\nVersion 1.2.0\n-------------\n"
        );
    }

    #[test]
    fn unreleased_insertion_normalizes_title_and_history_spacing() {
        let unreleased = "Unreleased\n----------\n\nTo be released.\n";

        assert_eq!(
            insert_unreleased_region("Version 1.2.0\n-------------\n", unreleased, "Changelog",),
            "Unreleased\n----------\n\nTo be released.\n\n\nVersion 1.2.0\n-------------\n"
        );
        assert_eq!(
            insert_unreleased_region(
                "# Changelog\nVersion 1.2.0\n-------------\n",
                unreleased,
                "Changelog",
            ),
            "# Changelog\n\nUnreleased\n----------\n\nTo be released.\n\n\nVersion 1.2.0\n-------------\n"
        );
        assert_eq!(
            insert_unreleased_region("# Changelog", unreleased, "Changelog"),
            "# Changelog\n\nUnreleased\n----------\n\nTo be released.\n"
        );
        assert_eq!(
            insert_unreleased_region(
                "# Changelog\n\nVersion 1.2.0\n-------------\n",
                unreleased,
                "Changelog",
            ),
            "# Changelog\n\nUnreleased\n----------\n\nTo be released.\n\n\nVersion 1.2.0\n-------------\n"
        );
        assert_eq!(
            insert_unreleased_region(
                "# Changelog\n\n### Version 1.2.0\n",
                unreleased,
                "Changelog",
            ),
            "# Changelog\n\nUnreleased\n----------\n\nTo be released.\n\n### Version 1.2.0\n"
        );
        assert_eq!(
            insert_unreleased_region("# Changelog\n\nIntro paragraph.\n", unreleased, "Changelog",),
            "# Changelog\n\nUnreleased\n----------\n\nTo be released.\n\nIntro paragraph.\n"
        );
    }

    #[test]
    fn release_line_scanning_helpers_classify_titles_and_blanks() {
        let lines = source_lines_with_offsets("Title\n=====\n\nBody\n");

        assert_eq!(skip_blank_lines(&lines, 2), Some(3));
        assert_eq!(
            insertion_index_after_title("Plain one-line file", "Changelog"),
            0
        );
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
    fn sync_skips_when_materialization_is_disabled() {
        let (temp, repo) = repo_with_config(
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
        assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
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
    fn resolve_links_pins_fragments_and_syncs_generated_output_atomically() {
        let server = TestHttpServer::spawn(vec![
            http_response("302 Found", Some("/pull/1")),
            http_response("200 OK", None),
        ]);
        let (temp, repo) = repo_with_config(&format!(
            "[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment_path = temp.path().join("changes.d/change.md");
        let fragment_before = " -  Fixed links.  [[#1]]\n";
        fs::write(&fragment_path, fragment_before).expect("fragment");
        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let changelog_before = format!("Changelog\n=========\n\n{}", compiled.markdown);
        fs::write(temp.path().join("CHANGES.md"), &changelog_before).expect("changelog");

        let plan = plan_resolve_links(&repo, ResolveLinksOptions::default()).expect("plan");

        assert_eq!(
            plan.changed_fragments,
            vec![PathBuf::from("changes.d/change.md")]
        );
        assert!(matches!(plan.sync, SyncPlan::Apply(_)));
        assert_eq!(
            fs::read_to_string(&fragment_path).expect("fragment"),
            fragment_before
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog_before
        );

        let result = apply_resolve_links(&repo, plan).expect("apply");

        assert!(result.changelog_changed);
        let fragment = fs::read_to_string(&fragment_path).expect("resolved fragment");
        assert!(fragment.contains("'#1':"));
        assert!(fragment.contains(&format!("{}/pull/1", server.base)));
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains(&format!("[#1]: {}/pull/1", server.base)));
        assert_eq!(
            server.finish(),
            vec!["HEAD /issues/1 HTTP/1.1", "HEAD /pull/1 HTTP/1.1"]
        );
    }

    #[test]
    fn resolving_preview_uses_existing_pin_without_writing_or_network() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let source =
            "---\nlinks:\n  \"#1\": https://example.com/pull/1\n---\n -  Fixed links.  [[#1]]\n";
        let path = temp.path().join("changes.d/change.md");
        fs::write(&path, source).expect("fragment");

        let compiled = compile_unreleased_with_link_resolution(
            &repo,
            CompileOptions::default(),
            LinkResolutionPolicy::Always,
        )
        .expect("preview");

        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/pull/1")
        );
        assert_eq!(fs::read_to_string(path).expect("fragment"), source);
    }

    #[test]
    fn resolving_preview_only_requests_links_from_the_selected_section() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core fragments");
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("CLI fragments");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            "---\nlinks:\n  \"#1\": https://example.com/pull/1\n---\n -  Fixed core.  [[#1]]\n",
        )
        .expect("core fragment");
        fs::write(
            temp.path().join("changes.d/cli/change.md"),
            " -  Fixed CLI.  [[#2]]\n",
        )
        .expect("CLI fragment");

        let compiled = compile_unreleased_with_link_resolution(
            &repo,
            CompileOptions {
                section: Some(String::from("core")),
                ..CompileOptions::default()
            },
            LinkResolutionPolicy::Always,
        )
        .expect("selected preview");

        assert!(compiled.markdown.contains("Fixed core."));
        assert!(!compiled.markdown.contains("Fixed CLI."));
        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/pull/1")
        );
    }

    #[test]
    fn resolving_preview_uses_a_pin_from_an_unselected_section() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core fragments");
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("CLI fragments");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            " -  Fixed core.  [[#1]]\n",
        )
        .expect("core fragment");
        fs::write(
            temp.path().join("changes.d/cli/change.md"),
            "---\nlinks:\n  \"#1\": https://example.com/pull/1\n---\n -  Fixed CLI.  [[#1]]\n",
        )
        .expect("CLI fragment");

        let compiled = compile_unreleased_with_link_resolution(
            &repo,
            CompileOptions {
                section: Some(String::from("core")),
                ..CompileOptions::default()
            },
            LinkResolutionPolicy::Always,
        )
        .expect("selected preview");

        assert!(compiled.markdown.contains("Fixed core."));
        assert!(!compiled.markdown.contains("Fixed CLI."));
        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/pull/1")
        );
    }

    proptest! {
        #[test]
        fn resolving_preview_leaves_non_http_references_unpinned(
            template in prop_oneof![
                Just("/issues/{n}"),
                Just("mailto:issue-{n}@example.com"),
            ],
        ) {
            let config = format!(
                "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{template}\"\n"
            );
            let (temp, repo) = repo_with_config(&config);
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            fs::write(
                temp.path().join("changes.d/change.md"),
                " -  Fixed links.  [[#1]]\n",
            )
            .expect("fragment");

            let compiled = compile_unreleased_with_link_resolution(
                &repo,
                CompileOptions::default(),
                LinkResolutionPolicy::Always,
            )
            .expect("preview");

            let expected = format!("[#1]: {}", template.replace("{n}", "1"));
            prop_assert!(compiled.markdown.contains(&expected));
        }
    }

    #[test]
    fn resolve_links_still_pins_fragments_when_materialization_is_disabled() {
        let server = TestHttpServer::spawn(vec![http_response("200 OK", None)]);
        let (temp, repo) = repo_with_config(&format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let path = temp.path().join("changes.d/change.md");
        fs::write(&path, " -  Fixed links.  [[#1]]\n").expect("fragment");

        let plan = plan_resolve_links(&repo, ResolveLinksOptions::default()).expect("plan");
        assert!(matches!(
            plan.sync,
            SyncPlan::Skipped(SyncSkipReason::MaterializationDisabled)
        ));
        apply_resolve_links(&repo, plan).expect("apply");

        assert!(
            fs::read_to_string(path)
                .expect("fragment")
                .contains("links:")
        );
        assert_eq!(server.finish(), vec!["HEAD /issues/1 HTTP/1.1"]);
    }

    #[test]
    fn resolve_links_requests_each_label_once_and_pins_every_using_fragment() {
        let server = TestHttpServer::spawn(vec![http_response("200 OK", None)]);
        let (temp, repo) = repo_with_config(&format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let first = temp.path().join("changes.d/first.md");
        let second = temp.path().join("changes.d/second.md");
        fs::write(&first, " -  Fixed the first path.  [[#1]]\n").expect("first fragment");
        fs::write(&second, " -  Fixed the second path.  [[#1]]\n").expect("second fragment");

        let plan = plan_resolve_links(&repo, ResolveLinksOptions::default()).expect("plan");
        assert_eq!(
            plan.changed_fragments,
            vec![
                PathBuf::from("changes.d/first.md"),
                PathBuf::from("changes.d/second.md"),
            ]
        );
        apply_resolve_links(&repo, plan).expect("apply");

        let expected_url = format!("{}/issues/1", server.base);
        assert!(
            fs::read_to_string(first)
                .expect("first")
                .contains(&expected_url)
        );
        assert!(
            fs::read_to_string(second)
                .expect("second")
                .contains(&expected_url)
        );
        assert_eq!(server.finish(), vec!["HEAD /issues/1 HTTP/1.1"]);
    }

    #[test]
    fn resolve_links_propagates_an_existing_pin_without_network_access() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let first = temp.path().join("changes.d/first.md");
        let second = temp.path().join("changes.d/second.md");
        fs::write(
            &first,
            "---\nlinks:\n  '#1': https://example.com/pull/1\n---\n -  First.  [[#1]]\n",
        )
        .expect("first fragment");
        fs::write(&second, " -  Second.  [[#1]]\n").expect("second fragment");

        let plan = plan_resolve_links(&repo, ResolveLinksOptions::default()).expect("plan");
        assert_eq!(
            plan.changed_fragments,
            vec![PathBuf::from("changes.d/second.md")]
        );
        apply_resolve_links(&repo, plan).expect("apply");

        assert!(
            fs::read_to_string(second)
                .expect("second")
                .contains("https://example.com/pull/1")
        );
    }

    #[test]
    fn resolve_links_rejects_conflicting_existing_pins_before_network_access() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/first.md"),
            "---\nlinks:\n  '#1': https://example.com/issues/1\n---\n -  First.  [[#1]]\n",
        )
        .expect("first fragment");
        fs::write(
            temp.path().join("changes.d/second.md"),
            "---\nlinks:\n  '#1': https://example.com/pull/1\n---\n -  Second.  [[#1]]\n",
        )
        .expect("second fragment");

        let error = plan_resolve_links(&repo, ResolveLinksOptions::default())
            .expect_err("conflicting pins");

        assert!(matches!(
            error,
            Error::ConflictingResolvedLinks { ref label, ref first, ref second }
                if label == "#1"
                    && first == "https://example.com/issues/1"
                    && second == "https://example.com/pull/1"
        ));
    }

    #[test]
    fn resolved_sync_rejects_next_file_changed_after_version_snapshot() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let next = temp.path().join("changes.d/next");
        fs::write(&next, "0.2.0\n").expect("next");
        fs::write(
            temp.path().join("changes.d/change.md"),
            "---\nlinks:\n  '#1': https://example.com/pull/1\n---\n -  Fixed links.  [[#1]]\n",
        )
        .expect("fragment");
        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        fs::write(
            temp.path().join("CHANGES.md"),
            format!("Changelog\n=========\n\n{}", compiled.markdown),
        )
        .expect("changelog");

        let plan = plan_resolve_links_with_snapshot_hook(
            &repo,
            ResolveLinksOptions { force: true },
            || fs::write(&next, "0.3.0\n").expect("concurrent next"),
        )
        .expect("plan");

        let error = apply_resolve_links(&repo, plan).expect_err("stale next snapshot");
        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::ResolveLinks,
                ref path,
            } if path == Path::new("changes.d/next")
        ));
        assert_eq!(fs::read_to_string(next).expect("next"), "0.3.0\n");
    }

    proptest! {
        #[test]
        fn resolved_link_frontmatter_is_idempotent(
            numbers in prop::collection::btree_set(1_u64..10_000, 0..20),
        ) {
            let links = numbers
                .into_iter()
                .map(|number| {
                    (
                        format!("#{number}"),
                        format!("https://example.com/pull/{number}"),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let source = "---\npriority: 2\nlinks:\n  stale: https://example.com/stale\n---\n -  Fixed links.\n";

            let once = source_with_resolved_links(source, &links).expect("first rewrite");
            let twice = source_with_resolved_links(&once, &links).expect("second rewrite");

            prop_assert_eq!(once, twice);
        }
    }

    #[test]
    fn link_resolution_failure_leaves_every_fragment_unchanged() {
        let server = TestHttpServer::spawn(vec![http_response("404 Not Found", None)]);
        let (temp, repo) = repo_with_config(&format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let path = temp.path().join("changes.d/change.md");
        let source = " -  Fixed links.  [[#1]]\n";
        fs::write(&path, source).expect("fragment");

        let error = plan_resolve_links(&repo, ResolveLinksOptions::default())
            .expect_err("resolution failure");

        assert!(matches!(error, Error::LinkResolutionFailed { .. }));
        assert_eq!(fs::read_to_string(path).expect("fragment"), source);
        server.finish();
    }

    #[test]
    fn resolved_link_plan_rejects_a_concurrent_fragment_edit() {
        let server = TestHttpServer::spawn(vec![http_response("200 OK", None)]);
        let (temp, repo) = repo_with_config(&format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{}/issues/{{n}}\"\n",
            server.base
        ));
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let path = temp.path().join("changes.d/change.md");
        fs::write(&path, " -  Fixed links.  [[#1]]\n").expect("fragment");
        let plan = plan_resolve_links(&repo, ResolveLinksOptions::default()).expect("plan");
        let concurrent = " -  Changed concurrently.  [[#1]]\n";
        fs::write(&path, concurrent).expect("concurrent edit");

        let error = apply_resolve_links(&repo, plan).expect_err("stale plan");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::ResolveLinks,
                ..
            }
        ));
        assert_eq!(fs::read_to_string(path).expect("fragment"), concurrent);
        server.finish();
    }

    #[test]
    fn sync_starts_materialized_region_for_new_fragment_after_closed_release() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n",
        )
        .expect("changelog");

        let plan = plan_sync(&repo, SyncOptions::default()).expect("plan");
        assert!(matches!(plan, SyncPlan::Apply(_)));
        let result = apply_sync(&repo, plan).expect("apply");

        assert!(result.changed);
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n\nUnreleased\n----------\n\nTo be released.\n\n -  Fixed sync.\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn sync_does_not_invent_missing_markers_for_an_active_region() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            region-detection = "marker"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n",
        )
        .expect("changelog");

        let error = plan_sync(&repo, SyncOptions::default()).expect_err("missing markers");

        assert!(matches!(error, Error::RegionNotFound { .. }));
    }

    #[test]
    fn sync_rejects_plan_when_changelog_changed_after_planning() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");
        let plan = plan_sync(&repo, SyncOptions { force: true }).expect("plan");
        let changed = "Unreleased\n----------\n\nChanged after prompt.\n";
        fs::write(temp.path().join("CHANGES.md"), changed).expect("change changelog");

        let error = apply_sync(&repo, plan).expect_err("stale plan");

        assert!(matches!(error, Error::StaleSyncPlan { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changed
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
        assert_eq!(result.sync, None);
        assert_eq!(
            fs::read_to_string(temp.path().join(&result.path)).expect("fragment"),
            " -\n"
        );
    }

    #[test]
    fn add_creates_fragment_in_patterned_section() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{scope}/plugin-{name}"
            id = "@{scope}/{name}"
            directory = "{scope}/plugin-{name}"
            "#,
        );

        let result = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("@acme/http")),
                name: String::from("timeouts"),
            },
        )
        .expect("add");

        assert_eq!(
            result.path,
            PathBuf::from("changes.d/acme/plugin-http/timeouts.md")
        );
        assert!(temp.path().join(&result.path).is_file());
    }

    #[test]
    fn add_rejects_an_ambiguous_patterned_section_directory() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "{name}"
            "#,
        );

        let error = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("pkg/core")),
                name: String::from("unsafe"),
            },
        )
        .expect_err("ambiguous patterned section directory");

        let message = error.to_string();
        assert!(message.contains("ambiguous"), "{message}");
        assert!(
            !temp.path().join("changes.d/core/unsafe.md").exists(),
            "add must reject the ambiguous directory before writing"
        );
    }

    #[test]
    fn add_rejects_a_patterned_section_overlapping_the_next_file() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        );

        let error = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("next")),
                name: String::from("unsafe"),
            },
        )
        .expect_err("next-file overlap");

        let message = error.to_string();
        assert!(message.contains("fragments.next-file"), "{message}");
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_rejects_a_patterned_section_symlink_outside_the_repository() {
        use std::os::unix::fs::symlink;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        );
        let outside = TempDir::new().expect("outside tempdir");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        symlink(outside.path(), temp.path().join("changes.d/core")).expect("outside symlink");

        let error = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("core")),
                name: String::from("unsafe"),
            },
        )
        .expect_err("outside patterned section");

        let message = error.to_string();
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
        assert!(message.contains("fragments.directory"), "{message}");
        assert!(!outside.path().join("unsafe.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn add_rejects_a_patterned_section_symlink_alias() {
        use std::os::unix::fs::symlink;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/actual")).expect("actual directory");
        symlink("actual", temp.path().join("changes.d/core")).expect("internal symlink");

        let error = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("core")),
                name: String::from("safe"),
            },
        )
        .expect_err("patterned section alias");

        let message = error.to_string();
        assert!(message.contains("exactly its rendered path"), "{message}");
        assert!(!temp.path().join("changes.d/actual/safe.md").exists());
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
        assert_eq!(result.sync, None);
        assert_eq!(
            fs::read_to_string(temp.path().join(&result.path)).expect("fragment"),
            " -\n"
        );
    }

    #[test]
    fn add_syncs_materialized_changelog_without_sections() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let result = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function"),
            },
        )
        .expect("add");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains(" -\n"));
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn add_starts_materialized_region_after_closed_release() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n",
        )
        .expect("changelog");

        let result = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("next-change"),
            },
        )
        .expect("add");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n\nUnreleased\n----------\n\nTo be released.\n\n -\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn add_syncs_materialized_changelog_with_sections() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "core"
            directory = "core"
            "#,
        );
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let result = add_fragment(
            &repo,
            AddOptions {
                section: Some(String::from("core")),
                name: String::from("clear-function"),
            },
        )
        .expect("add");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert!(changelog.contains("### core\n"));
        assert!(changelog.contains(" -\n"));
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn add_rejects_stale_materialized_changelog_before_creating_fragment() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\n -  Hand edit.\n",
        )
        .expect("changelog");

        let error = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function"),
            },
        )
        .expect_err("stale changelog");

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert!(!temp.path().join("changes.d/clear-function.md").exists());
    }

    #[test]
    fn add_does_not_create_fragment_when_existing_fragments_do_not_compile() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/invalid.md"), "not a list\n")
            .expect("invalid fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function"),
            },
        )
        .expect_err("invalid existing fragment");

        assert!(!temp.path().join("changes.d/clear-function.md").exists());
    }

    #[test]
    fn add_ignores_a_legacy_temporary_path_collision() {
        let (temp, repo) = repo_with_config("");
        let original = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), original).expect("changelog");
        fs::create_dir(temp.path().join("CHANGES.md.tmp")).expect("blocking temp path");

        let result = add_fragment(
            &repo,
            AddOptions {
                section: None,
                name: String::from("clear-function"),
            },
        )
        .expect("add");

        assert_eq!(result.path, Path::new("changes.d/clear-function.md"));
        assert!(temp.path().join("changes.d/clear-function.md").exists());
        assert!(temp.path().join("CHANGES.md.tmp").is_dir());
        assert_ne!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            original
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
    fn next_rejects_multiple_nonempty_version_lines_before_writing() {
        for materialize in [false, true] {
            let config = format!("[changelog]\nmaterialize = {materialize}\n",);
            let (temp, repo) = repo_with_config(&config);
            let changelog = "Unreleased\n----------\n\nTo be released.\n";
            if materialize {
                fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
            }

            let error = set_next_version(
                &repo,
                NextOptions {
                    version: String::from("1.2.0\n2.0.0"),
                },
            )
            .expect_err("multiple next versions");

            assert!(matches!(
                error,
                Error::InvalidNextVersion { path }
                    if path == temp.path().join("changes.d/next")
            ));
            assert!(!temp.path().join("changes.d/next").exists());
            if materialize {
                assert_eq!(
                    fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
                    changelog
                );
                assert!(
                    check(&repo, CheckOptions::default())
                        .expect("check")
                        .is_clean()
                );
            }
        }
    }

    #[test]
    fn next_ignores_a_malformed_fragment_without_materialization() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let malformed = temp.path().join("changes.d/malformed.md");
        fs::write(&malformed, "not a fragment\n").expect("malformed fragment");

        let result = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next ignores malformed fragment");

        assert_eq!(result.sync, None);
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert_eq!(
            fs::read_to_string(malformed).expect("malformed fragment"),
            "not a fragment\n"
        );
    }

    #[test]
    fn next_overwrites_an_invalid_utf8_next_file_without_materialization() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let next_file = temp.path().join("changes.d/next");
        fs::write(&next_file, [0xff]).expect("invalid UTF-8 next file");

        let result = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next replaces invalid UTF-8 file");

        assert_eq!(result.sync, None);
        assert_eq!(fs::read(next_file).expect("next file"), b"1.2.0\n");
    }

    #[test]
    fn next_rollback_restores_invalid_utf8_bytes() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let next_file = temp.path().join("changes.d/next");
        fs::write(&next_file, [0xff, 0xfe]).expect("invalid UTF-8 next file");
        set_release_failpoint(
            ReleaseApplyStage::DiscardClaimed,
            PathBuf::from("changes.d/next"),
        );

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect_err("commit preflight failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Next,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert_eq!(
            fs::read(next_file).expect("restored next file"),
            [0xff, 0xfe]
        );
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn next_rejects_the_repository_mutation_lock_as_its_destination() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join(Repository::CONFIG_FILE),
            r#"
            [changelog]
            materialize = false

            [fragments]
            directory = ".sacho.lock"
            "#,
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("mutation lock destination");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(source.as_ref(), crate::ConfigError::ConfigPathOverlap { .. })
        ));
        assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
    }

    #[test]
    fn sync_rejects_the_repository_mutation_lock_as_its_destination() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join(Repository::CONFIG_FILE),
            r#"
            [changelog]
            path = ".sacho.lock"
            materialize = true
            "#,
        )
        .expect("config");
        let path = PathBuf::from(MUTATION_LOCK_FILE);

        let error = Repository::from_root(temp.path()).expect_err("mutation lock destination");

        assert!(matches!(
            error,
            Error::Config { source, .. }
                if matches!(
                    source.as_ref(),
                    crate::ConfigError::ConfigPathOverlap { first_path, .. }
                        if first_path == &path
                )
        ));
        assert!(!temp.path().join(MUTATION_LOCK_FILE).exists());
    }

    #[test]
    fn next_updates_heading_when_materialized() {
        let (temp, repo) = repo_with_config("");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");

        let result = set_next_version(
            &repo,
            NextOptions {
                version: String::from(" 1.2.0 "),
            },
        )
        .expect("next");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.2.0\n"
        );
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("changelog")
                .contains("Version 1.2.0\n-------------")
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn next_preserves_marker_changelog_markdown_normal_form() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            region-detection = "marker"
            "#,
        );
        let changelog = "Project changes\n===============\n\n<!-- sacho:unreleased:begin -->\n\n\nUnreleased\n----------\n\nTo be released.\n\n<!-- sacho:unreleased:end -->\n";
        assert_eq!(
            format_markdown(changelog).expect("format changelog"),
            changelog
        );
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn next_preserves_changelog_section_spacing_with_released_history() {
        let (temp, repo) = repo_with_config("");
        let changelog = "Project changes\n===============\n\nUnreleased\n----------\n\nTo be released.\n\n\nVersion 1.1.0\n-------------\n\nReleased on July 1, 2026.\n";
        assert_eq!(
            format_markdown(changelog).expect("format changelog"),
            changelog
        );
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn next_preserves_atx_history_markdown_normal_form() {
        let (temp, repo) = repo_with_config("");
        let changelog = "Project changes\n===============\n\nUnreleased\n----------\n\nTo be released.\n\n### Version 1.1.0\n\nReleased on July 1, 2026.\n";
        assert_eq!(
            format_markdown(changelog).expect("format changelog"),
            changelog
        );
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[test]
    fn next_preserves_html_comment_suffix_markdown_normal_form() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let changelog = "Project changes\n===============\n\n<!--\n---\n-->\n";
        assert_eq!(
            format_markdown(changelog).expect("format changelog"),
            changelog
        );
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect("next");

        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
        assert_eq!(
            format_markdown(&changelog).expect("format changelog"),
            changelog
        );
        assert!(changelog.contains("To be released.\n\n<!--\n---\n-->"));
    }

    #[test]
    fn next_starts_materialized_region_after_closed_release() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Changelog\n=========\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n",
        )
        .expect("changelog");

        let result = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.3.0"),
            },
        )
        .expect("next");

        assert_eq!(result.sync, Some(SyncResult { changed: true }));
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            "Changelog\n=========\n\nVersion 1.3.0\n-------------\n\nTo be released.\n\n\nVersion 1.2.0\n-------------\n\nReleased on July 8, 2026.\n"
        );
        let report = check(&repo, CheckOptions::default()).expect("check");
        assert!(report.is_clean(), "{report:?}");
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
    fn next_reports_a_missing_materialized_changelog_instead_of_panicking() {
        let (temp, repo) = repo_with_config("");

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.2.0"),
            },
        )
        .expect_err("missing changelog");

        assert!(matches!(
            error,
            Error::ReadFile { path, source }
                if path == temp.path().join("CHANGES.md")
                    && source.kind() == ErrorKind::NotFound
        ));
        assert!(!temp.path().join("changes.d/next").exists());
    }

    #[test]
    fn next_sync_failure_restores_the_original_next_file() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        let changelog = "Version 1.0.0\n-------------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("CHANGES.md"));

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.1.0"),
            },
        )
        .expect_err("injected sync failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Next,
                rollback_failures,
                ..
            } if rollback_failures.is_empty()
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
            "1.0.0\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn next_rollback_preserves_an_external_edit_and_reports_the_conflict() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.0.0\n-------------\n\nTo be released.\n",
        )
        .expect("changelog");
        set_release_interference(
            ReleaseApplyStage::Apply,
            "CHANGES.md",
            "changes.d/next",
            "external edit\n",
        );
        set_release_failpoint(ReleaseApplyStage::Apply, PathBuf::from("CHANGES.md"));

        let error = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.1.0"),
            },
        )
        .expect_err("rollback conflict");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Next,
                rollback_failures,
                ..
            } if rollback_failures.len() == 1
                && rollback_failures[0].contains("changed after next wrote it")
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/next")).expect("external next"),
            "external edit\n"
        );
    }

    #[test]
    fn next_reports_cleanup_failure_as_a_success_warning() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Version 1.0.0\n-------------\n\nTo be released.\n",
        )
        .expect("changelog");
        set_release_failpoint(
            ReleaseApplyStage::CommittedDiscardClaimed,
            PathBuf::from("changes.d/next"),
        );

        let result = set_next_version(
            &repo,
            NextOptions {
                version: String::from("1.1.0"),
            },
        )
        .expect("committed next");
        clear_release_failpoint();

        assert_eq!(result.cleanup_warnings.len(), 1);
        assert_eq!(result.cleanup_warnings[0].path, Path::new("changes.d/next"));
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
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
    fn check_allows_unpinned_links_without_network_when_resolution_is_enabled() {
        let (temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "http://127.0.0.1:1/issues/{n}"

            [link-resolution]
            enabled = true
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/change.md"),
            " -  Fixed links.  [[#1]]\n",
        )
        .expect("fragment");

        let report = check(&repo, CheckOptions::default()).expect("check");

        assert!(report.is_clean());
    }

    #[test]
    fn check_rejects_conflicting_pins_when_materialization_is_disabled() {
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
            temp.path().join("changes.d/one.md"),
            "---\nlinks:\n  '#1': https://example.com/pull/1\n---\n -  Fixed one. [[#1]]\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("changes.d/two.md"),
            "---\nlinks:\n  '#1': https://example.net/pull/1\n---\n -  Fixed two. [[#1]]\n",
        )
        .expect("fragment");

        let error = check(&repo, CheckOptions::default()).expect_err("conflicting pins");

        assert!(matches!(error, Error::ConflictingResolvedLinks { .. }));
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
    fn format_plan_does_not_change_files_before_confirmation() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\n -  Hand edited entry.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        let plan = plan_format(&repo, FormatOptions).expect("format plan");

        assert!(matches!(plan.sync, SyncPlan::NeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn format_fragments_requires_confirmation_without_changing_files() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        let error = format_fragments(&repo, FormatOptions).expect_err("confirmation required");

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn applying_confirmed_format_plan_updates_fragments_and_changelog() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");

        let result = apply_format(&repo, plan).expect("apply format");

        assert_eq!(result.changed, vec![PathBuf::from("changes.d/fmt.md")]);
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            " -  Fixed issue.\n"
        );
        assert!(
            fs::read_to_string(temp.path().join("CHANGES.md"))
                .expect("changelog")
                .contains(" -  Fixed issue.\n")
        );
        assert!(
            check(&repo, CheckOptions::default())
                .expect("check")
                .is_clean()
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_op_format_accepts_a_read_only_fragment_directory() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        let fragments = temp.path().join("changes.d");
        fs::create_dir_all(&fragments).expect("fragments dir");
        fs::write(fragments.join("clean.md"), " -  Already normalized.\n").expect("fragment");
        let original_permissions = fs::metadata(&fragments)
            .expect("fragment directory metadata")
            .permissions();
        fs::set_permissions(&fragments, fs::Permissions::from_mode(0o555))
            .expect("read-only fragments");

        let result = format_fragments(&repo, FormatOptions);

        fs::set_permissions(&fragments, original_permissions).expect("restore fragments");
        assert_eq!(result.expect("no-op format").changed, Vec::<PathBuf>::new());
    }

    #[cfg(unix)]
    #[test]
    fn format_does_not_probe_an_unchanged_read_only_section_for_writes() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "writable"
            directory = "writable"

            [[sections]]
            id = "read-only"
            directory = "read-only"
            "#,
        );
        let writable = temp.path().join("changes.d/writable");
        let read_only = temp.path().join("changes.d/read-only");
        fs::create_dir_all(&writable).expect("writable section");
        fs::create_dir_all(&read_only).expect("read-only section");
        fs::write(writable.join("change.md"), "- Needs formatting.\n").expect("changed fragment");
        fs::write(read_only.join("clean.md"), " -  Already normalized.\n")
            .expect("unchanged fragment");
        let original_permissions = fs::metadata(&read_only)
            .expect("read-only section metadata")
            .permissions();
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555))
            .expect("read-only section");

        let result = format_fragments(&repo, FormatOptions);

        fs::set_permissions(&read_only, original_permissions).expect("restore section");
        assert_eq!(
            result.expect("format").changed,
            vec![PathBuf::from("changes.d/writable/change.md")]
        );
        assert_eq!(
            fs::read_to_string(read_only.join("clean.md")).expect("unchanged fragment"),
            " -  Already normalized.\n"
        );
    }

    #[test]
    fn format_apply_rejects_a_fragment_changed_after_planning_without_other_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/a.md"), "- Added first.\n").expect("first");
        fs::write(temp.path().join("changes.d/b.md"), "- Added second.\n").expect("second");
        let changelog = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        fs::write(temp.path().join("changes.d/b.md"), " -  Concurrent edit.\n")
            .expect("concurrent edit");

        let error = apply_format(&repo, plan).expect_err("stale format plan");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::Format,
                path,
            } if path == Path::new("changes.d/b.md")
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/a.md")).expect("first"),
            "- Added first.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn format_apply_classifies_invalid_utf8_after_planning_as_stale() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let fragment = temp.path().join("changes.d/change.md");
        fs::write(&fragment, "- Needs formatting.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        fs::write(&fragment, [0xff]).expect("invalid UTF-8 replacement");

        let error = apply_format(&repo, plan).expect_err("stale format plan");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::Format,
                path,
            } if path == Path::new("changes.d/change.md")
        ));
        assert_eq!(fs::read(fragment).expect("concurrent contents"), [0xff]);
    }

    #[test]
    fn format_move_probe_failure_changes_nothing() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let first = "- Added first.\n";
        let second = "- Added second.\n";
        let changelog = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("changes.d/a.md"), first).expect("first");
        fs::write(temp.path().join("changes.d/b.md"), second).expect("second");
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        set_release_failpoint(ReleaseApplyStage::Probe, PathBuf::from("changes.d/a.md"));

        let error = apply_format(&repo, plan).expect_err("move probe failure");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationTransactionUnsupported {
                command: MutationCommand::Format,
            }
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/a.md")).expect("first"),
            first
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b.md")).expect("second"),
            second
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
        assert_no_release_artifacts(temp.path());
    }

    #[test]
    fn format_apply_preserves_a_fragment_changed_after_preflight_validation() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/a.md"), "- Added first.\n").expect("first");
        fs::write(temp.path().join("changes.d/b.md"), "- Added second.\n").expect("second");
        let changelog = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        let concurrent = " -  Concurrent edit.\n";
        set_release_interference(
            ReleaseApplyStage::Apply,
            "changes.d/b.md",
            "changes.d/b.md",
            concurrent,
        );

        let error = apply_format(&repo, plan).expect_err("stale format plan");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Format,
                ..
            }
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/a.md")).expect("first"),
            "- Added first.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b.md")).expect("second"),
            concurrent
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn format_apply_preserves_an_atomic_save_after_claim_and_rolls_back() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/a.md"), "- Added first.\n").expect("first");
        fs::write(temp.path().join("changes.d/b.md"), "- Added second.\n").expect("second");
        let changelog = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        let concurrent = " -  Concurrent atomic save.\n";
        set_release_interference(
            ReleaseApplyStage::ApplyClaimed,
            "changes.d/b.md",
            "changes.d/b.md",
            concurrent,
        );

        let error = apply_format(&repo, plan).expect_err("stale format plan");
        clear_release_failpoint();

        assert!(matches!(
            error,
            Error::MutationApply {
                command: MutationCommand::Format,
                ..
            }
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/a.md")).expect("first"),
            "- Added first.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b.md")).expect("second"),
            concurrent
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
        assert!(
            fs::read_dir(temp.path().join("changes.d"))
                .expect("fragments")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sacho-claim-"))
        );
    }

    #[test]
    fn format_apply_rejects_a_fragment_added_after_planning_without_other_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/planned.md"),
            "- Added planned.\n",
        )
        .expect("planned");
        let changelog = "Unreleased\n----------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        fs::write(temp.path().join("changes.d/late.md"), " -  Added late.\n")
            .expect("late fragment");

        let error = apply_format(&repo, plan).expect_err("stale fragment set");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::Format,
                path,
            } if path == Path::new("changes.d/late.md")
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/planned.md")).expect("planned"),
            "- Added planned.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn format_apply_rejects_next_file_changed_after_planning_without_other_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        let changelog = "Version 1.0.0\n-------------\n\nTo be released.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        fs::write(temp.path().join("changes.d/next"), "2.0.0\n").expect("concurrent next");

        let error = apply_format(&repo, plan).expect_err("stale next file");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::Format,
                path,
            } if path == Path::new("changes.d/next")
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    #[test]
    fn format_apply_rejects_changelog_changed_after_planning_without_fragment_writes() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("changelog");
        let plan = plan_format(&repo, FormatOptions).expect("format plan");
        let concurrent = "Unreleased\n----------\n\nTo be released.\n\nConcurrent edit.\n";
        fs::write(temp.path().join("CHANGES.md"), concurrent).expect("concurrent changelog");

        let error = apply_format(&repo, plan).expect_err("stale changelog");

        assert!(matches!(
            error,
            Error::StaleMutationPlan {
                command: MutationCommand::Format,
                path,
            } if path == Path::new("CHANGES.md")
        ));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            concurrent
        );
    }

    #[test]
    fn format_fragments_without_materialization_keeps_simple_formatting_behavior() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");

        let result = format_fragments(&repo, FormatOptions).expect("format fragments");

        assert_eq!(result.changed, vec![PathBuf::from("changes.d/fmt.md")]);
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
            " -  Fixed issue.\n"
        );
    }

    #[test]
    fn planning_format_does_not_mutate_before_sync_confirmation() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
        fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n",
        )
        .expect("changelog");

        let prepared = plan_format(&repo, FormatOptions).expect("format plan");

        assert!(matches!(prepared.sync, SyncPlan::NeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
    }

    #[test]
    fn check_fix_requires_confirmation_without_changing_files() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
        let changelog = "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n";
        fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

        let error = check(
            &repo,
            CheckOptions {
                fix: true,
                ..CheckOptions::default()
            },
        )
        .expect_err("confirmation required");

        assert!(matches!(error, Error::SyncNeedsConfirmation { .. }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
            "- Fixed issue.\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
            changelog
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn format_plans_are_pure_and_idempotent(
            entries in prop::collection::vec("[A-Za-z]{1,16}", 1..6),
        ) {
            let (temp, repo) = repo_with_config("");
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            for (index, entry) in entries.iter().enumerate() {
                fs::write(
                    temp.path().join(format!("changes.d/{index}.md")),
                    format!("- Added {entry}.\n"),
                )
                .expect("fragment");
            }
            let changelog = "Unreleased\n----------\n\nTo be released.\n";
            fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
            let before = (0..entries.len())
                .map(|index| {
                    fs::read_to_string(temp.path().join(format!("changes.d/{index}.md")))
                        .expect("fragment")
                })
                .collect::<Vec<_>>();

            let plan = plan_format(&repo, FormatOptions).expect("format plan");

            let still_before = (0..entries.len())
                .map(|index| {
                    fs::read_to_string(temp.path().join(format!("changes.d/{index}.md")))
                        .expect("fragment")
                })
                .collect::<Vec<_>>();
            prop_assert_eq!(still_before, before);
            prop_assert_eq!(
                fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
                changelog,
            );

            apply_format(&repo, plan).expect("apply format");
            prop_assert!(check(&repo, CheckOptions::default()).expect("check").is_clean());
            let formatted = (0..entries.len())
                .map(|index| {
                    fs::read_to_string(temp.path().join(format!("changes.d/{index}.md")))
                        .expect("fragment")
                })
                .collect::<Vec<_>>();
            let second = plan_format(&repo, FormatOptions).expect("second plan");
            prop_assert!(second.formatting.changed.is_empty());
            prop_assert!(matches!(
                &second.sync,
                SyncPlan::Skipped(SyncSkipReason::AlreadyCurrent)
            ));
            apply_format(&repo, second).expect("apply second plan");
            let after_second = (0..entries.len())
                .map(|index| {
                    fs::read_to_string(temp.path().join(format!("changes.d/{index}.md")))
                        .expect("fragment")
                })
                .collect::<Vec<_>>();
            prop_assert_eq!(after_second, formatted);
        }
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
    fn format_fragment_source_sorts_resolved_links() {
        let formatted = format_fragment_source(
            "---\nlinks:\n  \"#2\": https://example.com/issues/2\n  \"#1\": https://example.com/pull/1\n---\n- Fixed links. [[#1], [#2]]\n",
        )
        .expect("format");

        assert_eq!(
            formatted,
            "---\nlinks:\n  '#1': https://example.com/pull/1\n  '#2': https://example.com/issues/2\n---\n -  Fixed links. [[#1], [#2]]\n"
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
            } if path == Path::new("changes.d/bad-priority.md")
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
                staged: false,
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
    fn check_fix_preserves_post_commit_cleanup_warnings() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
        set_release_failpoint(
            ReleaseApplyStage::CommittedDiscardClaimed,
            PathBuf::from("changes.d/fix.md"),
        );

        let report = check(
            &repo,
            CheckOptions {
                fix: true,
                base: None,
                staged: false,
            },
        )
        .expect("committed check --fix");
        clear_release_failpoint();

        assert_eq!(report.status(), CheckStatus::HasWarnings);
        assert!(report.warnings.iter().any(|warning| {
            warning
                .message
                .contains("check --fix committed successfully")
                && warning.message.contains("changes.d/fix.md")
                && warning.message.contains("cleanup failed")
        }));
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
            " -  Fixed issue.\n"
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
            skipped.message.contains(
                "missing-fragment check skipped because neither --base nor --staged was supplied",
            )
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
    fn staged_layer_three_requires_a_staged_fragment_and_ignores_unstaged_source() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        init_test_git_repository(temp.path());
        git(temp.path(), ["add", "sacho.toml"]).expect("stage config");
        git(
            temp.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "Initial config",
            ],
        )
        .expect("initial commit");
        fs::create_dir_all(temp.path().join("src")).expect("src dir");
        fs::write(temp.path().join("src/lib.rs"), "pub fn changed() {}\n").expect("source");
        git(temp.path(), ["add", "src/lib.rs"]).expect("stage source");
        let vcs = GitVcs::new(temp.path());

        let missing = staged_missing_fragment_violations(&repo, &vcs).expect("staged check");
        assert_eq!(missing.violations.len(), 1);
        assert!(missing.violations[0].message.starts_with("staged changes:"));
        assert!(!missing.violations[0].message.contains("Changelog: none"));

        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/changed.md"),
            " -  Changed public behavior.\n",
        )
        .expect("fragment");
        let unstaged_fragment =
            staged_missing_fragment_violations(&repo, &vcs).expect("unstaged fragment check");
        assert_eq!(unstaged_fragment.violations.len(), 1);

        git(temp.path(), ["add", "changes.d/changed.md"]).expect("stage fragment");
        fs::write(
            temp.path().join("src/unstaged.rs"),
            "pub fn unrelated() {}\n",
        )
        .expect("unstaged source");
        let covered = staged_missing_fragment_violations(&repo, &vcs).expect("covered check");
        assert!(covered.violations.is_empty());
    }

    #[test]
    fn staged_layer_three_enforces_section_attribution() {
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

            [[sections]]
            id = "logtape"
            directory = "logtape"
            paths = ["packages/logtape/**"]
            "#,
        );
        init_test_git_repository(temp.path());
        git(temp.path(), ["add", "sacho.toml"]).expect("stage config");
        git(
            temp.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "Initial config",
            ],
        )
        .expect("initial commit");
        fs::create_dir_all(temp.path().join("packages/core")).expect("source dir");
        fs::create_dir_all(temp.path().join("changes.d/logtape")).expect("fragment dir");
        fs::write(
            temp.path().join("packages/core/lib.rs"),
            "pub fn changed() {}\n",
        )
        .expect("source");
        fs::write(
            temp.path().join("changes.d/logtape/changed.md"),
            " -  Changed LogTape behavior.\n",
        )
        .expect("fragment");
        git(temp.path(), ["add", "."]).expect("stage changes");

        let report = staged_missing_fragment_violations(&repo, &GitVcs::new(temp.path()))
            .expect("staged check");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.contains("section \"core\""));
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
    fn layer_three_requirements_follow_section_declaration_order() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "zeta"
            directory = "zeta"
            paths = ["packages/zeta/**"]

            [[sections]]
            id = "alpha"
            directory = "alpha"
            paths = ["packages/alpha/**"]
            "#,
        );

        let requirements = missing_fragment_requirements(
            &repo,
            &[
                PathBuf::from("packages/alpha/src/lib.rs"),
                PathBuf::from("packages/zeta/src/lib.rs"),
            ],
        )
        .expect("requirements");

        assert_eq!(
            requirements
                .requirements
                .iter()
                .map(|requirement| &requirement.target)
                .collect::<Vec<_>>(),
            [
                &FragmentTarget::Section(String::from("zeta")),
                &FragmentTarget::Section(String::from("alpha")),
            ]
        );
    }

    #[test]
    fn layer_three_attributes_patterned_and_deleted_packages() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/**"]

            [[section-patterns]]
            source = "packages/{name}"
            id = "@acme/{name}"
            directory = "{name}"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/core/remove.md"),
            " -  Removed the legacy API.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/core/src/legacy.rs", "changes.d/core/remove.md"],
                "Remove legacy API",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
    }

    #[test]
    fn layer_three_keeps_pattern_family_files_at_repository_level() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/**"]

            [[section-patterns]]
            source = "packages/{name}"
            id = "@acme/{name}"
            directory = "{name}"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("fragment directory");
        fs::write(
            temp.path().join("changes.d/core/readme.md"),
            " -  Documented the package family.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/README.md", "changes.d/core/readme.md"],
                "Document the package family",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert!(report.violations.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn layer_three_rejects_an_ambiguous_patterned_section_directory() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "{name}"
            "#,
        );

        let error =
            missing_fragment_requirements(&repo, &[PathBuf::from("packages/core/src/lib.rs")])
                .expect_err("ambiguous patterned section directory");

        let message = error.to_string();
        assert!(message.contains("ambiguous"), "{message}");
    }

    #[test]
    fn layer_three_rejects_a_patterned_section_overlapping_the_next_file() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        );

        let error =
            missing_fragment_requirements(&repo, &[PathBuf::from("packages/next/src/lib.rs")])
                .expect_err("next-file overlap");

        let message = error.to_string();
        assert!(message.contains("fragments.next-file"), "{message}");
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
    }

    #[test]
    fn layer_three_rejects_a_fragment_from_another_pattern_instance() {
        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["packages/**"]

            [[section-patterns]]
            source = "packages/{name}"
            id = "@acme/{name}"
            directory = "{name}"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("fragment dir");
        fs::write(
            temp.path().join("changes.d/cli/change.md"),
            " -  Changed the CLI.\n",
        )
        .expect("fragment");
        let vcs = FakeVcs {
            commits: vec![fake_commit(
                "a1",
                &["packages/core/src/lib.rs", "changes.d/cli/change.md"],
                "Change core",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.contains("@acme/core"));
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
    fn layer_three_checks_rename_origins_from_every_merge_parent() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [check]
            paths = ["src/**"]
            "#,
        );
        let mut merged =
            ChangedPath::with_old_path_and_similarity("docs/c", ChangeKind::Renamed, "docs/b", 100);
        merged.rename_origins.push(PathBuf::from("src/a"));
        let vcs = FakeVcs {
            commits: vec![fake_commit_with_paths(
                "a1",
                vec![merged],
                "Resolve moved files",
            )],
        };

        let report = missing_fragment_violations(&repo, &vcs, "main").expect("layer three");

        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].message.contains("src/a"));
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
        fn successful_add_leaves_layer_one_and_two_clean(
            name in "[a-z][a-z0-9-]{0,16}",
            with_sections in any::<bool>(),
        ) {
            let config = if with_sections {
                r#"
                [[sections]]
                id = "core"
                directory = "core"
                "#
            } else {
                ""
            };
            let (temp, repo) = repo_with_config(config);
            fs::write(
                temp.path().join("CHANGES.md"),
                "Unreleased\n----------\n\nTo be released.\n",
            )
            .expect("changelog");

            add_fragment(
                &repo,
                AddOptions {
                    section: with_sections.then(|| String::from("core")),
                    name,
                },
            )
            .expect("add");

            let report = check(&repo, CheckOptions::default()).expect("check");
            prop_assert!(report.is_clean(), "{report:?}");
        }

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
