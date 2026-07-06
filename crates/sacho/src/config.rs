use std::fmt;
use std::hash::Hash;
use std::path::PathBuf;
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
        for (sigil, template) in &self.links {
            if !template.as_str().contains("{n}") {
                return Err(ConfigError::LinkTemplateMissingNumber {
                    sigil: sigil.to_string(),
                });
            }
        }
        for section in &self.sections {
            if section.directory.as_os_str().is_empty() {
                return Err(ConfigError::EmptySectionDirectory {
                    section: section.id.clone(),
                });
            }
        }
        Ok(())
    }
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

/// Version-control integration configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VcsConfig {
    /// Selected VCS preset.
    pub preset: VcsPreset,
}

impl Default for VcsConfig {
    fn default() -> Self {
        Self {
            preset: VcsPreset::Git,
        }
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
                    vcs: VcsConfig::default(),
                    check: CheckConfig::default(),
                    sections: sections
                        .into_iter()
                        .map(|(id, directory)| SectionConfig {
                            id,
                            directory,
                            paths: Vec::new(),
                        })
                        .collect(),
                }
            })
    }

    #[test]
    fn fills_documented_defaults() {
        let config = Config::parse("").expect("empty config should use defaults");

        assert_eq!(config.changelog.path, PathBuf::from("CHANGES.md"));
        assert_eq!(config.fragments.directory, PathBuf::from("changes.d"));
        assert_eq!(config.vcs.preset, VcsPreset::Git);
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
    }
}
