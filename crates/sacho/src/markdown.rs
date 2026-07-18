use hongdown::{
    DashSetting, IndentWidth, LeadingSpaces, LineWidth, Options, TrailingSpaces, UnorderedMarker,
};

use crate::error::{Error, Result};

/// Markdown text formatted in Sacho's normal form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedMarkdown {
    /// Formatted Markdown text.
    pub text: String,
}

pub(crate) fn format_markdown(source: &str) -> Result<String> {
    hongdown::format(source, &hongdown_options()).map_err(|source| Error::Format {
        source: Box::new(source),
    })
}

fn hongdown_options() -> Options {
    Options {
        line_width: Some(LineWidth::new(80).expect("80 is a valid line width")),
        unordered_marker: UnorderedMarker::Hyphen,
        leading_spaces: LeadingSpaces::new(1).expect("1 is valid leading spaces"),
        trailing_spaces: TrailingSpaces::new(2).expect("2 is valid trailing spaces"),
        indent_width: IndentWidth::new(4).expect("4 is a valid indent width"),
        curly_double_quotes: false,
        curly_single_quotes: false,
        ellipsis: false,
        em_dash: DashSetting::Disabled,
        ..Options::default()
    }
}
