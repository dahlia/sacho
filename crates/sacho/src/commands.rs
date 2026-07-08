use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use hongdown::{
    DashSetting, IndentWidth, LeadingSpaces, LineWidth, Options as HongdownOptions, TrailingSpaces,
    UnorderedMarker, format,
};
use serde_yaml_ng::{Mapping, Value};
use similar::TextDiff;

use crate::changelog::{ChangelogError, replace_unreleased_region};
use crate::error::{Error, Result};
use crate::fragment::{
    DiscoveryWarning, FragmentWarning, discover_fragment_candidates, parse_fragment,
};
use crate::repo::Repository;

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
    /// Fragment paths changed by the carry operation.
    pub changed_paths: Vec<PathBuf>,
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
pub fn plan_release(_repo: &Repository, _options: ReleaseOptions) -> Result<ReleasePlan> {
    Err(Error::UnsupportedCommand { command: "release" })
}

/// Applies a previously planned release.
pub fn apply_release(_repo: &Repository, _plan: ReleasePlan) -> Result<ReleaseResult> {
    Err(Error::UnsupportedCommand { command: "release" })
}

/// Carries entries from an existing release into unreleased fragments.
pub fn carry(_repo: &Repository, _options: CarryOptions) -> Result<CarryResult> {
    Err(Error::UnsupportedCommand { command: "carry" })
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

    Ok(CheckReport {
        violations,
        warnings,
        skipped,
    })
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
    fn commands_outside_milestone_report_unsupported_command() {
        let (_temp, repo) = repo_with_config("");

        let release_error = apply_release(
            &repo,
            ReleasePlan {
                version: String::from("1.0.0"),
            },
        )
        .expect_err("release is unsupported");
        let carry_error = carry(
            &repo,
            CarryOptions {
                version: String::from("1.0.0"),
            },
        )
        .expect_err("carry is unsupported");

        assert!(matches!(
            release_error,
            Error::UnsupportedCommand { command: "release" }
        ));
        assert!(matches!(
            carry_error,
            Error::UnsupportedCommand { command: "carry" }
        ));
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
