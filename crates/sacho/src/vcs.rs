use std::path::PathBuf;

/// Commit information returned by a VCS integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Commit identifier.
    pub id: String,

    /// Paths changed by the commit.
    pub changed_paths: Vec<PathBuf>,

    /// Raw commit message.
    pub message: String,
}
