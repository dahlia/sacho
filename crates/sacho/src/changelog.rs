use std::path::PathBuf;

/// A released changelog section keyed by version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedSection {
    /// Released version string.
    pub version: String,

    /// Markdown body for the released section.
    pub body: String,
}

/// The unreleased changelog region in a materialized changelog file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreleasedRegion {
    /// Changelog file containing the region.
    pub path: PathBuf,

    /// Markdown body of the unreleased region.
    pub body: String,
}
