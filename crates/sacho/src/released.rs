//! Released changelog section parsing for carry and merge workflows.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, ListType, NodeValue, Sourcepos};
use comrak::{Arena, Options as ComrakOptions, format_commonmark, parse_document};
use indexmap::IndexMap;
use serde::Serialize;

use crate::changelog::{ReleasedSection, UnreleasedRegionSpan};
use crate::config::{ReferenceSigil, UrlTemplate};
use crate::error::{Error, Result};
use crate::fragment::{
    ReferenceUse, comrak_options_with_configured_references, configured_link_marker_prefix,
    configured_link_references_with_bracket_wrapped, parse_reference_label,
    render_item_with_configured_markers, replace_configured_link_markers,
};
use crate::markdown::{escape_angle_bracket_destination, format_markdown_with_word_wrap};
use crate::repo::Repository;
use crate::section::{ResolvedSection, SectionResolver, has_sections};

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
struct TargetSection {
    body_start: usize,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct ParsedEntry {
    section: Option<String>,
    item_markdown: String,
    links: BTreeMap<String, String>,
    markers: Vec<(String, String)>,
    ordinary_references: Vec<OrdinaryReference>,
}

#[derive(Debug, Clone)]
struct FoldedItem {
    markdown: String,
    links: BTreeMap<String, String>,
    markers: Vec<(String, String)>,
    ordinary_references: Vec<OrdinaryReference>,
}

#[derive(Debug, Clone)]
struct OrdinaryReference {
    marker: String,
    text: String,
    url: String,
    title: String,
    image: bool,
}

#[derive(Debug, Clone, Default)]
struct FragmentContents {
    items: Vec<String>,
    links: BTreeMap<String, String>,
    markers: Vec<(String, String)>,
    ordinary_references: Vec<OrdinaryReference>,
}

#[derive(Serialize)]
struct FragmentFrontmatter<'a> {
    links: &'a BTreeMap<String, String>,
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
    let entries = parse_entries(repo, changelog, target.body_start..target.end)?;
    let mut grouped = BTreeMap::<Option<String>, FragmentContents>::new();
    for entry in entries {
        merge_fragment_entry(&mut grouped, entry)?;
    }

    let mut fragments = Vec::new();
    if !has_sections(repo.config()) {
        let contents = grouped.remove(&None).unwrap_or_default();
        if !contents.items.is_empty() {
            let path = carried_fragment_path(repo, None, version)?;
            fragments.push(CarriedFragment {
                section: None,
                markdown: fragment_markdown(&path, contents)?,
                path,
            });
        }
    } else {
        for section in grouped_sections(repo, &grouped)? {
            let section_id = Some(section.id.clone());
            let Some(contents) = grouped.remove(&section_id) else {
                continue;
            };
            let path = carried_fragment_path(repo, Some(&section.directory), version)?;
            fragments.push(CarriedFragment {
                section: section_id,
                markdown: fragment_markdown(&path, contents)?,
                path,
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
    import_unreleased_document_region(repo, region, 0..region.len())
}

pub(crate) fn import_unreleased_document_region(
    repo: &Repository,
    document: &str,
    region: Range<usize>,
) -> Result<ImportedUnreleased> {
    let arena = Arena::new();
    let options = comrak_options_with_configured_references(&BTreeMap::new(), &repo.config().links);
    let root = parse_document(&arena, document, &options);
    let configured_links = released_configured_links(repo, document, root);
    let marker_prefix = configured_link_marker_prefix(document);
    let line_starts = line_starts(document);
    let mut version = None;
    let mut saw_heading = false;
    let mut current_section = None;
    let mut entries = Vec::new();

    for child in root.children().filter(|child| {
        sourcepos_start_offset(&line_starts, child.data().sourcepos.start)
            .is_some_and(|start| region.contains(&start))
    }) {
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
            NodeValue::Heading(heading) if heading.level == 3 && has_sections(repo.config()) => {
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
                if has_sections(repo.config()) && current_section.is_none() {
                    return Err(Error::ReleasedEntryWithoutSection);
                }
                for item in child.children() {
                    let folded = fold_item_references(
                        item,
                        &configured_links,
                        &marker_prefix,
                        &repo.config().links,
                    )?;
                    entries.push(ParsedEntry {
                        section: current_section.clone(),
                        item_markdown: folded.markdown,
                        links: folded.links,
                        markers: folded.markers,
                        ordinary_references: folded.ordinary_references,
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
    let mut grouped = BTreeMap::<Option<String>, FragmentContents>::new();
    for entry in entries {
        merge_fragment_entry(&mut grouped, entry)?;
    }
    let mut fragments = Vec::new();
    if !has_sections(repo.config()) {
        let contents = grouped.remove(&None).unwrap_or_default();
        if !contents.items.is_empty() {
            let path = repo.config().fragments.directory.join(filename);
            fragments.push(CarriedFragment {
                section: None,
                markdown: fragment_markdown(&path, contents)?,
                path,
            });
        }
    } else {
        for section in grouped_sections(repo, &grouped)? {
            let section_id = Some(section.id.clone());
            let Some(contents) = grouped.remove(&section_id) else {
                continue;
            };
            let path = repo
                .config()
                .fragments
                .directory
                .join(section.directory)
                .join(filename);
            fragments.push(CarriedFragment {
                section: section_id,
                markdown: fragment_markdown(&path, contents)?,
                path,
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
    word_wrap: bool,
) -> Result<Option<ReleasedSection>> {
    let Some(target) = find_target_section(source, version, unreleased_region) else {
        return Ok(None);
    };
    let markdown = render_target_section(source, &target, skip_heading, word_wrap)?;
    Ok(Some(ReleasedSection {
        version: version.to_owned(),
        markdown,
    }))
}

fn render_target_section(
    source: &str,
    target: &TargetSection,
    skip_heading: bool,
    word_wrap: bool,
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
    format_markdown_with_word_wrap(&rendered, word_wrap)
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
    section_directory: Option<&Path>,
    version: &str,
) -> Result<PathBuf> {
    let filename = format!("carried-from-{version}.md");
    validate_carried_filename(&filename)?;
    let path = match section_directory {
        Some(directory) => repo
            .config()
            .fragments
            .directory
            .join(directory)
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

fn merge_fragment_entry(
    grouped: &mut BTreeMap<Option<String>, FragmentContents>,
    entry: ParsedEntry,
) -> Result<()> {
    let contents = grouped.entry(entry.section).or_default();
    for (label, url) in &entry.links {
        if let Some(existing) = contents.links.get(label)
            && existing != url
        {
            return Err(Error::ConflictingResolvedLinks {
                label: label.clone(),
                first: existing.clone(),
                second: url.clone(),
            });
        }
    }
    contents.items.push(entry.item_markdown);
    contents.links.extend(entry.links);
    contents.markers.extend(entry.markers);
    contents
        .ordinary_references
        .extend(entry.ordinary_references);
    Ok(())
}

fn fragment_markdown(path: &Path, contents: FragmentContents) -> Result<String> {
    let mut markdown = String::new();
    if !contents.links.is_empty() {
        let serialized = serde_yaml_ng::to_string(&FragmentFrontmatter {
            links: &contents.links,
        })
        .map_err(|source| Error::Fragment {
            path: path.to_path_buf(),
            source: crate::FragmentError::Frontmatter { source },
        })?;
        markdown.push_str("---\n");
        markdown.push_str(serialized.trim_start_matches("---\n").trim_end());
        markdown.push_str("\n---\n");
    }
    let mut body = String::new();
    for item in contents.items {
        body.push_str(item.trim_end());
        body.push('\n');
    }
    for reference in &contents.ordinary_references {
        let image_marker = if reference.image { "!" } else { "" };
        body = body.replace(
            &reference.marker,
            &format!("{image_marker}[{}][{}]", reference.text, reference.marker),
        );
    }
    if !contents.ordinary_references.is_empty() {
        body.push('\n');
        for reference in &contents.ordinary_references {
            append_ordinary_reference_definition(&mut body, reference);
        }
    }
    let formatted = format_markdown_with_word_wrap(&body, true)?;
    markdown.push_str(&replace_configured_link_markers(
        formatted,
        &contents.markers,
    ));
    markdown.push('\n');
    Ok(markdown)
}

fn append_ordinary_reference_definition(markdown: &mut String, reference: &OrdinaryReference) {
    markdown.push('[');
    markdown.push_str(&reference.marker);
    markdown.push_str("]: <");
    markdown.push_str(&escape_angle_bracket_destination(&reference.url));
    markdown.push('>');
    if !reference.title.is_empty() {
        markdown.push_str(" \"");
        for character in reference.title.chars() {
            markdown.push_str(&format!("&#{};", u32::from(character)));
        }
        markdown.push('"');
    }
    markdown.push('\n');
}

fn find_target_section(
    source: &str,
    version: &str,
    unreleased_region: Option<UnreleasedRegionSpan>,
) -> Option<TargetSection> {
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
            body_start: candidate.body_start,
            start: candidate.start,
            end,
        });
    }
    None
}

fn parse_entries(
    repo: &Repository,
    document: &str,
    body: Range<usize>,
) -> Result<Vec<ParsedEntry>> {
    let arena = Arena::new();
    let options = comrak_options_with_configured_references(&BTreeMap::new(), &repo.config().links);
    let root = parse_document(&arena, document, &options);
    let configured_links = released_configured_links(repo, document, root);
    let marker_prefix = configured_link_marker_prefix(document);
    let line_starts = line_starts(document);
    let mut entries = Vec::new();
    let mut current_section = None;

    for child in root.children().filter(|child| {
        sourcepos_start_offset(&line_starts, child.data().sourcepos.start)
            .is_some_and(|start| body.contains(&start))
    }) {
        let value = child.data().value.clone();
        match value {
            NodeValue::Heading(heading) if heading.level == 3 && has_sections(repo.config()) => {
                let section = plain_text(child).trim().to_owned();
                ensure_known_section(repo, &section)?;
                current_section = Some(section);
            }
            NodeValue::List(list) if list.list_type == ListType::Bullet => {
                if has_sections(repo.config()) && current_section.is_none() {
                    return Err(Error::ReleasedEntryWithoutSection);
                }
                for item in child.children() {
                    let folded = fold_item_references(
                        item,
                        &configured_links,
                        &marker_prefix,
                        &repo.config().links,
                    )?;
                    entries.push(ParsedEntry {
                        section: current_section.clone(),
                        item_markdown: folded.markdown,
                        links: folded.links,
                        markers: folded.markers,
                        ordinary_references: folded.ordinary_references,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(entries)
}

fn ensure_known_section(repo: &Repository, section: &str) -> Result<()> {
    if SectionResolver::from_config(repo.config())?
        .resolve_id(section)
        .map_err(|error| Error::SectionPattern {
            message: error.to_string(),
        })?
        .is_some()
    {
        Ok(())
    } else {
        Err(Error::UnknownSection {
            section: section.to_owned(),
        })
    }
}

fn grouped_sections(
    repo: &Repository,
    grouped: &BTreeMap<Option<String>, FragmentContents>,
) -> Result<Vec<ResolvedSection>> {
    let present = grouped
        .keys()
        .filter_map(Clone::clone)
        .collect::<BTreeSet<_>>();
    let resolver = SectionResolver::from_config(repo.config())?;
    let sections = resolver
        .ordered_present(&present)
        .map_err(|error| Error::SectionPattern {
            message: error.to_string(),
        })?
        .into_iter()
        .map(|id| {
            resolver
                .resolve_id(&id)
                .map_err(|error| Error::SectionPattern {
                    message: error.to_string(),
                })?
                .ok_or(Error::UnknownSection { section: id })
        })
        .collect::<Result<Vec<_>>>()?;
    for section in &sections {
        if section.pattern_index.is_some() {
            repo.validate_pattern_section_directory(&section.directory)?;
        }
    }
    Ok(sections)
}

fn fold_item_references<'a>(
    item: &'a AstNode<'a>,
    configured_references: &BTreeMap<Sourcepos, ReferenceUse>,
    marker_prefix: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> Result<FoldedItem> {
    let mut links = BTreeMap::<String, String>::new();
    collect_configured_links(item, configured_references, &mut links)?;
    let options = comrak_options();
    let ordinary_nodes = ordinary_configured_looking_links(
        item,
        &options,
        configured_references,
        marker_prefix,
        link_templates,
    );
    let removed_children = ordinary_nodes
        .iter()
        .map(|(node, reference, _)| {
            let children = node.children().collect::<Vec<_>>();
            for child in &children {
                child.detach();
            }
            node.data_mut().value = NodeValue::Text(reference.marker.clone().into());
            children
        })
        .collect::<Vec<_>>();
    let (mut markdown, markers) =
        render_item_with_configured_markers(item, &options, configured_references, marker_prefix);
    for ((node, _, value), children) in ordinary_nodes.iter().zip(removed_children) {
        node.data_mut().value = value.clone();
        for child in children {
            node.append(child);
        }
    }
    let end = markdown.trim_end().len();
    markdown.truncate(end);
    Ok(FoldedItem {
        markdown,
        links,
        markers,
        ordinary_references: ordinary_nodes
            .into_iter()
            .map(|(_, reference, _)| reference)
            .collect(),
    })
}

fn ordinary_configured_looking_links<'a>(
    item: &'a AstNode<'a>,
    options: &ComrakOptions<'_>,
    configured_references: &BTreeMap<Sourcepos, ReferenceUse>,
    marker_prefix: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> Vec<(&'a AstNode<'a>, OrdinaryReference, NodeValue)> {
    item.descendants()
        .filter_map(|node| {
            let data = node.data();
            let (link, image) = match &data.value {
                NodeValue::Link(link) => (link, false),
                NodeValue::Image(link) => (link, true),
                _ => return None,
            };
            let position = data.sourcepos;
            if configured_references.contains_key(&position)
                || node
                    .descendants()
                    .skip(1)
                    .any(|child| configured_references.contains_key(&child.data().sourcepos))
                || node
                    .ancestors()
                    .skip(1)
                    .any(|ancestor| configured_references.contains_key(&ancestor.data().sourcepos))
            {
                return None;
            }
            let text = plain_text(node);
            let reference = parse_reference_label(text.trim(), link_templates)
                .ok()
                .flatten()?;
            if reference.label != text.trim() {
                return None;
            }
            let mut rendered_text = String::new();
            for child in node.children() {
                format_commonmark(child, options, &mut rendered_text)
                    .expect("writing CommonMark to a String cannot fail");
            }
            Some((
                node,
                OrdinaryReference {
                    marker: format!(
                        "{marker_prefix}ordinaryx{}x{}x{}x{}",
                        position.start.line,
                        position.start.column,
                        position.end.line,
                        position.end.column,
                    ),
                    text: rendered_text.trim_end().to_owned(),
                    url: link.url.clone(),
                    title: link.title.clone(),
                    image,
                },
                data.value.clone(),
            ))
        })
        .collect()
}

fn collect_configured_links<'a>(
    node: &'a AstNode<'a>,
    configured_references: &BTreeMap<Sourcepos, ReferenceUse>,
    links: &mut BTreeMap<String, String>,
) -> Result<()> {
    if let NodeValue::Link(link) | NodeValue::Image(link) = &node.data().value
        && let Some(reference) = configured_references.get(&node.data().sourcepos)
    {
        let label = reference.label.as_str();
        if crate::fragment::validate_resolved_link(label, &link.url).is_ok() {
            if let Some(existing) = links.get(label)
                && existing != &link.url
            {
                return Err(Error::ConflictingResolvedLinks {
                    label: label.to_owned(),
                    first: existing.clone(),
                    second: link.url.clone(),
                });
            }
            links.insert(label.to_owned(), link.url.clone());
        }
    }
    for child in node.children() {
        collect_configured_links(child, configured_references, links)?;
    }
    Ok(())
}

fn released_configured_links<'a>(
    repo: &Repository,
    source: &str,
    root: &'a AstNode<'a>,
) -> BTreeMap<Sourcepos, ReferenceUse> {
    configured_link_references_with_bracket_wrapped(source, root, &repo.config().links)
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

    fn rendered_links(markdown: &str) -> Vec<(String, String, String)> {
        fn collect<'a>(node: &'a AstNode<'a>, links: &mut Vec<(String, String, String)>) {
            if let NodeValue::Link(link) = &node.data().value {
                links.push((plain_text(node), link.url.clone(), link.title.clone()));
            }
            for child in node.children() {
                collect(child, links);
            }
        }

        let arena = Arena::new();
        let root = parse_document(&arena, markdown, &comrak_options());
        let mut links = Vec::new();
        collect(root, &mut links);
        links
    }

    fn rendered_images(markdown: &str) -> Vec<(String, String, String)> {
        fn collect<'a>(node: &'a AstNode<'a>, images: &mut Vec<(String, String, String)>) {
            if let NodeValue::Image(image) = &node.data().value {
                images.push((plain_text(node), image.url.clone(), image.title.clone()));
            }
            for child in node.children() {
                collect(child, images);
            }
        }

        let arena = Arena::new();
        let root = parse_document(&arena, markdown, &comrak_options());
        let mut images = Vec::new();
        collect(root, &mut images);
        images
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
                .contains(" -  Fixed carry.  [[#8]]\n"),
            "{}",
            carried.fragments[0].markdown
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
    fn rejects_a_level_three_unreleased_region_heading() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(&repo, "### Unreleased\n\n -  Added a feature.\n")
            .expect_err("level-three region heading");

        assert!(matches!(error, Error::UnreleasedImportIncompatible { .. }));
    }

    #[test]
    fn rejects_a_second_version_heading_during_import() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(
            &repo,
            "Unreleased\n----------\n\nTo be released.\n\n -  Added one feature.\n\n## Unreleased\n\n -  Added another feature.\n",
        )
        .expect_err("second version heading");

        assert!(matches!(error, Error::UnreleasedImportIncompatible { .. }));
    }

    #[test]
    fn rejects_ordered_lists_during_import() {
        let (_temp, repo) = repo_with_config("");
        let error = import_unreleased_region(
            &repo,
            "Unreleased\n----------\n\nTo be released.\n\n 1. Added a feature.\n",
        )
        .expect_err("ordered list");

        assert!(matches!(error, Error::UnreleasedImportIncompatible { .. }));
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
    fn maps_patterned_section_heading_to_directory_without_source_tree() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "@optique/{name}"
            directory = "{name}"
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
    fn rejects_an_ambiguous_patterned_carry_directory() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "{name}"
            "#,
        );
        let changelog = "\
## Version 1.1.5

Released on July 7, 2026.

### pkg/core

 -  Added core change.
";

        let error =
            carry_release(&repo, changelog, "1.1.5").expect_err("ambiguous carry directory");

        let message = error.to_string();
        assert!(message.contains("ambiguous"), "{message}");
    }

    #[test]
    fn rejects_an_ambiguous_patterned_import_directory() {
        let (_temp, repo) = repo_with_config(
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "{name}"
            "#,
        );
        let region = "\
Unreleased
----------

To be released.

### pkg/core

 -  Added core support.
";

        let error =
            import_unreleased_region(&repo, region).expect_err("ambiguous import directory");

        let message = error.to_string();
        assert!(message.contains("ambiguous"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_patterned_carry_directory_symlinked_outside_the_repository() {
        use std::os::unix::fs::symlink;

        let (temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[section-patterns]]
            source = "packages/{name}"
            id = "@optique/{name}"
            directory = "{name}"
            "#,
        );
        let outside = TempDir::new().expect("outside tempdir");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        symlink(outside.path(), temp.path().join("changes.d/core")).expect("outside symlink");
        let changelog = "\
## Version 1.1.5

Released on July 7, 2026.

### @optique/core

 -  Added core change.
";

        let error = carry_release(&repo, changelog, "1.1.5").expect_err("outside directory");

        let message = error.to_string();
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
        assert!(
            outside
                .path()
                .read_dir()
                .expect("outside directory")
                .next()
                .is_none()
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
                .contains(" -  Fixed carry.  [[#8]]\n"),
            "{}",
            carried.fragments[0].markdown
        );
    }

    #[test]
    fn preserves_folded_reference_definitions_as_carried_fragment_pins() {
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

 -  Fixed carry.  [[#8](https://example.com/pull/8)]

[#8]: https://example.com/pull/8
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let fragment = crate::fragment::parse_fragment(
            carried.fragments[0].path.clone(),
            &carried.fragments[0].markdown,
            None,
            &repo.config().links,
        )
        .expect("carried fragment");

        assert_eq!(
            fragment.links,
            BTreeMap::from([(
                String::from("#8"),
                String::from("https://example.com/pull/8"),
            )])
        );
        assert_eq!(fragment.items[0].references[0].label, "#8");
    }

    #[test]
    fn preserves_folded_reference_definitions_as_imported_fragment_pins() {
        let (_temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        let region = "\
Unreleased
----------

To be released.

 -  Fixed import.  [[#9](https://example.com/discussions/9)]

[#9]: https://example.com/discussions/9
";

        let imported = import_unreleased_region(&repo, region).expect("import");
        let fragment = crate::fragment::parse_fragment(
            imported.fragments[0].path.clone(),
            &imported.fragments[0].markdown,
            None,
            &repo.config().links,
        )
        .expect("imported fragment");

        assert_eq!(
            fragment.links,
            BTreeMap::from([(
                String::from("#9"),
                String::from("https://example.com/discussions/9"),
            )])
        );
        assert_eq!(fragment.items[0].references[0].label, "#9");
    }

    #[test]
    fn preserves_configured_shortcuts_without_definitions_during_decompilation() {
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

Released today.

 -  Fixed carry.  [[#8]]
";
        let region = "\
Unreleased
----------

To be released.

 -  Fixed import.  [[#8]]
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");

            assert_eq!(
                fragment.links,
                BTreeMap::from([(
                    String::from("#8"),
                    String::from("https://example.com/issues/8"),
                )]),
                "{}",
                output.markdown
            );
            assert_eq!(
                fragment.items[0].references,
                vec![ReferenceUse {
                    label: String::from("#8"),
                    sigil: String::from("#"),
                    number: 8,
                }],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn preserves_configured_full_references_during_decompilation() {
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

 -  Fixed [the issue][#8].

[#8]: https://example.com/pull/8
";
        let region = "\
Unreleased
----------

To be released.

 -  Fixed [the issue][#8].

[#8]: https://example.com/pull/8
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let carried = result.expect("decompiled release");
            let source = &carried.fragments[0].markdown;
            let fragment = crate::fragment::parse_fragment(
                carried.fragments[0].path.clone(),
                source,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");

            assert_eq!(
                fragment.links,
                BTreeMap::from([(
                    String::from("#8"),
                    String::from("https://example.com/pull/8"),
                )])
            );
            assert_eq!(fragment.items[0].references[0].label, "#8");
            assert!(
                fragment.items[0].markdown.contains("[the issue][#8]"),
                "{source}"
            );
        }
    }

    #[test]
    fn preserves_configured_image_destinations_during_decompilation() {
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

Released today.

 -  Added ![screenshot][#1].

[#1]: https://images.example/screenshot.png
";
        let region = "\
Unreleased
----------

To be released.

 -  Added ![screenshot][#1].

[#1]: https://images.example/screenshot.png
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");

            assert_eq!(
                fragment.links.get("#1").map(String::as_str),
                Some("https://images.example/screenshot.png")
            );
            assert_eq!(fragment.items[0].references[0].label, "#1");
            assert!(
                fragment.items[0].markdown.contains("![screenshot][#1]"),
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn does_not_fold_an_ordinary_reference_that_shares_a_configured_destination() {
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

 -  Read the [guide].

[#1]: https://example.com/shared
[guide]: https://example.com/shared
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [guide].

[#1]: https://example.com/shared
[guide]: https://example.com/shared
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let carried = result.expect("decompiled release");
            let markdown = &carried.fragments[0].markdown;
            let fragment = crate::fragment::parse_fragment(
                carried.fragments[0].path.clone(),
                markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");

            assert!(!markdown.contains("[[#1]]"));
            assert!(fragment.links.is_empty());
            assert!(fragment.items[0].references.is_empty());
            assert!(markdown.contains("[guide]: https://example.com/shared"));
        }
    }

    #[test]
    fn normalizes_carried_entries_with_one_reference_label_namespace() {
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

 -  Read the [guide](https://example.com/first).
 -  Read the [guide](https://example.com/second).
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [guide](https://example.com/first).
 -  Read the [guide](https://example.com/second).
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let carried = result.expect("decompiled release");
            assert_eq!(
                rendered_links(&carried.fragments[0].markdown),
                vec![
                    (
                        String::from("guide"),
                        String::from("https://example.com/first"),
                        String::new(),
                    ),
                    (
                        String::from("guide"),
                        String::from("https://example.com/second"),
                        String::new(),
                    ),
                ],
                "{}",
                carried.fragments[0].markdown
            );
        }
    }

    #[test]
    fn does_not_pin_ordinary_links_with_configured_looking_text() {
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

 -  Read [#1][first].
 -  Read [#1][second].

[first]: https://example.com/first
[second]: https://example.com/second
";
        let region = "\
Unreleased
----------

To be released.

 -  Read [#1][first].
 -  Read [#1][second].

[first]: https://example.com/first
[second]: https://example.com/second
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let carried = result.expect("decompiled release");
            let fragment = &carried.fragments[0];
            assert!(!fragment.markdown.starts_with("---\n"));
            assert_eq!(
                rendered_links(&fragment.markdown)
                    .into_iter()
                    .map(|(_, url, _)| url)
                    .collect::<Vec<_>>(),
                vec![
                    String::from("https://example.com/first"),
                    String::from("https://example.com/second"),
                ],
                "{}",
                fragment.markdown
            );
        }
    }

    #[test]
    fn preserves_ordinary_configured_looking_links_through_decompilation() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        for (link, definition, expected_url, expected_title) in [
            (
                "[#1][migration]",
                "\n[migration]: https://docs.example/migration\n",
                "https://docs.example/migration",
                "",
            ),
            (
                "[#1](https://docs.example/guide \"Guide title\")",
                "",
                "https://docs.example/guide",
                "Guide title",
            ),
            (
                "[[#1](https://docs.example/bracketed \"Bracketed guide\")]",
                "",
                "https://docs.example/bracketed",
                "Bracketed guide",
            ),
            (
                "[[#1](https://docs.example/defined \"Defined guide\")]",
                "\n[#1]: https://docs.example/defined \"Defined guide\"\n",
                "https://docs.example/defined",
                "Defined guide",
            ),
        ] {
            let changelog = format!(
                "Version 1.1.5\n-------------\n\nReleased today.\n\n -  Read {link}.\n{definition}"
            );
            let region = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Read {link}.\n{definition}"
            );

            for result in [
                carry_release(&repo, &changelog, "1.1.5"),
                import_unreleased_region(&repo, &region).map(|imported| CarriedRelease {
                    version: imported.version.unwrap_or_default(),
                    fragments: imported.fragments,
                }),
            ] {
                let decompiled = result.expect("decompiled release");
                let output = &decompiled.fragments[0];
                let fragment = crate::fragment::parse_fragment(
                    output.path.clone(),
                    &output.markdown,
                    None,
                    &repo.config().links,
                )
                .expect("decompiled fragment");
                assert!(fragment.links.is_empty(), "{}", output.markdown);
                assert!(
                    fragment.items[0].references.is_empty(),
                    "{}",
                    output.markdown
                );
                let compiled = crate::compile::compile_parsed_fragments(
                    &repo,
                    crate::compile::CompileOptions::default(),
                    crate::compile::VersionLabel::Unreleased,
                    vec![fragment],
                )
                .expect("compiled fragment");

                assert_eq!(
                    rendered_links(&compiled.markdown),
                    vec![(
                        String::from("#1"),
                        String::from(expected_url),
                        String::from(expected_title),
                    )],
                    "{}",
                    output.markdown
                );
            }
        }
    }

    #[test]
    fn preserves_bracketed_inline_links_alongside_configured_references() {
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

Released today.

 -  Read [[#1](https://docs.example/inline)] and fixed [#1].

[#1]: https://example.com/pull/1
";
        let region = "\
Unreleased
----------

To be released.

 -  Read [[#1](https://docs.example/inline)] and fixed [#1].

[#1]: https://example.com/pull/1
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            assert_eq!(
                fragment.links,
                BTreeMap::from([(
                    String::from("#1"),
                    String::from("https://example.com/pull/1"),
                )]),
                "{}",
                output.markdown
            );
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown)
                    .into_iter()
                    .map(|(_, url, _)| url)
                    .collect::<Vec<_>>(),
                vec![
                    String::from("https://docs.example/inline"),
                    String::from("https://example.com/pull/1"),
                ],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn preserves_ordinary_images_nested_in_configured_links() {
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

Released today.

 -  Added [![#2](https://images.example/pic.png \"Pic\")][#1].

[#1]: https://example.com/pull/1
";
        let region = "\
Unreleased
----------

To be released.

 -  Added [![#2](https://images.example/pic.png \"Pic\")][#1].

[#1]: https://example.com/pull/1
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            assert!(
                !output.markdown.contains("sachointernalconfiguredlink"),
                "{}",
                output.markdown
            );
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown),
                vec![(
                    String::from("#2"),
                    String::from("https://example.com/pull/1"),
                    String::new(),
                )],
                "{}",
                output.markdown
            );
            assert_eq!(
                rendered_images(&compiled.markdown),
                vec![(
                    String::from("#2"),
                    String::from("https://images.example/pic.png"),
                    String::from("Pic"),
                )],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn preserves_configured_images_nested_in_configured_links() {
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

Released today.

 -  Added [![screenshot][#1]][#2].

[#1]: https://images.example/screenshot.png
[#2]: https://docs.example/page
";
        let region = "\
Unreleased
----------

To be released.

 -  Added [![screenshot][#1]][#2].

[#1]: https://images.example/screenshot.png
[#2]: https://docs.example/page
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            assert_eq!(
                fragment.links,
                BTreeMap::from([
                    (
                        String::from("#1"),
                        String::from("https://images.example/screenshot.png"),
                    ),
                    (
                        String::from("#2"),
                        String::from("https://docs.example/page"),
                    ),
                ]),
                "{}",
                output.markdown
            );
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown),
                vec![(
                    String::from("screenshot"),
                    String::from("https://docs.example/page"),
                    String::new(),
                )],
                "{}",
                output.markdown
            );
            assert_eq!(
                rendered_images(&compiled.markdown),
                vec![(
                    String::from("screenshot"),
                    String::from("https://images.example/screenshot.png"),
                    String::new(),
                )],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn marker_prefix_avoids_entity_decoded_text_during_decompilation() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        let prefix = "sachointernalconfiguredlink0";
        let source_without_token = "\
Version 1.1.5
-------------

Released today.

 -  Read [#1][migration].

[migration]: https://docs.example/migration
";
        let arena = Arena::new();
        let root = parse_document(&arena, source_without_token, &comrak_options());
        let position = root
            .descendants()
            .find(|node| {
                matches!(node.data().value, NodeValue::Link(_)) && plain_text(node) == "#1"
            })
            .expect("ordinary link")
            .data()
            .sourcepos;
        let marker = format!(
            "{prefix}ordinaryx{}x{}x{}x{}",
            position.start.line, position.start.column, position.end.line, position.end.column,
        );
        let encoded_marker = marker.replacen('s', "&#115;", 1);
        let changelog = source_without_token.replacen(
            "[#1][migration].",
            &format!("[#1][migration] and {encoded_marker}."),
            1,
        );
        let region = changelog.replacen(
            "Version 1.1.5\n-------------\n\nReleased today.",
            "Unreleased\n----------\n\nTo be released.",
            1,
        );

        for result in [
            carry_release(&repo, &changelog, "1.1.5"),
            import_unreleased_region(&repo, &region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            assert!(output.markdown.contains(&marker), "{}", output.markdown);
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            assert!(fragment.links.is_empty(), "{}", output.markdown);
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown),
                vec![(
                    String::from("#1"),
                    String::from("https://docs.example/migration"),
                    String::new(),
                )],
                "{}",
                output.markdown
            );
            assert!(compiled.markdown.contains(&marker), "{}", compiled.markdown);
        }
    }

    #[test]
    fn preserves_control_characters_in_ordinary_destinations_through_decompilation() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        for (entity, character) in [("&#9;", '\t'), ("&#10;", '\n'), ("&#13;", '\r')] {
            let definition =
                format!("[migration]: <https://docs.example/a{entity}b> \"Control character\"\n");
            let changelog = format!(
                "Version 1.1.5\n-------------\n\nReleased today.\n\n -  Read [#1][migration].\n\n{definition}"
            );
            let region = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Read [#1][migration].\n\n{definition}"
            );
            let expected_url = format!("https://docs.example/a{character}b");

            for result in [
                carry_release(&repo, &changelog, "1.1.5"),
                import_unreleased_region(&repo, &region).map(|imported| CarriedRelease {
                    version: imported.version.unwrap_or_default(),
                    fragments: imported.fragments,
                }),
            ] {
                let decompiled = result.expect("decompiled release");
                let output = &decompiled.fragments[0];
                let fragment = crate::fragment::parse_fragment(
                    output.path.clone(),
                    &output.markdown,
                    None,
                    &repo.config().links,
                )
                .expect("decompiled fragment");
                let fragment_markdown = fragment.items[0].markdown.clone();
                let compiled = crate::compile::compile_parsed_fragments(
                    &repo,
                    crate::compile::CompileOptions::default(),
                    crate::compile::VersionLabel::Unreleased,
                    vec![fragment],
                )
                .expect("compiled fragment");

                assert_eq!(
                    rendered_links(&compiled.markdown),
                    vec![(
                        String::from("#1"),
                        expected_url.clone(),
                        String::from("Control character"),
                    )],
                    "decompiled:\n{}\nfragment item:\n{}\ncompiled:\n{}",
                    output.markdown,
                    fragment_markdown,
                    compiled.markdown,
                );
            }
        }
    }

    #[test]
    fn preserves_ordinary_configured_looking_images_through_decompilation() {
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

Released today.

 -  Added ![#1][screenshot].

[screenshot]: https://images.example/ordinary.png \"Ordinary image\"
";
        let region = "\
Unreleased
----------

To be released.

 -  Added ![#1][screenshot].

[screenshot]: https://images.example/ordinary.png \"Ordinary image\"
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            assert!(fragment.links.is_empty(), "{}", output.markdown);
            assert!(
                fragment.items[0].references.is_empty(),
                "{}",
                output.markdown
            );
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_images(&compiled.markdown),
                vec![(
                    String::from("#1"),
                    String::from("https://images.example/ordinary.png"),
                    String::from("Ordinary image"),
                )],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn preserves_distinct_bracketed_inline_destinations_across_entries() {
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

Released today.

 -  Fixed the first path.  [[#8](https://example.com/pull/8)]
 -  Fixed the second path.  [[#8](https://example.net/pull/8)]

[#8]: https://example.com/pull/8
[#8]: https://example.net/pull/8
";
        let region = "\
Unreleased
----------

To be released.

 -  Fixed the first path.  [[#8](https://example.com/pull/8)]
 -  Fixed the second path.  [[#8](https://example.net/pull/8)]

[#8]: https://example.com/pull/8
[#8]: https://example.net/pull/8
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown)
                    .into_iter()
                    .map(|(_, url, _)| url)
                    .collect::<Vec<_>>(),
                vec![
                    String::from("https://example.com/pull/8"),
                    String::from("https://example.net/pull/8"),
                ],
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn preserves_distinct_bracketed_inline_destinations_within_one_entry() {
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

Released today.

 -  Fixed both [[#8](https://example.com/pull/8)] and [[#8](https://example.net/pull/8)].

[#8]: https://example.com/pull/8
[#8]: https://example.net/pull/8
";
        let region = "\
Unreleased
----------

To be released.

 -  Fixed both [[#8](https://example.com/pull/8)] and [[#8](https://example.net/pull/8)].

[#8]: https://example.com/pull/8
[#8]: https://example.net/pull/8
";

        for result in [
            carry_release(&repo, changelog, "1.1.5"),
            import_unreleased_region(&repo, region).map(|imported| CarriedRelease {
                version: imported.version.unwrap_or_default(),
                fragments: imported.fragments,
            }),
        ] {
            let decompiled = result.expect("decompiled release");
            let output = &decompiled.fragments[0];
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compiled fragment");

            assert_eq!(
                rendered_links(&compiled.markdown)
                    .into_iter()
                    .map(|(_, url, _)| url)
                    .collect::<Vec<_>>(),
                vec![
                    String::from("https://example.com/pull/8"),
                    String::from("https://example.net/pull/8"),
                ],
                "{}",
                output.markdown
            );
        }
    }

    proptest! {
        #[test]
        fn decompiled_reference_pins_preserve_generated_destinations(
            number in 1_u64..=u32::MAX.into(),
            destination in prop_oneof![Just("pull"), Just("discussions")],
        ) {
            let (_temp, repo) = repo_with_config(
                r##"
                [changelog]
                materialize = false

                [links]
                "#" = "https://example.com/issues/{n}"
                "##,
            );
            let label = format!("#{number}");
            let url = format!("https://example.com/{destination}/{number}");
            let changelog = format!(
                "Version 1.1.5\n-------------\n\nReleased today.\n\n -  Fixed carry.  [[{label}]({url})]\n\n[{label}]: {url}\n"
            );
            let region = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Fixed import.  [[{label}]({url})]\n\n[{label}]: {url}\n"
            );

            let carried = carry_release(&repo, &changelog, "1.1.5").expect("carry");
            let imported = import_unreleased_region(&repo, &region).expect("import");

            for output in [&carried.fragments[0], &imported.fragments[0]] {
                let fragment = crate::fragment::parse_fragment(
                    output.path.clone(),
                    &output.markdown,
                    None,
                    &repo.config().links,
                )
                .expect("decompiled fragment");
                prop_assert_eq!(fragment.links.get(&label), Some(&url));
            }
        }

        #[test]
        fn ordinary_reference_styles_survive_decompile_and_compile(
            label in "[a-z]{1,12}",
            style in 0_u8..3,
            multiline in any::<bool>(),
        ) {
            let (_temp, repo) = repo_with_config(
                r#"
                [changelog]
                materialize = false
                "#,
            );
            let reference = match style {
                0 => format!("[migration guide][{label}]"),
                1 => format!("[{label}][]"),
                _ => format!("[{label}]"),
            };
            let url = format!("https://example.com/{label}");
            let definition = if multiline {
                format!("[{label}]:\n  <{url}>\n  \"Reference title\"")
            } else {
                format!("[{label}]: {url}")
            };
            let changelog = format!(
                "Version 1.1.5\n-------------\n\nReleased today.\n\n -  Read {reference}.\n\n{definition}\n"
            );
            let region = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Read {reference}.\n\n{definition}\n"
            );

            let carried = carry_release(&repo, &changelog, "1.1.5").expect("carry");
            let imported = import_unreleased_region(&repo, &region).expect("import");

            for output in [&carried.fragments[0], &imported.fragments[0]] {
                prop_assert!(output.markdown.contains(&url));
                let fragment = crate::fragment::parse_fragment(
                    output.path.clone(),
                    &output.markdown,
                    None,
                    &repo.config().links,
                )
                .expect("decompiled fragment");
                let compiled = crate::compile::compile_parsed_fragments(
                    &repo,
                    crate::compile::CompileOptions::default(),
                    crate::compile::VersionLabel::Unreleased,
                    vec![fragment],
                )
                .expect("compiled fragment");
                prop_assert!(compiled.markdown.contains(&url));
                if multiline {
                    prop_assert!(compiled.markdown.contains("Reference title"));
                }
            }
        }

        #[test]
        fn decompiled_non_http_references_remain_unpinned(
            number in 1_u64..=u32::MAX.into(),
            relative in any::<bool>(),
        ) {
            let template = if relative {
                "/issues/{n}"
            } else {
                "mailto:issue-{n}@example.com"
            };
            let config = format!(
                "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{template}\"\n"
            );
            let (_temp, repo) = repo_with_config(&config);
            let label = format!("#{number}");
            let url = template.replace("{n}", &number.to_string());
            let changelog = format!(
                "Version 1.1.5\n-------------\n\nReleased today.\n\n -  Fixed carry.  [[{label}]({url})]\n\n[{label}]: {url}\n"
            );
            let region = format!(
                "Unreleased\n----------\n\nTo be released.\n\n -  Fixed import.  [[{label}]({url})]\n\n[{label}]: {url}\n"
            );

            let carried = carry_release(&repo, &changelog, "1.1.5").expect("carry");
            let imported = import_unreleased_region(&repo, &region).expect("import");

            for output in [&carried.fragments[0], &imported.fragments[0]] {
                let fragment = crate::fragment::parse_fragment(
                    output.path.clone(),
                    &output.markdown,
                    None,
                    &repo.config().links,
                )
                .expect("decompiled fragment");
                prop_assert!(fragment.links.is_empty());
                prop_assert_eq!(&fragment.items[0].references[0].label, &label);
                let compiled = crate::compile::compile_parsed_fragments(
                    &repo,
                    crate::compile::CompileOptions::default(),
                    crate::compile::VersionLabel::Unreleased,
                    vec![fragment],
                )
                .expect("compiled fragment");
                let definition = format!("[{label}]: {url}");
                prop_assert!(compiled.markdown.contains(&definition));
            }
        }
    }

    #[test]
    fn does_not_infer_a_configured_label_from_an_ordinary_link_destination() {
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
                .contains("https://example.com/issues/8")
        );
        assert!(!carried.fragments[0].markdown.contains("[[#8]]"));
    }

    #[test]
    fn leaves_ordinary_markdown_reference_definitions_out_of_fragment_pins() {
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

 -  Read the [documentation](https://example.com/docs).

[docs]: https://example.com/docs
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [documentation](https://example.com/docs).

[docs]: https://example.com/docs
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for output in [&carried.fragments[0], &imported.fragments[0]] {
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("ordinary references must remain valid Markdown");
            assert!(fragment.links.is_empty());
            assert_eq!(
                fragment.items[0].markdown,
                "- Read the [documentation](https://example.com/docs)."
            );
        }
    }

    #[test]
    fn preserves_ordinary_reference_style_links_during_decompilation() {
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

 -  Read the [migration guide][guide].

[guide]: https://example.com/migration
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [migration guide][guide].

[guide]: https://example.com/migration
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for output in [&carried.fragments[0], &imported.fragments[0]] {
            assert_eq!(
                output.markdown,
                " -  Read the [migration guide].\n\n[migration guide]: https://example.com/migration\n"
            );
            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compile decompiled fragment");

            assert!(
                compiled
                    .markdown
                    .contains(" -  Read the [migration guide].")
            );
            assert!(
                compiled
                    .markdown
                    .contains("[migration guide]: https://example.com/migration")
            );
        }
    }

    #[test]
    fn preserves_multiline_reference_definitions_during_decompilation() {
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

 -  Read the [migration guide][guide].

[guide]:
  <https://example.com/migration>
  \"Migration guide\"
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [migration guide][guide].

[guide]:
  <https://example.com/migration>
  \"Migration guide\"
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for output in [&carried.fragments[0], &imported.fragments[0]] {
            assert!(
                output.markdown.contains(
                    "[migration guide]: https://example.com/migration \"Migration guide\""
                ),
                "{}",
                output.markdown
            );
        }
    }

    #[test]
    fn carry_preserves_definition_from_another_release() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false
            "#,
        );
        let changelog = "\
Version 2.0.0
-------------

Released on July 8, 2026.

 -  Read the [migration guide][guide].

Version 1.0.0
-------------

Released on July 1, 2026.

[guide]: https://example.com/migration
";

        let carried = carry_release(&repo, changelog, "2.0.0").expect("carry");

        assert!(
            carried.fragments[0]
                .markdown
                .contains("[migration guide]: https://example.com/migration"),
            "{}",
            carried.fragments[0].markdown
        );
    }

    #[test]
    fn preserves_document_first_definition_over_nested_duplicate() {
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

[guide]: https://example.com/first

 -  Read the [migration guide][guide].

    [guide]: https://example.com/second
";
        let region = "\
Unreleased
----------

To be released.

[guide]: https://example.com/first

 -  Read the [migration guide][guide].

    [guide]: https://example.com/second
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for output in [&carried.fragments[0], &imported.fragments[0]] {
            assert!(
                output
                    .markdown
                    .contains("[migration guide]: https://example.com/first"),
                "{}",
                output.markdown
            );
            assert!(!output.markdown.contains("https://example.com/second"));

            let fragment = crate::fragment::parse_fragment(
                output.path.clone(),
                &output.markdown,
                None,
                &repo.config().links,
            )
            .expect("decompiled fragment");
            let compiled = crate::compile::compile_parsed_fragments(
                &repo,
                crate::compile::CompileOptions::default(),
                crate::compile::VersionLabel::Unreleased,
                vec![fragment],
            )
            .expect("compile decompiled fragment");

            assert!(
                compiled
                    .markdown
                    .contains("[migration guide]: https://example.com/first")
            );
            assert!(!compiled.markdown.contains("https://example.com/second"));
        }
    }

    #[test]
    fn preserves_commonmark_first_reference_definition_during_decompilation() {
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

 -  Read the [migration guide][FOO].

[foo]: https://example.com/first
[FOO]: https://example.com/second
";
        let region = "\
Unreleased
----------

To be released.

 -  Read the [migration guide][FOO].

[foo]: https://example.com/first
[FOO]: https://example.com/second
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for output in [&carried.fragments[0], &imported.fragments[0]] {
            assert!(
                output
                    .markdown
                    .contains("[migration guide]: https://example.com/first")
            );
            assert!(!output.markdown.contains("https://example.com/second"));
        }
    }

    #[test]
    fn preserves_reference_definition_only_in_the_section_that_uses_it() {
        let (_temp, repo) = repo_with_config(
            r#"
            [changelog]
            materialize = false

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

### core

 -  Read the [migration guide][guide].

### cli

 -  Added a command.

[guide]: https://example.com/migration
";
        let region = "\
Unreleased
----------

To be released.

### core

 -  Read the [migration guide][guide].

### cli

 -  Added a command.

[guide]: https://example.com/migration
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let imported = import_unreleased_region(&repo, region).expect("import");

        for fragments in [&carried.fragments, &imported.fragments] {
            let core = fragments
                .iter()
                .find(|fragment| fragment.path.starts_with("changes.d/core"))
                .expect("core fragment");
            let cli = fragments
                .iter()
                .find(|fragment| fragment.path.starts_with("changes.d/cli"))
                .expect("cli fragment");
            assert!(
                core.markdown
                    .contains("[migration guide]: https://example.com/migration")
            );
            assert!(!cli.markdown.contains("[migration guide]:"));
        }
    }

    #[test]
    fn does_not_infer_an_unpinned_label_from_a_relative_link_destination() {
        let (_temp, repo) = repo_with_config(
            r##"
            [changelog]
            materialize = false

            [links]
            "#" = "/issues/{n}"
            "##,
        );
        let changelog = "\
Version 1.1.5
-------------

Released on July 7, 2026.

 -  Fixed carry.  [issue](/issues/8)

[#8]: /issues/8
";

        let carried = carry_release(&repo, changelog, "1.1.5").expect("carry");
        let fragment = crate::fragment::parse_fragment(
            carried.fragments[0].path.clone(),
            &carried.fragments[0].markdown,
            None,
            &repo.config().links,
        )
        .expect("carried fragment");

        assert!(fragment.links.is_empty());
        assert!(fragment.items[0].references.is_empty());
        assert!(
            carried.fragments[0]
                .markdown
                .contains(" -  Fixed carry.  [issue](/issues/8)\n")
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
