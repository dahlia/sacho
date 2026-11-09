//! Stable section identity comments and their structural attachment.

use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue};

/// Prefix reserved in HTML nodes, including damaged comments.
pub(crate) const RESERVED_PREFIX: &str = "sacho:section:";
const OPEN: &str = "<!-- sacho:section:v1 id=";
const CLOSE: &str = " -->";

/// A malformed or misplaced section identity comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataError {
    /// Original one-based line, or zero when parsing a literal alone.
    pub(crate) line: usize,
    /// Original one-based column, or zero when parsing a literal alone.
    pub(crate) column: usize,
    /// Explanation independent of the enclosing command's error type.
    pub(crate) message: &'static str,
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "section metadata at {}:{}: {}; manually correct or delete the comment and use an explicit section mapping to recover",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for MetadataError {}

fn invalid(message: &'static str) -> MetadataError {
    MetadataError {
        line: 0,
        column: 0,
        message,
    }
}

fn allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~')
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode an exact reserved comment, leaving ordinary HTML alone.
///
/// IDs are decoded once and remain opaque: only wholly blank IDs are rejected.
pub(crate) fn parse_marker(literal: &str) -> Result<Option<String>, MetadataError> {
    if !literal.contains(RESERVED_PREFIX) {
        return Ok(None);
    }
    let token = literal
        .trim()
        .strip_prefix(OPEN)
        .and_then(|s| s.strip_suffix(CLOSE))
        .ok_or_else(|| invalid("expected one exact v1 section identity comment"))?;
    let bytes = token.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut bytes = bytes.iter().copied();
    while let Some(byte) = bytes.next() {
        if allowed(byte) {
            decoded.push(byte);
        } else if byte == b'%' {
            let hi = bytes.next().and_then(hex);
            let lo = bytes.next().and_then(hex);
            match (hi, lo) {
                (Some(hi), Some(lo)) => decoded.push(hi * 16 + lo),
                _ => return Err(invalid("invalid percent escape in section ID")),
            }
        } else {
            return Err(invalid("section ID contains an unencoded byte"));
        }
    }
    let id = String::from_utf8(decoded).map_err(|_| invalid("section ID is not valid UTF-8"))?;
    if id.trim().is_empty() {
        return Err(invalid("section ID must not be blank"));
    }
    Ok(Some(id))
}

/// Render a validated nonblank opaque ID using canonical UTF-8 percent encoding.
pub(crate) fn marker(id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::from(OPEN);
    for byte in id.bytes() {
        if allowed(byte) {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(HEX[usize::from(byte >> 4)]));
            result.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    result.push_str(CLOSE);
    result
}

/// An identity attached to one direct level-three heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionRecord {
    /// Byte offset of the heading in the original source.
    pub(crate) heading_offset: usize,
    /// Original marker bytes, excluding surrounding whitespace/newlines.
    pub(crate) marker_span: Range<usize>,
    /// Decoded stable ID, without trimming or normalization.
    pub(crate) id: String,
}

fn html(value: &NodeValue) -> Option<&str> {
    match value {
        NodeValue::HtmlBlock(block) => Some(&block.literal),
        NodeValue::HtmlInline(literal) => Some(literal),
        _ => None,
    }
}

/// Locate reserved HTML in a selected region, including nested and inline nodes.
///
/// Code and escaped text never appear as HTML and are intentionally ignored.
pub(crate) fn reserved_position<'a>(
    root: &'a AstNode<'a>,
    starts: &[usize],
    range: Range<usize>,
) -> Option<(usize, usize)> {
    root.descendants().find_map(|node| {
        let data = node.data();
        let position = data.sourcepos.start;
        let offset = crate::release_context::node_offset(starts, node);
        (range.contains(&offset) && html(&data.value).is_some_and(|s| s.contains(RESERVED_PREFIX)))
            .then_some((position.line, position.column))
    })
}

/// Strictly validate all reserved HTML descendants in a selected release body.
///
/// The range must align with complete Markdown blocks. Every reserved node must
/// be a standalone direct HTML block immediately following a direct H3.
/// Callers own checking that section configuration exists.
pub(crate) fn records<'a>(
    root: &'a AstNode<'a>,
    source: &str,
    range: Range<usize>,
) -> Result<Vec<SectionRecord>, MetadataError> {
    let starts = crate::release_context::line_starts(source);
    let introductions = crate::release_context::introduction_spans(source);
    let mut records = Vec::new();
    for node in root.descendants() {
        let offset = crate::release_context::node_offset(&starts, node);
        if !range.contains(&offset) {
            continue;
        }
        let data = node.data();
        let Some(literal) = html(&data.value) else {
            continue;
        };
        if !literal.contains(RESERVED_PREFIX) {
            continue;
        }
        let locate = |mut error: MetadataError| {
            error.line = data.sourcepos.start.line;
            error.column = data.sourcepos.start.column;
            error
        };
        if introductions.iter().any(|span| span.contains(&offset)) {
            return Err(locate(invalid(
                "section identity comments are not allowed in release introductions",
            )));
        }
        let id = parse_marker(literal)
            .map_err(locate)?
            .expect("reserved literal parsed above");
        let heading = node.previous_sibling().filter(|heading| {
            heading
                .parent()
                .is_some_and(|parent| std::ptr::eq(parent, root))
                && matches!(&heading.data().value, NodeValue::Heading(h) if h.level == 3)
                && range.contains(&crate::release_context::node_offset(&starts, heading))
        });
        if !matches!(data.value, NodeValue::HtmlBlock(_)) || heading.is_none() {
            return Err(locate(invalid(
                "identity comment must be the first block after a direct level-three heading",
            )));
        }
        let end = data.sourcepos.end;
        let end = starts[end.line.saturating_sub(1)] + end.column;
        let span = source
            .get(offset..end)
            .ok_or_else(|| locate(invalid("invalid marker source position")))?;
        records.push(SectionRecord {
            heading_offset: crate::release_context::node_offset(
                &starts,
                heading.expect("checked above"),
            ),
            marker_span: offset..offset + span.trim_end().len(),
            id,
        });
    }
    Ok(records)
}

/// Probe a single H3 without rejecting unrelated or damaged document content.
///
/// Only the immediately following standalone valid HTML block is attached.
/// Malformed, orphan, nested, and subsequent comments remain visible to callers.
pub(crate) fn probe<'a>(heading: &'a AstNode<'a>) -> Option<String> {
    if !matches!(&heading.data().value, NodeValue::Heading(h) if h.level == 3)
        || !heading
            .parent()
            .is_some_and(|p| matches!(p.data().value, NodeValue::Document))
    {
        return None;
    }
    let node = heading.next_sibling()?;
    let data = node.data();
    let NodeValue::HtmlBlock(block) = &data.value else {
        return None;
    };
    parse_marker(&block.literal).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use comrak::{Arena, parse_document};
    use proptest::prelude::*;

    fn scan(source: &str) -> Result<Vec<SectionRecord>, MetadataError> {
        let arena = Arena::new();
        let root = parse_document(&arena, source, &crate::release_context::options());
        records(root, source, 0..source.len())
    }

    #[test]
    fn codec_contract() {
        assert_eq!(
            marker("a-/%.~_λ"),
            "<!-- sacho:section:v1 id=a%2D%2F%25.~_%CE%BB -->"
        );
        for id in [" key ", "foo\nbar", "\0key", "%2F", "-->"] {
            assert_eq!(parse_marker(&marker(id)).unwrap().as_deref(), Some(id));
        }
        assert_eq!(
            parse_marker("<!-- sacho:section:v1 id=%2f -->")
                .unwrap()
                .as_deref(),
            Some("/")
        );
        assert_eq!(parse_marker("<!-- ordinary -->").unwrap(), None);
        for token in ["", "%20", "%", "%GG", "%FF", "/", "-", "λ"] {
            assert!(
                parse_marker(&format!("{OPEN}{token}{CLOSE}")).is_err(),
                "{token}"
            );
        }
        for literal in [
            "<!-- sacho:section:v2 id=a -->",
            "<!-- sacho:section:v1  id=a -->",
            "<!-- sacho:section:v1 id=a -->extra",
            "<!-- sacho:section:v1 id=a --><!-- ordinary -->",
        ] {
            assert!(parse_marker(literal).is_err());
        }
    }

    #[test]
    fn structural_contract() {
        let m = marker("id");
        let source = format!("### Title\n\n{m}\n\n- Item\n");
        let records = scan(&source).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(&source[records[0].marker_span.clone()], m);
        for source in [
            format!("{m}\n"),
            format!("### T\n\n- Item\n\n{m}\n"),
            format!("### T\n\n{m}\n\n{m}\n"),
            format!("### T\n\n> {m}\n"),
            format!("### T\n\n- Item {m}\n"),
            format!("### T {m}\n"),
            format!("### T\n\nIntro.\n\n{m}\n"),
        ] {
            assert!(scan(&source).is_err(), "{source}");
        }
        scan(&format!(
            "```html\n{m}\n```\n\n`{m}`\n\n\\<!-- sacho:section:v1 id=id -->\n"
        ))
        .unwrap();
    }

    #[test]
    fn selected_region_isolation_and_tolerant_probe() {
        let m = marker("id");
        let source = format!("## Old\n\n<!-- sacho:section:v9 -->\n\n## New\n\n### Title\n\n{m}\n");
        let arena = Arena::new();
        let root = parse_document(&arena, &source, &crate::release_context::options());
        assert!(records(root, &source, 0..source.len()).is_err());
        assert_eq!(
            records(root, &source, source.find("## New").unwrap()..source.len())
                .unwrap()
                .len(),
            1
        );
        let heading = root
            .children()
            .find(|node| matches!(&node.data().value, NodeValue::Heading(h) if h.level == 3))
            .unwrap();
        assert_eq!(probe(heading).as_deref(), Some("id"));
    }

    #[test]
    fn rejects_intro_metadata_and_probes_reserved_html_only() {
        let m = marker("id");
        let source = format!(
            "<!-- sacho:release-intro:begin -->\n\n### Intro\n\n{m}\n\n<!-- sacho:release-intro:end -->\n"
        );
        assert!(scan(&source).is_err());
        for (source, reserved) in [
            (format!("Text {m}\n"), true),
            (format!("> {m}\n"), true),
            (format!("`{m}`\n"), false),
            (format!("```html\n{m}\n```\n"), false),
            (format!("\\{m}\n"), false),
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, &source, &crate::release_context::options());
            let starts = crate::release_context::line_starts(&source);
            assert_eq!(
                reserved_position(root, &starts, 0..source.len()).is_some(),
                reserved
            );
        }
        let arena = Arena::new();
        let source = "### Title\n\n<!-- sacho:section:v9 -->\n";
        let root = parse_document(&arena, source, &crate::release_context::options());
        assert_eq!(probe(root.first_child().unwrap()), None);
    }

    proptest! {
        #[test]
        fn arbitrary_nonblank_ids_round_trip(id in ".*") {
            prop_assume!(!id.trim().is_empty());
            let encoded = marker(&id);
            prop_assert_eq!(parse_marker(&encoded).unwrap(), Some(id));
            let token = encoded.strip_prefix(OPEN).unwrap().strip_suffix(CLOSE).unwrap();
            prop_assert!(!token.contains('-'));
            prop_assert!(!token.contains('/'));
        }

        #[test]
        fn repeated_ids_and_line_endings(id in "[a-zλ% /-]{1,30}", ending in prop::sample::select(vec!["\n", "\r", "\r\n"])) {
            prop_assume!(!id.trim().is_empty());
            let source = format!("### A{ending}{ending}{}{ending}{ending}- First{ending}{ending}### B{ending}{ending}{}{ending}", marker(&id), marker(&id));
            let records = scan(&source).unwrap();
            prop_assert_eq!(records.len(), 2);
            for record in records {
                prop_assert_eq!(&record.id, &id);
                prop_assert_eq!(&source[record.marker_span], marker(&id));
            }
        }
    }
    #[test]
    fn marker_spans_preserve_indented_source_bytes() {
        for indent in 0..=3 {
            for ending in ["\n", "\r\n"] {
                let comment = marker(" key λ ");
                let source = format!(
                    "## Version 1.0.0{ending}{ending}### API{ending}{ending}{}{comment}{ending}{ending} -  Item.{ending}",
                    " ".repeat(indent)
                );
                let records = scan(&source).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(&source[records[0].marker_span.clone()], comment);
                assert_eq!(records[0].marker_span.start, source.find("<!--").unwrap());
                assert_eq!(records[0].id, " key λ ");
            }
        }
    }

    #[test]
    fn probe_rejects_non_h3_and_nested_h3_predecessors() {
        let comment = marker("core");
        for source in [
            format!("Paragraph.\n\n{comment}\n"),
            format!("#### API\n\n{comment}\n"),
            format!("> ### Nested\n>\n> {comment}\n"),
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, &source, &crate::release_context::options());
            for node in root.descendants() {
                assert_eq!(probe(node), None, "{source}");
            }
        }
    }
}
