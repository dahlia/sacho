use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use comrak::nodes::{AstNode, ListType, NodeValue, Sourcepos};
use comrak::options::BrokenLinkReference;
use comrak::{
    Arena, Options as ComrakOptions, ResolvedReference, format_commonmark, parse_document,
};
use indexmap::IndexMap;
use serde::Deserialize;
use snafu::ResultExt;

use crate::config::{ReferenceSigil, UrlTemplate};
use crate::error::{
    FragmentError, FrontmatterSnafu, ReadFileSnafu, Result, UnclosedFrontmatterSnafu,
    UnknownReferenceSnafu, redact_url_credentials,
};
use crate::link_resolution::validate_http_url;
use crate::markdown::escape_angle_bracket_destination;
use crate::repo::Repository;
use crate::section::{ResolvedSection, SectionResolutionError, SectionResolver, has_sections};

/// Parsed changelog fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Path to the fragment file.
    pub path: PathBuf,

    /// Section identifier inferred from the fragment directory.
    pub section: Option<String>,

    /// Sort priority read from frontmatter.
    pub priority: i32,

    /// Resolved reference URLs pinned in frontmatter.
    pub links: BTreeMap<String, String>,

    /// Items contributed by the fragment.
    pub items: Vec<FragmentItem>,

    /// Non-fatal warnings found while parsing the fragment.
    pub warnings: Vec<FragmentWarning>,
}

/// A single changelog item inside a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentItem {
    /// Zero-based item position inside the fragment.
    pub ordinal: usize,

    /// Original Markdown for the item.
    pub markdown: String,

    /// Plain-text key used for deterministic sorting.
    pub sort_text: String,

    /// Whether the item contains intentionally authored CommonMark content.
    pub has_substantive_content: bool,

    /// Reference shortcut labels found in the item.
    pub references: Vec<ReferenceUse>,
}

/// Parsed YAML frontmatter for a fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// Sort priority for all items in the fragment.
    pub priority: i32,

    /// Resolved reference URLs keyed by complete reference label.
    pub links: BTreeMap<String, String>,

    /// Frontmatter keys Sacho does not understand.
    pub unknown_keys: Vec<String>,
}

/// Non-fatal warning found while parsing a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentWarning {
    /// A frontmatter key was ignored because Sacho does not define it.
    UnknownFrontmatterKey {
        /// Unknown key name.
        key: String,
    },
}

/// A reference shortcut use found in a fragment item.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReferenceUse {
    /// Full shortcut label, such as `#842`.
    pub label: String,

    /// Configured sigil that resolved the label.
    pub sigil: String,

    /// Numeric reference target following the sigil.
    pub number: u64,
}

/// Result of deterministic fragment discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFragments {
    /// Parsed fragments in deterministic path order.
    pub fragments: Vec<Fragment>,

    /// Non-fatal discovery warnings.
    pub warnings: Vec<DiscoveryWarning>,
}

/// Deterministically discovered fragment path before parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentCandidate {
    /// Absolute path to the fragment file.
    pub path: PathBuf,

    /// Repository-relative path to the fragment file.
    pub relative_path: PathBuf,

    /// Section identifier inferred from the fragment directory.
    pub section: Option<String>,
}

/// Result of deterministic fragment path discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFragmentCandidates {
    /// Fragment paths in deterministic order.
    pub candidates: Vec<FragmentCandidate>,

    /// Non-fatal discovery warnings.
    pub warnings: Vec<DiscoveryWarning>,
}

/// Non-fatal warning found while discovering fragments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryWarning {
    /// A fragment was found under a directory absent from section config.
    UnknownSectionFragment {
        /// Repository-relative fragment path.
        path: PathBuf,

        /// Directory name treated as the unknown section identifier.
        section: String,
    },
}

#[derive(Debug, Deserialize)]
struct RawFrontmatter {
    priority: Option<i32>,
    #[serde(default)]
    links: BTreeMap<String, String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml_ng::Value>,
}

/// Discovers and parses all fragment files for a repository.
pub fn discover_fragments(repo: &Repository) -> Result<DiscoveredFragments> {
    let discovered = discover_fragment_candidates(repo)?;
    let config = repo.config();
    let mut fragments = Vec::with_capacity(discovered.candidates.len());
    for candidate in discovered.candidates {
        let source = fs::read_to_string(&candidate.path).context(ReadFileSnafu {
            path: candidate.path.clone(),
        })?;
        let fragment = parse_fragment(
            candidate.relative_path.clone(),
            &source,
            candidate.section,
            &config.links,
        )
        .map_err(|source| crate::Error::Fragment {
            path: candidate.relative_path,
            source,
        })?;
        fragments.push(fragment);
    }

    Ok(DiscoveredFragments {
        fragments,
        warnings: discovered.warnings,
    })
}

/// Discovers fragment file paths without parsing their contents.
pub fn discover_fragment_candidates(repo: &Repository) -> Result<DiscoveredFragmentCandidates> {
    repo.revalidate_paths()?;
    let config = repo.config();
    let fragment_dir = repo.resolve(&config.fragments.directory);
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();

    if !has_sections(config) {
        collect_markdown_files(&fragment_dir, None, &mut candidates)?;
    } else {
        let resolver = SectionResolver::from_config(config)?;
        for section in &config.sections {
            collect_markdown_files(
                &fragment_dir.join(&section.directory),
                Some(section.id.clone()),
                &mut candidates,
            )?;
        }
        let max_pattern_depth = config
            .section_patterns
            .iter()
            .map(|pattern| pattern.directory.segments().len())
            .max()
            .unwrap_or(0);
        collect_pattern_markdown_files(
            repo,
            &fragment_dir,
            &fragment_dir,
            Path::new(""),
            max_pattern_depth,
            &resolver,
            &mut candidates,
        )?;

        let entries = match fs::read_dir(&fragment_dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(source) => {
                return Err(crate::Error::ReadFile {
                    path: fragment_dir,
                    source,
                });
            }
        };
        for entry in entries.into_iter().flatten() {
            let entry = entry.context(ReadFileSnafu {
                path: fragment_dir.clone(),
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let relative = path
                .strip_prefix(&fragment_dir)
                .expect("fragment entry remains below fragment directory");
            if resolve_discovered_directory(&resolver, relative)?.is_some()
                || is_configured_section_dir(repo, &resolver, &path)?
            {
                continue;
            }
            let Some(section) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let before = candidates.len();
            collect_markdown_files(&path, Some(section.clone()), &mut candidates)?;
            for (fragment_path, _) in &candidates[before..] {
                warnings.push(DiscoveryWarning::UnknownSectionFragment {
                    path: repo_relative_path(repo, fragment_path),
                    section: section.clone(),
                });
            }
        }
    }

    candidates.sort_by(|(left, _), (right, _)| compare_fragment_paths(left, right));

    let candidates = candidates
        .into_iter()
        .map(|(path, section)| FragmentCandidate {
            relative_path: repo_relative_path(repo, &path),
            path,
            section,
        })
        .collect();

    Ok(DiscoveredFragmentCandidates {
        candidates,
        warnings,
    })
}

fn collect_pattern_markdown_files(
    repo: &Repository,
    fragment_dir: &Path,
    directory: &Path,
    relative: &Path,
    remaining_depth: usize,
    resolver: &SectionResolver<'_>,
    candidates: &mut Vec<(PathBuf, Option<String>)>,
) -> Result<()> {
    if !relative.as_os_str().is_empty()
        && let Some(section) = resolve_discovered_directory(resolver, relative)?
        && section.pattern_index.is_some()
    {
        repo.validate_pattern_section_directory(relative)?;
        collect_markdown_files(directory, Some(section.id), candidates)?;
    }
    if remaining_depth == 0 {
        return Ok(());
    }

    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error)
            if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
                || fs::symlink_metadata(directory)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink()) =>
        {
            return Ok(());
        }
        Err(source) => {
            return Err(crate::Error::ReadFile {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.context(ReadFileSnafu {
            path: directory.to_path_buf(),
        })?;
        let file_type = entry
            .file_type()
            .context(ReadFileSnafu { path: entry.path() })?;
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        let child_relative = relative.join(entry.file_name());
        if !resolver
            .directory_prefix_is_viable(&child_relative)
            .map_err(|error| crate::Error::SectionPattern {
                message: error.to_string(),
            })?
        {
            continue;
        }
        let child_depth = remaining_depth
            .checked_sub(1)
            .expect("zero remaining depth returned before recursion");
        collect_pattern_markdown_files(
            repo,
            fragment_dir,
            &fragment_dir.join(&child_relative),
            &child_relative,
            child_depth,
            resolver,
            candidates,
        )?;
    }
    Ok(())
}

fn resolve_discovered_directory(
    resolver: &SectionResolver<'_>,
    relative: &Path,
) -> Result<Option<ResolvedSection>> {
    match resolver.resolve_directory(relative) {
        Ok(section) => Ok(section),
        Err(SectionResolutionError::NonUtf8Path { .. }) => Ok(None),
        Err(error) => Err(crate::Error::SectionPattern {
            message: error.to_string(),
        }),
    }
}

pub(crate) fn compare_fragment_paths(left: &Path, right: &Path) -> Ordering {
    left.to_string_lossy().cmp(&right.to_string_lossy())
}

/// Parses one fragment from source text.
pub fn parse_fragment(
    path: PathBuf,
    source: &str,
    section: Option<String>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> std::result::Result<Fragment, FragmentError> {
    let ParsedSource {
        frontmatter,
        body,
        body_line_offset,
    } = split_frontmatter(source)?;
    let warnings = frontmatter
        .unknown_keys
        .iter()
        .cloned()
        .map(|key| FragmentWarning::UnknownFrontmatterKey { key })
        .collect::<Vec<_>>();

    let options = comrak_options_with_configured_references(&frontmatter.links, link_templates);
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options);
    let original_list = validate_fragment_shape(root, body_line_offset)?;
    let configured_links = configured_link_references(body, original_list, link_templates);
    let configured_references = configured_links
        .values()
        .map(|reference| (reference.label.clone(), reference.clone()))
        .collect::<BTreeMap<_, _>>();
    let authoritative_body = authoritative_reference_source(
        body,
        configured_references.values(),
        &frontmatter.links,
        link_templates,
    );
    let (list, configured_links) = if let Some(body) = authoritative_body.as_deref() {
        let root = parse_document(&arena, body, &options);
        (
            root.first_child()
                .expect("authoritative definitions leave the fragment list intact"),
            shift_configured_links(configured_links, configured_references.len() + 1),
        )
    } else {
        (original_list, configured_links)
    };
    let mut items = Vec::new();
    for (ordinal, item) in list.children().enumerate() {
        let markdown = render_item_with_configured_shortcuts(item, &options, &configured_links);
        let first_block = item.first_child();
        let sort_text = first_block
            .map(plain_text)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let substantive_content = has_substantive_content(item);
        let mut references = Vec::new();
        collect_references(item, &configured_links, link_templates, &mut references)?;
        references.sort();
        references.dedup();
        items.push(FragmentItem {
            ordinal,
            markdown,
            sort_text,
            has_substantive_content: substantive_content,
            references,
        });
    }
    validate_resolved_link_labels(&frontmatter.links, &items, link_templates)?;

    Ok(Fragment {
        path,
        section,
        priority: frontmatter.priority,
        links: frontmatter.links,
        items,
        warnings,
    })
}

fn validate_resolved_link_labels(
    links: &BTreeMap<String, String>,
    items: &[FragmentItem],
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> std::result::Result<(), FragmentError> {
    for label in links.keys() {
        if !is_complete_reference_label(label, link_templates) {
            return Err(FragmentError::InvalidResolvedLinkLabel {
                label: label.clone(),
                reason: String::from(
                    "must be a configured link sigil followed by a reference number",
                ),
            });
        }
        let is_used = items
            .iter()
            .flat_map(|item| &item.references)
            .any(|reference| reference.label == *label);
        if !is_used {
            return Err(FragmentError::InvalidResolvedLinkLabel {
                label: label.clone(),
                reason: String::from("label is not used by this fragment"),
            });
        }
    }
    Ok(())
}

pub(crate) fn is_complete_reference_label(
    label: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> bool {
    matches!(
        parse_reference_label(label, link_templates),
        Ok(Some(reference)) if reference.label == label
    )
}

fn collect_markdown_files(
    dir: &Path,
    section: Option<String>,
    candidates: &mut Vec<(PathBuf, Option<String>)>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(crate::Error::ReadFile {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.context(ReadFileSnafu {
            path: dir.to_path_buf(),
        })?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "md") {
            candidates.push((path, section.clone()));
        }
    }
    Ok(())
}

fn is_configured_section_dir(
    repo: &Repository,
    resolver: &SectionResolver<'_>,
    path: &Path,
) -> Result<bool> {
    let Ok(identity) = fs::canonicalize(path) else {
        return Ok(false);
    };
    let fragment_dir = repo.resolve(&repo.config().fragments.directory);
    if repo
        .config()
        .sections
        .iter()
        .filter_map(|section| fs::canonicalize(fragment_dir.join(&section.directory)).ok())
        .any(|configured| configured == identity)
    {
        return Ok(true);
    }
    let Ok(fragment_identity) = fs::canonicalize(fragment_dir) else {
        return Ok(false);
    };
    let Ok(relative) = identity.strip_prefix(fragment_identity) else {
        return Ok(false);
    };
    Ok(resolve_discovered_directory(resolver, relative)?
        .is_some_and(|section| section.pattern_index.is_some()))
}

fn repo_relative_path(repo: &Repository, path: &Path) -> PathBuf {
    path.strip_prefix(repo.root()).unwrap_or(path).to_path_buf()
}

struct ParsedSource<'a> {
    frontmatter: Frontmatter,
    body: &'a str,
    body_line_offset: usize,
}

fn split_frontmatter(source: &str) -> std::result::Result<ParsedSource<'_>, FragmentError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some(first_line_end) = source.find('\n') else {
        if source.strip_suffix('\r').unwrap_or(source) == "---" {
            return UnclosedFrontmatterSnafu.fail();
        }
        return Ok(ParsedSource {
            frontmatter: Frontmatter::default(),
            body: source,
            body_line_offset: 0,
        });
    };
    let first_line = source[..first_line_end]
        .strip_suffix('\r')
        .unwrap_or(&source[..first_line_end]);
    if first_line != "---" {
        return Ok(ParsedSource {
            frontmatter: Frontmatter::default(),
            body: source,
            body_line_offset: 0,
        });
    }

    let mut offset = first_line_end + 1;
    for (body_line_offset, line) in (1..).zip(source[offset..].split_inclusive('\n')) {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let delimiter = line_without_newline
            .strip_suffix('\r')
            .unwrap_or(line_without_newline);
        if delimiter == "---" {
            let yaml = &source[first_line_end..offset];
            let yaml = yaml
                .strip_prefix("\r\n")
                .or_else(|| yaml.strip_prefix('\n'))
                .expect("frontmatter delimiter line ended at first_line_end");
            let body = &source[offset + line.len()..];
            return Ok(ParsedSource {
                frontmatter: Frontmatter::parse(yaml)?,
                body,
                body_line_offset: body_line_offset + 1,
            });
        }
        offset += line.len();
    }

    UnclosedFrontmatterSnafu.fail()
}

impl Frontmatter {
    fn parse(yaml: &str) -> std::result::Result<Self, FragmentError> {
        if yaml.trim().is_empty() {
            return Ok(Self::default());
        }
        let raw: RawFrontmatter = serde_yaml_ng::from_str(yaml).context(FrontmatterSnafu)?;
        for (label, value) in &raw.links {
            validate_resolved_link(label, value)?;
        }
        Ok(Self {
            priority: raw.priority.unwrap_or_default(),
            links: raw.links,
            unknown_keys: raw.extra.into_keys().collect(),
        })
    }
}

pub(crate) fn validate_resolved_link(
    label: &str,
    value: &str,
) -> std::result::Result<(), FragmentError> {
    validate_http_url(value)
        .map(drop)
        .map_err(|reason| FragmentError::InvalidResolvedLink {
            label: label.to_owned(),
            url: redact_url_credentials(value),
            reason,
        })
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

pub(crate) fn comrak_options_with_configured_references(
    links: &BTreeMap<String, String>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> ComrakOptions<'static> {
    let links = links.clone();
    let link_templates = link_templates.clone();
    let mut options = comrak_options();
    options.parse.broken_link_callback =
        Some(Arc::new(move |reference: BrokenLinkReference<'_>| {
            let configured = parse_reference_label(reference.original, &link_templates).ok()??;
            Some(ResolvedReference {
                url: configured_reference_url(&configured, &links, &link_templates),
                title: String::new(),
            })
        }));
    options
}

fn configured_reference_marker_prefix(source: &str) -> String {
    (0_u64..)
        .map(|nonce| format!("https://sacho.invalid/configured-reference/{nonce}/"))
        .find(|prefix| !source.contains(prefix))
        .expect("an absent configured-reference marker prefix")
}

fn configured_reference_url(
    reference: &ReferenceUse,
    links: &BTreeMap<String, String>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> String {
    links.get(&reference.label).cloned().unwrap_or_else(|| {
        link_templates
            .iter()
            .find(|(sigil, _)| sigil.as_str() == reference.sigil)
            .map(|(_, template)| {
                template
                    .as_str()
                    .replace("{n}", &reference.number.to_string())
            })
            .expect("a parsed configured reference has a matching template")
    })
}

pub(crate) fn configured_link_references<'a>(
    source: &str,
    root: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> BTreeMap<Sourcepos, ReferenceUse> {
    configured_link_references_impl(source, root, link_templates, false)
}

pub(crate) fn configured_link_references_with_bracket_wrapped<'a>(
    source: &str,
    root: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> BTreeMap<Sourcepos, ReferenceUse> {
    configured_link_references_impl(source, root, link_templates, true)
}

fn configured_link_references_impl<'a>(
    source: &str,
    root: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    include_bracket_wrapped: bool,
) -> BTreeMap<Sourcepos, ReferenceUse> {
    let line_starts = line_starts(source);
    let bracket_wrapped_targets = if include_bracket_wrapped {
        bracket_wrapped_reference_targets(source, root, link_templates)
    } else {
        BTreeMap::new()
    };
    root.descendants()
        .filter(|node| matches!(node.data().value, NodeValue::Link(_) | NodeValue::Image(_)))
        .filter_map(|node| {
            let position = node.data().sourcepos;
            let link_source = slice_sourcepos(source, &line_starts, position)?;
            let reference = configured_reference_from_link_source(link_source, link_templates)
                .or_else(|| {
                    let reference = include_bracket_wrapped
                        .then(|| bracket_wrapped_reference(node, link_templates))
                        .flatten()?;
                    let data = node.data();
                    let link = match &data.value {
                        NodeValue::Link(link) | NodeValue::Image(link) => link,
                        _ => return None,
                    };
                    bracket_wrapped_targets
                        .get(&reference.label)
                        .is_some_and(|(url, title)| {
                            link.url == *url && link.title.is_empty() && title.is_empty()
                        })
                        .then_some(reference)
                })?;
            Some((position, reference))
        })
        .collect()
}

fn bracket_wrapped_reference_targets<'a>(
    source: &str,
    root: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> BTreeMap<String, (String, String)> {
    let candidates = root
        .descendants()
        .filter_map(|node| bracket_wrapped_reference(node, link_templates))
        .map(|reference| (reference.label.clone(), reference))
        .collect::<BTreeMap<_, _>>();
    if candidates.is_empty() {
        return BTreeMap::new();
    }

    let marker_prefix = configured_link_marker_prefix(source);
    let mut probes = String::new();
    let mut labels_by_marker = BTreeMap::new();
    for (index, label) in candidates.keys().enumerate() {
        let marker = format!("{marker_prefix}referenceprobe{index}x");
        probes.push_str(&format!("[{marker}][{label}] "));
        labels_by_marker.insert(marker, label.clone());
    }
    probes.push_str("\n\n");
    probes.push_str(source);

    let arena = Arena::new();
    let root = parse_document(&arena, &probes, &comrak_options());
    root.descendants()
        .filter_map(|node| {
            let data = node.data();
            let (NodeValue::Link(link) | NodeValue::Image(link)) = &data.value else {
                return None;
            };
            let label = labels_by_marker.get(&plain_text(node))?;
            Some((label.clone(), (link.url.clone(), link.title.clone())))
        })
        .collect()
}

fn bracket_wrapped_reference<'a>(
    node: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> Option<ReferenceUse> {
    let previous_is_open = node.previous_sibling().is_some_and(
        |sibling| matches!(&sibling.data().value, NodeValue::Text(text) if text.ends_with('[')),
    );
    let next_is_close = node.next_sibling().is_some_and(
        |sibling| matches!(&sibling.data().value, NodeValue::Text(text) if text.starts_with(']')),
    );
    if !previous_is_open || !next_is_close {
        return None;
    }
    let label = plain_text(node);
    parse_reference_label(label.trim(), link_templates)
        .ok()
        .flatten()
        .filter(|reference| reference.label == label.trim())
}

fn configured_reference_from_link_source(
    source: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> Option<ReferenceUse> {
    let marker_prefix = configured_reference_marker_prefix(source);
    let captured = Arc::new(Mutex::new(BTreeMap::<String, ReferenceUse>::new()));
    let callback_captured = Arc::clone(&captured);
    let callback_templates = link_templates.clone();
    let callback_prefix = marker_prefix.clone();
    let mut options = comrak_options();
    options.parse.broken_link_callback =
        Some(Arc::new(move |reference: BrokenLinkReference<'_>| {
            let configured =
                parse_reference_label(reference.original, &callback_templates).ok()??;
            let mut captured = callback_captured
                .lock()
                .expect("configured-reference capture lock");
            let marker = format!("{callback_prefix}{}", captured.len());
            captured.insert(marker.clone(), configured);
            Some(ResolvedReference {
                url: marker,
                title: String::new(),
            })
        }));

    let arena = Arena::new();
    let root = parse_document(&arena, source, &options);
    let paragraph = root.first_child()?;
    let node = paragraph.first_child()?;
    if node.next_sibling().is_some() {
        return None;
    }
    let marker = match &node.data().value {
        NodeValue::Link(link) | NodeValue::Image(link) if link.url.starts_with(&marker_prefix) => {
            link.url.clone()
        }
        _ => return None,
    };
    captured
        .lock()
        .expect("configured-reference capture lock")
        .get(&marker)
        .cloned()
}

fn shift_configured_links(
    links: BTreeMap<Sourcepos, ReferenceUse>,
    line_delta: usize,
) -> BTreeMap<Sourcepos, ReferenceUse> {
    links
        .into_iter()
        .filter_map(|(position, reference)| {
            shift_sourceposition(position, line_delta as isize)
                .map(|position| (position, reference))
        })
        .collect()
}

fn shift_sourceposition(position: Sourcepos, line_delta: isize) -> Option<Sourcepos> {
    Some(Sourcepos {
        start: comrak::nodes::LineColumn {
            line: position.start.line.checked_add_signed(line_delta)?,
            column: position.start.column,
        },
        end: comrak::nodes::LineColumn {
            line: position.end.line.checked_add_signed(line_delta)?,
            column: position.end.column,
        },
    })
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

fn slice_sourcepos<'a>(
    source: &'a str,
    line_starts: &[usize],
    sourcepos: Sourcepos,
) -> Option<&'a str> {
    let start_line = sourcepos.start.line.checked_sub(1)?;
    let end_line = sourcepos.end.line.checked_sub(1)?;
    let start = *line_starts.get(start_line)? + sourcepos.start.column.checked_sub(1)?;
    let end_line_start = *line_starts.get(end_line)?;
    let end = (end_line_start + sourcepos.end.column).min(source.len());
    source.get(start..end)
}

fn authoritative_reference_source<'a>(
    body: &str,
    references: impl Iterator<Item = &'a ReferenceUse>,
    links: &BTreeMap<String, String>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> Option<String> {
    let references = references.collect::<Vec<_>>();
    if references.is_empty() {
        None
    } else {
        Some(reference_source(
            body,
            references.into_iter(),
            |reference| configured_reference_url(reference, links, link_templates),
        ))
    }
}

fn reference_source<'a, U>(
    body: &str,
    references: impl Iterator<Item = &'a ReferenceUse>,
    url: impl Fn(&ReferenceUse) -> U,
) -> String
where
    U: AsRef<str>,
{
    let mut definitions = String::new();
    for reference in references {
        definitions.push('[');
        definitions.push_str(&reference.label);
        definitions.push_str("]: <");
        definitions.push_str(&escape_angle_bracket_destination(url(reference).as_ref()));
        definitions.push_str(">\n");
    }
    definitions.push('\n');
    definitions.push_str(body);
    definitions
}

pub(crate) fn render_item_with_configured_shortcuts<'a>(
    item: &'a AstNode<'a>,
    options: &ComrakOptions<'_>,
    configured_links: &BTreeMap<Sourcepos, ReferenceUse>,
) -> String {
    let original = render_item_commonmark(item, options);
    let marker_prefix = configured_link_marker_prefix(&original);
    let (markdown, markers) =
        render_item_with_configured_markers(item, options, configured_links, &marker_prefix);
    replace_configured_link_markers(markdown, &markers)
}

pub(crate) fn render_item_commonmark<'a>(
    item: &'a AstNode<'a>,
    options: &ComrakOptions<'_>,
) -> String {
    let affected_links = item
        .descendants()
        .filter_map(|node| {
            let data = node.data();
            let url = match &data.value {
                NodeValue::Link(link) | NodeValue::Image(link) => &link.url,
                _ => return None,
            };
            url.chars()
                .any(|character| matches!(character, '\t' | '\n' | '\r'))
                .then(|| (node, url.clone()))
        })
        .collect::<Vec<_>>();
    let mut markdown = String::new();
    format_commonmark(item, options, &mut markdown)
        .expect("writing CommonMark to a String cannot fail");
    if !affected_links.is_empty() {
        let marker_prefix = configured_link_marker_prefix(&markdown);
        let markers = affected_links
            .iter()
            .enumerate()
            .map(|(index, (node, url))| {
                let marker = format!("{marker_prefix}destination{index}x");
                match &mut node.data_mut().value {
                    NodeValue::Link(link) | NodeValue::Image(link) => {
                        link.url.clone_from(&marker);
                    }
                    _ => unreachable!("only links and images were collected"),
                }
                (node, marker, url)
            })
            .collect::<Vec<_>>();
        markdown.clear();
        format_commonmark(item, options, &mut markdown)
            .expect("writing CommonMark to a String cannot fail");
        for (node, marker, url) in markers {
            match &mut node.data_mut().value {
                NodeValue::Link(link) | NodeValue::Image(link) => {
                    link.url.clone_from(url);
                }
                _ => unreachable!("only links and images were collected"),
            }
            markdown = markdown.replace(
                &marker,
                &format!("<{}>", escape_angle_bracket_destination(url)),
            );
        }
    }
    markdown.trim_end().to_owned()
}

pub(crate) fn render_item_with_configured_markers<'a>(
    item: &'a AstNode<'a>,
    options: &ComrakOptions<'_>,
    configured_links: &BTreeMap<Sourcepos, ReferenceUse>,
    marker_prefix: &str,
) -> (String, Vec<(String, String)>) {
    let configured_nodes = item
        .descendants()
        .filter_map(|node| {
            configured_links
                .get(&node.data().sourcepos)
                .map(|reference| (node, reference.clone()))
        })
        .collect::<Vec<_>>();
    let mut rendered_nodes = Vec::with_capacity(configured_nodes.len());
    for (node, reference) in configured_nodes.into_iter().rev() {
        let data = node.data();
        let is_image = matches!(data.value, NodeValue::Image(_));
        if !is_image && !matches!(data.value, NodeValue::Link(_)) {
            continue;
        }
        let sourcepos = data.sourcepos;
        let value = data.value.clone();
        drop(data);

        let plain_display = plain_text(node);
        let display = if plain_display == reference.label {
            plain_display
        } else {
            render_link_text(node, options)
        };
        let image_marker = if is_image { "!" } else { "" };
        let shortcut = if display == reference.label {
            format!("{image_marker}[{}]", reference.label)
        } else {
            format!("{image_marker}[{display}][{}]", reference.label)
        };
        let marker = format!(
            "{marker_prefix}{}x{}x{}x{}",
            sourcepos.start.line, sourcepos.start.column, sourcepos.end.line, sourcepos.end.column,
        );
        let children = node.children().collect::<Vec<_>>();
        for child in &children {
            child.detach();
        }
        node.data_mut().value = NodeValue::Text(marker.clone().into());
        rendered_nodes.push((node, marker, shortcut, value, children));
    }
    let markdown = render_item_commonmark(item, options);
    rendered_nodes.reverse();
    for (node, _, _, value, children) in &rendered_nodes {
        node.data_mut().value = value.clone();
        for child in children {
            node.append(*child);
        }
    }
    let markers = rendered_nodes
        .into_iter()
        .map(|(_, marker, shortcut, _, _)| (marker, shortcut))
        .collect();
    (markdown, markers)
}

pub(crate) fn configured_link_marker_prefix(source: &str) -> String {
    let arena = Arena::new();
    let options = comrak_options();
    let root = parse_document(&arena, source, &options);
    let mut decoded = String::new();
    format_commonmark(root, &options, &mut decoded)
        .expect("writing CommonMark to a String cannot fail");
    (0_u64..)
        .map(|nonce| format!("sachointernalconfiguredlink{nonce}"))
        .find(|prefix| !source.contains(prefix) && !decoded.contains(prefix))
        .expect("an absent configured-link marker prefix")
}

pub(crate) fn replace_configured_link_markers(
    mut markdown: String,
    markers: &[(String, String)],
) -> String {
    for (marker, shortcut) in markers {
        markdown = markdown.replace(&format!(r"\[{marker}"), &format!("[{marker}"));
        markdown = markdown.replace(&format!(r"{marker}\]"), &format!("{marker}]"));
        markdown = markdown.replace(marker, shortcut);
    }
    markdown.trim_end().to_owned()
}

fn render_link_text<'a>(node: &'a AstNode<'a>, options: &ComrakOptions<'_>) -> String {
    let mut markdown = String::new();
    for child in node.children() {
        format_commonmark(child, options, &mut markdown)
            .expect("writing CommonMark to a String cannot fail");
    }
    markdown.trim_end().to_owned()
}

fn validate_fragment_shape<'a>(
    root: &'a AstNode<'a>,
    line_offset: usize,
) -> std::result::Result<&'a AstNode<'a>, FragmentError> {
    let mut children = root.children();
    let Some(first) = children.next() else {
        return Err(invalid_shape(
            "empty document",
            root.data().sourcepos,
            line_offset,
        ));
    };
    if let Some(second) = children.next() {
        return Err(invalid_shape(
            node_kind(second),
            second.data().sourcepos,
            line_offset,
        ));
    }
    match &first.data().value {
        NodeValue::List(list) if list.list_type == ListType::Bullet => Ok(first),
        _ => Err(invalid_shape(
            node_kind(first),
            first.data().sourcepos,
            line_offset,
        )),
    }
}

fn invalid_shape(kind: &'static str, sourcepos: Sourcepos, line_offset: usize) -> FragmentError {
    FragmentError::InvalidShape {
        kind,
        line: sourcepos.start.line + line_offset,
        column: sourcepos.start.column,
    }
}

fn node_kind<'a>(node: &'a AstNode<'a>) -> &'static str {
    match &node.data().value {
        NodeValue::BlockQuote => "block quote",
        NodeValue::CodeBlock(_) => "code block",
        NodeValue::Document => "document",
        NodeValue::Heading(_) => "heading",
        NodeValue::HtmlBlock(_) => "HTML block",
        NodeValue::Item(_) => "list item",
        NodeValue::List(list) if list.list_type == ListType::Ordered => "ordered list",
        NodeValue::List(_) => "unordered list",
        NodeValue::Paragraph => "paragraph",
        NodeValue::ThematicBreak => "thematic break",
        _ => "unsupported top-level node",
    }
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

// Sacho checks whether an item contains intentionally authored CommonMark
// content, not whether a particular browser would paint pixels for it. HTML
// visibility depends on the renderer, sanitizer, user agent, and stylesheets,
// so attempting to reproduce those rules here would be both incomplete and
// outside Sacho's responsibility. Only whitespace and complete HTML comments
// are treated as scaffolding; every other Markdown or raw HTML construct is
// substantive.
fn has_substantive_content<'a>(node: &'a AstNode<'a>) -> bool {
    let data = node.data();
    match &data.value {
        NodeValue::Text(text) => !text.trim().is_empty(),
        NodeValue::HtmlInline(source) => html_has_substantive_content(source),
        NodeValue::HtmlBlock(block) => html_has_substantive_content(&block.literal),
        NodeValue::Code(_)
        | NodeValue::CodeBlock(_)
        | NodeValue::FootnoteReference(_)
        | NodeValue::Image(_)
        | NodeValue::Link(_)
        | NodeValue::Math(_)
        | NodeValue::TaskItem(_)
        | NodeValue::ThematicBreak => true,
        _ => {
            drop(data);
            node.children().any(has_substantive_content)
        }
    }
}

fn html_has_substantive_content(source: &str) -> bool {
    let mut rest = source.trim();
    while let Some(comment) = rest.strip_prefix("<!--") {
        let Some((_, remainder)) = comment.split_once("-->") else {
            return true;
        };
        rest = remainder.trim_start();
    }
    !rest.is_empty()
}

fn collect_references<'a>(
    node: &'a AstNode<'a>,
    configured_links: &BTreeMap<Sourcepos, ReferenceUse>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    references: &mut Vec<ReferenceUse>,
) -> std::result::Result<(), FragmentError> {
    let data = node.data();
    match &data.value {
        NodeValue::Text(text) => scan_reference_labels(text, link_templates, references)?,
        NodeValue::Link(_) | NodeValue::Image(_) => {
            drop(data);
            if let Some(reference) = configured_links.get(&node.data().sourcepos) {
                references.push(reference.clone());
            }
            for child in node.children() {
                collect_references(child, configured_links, link_templates, references)?;
            }
            return Ok(());
        }
        _ => {}
    }
    drop(data);
    for child in node.children() {
        collect_references(child, configured_links, link_templates, references)?;
    }
    Ok(())
}

fn scan_reference_labels(
    text: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    references: &mut Vec<ReferenceUse>,
) -> std::result::Result<(), FragmentError> {
    let mut rest = text;
    while let Some((_, after_open)) = rest.split_once('[') {
        let Some((label, after_close)) = after_open.split_once(']') else {
            break;
        };
        if let Some(reference) = parse_reference_label(label, link_templates)? {
            references.push(reference);
        }
        rest = after_close;
    }
    Ok(())
}

pub(crate) fn parse_reference_label(
    label: &str,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
) -> std::result::Result<Option<ReferenceUse>, FragmentError> {
    let label = label.trim_matches(|character| character == '[' || character == ']');
    for sigil in link_templates.keys() {
        let sigil_text = sigil.as_str();
        if let Some(number) = label.strip_prefix(sigil_text)
            && !number.is_empty()
            && number.chars().all(|character| character.is_ascii_digit())
        {
            let Ok(number) = number.parse() else {
                return UnknownReferenceSnafu {
                    label: label.to_owned(),
                }
                .fail();
            };
            return Ok(Some(ReferenceUse {
                label: label.to_owned(),
                sigil: sigil_text.to_owned(),
                number,
            }));
        }
    }

    if looks_like_reference_label(label) {
        return UnknownReferenceSnafu {
            label: label.to_owned(),
        }
        .fail();
    }

    Ok(None)
}

fn looks_like_reference_label(label: &str) -> bool {
    if let Some(number) = label.strip_prefix('#') {
        return !number.is_empty() && number.chars().all(|character| character.is_ascii_digit());
    }

    let Some((prefix, number)) = label.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        && !number.is_empty()
        && number.chars().all(|character| character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn links() -> IndexMap<ReferenceSigil, UrlTemplate> {
        IndexMap::from([(
            ReferenceSigil::new("#"),
            UrlTemplate::new("https://example.com/issues/{n}"),
        )])
    }

    #[test]
    fn parses_fragment_without_frontmatter() {
        let fragment = parse_fragment("change.md".into(), " -  Added thing.\n", None, &links())
            .expect("fragment");

        assert_eq!(fragment.priority, 0);
        assert_eq!(fragment.items[0].sort_text, "Added thing.");
    }

    #[test]
    fn treats_null_resolved_links_as_empty() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n---\n -  Added thing.\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert!(fragment.links.is_empty());
    }

    #[test]
    fn parses_empty_frontmatter() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\n---\n -  Added thing.\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(fragment.priority, 0);
    }

    #[test]
    fn parses_resolved_links_from_frontmatter() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#123\": https://example.com/pull/123\n---\n -  Fixed thing.  [[#123]]\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.links,
            BTreeMap::from([(
                String::from("#123"),
                String::from("https://example.com/pull/123")
            )])
        );
        assert!(fragment.warnings.is_empty());
    }

    #[test]
    fn recognizes_pinned_configured_full_reference_labels() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#1\": https://example.com/pull/1\n---\n -  Fixed [the issue][#1].\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].references,
            vec![ReferenceUse {
                label: String::from("#1"),
                sigil: String::from("#"),
                number: 1,
            }]
        );
    }

    #[test]
    fn recognizes_pinned_configured_reference_style_images() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#1\": https://example.com/pull/1\n---\n -  Added ![a screenshot][#1].\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].references,
            vec![ReferenceUse {
                label: String::from("#1"),
                sigil: String::from("#"),
                number: 1,
            }]
        );
        assert_eq!(fragment.items[0].markdown, "- Added ![a screenshot][#1].");
    }

    #[test]
    fn preserves_bracket_wrapped_ordinary_inline_links() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Read [[#1](https://docs.example/guide)].\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert!(fragment.items[0].references.is_empty());
        assert_eq!(
            fragment.items[0].markdown,
            "- Read \\[[\\#1](https://docs.example/guide)\\]."
        );
    }

    #[test]
    fn preserves_an_ordinary_link_containing_a_configured_image() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#1\": https://images.example/screenshot.png\n---\n -  Read [![screenshot][#1]](https://docs.example/guide).\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].references,
            vec![ReferenceUse {
                label: String::from("#1"),
                sigil: String::from("#"),
                number: 1,
            }]
        );
        assert_eq!(
            fragment.items[0].markdown,
            "- Read [![screenshot][#1]](https://docs.example/guide)."
        );
    }

    #[test]
    fn rejects_malformed_resolved_link_labels() {
        let error = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#l\": https://example.com/pull/1\n---\n -  Fixed thing.  [[#1]]\n",
            None,
            &links(),
        )
        .expect_err("resolved link labels must be complete configured references");

        assert!(error.to_string().contains("resolved link label"));
        assert!(error.to_string().contains("#l"));
    }

    proptest! {
        #[test]
        fn resolved_link_labels_must_be_used_by_the_fragment(
            pinned in 0_u64..10_000,
            used in 0_u64..10_000,
        ) {
            let source = format!(
                "---\nlinks:\n  \"#{pinned}\": https://example.com/pull/{pinned}\n---\n -  Fixed thing.  [[#{used}]]\n"
            );
            let result = parse_fragment("change.md".into(), &source, None, &links());

            if pinned == used {
                prop_assert!(result.is_ok());
            } else {
                let error = result.expect_err("unused resolved link labels must fail");
                prop_assert!(error.to_string().contains("resolved link label"));
                prop_assert!(error.to_string().contains("not used"));
            }
        }
    }

    #[test]
    fn rejects_non_http_resolved_link() {
        let error = parse_fragment(
            "change.md".into(),
            "---\nlinks:\n  \"#123\": file:///tmp/123\n---\n -  Fixed thing.  [[#123]]\n",
            None,
            &links(),
        )
        .expect_err("resolved links must be HTTP URLs");

        assert!(matches!(
            error,
            FragmentError::InvalidResolvedLink { ref label, .. } if label == "#123"
        ));
    }

    #[test]
    fn rejects_resolved_links_with_username_or_password() {
        for url in [
            "https://user:secret@example.com/pull/123",
            "https://:secret@example.com/pull/123",
        ] {
            let source =
                format!("---\nlinks:\n  \"#123\": {url}\n---\n -  Fixed thing.  [[#123]]\n");
            let error = parse_fragment("change.md".into(), &source, None, &links())
                .expect_err("resolved links must not contain userinfo");

            assert!(matches!(
                error,
                FragmentError::InvalidResolvedLink { ref label, ref reason, .. }
                    if label == "#123" && reason.contains("userinfo")
            ));
            let FragmentError::InvalidResolvedLink {
                url: reported_url, ..
            } = &error
            else {
                unreachable!("checked above");
            };
            assert!(!reported_url.contains("user:secret"));
            assert!(!reported_url.contains(":secret@"));
            assert!(!error.to_string().contains("user:secret"));
            assert!(!error.to_string().contains(":secret@"));
        }
    }

    #[test]
    fn reports_unknown_frontmatter_key_as_warning() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\npriority: -5\nowner: core\n---\n -  Added thing.\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(fragment.priority, -5);
        assert_eq!(
            fragment.warnings,
            vec![FragmentWarning::UnknownFrontmatterKey {
                key: String::from("owner")
            }]
        );
    }

    #[test]
    fn rejects_non_integer_priority() {
        let error = parse_fragment(
            "change.md".into(),
            "---\npriority: high\n---\n -  Added thing.\n",
            None,
            &links(),
        )
        .expect_err("priority must be integer");

        assert!(matches!(error, FragmentError::Frontmatter { .. }));
    }

    #[test]
    fn rejects_unclosed_frontmatter() {
        let error = parse_fragment("change.md".into(), "---\npriority: 1\n", None, &links())
            .expect_err("frontmatter must close");

        assert!(matches!(error, FragmentError::UnclosedFrontmatter));
    }

    #[test]
    fn rejects_frontmatter_opening_without_newline() {
        let error =
            parse_fragment("change.md".into(), "---", None, &links()).expect_err("frontmatter");

        assert!(matches!(error, FragmentError::UnclosedFrontmatter));
    }

    #[test]
    fn rejects_top_level_paragraph() {
        let error = parse_fragment("change.md".into(), "Added thing.\n", None, &links())
            .expect_err("paragraph is invalid");

        assert!(matches!(
            error,
            FragmentError::InvalidShape {
                kind: "paragraph",
                ..
            }
        ));
    }

    #[test]
    fn reports_shape_error_line_after_frontmatter() {
        let error = parse_fragment(
            "change.md".into(),
            "---\npriority: 1\n---\nAdded thing.\n",
            None,
            &links(),
        )
        .expect_err("paragraph is invalid");

        assert!(matches!(
            error,
            FragmentError::InvalidShape {
                kind: "paragraph",
                line: 4,
                column: 1,
            }
        ));
    }

    #[test]
    fn strips_frontmatter_before_fragment_body() {
        let fragment = parse_fragment(
            "change.md".into(),
            "---\npriority: 1\nowner: docs\n---\n -  Added thing.\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(fragment.items[0].markdown, "- Added thing.");
        assert_eq!(fragment.items[0].sort_text, "Added thing.");
    }

    #[test]
    fn rejects_ordered_list() {
        let error = parse_fragment("change.md".into(), "1. Added thing.\n", None, &links())
            .expect_err("ordered list is invalid");

        assert!(matches!(
            error,
            FragmentError::InvalidShape {
                kind: "ordered list",
                ..
            }
        ));
    }

    #[test]
    fn reports_specific_invalid_top_level_node_kinds() {
        let cases = [
            ("> quoted\n", "block quote"),
            ("~~~~ text\ncode\n~~~~\n", "code block"),
            ("# Heading\n", "heading"),
            ("<div>\nhtml\n</div>\n", "HTML block"),
            ("***\n", "thematic break"),
        ];

        for (source, expected_kind) in cases {
            let error = parse_fragment("change.md".into(), source, None, &links())
                .expect_err("top-level node is invalid");
            assert!(matches!(
                error,
                FragmentError::InvalidShape {
                    kind,
                    ..
                } if kind == expected_kind
            ));
        }

        let arena = Arena::new();
        let root = parse_document(&arena, " -  item\n", &comrak_options());
        assert_eq!(node_kind(root), "document");
        let list = root.first_child().expect("list");
        let item = list.first_child().expect("item");
        assert_eq!(node_kind(item), "list item");
    }

    #[test]
    fn rejects_multiple_top_level_lists() {
        let error = parse_fragment(
            "change.md".into(),
            " -  Added thing.\n\n* Fixed thing.\n",
            None,
            &links(),
        )
        .expect_err("multiple lists are invalid");

        assert!(matches!(
            error,
            FragmentError::InvalidShape {
                kind: "unordered list",
                ..
            }
        ));
    }

    #[test]
    fn accepts_nested_list_and_code_block_inside_item() {
        let source = " -  Added thing.\n\n    -  Nested detail.\n\n    ~~~~ rust\n    fn main() {}\n    ~~~~\n";
        let fragment = parse_fragment("change.md".into(), source, Some("core".into()), &links())
            .expect("fragment");

        assert_eq!(fragment.section, Some(String::from("core")));
        assert_eq!(fragment.items.len(), 1);
    }

    #[test]
    fn extracts_sort_text_without_markup() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Added `clear()` **method** and [docs](https://example.com).\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].sort_text,
            "Added clear() method and docs."
        );
    }

    #[test]
    fn extracts_sort_text_from_extended_markdown_nodes() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Added <span>inline</span> HTML, $math$, and line\n    break.\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].sort_text,
            "Added <span>inline</span> HTML, math, and line break."
        );
    }

    #[test]
    fn distinguishes_substantive_content_from_html_comments() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  <!-- TODO: write this -->\n -  <!-- first --><!-- second -->\n -  <span hidden></span>\n -  ![](release.png)\n -  [](/release-artifact)\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert!(!fragment.items[0].has_substantive_content);
        assert!(!fragment.items[1].has_substantive_content);
        assert!(fragment.items[2].has_substantive_content);
        assert!(fragment.items[3].has_substantive_content);
        assert!(fragment.items[4].has_substantive_content);
    }

    #[test]
    fn substantive_content_can_appear_after_an_empty_first_block() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  <!-- TODO -->\n\n    **Visible text.**\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert!(fragment.items[0].has_substantive_content);
    }

    #[test]
    fn html_comment_scaffolding_requires_complete_comments_only() {
        assert!(!html_has_substantive_content(
            "<!-- first --> <!-- second -->"
        ));
        assert!(html_has_substantive_content(
            "<!-- comment --> <span></span>"
        ));
        assert!(html_has_substantive_content("<!-- unterminated"));
    }

    #[test]
    fn plain_text_handles_block_nodes() {
        let arena = Arena::new();
        let root = parse_document(
            &arena,
            "~~~~ text\ncode literal\n~~~~\n\n<div>\nhtml literal\n</div>\n",
            &comrak_options(),
        );
        let code = root.first_child().expect("code block");
        let html = code.next_sibling().expect("HTML block");

        assert_eq!(plain_text(code), "code literal\n");
        assert_eq!(plain_text(html), "<div>\nhtml literal\n</div>\n");
        assert!(has_substantive_content(html));

        let arena = Arena::new();
        let root = parse_document(
            &arena,
            "[^note]\n\n[^note]: Footnote text.\n",
            &comrak_options(),
        );
        let paragraph = root.first_child().expect("paragraph");
        assert_eq!(plain_text(paragraph), "note");
    }
    #[test]
    fn collects_resolvable_references() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Added thing.  [[#842], [#848] by Lee]\n",
            None,
            &links(),
        )
        .expect("fragment");

        assert_eq!(
            fragment.items[0].references,
            vec![
                ReferenceUse {
                    label: String::from("#842"),
                    sigil: String::from("#"),
                    number: 842
                },
                ReferenceUse {
                    label: String::from("#848"),
                    sigil: String::from("#"),
                    number: 848
                }
            ]
        );
    }

    #[test]
    fn rejects_unconfigured_reference_sigil() {
        let error = parse_fragment(
            "change.md".into(),
            " -  Added thing.  [ABC-123]\n",
            None,
            &links(),
        )
        .expect_err("reference should be unresolved");

        assert!(matches!(error, FragmentError::UnknownReference { .. }));
    }

    #[test]
    fn ignores_reference_like_text_inside_code() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Documented literal `[#123]` and `ABC-123` values.\n\n    ~~~~ text\n    [ABC-123]\n    [#456]\n    ~~~~\n",
            None,
            &links(),
        )
        .expect("code literals should not be references");

        assert!(fragment.items[0].references.is_empty());
    }

    #[test]
    fn scans_multiple_reference_labels_with_surrounding_text() {
        let mut references = Vec::new();

        scan_reference_labels(
            "Fixed before [#12], middle [#345], and after.",
            &links(),
            &mut references,
        )
        .expect("references");

        assert_eq!(
            references,
            vec![
                ReferenceUse {
                    label: String::from("#12"),
                    sigil: String::from("#"),
                    number: 12,
                },
                ReferenceUse {
                    label: String::from("#345"),
                    sigil: String::from("#"),
                    number: 345,
                },
            ]
        );
    }

    #[test]
    fn ignores_incomplete_reference_like_labels() {
        let fragment = parse_fragment(
            "change.md".into(),
            " -  Mentioned [#], [ABC-], [abc-123], and [ABC-x] literally.\n",
            None,
            &links(),
        )
        .expect("incomplete labels are plain text");

        assert!(fragment.items[0].references.is_empty());
    }

    #[test]
    fn reports_unreadable_fragment_directory() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "Core"
            directory = "core"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        std::fs::write(temp.path().join("changes.d/core"), "not a directory\n")
            .expect("section file");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let error = discover_fragment_candidates(&repo).expect_err("read_dir should fail");

        assert!(matches!(error, crate::Error::ReadFile { .. }));
    }

    #[test]
    fn discovers_section_fragments_in_deterministic_order() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"

            [[sections]]
            id = "Core"
            directory = "core"
            "##,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/core")).expect("section dir");
        std::fs::write(temp.path().join("changes.d/core/z.md"), " -  Fixed z.\n").expect("z");
        std::fs::write(temp.path().join("changes.d/core/a.md"), " -  Fixed a.\n").expect("a");
        std::fs::write(temp.path().join("changes.d/next"), "1.0.0\n").expect("next");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(
            discovered
                .candidates
                .iter()
                .map(|candidate| candidate.relative_path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("changes.d/core/a.md"),
                PathBuf::from("changes.d/core/z.md")
            ]
        );
    }

    #[test]
    fn discovers_nested_patterned_section_fragments() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{scope}/{name}"
            id = "@{scope}/{name}"
            directory = "{scope}/{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/acme/core")).expect("section dir");
        std::fs::write(
            temp.path().join("changes.d/acme/core/change.md"),
            " -  Fixed core.\n",
        )
        .expect("fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(
            discovered.candidates[0].section.as_deref(),
            Some("@acme/core")
        );
        assert!(discovered.warnings.is_empty());
    }

    #[test]
    fn patterned_discovery_accepts_a_missing_fragment_directory() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn patterned_discovery_stops_at_the_maximum_directory_depth() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{scope}/{name}"
            id = "@{scope}/{name}"
            directory = "{scope}/{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/acme/core")).expect("section dir");
        symlink("loop", temp.path().join("changes.d/acme/core/loop")).expect("symlink loop");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn patterned_discovery_prunes_unrelated_nested_directories() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "Legacy"
            directory = "legacy"

            [[section-patterns]]
            source = "packages/{scope}/{name}"
            id = "@{scope}/{name}"
            directory = "packages/{scope}/{name}"
            "#,
        )
        .expect("config");
        let legacy = temp.path().join("changes.d/legacy");
        let private = legacy.join("private");
        std::fs::create_dir_all(&private).expect("private directory");
        std::fs::write(legacy.join("change.md"), " -  Fixed legacy.\n").expect("fragment");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o000))
            .expect("remove permissions");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo);

        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions");
        let discovered = discovered.expect("unrelated directory should be pruned");
        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(discovered.candidates[0].section.as_deref(), Some("Legacy"));
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn patterned_discovery_ignores_file_symlinks_at_intermediate_depth() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{scope}/{name}"
            id = "@{scope}/{name}"
            directory = "{scope}/{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        std::fs::write(temp.path().join("changes.d/archive.md"), " -  Archived.\n")
            .expect("fragment");
        symlink("archive.md", temp.path().join("changes.d/latest")).expect("file symlink");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn patterned_discovery_ignores_symlink_loops_at_intermediate_depth() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{scope}/{name}"
            id = "@{scope}/{name}"
            directory = "{scope}/{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        symlink("loop", temp.path().join("changes.d/loop")).expect("symlink loop");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn patterned_discovery_ignores_non_utf8_directories() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        )
        .expect("config");
        let alien = temp
            .path()
            .join("changes.d")
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        std::fs::create_dir_all(&alien).expect("non-UTF-8 directory");
        std::fs::write(alien.join("change.md"), " -  Alien.\n").expect("fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_patterned_fragment_directory_symlinked_outside_the_repository() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "{name}"
            directory = "{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        std::fs::write(outside.path().join("change.md"), " -  Escaped.\n")
            .expect("outside fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");
        symlink(outside.path(), temp.path().join("changes.d/core")).expect("outside symlink");

        let error = discover_fragment_candidates(&repo).expect_err("outside patterned directory");

        let message = error.to_string();
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
        assert!(message.contains("fragments.directory"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn discovers_symlinked_unknown_section_with_a_warning() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "core"
            directory = "core"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment directory");
        std::fs::create_dir(temp.path().join("shared-extra")).expect("shared directory");
        std::fs::write(temp.path().join("shared-extra/change.md"), " -  Shared.\n")
            .expect("shared fragment");
        symlink("../shared-extra", temp.path().join("changes.d/extra")).expect("section symlink");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(
            discovered.candidates[0].relative_path,
            PathBuf::from("changes.d/extra/change.md")
        );
        assert!(matches!(
            &discovered.warnings[0],
            DiscoveryWarning::UnknownSectionFragment { section, .. } if section == "extra"
        ));
    }

    #[test]
    fn discovers_a_parent_normalized_section_exactly_once() {
        for archive_exists in [false, true] {
            let temp = tempfile::TempDir::new().expect("tempdir");
            std::fs::write(
                temp.path().join("sacho.toml"),
                r#"
                [[sections]]
                id = "Core"
                directory = "packages/archive/.."
                "#,
            )
            .expect("config");
            std::fs::create_dir_all(temp.path().join("changes.d/packages")).expect("section dir");
            if archive_exists {
                std::fs::create_dir(temp.path().join("changes.d/packages/archive"))
                    .expect("archive dir");
            }
            std::fs::write(
                temp.path().join("changes.d/packages/change.md"),
                " -  Fixed package discovery.\n",
            )
            .expect("fragment");
            let repo = Repository::from_root(temp.path()).expect("repo");

            let discovered = discover_fragment_candidates(&repo).expect("candidates");

            assert_eq!(discovered.candidates.len(), 1, "archive={archive_exists}");
            assert_eq!(
                discovered.candidates[0].section,
                Some(String::from("Core")),
                "archive={archive_exists}"
            );
            assert!(discovered.warnings.is_empty(), "archive={archive_exists}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovers_a_symlinked_section_exactly_once() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "Core"
            directory = "alias"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/actual")).expect("section directory");
        std::fs::write(
            temp.path().join("changes.d/actual/change.md"),
            " -  Fixed symlinked section discovery.\n",
        )
        .expect("fragment");
        symlink("actual", temp.path().join("changes.d/alias")).expect("section alias");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(discovered.candidates[0].section, Some(String::from("Core")));
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn discovers_a_patterned_section_symlink_alias_exactly_once() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "package/{name}"
            directory = "packages/{name}"
            "#,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/packages/core"))
            .expect("section directory");
        std::fs::write(
            temp.path().join("changes.d/packages/core/change.md"),
            " -  Fixed patterned alias discovery.\n",
        )
        .expect("fragment");
        symlink("packages/core", temp.path().join("changes.d/alias")).expect("section alias");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(
            discovered.candidates[0].section.as_deref(),
            Some("package/core")
        );
        assert!(discovered.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_fragment_directory_created_as_an_external_symlink_after_opening() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let outside = tempfile::TempDir::new().expect("outside tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::write(
            outside.path().join("external.md"),
            " -  Must not be discovered.\n",
        )
        .expect("external fragment");
        let repo = Repository::from_root(temp.path()).expect("repository");
        symlink(outside.path(), temp.path().join("changes.d")).expect("external symlink");

        let error = discover_fragment_candidates(&repo).expect_err("external fragment directory");
        let message = error.to_string();

        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("outside repository root"), "{message}");
    }

    #[test]
    fn discovers_no_candidates_when_section_fragment_directory_is_missing() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "Core"
            directory = "core"
            "#,
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert!(discovered.candidates.is_empty());
        assert!(discovered.warnings.is_empty());
    }

    #[test]
    fn reports_unknown_section_fragment_as_discovery_warning() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            r##"
            [[sections]]
            id = "Core"
            directory = "core"
            "##,
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d/extra")).expect("extra dir");
        std::fs::write(
            temp.path().join("changes.d/extra/change.md"),
            " -  Fixed extra.\n",
        )
        .expect("fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");

        let discovered = discover_fragment_candidates(&repo).expect("candidates");

        assert_eq!(
            discovered.warnings,
            vec![DiscoveryWarning::UnknownSectionFragment {
                path: PathBuf::from("changes.d/extra/change.md"),
                section: String::from("extra"),
            }]
        );
        assert_eq!(
            discovered.candidates[0].section,
            Some(String::from("extra"))
        );
    }

    proptest! {
        #[test]
        fn comment_sequences_are_scaffolding(
            comments in prop::collection::vec("[a-z ]{0,20}", 1..8),
        ) {
            let scaffolding = comments
                .iter()
                .map(|comment| format!("<!--{comment}-->\n"))
                .collect::<String>();

            prop_assert!(!html_has_substantive_content(&scaffolding));
            let content = format!("{scaffolding}<span></span>");
            let is_substantive = html_has_substantive_content(&content);
            prop_assert!(is_substantive);
        }

        #[test]
        fn parses_frontmatter_priority(priority in -10_000_i32..10_000) {
            let source = format!("---\npriority: {priority}\n---\n -  Changed thing.\n");

            let fragment = parse_fragment("change.md".into(), &source, None, &links())
                .expect("fragment");

            prop_assert_eq!(fragment.priority, priority);
        }

        #[test]
        fn preserves_item_ordinals(words in prop::collection::vec("[a-z]{1,8}", 1..12)) {
            let source = words
                .iter()
                .map(|word| format!(" -  Changed {word}.\n"))
                .collect::<String>();

            let fragment = parse_fragment("change.md".into(), &source, None, &links())
                .expect("fragment");

            prop_assert_eq!(fragment.items.len(), words.len());
            prop_assert_eq!(
                fragment
                    .items
                    .iter()
                    .map(|item| item.ordinal)
                    .collect::<Vec<_>>(),
                (0..words.len()).collect::<Vec<_>>()
            );
        }

        #[test]
        fn collects_and_deduplicates_references(numbers in prop::collection::vec(1_u64..100, 1..25)) {
            let labels = numbers
                .iter()
                .map(|number| format!("[#{number}]"))
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(" -  Fixed references.  [{labels}]\n");

            let fragment = parse_fragment("change.md".into(), &source, None, &links())
                .expect("fragment");

            let mut expected = numbers
                .into_iter()
                .map(|number| ReferenceUse {
                    label: format!("#{number}"),
                    sigil: String::from("#"),
                    number,
                })
                .collect::<Vec<_>>();
            expected.sort();
            expected.dedup();

            prop_assert_eq!(fragment.items[0].references.clone(), expected);
        }

        #[test]
        fn discovers_fragment_candidates_in_path_order(stems in prop::collection::vec("[a-z]{1,8}", 1..12)) {
            let temp = tempfile::TempDir::new().expect("tempdir");
            std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
            std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            let mut paths = Vec::new();
            for (index, stem) in stems.iter().enumerate().rev() {
                let path = PathBuf::from(format!("changes.d/{index:02}-{stem}.md"));
                std::fs::write(temp.path().join(&path), " -  Changed thing.\n")
                    .expect("fragment");
                paths.push(path);
            }
            paths.sort();
            let repo = Repository::from_root(temp.path()).expect("repo");

            let discovered = discover_fragment_candidates(&repo).expect("candidates");

            prop_assert_eq!(
                discovered
                    .candidates
                    .iter()
                    .map(|candidate| candidate.relative_path.clone())
                    .collect::<Vec<_>>(),
                paths
            );
        }
    }
}
