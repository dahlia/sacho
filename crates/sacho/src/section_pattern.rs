//! Section pattern parsing and matching.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Parsed section pattern.
///
/// Patterns are made of slash-delimited segments. Each segment is either
/// literal text or contains one named capture such as `{name}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionPattern {
    segments: Vec<SectionPatternSegment>,
}

/// One slash-delimited segment in a [`SectionPattern`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SectionPatternSegment {
    /// A segment that must match this text exactly.
    Literal(String),

    /// A segment containing one nonempty named capture.
    Capture {
        /// Literal text before the capture.
        prefix: String,

        /// Capture name.
        name: String,

        /// Literal text after the capture.
        suffix: String,
    },
}

/// Error returned while parsing or rendering a [`SectionPattern`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SectionPatternError {
    /// The pattern string does not follow the section pattern grammar.
    #[error("invalid section pattern {pattern:?}: {reason}")]
    InvalidSyntax {
        /// Pattern that could not be parsed.
        pattern: String,

        /// Explanation of the invalid syntax.
        reason: &'static str,
    },

    /// A required capture value was not supplied.
    #[error("section pattern capture {name:?} has no value")]
    MissingCapture {
        /// Name of the missing capture.
        name: String,
    },

    /// A capture value cannot be rendered as one path segment.
    #[error("invalid value {value:?} for section pattern capture {name:?}")]
    InvalidCapture {
        /// Name of the capture.
        name: String,

        /// Invalid capture value.
        value: String,
    },

    /// A requested literal glob prefix is not a structural pattern prefix.
    #[error("section pattern {prefix:?} is not a prefix of {pattern:?}")]
    InvalidGlobPrefix {
        /// Pattern being rendered.
        pattern: String,

        /// Pattern expected to form its literal prefix.
        prefix: String,
    },
}

/// Configuration for a family of changelog sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SectionPatternConfig {
    /// Repository-relative source subtree template.
    pub source: SectionPattern,

    /// Changelog section identifier template.
    pub id: SectionPattern,

    /// Fragment subdirectory template.
    pub directory: SectionPattern,

    /// Optional section-attribution glob templates.
    ///
    /// An absent value attributes strict descendants below the matched source.
    /// Use an explicit path to attribute a source-depth file. An explicitly
    /// empty list disables section-specific attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<SectionPattern>>,
}

/// Semantic error in a [`SectionPatternConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid section pattern field {field}: {reason}")]
pub struct SectionPatternConfigError {
    /// Field containing the invalid pattern.
    pub field: String,

    /// Explanation of the semantic constraint.
    pub reason: String,
}

impl SectionPatternConfig {
    /// Validates reversibility and path-safety constraints.
    pub fn validate(&self) -> Result<(), SectionPatternConfigError> {
        validate_path_template("source", &self.source)?;
        validate_path_template("directory", &self.directory)?;

        let source_variables = unique_variables("source", &self.source)?;
        if source_variables.is_empty() {
            return Err(config_error("source", "must contain at least one capture"));
        }
        for (field, pattern) in [("id", &self.id), ("directory", &self.directory)] {
            let variables = unique_variables(field, pattern)?;
            if variables != source_variables {
                return Err(config_error(
                    field,
                    "must contain each source capture exactly once and no other captures",
                ));
            }
        }

        for (index, pattern) in self.paths.iter().flatten().enumerate() {
            let field = format!("paths[{index}]");
            validate_path_template(&field, pattern)?;
            if !pattern.segments.starts_with(&self.source.segments) {
                return Err(config_error(
                    field,
                    "must start with the complete source template",
                ));
            }
            if let Some(variable) = pattern
                .variables()
                .find(|variable| !source_variables.contains(*variable))
            {
                return Err(config_error(
                    field,
                    format!("uses undeclared capture {variable:?}"),
                ));
            }
        }
        Ok(())
    }
}

impl SectionPattern {
    /// Returns this pattern's parsed segments.
    pub fn segments(&self) -> &[SectionPatternSegment] {
        &self.segments
    }

    /// Returns the capture names in source order.
    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            SectionPatternSegment::Literal(_) => None,
            SectionPatternSegment::Capture { name, .. } => Some(name.as_str()),
        })
    }

    /// Renders the pattern using the supplied capture values.
    pub fn render(
        &self,
        captures: &BTreeMap<String, String>,
    ) -> Result<String, SectionPatternError> {
        self.render_with(captures, str::to_owned, str::to_owned)
    }

    /// Renders the pattern as a glob while escaping capture values.
    ///
    /// Literal pattern text retains its glob meaning. Capture values are
    /// escaped so names containing glob metacharacters remain literal.
    /// Doubled braces parsed as literal text also remain literal.
    pub fn render_glob(
        &self,
        captures: &BTreeMap<String, String>,
    ) -> Result<String, SectionPatternError> {
        self.render_with(captures, escape_glob_braces, globset::escape)
    }

    /// Renders a glob while treating a structural prefix as literal text.
    ///
    /// This is useful for path globs whose repository subtree must match
    /// exactly while later segments retain glob syntax.
    pub fn render_glob_with_literal_prefix(
        &self,
        prefix: &Self,
        captures: &BTreeMap<String, String>,
    ) -> Result<String, SectionPatternError> {
        if !self.segments.starts_with(&prefix.segments) {
            return Err(SectionPatternError::InvalidGlobPrefix {
                pattern: self.to_string(),
                prefix: prefix.to_string(),
            });
        }

        let mut rendered = String::new();
        for (index, segment) in self.segments.iter().enumerate() {
            if index != 0 {
                rendered.push('/');
            }
            if index < prefix.segments.len() {
                let literal = render_segment(segment, captures, str::to_owned, str::to_owned)?;
                rendered.push_str(&globset::escape(&literal));
            } else {
                rendered.push_str(&render_segment(
                    segment,
                    captures,
                    escape_glob_braces,
                    globset::escape,
                )?);
            }
        }
        Ok(rendered)
    }

    fn render_with(
        &self,
        captures: &BTreeMap<String, String>,
        render_literal: impl Fn(&str) -> String,
        render_capture: impl Fn(&str) -> String,
    ) -> Result<String, SectionPatternError> {
        let mut rendered = String::new();
        for (index, segment) in self.segments.iter().enumerate() {
            if index != 0 {
                rendered.push('/');
            }
            rendered.push_str(&render_segment(
                segment,
                captures,
                &render_literal,
                &render_capture,
            )?);
        }
        Ok(rendered)
    }

    /// Matches a complete value and returns its named captures.
    pub fn captures(&self, value: &str) -> Option<BTreeMap<String, String>> {
        let values = value.split('/').collect::<Vec<_>>();
        if values.len() != self.segments.len() {
            return None;
        }
        self.captures_segments(&values)
    }

    /// Matches this pattern at the start of a slash-delimited value.
    ///
    /// Any complete segments after the pattern are ignored.
    pub fn captures_prefix(&self, value: &str) -> Option<BTreeMap<String, String>> {
        let values = value.split('/').collect::<Vec<_>>();
        if values.len() < self.segments.len() {
            return None;
        }
        self.captures_segments(&values[..self.segments.len()])
    }

    fn captures_segments(&self, values: &[&str]) -> Option<BTreeMap<String, String>> {
        let mut captures = BTreeMap::new();
        for (segment, value) in self.segments.iter().zip(values.iter().copied()) {
            match segment {
                SectionPatternSegment::Literal(literal) if literal == value => {}
                SectionPatternSegment::Literal(_) => return None,
                SectionPatternSegment::Capture {
                    prefix,
                    name,
                    suffix,
                } => {
                    let captured = value.strip_prefix(prefix)?.strip_suffix(suffix)?;
                    if invalid_capture_value(captured) {
                        return None;
                    }
                    if let Some(previous) = captures.insert(name.clone(), captured.to_owned())
                        && previous != captured
                    {
                        return None;
                    }
                }
            }
        }
        Some(captures)
    }
}

impl FromStr for SectionPattern {
    type Err = SectionPatternError;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> {
        if pattern.is_empty() {
            return Err(invalid(pattern, "must not be empty"));
        }
        let segments = pattern
            .split('/')
            .map(|segment| parse_segment(pattern, segment))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { segments })
    }
}

impl fmt::Display for SectionPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.segments.iter().enumerate() {
            if index != 0 {
                formatter.write_str("/")?;
            }
            match segment {
                SectionPatternSegment::Literal(literal) => {
                    formatter.write_str(&escape_braces(literal))?;
                }
                SectionPatternSegment::Capture {
                    prefix,
                    name,
                    suffix,
                } => {
                    formatter.write_str(&escape_braces(prefix))?;
                    write!(formatter, "{{{name}}}")?;
                    formatter.write_str(&escape_braces(suffix))?;
                }
            }
        }
        Ok(())
    }
}

impl Serialize for SectionPattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SectionPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let source = String::deserialize(deserializer)?;
        source.parse().map_err(D::Error::custom)
    }
}

fn parse_segment(
    pattern: &str,
    source: &str,
) -> Result<SectionPatternSegment, SectionPatternError> {
    if source.is_empty() {
        return Err(invalid(pattern, "segments must not be empty"));
    }

    let mut before = String::new();
    let mut after = String::new();
    let mut capture = None;
    let mut characters = source.char_indices().peekable();
    while let Some((_, character)) = characters.next() {
        let literal = if capture.is_some() {
            &mut after
        } else {
            &mut before
        };
        match character {
            '{' if characters.peek().is_some_and(|(_, next)| *next == '{') => {
                characters.next();
                literal.push('{');
            }
            '}' if characters.peek().is_some_and(|(_, next)| *next == '}') => {
                characters.next();
                literal.push('}');
            }
            '{' => {
                if capture.is_some() {
                    return Err(invalid(
                        pattern,
                        "each segment may contain at most one capture",
                    ));
                }
                let mut name = String::new();
                let mut closed = false;
                for (_, next) in characters.by_ref() {
                    match next {
                        '}' => {
                            closed = true;
                            break;
                        }
                        '{' => return Err(invalid(pattern, "captures must not be nested")),
                        character => name.push(character),
                    }
                }
                if !closed {
                    return Err(invalid(pattern, "capture is not closed"));
                }
                if !valid_identifier(&name) {
                    return Err(invalid(pattern, "capture name is not a valid identifier"));
                }
                capture = Some(name);
            }
            '}' => return Err(invalid(pattern, "closing brace is not escaped")),
            character => literal.push(character),
        }
    }

    Ok(match capture {
        Some(name) => SectionPatternSegment::Capture {
            prefix: before,
            name,
            suffix: after,
        },
        None => SectionPatternSegment::Literal(before),
    })
}

fn valid_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn invalid_capture_value(value: &str) -> bool {
    value.is_empty() || value == "." || value == ".." || value.contains('/') || value.contains('\\')
}

fn render_segment(
    segment: &SectionPatternSegment,
    captures: &BTreeMap<String, String>,
    render_literal: impl Fn(&str) -> String,
    render_capture: impl Fn(&str) -> String,
) -> Result<String, SectionPatternError> {
    match segment {
        SectionPatternSegment::Literal(literal) => Ok(render_literal(literal)),
        SectionPatternSegment::Capture {
            prefix,
            name,
            suffix,
        } => {
            let value = captures
                .get(name)
                .ok_or_else(|| SectionPatternError::MissingCapture { name: name.clone() })?;
            if invalid_capture_value(value) {
                return Err(SectionPatternError::InvalidCapture {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            Ok(format!(
                "{}{}{}",
                render_literal(prefix),
                render_capture(value),
                render_literal(suffix)
            ))
        }
    }
}

fn escape_glob_braces(value: &str) -> String {
    value.replace('{', "[{]").replace('}', "[}]")
}

fn invalid(pattern: &str, reason: &'static str) -> SectionPatternError {
    SectionPatternError::InvalidSyntax {
        pattern: pattern.to_owned(),
        reason,
    }
}

fn escape_braces(value: &str) -> String {
    value.replace('{', "{{").replace('}', "}}")
}

fn unique_variables<'a>(
    field: &str,
    pattern: &'a SectionPattern,
) -> Result<std::collections::BTreeSet<&'a str>, SectionPatternConfigError> {
    let mut variables = std::collections::BTreeSet::new();
    for variable in pattern.variables() {
        if !variables.insert(variable) {
            return Err(config_error(
                field,
                format!("capture {variable:?} must appear exactly once"),
            ));
        }
    }
    Ok(variables)
}

fn validate_path_template(
    field: &str,
    pattern: &SectionPattern,
) -> Result<(), SectionPatternConfigError> {
    for segment in pattern.segments() {
        let literal = match segment {
            SectionPatternSegment::Literal(literal) => literal,
            SectionPatternSegment::Capture { prefix, suffix, .. } => {
                if prefix.contains('\\') || suffix.contains('\\') {
                    return Err(config_error(
                        field,
                        "must use forward slashes as path separators",
                    ));
                }
                continue;
            }
        };
        if literal == "." || literal == ".." {
            return Err(config_error(
                field,
                "must not contain current- or parent-directory segments",
            ));
        }
        if literal.contains('\\') {
            return Err(config_error(
                field,
                "must use forward slashes as path separators",
            ));
        }
    }
    Ok(())
}

fn config_error(field: impl Into<String>, reason: impl Into<String>) -> SectionPatternConfigError {
    SectionPatternConfigError {
        field: field.into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn parses_literal_and_capture_segments() {
        let pattern = SectionPattern::from_str("packages/{scope}/plugin-{name}").expect("pattern");

        assert_eq!(
            pattern.segments(),
            &[
                SectionPatternSegment::Literal(String::from("packages")),
                SectionPatternSegment::Capture {
                    prefix: String::new(),
                    name: String::from("scope"),
                    suffix: String::new(),
                },
                SectionPatternSegment::Capture {
                    prefix: String::from("plugin-"),
                    name: String::from("name"),
                    suffix: String::new(),
                },
            ]
        );
    }

    #[test]
    fn renders_and_matches_multiple_captures() {
        let pattern = SectionPattern::from_str("packages/{scope}/plugin-{name}").expect("pattern");
        let captures = [
            (String::from("scope"), String::from("acme")),
            (String::from("name"), String::from("http")),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            pattern.render(&captures).expect("render"),
            "packages/acme/plugin-http"
        );
        assert_eq!(
            pattern
                .captures("packages/acme/plugin-http")
                .expect("match"),
            captures
        );
        assert!(pattern.captures("packages/acme/plugin-").is_none());
        assert!(pattern.captures("packages/acme/plugin/http").is_none());
        assert!(pattern.captures("crates/acme/plugin-http").is_none());
    }

    #[test]
    fn rejects_capture_values_that_are_not_one_nonempty_segment() {
        let pattern = SectionPattern::from_str("packages/{name}").expect("pattern");

        for value in ["", ".", "..", "core/http", r"core\http"] {
            let captures = [(String::from("name"), String::from(value))]
                .into_iter()
                .collect();
            assert!(pattern.render(&captures).is_err(), "{value:?} should fail");
            assert!(
                pattern.captures(&format!("packages/{value}")).is_none(),
                "{value:?} should not match"
            );
        }
    }

    #[test]
    fn escapes_capture_values_when_rendering_globs() {
        let pattern = SectionPattern::from_str("packages/{name}/src/**").expect("pattern");
        let captures = [(String::from("name"), String::from("core[0]*?"))]
            .into_iter()
            .collect();

        assert_eq!(
            pattern.render_glob(&captures).expect("glob"),
            r"packages/core[[]0[]][*][?]/src/**"
        );
    }

    #[test]
    fn rendered_globs_preserve_escaped_literal_braces() {
        let pattern = SectionPattern::from_str("packages/{name}/{{draft}}-*.rs").expect("pattern");
        let captures = BTreeMap::from([(String::from("name"), String::from("core"))]);

        let rendered = pattern.render_glob(&captures).expect("rendered glob");
        let matcher = globset::Glob::new(&rendered)
            .expect("glob")
            .compile_matcher();

        assert!(matcher.is_match("packages/core/{draft}-api.rs"));
        assert!(!matcher.is_match("packages/core/draft-api.rs"));
    }

    #[test]
    fn literal_prefix_ends_before_the_first_custom_glob_segment() {
        let prefix = SectionPattern::from_str("packages/{name}").expect("prefix");
        let pattern = SectionPattern::from_str("packages/{name}/[st]rc/**").expect("path pattern");
        let captures = BTreeMap::from([(String::from("name"), String::from("core"))]);

        let rendered = pattern
            .render_glob_with_literal_prefix(&prefix, &captures)
            .expect("rendered glob");

        assert_eq!(rendered, "packages/core/[st]rc/**");
        assert!(
            globset::Glob::new(&rendered)
                .expect("glob")
                .compile_matcher()
                .is_match("packages/core/src/lib.rs")
        );
    }

    #[test]
    fn rendered_path_globs_preserve_escaped_literal_braces() {
        let prefix = SectionPattern::from_str("packages/{name}").expect("prefix");
        let pattern = SectionPattern::from_str("packages/{name}/{{draft}}-{kind}-[st]*.rs")
            .expect("path pattern");
        let captures = BTreeMap::from([
            (String::from("name"), String::from("core")),
            (String::from("kind"), String::from("api")),
        ]);

        let rendered = pattern
            .render_glob_with_literal_prefix(&prefix, &captures)
            .expect("rendered glob");
        let matcher = globset::Glob::new(&rendered)
            .expect("glob")
            .compile_matcher();

        assert!(matcher.is_match("packages/core/{draft}-api-src.rs"));
        assert!(!matcher.is_match("packages/core/draft-api-src.rs"));
    }

    #[test]
    fn repeated_capture_names_must_match_the_same_value() {
        let pattern = SectionPattern::from_str("{name}/{name}").expect("pattern");

        assert_eq!(
            pattern.captures("core/core").expect("same capture")["name"],
            "core"
        );
        assert!(pattern.captures("core/cli").is_none());
    }

    #[test]
    fn escapes_literal_braces() {
        let pattern = SectionPattern::from_str("packages/{{legacy}}-{name}").expect("pattern");

        assert_eq!(pattern.to_string(), "packages/{{legacy}}-{name}");
        assert_eq!(
            pattern.captures("packages/{legacy}-core").expect("match")["name"],
            "core"
        );
    }

    #[test]
    fn rejects_invalid_syntax() {
        for source in [
            "",
            "/packages/{name}",
            "packages/{name}/",
            "packages//{name}",
            "packages/{",
            "packages/}",
            "packages/{}",
            "packages/{two captures}-{here}",
            "packages/{9name}",
            "packages/{name}-{other}",
        ] {
            assert!(
                SectionPattern::from_str(source).is_err(),
                "{source:?} should fail"
            );
        }
    }

    #[test]
    fn accepts_identifiers_starting_with_an_underscore() {
        let leading = SectionPattern::from_str("packages/{_name}").expect("leading underscore");
        let internal =
            SectionPattern::from_str("packages/{package_name}").expect("internal underscore");

        assert_eq!(leading.variables().collect::<Vec<_>>(), ["_name"]);
        assert_eq!(internal.variables().collect::<Vec<_>>(), ["package_name"]);
    }

    #[test]
    fn serializes_as_a_string() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Wrapper {
            pattern: SectionPattern,
        }

        let pattern = SectionPattern::from_str("packages/{name}").expect("pattern");
        let encoded = toml::to_string(&Wrapper {
            pattern: pattern.clone(),
        })
        .expect("serialize");
        let decoded = toml::from_str::<Wrapper>(&encoded).expect("deserialize");

        assert_eq!(decoded.pattern, pattern);
        assert_eq!(encoded, "pattern = \"packages/{name}\"\n");
    }

    #[test]
    fn validates_reversible_pattern_configuration() {
        let config = SectionPatternConfig {
            source: SectionPattern::from_str("packages/{scope}/plugin-{name}").expect("source"),
            id: SectionPattern::from_str("@{scope}/{name}").expect("id"),
            directory: SectionPattern::from_str("{scope}/plugin-{name}").expect("directory"),
            paths: Some(vec![
                SectionPattern::from_str("packages/{scope}/plugin-{name}/src/**").expect("path"),
            ]),
        };

        config.validate().expect("valid pattern configuration");
    }

    #[test]
    fn rejects_non_reversible_pattern_configuration() {
        let cases = [
            SectionPatternConfig {
                source: SectionPattern::from_str("packages/core").expect("source"),
                id: SectionPattern::from_str("core").expect("id"),
                directory: SectionPattern::from_str("core").expect("directory"),
                paths: None,
            },
            SectionPatternConfig {
                source: SectionPattern::from_str("packages/{name}").expect("source"),
                id: SectionPattern::from_str("{other}").expect("id"),
                directory: SectionPattern::from_str("{name}").expect("directory"),
                paths: None,
            },
            SectionPatternConfig {
                source: SectionPattern::from_str("packages/{name}").expect("source"),
                id: SectionPattern::from_str("{name}/{name}").expect("id"),
                directory: SectionPattern::from_str("{name}").expect("directory"),
                paths: None,
            },
            SectionPatternConfig {
                source: SectionPattern::from_str("packages/{name}").expect("source"),
                id: SectionPattern::from_str("{name}").expect("id"),
                directory: SectionPattern::from_str("../{name}").expect("directory"),
                paths: None,
            },
            SectionPatternConfig {
                source: SectionPattern::from_str("packages/{name}").expect("source"),
                id: SectionPattern::from_str("{name}").expect("id"),
                directory: SectionPattern::from_str("{name}").expect("directory"),
                paths: Some(vec![
                    SectionPattern::from_str("crates/{name}/**").expect("path"),
                ]),
            },
            SectionPatternConfig {
                source: SectionPattern::from_str(r"packages\{name}").expect("source"),
                id: SectionPattern::from_str("{name}").expect("id"),
                directory: SectionPattern::from_str("{name}").expect("directory"),
                paths: None,
            },
            SectionPatternConfig {
                source: SectionPattern::from_str(r"{name}\packages").expect("source"),
                id: SectionPattern::from_str("{name}").expect("id"),
                directory: SectionPattern::from_str("{name}").expect("directory"),
                paths: None,
            },
        ];

        for config in cases {
            assert!(config.validate().is_err(), "{config:?} should fail");
        }
    }

    #[test]
    fn distinguishes_default_and_empty_path_lists() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Wrapper {
            pattern: SectionPatternConfig,
        }

        let source = "source = \"packages/{name}\"\n\
                      id = \"{name}\"\n\
                      directory = \"{name}\"\n";
        let default =
            toml::from_str::<Wrapper>(&format!("[pattern]\n{source}")).expect("default paths");
        let empty = toml::from_str::<Wrapper>(&format!("[pattern]\n{source}paths = []\n"))
            .expect("empty paths");

        assert_eq!(default.pattern.paths, None);
        assert_eq!(empty.pattern.paths, Some(Vec::new()));
    }

    proptest! {
        #[test]
        fn display_round_trips(
            prefix in "[A-Za-z0-9_-]{0,8}",
            suffix in "[A-Za-z0-9_-]{0,8}",
        ) {
            let source = format!("packages/{prefix}{{name}}{suffix}");
            let parsed = SectionPattern::from_str(&source).expect("generated pattern");

            prop_assert_eq!(
                SectionPattern::from_str(&parsed.to_string()).expect("round trip"),
                parsed,
            );
        }

        #[test]
        fn rendered_values_match_back(
            prefix in "[A-Za-z0-9_-]{0,8}",
            suffix in "[A-Za-z0-9_-]{0,8}",
            value in "[A-Za-z0-9_.*?\\[\\]_-]{1,12}"
                .prop_filter("capture is not path traversal", |value| {
                    value != "." && value != ".."
                }),
        ) {
            let pattern = SectionPattern::from_str(
                &format!("packages/{prefix}{{name}}{suffix}"),
            )
            .expect("generated pattern");
            let captures = [(String::from("name"), value)]
                .into_iter()
                .collect();
            let rendered = pattern.render(&captures).expect("render");

            prop_assert_eq!(pattern.captures(&rendered), Some(captures));
        }

        #[test]
        fn rendered_path_globs_keep_doubled_braces_literal(
            name in "[A-Za-z0-9_-]{1,8}",
            literal in "[A-Za-z0-9_-]{1,8}",
        ) {
            let prefix = SectionPattern::from_str("packages/{name}").expect("prefix");
            let pattern = SectionPattern::from_str(
                &format!("packages/{{name}}/{{{{{literal}}}}}.rs"),
            )
            .expect("path pattern");
            let captures = BTreeMap::from([(String::from("name"), name.clone())]);
            let rendered = pattern
                .render_glob_with_literal_prefix(&prefix, &captures)
                .expect("rendered glob");
            let matcher = globset::Glob::new(&rendered)
                .expect("glob")
                .compile_matcher();
            let literal_path = format!("packages/{name}/{{{literal}}}.rs");
            let unbraced_path = format!("packages/{name}/{literal}.rs");

            prop_assert!(matcher.is_match(literal_path));
            prop_assert!(!matcher.is_match(unbraced_path));
        }
    }
}
