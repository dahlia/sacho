use std::fmt;
use std::hash::Hash;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Result};

/// Parsed Sacho configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// Changelog file and unreleased-region settings.
    #[serde(default)]
    pub changelog: ChangelogConfig,

    /// Fragment directory settings.
    #[serde(default)]
    pub fragments: FragmentsConfig,

    /// Reference-link URL templates keyed by sigil.
    #[serde(default)]
    pub links: IndexMap<ReferenceSigil, UrlTemplate>,

    /// HTTP redirect resolution settings for reference links.
    #[serde(default)]
    pub link_resolution: LinkResolutionConfig,

    /// Version-control integration settings.
    #[serde(default)]
    pub vcs: VcsConfig,

    /// Check command settings.
    #[serde(default)]
    pub check: CheckConfig,

    /// Ordered section configuration.
    #[serde(default)]
    pub sections: Vec<SectionConfig>,
}

impl Config {
    /// Parses and validates a TOML configuration string.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(input).map_err(|source| ConfigError::Parse { source })?;
        config.validate()?;
        Ok(config)
    }

    /// Validates semantic configuration constraints that TOML cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_config_path("changelog.path", &self.changelog.path)?;
        validate_config_path("fragments.directory", &self.fragments.directory)?;
        validate_config_path("fragments.next-file", &self.fragments.next_file)?;
        if self
            .fragments
            .next_file
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            return Err(ConfigError::InvalidPath {
                key: String::from("fragments.next-file"),
                path: self.fragments.next_file.clone(),
                reason: "must not name a Markdown fragment",
            });
        }

        for (sigil, template) in &self.links {
            if !template.as_str().contains("{n}") {
                return Err(ConfigError::LinkTemplateMissingNumber {
                    sigil: sigil.to_string(),
                });
            }
        }
        let mut section_ids = Vec::<(&str, String)>::new();
        let mut section_directories = Vec::<(PathBuf, String)>::new();
        for (index, section) in self.sections.iter().enumerate() {
            let id_key = format!("sections[{index}].id");
            if section.id.trim().is_empty() {
                return Err(ConfigError::EmptySectionId { key: id_key });
            }
            if let Some((_, first_key)) = section_ids
                .iter()
                .find(|(configured, _)| *configured == section.id)
            {
                return Err(ConfigError::DuplicateSectionId {
                    id: section.id.clone(),
                    first_key: first_key.clone(),
                    second_key: id_key,
                });
            }
            section_ids.push((&section.id, id_key));

            let directory_key = format!("sections[{index}].directory");
            let comparison_path = validate_config_path(&directory_key, &section.directory)?;
            if let Some((_, first_key)) = section_directories
                .iter()
                .find(|(configured, _)| *configured == comparison_path)
            {
                return Err(ConfigError::DuplicateSectionDirectory {
                    path: section.directory.clone(),
                    first_key: first_key.clone(),
                    second_key: directory_key,
                });
            }
            section_directories.push((comparison_path, directory_key));
        }
        self.vcs.validate()?;
        Ok(())
    }
}

fn validate_config_path(key: &str, path: &Path) -> Result<PathBuf, ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(ConfigError::InvalidPath {
            key: key.to_owned(),
            path: path.to_path_buf(),
            reason: "must not be empty",
        });
    }

    let mut comparison_path = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(ConfigError::InvalidPath {
                    key: key.to_owned(),
                    path: path.to_path_buf(),
                    reason: "must be relative to its anchor",
                });
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !comparison_path.pop() {
                    return Err(ConfigError::InvalidPath {
                        key: key.to_owned(),
                        path: path.to_path_buf(),
                        reason: "escapes its anchor",
                    });
                }
            }
            Component::Normal(name) => {
                comparison_path.push(name);
            }
        }
    }
    if comparison_path.as_os_str().is_empty() {
        return Err(ConfigError::InvalidPath {
            key: key.to_owned(),
            path: path.to_path_buf(),
            reason: "must not resolve to its anchor",
        });
    }
    Ok(comparison_path)
}

/// Changelog file configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ChangelogConfig {
    /// Path to the rendered changelog file.
    pub path: PathBuf,

    /// Top-level changelog title.
    pub title: String,

    /// Date line used for the unreleased region.
    pub unreleased_heading: String,

    /// Whether the unreleased region is kept in the changelog file.
    pub materialize: bool,

    /// Strategy for finding the unreleased region.
    pub region_detection: RegionDetection,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("CHANGES.md"),
            title: String::from("Changelog"),
            unreleased_heading: String::from("To be released."),
            materialize: true,
            region_detection: RegionDetection::Heading,
        }
    }
}

/// Strategy for locating the unreleased region in a changelog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionDetection {
    /// Infer the region from Sacho's rendered heading and unreleased date line.
    Heading,

    /// Use explicit HTML marker comments around the region.
    Marker,
}

/// Fragment directory configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FragmentsConfig {
    /// Directory containing unreleased fragments.
    pub directory: PathBuf,

    /// File, relative to the fragment directory, containing the next version.
    pub next_file: PathBuf,
}

impl Default for FragmentsConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("changes.d"),
            next_file: PathBuf::from("next"),
        }
    }
}

/// Reference sigil used in shortcut links, such as `#`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ReferenceSigil(String);

impl ReferenceSigil {
    /// Creates a reference sigil from a string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the sigil as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReferenceSigil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ReferenceSigil {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::new(value))
    }
}

/// URL template for resolving numeric references.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct UrlTemplate(String);

impl UrlTemplate {
    /// Creates a URL template.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the template as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// HTTP redirect resolution settings for reference links.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LinkResolutionConfig {
    /// Whether supported commands resolve unpinned links by default.
    pub enabled: bool,
}

/// Version-control integration configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VcsConfig {
    /// Selected VCS preset.
    pub preset: VcsPreset,

    /// Per-query command overrides layered on top of the selected preset.
    #[serde(default)]
    pub commands: VcsCommandOverrides,
}

impl Default for VcsConfig {
    fn default() -> Self {
        Self {
            preset: VcsPreset::Git,
            commands: VcsCommandOverrides::default(),
        }
    }
}

impl VcsConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.preset == VcsPreset::None && !self.commands.is_empty() {
            return Err(ConfigError::VcsCommandsWithNonePreset);
        }
        for (query, command) in self.commands.iter() {
            command.validate(query, self.preset)?;
        }
        Ok(())
    }
}

/// A VCS command represented as a program followed by literal arguments.
///
/// Commands are executed directly without a shell. Supported placeholders are
/// expanded within arguments, but never within the program name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct VcsCommand(Vec<String>);

impl VcsCommand {
    /// Creates a command from a program and its arguments.
    pub fn new(argv: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(argv.into_iter().map(Into::into).collect())
    }

    /// Returns the program and arguments in configured order.
    pub fn argv(&self) -> &[String] {
        &self.0
    }

    fn validate(&self, query: VcsQuery, preset: VcsPreset) -> Result<(), ConfigError> {
        let Some(program) = self.0.first() else {
            return Err(ConfigError::EmptyVcsCommand { query });
        };
        if program.is_empty() {
            return Err(ConfigError::EmptyVcsProgram { query });
        }
        if program.contains("${") {
            return Err(ConfigError::VcsPlaceholderInProgram { query });
        }

        let mut found = Vec::new();
        for argument in &self.0[1..] {
            let mut rest = argument.as_str();
            while let Some(start) = rest.find("${") {
                let tail = &rest[start + 2..];
                let Some((placeholder, suffix)) = tail.split_once('}') else {
                    return Err(ConfigError::MalformedVcsPlaceholder { query });
                };
                if !query.allowed_placeholders(preset).contains(&placeholder) {
                    return Err(ConfigError::UnknownVcsPlaceholder {
                        query,
                        placeholder: placeholder.to_owned(),
                    });
                }
                found.push(placeholder);
                rest = suffix;
            }
        }
        for required in query.required_placeholders(preset) {
            if !found.contains(required) {
                return Err(ConfigError::MissingVcsPlaceholder {
                    query,
                    placeholder: (*required).to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Optional command overrides for the three VCS queries Sacho performs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VcsCommandOverrides {
    /// Command that lists commits after `${base}`, oldest first.
    pub commits: Option<VcsCommand>,

    /// Command that emits NUL-delimited changed-path records for `${commit}`.
    pub changed_paths: Option<VcsCommand>,

    /// Command that emits the raw message for `${commit}`.
    pub message: Option<VcsCommand>,
}

impl VcsCommandOverrides {
    /// Returns true when no query command is overridden.
    pub fn is_empty(&self) -> bool {
        self.commits.is_none() && self.changed_paths.is_none() && self.message.is_none()
    }

    fn iter(&self) -> impl Iterator<Item = (VcsQuery, &VcsCommand)> {
        [
            (VcsQuery::Commits, self.commits.as_ref()),
            (VcsQuery::ChangedPaths, self.changed_paths.as_ref()),
            (VcsQuery::Message, self.message.as_ref()),
        ]
        .into_iter()
        .filter_map(|(query, command)| command.map(|command| (query, command)))
    }
}

/// A configurable VCS query performed by Sacho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsQuery {
    /// List commits between the configured base and the working revision.
    Commits,
    /// List paths changed by one commit.
    ChangedPaths,
    /// Read one commit's raw message.
    Message,
}

impl VcsQuery {
    fn allowed_placeholders(self, _preset: VcsPreset) -> &'static [&'static str] {
        match self {
            Self::Commits => &["base"],
            Self::ChangedPaths | Self::Message => &["commit"],
        }
    }

    fn required_placeholders(self, preset: VcsPreset) -> &'static [&'static str] {
        self.allowed_placeholders(preset)
    }
}

impl fmt::Display for VcsQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Commits => "commits",
            Self::ChangedPaths => "changed-paths",
            Self::Message => "message",
        })
    }
}

/// Built-in VCS integration presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VcsPreset {
    /// Git preset.
    Git,

    /// Jujutsu preset.
    Jj,

    /// Mercurial preset.
    Hg,

    /// No VCS integration.
    None,
}

/// Check command configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CheckConfig {
    /// Paths whose changes require changelog fragments.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Section configuration for repositories with multiple changelog sections.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SectionConfig {
    /// Rendered section identifier.
    pub id: String,

    /// Fragment subdirectory for this section.
    pub directory: PathBuf,

    /// Paths whose changes are attributed to this section.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn safe_text() -> impl Strategy<Value = String> {
        "[A-Za-z0-9][A-Za-z0-9_-]{0,12}"
    }

    fn safe_path() -> impl Strategy<Value = PathBuf> {
        prop::collection::vec("[a-z][a-z0-9_-]{0,8}", 1..4)
            .prop_map(|segments| segments.into_iter().collect())
    }

    fn config_strategy() -> impl Strategy<Value = Config> {
        (
            safe_path(),
            safe_path(),
            prop::collection::vec((safe_text(), safe_path()), 0..8),
            prop::collection::vec(("[!#$%&*+./:=?@^|~-]{1,3}", "[a-z]{1,8}"), 0..8),
        )
            .prop_map(|(changelog_path, fragment_dir, sections, links)| {
                let mut link_map = IndexMap::new();
                for (sigil, stem) in links {
                    link_map.insert(
                        ReferenceSigil::new(sigil),
                        UrlTemplate::new(format!("https://example.com/{stem}/{{n}}")),
                    );
                }

                let mut unique_sections = Vec::<SectionConfig>::new();
                for (id, directory) in sections {
                    if unique_sections
                        .iter()
                        .any(|section| section.id == id || section.directory == directory)
                    {
                        continue;
                    }
                    unique_sections.push(SectionConfig {
                        id,
                        directory,
                        paths: Vec::new(),
                    });
                }

                Config {
                    changelog: ChangelogConfig {
                        path: changelog_path,
                        ..ChangelogConfig::default()
                    },
                    fragments: FragmentsConfig {
                        directory: fragment_dir,
                        ..FragmentsConfig::default()
                    },
                    links: link_map,
                    link_resolution: LinkResolutionConfig::default(),
                    vcs: VcsConfig::default(),
                    check: CheckConfig::default(),
                    sections: unique_sections,
                }
            })
    }

    #[test]
    fn fills_documented_defaults() {
        let config = Config::parse("").expect("empty config should use defaults");

        assert_eq!(config.changelog.path, PathBuf::from("CHANGES.md"));
        assert_eq!(config.fragments.directory, PathBuf::from("changes.d"));
        assert!(!config.link_resolution.enabled);
        assert_eq!(config.vcs.preset, VcsPreset::Git);
        assert!(config.vcs.commands.is_empty());
    }

    #[test]
    fn parses_link_resolution_policy_without_changing_link_templates() {
        let config = Config::parse(
            r##"
            [links]
            "#" = "https://example.com/issues/{n}"

            [link-resolution]
            enabled = true
            "##,
        )
        .expect("link resolution config");

        assert!(config.link_resolution.enabled);
        assert_eq!(
            config.links[&ReferenceSigil::new("#")].as_str(),
            "https://example.com/issues/{n}"
        );
    }

    #[test]
    fn parses_partial_vcs_command_overrides() {
        let config = Config::parse(
            r#"
            [vcs]
            preset = "jj"

            [vcs.commands]
            message = ["my-jj", "show", "${commit}"]
            "#,
        )
        .expect("valid command override");

        assert_eq!(
            config.vcs.commands.message,
            Some(VcsCommand::new(["my-jj", "show", "${commit}"]))
        );
        assert!(config.vcs.commands.commits.is_none());
    }

    #[test]
    fn rejects_commands_for_none_preset() {
        let error = Config::parse(
            r#"
            [vcs]
            preset = "none"
            [vcs.commands]
            commits = ["vcs", "${base}"]
            "#,
        )
        .expect_err("disabled VCS must reject command overrides");

        assert!(matches!(error, ConfigError::VcsCommandsWithNonePreset));
    }

    #[test]
    fn validates_query_placeholders() {
        let cases = [
            (
                "[vcs.commands]\ncommits = [\"vcs\", \"log\"]\n",
                "must contain placeholder ${base}",
            ),
            (
                "[vcs.commands]\nmessage = [\"${commit}\"]\n",
                "placeholders are not allowed in the program name",
            ),
            (
                "[vcs.commands]\nmessage = [\"vcs\", \"${base}\"]\n",
                "does not support placeholder ${base}",
            ),
            (
                "[vcs.commands]\nmessage = [\"vcs\", \"${commit\"]\n",
                "contains an unclosed placeholder",
            ),
        ];

        for (input, expected) in cases {
            let error = Config::parse(input).expect_err("invalid placeholder contract");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn mercurial_changed_paths_override_uses_only_the_commit() {
        let config = Config::parse(
            r#"
            [vcs]
            preset = "hg"
            [vcs.commands]
            changed-paths = ["custom-hg-query", "changed-paths", "${commit}"]
            "#,
        )
        .expect("self-contained Mercurial query");

        assert_eq!(
            config.vcs.commands.changed_paths.expect("override").argv(),
            ["custom-hg-query", "changed-paths", "${commit}"]
        );
    }

    #[test]
    fn mercurial_changed_paths_rejects_the_internal_parent_placeholder() {
        let error = Config::parse(
            r#"
            [vcs]
            preset = "hg"
            [vcs.commands]
            changed-paths = ["custom-hg-query", "${commit}", "${parent}"]
            "#,
        )
        .expect_err("override cannot receive an internally discovered parent");

        assert!(
            error
                .to_string()
                .contains("does not support placeholder ${parent}")
        );
    }

    #[test]
    fn rejects_link_templates_without_number_placeholder() {
        let error = Config::parse("[links]\n\"#\" = \"https://example.com/issues\"\n")
            .expect_err("template without {n} should be rejected");

        assert!(matches!(
            error,
            ConfigError::LinkTemplateMissingNumber { .. }
        ));
    }

    #[test]
    fn rejects_paths_that_are_absolute_empty_or_escape_their_anchor() {
        let cases = [
            (
                "[changelog]\npath = \"\"\n",
                "changelog.path",
                "must not be empty",
            ),
            (
                "[changelog]\npath = \"/outside/CHANGES.md\"\n",
                "changelog.path",
                "must be relative",
            ),
            (
                "[changelog]\npath = \"../outside/CHANGES.md\"\n",
                "changelog.path",
                "escapes its anchor",
            ),
            (
                "[fragments]\ndirectory = \"/outside\"\n",
                "fragments.directory",
                "must be relative",
            ),
            (
                "[fragments]\ndirectory = \"../outside\"\n",
                "fragments.directory",
                "escapes its anchor",
            ),
            (
                "[fragments]\nnext-file = \"/outside\"\n",
                "fragments.next-file",
                "must be relative",
            ),
            (
                "[fragments]\nnext-file = \"state/../..\"\n",
                "fragments.next-file",
                "escapes its anchor",
            ),
            (
                "[[sections]]\nid = \"core\"\ndirectory = \"/outside\"\n",
                "sections[0].directory",
                "must be relative",
            ),
            (
                "[[sections]]\nid = \"core\"\ndirectory = \"../outside\"\n",
                "sections[0].directory",
                "escapes its anchor",
            ),
            (
                "[[sections]]\nid = \"core\"\ndirectory = \"nested/..\"\n",
                "sections[0].directory",
                "must not resolve to its anchor",
            ),
        ];

        for (input, key, reason) in cases {
            let error = Config::parse(input).expect_err("invalid configured path");
            let message = error.to_string();
            assert!(message.contains(key), "{message}");
            assert!(message.contains(reason), "{message}");
        }
    }

    #[test]
    fn accepts_safe_parent_components_within_each_path_anchor() {
        let config = Config::parse(
            r#"
            [changelog]
            path = "docs/archive/../CHANGES.md"

            [fragments]
            directory = "state/archive/../changes.d"
            next-file = "metadata/archive/../next"

            [[sections]]
            id = "core"
            directory = "packages/archive/../core"
            "#,
        )
        .expect("parents that remain below their anchor are valid");

        assert_eq!(
            config.changelog.path,
            PathBuf::from("docs/archive/../CHANGES.md")
        );
    }

    #[test]
    fn rejects_markdown_next_file_before_fragment_discovery() {
        let error = Config::parse("[fragments]\nnext-file = \"metadata/next.md\"\n")
            .expect_err("Markdown next-version path");
        let message = error.to_string();

        assert!(message.contains("fragments.next-file"), "{message}");
        assert!(message.contains("next.md"), "{message}");
        assert!(message.contains("Markdown fragment"), "{message}");
    }

    #[test]
    fn rejects_empty_and_duplicate_section_identifiers() {
        let empty = Config::parse("[[sections]]\nid = \"  \"\ndirectory = \"core\"\n")
            .expect_err("blank section id");
        assert!(empty.to_string().contains("sections[0].id"));

        let duplicate = Config::parse(
            r#"
            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "core"
            directory = "other"
            "#,
        )
        .expect_err("duplicate section id");
        let message = duplicate.to_string();
        assert!(message.contains("sections[0].id"), "{message}");
        assert!(message.contains("sections[1].id"), "{message}");
        assert!(message.contains("core"), "{message}");
    }

    #[test]
    fn rejects_statically_duplicate_section_directories_but_allows_nesting() {
        let duplicate = Config::parse(
            r#"
            [[sections]]
            id = "core"
            directory = "packages/./core"

            [[sections]]
            id = "other"
            directory = "packages/core"
            "#,
        )
        .expect_err("duplicate section directory");
        let message = duplicate.to_string();
        assert!(message.contains("sections[0].directory"), "{message}");
        assert!(message.contains("sections[1].directory"), "{message}");

        let parent_alias = Config::parse(
            r#"
            [[sections]]
            id = "core"
            directory = "packages/archive/../core"

            [[sections]]
            id = "other"
            directory = "packages/core"
            "#,
        )
        .expect_err("lexically equivalent section directory");
        assert!(matches!(
            parent_alias,
            ConfigError::DuplicateSectionDirectory { .. }
        ));

        Config::parse(
            r#"
            [[sections]]
            id = "packages"
            directory = "packages"

            [[sections]]
            id = "core"
            directory = "packages/core"
            "#,
        )
        .expect("nested section directories are valid");
    }

    #[test]
    fn preserves_defaults_for_partial_nested_tables() {
        let config = Config::parse(
            r#"
            [changelog]
            path = "CHANGELOG.md"

            [fragments]
            directory = "news.d"
            "#,
        )
        .expect("partial nested tables should use defaults");

        assert_eq!(config.changelog.path, PathBuf::from("CHANGELOG.md"));
        assert_eq!(config.changelog.title, "Changelog");
        assert_eq!(config.changelog.unreleased_heading, "To be released.");
        assert!(config.changelog.materialize);
        assert_eq!(config.changelog.region_detection, RegionDetection::Heading);
        assert_eq!(config.fragments.directory, PathBuf::from("news.d"));
        assert_eq!(config.fragments.next_file, PathBuf::from("next"));
    }

    #[test]
    fn preserves_link_template_order() {
        let config = Config::parse(
            r##"
            [links]
            "!" = "https://example.com/pulls/{n}"
            "#" = "https://example.com/issues/{n}"
            "##,
        )
        .expect("config");

        assert_eq!(
            config
                .links
                .keys()
                .map(ReferenceSigil::as_str)
                .collect::<Vec<_>>(),
            vec!["!", "#"]
        );
    }

    proptest! {
        #[test]
        fn serialized_valid_configs_parse_back_to_same_value(config in config_strategy()) {
            let toml = toml::to_string(&config).expect("serialize config");

            let parsed = Config::parse(&toml).expect("serialized config should parse");

            prop_assert_eq!(parsed, config);
        }

        #[test]
        fn validates_any_link_template_containing_number_placeholder(
            prefix in "[A-Za-z0-9/:._?=&-]{0,24}",
            suffix in "[A-Za-z0-9/:._?=&-]{0,24}",
        ) {
            let mut config = Config::parse("").expect("default config");
            config.links.insert(
                ReferenceSigil::new("#"),
                UrlTemplate::new(format!("{prefix}{{n}}{suffix}")),
            );

            prop_assert!(config.validate().is_ok());
        }

        #[test]
        fn rejects_any_link_template_missing_number_placeholder(
            template in "[A-Za-z0-9/:._?=&-]{0,48}",
        ) {
            prop_assume!(!template.contains("{n}"));
            let mut config = Config::parse("").expect("default config");
            config
                .links
                .insert(ReferenceSigil::new("#"), UrlTemplate::new(template));

            let error = config.validate().expect_err("missing placeholder");
            let is_missing_placeholder = matches!(
                error,
                ConfigError::LinkTemplateMissingNumber { .. }
            );

            prop_assert!(is_missing_placeholder);
        }

        #[test]
        fn accepts_generated_parent_components_that_cancel_below_the_anchor(
            prefix in prop::collection::vec("[a-z][a-z0-9_-]{0,8}", 0..4),
            terminal in "[a-z][a-z0-9_-]{0,8}",
            suffix in prop::collection::vec("[a-z][a-z0-9_-]{0,8}", 0..4),
        ) {
            let mut path = PathBuf::new();
            path.extend(prefix);
            path.push("detour");
            path.push("..");
            path.push(terminal);
            path.extend(suffix);

            prop_assert!(validate_config_path("generated.path", &path).is_ok());
        }

        #[test]
        fn rejects_generated_parent_components_that_underflow_the_anchor(
            parent_count in 1_usize..8,
            suffix in prop::collection::vec("[a-z][a-z0-9_-]{0,8}", 0..4),
        ) {
            let mut path = PathBuf::new();
            for _ in 0..parent_count {
                path.push("..");
            }
            path.extend(suffix);

            let error = validate_config_path("generated.path", &path)
                .expect_err("parent path must underflow its anchor");
            let invalid_path = matches!(error, ConfigError::InvalidPath { .. });
            prop_assert!(invalid_path);
        }
    }
}
