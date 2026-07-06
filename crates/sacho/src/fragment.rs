use std::path::PathBuf;

/// Parsed changelog fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Path to the fragment file.
    pub path: PathBuf,

    /// Sort priority read from frontmatter.
    pub priority: i32,

    /// Items contributed by the fragment.
    pub items: Vec<FragmentItem>,
}

/// A single changelog item inside a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentItem {
    /// Original Markdown for the item.
    pub markdown: String,

    /// Plain-text key used for deterministic sorting.
    pub sort_text: String,
}
