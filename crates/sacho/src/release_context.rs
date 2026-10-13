//! Per-release context validation and stored introduction boundaries.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, parse_document};

use crate::error::{Error, Result};

pub(crate) const INTRO_BEGIN: &str = "<!-- sacho:release-intro:begin -->";
pub(crate) const INTRO_END: &str = "<!-- sacho:release-intro:end -->";

pub(crate) fn options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.description_lists = true;
    options.extension.alerts = true;
    options.extension.footnotes = true;
    options.extension.tasklist = true;
    options.extension.math_dollars = true;
    options.parse.leave_footnote_definitions = true;
    options
}

fn invalid(option: &str, message: &str) -> Error {
    Error::InvalidReleaseContext {
        option: option.to_owned(),
        message: message.to_owned(),
    }
}

pub(crate) fn note_text(note: Option<&str>) -> Option<&str> {
    note.map(str::trim).filter(|note| !note.is_empty())
}

pub(crate) fn intro_text(intro: Option<&str>) -> Option<&str> {
    intro.filter(|intro| !intro.trim().is_empty())
}

pub(crate) fn validate(note: Option<&str>, intro: Option<&str>) -> Result<()> {
    for (option, source) in [
        ("--note", note_text(note)),
        ("--intro-file", intro_text(intro)),
    ] {
        let Some(source) = source else { continue };
        let arena = Arena::new();
        let root = parse_document(&arena, source, &options());
        for node in root.descendants() {
            let value = &node.data().value;
            let html = match value {
                NodeValue::HtmlBlock(block) => Some(block.literal.as_str()),
                NodeValue::HtmlInline(html) => Some(html.as_str()),
                _ => None,
            };
            if html.is_some_and(|html| {
                [
                    INTRO_BEGIN,
                    INTRO_END,
                    crate::changelog::BEGIN_MARKER,
                    crate::changelog::END_MARKER,
                ]
                .iter()
                .any(|marker| html.contains(marker))
            }) {
                return Err(invalid(
                    option,
                    "reserved Sacho boundary comments are not allowed",
                ));
            }
        }
        if option == "--note" {
            if source.contains(['\r', '\n']) || root.children().count() != 1
                || !root.first_child().is_some_and(|node| matches!(node.data().value, NodeValue::Paragraph)) {
                return Err(invalid(option, "use one inline Markdown paragraph without line breaks; use --intro-file for blocks"));
            }
        } else if root.children().any(|node| matches!(node.data().value, NodeValue::Heading(ref heading) if heading.level <= 2)) {
            return Err(invalid(option, "top-level headings must be level three or deeper"));
        }
    }
    Ok(())
}

pub(crate) fn node_offset(source: &str, node: &AstNode<'_>) -> usize {
    let position = node.data().sourcepos.start;
    source
        .split_inclusive('\n')
        .take(position.line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>()
        + position.column.saturating_sub(1)
}

pub(crate) fn marker(node: &AstNode<'_>) -> Option<bool> {
    match &node.data().value {
        NodeValue::HtmlBlock(block) if block.literal.trim() == INTRO_BEGIN => Some(true),
        NodeValue::HtmlBlock(block) if block.literal.trim() == INTRO_END => Some(false),
        _ => None,
    }
}

pub(crate) fn introduction<'a>(
    source: &str,
    root: &'a AstNode<'a>,
    range: Range<usize>,
) -> Result<Option<Range<usize>>> {
    let nodes = root
        .children()
        .filter(|node| range.contains(&node_offset(source, node)))
        .collect::<Vec<_>>();
    let boundaries = nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| marker(node).map(|begin| (index, begin)))
        .collect::<Vec<_>>();
    match boundaries.as_slice() {
        [] => Ok(None),
        [(begin, true), (end, false)] => {
            if nodes[..*begin].iter().any(|node| matches!(node.data().value, NodeValue::List(_)) || matches!(node.data().value, NodeValue::Heading(ref heading) if heading.level >= 3)) {
                return Err(invalid("release introduction", "introduction boundary comments must precede change items and section headings"));
            }
            Ok(Some(
                node_offset(source, nodes[*begin])..node_end_offset(source, nodes[*end]),
            ))
        }
        _ => Err(invalid(
            "release introduction",
            "expected one ordered introduction boundary pair; a boundary is missing, repeated, or reversed",
        )),
    }
}

fn node_end_offset(source: &str, node: &AstNode<'_>) -> usize {
    let position = node.data().sourcepos.end;
    source
        .split_inclusive('\n')
        .take(position.line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>()
        + position.column
}

/// Collect valid pairs without rejecting unrelated damaged releases.
pub(crate) fn introduction_spans(source: &str) -> Vec<Range<usize>> {
    let arena = Arena::new();
    let root = parse_document(&arena, source, &options());
    let mut begin = None;
    let mut spans = Vec::new();
    for node in root.children() {
        let offset = node_offset(source, node);
        match marker(node) {
            Some(true) => begin = Some(offset),
            Some(false) => {
                if let Some(begin) = begin.take() {
                    spans.push(begin..node_end_offset(source, node));
                }
            }
            None if matches!(node.data().value, NodeValue::Heading(ref heading) if heading.level <= 2) => {
                begin = None
            }
            None => {}
        }
    }
    spans
}

fn footnote_label(name: &str) -> String {
    // Match Comrak's whitespace collapse and Unicode default case folding.
    let name = name.trim_matches(|c: char| c.is_ascii_whitespace());
    let mut normalized = String::new();
    let mut whitespace = false;
    for character in name.chars() {
        if character.is_whitespace() {
            if !whitespace {
                normalized.push(' ');
            }
            whitespace = true;
        } else {
            normalized.push(character);
            whitespace = false;
        }
    }
    caseless::default_case_fold_str(&normalized)
}

fn validate_footnote_definitions(context: &str, source: &str) -> Result<()> {
    let context_arena = Arena::new();
    let context_root = parse_document(&context_arena, context, &options());
    let added = context_root
        .descendants()
        .filter_map(|node| match &node.data().value {
            NodeValue::FootnoteDefinition(definition) => Some(footnote_label(&definition.name)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let arena = Arena::new();
    let root = parse_document(&arena, source, &options());
    let mut definitions = BTreeMap::new();
    for node in root.descendants() {
        let name = match &node.data().value {
            NodeValue::FootnoteDefinition(definition) => footnote_label(&definition.name),
            _ => continue,
        };
        if !added.contains(&name) {
            continue;
        }
        let content = node
            .children()
            .map(|child| signature(child, root, &mut BTreeSet::from([name.clone()])))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(previous) = definitions.insert(name, content.clone())
            && previous != content
        {
            return Err(invalid(
                "--intro-file",
                "conflicting definitions for a normalized footnote identifier",
            ));
        }
    }
    Ok(())
}

fn signature<'a>(
    node: &'a AstNode<'a>,
    root: &'a AstNode<'a>,
    visited: &mut BTreeSet<String>,
) -> String {
    let data = node.data();
    let value = &data.value;
    let mut text = match value {
        NodeValue::Text(text) => return text.to_string(),
        NodeValue::SoftBreak => return " ".to_owned(),
        NodeValue::Code(code) => format!("<code>{:?}</code>", code.literal.as_bytes()),
        NodeValue::CodeBlock(code) => format!(
            "<code_block>{:?}:{:?}</code_block>",
            code.info.as_bytes(),
            code.literal.as_bytes()
        ),
        NodeValue::HtmlBlock(html) => format!("<html>{:?}</html>", html.literal.trim().as_bytes()),
        NodeValue::HtmlInline(html) => format!("<html>{:?}</html>", html.as_bytes()),
        NodeValue::Link(link) => format!(
            "<link {:?} {:?}>",
            link.url.as_bytes(),
            link.title.as_bytes()
        ),
        NodeValue::Image(link) => format!(
            "<image {:?} {:?}>",
            link.url.as_bytes(),
            link.title.as_bytes()
        ),
        NodeValue::Heading(heading) => format!("<heading {}>", heading.level),
        NodeValue::List(list) => format!("<list {:?} {}>", list.list_type, list.start),
        NodeValue::TaskItem(item) => format!("<task_item {}>", item.symbol.is_some()),
        NodeValue::Math(math) => format!(
            "<math {} {} {:?}>",
            math.dollar_math,
            math.display_math,
            math.literal.as_bytes()
        ),
        // Table dimensions follow its children; the fallback retains alignments.
        // The fallback also includes TableRow's header flag without layout metadata.
        NodeValue::Alert(alert) => format!(
            "<alert {:?} {:?}>",
            alert.alert_type,
            alert.title.as_ref().map(|title| title.as_bytes())
        ),
        NodeValue::Item(_) | NodeValue::DescriptionItem(_) | NodeValue::MultilineBlockQuote(_) => {
            format!("<{}>", value.xml_node_name())
        }
        // Root definitions are skipped; references visit only their children.
        NodeValue::FootnoteReference(reference) => {
            let name = footnote_label(&reference.name);
            let mut content = String::new();
            if visited.insert(name.clone()) {
                if let Some(definition) = root.descendants().find(|node| matches!(&node.data().value, NodeValue::FootnoteDefinition(definition) if footnote_label(&definition.name) == name)) {
                    for child in definition.children() { content.push_str(&signature(child, root, visited)); }
                }
                visited.remove(&name);
            }
            format!("<footnote {:?}>{content}</footnote>", name)
        }
        _ => format!("<{value:?}>"),
    };
    drop(data);
    for child in node.children() {
        text.push_str(&signature(child, root, visited));
    }
    text.push_str(&format!("</{}>", node.data().value.xml_node_name()));
    text
}

#[derive(Clone, Copy)]
enum Part {
    Outside,
    Changes,
    Context,
}

fn signatures(source: &str, section: &Range<usize>, part: Part) -> Result<Vec<String>> {
    let arena = Arena::new();
    let root = parse_document(&arena, source, &options());
    let intro = introduction(source, root, section.clone())?;
    let mut date_seen = false;
    let mut result = Vec::new();
    for node in root.children() {
        let offset = node_offset(source, node);
        if matches!(node.data().value, NodeValue::FootnoteDefinition(_)) || marker(node).is_some() {
            continue;
        }
        let inside = section.contains(&offset);
        let is_heading =
            matches!(node.data().value, NodeValue::Heading(ref heading) if heading.level <= 2);
        let date = inside && !date_seen && matches!(node.data().value, NodeValue::Paragraph);
        if date {
            date_seen = true;
        }
        let context = date || intro.as_ref().is_some_and(|span| span.contains(&offset));
        let selected = match part {
            Part::Outside => !inside,
            Part::Changes => inside && !context && !is_heading,
            Part::Context => inside && context,
        };
        if selected {
            result.push(
                signature(node, root, &mut BTreeSet::new())
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    Ok(result)
}

pub(crate) fn validate_meaning(
    baseline: &str,
    baseline_section: Range<usize>,
    raw: &str,
    raw_section: Range<usize>,
    formatted: &str,
    formatted_section: Range<usize>,
    intro: Option<&str>,
) -> Result<()> {
    let option = if intro.is_some() {
        "--intro-file"
    } else {
        "--note"
    };
    if let Some(intro) = intro {
        validate_footnote_definitions(intro, raw)?;
        validate_footnote_definitions(intro, formatted)?;
    }
    for part in [Part::Outside, Part::Changes] {
        if signatures(baseline, &baseline_section, part)?
            != signatures(formatted, &formatted_section, part)?
        {
            return Err(invalid(
                option,
                "reference or footnote definitions change existing changelog content",
            ));
        }
    }
    if signatures(raw, &raw_section, Part::Context)?
        != signatures(formatted, &formatted_section, Part::Context)?
    {
        return Err(invalid(
            option,
            "formatting changes a reference or Markdown block; provide an identical local reference definition",
        ));
    }
    if intro.is_some() {
        let arena = Arena::new();
        let root = parse_document(&arena, raw, &options());
        let span = introduction(raw, root, raw_section.clone())?
            .ok_or_else(|| invalid("--intro-file", "missing introduction boundary comments"))?;
        // Only context definitions should take precedence in this oracle.
        // Moving compiled definitions ahead of the preamble would reject
        // references that are legitimately undefined in the supplied context.
        let context = &raw[raw_section.start..span.end];
        let local = format!("{context}\n\n{raw}");
        if signatures(&local, &(0..context.len()), Part::Context)?
            != signatures(raw, &raw_section, Part::Context)?
        {
            return Err(invalid(
                "--intro-file",
                "local reference or footnote definitions conflict with the changelog",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn footnote_block_attributes_are_part_of_their_meaning() {
        for (old, new) in [
            ("$a$", "$$a$$"),
            ("$a  b$", "$a b$"),
            ("`a  b`", "`a b`"),
            (
                "Text <span title=\"a  b\">inline</span>.",
                "Text <span title=\"a b\">inline</span>.",
            ),
            (
                "<div title=\"a  b\">block</div>",
                "<div title=\"a b\">block</div>",
            ),
            ("[link](/path \"a  b\")", "[link](/path \"a b\")"),
            ("![image](/path \"a  b\")", "![image](/path \"a b\")"),
            ("> [!NOTE] a  b\n    > Read.", "> [!NOTE] a b\n    > Read."),
            (
                "> [!NOTE]\n    > Read this.",
                "> [!WARNING]\n    > Read this.",
            ),
            (
                "| Name |\n    | :--- |\n    | API |",
                "| Name |\n    | ---: |\n    | API |",
            ),
        ] {
            let original = format!("[^x]: {old}\n");
            let changed = format!("[^x]: {new}\n");
            assert!(
                validate_footnote_definitions(&changed, &format!("{original}\n{changed}")).is_err(),
                "{old:?} versus {new:?}"
            );
        }
        validate_footnote_definitions(
            "[^x]: - [x] Read.\n",
            "[^x]: - [X] Read.\n\n[^x]: - [x] Read.\n",
        )
        .unwrap();
    }

    #[test]
    fn footnote_code_indentation_is_part_of_its_meaning() {
        let original = "[^x]:\n\n    ```python\n    if ready:\n        run()\n    ```\n";
        let changed = original.replace("        run()", "      run()");
        let document = format!("{original}\n{changed}");
        assert!(validate_footnote_definitions(&changed, &document).is_err());
    }

    #[test]
    fn footnote_identifiers_use_unicode_case_folding_and_collapsed_whitespace() {
        assert_eq!(footnote_label(" \tStraße \n A   B "), "strasse a b");
        assert_eq!(footnote_label("STRASSE A B"), "strasse a b");
        validate_footnote_definitions("[^X]: Same.\n", "[^x]: Same.\n\n[^X]: Same.\n").unwrap();
        assert!(
            validate_footnote_definitions(
                "[^Straße]: New.\n",
                "[^STRASSE]: Old.\n\n[^Straße]: New.\n"
            )
            .is_err()
        );
        // Unrelated legacy conflicts are not introduced by this context.
        validate_footnote_definitions("Intro.\n", "[^x]: Old.\n\n[^X]: New.\n").unwrap();
    }

    #[test]
    fn validates_note_and_intro_structure() {
        for note in ["", "  ", "Initial release.", "*Initial* `release`.", "-x"] {
            validate(Some(note), None).unwrap();
        }
        for note in [
            "first\nsecond",
            "first\rsecond",
            "# Heading",
            " -  Item",
            "---",
            "```",
            "[a]: /url",
        ] {
            assert!(validate(Some(note), None).is_err(), "{note:?}");
        }
        for intro in ["# Heading", "Heading\n=======", "## Heading"] {
            assert!(validate(None, Some(intro)).is_err(), "{intro:?}");
        }
        for intro in [
            "### Details",
            "> # Nested heading",
            "```markdown\n# Heading\n```",
            "    # Code",
        ] {
            validate(None, Some(intro)).unwrap();
        }
    }

    #[test]
    fn rejects_reserved_comments_outside_code() {
        for marker in [
            INTRO_BEGIN,
            INTRO_END,
            crate::changelog::BEGIN_MARKER,
            crate::changelog::END_MARKER,
        ] {
            for text in [
                marker.to_owned(),
                format!("Text {marker}"),
                format!("> {marker}"),
            ] {
                assert!(validate(None, Some(&text)).is_err(), "{text}");
            }
            validate(
                Some(&format!("`{marker}`")),
                Some(&format!("```\n{marker}\n```")),
            )
            .unwrap();
        }
    }

    #[test]
    fn indented_boundary_comments_do_not_consume_following_items() {
        let source = format!("   {INTRO_BEGIN}\n\nIntro.\n\n   {INTRO_END}\n\n -  Change.\n");
        let arena = Arena::new();
        let root = parse_document(&arena, &source, &options());
        let span = introduction(&source, root, 0..source.len())
            .unwrap()
            .unwrap();
        assert_eq!(&source[span.end..], "\n\n -  Change.\n");
        assert_eq!(introduction_spans(&source), vec![span]);
    }

    #[test]
    fn rejects_malformed_and_misplaced_boundaries() {
        for source in [
            INTRO_BEGIN.to_owned(),
            INTRO_END.to_owned(),
            format!("{INTRO_BEGIN}\n\n{INTRO_BEGIN}\n\n{INTRO_END}"),
            format!("{INTRO_BEGIN}\n\n{INTRO_END}\n\n{INTRO_END}"),
            format!(" -  Change\n\n{INTRO_BEGIN}\n\n{INTRO_END}"),
        ] {
            let arena = Arena::new();
            let root = parse_document(&arena, &source, &options());
            assert!(
                introduction(&source, root, 0..source.len()).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn semantic_signatures_compare_resolved_footnotes_and_ignore_definition_position() {
        let before =
            "Version 1.0\n-----------\n\nReleased today.\n\n -  Item[^x].\n\n[^x]: Original.\n";
        let after = before.replace("Original.", "Changed.");
        assert!(
            validate_meaning(
                before,
                0..before.len(),
                before,
                0..before.len(),
                &after,
                0..after.len(),
                None,
            )
            .is_err()
        );
        assert!(
            signatures(before, &(0..before.len()), Part::Changes).unwrap()[0].contains("Original.")
        );
    }

    #[test]
    fn description_padding_does_not_change_meaning() {
        let source = format!("{INTRO_BEGIN}\n\nTerm\n: Definition.\n\n{INTRO_END}\n");
        let padded = source.replace(": Definition.", ":   Definition.");
        assert_eq!(
            signatures(&source, &(0..source.len()), Part::Context).unwrap(),
            signatures(&padded, &(0..padded.len()), Part::Context).unwrap(),
        );
    }

    #[test]
    fn completing_empty_table_cells_does_not_change_meaning() {
        let source = format!(
            "{INTRO_BEGIN}\n\n| First | Second |\n| --- | --- |\n| Value |\n\n{INTRO_END}\n"
        );
        let completed = source.replace("| Value |", "| Value | | ");
        assert_eq!(
            signatures(&source, &(0..source.len()), Part::Context).unwrap(),
            signatures(&completed, &(0..completed.len()), Part::Context).unwrap(),
        );
    }

    #[test]
    fn context_formatting_preserves_block_meaning() {
        for intro in [
            "* First.\n* Second.",
            "1) First.\n2) Second.",
            "### Details ###\n\nRead this.",
            "Use ``word`` here.",
        ] {
            let source = format!(
                "# Version 1.0\n\nReleased today.\n\n{INTRO_BEGIN}\n\n{intro}\n\n{INTRO_END}\n"
            );
            let formatted = crate::markdown::format_markdown(&source).unwrap();
            validate_meaning(
                &source,
                0..source.len(),
                &source,
                0..source.len(),
                &formatted,
                0..formatted.len(),
                Some(intro),
            )
            .unwrap();
        }
    }

    proptest! {
        #[test]
        fn semantic_signatures_ignore_soft_line_wrapping(
            first in "[A-Za-z]{1,40}",
            second in "[A-Za-z]{1,40}",
        ) {
            let inline = format!("{first} {second}\n");
            let wrapped = format!("{first}\n{second}\n");
            prop_assert_eq!(
                signatures(&inline, &(0..inline.len()), Part::Context).unwrap(),
                signatures(&wrapped, &(0..wrapped.len()), Part::Context).unwrap(),
            );
        }

        #[test]
        fn introduction_spans_never_cross_release_headings(
            level in 1usize..=2,
            words in "[A-Za-z][A-Za-z ]{0,80}",
        ) {
            for heading in [
                format!("{} {words}", "#".repeat(level)),
                format!("{words}\n{}", if level == 1 { "========" } else { "--------" }),
            ] {
                let source = format!("{INTRO_BEGIN}\n\nDamaged introduction.\n\n{heading}\n\nReleased today.\n\n{INTRO_END}\n");
                prop_assert!(introduction_spans(&source).is_empty());
                let valid = format!("\n{INTRO_BEGIN}\n\nValid introduction.\n\n{INTRO_END}\n");
                let document = format!("{source}{valid}");
                let spans = introduction_spans(&document);
                prop_assert_eq!(spans.len(), 1);
                prop_assert!(spans[0].start >= source.len());
            }
        }

        #[test]
        fn fenced_marker_examples_never_open_an_introduction(words in "[A-Za-z ]{0,80}") {
            let source = format!("```markdown\n{INTRO_BEGIN}\n{words}\n{INTRO_END}\n```\n");
            prop_assert!(introduction_spans(&source).is_empty());
            validate(None, Some(&source)).unwrap();
        }

        #[test]
        fn generated_plain_notes_are_inline(words in "[A-Za-z][A-Za-z ]{0,240}") {
            validate(Some(&words), None).unwrap();
            let source = format!("Version 1.0\n-----------\n\nReleased on October 13, 2026.  {words}\n");
            let formatted = crate::markdown::format_markdown(&source).unwrap();
            prop_assert_eq!(&formatted, &crate::markdown::format_markdown(&formatted).unwrap());
            let arena = Arena::new();
            let root = parse_document(&arena, &formatted, &options());
            prop_assert_eq!(root.children().filter(|node| matches!(node.data().value, NodeValue::Paragraph)).count(), 1);
            prop_assert!(!root.descendants().any(|node| matches!(node.data().value, NodeValue::LineBreak)));
        }
    }
}
