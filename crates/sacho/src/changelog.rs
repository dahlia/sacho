use std::path::PathBuf;

use crate::config::RegionDetection;

pub(crate) const BEGIN_MARKER: &str = "<!-- sacho:unreleased:begin -->";
pub(crate) const END_MARKER: &str = "<!-- sacho:unreleased:end -->";

/// A released changelog section keyed by version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedSection {
    /// Released version string.
    pub version: String,

    /// Markdown for the section, including its version heading.
    pub markdown: String,
}

/// The unreleased changelog region in a materialized changelog file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreleasedRegion {
    /// Changelog file containing the region.
    pub path: PathBuf,

    /// Markdown body of the unreleased region.
    pub body: String,
}

/// Byte span of an unreleased region inside a changelog source string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreleasedRegionSpan {
    /// Byte offset where the replaceable region starts.
    pub start: usize,

    /// Byte offset where the replaceable region ends.
    pub end: usize,
}

/// Byte span and text of a version-like changelog heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionHeadingSpan {
    /// Byte offset where the heading starts.
    pub start: usize,

    /// Rendered heading text without Markdown markers.
    pub text: String,
}

/// Result of replacing an unreleased region in a changelog source string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionReplacement {
    /// Region text that was replaced.
    pub old_region: String,

    /// Full changelog contents after replacement.
    pub new_contents: String,
}

/// Error returned while locating or replacing an unreleased changelog region.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangelogError {
    /// No unreleased region matched the configured detection strategy.
    #[error("unreleased changelog region not found")]
    RegionNotFound,

    /// A marker that must appear exactly once appeared more than once.
    #[error("duplicate unreleased changelog marker {marker:?}")]
    DuplicateMarker {
        /// Marker text that appeared more than once.
        marker: &'static str,
    },

    /// The end marker appeared before the begin marker.
    #[error("unreleased changelog end marker appears before begin marker")]
    ReversedMarkers,

    /// A code fence inside the unreleased region was not closed.
    #[error("unreleased changelog region contains an unterminated code fence")]
    UnterminatedCodeFence,
}

/// Finds the unreleased region span in a changelog source string.
pub fn find_unreleased_region(
    source: &str,
    detection: RegionDetection,
    unreleased_heading: &str,
) -> std::result::Result<UnreleasedRegionSpan, ChangelogError> {
    match detection {
        RegionDetection::Heading => find_heading_region(source, unreleased_heading),
        RegionDetection::Marker => find_marker_region(source),
    }
}

/// Replaces the unreleased region in a changelog source string.
pub fn replace_unreleased_region(
    source: &str,
    compiled: &str,
    detection: RegionDetection,
    unreleased_heading: &str,
) -> std::result::Result<RegionReplacement, ChangelogError> {
    let span = find_unreleased_region(source, detection, unreleased_heading)?;
    let mut replacement = match detection {
        RegionDetection::Heading => normalize_compiled_region(compiled),
        RegionDetection::Marker => marker_region_contents(compiled),
    };
    if detection == RegionDetection::Heading && span.end < source.len() {
        set_hongdown_separator_before(&mut replacement, &source[span.end..]);
    }
    let mut new_contents =
        String::with_capacity(source.len() - (span.end - span.start) + replacement.len());
    new_contents.push_str(&source[..span.start]);
    new_contents.push_str(&replacement);
    new_contents.push_str(&source[span.end..]);

    Ok(RegionReplacement {
        old_region: source[span.start..span.end].to_owned(),
        new_contents,
    })
}

/// Finds all version-like headings outside code blocks.
pub(crate) fn version_heading_spans(source: &str) -> Vec<VersionHeadingSpan> {
    let lines = source_lines(source);
    heading_candidates(&lines)
        .candidates
        .into_iter()
        .filter_map(|candidate| {
            let line = lines.iter().find(|line| line.start == candidate.start)?;
            let text = if lines
                .get(candidate.after_line.saturating_sub(1))
                .is_some_and(|underline| is_setext_underline(underline.text))
            {
                line.text.trim().to_owned()
            } else {
                atx_heading_text(line.text)?.to_owned()
            };
            Some(VersionHeadingSpan {
                start: candidate.start,
                text,
            })
        })
        .collect()
}

fn normalize_compiled_region(compiled: &str) -> String {
    let mut normalized = compiled.trim_end_matches(['\n', '\r']).to_owned();
    normalized.push('\n');
    normalized
}

pub(crate) fn marker_region_contents(compiled: &str) -> String {
    let compiled = compiled.trim_end_matches(['\n', '\r']);
    if compiled.is_empty() {
        String::from("\n")
    } else {
        format!("\n\n{compiled}\n\n")
    }
}

pub(crate) fn set_hongdown_separator_before(output: &mut String, following: &str) {
    let following = following.trim_start_matches(['\n', '\r']);
    let count = if starts_with_setext_heading(following) {
        3
    } else {
        2
    };
    set_trailing_newline_count(output, count);
}

fn starts_with_setext_heading(source: &str) -> bool {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, source, &comrak::Options::default());
    let Some(first_block) = root.first_child() else {
        return false;
    };
    let block = first_block.data.borrow();
    if !matches!(&block.value, comrak::nodes::NodeValue::Heading(_)) {
        return false;
    }

    source_lines(source)
        .get(block.sourcepos.end.line.saturating_sub(1))
        .is_some_and(|line| !is_indented_code_line(line.text) && is_setext_underline(line.text))
}

pub(crate) fn set_trailing_newline_count(output: &mut String, count: usize) {
    output.truncate(output.trim_end_matches(['\n', '\r']).len());
    output.extend(std::iter::repeat_n('\n', count));
}

fn find_heading_region(
    source: &str,
    unreleased_heading: &str,
) -> std::result::Result<UnreleasedRegionSpan, ChangelogError> {
    let lines = source_lines(source);
    let scan = heading_candidates(&lines);
    for (index, candidate) in scan.candidates.iter().enumerate() {
        let Some(date_line) = next_non_empty_line(&lines, candidate.after_line) else {
            continue;
        };
        if date_line.text != unreleased_heading {
            continue;
        }
        let end = scan
            .candidates
            .get(index + 1)
            .map(|next| next.start)
            .unwrap_or(source.len());
        if scan
            .unterminated_fence_start
            .is_some_and(|start| (candidate.start..end).contains(&start))
        {
            return Err(ChangelogError::UnterminatedCodeFence);
        }
        return Ok(UnreleasedRegionSpan {
            start: candidate.start,
            end,
        });
    }

    Err(ChangelogError::RegionNotFound)
}

fn find_marker_region(source: &str) -> std::result::Result<UnreleasedRegionSpan, ChangelogError> {
    let mut begin = None;
    let mut end = None;
    for line in source_lines(source) {
        match line.text {
            BEGIN_MARKER => {
                if begin.is_some() {
                    return Err(ChangelogError::DuplicateMarker {
                        marker: BEGIN_MARKER,
                    });
                }
                begin = Some(line);
            }
            END_MARKER => {
                if begin.is_none() {
                    return Err(ChangelogError::ReversedMarkers);
                }
                if end.is_some() {
                    return Err(ChangelogError::DuplicateMarker { marker: END_MARKER });
                }
                end = Some(line);
            }
            _ => {}
        }
    }

    let (Some(begin), Some(end)) = (begin, end) else {
        return Err(ChangelogError::RegionNotFound);
    };

    Ok(UnreleasedRegionSpan {
        start: begin.end,
        end: end.start,
    })
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct HeadingCandidate {
    start: usize,
    after_line: usize,
}

#[derive(Debug, Clone)]
struct HeadingScan {
    candidates: Vec<HeadingCandidate>,
    unterminated_fence_start: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeFence {
    marker: u8,
    length: usize,
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for raw in source.split_inclusive('\n') {
        let end = start + raw.len();
        let mut text_end = end;
        if raw.ends_with('\n') {
            text_end -= 1;
        }
        if source[start..text_end].ends_with('\r') {
            text_end -= 1;
        }
        lines.push(SourceLine {
            start,
            end,
            text: &source[start..text_end],
        });
        start = end;
    }
    lines
}

fn heading_candidates(lines: &[SourceLine<'_>]) -> HeadingScan {
    let mut candidates = Vec::new();
    let mut code_fence = None;
    let mut code_fence_start = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(fence) = code_fence {
            if is_closing_code_fence(line.text, fence) {
                code_fence = None;
                code_fence_start = None;
            }
            continue;
        }

        if let Some(fence) = opening_code_fence(line.text) {
            code_fence = Some(fence);
            code_fence_start = Some(line.start);
            continue;
        }

        if !is_indented_code_line(line.text)
            && atx_heading_text(line.text).is_some_and(is_version_heading_text)
        {
            candidates.push(HeadingCandidate {
                start: line.start,
                after_line: index + 1,
            });
        }
        if lines
            .get(index + 1)
            .is_some_and(|next| !is_indented_code_line(next.text) && is_setext_underline(next.text))
            && !is_indented_code_line(line.text)
            && is_version_heading_text(line.text.trim())
        {
            candidates.push(HeadingCandidate {
                start: line.start,
                after_line: index + 2,
            });
        }
    }
    candidates.sort_by_key(|candidate| candidate.start);
    HeadingScan {
        candidates,
        unterminated_fence_start: code_fence_start,
    }
}

fn next_non_empty_line<'a>(lines: &'a [SourceLine<'a>], start: usize) -> Option<SourceLine<'a>> {
    lines
        .iter()
        .skip(start)
        .find(|line| !line.text.trim().is_empty())
        .copied()
}

fn is_version_heading_text(text: &str) -> bool {
    text == "Unreleased" || text.starts_with("Version ")
}

fn is_indented_code_line(text: &str) -> bool {
    indentation_columns(text) >= 4
}

fn indentation_columns(text: &str) -> usize {
    let mut columns = 0;
    for character in text.chars() {
        match character {
            ' ' => columns += 1,
            '\t' => columns += 4 - (columns % 4),
            _ => break,
        }
        if columns >= 4 {
            break;
        }
    }
    columns
}

fn is_setext_underline(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with('-') {
        text.trim_matches('-').is_empty()
    } else if text.starts_with('=') {
        text.trim_matches('=').is_empty()
    } else {
        false
    }
}

fn opening_code_fence(text: &str) -> Option<CodeFence> {
    if is_indented_code_line(text) {
        return None;
    }

    let text = text.trim_start_matches(' ');
    let marker = match *text.as_bytes().first()? {
        b'`' => b'`',
        b'~' => b'~',
        _ => return None,
    };
    let length = text.bytes().take_while(|byte| *byte == marker).count();
    if length < 3 {
        return None;
    }

    let info = &text[length..];
    if marker == b'`' && info.contains('`') {
        return None;
    }

    Some(CodeFence { marker, length })
}

fn is_closing_code_fence(text: &str, fence: CodeFence) -> bool {
    if is_indented_code_line(text) {
        return false;
    }

    let text = text.trim_start_matches(' ');
    let length = text
        .bytes()
        .take_while(|byte| *byte == fence.marker)
        .count();
    length >= fence.length && text[length..].trim().is_empty()
}

fn atx_heading_text(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let marker_count = line.bytes().take_while(|byte| *byte == b'#').count();
    if marker_count == 0 || marker_count > 6 {
        return None;
    }
    let after_markers = &line[marker_count..];
    if !after_markers.is_empty()
        && !after_markers
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }

    let text = after_markers.trim();
    if text.is_empty() {
        return Some(text);
    }

    Some(strip_atx_closing_sequence(text))
}

fn strip_atx_closing_sequence(text: &str) -> &str {
    let text = text.trim_end();
    if text.strip_suffix('#').is_none() {
        return text;
    }

    let without_hashes = text.trim_end_matches('#');
    if without_hashes
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
    {
        without_hashes.trim_end()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::config::RegionDetection;
    use crate::markdown::format_markdown;

    const COMPILED: &str = "Unreleased\n----------\n\nTo be released.\n\n -  Fixed sync.\n";

    fn safe_lines() -> impl Strategy<Value = String> {
        prop::collection::vec("[a-z0-9 .,!]{0,24}", 0..8).prop_map(|lines| {
            if lines.is_empty() {
                String::new()
            } else {
                format!("{}\n", lines.join("\n"))
            }
        })
    }

    #[test]
    fn heading_mode_finds_version_heading_with_unreleased_date_line() {
        let source = "\
Project changes
===============

Version 1.2.0
-------------

To be released.

 -  Old entry.

Version 1.1.0
-------------

Released on July 1, 2026.
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert!(replacement.new_contents.contains(" -  Fixed sync.\n"));
        assert!(
            replacement
                .new_contents
                .contains("Version 1.1.0\n-------------")
        );
        assert!(!replacement.new_contents.contains(" -  Old entry.\n"));
    }

    #[test]
    fn heading_mode_finds_unreleased_heading_with_unreleased_date_line() {
        let source = "\
Unreleased
----------

To be released.

 -  Old entry.
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(replacement.new_contents, COMPILED);
    }

    #[test]
    fn heading_replacement_does_not_add_a_separator_at_end_of_file() {
        let source = "Unreleased\n----------\n\nTo be released.\n\n";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(replacement.new_contents, COMPILED);
    }

    #[test]
    fn heading_mode_ignores_released_date_lines() {
        let source = "\
Version 1.2.0
-------------

Released on July 7, 2026.
";

        let error = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect_err("released section is not unreleased");

        assert!(matches!(error, ChangelogError::RegionNotFound));
    }

    #[test]
    fn heading_mode_finds_atx_version_heading() {
        let source = "\
## Version 1.2.0

To be released.

 -  Old entry.

## Version 1.1.0

Released on July 1, 2026.
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(
            replacement.new_contents,
            format!("{COMPILED}\n## Version 1.1.0\n\nReleased on July 1, 2026.\n")
        );
    }

    #[test]
    fn heading_mode_allows_headings_indented_less_than_four_spaces() {
        let atx = "\
   ## Version 1.2.0

To be released.
";
        let setext = "\
   Version 1.2.0
   -------------

To be released.
";

        for source in [atx, setext] {
            let replacement = replace_unreleased_region(
                source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("replace");

            assert_eq!(replacement.new_contents, COMPILED);
        }
    }

    #[test]
    fn heading_mode_ignores_indented_code_blocks() {
        for source in [
            "    ## Version 1.2.0\n\nTo be released.\n",
            "    Version 1.2.0\n    -------------\n\nTo be released.\n",
            "\t## Version 1.2.0\n\nTo be released.\n",
        ] {
            let error = replace_unreleased_region(
                source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect_err("indented code is not a version section");

            assert!(matches!(error, ChangelogError::RegionNotFound));
        }
    }

    #[test]
    fn heading_mode_ignores_fenced_code_blocks_before_region() {
        let source = "\
```markdown
## Version 9.9.9

To be released.
```

Unreleased
----------

To be released.

 -  Old entry.
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert!(replacement.new_contents.starts_with(
            "\
```markdown
## Version 9.9.9

To be released.
```

"
        ));
        assert!(replacement.new_contents.ends_with(COMPILED));
    }

    #[test]
    fn heading_mode_ignores_fenced_code_blocks_when_finding_region_end() {
        for fence in ["```markdown", "~~~markdown"] {
            let marker = &fence[..3];
            let source = format!(
                "\
Unreleased
----------

To be released.

{fence}
## Version 9.9.9

Released on July 7, 2026.
{marker}

 -  Old entry.

Version 1.0.0
-------------

Released on July 1, 2026.
"
            );

            let replacement = replace_unreleased_region(
                &source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("replace");

            assert_eq!(
                replacement.new_contents,
                format!(
                    "{COMPILED}\n\nVersion 1.0.0\n-------------\n\nReleased on July 1, 2026.\n"
                )
            );
        }
    }

    #[test]
    fn heading_mode_rejects_unterminated_fence_before_region_end() {
        let source = "\
Unreleased
----------

To be released.

```markdown
## Version 9.9.9

Version 1.0.0
-------------

Released on July 1, 2026.
";

        let error = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect_err("unterminated fence makes the region end ambiguous");

        assert!(matches!(error, ChangelogError::UnterminatedCodeFence));
    }

    #[test]
    fn heading_mode_allows_unterminated_fence_after_region_end() {
        let source = "\
Unreleased
----------

To be released.

 -  Old entry.

Version 1.0.0
-------------

Released on July 1, 2026.

```markdown
## Historical example
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(
            replacement.new_contents,
            format!(
                "{COMPILED}\n\nVersion 1.0.0\n-------------\n\nReleased on July 1, 2026.\n\n```markdown\n## Historical example\n"
            )
        );
    }

    #[test]
    fn heading_mode_strips_crlf_before_comparing_lines() {
        let source = "\
Version 1.2.0\r
-------------\r
\r
To be released.\r
\r
 -  Old entry.\r
";

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(replacement.new_contents, COMPILED);
    }

    #[test]
    fn heading_mode_does_not_treat_document_title_as_version_section() {
        let source = "\
Project changes
===============

To be released.
";

        let error = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect_err("document title is not a version section");

        assert!(matches!(error, ChangelogError::RegionNotFound));
    }

    #[test]
    fn heading_mode_requires_real_setext_underline() {
        for source in [
            "Version 1.2.0\n\nTo be released.\n",
            "Version 1.2.0\nnot an underline\n\nTo be released.\n",
        ] {
            let error = replace_unreleased_region(
                source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect_err("invalid underline is not a version section");

            assert!(matches!(error, ChangelogError::RegionNotFound));
        }
    }

    #[test]
    fn marker_mode_rejects_duplicate_markers() {
        let source = "\
<!-- sacho:unreleased:begin -->
<!-- sacho:unreleased:begin -->
<!-- sacho:unreleased:end -->
";

        let error =
            replace_unreleased_region(source, COMPILED, RegionDetection::Marker, "To be released.")
                .expect_err("duplicate marker");

        assert!(matches!(
            error,
            ChangelogError::DuplicateMarker {
                marker: "<!-- sacho:unreleased:begin -->"
            }
        ));
    }

    #[test]
    fn marker_mode_rejects_reversed_markers() {
        let source = "\
<!-- sacho:unreleased:end -->
<!-- sacho:unreleased:begin -->
";

        let error =
            replace_unreleased_region(source, COMPILED, RegionDetection::Marker, "To be released.")
                .expect_err("reversed markers");

        assert!(matches!(error, ChangelogError::ReversedMarkers));
    }

    #[test]
    fn marker_mode_accepts_empty_region() {
        let source = "\
Before.
<!-- sacho:unreleased:begin -->
<!-- sacho:unreleased:end -->
After.
";

        let replacement =
            replace_unreleased_region(source, COMPILED, RegionDetection::Marker, "To be released.")
                .expect("replace");

        assert_eq!(
            replacement.new_contents,
            format!(
                "Before.\n<!-- sacho:unreleased:begin -->\n\n\n{}\n\n<!-- sacho:unreleased:end -->\nAfter.\n",
                COMPILED.trim_end()
            )
        );
    }

    #[test]
    fn empty_marker_replacement_preserves_markdown_normal_form() {
        let source = "<!-- sacho:unreleased:begin -->\n\n<!-- sacho:unreleased:end -->\n";
        assert_eq!(source, format_markdown(source).expect("format source"));

        let replacement =
            replace_unreleased_region(source, "", RegionDetection::Marker, "To be released.")
                .expect("replace");

        assert_eq!(replacement.new_contents, source);
        assert_eq!(
            replacement.new_contents,
            format_markdown(&replacement.new_contents).expect("format replacement")
        );
    }

    #[test]
    fn heading_replacement_preserves_atx_history_spacing() {
        let source = "Unreleased\n----------\n\nTo be released.\n\n### Version 1.0.0\n\nReleased on July 1, 2026.\n";
        assert_eq!(source, format_markdown(source).expect("format source"));

        let replacement = replace_unreleased_region(
            source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert_eq!(
            replacement.new_contents,
            format_markdown(&replacement.new_contents).expect("format replacement")
        );
    }

    #[test]
    fn hongdown_separator_distinguishes_setext_headings_from_prose() {
        for (following, expected) in [
            ("Intro paragraph.\n", "Before.\n\n"),
            ("### Version 1.0.0\n", "Before.\n\n"),
            ("Version 1.0.0\n-------------\n", "Before.\n\n\n"),
            ("<!--\n---\n-->\n", "Before.\n\n"),
        ] {
            let mut output = String::from("Before.\n");

            set_hongdown_separator_before(&mut output, following);

            assert_eq!(output, expected);
        }
    }

    #[test]
    fn replacement_preserves_suffix_without_trailing_newline() {
        let released = "Version 1.0.0\n-------------\n\nReleased on July 1, 2026.";
        let source = format!("Unreleased\n----------\n\nTo be released.\n\n -  Old.\n{released}");

        let replacement = replace_unreleased_region(
            &source,
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect("replace");

        assert!(replacement.new_contents.ends_with(released));
        assert!(!replacement.new_contents.ends_with('\n'));
    }

    #[test]
    fn missing_region_is_reported() {
        let error = replace_unreleased_region(
            "Project changes\n===============\n",
            COMPILED,
            RegionDetection::Heading,
            "To be released.",
        )
        .expect_err("missing region");

        assert!(matches!(error, ChangelogError::RegionNotFound));
    }

    #[test]
    fn atx_heading_parser_accepts_commonmark_headings() {
        assert_eq!(atx_heading_text("# Version 1.2.0"), Some("Version 1.2.0"));
        assert_eq!(
            atx_heading_text("###### Version 1.2.0"),
            Some("Version 1.2.0")
        );
        assert_eq!(
            atx_heading_text("  ## Version 1.2.0 ###  "),
            Some("Version 1.2.0")
        );
        assert_eq!(atx_heading_text("# ###"), Some(""));
    }

    #[test]
    fn atx_heading_parser_rejects_non_headings() {
        assert_eq!(atx_heading_text("Version 1.2.0"), None);
        assert_eq!(atx_heading_text("####### Version 1.2.0"), None);
        assert_eq!(atx_heading_text("#Version 1.2.0"), None);
    }

    #[test]
    fn atx_closing_sequence_requires_preceding_space() {
        assert_eq!(atx_heading_text("# Version 1.2.0#"), Some("Version 1.2.0#"));
        assert_eq!(
            atx_heading_text("# Version 1.2.0 ###x"),
            Some("Version 1.2.0 ###x")
        );
    }

    #[test]
    fn indentation_columns_obeys_markdown_tab_stops() {
        assert_eq!(indentation_columns("## Version 1.2.0"), 0);
        assert_eq!(indentation_columns("   ## Version 1.2.0"), 3);
        assert_eq!(indentation_columns("\t## Version 1.2.0"), 4);
        assert_eq!(indentation_columns(" \t## Version 1.2.0"), 4);
        assert_eq!(indentation_columns("  \t## Version 1.2.0"), 4);
        assert_eq!(indentation_columns("   \t## Version 1.2.0"), 4);
    }

    #[test]
    fn code_fence_parser_accepts_commonmark_fences() {
        assert_eq!(
            opening_code_fence("```rust"),
            Some(CodeFence {
                marker: b'`',
                length: 3
            })
        );
        assert_eq!(
            opening_code_fence("  ~~~~ markdown"),
            Some(CodeFence {
                marker: b'~',
                length: 4
            })
        );
        assert_eq!(opening_code_fence("``"), None);
        assert_eq!(opening_code_fence("```` rust `"), None);
        assert_eq!(opening_code_fence("    ```rust"), None);
    }

    #[test]
    fn code_fence_parser_accepts_only_matching_closing_fences() {
        let fence = opening_code_fence("```rust").expect("opening fence");

        assert!(is_closing_code_fence("```", fence));
        assert!(is_closing_code_fence("  ````  ", fence));
        assert!(!is_closing_code_fence("``", fence));
        assert!(!is_closing_code_fence("``` rust", fence));
        assert!(!is_closing_code_fence("~~~", fence));
        assert!(!is_closing_code_fence("    ```", fence));
    }

    #[test]
    fn strips_valid_atx_closing_sequences() {
        assert_eq!(strip_atx_closing_sequence("Version 1.2.0"), "Version 1.2.0");
        assert_eq!(
            strip_atx_closing_sequence("Version 1.2.0 ###"),
            "Version 1.2.0"
        );
        assert_eq!(
            strip_atx_closing_sequence("Version 1.2.0 ###   "),
            "Version 1.2.0"
        );
        assert_eq!(strip_atx_closing_sequence("###"), "");
        assert_eq!(
            strip_atx_closing_sequence("Version 1.2.0#"),
            "Version 1.2.0#"
        );
    }

    proptest! {
        #[test]
        fn heading_replacement_preserves_prefix_and_suffix(
            prefix in safe_lines(),
            old_region in safe_lines(),
            suffix in safe_lines(),
        ) {
            let released = "Version 1.0.0\n-------------\n\nReleased on July 1, 2026.\n";
            let source = format!(
                "{prefix}Unreleased\n----------\n\nTo be released.\n{old_region}{released}{suffix}"
            );

            let replacement = replace_unreleased_region(
                &source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("replace");

            let expected_prefix = format!("{}{}", prefix, COMPILED);
            let expected_suffix = format!("{}{}", released, suffix);
            prop_assert!(replacement.new_contents.starts_with(&expected_prefix));
            prop_assert!(replacement.new_contents.ends_with(&expected_suffix));
        }

        #[test]
        fn replacing_same_compiled_region_twice_is_idempotent(old_region in safe_lines()) {
            let source = format!(
                "Unreleased\n----------\n\nTo be released.\n{old_region}"
            );

            let first = replace_unreleased_region(
                &source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("first");
            let second = replace_unreleased_region(
                &first.new_contents,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("second");

            prop_assert_eq!(first.new_contents, second.new_contents);
        }

        #[test]
        fn marker_replacement_preserves_bytes_outside_markers(
            prefix in safe_lines(),
            old_region in safe_lines(),
            suffix in safe_lines(),
        ) {
            let before = format!("{prefix}<!-- sacho:unreleased:begin -->\n");
            let after = format!("<!-- sacho:unreleased:end -->\n{suffix}");
            let source = format!("{before}{old_region}{after}");

            let replacement = replace_unreleased_region(
                &source,
                COMPILED,
                RegionDetection::Marker,
                "To be released.",
            )
            .expect("replace");

            prop_assert!(replacement.new_contents.starts_with(&before));
            prop_assert!(replacement.new_contents.ends_with(&after));
        }

        #[test]
        fn heading_replacement_preserves_released_section_bytes(suffix in safe_lines()) {
            let released = format!(
                "Version 1.1.0\n-------------\n\nReleased on July 1, 2026.\n{suffix}"
            );
            let source = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Old.\n{released}"
            );

            let replacement = replace_unreleased_region(
                &source,
                COMPILED,
                RegionDetection::Heading,
                "To be released.",
            )
            .expect("replace");

            prop_assert!(replacement.new_contents.ends_with(&released));
        }
    }
}
