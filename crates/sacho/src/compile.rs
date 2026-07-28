use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use comrak::nodes::NodeValue;
use comrak::{Arena, Options as ComrakOptions, parse_document};
use indexmap::IndexMap;
use snafu::ResultExt;

use crate::config::{ReferenceSigil, UrlTemplate};
use crate::error::{ReadFileSnafu, Result};
use crate::fragment::{Fragment, ReferenceUse, discover_fragments};
use crate::markdown::{escape_angle_bracket_destination, format_markdown_with_word_wrap};
use crate::repo::Repository;
use crate::section::{SectionResolver, has_sections};

/// Rendered section identifier.
pub type SectionId = String;

/// Options for compiling the unreleased region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOptions {
    /// Optional section identifier to compile by itself.
    pub section: Option<SectionId>,

    /// Whether to include the version heading and date line when no entries
    /// are available.
    pub include_empty_region: bool,

    /// Whether to wrap rendered Markdown at Sacho's canonical line width.
    pub word_wrap: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            section: None,
            include_empty_region: true,
            word_wrap: true,
        }
    }
}

/// Compiled unreleased changelog region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRegion {
    /// Version label used by the rendered heading.
    pub version_label: VersionLabel,

    /// Markdown text produced by the compiler.
    pub markdown: String,

    /// Rendered sections included in the region.
    pub sections: Vec<CompiledSection>,

    /// Number of compiled items that contain substantive CommonMark content.
    pub substantive_item_count: usize,
}

impl CompiledRegion {
    pub(crate) fn is_active(&self) -> bool {
        matches!(self.version_label, VersionLabel::Version(_)) || !self.sections.is_empty()
    }
}

/// Version label for an unreleased changelog region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionLabel {
    /// Explicit next version label.
    Version(String),

    /// Fallback label when no next-version file exists.
    Unreleased,
}

impl VersionLabel {
    fn heading(&self) -> String {
        match self {
            Self::Version(version) => format!("Version {version}"),
            Self::Unreleased => String::from("Unreleased"),
        }
    }
}

/// Compiled contents for one rendered section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSection {
    /// Section identifier, or `None` for repositories without sections.
    pub id: Option<SectionId>,

    /// Markdown text for this section, without the version heading.
    pub markdown: String,

    /// Items rendered in this section.
    pub items: Vec<CompiledItem>,

    /// Reference definitions emitted after this section's items.
    pub references: Vec<CompiledReference>,
}

/// Compiled changelog item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledItem {
    /// Repository-relative fragment path that contributed this item.
    pub fragment_path: PathBuf,

    /// Zero-based item position inside the fragment.
    pub ordinal: usize,

    /// Rendered Markdown for the item.
    pub markdown: String,
}

/// Reference definition emitted by the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledReference {
    /// Shortcut reference label, such as `#842`.
    pub label: String,

    /// URL resolved from the configured template.
    pub url: String,
}

#[derive(Debug, Clone)]
struct SortableItem {
    section: Option<SectionId>,
    priority: i32,
    fragment_sort_text: String,
    fragment_path: PathBuf,
    ordinal: usize,
    markdown: String,
    has_substantive_content: bool,
    references: Vec<ReferenceUse>,
}

/// Compiles the current fragments into an unreleased changelog region.
pub fn compile_unreleased(repo: &Repository, options: CompileOptions) -> Result<CompiledRegion> {
    let discovered = discover_fragments(repo)?;
    let version_label = read_version_label(repo)?;
    compile_parsed_fragments(repo, options, version_label, discovered.fragments)
}

/// Compiles fragment values already read and parsed by a command plan.
pub(crate) fn compile_parsed_fragments(
    repo: &Repository,
    options: CompileOptions,
    version_label: VersionLabel,
    fragments: Vec<Fragment>,
) -> Result<CompiledRegion> {
    let config = repo.config();

    validate_requested_section(repo, options.section.as_deref(), &fragments)?;
    let resolved_links = resolved_link_overrides(&fragments)?;
    let mut items = sortable_items(fragments);
    if let Some(section) = &options.section {
        items.retain(|item| item.section.as_deref() == Some(section.as_str()));
    }
    items.sort_by(compare_items);
    let substantive_item_count = items
        .iter()
        .filter(|item| item.has_substantive_content)
        .count();

    let section_ids = ordered_sections(repo, &items)?;
    let mut sections = Vec::new();
    for section_id in section_ids {
        let section_items = items
            .iter()
            .filter(|item| item.section == section_id)
            .collect::<Vec<_>>();
        if section_items.is_empty() {
            continue;
        }
        sections.push(compile_section(
            section_id,
            section_items,
            &config.links,
            &resolved_links,
        ));
    }

    let markdown = if sections.is_empty() && !options.include_empty_region {
        String::new()
    } else {
        format_region(
            &version_label,
            &config.changelog.unreleased_heading,
            &sections,
            options.word_wrap,
        )?
    };

    Ok(CompiledRegion {
        version_label,
        markdown,
        sections,
        substantive_item_count,
    })
}

fn resolved_link_overrides(fragments: &[Fragment]) -> Result<BTreeMap<String, String>> {
    let mut resolved = BTreeMap::<String, String>::new();
    for fragment in fragments {
        let used_labels = fragment
            .items
            .iter()
            .flat_map(|item| item.references.iter())
            .map(|reference| reference.label.as_str())
            .collect::<BTreeSet<_>>();
        for label in used_labels {
            let Some(url) = fragment.links.get(label) else {
                continue;
            };
            if let Some(existing) = resolved.get(label)
                && existing != url
            {
                return Err(crate::Error::ConflictingResolvedLinks {
                    label: label.to_owned(),
                    first: existing.clone(),
                    second: url.clone(),
                });
            }
            resolved.insert(label.to_owned(), url.clone());
        }
    }
    Ok(resolved)
}

pub(crate) fn validate_resolved_link_consistency(fragments: &[Fragment]) -> Result<()> {
    resolved_link_overrides(fragments).map(drop)
}

fn validate_requested_section(
    repo: &Repository,
    section: Option<&str>,
    fragments: &[Fragment],
) -> Result<()> {
    let Some(section) = section else {
        return Ok(());
    };
    if SectionResolver::from_config(repo.config())?
        .resolve_id(section)
        .map_err(|error| crate::Error::SectionPattern {
            message: error.to_string(),
        })?
        .is_some()
    {
        return Ok(());
    }
    if fragments
        .iter()
        .any(|fragment| fragment.section.as_deref() == Some(section))
    {
        return Ok(());
    }

    Err(crate::Error::UnknownSection {
        section: section.to_owned(),
    })
}

fn read_version_label(repo: &Repository) -> Result<VersionLabel> {
    let path = repo.resolve(
        repo.config()
            .fragments
            .directory
            .join(&repo.config().fragments.next_file),
    );
    match fs::read_to_string(&path) {
        Ok(contents) => version_label_from_contents(&path, Some(&contents)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(VersionLabel::Unreleased),
        Err(source) => Err(source).context(ReadFileSnafu { path }),
    }
}

pub(crate) fn version_label_from_contents(
    path: &Path,
    contents: Option<&str>,
) -> Result<VersionLabel> {
    let Some(contents) = contents else {
        return Ok(VersionLabel::Unreleased);
    };
    let values = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(VersionLabel::Unreleased),
        [value] => Ok(VersionLabel::Version((*value).to_owned())),
        _ => Err(crate::Error::InvalidNextVersion {
            path: path.to_path_buf(),
        }),
    }
}

fn sortable_items(fragments: Vec<Fragment>) -> Vec<SortableItem> {
    fragments
        .into_iter()
        .flat_map(|fragment| {
            let section = fragment.section;
            let priority = fragment.priority;
            let fragment_path = fragment.path;
            let fragment_sort_text = fragment
                .items
                .first()
                .map(|item| item.sort_text.clone())
                .unwrap_or_default();
            fragment.items.into_iter().map(move |item| SortableItem {
                section: section.clone(),
                priority,
                fragment_sort_text: fragment_sort_text.clone(),
                fragment_path: fragment_path.clone(),
                ordinal: item.ordinal,
                markdown: item.markdown,
                has_substantive_content: item.has_substantive_content,
                references: item.references,
            })
        })
        .collect()
}

fn compare_items(left: &SortableItem, right: &SortableItem) -> std::cmp::Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| left.fragment_sort_text.cmp(&right.fragment_sort_text))
        .then_with(|| path_key(&left.fragment_path).cmp(&path_key(&right.fragment_path)))
        .then_with(|| left.ordinal.cmp(&right.ordinal))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn ordered_sections(repo: &Repository, items: &[SortableItem]) -> Result<Vec<Option<SectionId>>> {
    if !has_sections(repo.config()) {
        return Ok(vec![None]);
    }

    let present = items
        .iter()
        .filter_map(|item| item.section.clone())
        .collect::<BTreeSet<_>>();
    SectionResolver::from_config(repo.config())?
        .ordered_present(&present)
        .map(|sections| sections.into_iter().map(Some).collect())
        .map_err(|error| crate::Error::SectionPattern {
            message: error.to_string(),
        })
}

fn compile_section(
    id: Option<SectionId>,
    items: Vec<&SortableItem>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    resolved_links: &BTreeMap<String, String>,
) -> CompiledSection {
    let references = compile_references(
        items.iter().flat_map(|item| item.references.iter()),
        link_templates,
        resolved_links,
    );
    let mut markdown = String::new();
    if let Some(id) = &id {
        markdown.push_str("### ");
        markdown.push_str(id);
        markdown.push_str("\n\n");
    }
    for item in &items {
        markdown.push_str(item.markdown.trim_end());
        markdown.push('\n');
    }

    CompiledSection {
        id,
        markdown,
        items: items
            .into_iter()
            .map(|item| CompiledItem {
                fragment_path: item.fragment_path.clone(),
                ordinal: item.ordinal,
                markdown: item.markdown.clone(),
            })
            .collect(),
        references,
    }
}

fn compile_references<'a>(
    references: impl Iterator<Item = &'a ReferenceUse>,
    link_templates: &IndexMap<ReferenceSigil, UrlTemplate>,
    resolved_links: &BTreeMap<String, String>,
) -> Vec<CompiledReference> {
    let sigil_order = link_templates
        .keys()
        .enumerate()
        .map(|(index, sigil)| (sigil.as_str(), index))
        .collect::<Vec<_>>();
    let mut references = references.cloned().collect::<Vec<_>>();
    references.sort_by(|left, right| {
        reference_sigil_order(&left.sigil, &sigil_order)
            .cmp(&reference_sigil_order(&right.sigil, &sigil_order))
            .then_with(|| left.number.cmp(&right.number))
            .then_with(|| left.label.cmp(&right.label))
    });
    references.dedup_by(|left, right| left.label == right.label);

    references
        .into_iter()
        .filter_map(|reference| {
            link_templates
                .iter()
                .find(|(sigil, _)| sigil.as_str() == reference.sigil)
                .map(|(_, template)| CompiledReference {
                    url: resolved_links
                        .get(&reference.label)
                        .cloned()
                        .unwrap_or_else(|| {
                            template
                                .as_str()
                                .replace("{n}", &reference.number.to_string())
                        }),
                    label: reference.label,
                })
        })
        .collect()
}

fn reference_sigil_order(sigil: &str, sigil_order: &[(&str, usize)]) -> usize {
    sigil_order
        .iter()
        .find_map(|(configured, order)| (*configured == sigil).then_some(*order))
        .unwrap_or(usize::MAX)
}

fn format_region(
    version_label: &VersionLabel,
    unreleased_heading: &str,
    sections: &[CompiledSection],
    word_wrap: bool,
) -> Result<String> {
    let mut header = String::new();
    header.push_str("## ");
    header.push_str(&version_label.heading());
    header.push_str("\n\n");
    header.push_str(unreleased_heading);
    header.push('\n');

    let mut markdown = header;
    for section in sections {
        markdown.push('\n');
        markdown.push_str(section.markdown.trim_end());
        markdown.push('\n');
    }
    let mut emitted_references = BTreeSet::new();
    for section in sections {
        for reference in &section.references {
            if emitted_references.insert(reference.label.as_str()) {
                markdown.push('\n');
                append_reference_definitions(&mut markdown, std::slice::from_ref(reference));
            }
        }
    }

    let formatted = format_markdown_with_word_wrap(&markdown, word_wrap)?;
    Ok(trim_empty_list_item_trailing_spaces(&formatted))
}

fn append_reference_definitions(markdown: &mut String, references: &[CompiledReference]) {
    for reference in references {
        append_reference_definition(markdown, &reference.label, &reference.url);
    }
}

fn append_reference_definition(markdown: &mut String, label: &str, url: &str) {
    markdown.push('[');
    markdown.push_str(label);
    markdown.push_str("]: <");
    markdown.push_str(&escape_angle_bracket_destination(url));
    markdown.push('>');
    markdown.push('\n');
}

fn trim_empty_list_item_trailing_spaces(markdown: &str) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &ComrakOptions::default());
    let empty_item_lines = root
        .descendants()
        .filter(|node| {
            matches!(node.data().value, NodeValue::Item(_)) && node.first_child().is_none()
        })
        .map(|node| node.data().sourcepos.start.line)
        .collect::<BTreeSet<_>>();
    let mut output = String::with_capacity(markdown.len());
    for (index, line) in markdown.split_inclusive('\n').enumerate() {
        let (content, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |content| (content, "\n"));
        if empty_item_lines.contains(&(index + 1)) && content.trim() == "-" {
            output.push_str(content.trim_end());
        } else {
            output.push_str(content);
        }
        output.push_str(newline);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use comrak::nodes::{AstNode, NodeValue};
    use comrak::{Arena, Options as ComrakOptions, parse_document};
    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;

    #[derive(Debug, Clone)]
    struct ModelFragment {
        path: PathBuf,
        priority: i32,
        items: Vec<String>,
    }

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

        fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
            match &node.data().value {
                NodeValue::Text(text) => text.to_string(),
                _ => {
                    let mut text = String::new();
                    for child in node.children() {
                        text.push_str(&plain_text(child));
                    }
                    text
                }
            }
        }

        let arena = Arena::new();
        let root = parse_document(&arena, markdown, &ComrakOptions::default());
        let mut links = Vec::new();
        collect(root, &mut links);
        links
    }

    fn rendered_code_blocks(markdown: &str) -> Vec<String> {
        fn collect<'a>(node: &'a AstNode<'a>, blocks: &mut Vec<String>) {
            if let NodeValue::CodeBlock(block) = &node.data().value {
                blocks.push(block.literal.clone());
            }
            for child in node.children() {
                collect(child, blocks);
            }
        }

        let arena = Arena::new();
        let root = parse_document(&arena, markdown, &ComrakOptions::default());
        let mut blocks = Vec::new();
        collect(root, &mut blocks);
        blocks
    }

    fn model_fragments() -> impl Strategy<Value = Vec<ModelFragment>> {
        prop::collection::vec(
            (
                -3..=3,
                "[a-z]{1,8}",
                prop::collection::vec("[a-z]{1,8}", 1..5),
            ),
            1..12,
        )
        .prop_map(|fragments| {
            fragments
                .into_iter()
                .enumerate()
                .map(|(index, (priority, stem, items))| ModelFragment {
                    path: PathBuf::from(format!("changes.d/{index:02}-{stem}.md")),
                    priority,
                    items,
                })
                .collect()
        })
    }

    fn write_model_fragments(root: &std::path::Path, fragments: &[ModelFragment]) {
        fs::create_dir_all(root.join("changes.d")).expect("fragments dir");
        for fragment in fragments {
            let mut source = String::new();
            if fragment.priority != 0 {
                source.push_str("---\npriority: ");
                source.push_str(&fragment.priority.to_string());
                source.push_str("\n---\n");
            }
            for item in &fragment.items {
                source.push_str(" -  Changed ");
                source.push_str(item);
                source.push_str(".\n");
            }
            fs::write(root.join(&fragment.path), source).expect("fragment");
        }
    }

    fn expected_item_order(fragments: &[ModelFragment]) -> Vec<(PathBuf, usize)> {
        let mut fragments = fragments.to_vec();
        fragments.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.items[0].cmp(&right.items[0]))
                .then_with(|| path_key(&left.path).cmp(&path_key(&right.path)))
        });
        fragments
            .into_iter()
            .flat_map(|fragment| {
                fragment
                    .items
                    .into_iter()
                    .enumerate()
                    .map(move |(ordinal, _)| (fragment.path.clone(), ordinal))
            })
            .collect()
    }

    fn compiled_item_order(region: &CompiledRegion) -> Vec<(PathBuf, usize)> {
        region
            .sections
            .iter()
            .flat_map(|section| {
                section
                    .items
                    .iter()
                    .map(|item| (item.fragment_path.clone(), item.ordinal))
            })
            .collect()
    }

    #[test]
    fn compiles_unreleased_heading_without_next_file() {
        let (_temp, repo) = repo_with_config("");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(compiled.version_label, VersionLabel::Unreleased);
        assert_eq!(
            compiled.markdown,
            "Unreleased\n----------\n\nTo be released.\n"
        );
    }

    #[test]
    fn compiles_version_heading_from_next_file() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), " 1.2.0 \n").expect("next");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            compiled.version_label,
            VersionLabel::Version("1.2.0".into())
        );
        assert!(
            compiled
                .markdown
                .starts_with("Version 1.2.0\n-------------\n")
        );
    }

    #[test]
    fn rejects_next_file_with_multiple_non_empty_lines() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "1.2.0\n1.3.0\n").expect("next");

        let error =
            compile_unreleased(&repo, CompileOptions::default()).expect_err("invalid next file");

        assert!(matches!(error, crate::Error::InvalidNextVersion { .. }));
    }

    #[test]
    fn treats_empty_next_file_as_unreleased() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/next"), "\n  \n").expect("next");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(compiled.version_label, VersionLabel::Unreleased);
        assert!(compiled.markdown.starts_with("Unreleased\n----------\n"));
    }

    #[test]
    fn excludes_scaffolds_from_substantive_item_count() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(temp.path().join("changes.d/scaffold.md"), " -  \n").expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(compiled.substantive_item_count, 0);
        assert_eq!(compiled.sections.len(), 1);
        assert!(
            compiled.markdown.ends_with("\n\n -\n"),
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn compiles_repository_without_sections() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/change.md"),
            " -  Fixed thing.  [[#2], [#2]]\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            compiled.markdown,
            "Unreleased\n----------\n\nTo be released.\n\n -  Fixed thing.  [[#2], [#2]]\n\n[#2]: https://example.com/issues/2\n"
        );
        assert_eq!(compiled.sections[0].id, None);
        assert_eq!(compiled.sections[0].references.len(), 1);
    }

    #[test]
    fn resolved_frontmatter_link_overrides_the_template() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/change.md"),
            "---\nlinks:\n  \"#2\": https://example.com/pull/2\n---\n -  Fixed thing.  [[#2]]\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled
                .markdown
                .contains("[#2]: https://example.com/pull/2")
        );
    }

    #[test]
    fn rejects_conflicting_resolved_links_across_fragments() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/one.md"),
            "---\nlinks:\n  \"#2\": https://example.com/pull/2\n---\n -  Fixed one.  [[#2]]\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("changes.d/two.md"),
            "---\nlinks:\n  \"#2\": https://example.net/pull/2\n---\n -  Fixed two.  [[#2]]\n",
        )
        .expect("fragment");

        let error = compile_unreleased(&repo, CompileOptions::default())
            .expect_err("conflicting link overrides");

        assert!(matches!(
            error,
            crate::Error::ConflictingResolvedLinks { ref label, .. } if label == "#2"
        ));
    }

    #[test]
    fn preserves_ascii_punctuation_in_compiled_markdown() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/punctuation.md"),
            " -  Added \"quoted\" 'single' can't wait... from 1---3.\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(compiled.markdown.contains("\"quoted\""));
        assert!(compiled.markdown.contains("'single'"));
        assert!(compiled.markdown.contains("can't"));
        assert!(compiled.markdown.contains("wait..."));
        assert!(compiled.markdown.contains("1---3"));
    }

    #[test]
    fn normalizes_reference_style_links_in_compiled_markdown() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/reference-link.md"),
            " -  Added the [migration guide][guide].\n\n[guide]: https://example.com/migration\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("changes.d/upgrade.md"),
            " -  Improved upgrade diagnostics.\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            compiled.markdown,
            "Unreleased\n----------\n\nTo be released.\n\n -  Added the [migration guide].\n -  Improved upgrade diagnostics.\n\n[migration guide]: https://example.com/migration\n"
        );
    }

    #[test]
    fn normalizes_escaped_reference_labels_from_the_resolved_ast() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/reference-link.md"),
            " -  Read [the guide][f\\]oo].\n\n[f\\]oo]: /guide\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled.markdown.contains("[the guide]"),
            "{}",
            compiled.markdown
        );
        assert!(
            compiled.markdown.contains("/guide"),
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn formats_ordinary_reference_definitions() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/reference-link.md"),
            " -  Read the [guide].\n\n[guide]:   https://example.com/guide\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled
                .markdown
                .contains("[guide]: https://example.com/guide")
        );
        assert!(!compiled.markdown.contains("[guide]:   "));
    }

    #[test]
    fn preserves_destinations_from_angle_bracket_reference_definitions() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/reference-links.md"),
            " -  Read [space] and [empty].\n\n[space]: <https://example.com/a b> \"A title\"\n[empty]: <>\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled
                .markdown
                .contains("[space]: https://example.com/a%20b \"A title\""),
            "{}",
            compiled.markdown
        );
        assert!(
            compiled.markdown.contains("[empty]()"),
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_distinct_targets_for_the_same_reference_label_across_fragments() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/one.md"),
            " -  Read [guide][shared].\n\n[shared]: https://example.com/first\n",
        )
        .expect("fragment");
        fs::write(
            temp.path().join("changes.d/two.md"),
            " -  Read [guide][shared].\n\n[shared]: https://example.com/second\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(compiled.markdown.contains("https://example.com/first"));
        assert!(compiled.markdown.contains("https://example.com/second"));
    }

    #[test]
    fn configured_references_ignore_markdown_definition_overrides() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/fix.md"),
            " -  Fixed [[#1]].\n\n[#1]: https://evil.example/override\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/issues/1")
        );
        assert!(!compiled.markdown.contains("evil.example"));
    }

    #[test]
    fn preserves_ordinary_reference_links_with_configured_looking_text() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Read [#1][migration].\n\n[migration]: https://docs.example/migration \"Migration guide\"\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(compiled.markdown.contains("https://docs.example/migration"));
        assert!(compiled.markdown.contains("Migration guide"));
        assert!(!compiled.markdown.contains("https://example.com/issues/1"));
    }

    #[test]
    fn coordinates_ordinary_reference_labels_across_compiled_sections() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        );
        for (section, url) in [
            ("core", "https://example.com/core"),
            ("cli", "https://example.com/cli"),
        ] {
            let directory = temp.path().join("changes.d").join(section);
            fs::create_dir_all(&directory).expect("section dir");
            fs::write(
                directory.join("guide.md"),
                format!(" -  Read the [guide]({url}).\n"),
            )
            .expect("fragment");
        }

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let links = rendered_links(&compiled.markdown);

        assert_eq!(
            links,
            vec![
                (
                    String::from("guide"),
                    String::from("https://example.com/core"),
                    String::new(),
                ),
                (
                    String::from("guide"),
                    String::from("https://example.com/cli"),
                    String::new(),
                ),
            ],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn reserves_configured_labels_across_compiled_sections() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "##,
        );
        for (section, source) in [
            (
                "core",
                " -  Read [#1][migration].\n\n[migration]: https://docs.example/migration\n",
            ),
            ("cli", " -  Fixed [[#1]].\n"),
        ] {
            let directory = temp.path().join("changes.d").join(section);
            fs::create_dir_all(&directory).expect("section dir");
            fs::write(directory.join("change.md"), source).expect("fragment");
        }

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let links = rendered_links(&compiled.markdown);

        assert_eq!(
            links,
            vec![
                (
                    String::from("#1"),
                    String::from("https://docs.example/migration"),
                    String::new(),
                ),
                (
                    String::from("#1"),
                    String::from("https://example.com/issues/1"),
                    String::new(),
                ),
            ],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_titles_on_ordinary_links_that_resemble_configured_references() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Read [#1][migration].\n\n[migration]: https://example.com/issues/1 \"Migration guide\"\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("#1"),
                String::from("https://example.com/issues/1"),
                String::from("Migration guide"),
            )],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_link_shaped_code_next_to_a_configured_reference() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Fixed [[#1]] and documented `[\\#1](https://example.com/issues/1)`.\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(
            compiled
                .markdown
                .contains("`[\\#1](https://example.com/issues/1)`"),
            "{}",
            compiled.markdown
        );
        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/issues/1")
        );
    }

    #[test]
    fn preserves_code_that_matches_a_configured_reference_definition() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Fixed [[#1]].\n\n    ~~~~ markdown\n    [#1]: https://example.com/issues/1\n    ~~~~\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_code_blocks(&compiled.markdown),
            vec![String::from("[#1]: https://example.com/issues/1\n")],
            "{}",
            compiled.markdown
        );
        assert_eq!(
            compiled
                .markdown
                .matches("[#1]: https://example.com/issues/1")
                .count(),
            2,
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_later_section_code_that_matches_an_earlier_definition() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "##,
        );
        for section in ["core", "cli"] {
            fs::create_dir_all(temp.path().join("changes.d").join(section)).expect("section dir");
        }
        fs::write(
            temp.path().join("changes.d/core/fix.md"),
            " -  Fixed [[#1]].\n",
        )
        .expect("core fragment");
        fs::write(
            temp.path().join("changes.d/cli/docs.md"),
            " -  Documented this example:\n\n    ~~~~ markdown\n    [#1]: https://example.com/issues/1\n    ~~~~\n",
        )
        .expect("cli fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_code_blocks(&compiled.markdown),
            vec![String::from("[#1]: https://example.com/issues/1\n")],
            "{}",
            compiled.markdown
        );
        assert_eq!(
            compiled
                .markdown
                .matches("[#1]: https://example.com/issues/1")
                .count(),
            2,
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn keeps_sectioned_configured_references_in_hongdown_normal_form() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"

            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "##,
        );
        for section in ["core", "cli"] {
            fs::create_dir_all(temp.path().join("changes.d").join(section)).expect("section dir");
        }
        fs::write(
            temp.path().join("changes.d/core/fix.md"),
            " -  Fixed [[#1]].\n",
        )
        .expect("core fragment");
        fs::write(
            temp.path().join("changes.d/cli/docs.md"),
            " -  Documented the command.\n",
        )
        .expect("cli fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let reformatted =
            format_markdown_with_word_wrap(&compiled.markdown, true).expect("reformat");

        assert_eq!(compiled.markdown, reformatted);
    }

    #[test]
    fn preserves_code_when_a_configured_destination_is_normalized() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}?a=1&amp;b=2"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Fixed [[#1]].\n\n    ~~~~ markdown\n    [#1]: https://example.com/issues/1?a=1&amp;b=2\n    ~~~~\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_code_blocks(&compiled.markdown),
            vec![String::from(
                "[#1]: https://example.com/issues/1?a=1&amp;b=2\n"
            )],
            "{}",
            compiled.markdown
        );
        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("#1"),
                String::from("https://example.com/issues/1?a=1&amp;b=2"),
                String::new(),
            )],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_greater_than_signs_in_pinned_destinations() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/fix.md"),
            "---\nlinks:\n  \"#1\": \"https://example.com/a>1\"\n---\n -  Fixed [[#1]].\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("#1"),
                String::from("https://example.com/a>1"),
                String::new(),
            )],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_hyphen_only_lines_with_trailing_spaces_in_code_blocks() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Documented this example:\n\n    ~~~~ markdown\n    -  \n    ~~~~\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_code_blocks(&compiled.markdown),
            vec![String::from("-  \n")],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn accepts_ordinary_links_with_unconfigured_reference_like_text() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/docs.md"),
            " -  Read [#123](https://example.com/issues/123).\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("#123"),
                String::from("https://example.com/issues/123"),
                String::new(),
            )],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_configured_full_reference_labels() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/fix.md"),
            " -  Fixed [the issue][#1].\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("the issue"),
                String::from("https://example.com/issues/1"),
                String::new(),
            )],
            "{}",
            compiled.markdown
        );
        assert!(
            compiled
                .markdown
                .contains("[#1]: https://example.com/issues/1"),
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_escaped_brackets_in_configured_full_reference_text() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/fix.md"),
            " -  Fixed [a \\] b][#1].\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            rendered_links(&compiled.markdown),
            vec![(
                String::from("a ] b"),
                String::from("https://example.com/issues/1"),
                String::new(),
            )],
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn preserves_more_than_ten_control_character_destinations() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        let mut fragment = String::from(" -  ");
        for index in 0..11 {
            if index > 0 {
                fragment.push_str(", ");
            }
            fragment.push_str(&format!("[link {index}][link {index}]"));
        }
        fragment.push_str(".\n\n");
        for index in 0..11 {
            fragment.push_str(&format!(
                "[link {index}]: <https://example.com/{index}&#9;x>\n"
            ));
        }
        fs::write(temp.path().join("changes.d/links.md"), fragment).expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let expected = (0..11)
            .map(|index| {
                (
                    format!("link {index}"),
                    format!("https://example.com/{index}\tx"),
                    String::new(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            rendered_links(&compiled.markdown),
            expected,
            "{}",
            compiled.markdown
        );
    }

    #[test]
    fn sorts_reference_metadata_by_link_config_order_then_number() {
        let (temp, repo) = repo_with_config(
            r##"
            [links]
            "!" = "https://example.com/pulls/{n}"
            "#" = "https://example.com/issues/{n}"
            "##,
        );
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/change.md"),
            " -  Fixed thing.  [[#2], [!10], [#1], [!2], [#2]]\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert_eq!(
            compiled.sections[0]
                .references
                .iter()
                .map(|reference| reference.label.as_str())
                .collect::<Vec<_>>(),
            vec!["!2", "!10", "#1", "#2"]
        );
        assert_eq!(compiled.markdown.matches("[#2]:").count(), 1);
        assert_eq!(
            compiled.markdown,
            format_markdown_with_word_wrap(&compiled.markdown, true).expect("reformat")
        );
    }

    #[test]
    fn compiles_configured_and_unknown_sections_in_order() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "Core"
            directory = "core"

            [[sections]]
            id = "Extra"
            directory = "extra"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::create_dir_all(temp.path().join("changes.d/unknown")).expect("unknown dir");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            " -  Fixed core.\n",
        )
        .expect("core");
        fs::write(
            temp.path().join("changes.d/unknown/change.md"),
            " -  Fixed unknown.\n",
        )
        .expect("unknown");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        assert!(compiled.markdown.contains("### Core\n\n -  Fixed core."));
        assert!(
            compiled
                .markdown
                .contains("### unknown\n\n -  Fixed unknown.")
        );
        assert!(
            compiled.markdown.find("### Core").expect("core")
                < compiled.markdown.find("### unknown").expect("unknown")
        );
        assert!(!compiled.markdown.contains("### Extra"));
    }

    #[test]
    fn compiles_patterned_sections_in_declaration_and_id_order() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "Workspace"
            directory = "workspace"

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "pkg-{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "tool-{name}"
            "#,
        );
        for (directory, text) in [
            ("workspace", "Workspace"),
            ("pkg-z", "Package z"),
            ("pkg-a", "Package a"),
            ("tool-a", "Tool a"),
        ] {
            fs::create_dir_all(temp.path().join("changes.d").join(directory)).expect("section dir");
            fs::write(
                temp.path()
                    .join("changes.d")
                    .join(directory)
                    .join("change.md"),
                format!(" -  Changed {text}.\n"),
            )
            .expect("fragment");
        }

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
        let headings = ["### Workspace", "### pkg/a", "### pkg/z", "### tool/a"];
        let positions = headings.map(|heading| compiled.markdown.find(heading).expect("heading"));

        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn compiles_requested_unknown_section_discovered_on_disk() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "Core"
            directory = "core"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::create_dir_all(temp.path().join("changes.d/experimental")).expect("experimental dir");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            " -  Fixed core.\n",
        )
        .expect("core");
        fs::write(
            temp.path().join("changes.d/experimental/change.md"),
            " -  Added experiment.\n",
        )
        .expect("experimental");

        let compiled = compile_unreleased(
            &repo,
            CompileOptions {
                section: Some("experimental".into()),
                ..CompileOptions::default()
            },
        )
        .expect("compile");

        assert!(!compiled.markdown.contains("### Core"));
        assert!(
            compiled
                .markdown
                .contains("### experimental\n\n -  Added experiment.")
        );
    }

    #[test]
    fn compiles_only_requested_section() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "Core"
            directory = "core"

            [[sections]]
            id = "Cli"
            directory = "cli"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::create_dir_all(temp.path().join("changes.d/cli")).expect("cli dir");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            " -  Fixed core.\n",
        )
        .expect("core");
        fs::write(
            temp.path().join("changes.d/cli/change.md"),
            " -  Fixed cli.\n",
        )
        .expect("cli");

        let compiled = compile_unreleased(
            &repo,
            CompileOptions {
                section: Some("Cli".into()),
                ..CompileOptions::default()
            },
        )
        .expect("compile");

        assert!(!compiled.markdown.contains("Core"));
        assert!(compiled.markdown.contains("### Cli"));
    }

    #[test]
    fn rejects_section_directory_name_when_id_differs() {
        let (temp, repo) = repo_with_config(
            r#"
            [[sections]]
            id = "@pkg/core"
            directory = "core"
            "#,
        );
        fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
        fs::write(
            temp.path().join("changes.d/core/change.md"),
            " -  Fixed core.\n",
        )
        .expect("core");

        let error = compile_unreleased(
            &repo,
            CompileOptions {
                section: Some("core".into()),
                ..CompileOptions::default()
            },
        )
        .expect_err("directory name is not a section id");

        assert!(matches!(error, crate::Error::UnknownSection { .. }));
    }

    #[test]
    fn rejects_unknown_requested_section() {
        let (_temp, repo) = repo_with_config("");

        let error = compile_unreleased(
            &repo,
            CompileOptions {
                section: Some("missing".into()),
                ..CompileOptions::default()
            },
        )
        .expect_err("unknown section");

        assert!(matches!(error, crate::Error::UnknownSection { .. }));
    }

    #[test]
    fn sorts_items_deterministically() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/z.md"),
            "---\npriority: 10\n---\n -  Added z.\n",
        )
        .expect("z");
        fs::write(
            temp.path().join("changes.d/a.md"),
            " -  Removed a.\n -  Added a.\n",
        )
        .expect("a");
        fs::write(temp.path().join("changes.d/m.md"), " -  Fixed m.\n").expect("m");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        let removed = compiled.markdown.find("Removed a.").expect("removed");
        let added_a = compiled.markdown.find("Added a.").expect("added a");
        let fixed = compiled.markdown.find("Fixed m.").expect("fixed");
        let added_z = compiled.markdown.find("Added z.").expect("z");
        assert!(fixed < removed);
        assert!(removed < added_a);
        assert!(added_a < added_z);
    }

    #[test]
    fn preserves_fragment_item_order_even_when_sort_text_differs() {
        let (temp, repo) = repo_with_config("");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        fs::write(
            temp.path().join("changes.d/change.md"),
            " -  Removed a.\n -  Added a.\n",
        )
        .expect("fragment");

        let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

        let removed = compiled.markdown.find("Removed a.").expect("removed");
        let added = compiled.markdown.find("Added a.").expect("added");
        assert!(removed < added);
    }

    #[test]
    fn ignores_filesystem_creation_order() {
        let (left_temp, left_repo) = repo_with_config("");
        let (right_temp, right_repo) = repo_with_config("");
        fs::create_dir_all(left_temp.path().join("changes.d")).expect("left fragments dir");
        fs::create_dir_all(right_temp.path().join("changes.d")).expect("right fragments dir");
        let fragments = [
            ("z.md", " -  Fixed z.\n"),
            ("a.md", " -  Added a.\n"),
            ("m.md", " -  Changed m.\n"),
        ];

        for (name, source) in fragments {
            fs::write(left_temp.path().join("changes.d").join(name), source).expect("left");
        }
        for (name, source) in fragments.into_iter().rev() {
            fs::write(right_temp.path().join("changes.d").join(name), source).expect("right");
        }

        let left = compile_unreleased(&left_repo, CompileOptions::default()).expect("left");
        let right = compile_unreleased(&right_repo, CompileOptions::default()).expect("right");

        assert_eq!(left.markdown, right.markdown);
    }

    proptest! {
        #[test]
        fn compiled_item_order_matches_sorting_model(fragments in model_fragments()) {
            let (temp, repo) = repo_with_config("");
            write_model_fragments(temp.path(), &fragments);

            let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");

            prop_assert_eq!(compiled_item_order(&compiled), expected_item_order(&fragments));
        }

        #[test]
        fn output_does_not_depend_on_fragment_creation_order(fragments in model_fragments()) {
            let (left_temp, left_repo) = repo_with_config("");
            write_model_fragments(left_temp.path(), &fragments);

            let (right_temp, right_repo) = repo_with_config("");
            let mut reversed = fragments.clone();
            reversed.reverse();
            write_model_fragments(right_temp.path(), &reversed);

            let left = compile_unreleased(&left_repo, CompileOptions::default()).expect("left");
            let right = compile_unreleased(&right_repo, CompileOptions::default()).expect("right");

            prop_assert_eq!(left.markdown, right.markdown);
        }

        #[test]
        fn reference_definitions_are_deduplicated_and_sorted(numbers in prop::collection::vec(1_u64..100, 1..25)) {
            let (temp, repo) = repo_with_config(
                r##"
                [links]
                "#" = "https://example.com/issues/{n}"
                "##,
            );
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            let labels = numbers
                .iter()
                .map(|number| format!("[#{number}]"))
                .collect::<Vec<_>>()
                .join(", ");
            fs::write(
                temp.path().join("changes.d/change.md"),
                format!(" -  Fixed references.  [{labels}]\n"),
            )
            .expect("fragment");

            let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
            let mut expected = numbers;
            expected.sort();
            expected.dedup();
            let expected = expected
                .into_iter()
                .map(|number| CompiledReference {
                    label: format!("#{number}"),
                    url: format!("https://example.com/issues/{number}"),
                })
                .collect::<Vec<_>>();

            prop_assert_eq!(&compiled.sections[0].references, &expected);
        }

        #[test]
        fn section_order_follows_configuration(core in "[a-z]{1,8}", cli in "[a-z]{1,8}", core_first in any::<bool>()) {
            let config = if core_first {
                r#"
                [[sections]]
                id = "Core"
                directory = "core"

                [[sections]]
                id = "Cli"
                directory = "cli"
                "#
            } else {
                r#"
                [[sections]]
                id = "Cli"
                directory = "cli"

                [[sections]]
                id = "Core"
                directory = "core"
                "#
            };
            let (temp, repo) = repo_with_config(config);
            fs::create_dir_all(temp.path().join("changes.d/core")).expect("core dir");
            fs::create_dir_all(temp.path().join("changes.d/cli")).expect("cli dir");
            fs::write(
                temp.path().join("changes.d/core/change.md"),
                format!(" -  Changed {core}.\n"),
            )
            .expect("core");
            fs::write(
                temp.path().join("changes.d/cli/change.md"),
                format!(" -  Changed {cli}.\n"),
            )
            .expect("cli");

            let compiled = compile_unreleased(&repo, CompileOptions::default()).expect("compile");
            let section_ids = compiled
                .sections
                .iter()
                .map(|section| section.id.as_deref())
                .collect::<Vec<_>>();
            let expected = if core_first {
                vec![Some("Core"), Some("Cli")]
            } else {
                vec![Some("Cli"), Some("Core")]
            };

            prop_assert_eq!(section_ids, expected);
        }

        #[test]
        fn compilation_is_deterministic_for_repeated_input(names in prop::collection::vec("[a-z]{1,8}", 1..8)) {
            let (temp, repo) = repo_with_config("");
            fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
            for (index, name) in names.iter().enumerate() {
                fs::write(
                    temp.path().join(format!("changes.d/{name}-{index}.md")),
                    format!(" -  Fixed {name} {index}.\n"),
                )
                .expect("fragment");
            }

            let first = compile_unreleased(&repo, CompileOptions::default()).expect("first");
            let second = compile_unreleased(&repo, CompileOptions::default()).expect("second");

            prop_assert_eq!(first.markdown, second.markdown);
        }
    }
}
