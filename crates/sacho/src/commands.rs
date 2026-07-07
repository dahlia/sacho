use std::fs;
use std::path::PathBuf;

use similar::TextDiff;

use crate::changelog::{ChangelogError, replace_unreleased_region};
use crate::error::{Error, Result};
use crate::fragment::{discover_fragment_candidates, parse_fragment};
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
}

impl CheckReport {
    /// Returns true when the report contains no violations.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }
}

/// A single check violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckViolation {
    /// Human-readable violation message.
    pub message: String,
}

/// Creates a changelog fragment.
pub fn add_fragment(_repo: &Repository, _options: AddOptions) -> Result<AddResult> {
    Err(Error::UnsupportedCommand { command: "add" })
}

/// Sets the next unreleased version.
pub fn set_next_version(_repo: &Repository, _options: NextOptions) -> Result<NextResult> {
    Err(Error::UnsupportedCommand { command: "next" })
}

/// Formats all changelog fragments into normal form.
pub fn format_fragments(_repo: &Repository, _options: FormatOptions) -> Result<FormatResult> {
    Err(Error::UnsupportedCommand { command: "fmt" })
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
pub fn check(repo: &Repository, _options: CheckOptions) -> Result<CheckReport> {
    let discovered = discover_fragment_candidates(repo)?;
    let mut violations = Vec::new();

    for candidate in discovered.candidates {
        let source = fs::read_to_string(&candidate.path).map_err(|source| Error::ReadFile {
            path: candidate.path.clone(),
            source,
        })?;
        if let Err(error) = parse_fragment(
            candidate.relative_path.clone(),
            &source,
            candidate.section,
            &repo.config().links,
        ) {
            violations.push(CheckViolation {
                message: format!("{}: {error}", candidate.relative_path.display()),
            });
        }
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
            SyncPlan::Skipped(_) => {}
        }
    }

    Ok(CheckReport { violations })
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
            }
            .is_clean()
        );
    }

    #[test]
    fn unimplemented_commands_report_unsupported_command() {
        let (_temp, repo) = repo_with_config("");

        let format_error =
            format_fragments(&repo, FormatOptions).expect_err("format is unsupported");
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
            format_error,
            Error::UnsupportedCommand { command: "fmt" }
        ));
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
}
