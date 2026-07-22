//! Released changelog section parsing for carry and merge workflows.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;

use comrak::nodes::{AstNode, ListType, NodeValue, Sourcepos};
use comrak::{Arena, Options as ComrakOptions, format_commonmark, parse_document};

use crate::changelog::{ReleasedSection, UnreleasedRegionSpan};
use crate::config::SectionConfig;
use crate::error::{Error, Result};
use crate::markdown::format_markdown;
use crate::repo::Repository;

/// Entries decompiled from a released changelog section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    /// Version whose released section was parsed.
    pub version: String,

    /// Fragment outputs to write.
    pub fragments: Vec<CarriedFragment>,
}

/// One fragment output produced by carrying a released section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedFragment {
    /// Section identifier, or `None` for repositories without sections.
    pub section: Option<String>,

    /// Repository-relative path for the fragment.
    pub path: PathBuf,

    /// Fragment Markdown source.
    pub markdown: String,
}

/// Fragments decompiled from the current unreleased changelog region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedUnreleased {
    /// Version inferred from a `Version X` heading, or `None` for `Unreleased`.
    pub version: Option<String>,

    /// Fragment outputs to write.
    pub fragments: Vec<CarriedFragment>,
}

/// A possible Sacho section found in an existing changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogSectionCandidate {
    /// Text of the direct level-three heading.
    pub id: String,

    /// Number of matching level-three heading occurrences.
    pub occurrences: usize,

    /// Whether the heading occurs in the current unreleased region.
    pub appears_in_unreleased: bool,
}

#[derive(Debug, Clone)]
struct TargetSection<'a> {
    body: &'a str,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct ParsedEntry {
    section: Option<String>,
    item_markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceDefinition {
    label: String,
    url: String,
}

#[derive(Debug, Clone)]
struct SectionCandidateRegion {
    unreleased: bool,
    sections: Vec<String>,
}

/// Finds direct level-three headings inside changelog version regions.
///
/// The first level-one or Setext heading is ignored when it matches
/// `document_title` so a title such as `Version history` does not open a
/// version region.
///
/// Candidates from a region containing `unreleased_heading` are ordered before
/// candidates found only in released history. Within each group, candidates
/// retain their first-appearance order. Callers must still ask a person which
/// headings represent stable repository sections because change categories
/// such as `Added` have the same Markdown shape.
pub fn discover_section_candidates(
    changelog: &str,
    document_title: &str,
    unreleased_heading: &str,
) -> Vec<ChangelogSectionCandidate> {
    let arena = Arena::new();
    let root = parse_document(&arena, changelog, &comrak_options());
    let mut regions = Vec::<SectionCandidateRegion>::new();
    let mut current = None;
    let mut first_heading = true;

    for child in root.children() {
        match &child.data().value {
            NodeValue::Heading(heading) => {
                let text = plain_text(child).trim().to_owned();
                if first_heading && (heading.level == 1 || heading.setext) && text == document_title
                {
                    first_heading = false;
                    continue;
                }
                first_heading = false;
                if heading.level <= 2 && (text == "Unreleased" || is_version_heading_text(&text)) {
                    regions.push(SectionCandidateRegion {
                        unreleased: text == "Unreleased",
                        sections: Vec::new(),
                    });
                    current = Some(regions.len() - 1);
                } else if heading.level == 3
                    && let Some(index) = current
                    && !text.is_empty()
                {
                    regions[index].sections.push(text);
                }
            }
            NodeValue::Paragraph
                if current.is_some() && plain_text(child).trim() == unreleased_heading =>
            {
                regions[current.expect("checked above")].unreleased = true;
            }
            _ => {}
        }
    }

    let mut candidates = Vec::<ChangelogSectionCandidate>::new();
    for unreleased in [true, false] {
        for region in regions
            .iter()
            .filter(|region| region.unreleased == unreleased)
        {
            for id in &region.sections {
                if candidates.iter().any(|candidate| candidate.id == *id) {
                    continue;
                }
                let occurrences = regions
                    .iter()
                    .flat_map(|region| &region.sections)
                    .filter(|section| *section == id)
                    .count();
                candidates.push(ChangelogSectionCandidate {
                    id: id.clone(),
                    occurrences,
                    appears_in_unreleased: regions
                        .iter()
                        .filter(|region| region.unreleased)
                        .flat_map(|region| &region.sections)
                        .any(|section| section == id),
                });
            }
        }
    }
    candidates
}

/// Parses a released version section and returns deterministic carry fragments.
pub fn carry_release(repo: &Repository, changelog: &str, version: &str) -> Result<CarriedRelease> {
    let target = find_target_section(changelog, version, None).ok_or_else(|| {
        Error::ReleasedVersionNotFound {
            version: version.to_owned(),
        }
    })?;
    let entries = parse_entries(repo, target.body)?;
    let mut grouped = BTreeMap::<Option<String>, Vec<String>>::new();
    for entry in entries {
        grouped
            .entry(entry.section)
            .or_default()
            .push(entry.item_markdown);
    }

    let mut fragments = Vec::new();
    if repo.config().sections.is_empty() {
        let items = grouped.remove(&None).unwrap_or_default();
        if !items.is_empty() {
            fragments.push(CarriedFragment {
                section: None,
                path: carried_fragment_path(repo, None, version)?,
                markdown: fragment_markdown(items),
            });
        }
    } else {
        for section in &repo.config().sections {
            let section_id = Some(section.id.clone());
            let Some(items) = grouped.remove(&section_id) else {
                continue;
            };
            fragments.push(CarriedFragment {
                section: section_id,
                path: carried_fragment_path(repo, Some(section), version)?,
                markdown: fragment_markdown(items),
            });
        }
    }

    Ok(CarriedRelease {
        version: version.to_owned(),
        fragments,
    })
}

/// Parses a materialized unreleased region into deterministic fragments.
///
/// Only the version heading, configured unreleased date line, direct
/// level-three section headings, and top-level unordered lists are accepted.
/// This conservative shape prevents an import from silently dropping hand
/// edits that fragments cannot represent.
pub fn import_unreleased_region(repo: &Repository, region: &str) -> Result<ImportedUnreleased> {
    let arena = Arena::new();
    let root = parse_document(&arena, region, &comrak_options());
    let line_starts = line_starts(region);
    let references = reference_definitions(region);
    let mut version = None;
    let mut saw_heading = false;
    let mut current_section = None;
    let mut entries = Vec::new();

    for child in root.children() {
        let value = child.data().value.clone();
        match value {
            NodeValue::Heading(heading) if heading.level <= 2 && !saw_heading => {
                let heading = plain_text(child).trim().to_owned();
                if heading == "Unreleased" {
                    version = None;
                } else if let Some(inferred) = heading.strip_prefix("Version ")
                    && !inferred.trim().is_empty()
                {
                    version = Some(inferred.trim().to_owned());
                } else {
                    return Err(import_incompatible(
                        "expected `Unreleased` or `Version X` heading",
                    ));
                }
                saw_heading = true;
            }
            NodeValue::Heading(heading)
                if heading.level == 3 && !repo.config().sections.is_empty() =>
            {
                if !saw_heading {
                    return Err(import_incompatible(
                        "section heading appears before version heading",
                    ));
                }
                let section = plain_text(child).trim().to_owned();
                ensure_known_section(repo, &section)?;
                current_section = Some(section);
            }
            NodeValue::Heading(heading) if heading.level == 3 => {
                if !saw_heading {
                    return Err(import_incompatible(
                        "heading appears before version heading",
                    ));
                }
                return Err(import_incompatible(
                    "section headings are not supported without configured sections",
                ));
            }
            NodeValue::Paragraph
                if saw_heading
                    && plain_text(child).trim() == repo.config().changelog.unreleased_heading => {}
            NodeValue::List(list) if saw_heading && list.list_type == ListType::Bullet => {
                if !repo.config().sections.is_empty() && current_section.is_none() {
                    return Err(Error::ReleasedEntryWithoutSection);
                }
                for item in child.children() {
                    let source = slice_item_sourcepos(region, &line_starts, item.data().sourcepos)
                        .unwrap_or_default();
                    entries.push(ParsedEntry {
                        section: current_section.clone(),
                        item_markdown: fold_item_references(
                            source,
                            item,
                            &line_starts,
                            &references,
                        ),
                    });
                }
            }
            _ => {
                return Err(import_incompatible(
                    "only the version heading, unreleased date line, section headings, and unordered lists are supported",
                ));
            }
        }
    }
    if !saw_heading {
        return Err(import_incompatible("unreleased version heading is missing"));
    }
    if entries.is_empty() {
        return Err(Error::UnreleasedImportEmpty);
    }

    Ok(ImportedUnreleased {
        version,
        fragments: fragments_from_entries(repo, entries, "imported-unreleased.md")?,
    })
}

fn import_incompatible(message: &str) -> Error {
    Error::UnreleasedImportIncompatible {
        message: message.to_owned(),
    }
}

fn fragments_from_entries(
    repo: &Repository,
    entries: Vec<ParsedEntry>,
    filename: &str,
) -> Result<Vec<CarriedFragment>> {
    let mut grouped = BTreeMap::<Option<String>, Vec<String>>::new();
    for entry in entries {
        grouped
            .entry(entry.section)
            .or_default()
            .push(entry.item_markdown);
    }
    let mut fragments = Vec::new();
    if repo.config().sections.is_empty() {
        let items = grouped.remove(&None).unwrap_or_default();
        if !items.is_empty() {
            fragments.push(CarriedFragment {
                section: None,
                path: repo.config().fragments.directory.join(filename),
                markdown: fragment_markdown(items),
            });
        }
    } else {
        for section in &repo.config().sections {
            let section_id = Some(section.id.clone());
            let Some(items) = grouped.remove(&section_id) else {
                continue;
            };
            fragments.push(CarriedFragment {
                section: section_id,
                path: repo
                    .config()
                    .fragments
                    .directory
                    .join(&section.directory)
                    .join(filename),
                markdown: fragment_markdown(items),
            });
        }
    }
    Ok(fragments)
}

/// Finds a released version section in changelog Markdown.
///
/// The returned Markdown preserves the source from the version heading through
/// the byte immediately before the next released version heading. A candidate
/// inside `unreleased_region` is ignored when that span is supplied.
pub fn find_released_section(
    source: &str,
    version: &str,
    unreleased_region: Option<UnreleasedRegionSpan>,
) -> Option<ReleasedSection> {
    let target = find_target_section(source, version, unreleased_region)?;
    Some(ReleasedSection {
        version: version.to_owned(),
        markdown: source[target.start..target.end].to_owned(),
    })
}

/// Reports whether the changelog contains any released version section.
///
/// A version heading inside `unreleased_region` is ignored when that span is
/// supplied. The first Markdown heading is also ignored when its text exactly
/// matches `document_title` and it is either a level-one ATX or Setext heading.
pub fn has_released_sections(
    source: &str,
    unreleased_region: Option<UnreleasedRegionSpan>,
    document_title: &str,
) -> bool {
    let lines = source_lines(source);
    let document_title_start =
        matching_document_title(&lines, document_title).map(|heading| heading.start);
    version_heading_candidates(&lines)
        .into_iter()
        .any(|candidate| {
            document_title_start != Some(candidate.start)
                && !unreleased_region
                    .is_some_and(|region| (region.start..region.end).contains(&candidate.start))
        })
}

pub(crate) fn insertion_title_span(source: &str, document_title: &str) -> Option<Range<usize>> {
    let lines = source_lines(source);
    let heading = first_markdown_heading(&lines)?;
    let matches_configured_title = heading.text == document_title
        && (heading.level == 1 || heading.style == HeadingStyle::Setext);
    let is_conventional_title = heading.level == 1 && !is_version_heading_text(heading.text);
    (matches_configured_title || is_conventional_title).then_some(heading.start..heading.body_start)
}

pub(crate) fn render_released_section(
    source: &str,
    version: &str,
    unreleased_region: Option<UnreleasedRegionSpan>,
    skip_heading: bool,
) -> Result<Option<ReleasedSection>> {
    let Some(target) = find_target_section(source, version, unreleased_region) else {
        return Ok(None);
    };
    let markdown = render_target_section(source, &target, skip_heading)?;
    Ok(Some(ReleasedSection {
        version: version.to_owned(),
        markdown,
    }))
}

fn render_target_section(
    source: &str,
    target: &TargetSection<'_>,
    skip_heading: bool,
) -> Result<String> {
    let arena = Arena::new();
    let options = comrak_options();
    let document = parse_document(&arena, source, &options);
    let section = arena.alloc(NodeValue::Document.into());
    let line_starts = line_starts(source);
    let mut nodes = document
        .children()
        .filter(|node| {
            sourcepos_start_offset(&line_starts, node.data().sourcepos.start)
                .is_some_and(|start| (target.start..target.end).contains(&start))
        })
        .collect::<Vec<_>>();
    if skip_heading
        && nodes
            .first()
            .is_some_and(|node| matches!(node.data().value, NodeValue::Heading(_)))
    {
        nodes.remove(0);
    }
    let footnote_definitions = referenced_footnote_definitions(document, &nodes);
    for node in nodes {
        section.append(node);
    }
    for definition in footnote_definitions {
        section.append(definition);
    }

    let mut rendered = String::new();
    format_commonmark(section, &options, &mut rendered)
        .expect("writing CommonMark to a String cannot fail");
    format_markdown(&rendered)
}

fn referenced_footnote_definitions<'a>(
    document: &'a AstNode<'a>,
    selected_nodes: &[&'a AstNode<'a>],
) -> Vec<&'a AstNode<'a>> {
    let definitions = document
        .children()
        .filter(|node| matches!(node.data().value, NodeValue::FootnoteDefinition(_)))
        .collect::<Vec<_>>();
    let mut referenced = BTreeSet::new();
    for node in selected_nodes {
        collect_footnote_references(node, &mut referenced);
    }
    let mut pending = referenced.iter().cloned().collect::<VecDeque<_>>();
    while let Some(name) = pending.pop_front() {
        let Some(definition) = definitions.iter().find(|definition| {
            matches!(
                &definition.data().value,
                NodeValue::FootnoteDefinition(definition) if definition.name == name
            )
        }) else {
            continue;
        };
        let mut nested_references = BTreeSet::new();
        collect_footnote_references(definition, &mut nested_references);
        for nested in nested_references {
            if referenced.insert(nested.clone()) {
                pending.push_back(nested);
            }
        }
    }

    definitions
        .into_iter()
        .filter(|definition| match &definition.data().value {
            NodeValue::FootnoteDefinition(definition) => referenced.contains(&definition.name),
            _ => false,
        })
        .collect()
}

fn collect_footnote_references<'a>(node: &'a AstNode<'a>, references: &mut BTreeSet<String>) {
    if let NodeValue::FootnoteReference(reference) = &node.data().value {
        references.insert(reference.name.clone());
    }
    for child in node.children() {
        collect_footnote_references(child, references);
    }
}

fn carried_fragment_path(
    repo: &Repository,
    section: Option<&SectionConfig>,
    version: &str,
) -> Result<PathBuf> {
    let filename = format!("carried-from-{version}.md");
    validate_carried_filename(&filename)?;
    let path = match section {
        Some(section) => repo
            .config()
            .fragments
            .directory
            .join(&section.directory)
            .join(filename),
        None => repo.config().fragments.directory.join(filename),
    };
    Ok(path)
}

fn validate_carried_filename(filename: &str) -> Result<()> {
    if filename.contains('/') || filename.contains('\\') {
        return Err(Error::InvalidFragmentName {
            name: filename.to_owned(),
            reason: "carried version must form a single path component",
        });
    }
    Ok(())
}

fn fragment_markdown(items: Vec<String>) -> String {
    let mut markdown = String::new();
    for item in items {
        markdown.push_str(item.trim_end());
        markdown.push('\n');
    }
    markdown
}

fn find_target_section<'a>(
    source: &'a str,
    version: &str,
    unreleased_region: Option<UnreleasedRegionSpan>,
) -> Option<TargetSection<'a>> {
    let lines = source_lines(source);
    let candidates = version_heading_candidates(&lines);
    let target = format!("Version {version}");
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.text != target {
            continue;
        }
        if unreleased_region
            .is_some_and(|region| (region.start..region.end).contains(&candidate.start))
        {
            continue;
        }
        let end = candidates
            .get(index + 1)
            .map(|next| next.start)
            .unwrap_or(source.len());
        return Some(TargetSection {
            body: &source[candidate.body_start..end],
            start: candidate.start,
            end,
        });
    }
    None
}

fn parse_entries(repo: &Repository, body: &str) -> Result<Vec<ParsedEntry>> {
    let arena = Arena::new();
    let root = parse_document(&arena, body, &comrak_options());
    let line_starts = line_starts(body);
    let mut entries = Vec::new();
    let mut current_section = None;
    let references = reference_definitions(body);

    for child in root.children() {
        let value = child.data().value.clone();
        match value {
            NodeValue::Heading(heading)
                if heading.level == 3 && !repo.config().sections.is_empty() =>
            {
                let section = plain_text(child).trim().to_owned();
                ensure_known_section(repo, &section)?;
                current_section = Some(section);
            }
            NodeValue::List(list) if list.list_type == ListType::Bullet => {
                if !repo.config().sections.is_empty() && current_section.is_none() {
                    return Err(Error::ReleasedEntryWithoutSection);
                }
                for item in child.children() {
                    let source = slice_item_sourcepos(body, &line_starts, item.data().sourcepos)
                        .unwrap_or_default();
                    let markdown = fold_item_references(source, item, &line_starts, &references);
                    entries.push(ParsedEntry {
                        section: current_section.clone(),
                        item_markdown: markdown,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(entries)
}

fn ensure_known_section(repo: &Repository, section: &str) -> Result<()> {
    if repo
        .config()
        .sections
        .iter()
        .any(|configured| configured.id == section)
    {
        Ok(())
    } else {
        Err(Error::UnknownSection {
            section: section.to_owned(),
        })
    }
}

fn fold_item_references<'a>(
    source: &str,
    item: &'a AstNode<'a>,
    line_starts: &[usize],
    references: &[ReferenceDefinition],
) -> String {
    let item_start_offset = item_source_start_offset(line_starts, item.data().sourcepos)
        .unwrap_or_else(|| {
            sourcepos_start_offset(line_starts, item.data().sourcepos.start).unwrap_or(0)
        });
    let mut replacements = Vec::<(usize, usize, String)>::new();
    let mut labels_for_item = BTreeSet::<String>::new();
    collect_link_replacements(
        item,
        references,
        line_starts,
        item_start_offset,
        &mut replacements,
        &mut labels_for_item,
    );

    replacements.sort_by_key(|(start, _, _)| *start);
    let mut output = source.to_owned();
    for (start, end, replacement) in replacements.into_iter().rev() {
        output.replace_range(start..end, &replacement);
    }

    for label in labels_for_item {
        if !output.contains(&format!("[{label}]")) {
            append_reference_label(&mut output, &label);
        }
    }
    output.trim_end().to_owned()
}

fn collect_link_replacements<'a>(
    node: &'a AstNode<'a>,
    references: &[ReferenceDefinition],
    line_starts: &[usize],
    item_start_offset: usize,
    replacements: &mut Vec<(usize, usize, String)>,
    labels_for_item: &mut BTreeSet<String>,
) {
    if let NodeValue::Link(link) = &node.data().value
        && let Some(reference) = references
            .iter()
            .find(|reference| reference.url == link.url)
    {
        let label_text = plain_text(node).trim().to_owned();
        labels_for_item.insert(reference.label.clone());
        if label_text == reference.label
            && let Some((start, end)) = sourcepos_offsets(line_starts, node.data().sourcepos)
        {
            replacements.push((
                start.saturating_sub(item_start_offset),
                end.saturating_sub(item_start_offset),
                format!("[{}]", reference.label),
            ));
        }
    }
    for child in node.children() {
        collect_link_replacements(
            child,
            references,
            line_starts,
            item_start_offset,
            replacements,
            labels_for_item,
        );
    }
}

fn append_reference_label(markdown: &mut String, label: &str) {
    let trimmed = markdown.trim_end();
    markdown.truncate(trimmed.len());
    markdown.push_str("  [[");
    markdown.push_str(label);
    markdown.push_str("]]");
}

fn reference_definitions(source: &str) -> Vec<ReferenceDefinition> {
    let mut definitions = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some((label, url)) = rest.split_once("]:") else {
            continue;
        };
        let url = url.trim();
        if label.is_empty() || url.is_empty() {
            continue;
        }
        definitions.push(ReferenceDefinition {
            label: label.to_owned(),
            url: url.to_owned(),
        });
    }
    definitions.sort_by(|left, right| left.label.cmp(&right.label));
    definitions
}

#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct VersionHeading<'a> {
    start: usize,
    body_start: usize,
    text: &'a str,
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for raw in source.split_inclusive('\n') {
        let end = start + raw.len();
        let text = raw.trim_end_matches(['\n', '\r']);
        lines.push(SourceLine { start, end, text });
        start = end;
    }
    lines
}

fn version_heading_candidates<'a>(lines: &'a [SourceLine<'a>]) -> Vec<VersionHeading<'a>> {
    let mut headings = Vec::new();
    let mut code_fence = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(fence) = code_fence {
            if is_closing_code_fence(line.text, fence) {
                code_fence = None;
            }
            continue;
        }
        if let Some(fence) = opening_code_fence(line.text) {
            code_fence = Some(fence);
            continue;
        }
        if !is_indented_code_line(line.text)
            && let Some(text) = atx_heading_text(line.text)
            && is_version_heading_text(text)
        {
            headings.push(VersionHeading {
                start: line.start,
                body_start: line.end,
                text,
            });
        }
        if lines
            .get(index + 1)
            .is_some_and(|next| !is_indented_code_line(next.text) && is_setext_underline(next.text))
        {
            let text = line.text.trim();
            if !is_indented_code_line(line.text) && is_version_heading_text(text) {
                headings.push(VersionHeading {
                    start: line.start,
                    body_start: lines[index + 1].end,
                    text,
                });
            }
        }
    }
    headings.sort_by_key(|heading| heading.start);
    headings
}

fn matching_document_title<'a>(
    lines: &'a [SourceLine<'a>],
    document_title: &str,
) -> Option<VersionHeading<'a>> {
    let heading = first_markdown_heading(lines)?;
    (heading.text == document_title
        && (heading.level == 1 || heading.style == HeadingStyle::Setext))
        .then_some(VersionHeading {
            start: heading.start,
            body_start: heading.body_start,
            text: heading.text,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadingStyle {
    Atx,
    Setext,
}

#[derive(Debug, Clone, Copy)]
struct MarkdownHeading<'a> {
    start: usize,
    body_start: usize,
    text: &'a str,
    level: usize,
    style: HeadingStyle,
}

fn first_markdown_heading<'a>(lines: &'a [SourceLine<'a>]) -> Option<MarkdownHeading<'a>> {
    let mut code_fence = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(fence) = code_fence {
            if is_closing_code_fence(line.text, fence) {
                code_fence = None;
            }
            continue;
        }
        if let Some(fence) = opening_code_fence(line.text) {
            code_fence = Some(fence);
            continue;
        }
        if is_indented_code_line(line.text) {
            continue;
        }
        if let Some((level, text)) = atx_heading(line.text) {
            return Some(MarkdownHeading {
                start: line.start,
                body_start: line.end,
                text,
                level,
                style: HeadingStyle::Atx,
            });
        }
        if !line.text.trim().is_empty()
            && let Some(next) = lines.get(index + 1)
            && !is_indented_code_line(next.text)
            && let Some(level) = setext_heading_level(next.text)
        {
            let text = line.text.trim();
            return Some(MarkdownHeading {
                start: line.start,
                body_start: next.end,
                text,
                level,
                style: HeadingStyle::Setext,
            });
        }
    }
    None
}

fn is_version_heading_text(text: &str) -> bool {
    text.starts_with("Version ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeFence {
    marker: u8,
    length: usize,
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
    text.starts_with('-') && text.trim_matches('-').is_empty()
}

fn setext_heading_level(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.starts_with('=') && text.trim_matches('=').is_empty() {
        Some(1)
    } else if text.starts_with('-') && text.trim_matches('-').is_empty() {
        Some(2)
    } else {
        None
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
    atx_heading(line).map(|(_, text)| text)
}

fn atx_heading(line: &str) -> Option<(usize, &str)> {
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
    Some((
        marker_count,
        strip_atx_closing_sequence(after_markers.trim()),
    ))
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

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn slice_item_sourcepos<'a>(
    source: &'a str,
    line_starts: &[usize],
    sourcepos: Sourcepos,
) -> Option<&'a str> {
    let start = item_source_start_offset(line_starts, sourcepos)?;
    let (_, end) = sourcepos_offsets(line_starts, sourcepos)?;
    source.get(start..end)
}

fn item_source_start_offset(line_starts: &[usize], sourcepos: Sourcepos) -> Option<usize> {
    let start_line = sourcepos.start.line.checked_sub(1)?;
    line_starts.get(start_line).copied()
}

fn sourcepos_offsets(line_starts: &[usize], sourcepos: Sourcepos) -> Option<(usize, usize)> {
    let start = sourcepos_start_offset(line_starts, sourcepos.start)?;
    let end_line = sourcepos.end.line.checked_sub(1)?;
    let end_line_start = *line_starts.get(end_line)?;
    let end = end_line_start + sourcepos.end.column;
    Some((start, end))
}

fn sourcepos_start_offset(
    line_starts: &[usize],
    position: comrak::nodes::LineColumn,
) -> Option<usize> {
    let start_line = position.line.checked_sub(1)?;
    Some(*line_starts.get(start_line)? + position.column.checked_sub(1)?)
}

fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
    let data = node.data();
    match &data.value {
        NodeValue::Text(text) => text.to_string(),
        NodeValue::Code(code) => code.literal.clone(),
        NodeValue::CodeBlock(code) => code.literal.clone(),
        NodeValue::HtmlInline(html) => html.clone(),
        NodeValue::HtmlBlock(html) => html.literal.clone(),
        NodeValue::Math(math) => math.literal.clone(),
        NodeValue::SoftBreak | NodeValue::LineBreak => String::from(" "),
        NodeValue::FootnoteReference(reference) => reference.name.clone(),
        _ => {
            drop(data);
            let mut text = String::new();
            for child in node.children() {
                text.push_str(&plain_text(child));
            }
            text
        }
    }
}

fn comrak_options() -> ComrakOptions<'static> {
    let mut options = ComrakOptions::default();
    options.extension.table = true;
    options.extension.description_lists = true;
    options.extension.alerts = true;
    options.extension.footnotes = true;
    options.extension.tasklist = true;
    options.extension.math_dollars = true;
    options
}

#[cfg(test)]
mod tests {
    use std::fs;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;

    fn repo_with_config(config: &str) -> (TempDir, Repository) {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), config).expect("config");
        let repo = Repository::from_root(temp.path()).expect("repo");
        (temp, repo)
    }

    #[test]
    fn discovers_section_candidates_with_unreleased_entries_first() {
        let changelog = "\
Project changes
===============

Version 2.0.0
-------------

To be released.

### core

 -  Added a core feature.

### Added

 -  Added something else.

### Version 9.9.9

 -  This is a section heading, not a version region.

Version 1.0.0
-------------

Released on July 1, 2026.

### cli

 -  Added a command.

### core

 -  Fixed the core.

 -  A nested heading:

    ### ignored

```markdown
### also ignored
```
";

        let candidates =
            discover_section_candidates(changelog, "Project changes", "To be released.");

        assert_eq!(
            candidates,
            vec![
                ChangelogSectionCandidate {
                    id: String::from("core"),
                    occurrences: 2,
                    appears_in_unreleased: true,
                },
                ChangelogSectionCandidate {
                    id: String::from("Added"),
                    occurrences: 1,
                    appears_in_unreleased: true,
                },
                ChangelogSectionCandidate {
                    id: String::from("Version 9.9.9"),
                    occurrences: 1,
                    appears_in_unreleased: true,
                },
                ChangelogSectionCandidate {
                    id: String::from("cli"),
                    occurrences: 1,
                    appears_in_unreleased: false,
                },
            ]
        );
    }

    #[test]
    fn discovers_candidates_below_a_literal_unreleased_heading() {
        let candidates = discover_section_candidates(
            "## Unreleased\n\n### server\n\n -  Added a server.\n",
            "Project changes",
            "To be released.",
        );

        assert_eq!(
            candidates,
            vec![ChangelogSectionCandidate {
                id: String::from("server"),
                occurrences: 1,
                appears_in_unreleased: true,
            }]
        );
    }

    #[test]
    fn ignores_a_version_shaped_document_title_and_its_preamble() {
        for underline in ["===============", "---------------"] {
            let changelog = format!(
                "\
Version history
{underline}

### Notation

This heading describes the document.

Version 1.0.0
-------------

Released on July 1, 2026.

### core

 -  Added core.
"
            );

            let candidates =
                discover_section_candidates(&changelog, "Version history", "To be released.");

            assert_eq!(
                candidates,
                vec![ChangelogSectionCandidate {
                    id: String::from("core"),
                    occurrences: 1,
                    appears_in_unreleased: false,
                }]
            );
        }
    }

    #[test]
    fn ignores_an_exact_version_shaped_document_title() {
        let candidates = discover_section_candidates(
            "# Version 9.9.9\n\n### Notation\n\nDocument notation.\n",
            "Version 9.9.9",
            "To be released.",
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn keeps_a_version_shaped_h1_that_is_not_the_document_title() {
        let candidates = discover_section_candidates(
            "# Version 9.9.9\n\n### core\n\n -  Added core.\n",
            "Project changes",
            "To be released.",
        );

        assert_eq!(
            candidates,
            vec![ChangelogSectionCandidate {
                id: String::from("core"),
                occurrences: 1,
                appears_in_unreleased: false,
            }]
        );
    }

    #[test]
    fn finds_setext_version_target() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

 -  Fixed carry.  [[#8]]

[#8]: https://example.com/issues/8
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");

        assert_eq!(
            carried.fragments[0].path,
            PathBuf::from("changes.d/carried-from-1.1.5.md")
        );
        assert!(
            carried.fragments[0]
                .markdown
                .contains(" -  Fixed carry.  [[#8]]\n")
        );
    }

    #[test]
    fn returns_exact_markdown_for_a_released_section() {
        let source = "\
Project changelog
=================

## Version 1.2.0

Released on July 19, 2026.

 -  Added show.

Version 1.1.0
-------------

Older release.
";

        let released = find_released_section(source, "1.2.0", None).expect("released section");

        assert_eq!(released.version, "1.2.0");
        assert_eq!(
            released.markdown,
            "## Version 1.2.0\n\nReleased on July 19, 2026.\n\n -  Added show.\n\n"
        );
    }

    #[test]
    fn reports_released_sections_outside_the_unreleased_region() {
        let source = "\
Version 1.2.0
-------------

To be released.

Version 1.1.0
-------------

Released on July 1, 2026.
";
        let unreleased = UnreleasedRegionSpan {
            start: 0,
            end: source.find("Version 1.1.0").expect("released heading"),
        };

        assert!(has_released_sections(
            source,
            Some(unreleased),
            "Project changes"
        ));
        assert!(!has_released_sections(
            &source[..unreleased.end],
            Some(unreleased),
            "Project changes"
        ));
    }

    #[test]
    fn ignores_h3_headings_when_repository_has_no_sections() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

### Fixed

 -  Fixed carry.

### Added

 -  Added carry.
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");

        assert_eq!(carried.fragments.len(), 1);
        assert_eq!(
            carried.fragments[0].path,
            PathBuf::from("changes.d/carried-from-1.1.5.md")
        );
        assert_eq!(
            carried.fragments[0].markdown,
            " -  Fixed carry.\n -  Added carry.\n"
        );
    }

    #[test]
    fn imports_unreleased_entries_into_stable_fragment_names() {
        let (_temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        );
        let region = "\
Version 2.4.0
-------------

To be released.

### core

 -  Added core support.

### cli

 -  Added a command.
";

        let imported = import_unreleased_region(&repo, region).expect("import");

        assert_eq!(imported.version.as_deref(), Some("2.4.0"));
        assert_eq!(
            imported
                .fragments
                .iter()
                .map(|fragment| fragment.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("changes.d/core/imported-unreleased.md"),
                PathBuf::from("changes.d/cli/imported-unreleased.md"),
            ]
        );
    }

    #[test]
    fn rejects_top_level_prose_during_unreleased_import() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(
            &repo,
            "Unreleased\n----------\n\nTo be released.\n\nHand-written note.\n",
        )
        .expect_err("top-level prose");

        assert!(matches!(error, Error::UnreleasedImportIncompatible { .. }));
    }

    #[test]
    fn rejects_section_headings_without_configured_sections_during_import() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(
            &repo,
            "Unreleased\n----------\n\nTo be released.\n\n### Fixed\n\n -  Fixed a bug.\n",
        )
        .expect_err("unrepresentable section heading");

        assert!(matches!(error, Error::UnreleasedImportIncompatible { .. }));
        assert!(error.to_string().contains("configured sections"));
    }

    #[test]
    fn rejects_empty_unreleased_import() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(&repo, "Unreleased\n----------\n\nTo be released.\n")
            .expect_err("empty import");

        assert!(matches!(error, Error::UnreleasedImportEmpty));
    }

    #[test]
    fn reports_missing_version() {
        let (_temp, repo) = repo_with_config("");

        let error = carry_release(&repo, "Version 1.0.0\n-------------\n", "1.1.5")
            .expect_err("missing version");

        assert!(matches!(error, Error::ReleasedVersionNotFound { .. }));
    }

    #[test]
    fn rejects_carried_versions_that_would_escape_fragment_file() {
        let (_temp, repo) = repo_with_config("");

        for version in ["bad/name", "bad\\name"] {
            let changelog = format!(
                "Version {version}\n-------------\n\nReleased on July 7, 2026.\n\n -  Fixed carry.\n"
            );

            let error = carry_release(&repo, &changelog, version).expect_err("unsafe filename");

            assert!(matches!(error, Error::InvalidFragmentName { .. }));
        }
    }

    #[test]
    fn maps_configured_section_heading_to_directory() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "@optique/core"
            directory = "core"
            "#,
        );
        let changelog = "\
## Version 1.1.5

Released on July 7, 2026.

### @optique/core

 -  Added core change.
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");

        assert_eq!(
            carried.fragments[0].path,
            PathBuf::from("changes.d/core/carried-from-1.1.5.md")
        );
    }

    #[test]
    fn rejects_unknown_section_heading() {
        let (_temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "@optique/core"
            directory = "core"
            "#,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

### unknown

 -  Added unknown change.
";

        let error = carry_release(&repo, changelog, "1.1.5").expect_err("unknown section");

        assert!(matches!(error, Error::UnknownSection { section } if section == "unknown"));
    }

    #[test]
    fn folds_full_links_that_match_reference_definitions() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

 -  Fixed carry.  [[#8](https://example.com/issues/8)]

[#8]: https://example.com/issues/8
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");

        assert!(
            carried.fragments[0]
                .markdown
                .contains(" -  Fixed carry.  [[#8]]\n")
        );
    }

    #[test]
    fn appends_reference_label_when_link_text_does_not_identify_reference() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

 -  Fixed carry.  [issue](https://example.com/issues/8)

[#8]: https://example.com/issues/8
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");

        assert!(
            carried.fragments[0]
                .markdown
                .contains(" -  Fixed carry.  [issue](https://example.com/issues/8)  [[#8]]\n")
        );
    }

    #[test]
    fn ignores_empty_reference_definitions() {
        let definitions = reference_definitions(
            "\
[]: https://example.com/issues/8
[#8]:
[#9]: https://example.com/issues/9
",
        );

        assert_eq!(
            definitions,
            vec![ReferenceDefinition {
                label: String::from("#9"),
                url: String::from("https://example.com/issues/9"),
            }]
        );
    }

    #[test]
    fn scans_released_version_headings_outside_code_blocks() {
        let source = "\
```markdown
Version 9.9.9
-------------
```

    Version 8.8.8
    -------------

## Version 1.2.0

body

Version 1.1.0
-------------

old body

Version 7.7.7
not an underline

Not a version
-------------

###### Version 1.0.0

six hashes
";
        let lines = source_lines(source);
        let headings = version_heading_candidates(&lines);

        assert_eq!(
            headings
                .iter()
                .map(|heading| heading.text)
                .collect::<Vec<_>>(),
            vec!["Version 1.2.0", "Version 1.1.0", "Version 1.0.0"]
        );
        assert_eq!(&source[headings[0].body_start..][..6], "\nbody\n");
        assert_eq!(&source[headings[1].body_start..][..10], "\nold body\n");
    }

    #[test]
    fn released_sections_ignore_a_matching_atx_document_title() {
        let source = "\
# Version history

## Version 1.0.0

Released on July 20, 2026.
";

        assert!(has_released_sections(source, None, "Version history"));
        assert!(has_released_sections(source, None, "Changelog"));
        assert!(!has_released_sections(
            "# Version history\n",
            None,
            "Version history"
        ));
        assert!(!has_released_sections(
            "\n<!-- Project release history. -->\n\n# Version history\n",
            None,
            "Version history"
        ));
        assert!(!has_released_sections(
            "Version history\n---------------\n",
            None,
            "Version history"
        ));
    }

    #[test]
    fn insertion_title_rejects_an_atx_h2_even_when_its_text_matches() {
        assert_eq!(
            insertion_title_span("## Project changes\n", "Project changes"),
            None
        );
    }

    #[test]
    fn finds_first_line_h1_version_section() {
        let source = "\
# Version 1.0.0

Released on July 20, 2026.
";

        let released = find_released_section(source, "1.0.0", None).expect("released section");

        assert_eq!(released.markdown, source);
    }

    proptest! {
        #[test]
        fn released_section_stops_exactly_before_the_next_version_heading(
            suffix in "[a-z0-9]{1,12}",
            body in "[a-z0-9 .!?]{0,80}",
        ) {
            let version = format!("target-{suffix}");
            let heading = format!("Version {version}");
            let target = format!(
                "{heading}\n{}\n\nRelease note: {body}\n\n",
                "-".repeat(heading.len()),
            );
            let source = format!(
                "Version before\n--------------\n\nOlder.\n\n{target}Version after\n-------------\n\nNewer.\n"
            );

            let released =
                find_released_section(&source, &version, None).expect("released section");

            prop_assert_eq!(released.markdown, target);
        }
    }

    #[test]
    fn indentation_columns_obeys_markdown_tab_stops() {
        assert_eq!(indentation_columns("   x"), 3);
        assert_eq!(indentation_columns("    x"), 4);
        assert_eq!(indentation_columns("\tx"), 4);
        assert_eq!(indentation_columns(" \tx"), 4);
        assert_eq!(indentation_columns("  \tx"), 4);
    }

    #[test]
    fn code_fence_helpers_accept_commonmark_fences() {
        let backtick = opening_code_fence("```markdown").expect("backtick fence");
        let tilde = opening_code_fence("~~~~ markdown").expect("tilde fence");

        assert_eq!(
            backtick,
            CodeFence {
                marker: b'`',
                length: 3,
            }
        );
        assert_eq!(
            tilde,
            CodeFence {
                marker: b'~',
                length: 4,
            }
        );
        assert!(opening_code_fence("``").is_none());
        assert!(opening_code_fence("    ```").is_none());
        assert!(is_closing_code_fence("```", backtick));
        assert!(is_closing_code_fence("````", backtick));
        assert!(!is_closing_code_fence("~~~", backtick));
        assert!(!is_closing_code_fence("``` info", backtick));
    }

    #[test]
    fn heading_helpers_accept_only_version_headings() {
        assert_eq!(atx_heading_text("## Version 1.2.0"), Some("Version 1.2.0"));
        assert_eq!(
            atx_heading_text("### Version 1.2.0 ###"),
            Some("Version 1.2.0")
        );
        assert_eq!(atx_heading_text("####### Version 1.2.0"), None);
        assert_eq!(atx_heading_text("##Version 1.2.0"), None);
        assert!(is_setext_underline("-------------"));
        assert!(!is_setext_underline("---- x"));
        assert!(is_version_heading_text("Version 1.2.0"));
        assert!(!is_version_heading_text("Unreleased"));
    }

    #[test]
    fn plain_text_extracts_extended_markdown_nodes() {
        fn first_block_text(source: &str) -> String {
            let arena = Arena::new();
            let root = parse_document(&arena, source, &comrak_options());
            plain_text(root.first_child().expect("first block"))
        }

        assert_eq!(first_block_text("`code`\n"), "code");
        assert_eq!(first_block_text("soft\nbreak\n"), "soft break");
        assert_eq!(
            first_block_text("[^note]\n\n[^note]: Footnote text.\n"),
            "note"
        );
        assert_eq!(first_block_text("$math$\n"), "math");

        let arena = Arena::new();
        let root = parse_document(
            &arena,
            "\
Paragraph `code` <span>html</span> $math$
soft
[^note]

<div>
html block
</div>

```
code block
```

[^note]: Footnote text.
",
            &comrak_options(),
        );

        let text = plain_text(root);

        for expected in [
            "Paragraph",
            "code",
            "<span>",
            "math",
            "soft",
            "note",
            "html block",
            "code block",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text:?}");
        }
    }
}
