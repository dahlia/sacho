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

    /// The next-version file did not contain a single version line.
    #[snafu(display("{} must contain at most one non-empty line", path.display()))]
    InvalidNextVersion {
        /// Next-version file path.
        path: PathBuf,
    },

    /// The unreleased changelog region could not be found.
    #[snafu(display("changelog region not found in {}", path.display()))]
    RegionNotFound {
        /// Changelog path that did not contain the region.
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
