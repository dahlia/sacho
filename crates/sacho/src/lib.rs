//! Library entry point for Sacho.

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Changelog parsing and region replacement types.
pub mod changelog;
/// Check command report and violation types.
pub mod check;
/// Command-level library API.
pub mod commands;
/// Unreleased-region compilation types.
pub mod compile;
/// Configuration loading and validation.
pub mod config;
/// Diagnostic and exit-code helpers shared by frontends.
pub mod diagnostic;
/// Error types returned by the library.
pub mod error;
/// Fragment discovery, parsing, and validation types.
pub mod fragment;
/// Reference-link redirect resolution types.
pub mod link_resolution;
/// Markdown formatting adapter types.
pub mod markdown;
/// Changelog merge driver types.
pub mod merge;
/// Released changelog parsing types.
pub mod released;
/// Repository discovery and path helpers.
pub mod repo;
mod repository_url;
/// Section pattern parsing and matching types.
pub mod section_pattern;
/// Version-control integration types.
pub mod vcs;

pub use crate::config::{
    ChangelogConfig, CheckConfig, Config, FragmentsConfig, LinkResolutionConfig, ReferenceSigil,
    RegionDetection, SectionConfig, UrlTemplate, VcsCommand, VcsCommandOverrides, VcsConfig,
    VcsPreset, VcsQuery,
};
pub use crate::error::{ConfigError, Error, FragmentError, MutationCommand, Result};
pub use crate::repo::Repository;
pub use crate::section_pattern::{
    SectionPattern, SectionPatternConfig, SectionPatternConfigError, SectionPatternError,
    SectionPatternSegment,
};
