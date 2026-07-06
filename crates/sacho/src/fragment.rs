use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use comrak::nodes::{AstNode, ListType, NodeValue, Sourcepos};
use comrak::{Arena, Options as ComrakOptions, parse_document};
use indexmap::IndexMap;
use serde::Deserialize;
use snafu::ResultExt;

use crate::config::{ReferenceSigil, UrlTemplate};
use crate::error::{
    FragmentError, FrontmatterSnafu, ReadFileSnafu, Result, UnclosedFrontmatterSnafu,
    UnknownReferenceSnafu,
};
use crate::repo::Repository;

/// Parsed changelog fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Path to the fragment file.
    pub path: PathBuf,

    /// Section identifier inferred from the fragment directory.
    pub section: Option<String>,

    /// Sort priority read from frontmatter.
    pub priority: i32,

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

    /// Reference shortcut labels found in the item.
    pub references: Vec<ReferenceUse>,
}

/// Parsed YAML frontmatter for a fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// Sort priority for all items in the fragment.
    pub priority: i32,

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
    let config = repo.config();
    let fragment_dir = repo.resolve(&config.fragments.directory);
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();

    if config.sections.is_empty() {
        collect_markdown_files(&fragment_dir, None, &mut candidates)?;
    } else {
        for section in &config.sections {
            collect_markdown_files(
                &fragment_dir.join(&section.directory),
                Some(section.id.clone()),
                &mut candidates,
            )?;
        }

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
            if !path.is_dir() || is_configured_section_dir(config, &path) {
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

    candidates
        .sort_by(|(left, _), (right, _)| left.to_string_lossy().cmp(&right.to_string_lossy()));

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

    let arena = Arena::new();
    let root = parse_document(&arena, body, &comrak_options());
    let list = validate_fragment_shape(root, body_line_offset)?;

    let line_starts = line_starts(body);
    let mut items = Vec::new();
    for (ordinal, item) in list.children().enumerate() {
        let markdown = slice_sourcepos(body, &line_starts, item.data().sourcepos)
            .unwrap_or_default()
            .trim_end()
            .to_owned();
        let first_block = item.first_child();
        let sort_text = first_block
            .map(plain_text)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let mut references = Vec::new();
        collect_references(item, link_templates, &mut references)?;
        references.sort();
        references.dedup();
        items.push(FragmentItem {
            ordinal,
            markdown,
            sort_text,
            references,
        });
    }

    Ok(Fragment {
        path,
        section,
        priority: frontmatter.priority,
        items,
        warnings,
    })
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

fn is_configured_section_dir(config: &crate::Config, path: &Path) -> bool {
    config
        .sections
        .iter()
        .any(|section| path.ends_with(&section.directory))
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
        Ok(Self {
            priority: raw.priority.unwrap_or_default(),
            unknown_keys: raw.extra.into_keys().collect(),
        })
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

fn collect_references<'a>(
    node: &'a AstNode<'a>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    references: &mut Vec<ReferenceUse>,
) -> std::result::Result<(), FragmentError> {
    let data = node.data();
    if let NodeValue::Text(text) = &data.value {
        scan_reference_labels(text, link_templates, references)?;
    }
    drop(data);
    for child in node.children() {
        collect_references(child, link_templates, references)?;
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

fn parse_reference_label(
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
    let mut end = end_line_start + sourcepos.end.column;
    end = end.min(source.len());
    source.get(start..end)
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

        assert_eq!(fragment.items[0].markdown, "-  Added thing.");
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
