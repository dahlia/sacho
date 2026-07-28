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

pub(crate) fn escape_angle_bracket_destination(destination: &str) -> String {
    let mut escaped = String::with_capacity(destination.len());
    for character in destination.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '\t' => escaped.push_str("&#9;"),
            '\n' => escaped.push_str("&#10;"),
            '\r' => escaped.push_str("&#13;"),
            '\\' | '<' | '>' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
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
    use comrak::nodes::NodeValue;
    use comrak::{Arena, Options as ComrakOptions, parse_document};
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn escapes_destination_control_characters_as_numeric_references() {
        for (character, entity) in [('\t', "&#9;"), ('\n', "&#10;"), ('\r', "&#13;")] {
            let destination = format!("https://example.com/a{character}b");
            let escaped = escape_angle_bracket_destination(&destination);
            let source = format!("[target]: <{escaped}>\n\n[target]\n");
            let arena = Arena::new();
            let root = parse_document(&arena, &source, &ComrakOptions::default());
            let parsed = root.descendants().find_map(|node| {
                if let NodeValue::Link(link) = &node.data().value {
                    Some(link.url.clone())
                } else {
                    None
                }
            });

            assert!(escaped.contains(entity), "{escaped:?}");
            assert_eq!(parsed.as_deref(), Some(destination.as_str()));
        }
    }

    proptest! {
        #[test]
        fn escaped_angle_bracket_destinations_round_trip(
            suffix in prop::collection::vec(0x21_u8..=0x7e, 0..40),
        ) {
            let suffix = String::from_utf8(suffix).expect("printable ASCII is UTF-8");
            let destination = format!("https://example.com/{suffix}");
            let source = format!(
                "[target]: <{}>\n\n[target]\n",
                escape_angle_bracket_destination(&destination),
            );
            let arena = Arena::new();
            let root = parse_document(&arena, &source, &ComrakOptions::default());
            let parsed = root.descendants().find_map(|node| {
                if let NodeValue::Link(link) = &node.data().value {
                    Some(link.url.clone())
                } else {
                    None
                }
            });

            prop_assert_eq!(parsed.as_deref(), Some(destination.as_str()));
        }

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
