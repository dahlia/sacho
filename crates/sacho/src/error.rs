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
