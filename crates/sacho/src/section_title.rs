//! Plain section titles and render-only title templates.

use crate::section_pattern::SectionPattern;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

/// Invalid plain title, template syntax, or capture substitution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid section title: {0}")]
pub struct SectionTitleError(
    /// Explanation of the invalid title or template.
    pub String,
);

/// Validates a nonempty, untrimmed, single-line plain title.
///
/// Titles retain their exact Unicode spelling and may contain Markdown syntax;
/// writers must escape that syntax when producing a heading.
pub fn validate_section_title(title: &str) -> Result<(), SectionTitleError> {
    if title.is_empty() || title.trim() != title || title.chars().any(char::is_control) {
        return Err(SectionTitleError(
            "must be nonempty, have no surrounding whitespace, and contain no control characters"
                .into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Part {
    Literal(String),
    Capture(String),
}

/// A plain title template with named captures and doubled literal braces.
///
/// Unlike path patterns, templates allow repeated or omitted captures, adjacent
/// captures, and arbitrary literal slashes. Only the existing reversible path
/// pattern subset can be used for automatic title recognition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectionTitleTemplate {
    parts: Vec<Part>,
}

impl SectionTitleTemplate {
    /// Returns capture names in occurrence order, including repeats.
    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Capture(name) => Some(name.as_str()),
            _ => None,
        })
    }

    /// Renders a title and checks the resulting plain title's validity.
    pub fn render(&self, captures: &BTreeMap<String, String>) -> Result<String, SectionTitleError> {
        let mut title = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => title.push_str(text),
                Part::Capture(name) => {
                    title.push_str(captures.get(name).ok_or_else(|| {
                        SectionTitleError(format!("capture {name:?} has no value"))
                    })?)
                }
            }
        }
        validate_section_title(&title)?;
        Ok(title)
    }

    /// Returns an inverse only when every source capture appears exactly once
    /// and the template obeys the existing reversible section pattern grammar.
    pub fn reversible_pattern(&self, source: &SectionPattern) -> Option<SectionPattern> {
        let expected = source.variables().collect::<BTreeSet<_>>();
        let actual = self.variables().collect::<Vec<_>>();
        if actual.len() != expected.len()
            || actual.iter().copied().collect::<BTreeSet<_>>() != expected
        {
            return None;
        }
        self.to_string().parse().ok()
    }
}

impl FromStr for SectionTitleTemplate {
    type Err = SectionTitleError;
    fn from_str(source: &str) -> Result<Self, Self::Err> {
        validate_section_title(source)?;
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = source.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '{' | '}' if chars.peek() == Some(&ch) => {
                    chars.next();
                    literal.push(ch);
                }
                '{' => {
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    let mut name = String::new();
                    let mut closed = false;
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        name.push(ch);
                    }
                    let mut names = name.chars();
                    if !closed
                        || !names
                            .next()
                            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                        || !names.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    {
                        return Err(SectionTitleError("invalid or unclosed capture".into()));
                    }
                    parts.push(Part::Capture(name));
                }
                '}' => return Err(SectionTitleError("closing brace is not escaped".into())),
                ch => literal.push(ch),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        Ok(Self { parts })
    }
}

impl fmt::Display for SectionTitleTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for part in &self.parts {
            match part {
                Part::Literal(text) => f.write_str(&text.replace('{', "{{").replace('}', "}}"))?,
                Part::Capture(name) => write!(f, "{{{name}}}")?,
            }
        }
        Ok(())
    }
}
impl Serialize for SectionTitleTemplate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for SectionTitleTemplate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn repeats_omits_and_escapes_without_path_constraints() {
        let title: SectionTitleTemplate = "{{{name}}}/{name} / API".parse().unwrap();
        assert_eq!(
            title
                .render(&BTreeMap::from([("name".into(), "core".into())]))
                .unwrap(),
            "{core}/core / API"
        );
        assert!(
            title
                .reversible_pattern(&"{name}/{scope}".parse().unwrap())
                .is_none()
        );
        assert!(
            "constant"
                .parse::<SectionTitleTemplate>()
                .unwrap()
                .render(&BTreeMap::new())
                .is_ok()
        );
        assert!(title.render(&BTreeMap::new()).is_err());
    }
    #[test]
    fn inverse_is_restricted_to_complete_reversible_templates() {
        let source = "packages/{scope}/{name}".parse().unwrap();
        for template in [
            "{name}",
            "{name}/{name}",
            "{scope}{name}",
            "{scope}//{name}",
            "{scope}/{other}",
        ] {
            assert!(
                template
                    .parse::<SectionTitleTemplate>()
                    .unwrap()
                    .reversible_pattern(&source)
                    .is_none(),
                "{template}"
            );
        }
        assert!(
            "@{scope}/{name} (Rust)"
                .parse::<SectionTitleTemplate>()
                .unwrap()
                .reversible_pattern(&source)
                .is_some()
        );
    }
    #[test]
    fn rejects_invalid_titles_and_template_syntax() {
        for title in ["", " x", "x ", "x\nY", "x\rY", "x\tY", "x\0", "\u{7f}"] {
            assert!(validate_section_title(title).is_err(), "{title:?}");
        }
        for title in ["{", "}", "{}", "{0x}", "{x{y}"] {
            assert!(title.parse::<SectionTitleTemplate>().is_err(), "{title:?}");
        }
        let title: SectionTitleTemplate = "{name}".parse().unwrap();
        for value in ["", " x", "x\n"] {
            assert!(
                title
                    .render(&BTreeMap::from([("name".into(), value.into())]))
                    .is_err()
            );
        }
        for title in ["$x$ API", "C #", "<a>", "&amp;", "\\", "한글"] {
            assert!(validate_section_title(title).is_ok());
        }
    }
    proptest! {
        #[test]
        fn template_display_and_render_round_trip(name in "[a-zA-Z0-9_-]{1,30}") {
            let template: SectionTitleTemplate = "{{{name}}} / {name}".parse().unwrap();
            prop_assert_eq!(template.to_string().parse::<SectionTitleTemplate>().unwrap(), template.clone());
            let rendered = template.render(&BTreeMap::from([("name".into(), name.clone())])).unwrap();
            prop_assert_eq!(rendered, format!("{{{name}}} / {name}"));
        }
        #[test]
        fn inverse_round_trip(name in "[a-z][a-z0-9_-]{0,20}") {
            let template: SectionTitleTemplate = "{name} (Rust)".parse().unwrap();
            let values = BTreeMap::from([("name".into(), name)]);
            prop_assert_eq!(template.reversible_pattern(&"crates/{name}".parse().unwrap()).unwrap().captures(&template.render(&values).unwrap()), Some(values));
        }
    }
}
