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
    format_markdown_with_word_wrap(source, true)
}

pub(crate) fn format_markdown_with_word_wrap(source: &str, word_wrap: bool) -> Result<String> {
    hongdown::format(source, &hongdown_options(word_wrap)).map_err(|source| Error::Format {
        source: Box::new(source),
    })
}

fn hongdown_options(word_wrap: bool) -> Options {
    Options {
        line_width: word_wrap.then(|| LineWidth::new(80).expect("80 is a valid line width")),
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

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn no_word_wrap_is_independent_of_soft_break_position(
            words in prop::collection::vec("[a-z]{1,12}", 2..24),
            split_seed in any::<usize>(),
        ) {
            let split = split_seed % (words.len() - 1) + 1;
            let source = format!(
                " -  {}\n    {}\n",
                words[..split].join(" "),
                words[split..].join(" "),
            );
            let expected = format!(" -  {}\n", words.join(" "));

            let formatted =
                format_markdown_with_word_wrap(&source, false).expect("format without wrapping");

            prop_assert_eq!(&formatted, &expected);
            prop_assert_eq!(
                format_markdown_with_word_wrap(&formatted, false)
                    .expect("reformat without wrapping"),
                formatted,
            );
        }
    }
}
