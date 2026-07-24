use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::config::Config;
use crate::config::SectionConfig;
use crate::error::{Error, Result};
use crate::section_pattern::{
    SectionPatternConfig, SectionPatternConfigError, SectionPatternError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSection {
    pub(crate) id: String,
    pub(crate) directory: PathBuf,
    pub(crate) pattern_index: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SectionResolutionError {
    #[error("invalid section pattern {index}: {source}")]
    InvalidPattern {
        index: usize,
        source: SectionPatternConfigError,
    },

    #[error("invalid section glob {pattern:?}: {source}")]
    InvalidGlob {
        pattern: String,
        source: globset::Error,
    },

    #[error(transparent)]
    Render {
        #[from]
        source: SectionPatternError,
    },

    #[error("path is not valid UTF-8: {}", path.display())]
    NonUtf8Path { path: PathBuf },

    #[error("path is not a normalized repository-relative path: {}", path.display())]
    NonRelativePath { path: PathBuf },

    #[error("section pattern match for {value:?} is ambiguous between patterns {patterns:?}")]
    Ambiguous { value: String, patterns: Vec<usize> },

    #[error(
        "section {requested_id:?} is shadowed by explicit section {explicit_id:?} at {}",
        directory.display()
    )]
    Shadowed {
        requested_id: String,
        explicit_id: String,
        directory: PathBuf,
    },
}

pub(crate) struct SectionResolver<'a> {
    explicit: &'a [SectionConfig],
    patterns: &'a [SectionPatternConfig],
    explicit_path_matchers: RefCell<BTreeMap<usize, globset::GlobSet>>,
    pattern_path_matchers: RefCell<BTreeMap<String, globset::GlobMatcher>>,
}

impl<'a> SectionResolver<'a> {
    pub(crate) fn from_config(config: &'a Config) -> Result<Self> {
        Self::new(&config.sections, &config.section_patterns).map_err(|error| {
            Error::SectionPattern {
                message: error.to_string(),
            }
        })
    }

    pub(crate) fn new(
        explicit: &'a [SectionConfig],
        patterns: &'a [SectionPatternConfig],
    ) -> Result<Self, SectionResolutionError> {
        for (index, pattern) in patterns.iter().enumerate() {
            pattern
                .validate()
                .map_err(|source| SectionResolutionError::InvalidPattern { index, source })?;
        }
        Ok(Self {
            explicit,
            patterns,
            explicit_path_matchers: RefCell::new(BTreeMap::new()),
            pattern_path_matchers: RefCell::new(BTreeMap::new()),
        })
    }

    pub(crate) fn resolve_id(
        &self,
        id: &str,
    ) -> Result<Option<ResolvedSection>, SectionResolutionError> {
        if let Some(section) = self.explicit.iter().find(|section| section.id == id) {
            return Ok(Some(explicit_section(section)));
        }

        let mut candidates = Vec::new();
        let mut shadowed = None;
        for (index, pattern) in self.patterns.iter().enumerate() {
            let Some(captures) = pattern.id.captures(id) else {
                continue;
            };
            let candidate = instantiate(pattern, index, &captures)?;
            if let Some(section) = self
                .explicit
                .iter()
                .find(|section| section.directory == candidate.directory)
            {
                shadowed.get_or_insert_with(|| SectionResolutionError::Shadowed {
                    requested_id: id.to_owned(),
                    explicit_id: section.id.clone(),
                    directory: section.directory.clone(),
                });
            } else {
                candidates.push(candidate);
            }
        }
        match one_candidate(id, candidates)? {
            Some(candidate) => Ok(Some(candidate)),
            None => match shadowed {
                Some(error) => Err(error),
                None => Ok(None),
            },
        }
    }

    pub(crate) fn resolve_directory(
        &self,
        directory: &Path,
    ) -> Result<Option<ResolvedSection>, SectionResolutionError> {
        if let Some(section) = self
            .explicit
            .iter()
            .find(|section| section.directory == directory)
        {
            return Ok(Some(explicit_section(section)));
        }
        if self.patterns.is_empty() {
            return Ok(None);
        }
        let value = path_pattern_string(directory)?;
        let mut candidates = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let Some(captures) = pattern.directory.captures(&value) else {
                continue;
            };
            let candidate = instantiate(pattern, index, &captures)?;
            if !self
                .explicit
                .iter()
                .any(|section| section.id == candidate.id)
            {
                candidates.push(candidate);
            }
        }
        one_candidate(&value, candidates)
    }

    pub(crate) fn resolve_source_path(
        &self,
        path: &Path,
    ) -> Result<Vec<ResolvedSection>, SectionResolutionError> {
        let mut resolved = Vec::new();
        for (index, section) in self.explicit.iter().enumerate() {
            if self.explicit_paths_are_match(index, section, path)? {
                resolved.push(explicit_section(section));
            }
        }
        if self.patterns.is_empty() {
            return Ok(resolved);
        }
        let value = path_pattern_string(path)?;

        let mut patterned = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let Some(captures) = pattern.source.captures_prefix(&value) else {
                continue;
            };
            let attributed = match &pattern.paths {
                None => true,
                Some(paths) => {
                    let mut matched = false;
                    for path_pattern in paths {
                        let rendered = path_pattern
                            .render_glob_with_literal_prefix(&pattern.source, &captures)?;
                        matched |= self.pattern_path_is_match(&rendered, path)?;
                    }
                    matched
                }
            };
            if !attributed {
                continue;
            }
            let candidate = instantiate(pattern, index, &captures)?;
            if !self.explicit.iter().any(|section| {
                section.id == candidate.id || section.directory == candidate.directory
            }) {
                patterned.push(candidate);
            }
        }
        if let Some(candidate) = one_candidate(&value, patterned)? {
            resolved.push(candidate);
        }
        Ok(resolved)
    }

    fn explicit_paths_are_match(
        &self,
        index: usize,
        section: &SectionConfig,
        path: &Path,
    ) -> Result<bool, SectionResolutionError> {
        if let Some(matched) = {
            let matchers = self.explicit_path_matchers.borrow();
            matchers.get(&index).map(|matcher| matcher.is_match(path))
        } {
            return Ok(matched);
        }
        let matcher = compile_globs(section.paths.iter().map(String::as_str))?;
        let matched = matcher.is_match(path);
        self.explicit_path_matchers
            .borrow_mut()
            .insert(index, matcher);
        Ok(matched)
    }

    fn pattern_path_is_match(
        &self,
        rendered: &str,
        path: &Path,
    ) -> Result<bool, SectionResolutionError> {
        if let Some(matched) = {
            let matchers = self.pattern_path_matchers.borrow();
            matchers.get(rendered).map(|matcher| matcher.is_match(path))
        } {
            return Ok(matched);
        }
        let matcher = globset::Glob::new(rendered)
            .map_err(|source| SectionResolutionError::InvalidGlob {
                pattern: rendered.to_owned(),
                source,
            })?
            .compile_matcher();
        let matched = matcher.is_match(path);
        self.pattern_path_matchers
            .borrow_mut()
            .insert(rendered.to_owned(), matcher);
        Ok(matched)
    }

    #[cfg(test)]
    fn cached_pattern_glob_count(&self) -> usize {
        self.pattern_path_matchers.borrow().len()
    }

    #[cfg(test)]
    fn cached_explicit_glob_count(&self) -> usize {
        self.explicit_path_matchers.borrow().len()
    }

    pub(crate) fn ordered_present(
        &self,
        present: &BTreeSet<String>,
    ) -> Result<Vec<String>, SectionResolutionError> {
        let mut remaining = present.clone();
        let mut ordered = Vec::new();
        for section in self.explicit {
            if remaining.remove(&section.id) {
                ordered.push(section.id.clone());
            }
        }

        let mut patterned = vec![Vec::new(); self.patterns.len()];
        let mut unknown = Vec::new();
        for id in remaining {
            match self.resolve_id(&id) {
                Ok(Some(section)) => match section.pattern_index {
                    Some(index) => patterned[index].push(id),
                    None => unknown.push(id),
                },
                Ok(None) | Err(SectionResolutionError::Shadowed { .. }) => unknown.push(id),
                Err(error) => return Err(error),
            }
        }
        for sections in patterned {
            ordered.extend(sections);
        }
        ordered.extend(unknown);
        Ok(ordered)
    }
}

pub(crate) fn has_sections(config: &Config) -> bool {
    !config.sections.is_empty() || !config.section_patterns.is_empty()
}

fn explicit_section(section: &SectionConfig) -> ResolvedSection {
    ResolvedSection {
        id: section.id.clone(),
        directory: section.directory.clone(),
        pattern_index: None,
    }
}

fn instantiate(
    pattern: &SectionPatternConfig,
    pattern_index: usize,
    captures: &BTreeMap<String, String>,
) -> Result<ResolvedSection, SectionResolutionError> {
    Ok(ResolvedSection {
        id: pattern.id.render(captures)?,
        directory: PathBuf::from(pattern.directory.render(captures)?),
        pattern_index: Some(pattern_index),
    })
}

fn one_candidate(
    value: &str,
    candidates: Vec<ResolvedSection>,
) -> Result<Option<ResolvedSection>, SectionResolutionError> {
    if candidates.len() > 1 {
        return Err(SectionResolutionError::Ambiguous {
            value: value.to_owned(),
            patterns: candidates
                .iter()
                .filter_map(|section| section.pattern_index)
                .collect(),
        });
    }
    Ok(candidates.into_iter().next())
}

fn compile_globs<'a>(
    patterns: impl Iterator<Item = &'a str>,
) -> Result<globset::GlobSet, SectionResolutionError> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(globset::Glob::new(pattern).map_err(|source| {
            SectionResolutionError::InvalidGlob {
                pattern: pattern.to_owned(),
                source,
            }
        })?);
    }
    builder
        .build()
        .map_err(|source| SectionResolutionError::InvalidGlob {
            pattern: String::from("<section paths>"),
            source,
        })
}

fn path_pattern_string(path: &Path) -> Result<String, SectionResolutionError> {
    let mut output = String::new();
    for component in path.components() {
        let value = match component {
            Component::Normal(value) => {
                value
                    .to_str()
                    .ok_or_else(|| SectionResolutionError::NonUtf8Path {
                        path: path.to_path_buf(),
                    })?
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(SectionResolutionError::NonRelativePath {
                    path: path.to_path_buf(),
                });
            }
        };
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(value);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::str::FromStr;

    use proptest::prelude::*;

    use crate::section_pattern::SectionPattern;

    use super::*;

    fn pattern(
        source: &str,
        id: &str,
        directory: &str,
        paths: Option<&[&str]>,
    ) -> SectionPatternConfig {
        SectionPatternConfig {
            source: SectionPattern::from_str(source).expect("source"),
            id: SectionPattern::from_str(id).expect("id"),
            directory: SectionPattern::from_str(directory).expect("directory"),
            paths: paths.map(|paths| {
                paths
                    .iter()
                    .map(|path| SectionPattern::from_str(path).expect("path"))
                    .collect()
            }),
        }
    }

    fn explicit(id: &str, directory: &str, paths: &[&str]) -> SectionConfig {
        SectionConfig {
            id: id.to_owned(),
            directory: PathBuf::from(directory),
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn explicit_globs_are_compiled_only_for_source_attribution() {
        let explicit = [explicit("core", "core", &["["])];
        let resolver = SectionResolver::new(&explicit, &[]).expect("resolver");

        assert_eq!(resolver.cached_explicit_glob_count(), 0);
        assert_eq!(
            resolver
                .resolve_id("core")
                .expect("id")
                .expect("section")
                .directory,
            PathBuf::from("core")
        );
        assert_eq!(resolver.cached_explicit_glob_count(), 0);
        assert!(matches!(
            resolver.resolve_source_path(Path::new("src/lib.rs")),
            Err(SectionResolutionError::InvalidGlob { pattern, .. }) if pattern == "["
        ));
        assert_eq!(resolver.cached_explicit_glob_count(), 0);
    }

    #[test]
    fn explicit_path_matchers_are_cached_by_section() {
        let explicit = [explicit("core", "core", &["src/**"])];
        let resolver = SectionResolver::new(&explicit, &[]).expect("resolver");

        resolver
            .resolve_source_path(Path::new("src/lib.rs"))
            .expect("first source");
        resolver
            .resolve_source_path(Path::new("src/parser.rs"))
            .expect("same section");

        assert_eq!(resolver.cached_explicit_glob_count(), 1);
    }

    #[test]
    fn resolves_the_same_section_in_all_directions() {
        let patterns = [pattern(
            "packages/{scope}/plugin-{name}",
            "@{scope}/{name}",
            "{scope}/plugin-{name}",
            None,
        )];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        let by_id = resolver
            .resolve_id("@acme/http")
            .expect("resolve id")
            .expect("section");
        let by_directory = resolver
            .resolve_directory(Path::new("acme/plugin-http"))
            .expect("resolve directory")
            .expect("section");
        let by_source = resolver
            .resolve_source_path(Path::new("packages/acme/plugin-http/src/lib.rs"))
            .expect("resolve source");

        assert_eq!(by_id, by_directory);
        assert_eq!(by_source, vec![by_id]);
    }

    #[test]
    fn source_paths_honor_default_custom_and_empty_attribution() {
        let patterns = [
            pattern("packages/{name}", "{name}", "{name}", None),
            pattern(
                "crates/{name}",
                "crate-{name}",
                "crate-{name}",
                Some(&["crates/{name}/src/**"]),
            ),
            pattern(
                "examples/{name}",
                "example-{name}",
                "example-{name}",
                Some(&[]),
            ),
        ];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        assert_eq!(
            resolver
                .resolve_source_path(Path::new("packages/core"))
                .expect("source")[0]
                .id,
            "core"
        );
        assert_eq!(
            resolver
                .resolve_source_path(Path::new("packages/core/tests/api.rs"))
                .expect("source")[0]
                .id,
            "core"
        );
        assert_eq!(
            resolver
                .resolve_source_path(Path::new("crates/parser/src/lib.rs"))
                .expect("source")[0]
                .id,
            "crate-parser"
        );
        assert!(
            resolver
                .resolve_source_path(Path::new("crates/parser/tests/api.rs"))
                .expect("source")
                .is_empty()
        );
        assert!(
            resolver
                .resolve_source_path(Path::new("examples/demo/main.rs"))
                .expect("source")
                .is_empty()
        );
    }

    #[test]
    fn custom_paths_treat_capture_glob_metacharacters_literally() {
        let patterns = [
            pattern(
                "packages/{name}",
                "{name}",
                "{name}",
                Some(&["packages/{name}/src/**"]),
            ),
            pattern(
                "vendor/[legacy]/{name}",
                "legacy-{name}",
                "legacy-{name}",
                Some(&["vendor/[legacy]/{name}/src/**"]),
            ),
        ];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        assert_eq!(
            resolver
                .resolve_source_path(Path::new("packages/core[0]/src/lib.rs"))
                .expect("source")[0]
                .id,
            "core[0]"
        );
        assert!(
            resolver
                .resolve_source_path(Path::new("packages/core0/src/lib.rs"))
                .expect("source")
                .iter()
                .all(|section| section.id != "core[0]")
        );
        assert_eq!(
            resolver
                .resolve_source_path(Path::new("vendor/[legacy]/parser/src/lib.rs"))
                .expect("literal source")[0]
                .id,
            "legacy-parser"
        );
    }

    #[test]
    fn custom_path_matchers_are_cached_by_rendered_glob() {
        let patterns = [pattern(
            "packages/{name}",
            "{name}",
            "{name}",
            Some(&["packages/{name}/src/**"]),
        )];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        resolver
            .resolve_source_path(Path::new("packages/core/src/lib.rs"))
            .expect("first source");
        resolver
            .resolve_source_path(Path::new("packages/core/src/parser.rs"))
            .expect("same capture");
        assert_eq!(resolver.cached_pattern_glob_count(), 1);

        resolver
            .resolve_source_path(Path::new("packages/cli/src/main.rs"))
            .expect("different capture");
        assert_eq!(resolver.cached_pattern_glob_count(), 2);
    }

    #[test]
    fn explicit_sections_suppress_colliding_pattern_instances() {
        let explicit = [explicit("@acme/core", "special", &["special-source/**"])];
        let patterns = [pattern("packages/{name}", "@acme/{name}", "{name}", None)];
        let resolver = SectionResolver::new(&explicit, &patterns).expect("resolver");

        assert_eq!(
            resolver
                .resolve_id("@acme/core")
                .expect("id")
                .expect("explicit")
                .directory,
            PathBuf::from("special")
        );
        assert!(
            resolver
                .resolve_source_path(Path::new("packages/core/src/lib.rs"))
                .expect("source")
                .is_empty()
        );
        assert_eq!(
            resolver
                .resolve_source_path(Path::new("special-source/lib.rs"))
                .expect("source")[0]
                .id,
            "@acme/core"
        );
    }

    #[test]
    fn reports_a_pattern_id_shadowed_by_an_explicit_directory() {
        let explicit = [explicit("special", "core", &[])];
        let patterns = [pattern("packages/{name}", "@acme/{name}", "{name}", None)];
        let resolver = SectionResolver::new(&explicit, &patterns).expect("resolver");

        assert!(matches!(
            resolver.resolve_id("@acme/core"),
            Err(SectionResolutionError::Shadowed {
                explicit_id,
                ..
            }) if explicit_id == "special"
        ));
        assert_eq!(
            resolver
                .resolve_directory(Path::new("core"))
                .expect("directory")
                .expect("explicit")
                .id,
            "special"
        );
        assert!(
            resolver
                .resolve_source_path(Path::new("packages/core/src/lib.rs"))
                .expect("source")
                .is_empty()
        );
    }

    #[test]
    fn rejects_ambiguous_concrete_pattern_matches() {
        let patterns = [
            pattern("packages/{name}", "pkg/{name}", "{name}", None),
            pattern("packages/plugin-{name}", "plugin/{name}", "{name}", None),
        ];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        assert!(matches!(
            resolver.resolve_source_path(Path::new("packages/plugin-http/src/lib.rs")),
            Err(SectionResolutionError::Ambiguous { .. })
        ));
    }

    #[test]
    fn orders_present_sections_by_explicit_and_pattern_declaration() {
        let explicit = [explicit("workspace", "workspace", &[])];
        let patterns = [
            pattern("packages/{name}", "pkg/{name}", "{name}", None),
            pattern("tools/{name}", "tool/{name}", "tool-{name}", None),
        ];
        let resolver = SectionResolver::new(&explicit, &patterns).expect("resolver");
        let present = ["unknown", "tool/z", "pkg/z", "workspace", "pkg/a", "tool/a"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            resolver.ordered_present(&present).expect("order"),
            ["workspace", "pkg/a", "pkg/z", "tool/a", "tool/z", "unknown",]
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_paths_when_patterns_need_capture_values() {
        use std::os::unix::ffi::OsStringExt;

        let patterns = [pattern("packages/{name}", "{name}", "{name}", None)];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'p', b'a', b'c', b'k', b'a', b'g', b'e', b's', b'/', 0xff,
        ]));

        assert!(matches!(
            resolver.resolve_source_path(&path),
            Err(SectionResolutionError::NonUtf8Path { .. })
        ));
    }

    #[test]
    fn reports_non_relative_paths_separately_from_non_utf8_paths() {
        let patterns = [pattern("packages/{name}", "{name}", "{name}", None)];
        let resolver = SectionResolver::new(&[], &patterns).expect("resolver");

        assert!(matches!(
            resolver.resolve_source_path(Path::new("../packages/core")),
            Err(SectionResolutionError::NonRelativePath { .. })
        ));
    }

    proptest! {
        #[test]
        fn directional_resolution_is_an_inverse(
            scope in "[a-z][a-z0-9_-]{0,8}",
            name in "[a-z][a-z0-9_-]{0,8}",
        ) {
            let patterns = [pattern(
                "packages/{scope}/plugin-{name}",
                "@{scope}/{name}",
                "{scope}/plugin-{name}",
                None,
            )];
            let resolver = SectionResolver::new(&[], &patterns).expect("resolver");
            let id = format!("@{scope}/{name}");
            let directory = PathBuf::from(format!("{scope}/plugin-{name}"));
            let source = PathBuf::from(format!(
                "packages/{scope}/plugin-{name}/src/lib.rs",
            ));

            let by_id = resolver.resolve_id(&id).expect("id").expect("section");
            let by_directory = resolver
                .resolve_directory(&directory)
                .expect("directory")
                .expect("section");
            let by_source = resolver
                .resolve_source_path(&source)
                .expect("source")
                .pop()
                .expect("section");

            prop_assert_eq!(&by_id, &by_directory);
            prop_assert_eq!(&by_id, &by_source);
        }
    }
}
