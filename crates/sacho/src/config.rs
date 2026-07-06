use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

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
    pub links: BTreeMap<ReferenceSigil, UrlTemplate>,

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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ReferenceSigil(String);

impl ReferenceSigil {
    /// Creates a reference sigil from a string.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
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
    use super::*;

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
}
