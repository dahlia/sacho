//! Flat historical section mappings shared by configuration and carry options.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::section::ResolvedSection;
use crate::section_pattern::SectionPattern;

/// The historical key domain used by a persistent section mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionMapKind {
    /// A trimmed historical heading's plain text.
    Title,
    /// A recorded or legacy stable identifier, without normalization.
    Id,
}

/// One persistent, single-hop historical section mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SectionMapConfig {
    /// Historical input domain; this field is required.
    pub kind: SectionMapKind,
    /// Whether `from` and `to` interpret capture templates.
    #[serde(default)]
    pub pattern: bool,
    /// Historical title or ID, interpreted literally unless `pattern` is true.
    pub from: String,
    /// Current stable ID or capture template, never another mapping's alias.
    pub to: String,
}

/// One carry-only override of a recorded ID or historical plain title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSectionMap {
    /// Exact opaque input and current ID; braces remain literal.
    Literal {
        /// Recorded ID or historical title to match exactly.
        from: String,
        /// Current stable section ID.
        to: String,
    },
    /// A reversible input pattern and a render-only current-ID template.
    Pattern {
        /// Reversible pattern containing at least one capture.
        from: String,
        /// Current ID template using only captures declared by `from`.
        to: String,
    },
}

/// Syntactic, target-resolution, ambiguity, or unused-mapping failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("section mapping: {message}")]
pub(crate) struct MapError {
    /// Actionable detail, including the input or rule when applicable.
    pub(crate) message: String,
}

fn invalid(message: impl Into<String>) -> MapError {
    MapError {
        message: message.into(),
    }
}

fn id(value: &str, field: &str) -> Result<(), MapError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{field} must not be blank")));
    }
    Ok(())
}

/// A template for opaque IDs; title validation must never apply here.
#[derive(Debug, Clone)]
struct TargetTemplate(Vec<TargetPart>);

#[derive(Debug, Clone)]
enum TargetPart {
    Literal(String),
    Capture(String),
}

impl TargetTemplate {
    fn parse(source: &str, declared: &BTreeSet<&str>) -> Result<Self, MapError> {
        id(source, "pattern target")?;
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
                        parts.push(TargetPart::Literal(std::mem::take(&mut literal)));
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
                    if !closed || !declared.contains(name.as_str()) {
                        return Err(invalid(format!(
                            "pattern target uses an unclosed or undeclared capture {name:?}"
                        )));
                    }
                    parts.push(TargetPart::Capture(name));
                }
                '}' => return Err(invalid("pattern target has an unescaped closing brace")),
                ch => literal.push(ch),
            }
        }
        if !literal.is_empty() {
            parts.push(TargetPart::Literal(literal));
        }
        Ok(Self(parts))
    }

    fn render(&self, captures: &BTreeMap<String, String>) -> String {
        self.0
            .iter()
            .map(|part| match part {
                TargetPart::Literal(value) => value.as_str(),
                TargetPart::Capture(name) => captures[name].as_str(),
            })
            .collect()
    }
}

/// Domain selector for a compiled mapping set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MapDomain {
    /// Persistent title mappings.
    Title,
    /// Persistent ID mappings.
    Id,
    /// Carry-only mappings applied to either kind of input key.
    Runtime,
}

impl From<SectionMapKind> for MapDomain {
    fn from(kind: SectionMapKind) -> Self {
        match kind {
            SectionMapKind::Title => Self::Title,
            SectionMapKind::Id => Self::Id,
        }
    }
}

#[derive(Debug, Clone)]
enum Matcher {
    Literal {
        from: String,
        to: String,
    },
    Pattern {
        from: SectionPattern,
        to: TargetTemplate,
    },
}

#[derive(Debug, Clone)]
struct Rule {
    domain: MapDomain,
    matcher: Matcher,
}

impl Rule {
    fn captures(&self, input: &str) -> Option<BTreeMap<String, String>> {
        match &self.matcher {
            Matcher::Literal { from, .. } => (from == input).then(BTreeMap::new),
            Matcher::Pattern { from, .. } => from.captures(input),
        }
    }
}

/// Validated syntax and literal targets, independent of document contents.
#[derive(Debug, Clone)]
pub(crate) struct CompiledMaps {
    rules: Vec<Rule>,
}

/// Per-invocation syntactic matching state, separate from target selection.
#[derive(Debug, Clone)]
pub(crate) struct MapUsage {
    matched: Vec<bool>,
}

impl CompiledMaps {
    /// Validate persistent rules and resolve all literal targets immediately.
    ///
    /// Pass `|id| resolver.resolve_id(id).map_err(|e| e.to_string())`.
    pub(crate) fn configuration(
        maps: &[SectionMapConfig],
        mut resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Self, MapError> {
        Self::compile(
            maps.iter().map(|map| {
                (
                    MapDomain::from(map.kind),
                    map.pattern,
                    map.from.as_str(),
                    map.to.as_str(),
                )
            }),
            &mut resolve,
        )
    }

    /// Validate carry options and resolve all literal targets before document IO.
    pub(crate) fn runtime(
        maps: &[RuntimeSectionMap],
        mut resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Self, MapError> {
        Self::compile(
            maps.iter().map(|map| match map {
                RuntimeSectionMap::Literal { from, to } => {
                    (MapDomain::Runtime, false, from.as_str(), to.as_str())
                }
                RuntimeSectionMap::Pattern { from, to } => {
                    (MapDomain::Runtime, true, from.as_str(), to.as_str())
                }
            }),
            &mut resolve,
        )
    }

    fn compile<'a>(
        maps: impl Iterator<Item = (MapDomain, bool, &'a str, &'a str)>,
        resolve: &mut impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Self, MapError> {
        let mut rules = Vec::new();
        let mut sources = BTreeSet::new();
        for (index, (domain, patterned, from, to)) in maps.enumerate() {
            if domain == MapDomain::Title {
                crate::section_title::validate_section_title(from)
                    .map_err(|error| invalid(format!("rule {index} title source: {error}")))?;
            } else {
                id(from, "mapping source")?;
            }
            id(to, "mapping target")?;
            let matcher = if patterned {
                let pattern: SectionPattern = from
                    .parse()
                    .map_err(|error| invalid(format!("rule {index} source: {error}")))?;
                let variables = pattern.variables().collect::<Vec<_>>();
                let declared = variables.iter().copied().collect::<BTreeSet<_>>();
                if declared.is_empty() || declared.len() != variables.len() {
                    return Err(invalid(format!(
                        "rule {index} source must contain at least one capture and each capture exactly once"
                    )));
                }
                let target = TargetTemplate::parse(to, &declared)?;
                // Compare parsed sources, so equivalent escaped spellings cannot
                // evade duplicate-source checks.
                let key = pattern.to_string();
                if !sources.insert((domain, true, key)) {
                    return Err(invalid(format!(
                        "rule {index} repeats a pattern source {from:?}"
                    )));
                }
                Matcher::Pattern {
                    from: pattern,
                    to: target,
                }
            } else {
                if !sources.insert((domain, false, from.to_owned())) {
                    return Err(invalid(format!(
                        "rule {index} repeats a literal source {from:?}"
                    )));
                }
                resolve_target(to, from, resolve)?;
                Matcher::Literal {
                    from: from.to_owned(),
                    to: to.to_owned(),
                }
            };
            rules.push(Rule { domain, matcher });
        }
        Ok(Self { rules })
    }

    /// Start usage accounting for an invocation over this exact mapping set.
    pub(crate) fn usage(&self) -> MapUsage {
        MapUsage {
            matched: vec![false; self.rules.len()],
        }
    }

    /// Observe one input key even if its H3 has no items or a literal wins.
    ///
    /// This does not render or resolve any target. Call for every active H3
    /// before checking unused options; malformed structure must fail separately.
    pub(crate) fn observe(&self, domain: MapDomain, input: &str, usage: &mut MapUsage) {
        assert_eq!(usage.matched.len(), self.rules.len());
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.domain == domain && rule.captures(input).is_some() {
                usage.matched[index] = true;
            }
        }
    }

    /// Resolve one precedence tier: literal if present, otherwise all patterns.
    ///
    /// Returns None only for no syntactic match. A matching broken target or a
    /// resolver ambiguity/shadowing error is terminal and must not fall through.
    pub(crate) fn select(
        &self,
        domain: MapDomain,
        input: &str,
        resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Option<ResolvedSection>, MapError> {
        self.select_with(domain, input, None, resolve)
    }

    /// Resolve only literal rules, allowing current-title lookup before patterns.
    pub(crate) fn select_literal(
        &self,
        domain: MapDomain,
        input: &str,
        resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Option<ResolvedSection>, MapError> {
        self.select_with(domain, input, Some(false), resolve)
    }

    /// Resolve only pattern rules at the caller's selected precedence stage.
    pub(crate) fn select_patterns(
        &self,
        domain: MapDomain,
        input: &str,
        resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Option<ResolvedSection>, MapError> {
        self.select_with(domain, input, Some(true), resolve)
    }

    fn select_with(
        &self,
        domain: MapDomain,
        input: &str,
        patterned: Option<bool>,
        mut resolve: impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
    ) -> Result<Option<ResolvedSection>, MapError> {
        let matches = self
            .rules
            .iter()
            .filter(|rule| rule.domain == domain)
            .filter(|rule| {
                patterned.is_none_or(|expected| {
                    matches!(rule.matcher, Matcher::Pattern { .. }) == expected
                })
            })
            .filter_map(|rule| rule.captures(input).map(|captures| (rule, captures)))
            .collect::<Vec<_>>();
        let literal = matches
            .iter()
            .find(|(rule, _)| matches!(rule.matcher, Matcher::Literal { .. }));
        if let Some((rule, _)) = literal {
            let Matcher::Literal { to, .. } = &rule.matcher else {
                unreachable!()
            };
            return resolve_target(to, input, &mut resolve).map(Some);
        }
        let mut selected: Option<ResolvedSection> = None;
        for (rule, captures) in matches {
            let Matcher::Pattern { to, .. } = &rule.matcher else {
                unreachable!()
            };
            let rendered = to.render(&captures);
            let candidate = resolve_target(&rendered, input, &mut resolve)?;
            if let Some(previous) = &selected {
                if previous.id != candidate.id || previous.directory != candidate.directory {
                    return Err(invalid(format!(
                        "input {input:?} maps ambiguously to {:?} at {:?} and {:?} at {:?}",
                        previous.id, previous.directory, candidate.id, candidate.directory
                    )));
                }
            } else {
                selected = Some(candidate);
            }
        }
        Ok(selected)
    }

    /// Report every unused rule index in declaration order before writing.
    pub(crate) fn unused(&self, usage: &MapUsage) -> Vec<usize> {
        assert_eq!(usage.matched.len(), self.rules.len());
        usage
            .matched
            .iter()
            .enumerate()
            .filter_map(|(index, used)| (!used).then_some(index))
            .collect()
    }
}

fn resolve_target(
    target: &str,
    input: &str,
    resolve: &mut impl FnMut(&str) -> Result<Option<ResolvedSection>, String>,
) -> Result<ResolvedSection, MapError> {
    id(target, "rendered mapping target")?;
    resolve(target)
        .map_err(|message| invalid(format!("target {target:?} for {input:?}: {message}")))?
        .ok_or_else(|| {
            invalid(format!(
                "target {target:?} for {input:?} is not a current section ID"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn resolved(id: &str) -> ResolvedSection {
        ResolvedSection {
            id: id.to_owned(),
            directory: PathBuf::from(format!("dir/{id}")),
            pattern_index: None,
        }
    }
    fn resolve(id: &str) -> Result<Option<ResolvedSection>, String> {
        Ok((id != "missing").then(|| resolved(id)))
    }
    fn literal(from: &str, to: &str) -> RuntimeSectionMap {
        RuntimeSectionMap::Literal {
            from: from.into(),
            to: to.into(),
        }
    }
    fn pattern(from: &str, to: &str) -> RuntimeSectionMap {
        RuntimeSectionMap::Pattern {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn config_requires_kind_defaults_pattern_and_rejects_typos() {
        let source = "kind = 'id'\nfrom = ' old {x} '\nto = ' current '\n";
        let parsed: SectionMapConfig = toml::from_str(source).unwrap();
        assert!(!parsed.pattern);
        assert_eq!(parsed.from, " old {x} ");
        assert_eq!(
            toml::from_str::<SectionMapConfig>(&toml::to_string(&parsed).unwrap()).unwrap(),
            parsed
        );
        for source in [
            "from='a'\nto='b'",
            "kind='unknown'\nfrom='a'\nto='b'",
            "kind='id'\npatern=true\nfrom='a'\nto='b'",
        ] {
            assert!(toml::from_str::<SectionMapConfig>(source).is_err());
        }
    }

    #[test]
    fn literals_are_opaque_and_validate_all_targets_up_front() {
        let maps =
            CompiledMaps::runtime(&[literal(" old {x}=-> ", " current\nID ")], resolve).unwrap();
        assert_eq!(
            maps.select(MapDomain::Runtime, " old {x}=-> ", resolve)
                .unwrap()
                .unwrap()
                .id,
            " current\nID "
        );
        assert!(
            maps.select(MapDomain::Runtime, "old {x}=->", resolve)
                .unwrap()
                .is_none()
        );
        assert!(CompiledMaps::runtime(&[literal("unused", "missing")], resolve).is_err());
        for maps in [
            vec![literal(" ", "x")],
            vec![literal("x", "\n")],
            vec![literal("x", "a"), literal("x", "a")],
        ] {
            assert!(CompiledMaps::runtime(&maps, resolve).is_err());
        }
    }

    #[test]
    fn literal_exception_counts_shadowed_patterns_without_resolving_them() {
        let maps = CompiledMaps::runtime(
            &[
                literal("old/core", "current"),
                pattern("old/{name}", "missing"),
            ],
            resolve,
        )
        .unwrap();
        let mut usage = maps.usage();
        maps.observe(MapDomain::Runtime, "old/core", &mut usage);
        assert!(maps.unused(&usage).is_empty());
        let mut targets = Vec::new();
        let selected = maps
            .select(MapDomain::Runtime, "old/core", |id| {
                targets.push(id.to_owned());
                resolve(id)
            })
            .unwrap()
            .unwrap();
        assert_eq!(selected.id, "current");
        assert_eq!(targets, ["current"]);
        assert!(
            maps.select(MapDomain::Runtime, "old/other", resolve)
                .is_err()
        );
    }

    #[test]
    fn patterns_omit_or_repeat_captures_and_preserve_opaque_target_literals() {
        let maps = CompiledMaps::runtime(
            &[pattern(
                "{scope}/{name}",
                " key\n//{name}/{name}/{{literal}} ",
            )],
            resolve,
        )
        .unwrap();
        assert_eq!(
            maps.select(MapDomain::Runtime, "rust/core", resolve)
                .unwrap()
                .unwrap()
                .id,
            " key\n//core/core/{literal} "
        );
        for map in [
            pattern("constant", "current"),
            pattern("{name}/{name}", "current"),
            pattern("old/{name}", "{unknown}"),
            pattern("old/{name}", "{name"),
            pattern("old/{name}", "stray}"),
        ] {
            assert!(CompiledMaps::runtime(&[map], resolve).is_err());
        }
        assert!(
            CompiledMaps::runtime(
                &[pattern("old/{name}", "a"), pattern("old/{name}", "b")],
                resolve
            )
            .is_err()
        );
        let maps = CompiledMaps::runtime(&[pattern("old/{name}", "new/{name}")], resolve).unwrap();
        for input in ["old/.", "old/..", "old/a\\b", "old/a/b", "old/"] {
            assert!(
                maps.select(MapDomain::Runtime, input, resolve)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn matching_patterns_dedupe_canonical_id_and_directory_or_fail() {
        let maps = CompiledMaps::runtime(
            &[
                pattern("old/{name}", "current"),
                pattern("{era}/core", "current"),
            ],
            resolve,
        )
        .unwrap();
        assert_eq!(
            maps.select(MapDomain::Runtime, "old/core", resolve)
                .unwrap()
                .unwrap()
                .id,
            "current"
        );
        let maps = CompiledMaps::runtime(
            &[pattern("old/{name}", "a"), pattern("{era}/core", "b")],
            resolve,
        )
        .unwrap();
        assert!(
            maps.select(MapDomain::Runtime, "old/core", resolve)
                .is_err()
        );
        assert!(
            maps.select(MapDomain::Runtime, "old/core", |id| {
                let mut section = resolved("same");
                section.directory = id.into();
                Ok(Some(section))
            })
            .is_err()
        );
        assert!(
            maps.select(MapDomain::Runtime, "old/core", |_| Err("shadowed".into()))
                .unwrap_err()
                .message
                .contains("shadowed")
        );
    }

    #[test]
    fn domains_and_title_precedence_remain_separate() {
        let rules = [
            SectionMapConfig {
                kind: SectionMapKind::Title,
                pattern: false,
                from: "Old".into(),
                to: "literal".into(),
            },
            SectionMapConfig {
                kind: SectionMapKind::Title,
                pattern: true,
                from: "{name}".into(),
                to: "missing".into(),
            },
            SectionMapConfig {
                kind: SectionMapKind::Id,
                pattern: false,
                from: "Old".into(),
                to: "id".into(),
            },
        ];
        let maps = CompiledMaps::configuration(&rules, resolve).unwrap();
        assert_eq!(
            maps.select_literal(MapDomain::Title, "Old", resolve)
                .unwrap()
                .unwrap()
                .id,
            "literal"
        );
        assert!(
            maps.select_literal(MapDomain::Title, "New", resolve)
                .unwrap()
                .is_none()
        );
        assert!(
            maps.select_patterns(MapDomain::Title, "New", resolve)
                .is_err()
        );
        assert_eq!(
            maps.select(MapDomain::Id, "Old", resolve)
                .unwrap()
                .unwrap()
                .id,
            "id"
        );
        for from in ["", " title", "title ", "foo\nbar", "foo\0bar"] {
            let rule = SectionMapConfig {
                kind: SectionMapKind::Title,
                pattern: false,
                from: from.into(),
                to: "x".into(),
            };
            assert!(CompiledMaps::configuration(&[rule], resolve).is_err());
        }
        let rule = SectionMapConfig {
            kind: SectionMapKind::Id,
            pattern: false,
            from: " key\n ".into(),
            to: "x".into(),
        };
        assert!(CompiledMaps::configuration(&[rule], resolve).is_ok());
    }

    #[test]
    fn usage_is_syntactic_and_independent_of_items_or_selection() {
        let maps = CompiledMaps::runtime(
            &[
                literal("Empty heading", "a"),
                literal("Unused", "a"),
                pattern("{name}", "missing"),
            ],
            resolve,
        )
        .unwrap();
        let mut usage = maps.usage();
        maps.observe(MapDomain::Runtime, "Empty heading", &mut usage);
        assert_eq!(maps.unused(&usage), [1]);
    }

    #[test]
    fn targets_never_follow_another_mapping() {
        let result = CompiledMaps::runtime(
            &[
                literal("old", "intermediate"),
                literal("intermediate", "current"),
            ],
            |id| Ok((id == "current").then(|| resolved(id))),
        );
        assert!(result.unwrap_err().message.contains("intermediate"));
        let maps = CompiledMaps::runtime(
            &[
                pattern("old/{name}", "alias/{name}"),
                pattern("alias/{name}", "current/{name}"),
            ],
            resolve,
        )
        .unwrap();
        let mut calls = Vec::new();
        assert!(
            maps.select(MapDomain::Runtime, "old/core", |id| {
                calls.push(id.to_owned());
                Ok(None)
            })
            .is_err()
        );
        assert_eq!(calls, ["alias/core"]);
    }

    proptest! {
        #[test]
        fn family_rules_map_to_current_ids_without_source_enumeration(name in "[a-z][a-z0-9_-]{0,30}") {
            let maps = CompiledMaps::runtime(&[pattern("legacy/{name}", "python/{name}")], resolve).unwrap();
            let input = format!("legacy/{name}");
            let expected = format!("python/{name}");
            let mut usage = maps.usage();
            maps.observe(MapDomain::Runtime, &input, &mut usage);
            prop_assert!(maps.unused(&usage).is_empty());
            prop_assert_eq!(maps.select(MapDomain::Runtime, &input, resolve).unwrap().unwrap().id, expected);
        }

        #[test]
        fn arbitrary_nonblank_literal_keys_and_targets_remain_exact(from in ".*", to in ".*") {
            prop_assume!(!from.trim().is_empty());
            prop_assume!(!to.trim().is_empty());
            let maps = CompiledMaps::runtime(&[literal(&from, &to)], |id| Ok(Some(resolved(id)))).unwrap();
            prop_assert_eq!(maps.select(MapDomain::Runtime, &from, |id| Ok(Some(resolved(id)))).unwrap().unwrap().id, to);
        }
    }
}
