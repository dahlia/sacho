use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::repo::Repository;

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

/// Options for compiling the unreleased region.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompileOptions {
    /// Optional section identifier to compile by itself.
    pub section: Option<String>,
}

/// Compiled unreleased changelog region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRegion {
    /// Markdown text produced by the compiler.
    pub text: String,
}

/// Options for planning a changelog synchronization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncOptions;

/// Planned synchronization action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPlan {
    /// The changelog already matches the compiled fragment output.
    Clean,
    /// The changelog can be updated without confirmation.
    Apply {
        /// Replacement Markdown for the unreleased region.
        replacement: String,
    },
    /// Applying the sync may discard hand edits and needs confirmation.
    NeedsConfirmation {
        /// Diff showing the content that would be replaced.
        diff: String,
    },
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
pub fn compile_unreleased(_repo: &Repository, _options: CompileOptions) -> Result<CompiledRegion> {
    Err(Error::UnsupportedCommand { command: "preview" })
}

/// Plans a synchronization between fragments and the materialized changelog.
pub fn plan_sync(_repo: &Repository, _options: SyncOptions) -> Result<SyncPlan> {
    Err(Error::UnsupportedCommand { command: "sync" })
}

/// Applies a previously planned synchronization.
pub fn apply_sync(_repo: &Repository, _plan: SyncPlan) -> Result<SyncResult> {
    Err(Error::UnsupportedCommand { command: "sync" })
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
pub fn check(_repo: &Repository, _options: CheckOptions) -> Result<CheckReport> {
    Err(Error::UnsupportedCommand { command: "check" })
}
