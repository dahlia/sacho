use std::path::PathBuf;

use crate::changelog::ChangelogError;
use snafu::Snafu;

/// Result type returned by Sacho library APIs.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error returned by Sacho library operations.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    /// A file could not be read.
    #[snafu(display("failed to read {}: {source}", path.display()))]
    ReadFile {
        /// Path that could not be read.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A file could not be written.
    #[snafu(display("failed to write {}: {source}", path.display()))]
    WriteFile {
        /// Path that could not be written.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A file could not be removed.
    #[snafu(display("failed to remove file {}: {source}", path.display()))]
    RemoveFile {
        /// File path that could not be removed.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A directory could not be removed.
    #[snafu(display("failed to remove directory {}: {source}", path.display()))]
    RemoveDirectory {
        /// Directory path that could not be removed.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A directory could not be created.
    #[snafu(display("failed to create directory {}: {source}", path.display()))]
    CreateDirectory {
        /// Directory path that could not be created.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A file rename failed.
    #[snafu(display("failed to rename {} to {}: {source}", from.display(), to.display()))]
    RenameFile {
        /// Source path for the rename.
        from: PathBuf,

        /// Destination path for the rename.
        to: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Configuration was invalid.
    #[snafu(display("invalid configuration in {}: {source}", path.display()))]
    Config {
        /// Path to the invalid configuration file.
        path: PathBuf,

        /// Configuration-specific error.
        source: ConfigError,
    },

    /// A fragment was invalid.
    #[snafu(display("invalid fragment {}: {source}", path.display()))]
    Fragment {
        /// Path to the invalid fragment.
        path: PathBuf,

        /// Fragment-specific error.
        source: FragmentError,
    },

    /// Markdown formatting failed.
    #[snafu(display("failed to format compiled Markdown: {source}"))]
    Format {
        /// Underlying Hongdown formatting error.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A requested section does not exist.
    #[snafu(display("unknown section {section:?}"))]
    UnknownSection {
        /// Section identifier requested by the caller.
        section: String,
    },

    /// A section was required but omitted by the caller.
    #[snafu(display("section is required because this repository configures sections"))]
    MissingSection,

    /// A section was supplied for a repository that does not use sections.
    #[snafu(display("section must not be supplied because this repository has no sections"))]
    UnexpectedSection,

    /// A fragment file name was invalid.
    #[snafu(display("invalid fragment name {name:?}: {reason}"))]
    InvalidFragmentName {
        /// Fragment file name supplied by the caller.
        name: String,

        /// Reason the name was rejected.
        reason: &'static str,
    },

    /// A fragment file already exists.
    #[snafu(display("fragment already exists at {}", path.display()))]
    FragmentAlreadyExists {
        /// Existing fragment path.
        path: PathBuf,
    },

    /// The next version argument was empty after trimming.
    #[snafu(display("next version must not be empty"))]
    EmptyNextVersion,

    /// The next-version file did not contain a single version line.
    #[snafu(display("{} must contain at most one non-empty line", path.display()))]
    InvalidNextVersion {
        /// Next-version file path.
        path: PathBuf,
    },

    /// A release version argument disagreed with the next-version file.
    #[snafu(display(
        "release version {version:?} does not match next-version file value {next_version:?}"
    ))]
    ReleaseVersionMismatch {
        /// Explicit release version supplied by the caller.
        version: String,

        /// Version read from the next-version file.
        next_version: String,
    },

    /// No release version was supplied or configured.
    #[snafu(display("release version is required when the next-version file is absent or empty"))]
    MissingReleaseVersion,

    /// No compiled changelog entries are available to release.
    #[snafu(display("no changelog entries to release; add a fragment before releasing"))]
    EmptyRelease,

    /// A release date was not a valid `YYYY-MM-DD` calendar date.
    #[snafu(display("invalid release date {date:?}; expected YYYY-MM-DD"))]
    InvalidReleaseDate {
        /// Invalid date string supplied by the caller.
        date: String,
    },

    /// The requested released version was absent from the changelog.
    #[snafu(display("released version {version:?} not found in changelog"))]
    ReleasedVersionNotFound {
        /// Released version requested by the caller.
        version: String,
    },

    /// A sectioned changelog had released entries before any section heading.
    #[snafu(display("released entries must appear under a configured section heading"))]
    ReleasedEntryWithoutSection,

    /// The unreleased changelog region could not be found.
    #[snafu(display("changelog region not found in {}", path.display()))]
    RegionNotFound {
        /// Changelog path that did not contain the region.
        path: PathBuf,
    },

    /// Synchronizing could overwrite hand edits in the materialized changelog.
    #[snafu(display(
        "materialized changelog may contain hand edits in {}; run `sacho sync --force` to discard them",
        path.display()
    ))]
    SyncNeedsConfirmation {
        /// Changelog path that needs explicit synchronization.
        path: PathBuf,
    },

    /// A synchronization plan no longer matches the changelog it was built from.
    #[snafu(display(
        "refusing to apply stale synchronization plan because {} changed after planning; run `sacho sync` again",
        path.display()
    ))]
    StaleSyncPlan {
        /// Changelog path that changed after the plan was prepared.
        path: PathBuf,
    },

    /// A release plan no longer matches a file it was built from.
    #[snafu(display(
        "refusing to apply stale release plan because {} changed after planning; plan the release again",
        path.display()
    ))]
    StaleReleasePlan {
        /// Path whose current state differs from the planned state.
        path: PathBuf,
    },

    /// A release path is not a regular file.
    #[snafu(display("release path {} is not a regular file", path.display()))]
    ReleasePathConflict {
        /// Path with an unsupported file type, including symbolic links.
        path: PathBuf,
    },

    /// Two release transaction entries resolve to overlapping filesystem paths.
    #[snafu(display(
        "release paths {} and {} overlap in the filesystem",
        first.display(),
        second.display()
    ))]
    ReleasePathOverlap {
        /// First conflicting repository-relative path.
        first: PathBuf,

        /// Second conflicting repository-relative path.
        second: PathBuf,
    },

    /// A configured mutation path overlaps the repository mutation lock.
    #[snafu(display(
        "configured mutation path {} overlaps the repository lock at {}",
        path.display(),
        lock_path.display()
    ))]
    MutationLockPathOverlap {
        /// Configured repository path that would replace or contain the lock.
        path: PathBuf,

        /// Resolved path of the repository mutation lock.
        lock_path: PathBuf,
    },

    /// Another mutation currently holds the repository mutation lock.
    #[snafu(display("another Sacho mutation is already running in this repository"))]
    ReleaseLocked,

    /// The platform cannot provide the no-replace directory move required by
    /// release transactions.
    #[snafu(display(
        "release transactions are unsupported because this platform has no atomic no-replace directory move"
    ))]
    ReleaseTransactionUnsupported,

    /// Rollback found that another writer changed an applied release path.
    #[snafu(display(
        "refusing to roll back {} because it changed after the release wrote it",
        path.display()
    ))]
    ReleaseRollbackConflict {
        /// Path whose concurrent state was preserved.
        path: PathBuf,
    },

    /// Applying a release failed, possibly followed by rollback failures.
    #[snafu(display(
        "release apply failed: {cause}{rollback_summary}",
        rollback_summary = if rollback_failures.is_empty() {
            String::from("; all applied changes were rolled back")
        } else {
            format!("; rollback also failed: {}", rollback_failures.join("; "))
        }
    ))]
    ReleaseApply {
        /// Original apply failure.
        cause: String,

        /// Failures encountered while restoring already-applied changes.
        rollback_failures: Vec<String>,
    },

    /// A release committed, but removing retained transaction claims failed.
    #[snafu(display(
        "release committed, but transaction cleanup failed: {}",
        failures.join("; ")
    ))]
    ReleaseCleanup {
        /// Failures encountered while deleting committed claim files.
        failures: Vec<String>,
    },

    /// A changelog file could not be parsed or updated.
    #[snafu(display("invalid changelog region in {}: {source}", path.display()))]
    Changelog {
        /// Changelog path containing the invalid region.
        path: PathBuf,

        /// Region-specific error.
        source: ChangelogError,
    },

    /// No configuration file was found during repository discovery.
    #[snafu(display("no sacho.toml found from {}", start.display()))]
    ConfigNotFound {
        /// Path where discovery started.
        start: PathBuf,
    },

    /// The command API exists but has not been implemented yet.
    #[snafu(display("{command} command is not implemented yet"))]
    UnsupportedCommand {
        /// Name of the unsupported command.
        command: &'static str,
    },

    /// Mutually exclusive command-line options were used together.
    #[snafu(display("{message}"))]
    Usage {
        /// Usage error message.
        message: String,
    },

    /// A requested interactive operation cannot run without a terminal.
    #[snafu(display("{message}"))]
    TerminalRequired {
        /// Terminal requirement message.
        message: String,
    },

    /// Existing repository integration needs manual attention.
    #[snafu(display("{message}"))]
    InitConflict {
        /// Conflict message.
        message: String,
    },

    /// A pre-commit hook exists and cannot be changed non-interactively.
    #[snafu(display("{} already exists without a Sacho marker; add the marked block manually or rerun interactively", path.display()))]
    HookNeedsManualInstall {
        /// Existing hook path.
        path: PathBuf,
    },

    /// A configured glob pattern could not be compiled.
    #[snafu(display("invalid glob pattern {pattern:?}: {source}"))]
    InvalidGlob {
        /// Glob pattern from configuration.
        pattern: String,

        /// Underlying globset error.
        source: globset::Error,
    },

    /// A VCS integration command failed.
    #[snafu(display("VCS command `{command}` failed with status {status}: {stderr}"))]
    VcsCommandFailed {
        /// Command line that failed.
        command: String,

        /// Process exit status.
        status: std::process::ExitStatus,

        /// Standard error emitted by the command.
        stderr: String,
    },

    /// A VCS integration command could not be started.
    #[snafu(display("failed to run VCS command `{command}`: {source}"))]
    VcsCommandIo {
        /// Command line that failed to start.
        command: String,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A configured VCS query command could not be started.
    #[snafu(display("failed to run VCS {query} command {command:?}: {source}"))]
    VcsQueryCommandIo {
        /// Query being evaluated.
        query: crate::config::VcsQuery,

        /// Expanded program and argument vector.
        command: Vec<String>,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A configured VCS query command exited unsuccessfully.
    #[snafu(display(
        "VCS {query} command {command:?} failed with status {status}: {stderr}",
        stderr = String::from_utf8_lossy(stderr)
    ))]
    VcsQueryCommandFailed {
        /// Query being evaluated.
        query: crate::config::VcsQuery,

        /// Expanded program and argument vector.
        command: Vec<String>,

        /// Process exit status.
        status: std::process::ExitStatus,

        /// Exact standard error bytes emitted by the command.
        stderr: Vec<u8>,
    },

    /// A configured VCS query emitted text that was not UTF-8.
    #[snafu(display("VCS {query} command {command:?} emitted invalid UTF-8 in {stream}"))]
    VcsQueryInvalidUtf8 {
        /// Query being evaluated.
        query: crate::config::VcsQuery,

        /// Expanded program and argument vector.
        command: Vec<String>,

        /// Stream containing invalid text.
        stream: &'static str,

        /// Invalid stream bytes.
        bytes: Vec<u8>,
    },

    /// A configured VCS query emitted records outside its output contract.
    #[snafu(display("VCS {query} command {command:?} emitted malformed output: {reason}"))]
    VcsQueryMalformedOutput {
        /// Query being evaluated.
        query: crate::config::VcsQuery,

        /// Expanded program and argument vector.
        command: Vec<String>,

        /// Human-readable contract violation.
        reason: String,

        /// Exact standard output bytes emitted by the command.
        stdout: Vec<u8>,
    },

    /// A fragment file could not be merged while reconstructing merge inputs.
    #[snafu(display("failed to merge fragment {}: {stderr}", path.display()))]
    MergeFragment {
        /// Fragment path that could not be merged.
        path: std::path::PathBuf,

        /// Process exit status from the merge tool.
        status: std::process::ExitStatus,

        /// Standard error emitted by the merge tool.
        stderr: String,
    },
}

/// Configuration-specific error.
#[derive(Debug, Snafu)]
pub enum ConfigError {
    /// The TOML document could not be parsed.
    #[snafu(display("TOML parse error: {source}"))]
    Parse {
        /// Underlying TOML parser error.
        source: toml::de::Error,
    },

    /// A configured section has an empty fragment directory.
    #[snafu(display("section directory must not be empty for section {section:?}"))]
    EmptySectionDirectory {
        /// Section whose directory is empty.
        section: String,
    },

    /// A reference URL template does not contain the numeric placeholder.
    #[snafu(display("link template for sigil {sigil:?} must contain {{n}}"))]
    LinkTemplateMissingNumber {
        /// Sigil whose template is invalid.
        sigil: String,
    },

    /// Query commands cannot be combined with the disabled VCS preset.
    #[snafu(display("vcs.commands cannot be configured when vcs.preset = \"none\""))]
    VcsCommandsWithNonePreset,

    /// A VCS query command contains no program.
    #[snafu(display("vcs.commands.{query} must contain a program"))]
    EmptyVcsCommand {
        /// Query whose command is empty.
        query: crate::config::VcsQuery,
    },

    /// A VCS query command has an empty program name.
    #[snafu(display("vcs.commands.{query} program must not be empty"))]
    EmptyVcsProgram {
        /// Query whose program name is empty.
        query: crate::config::VcsQuery,
    },

    /// A placeholder appeared in a command's program name.
    #[snafu(display("vcs.commands.{query} placeholders are not allowed in the program name"))]
    VcsPlaceholderInProgram {
        /// Query whose program contains a placeholder.
        query: crate::config::VcsQuery,
    },

    /// A command argument contains an unclosed placeholder.
    #[snafu(display("vcs.commands.{query} contains an unclosed placeholder"))]
    MalformedVcsPlaceholder {
        /// Query whose argument contains the malformed placeholder.
        query: crate::config::VcsQuery,
    },

    /// A command uses a placeholder unavailable to its query.
    #[snafu(display("vcs.commands.{query} does not support placeholder ${{{placeholder}}}"))]
    UnknownVcsPlaceholder {
        /// Query whose command contains the placeholder.
        query: crate::config::VcsQuery,
        /// Unsupported placeholder name.
        placeholder: String,
    },

    /// A command omits a placeholder required to evaluate its query.
    #[snafu(display("vcs.commands.{query} must contain placeholder ${{{placeholder}}}"))]
    MissingVcsPlaceholder {
        /// Query whose command omits the placeholder.
        query: crate::config::VcsQuery,
        /// Required placeholder name.
        placeholder: String,
    },
}

/// Fragment-specific error.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum FragmentError {
    /// YAML frontmatter was opened but never closed.
    #[snafu(display("YAML frontmatter delimiter was not closed"))]
    UnclosedFrontmatter,

    /// YAML frontmatter could not be parsed.
    #[snafu(display("YAML frontmatter parse error: {source}"))]
    Frontmatter {
        /// Underlying YAML parser error.
        source: serde_yaml_ng::Error,
    },

    /// The fragment Markdown did not match Sacho's structural constraints.
    #[snafu(display(
        "fragment body must contain exactly one top-level unordered list; found {kind} at {line}:{column}"
    ))]
    InvalidShape {
        /// Top-level node kind that violated the constraint.
        kind: &'static str,

        /// Source line where the violating node starts.
        line: usize,

        /// Source column where the violating node starts.
        column: usize,
    },

    /// A fragment referenced an unknown link sigil.
    #[snafu(display("reference label {label:?} does not match any configured link template"))]
    UnknownReference {
        /// Reference label that could not be resolved.
        label: String,
    },
}
