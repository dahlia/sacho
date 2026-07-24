use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use std::cell::Cell;

use crate::config::{Config, validate_next_file_is_not_markdown};
use crate::error::{ConfigError, ConfigSnafu, ReadFileSnafu, RenameFileSnafu, Result};
use crate::section::SectionResolver;
use snafu::ResultExt;

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static CASE_SENSITIVITY_DIRECTORY_SCANS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) const MUTATION_LOCK_FILE: &str = ".sacho.lock";

/// Repository handle containing the root path and parsed configuration.
#[derive(Debug, Clone)]
pub struct Repository {
    root: PathBuf,
    config: Config,
    source_config: Config,
    fixed_mutation_lock_path: Option<PathBuf>,
}

/// Filesystem identity of one configured path.
#[derive(Debug, Clone)]
pub(crate) struct ConfiguredPathIdentity {
    pub(crate) key: String,
    pub(crate) configured: PathBuf,
    pub(crate) identity: PathBuf,
    component_case_sensitivity: Vec<bool>,
}

#[derive(Debug, Default)]
pub(crate) struct PathValidationCache {
    case_sensitivity: HashMap<PathBuf, bool>,
}

/// A file whose contents have been written beside its final destination.
pub(crate) struct PreparedAtomicWrite {
    temporary: PathBuf,
    destination: PathBuf,
    created_directories: Vec<PathBuf>,
}

impl PreparedAtomicWrite {
    /// Transfers ownership of directories created while preparing the write.
    pub(crate) fn take_created_directories(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.created_directories)
    }

    /// Replaces the destination with the prepared file.
    pub(crate) fn commit(mut self) -> std::io::Result<()> {
        replace_file(&self.temporary, &self.destination)?;
        self.temporary.clear();
        self.created_directories.clear();
        Ok(())
    }

    /// Installs the prepared contents only when the destination is absent.
    pub(crate) fn commit_if_absent(mut self) -> std::io::Result<()> {
        move_path_if_absent(&self.temporary, &self.destination)?;
        self.temporary.clear();
        self.created_directories.clear();
        Ok(())
    }

    /// Installs the prepared contents and returns directories created while
    /// preparing the destination.
    pub(crate) fn commit_if_absent_with_created_directories(
        mut self,
    ) -> std::io::Result<Vec<PathBuf>> {
        move_path_if_absent(&self.temporary, &self.destination)?;
        self.temporary.clear();
        Ok(std::mem::take(&mut self.created_directories))
    }
}

/// Atomically moves a filesystem entry without replacing an existing
/// destination.
#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_vendor = "apple",
    target_os = "redox",
))]
pub(crate) fn move_path_if_absent(from: &Path, to: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "android",
        target_os = "linux",
        target_vendor = "apple",
        target_os = "redox",
    )),
))]
#[cfg_attr(test, mutants::skip)]
pub(crate) fn move_path_if_absent(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this platform has no atomic no-replace move for directories",
    ))
}

#[cfg(windows)]
#[cfg_attr(test, mutants::skip)]
pub(crate) fn move_path_if_absent(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file_ex(from, to, 0)
}

#[cfg(not(any(unix, windows)))]
#[cfg_attr(test, mutants::skip)]
pub(crate) fn move_path_if_absent(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this platform has no atomic no-replace move for directories",
    ))
}

impl Drop for PreparedAtomicWrite {
    fn drop(&mut self) {
        if !self.temporary.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.temporary);
        }
        remove_created_directories(&mut self.created_directories);
    }
}

impl Repository {
    /// Name of the Sacho configuration file.
    pub const CONFIG_FILE: &'static str = "sacho.toml";

    /// Opens the nearest existing Sacho repository at or above `start`.
    pub fn open_existing(start: impl AsRef<Path>) -> Result<Self> {
        let start = start.as_ref();
        let config_path =
            Self::discover_config(start).ok_or_else(|| crate::Error::ConfigNotFound {
                start: start.to_path_buf(),
            })?;
        let root = config_path
            .parent()
            .expect("configuration file path always has a parent")
            .to_path_buf();
        Self::from_root(root)
    }

    /// Opens a Sacho repository whose root is already known.
    pub fn from_root(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let lock_path = mutation_lock_path(&root);
        Self::from_root_with_mutation_lock_policy(root, &lock_path, false)
    }

    pub(crate) fn from_root_with_mutation_lock(
        root: impl AsRef<Path>,
        lock_path: &Path,
    ) -> Result<Self> {
        Self::from_root_with_mutation_lock_policy(root, lock_path, true)
    }

    fn from_root_with_mutation_lock_policy(
        root: impl AsRef<Path>,
        lock_path: &Path,
        fixed_lock_path: bool,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let config_path = root.join(Self::CONFIG_FILE);
        let contents = fs::read_to_string(&config_path).context(ReadFileSnafu {
            path: config_path.clone(),
        })?;
        let mut config = Config::parse(&contents).context(ConfigSnafu { path: config_path })?;
        let source_config = config.clone();
        let mut validation_cache = PathValidationCache::default();

        let configured =
            validate_repository_config_paths_with_cache(&root, &config, &mut validation_cache)
                .context(ConfigSnafu {
                    path: root.join(Self::CONFIG_FILE),
                })?;
        validate_configured_paths_against_reserved_with_cache(
            &configured,
            "repository mutation lock",
            lock_path,
            &mut validation_cache,
        )
        .context(ConfigSnafu {
            path: root.join(Self::CONFIG_FILE),
        })?;
        normalize_repository_config_paths(&root, &mut config, &configured);

        Ok(Self {
            root,
            config,
            source_config,
            fixed_mutation_lock_path: fixed_lock_path.then(|| lock_path.to_path_buf()),
        })
    }

    /// Finds the nearest `sacho.toml` at or above `start`.
    pub fn discover_config(start: impl AsRef<Path>) -> Option<PathBuf> {
        let start = start.as_ref();
        let mut current = if start.is_file() {
            start.parent()?.to_path_buf()
        } else {
            start.to_path_buf()
        };

        loop {
            let candidate = current.join(Self::CONFIG_FILE);
            if candidate.is_file() {
                return Some(candidate);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// Returns the repository root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the validated configuration with operational paths resolved.
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn source_config(&self) -> &Config {
        &self.source_config
    }

    pub(crate) fn revalidate_paths(&self) -> Result<()> {
        let lock_path = self
            .fixed_mutation_lock_path
            .clone()
            .unwrap_or_else(|| mutation_lock_path(&self.root));
        let mut validation_cache = PathValidationCache::default();
        let configured = validate_repository_config_paths_with_cache(
            &self.root,
            &self.config,
            &mut validation_cache,
        )
        .context(ConfigSnafu {
            path: self.root.join(Self::CONFIG_FILE),
        })?;
        validate_configured_paths_against_reserved_with_cache(
            &configured,
            "repository mutation lock",
            &lock_path,
            &mut validation_cache,
        )
        .context(ConfigSnafu {
            path: self.root.join(Self::CONFIG_FILE),
        })
    }

    pub(crate) fn validate_pattern_section_directory(&self, directory: &Path) -> Result<PathBuf> {
        SectionResolver::from_config(&self.config)?.ensure_unambiguous_directory(directory)?;
        let lock_path = self
            .fixed_mutation_lock_path
            .clone()
            .unwrap_or_else(|| mutation_lock_path(&self.root));
        let mut validation_cache = PathValidationCache::default();
        let configured = validate_repository_config_paths_with_cache(
            &self.root,
            &self.config,
            &mut validation_cache,
        )
        .context(ConfigSnafu {
            path: self.root.join(Self::CONFIG_FILE),
        })?;
        let [_changelog, fragments, next, sections @ ..] = &configured[..] else {
            unreachable!("repository path validation always returns its three base paths");
        };
        let section = validate_pattern_section_directory_with_cache(
            &self.root,
            &self.config.fragments.directory,
            directory,
            fragments,
            next,
            sections,
            &lock_path,
            &mut validation_cache,
        )
        .context(ConfigSnafu {
            path: self.root.join(Self::CONFIG_FILE),
        })?;
        Ok(section.identity)
    }

    /// Resolves a repository-relative path against the repository root.
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        self.root.join(path)
    }

    /// Writes a file through a temporary file followed by rename.
    pub fn atomic_write(&self, path: impl AsRef<Path>, contents: &[u8]) -> Result<()> {
        let path = path.as_ref();
        let prepared = self.prepare_atomic_write(path, contents)?;
        let from = prepared.temporary.clone();
        let to = prepared.destination.clone();
        prepared.commit().context(RenameFileSnafu { from, to })?;
        Ok(())
    }

    /// Writes a temporary file in the destination directory without replacing
    /// the destination yet.
    pub(crate) fn prepare_atomic_write(
        &self,
        path: impl AsRef<Path>,
        contents: &[u8],
    ) -> Result<PreparedAtomicWrite> {
        self.prepare_atomic_write_avoiding(path, contents, &[])
    }

    /// Writes a uniquely reserved temporary file while avoiding transaction
    /// participants that must remain untouched.
    pub(crate) fn prepare_atomic_write_avoiding(
        &self,
        path: impl AsRef<Path>,
        contents: &[u8],
        forbidden: &[PathBuf],
    ) -> Result<PreparedAtomicWrite> {
        let path = self.resolve(path);
        let parent = path
            .parent()
            .expect("resolved repository paths always have a parent");
        let mut created_directories = create_missing_directories(parent)?;
        let canonical_parent = match fs::canonicalize(parent) {
            Ok(parent) => parent,
            Err(source) => {
                remove_created_directories(&mut created_directories);
                return Err(crate::Error::ReadFile {
                    path: parent.to_path_buf(),
                    source,
                });
            }
        };
        let (tmp_path, mut file) = loop {
            let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!(".sacho-{}-{sequence}.tmp", std::process::id());
            let candidate = parent.join(&name);
            if temporary_path_is_forbidden(&canonical_parent, &name, forbidden) {
                continue;
            }
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => break (candidate, file),
                Err(source) if is_temporary_path_collision(&source) => continue,
                Err(source) => {
                    remove_created_directories(&mut created_directories);
                    return Err(crate::Error::WriteFile {
                        path: candidate,
                        source,
                    });
                }
            }
        };
        if let Err(source) = file.write_all(contents) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            remove_created_directories(&mut created_directories);
            return Err(crate::Error::WriteFile {
                path: tmp_path,
                source,
            });
        }
        drop(file);
        Ok(PreparedAtomicWrite {
            temporary: tmp_path,
            destination: path,
            created_directories,
        })
    }
}

/// Returns the stable mutation-lock path for the active repository kind.
pub(crate) fn mutation_lock_path(root: &Path) -> PathBuf {
    if root.join(".jj").is_dir() {
        return root.join(".jj/sacho.lock");
    }
    if root.join(".hg").is_dir() {
        return root.join(".hg/sacho.lock");
    }
    let git_path = Command::new("git")
        .args(["rev-parse", "--git-path", "sacho.lock"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let output = String::from_utf8_lossy(&output.stdout);
            let path = output.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        });
    git_path
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join(MUTATION_LOCK_FILE))
}

/// Resolves and validates every configured repository path.
#[cfg(test)]
pub(crate) fn validate_repository_config_paths(
    root: &Path,
    config: &Config,
) -> std::result::Result<Vec<ConfiguredPathIdentity>, ConfigError> {
    validate_repository_config_paths_with_cache(root, config, &mut PathValidationCache::default())
}

pub(crate) fn validate_repository_config_paths_with_cache(
    root: &Path,
    config: &Config,
    validation_cache: &mut PathValidationCache,
) -> std::result::Result<Vec<ConfiguredPathIdentity>, ConfigError> {
    let root_identity =
        filesystem_path_identity(root).map_err(|source| ConfigError::ConfigPathResolution {
            key: String::from("repository root"),
            path: root.to_path_buf(),
            effective_path: root.to_path_buf(),
            source,
        })?;
    let changelog = configured_path_identity(
        "changelog.path",
        &config.changelog.path,
        root.join(&config.changelog.path),
        validation_cache,
    )?;
    validate_strict_descendant(&changelog, "repository root", root, &root_identity)?;
    let fragments = configured_path_identity(
        "fragments.directory",
        &config.fragments.directory,
        root.join(&config.fragments.directory),
        validation_cache,
    )?;
    validate_strict_descendant(&fragments, "repository root", root, &root_identity)?;

    validate_path_overlap(&changelog, &fragments)?;

    let next = configured_path_identity(
        "fragments.next-file",
        &config.fragments.next_file,
        root.join(&config.fragments.directory)
            .join(&config.fragments.next_file),
        validation_cache,
    )?;
    validate_next_file_is_not_markdown(&next.identity, &config.fragments.next_file)?;
    validate_strict_descendant(
        &next,
        "fragments.directory",
        &config.fragments.directory,
        &fragments.identity,
    )?;

    let mut sections = Vec::<ConfiguredPathIdentity>::with_capacity(config.sections.len());
    for (index, section) in config.sections.iter().enumerate() {
        let configured = configured_path_identity(
            &format!("sections[{index}].directory"),
            &section.directory,
            root.join(&config.fragments.directory)
                .join(&section.directory),
            validation_cache,
        )?;
        validate_strict_descendant(
            &configured,
            "fragments.directory",
            &config.fragments.directory,
            &fragments.identity,
        )?;
        for earlier in &sections {
            if configured_paths_equal(&configured, earlier) {
                return Err(ConfigError::ConfigPathOverlap {
                    first_key: earlier.key.clone(),
                    first_path: earlier.configured.clone(),
                    second_key: configured.key,
                    second_path: configured.configured,
                });
            }
        }
        validate_path_overlap(&next, &configured)?;
        sections.push(configured);
    }

    let config_path = configured_path_identity(
        Repository::CONFIG_FILE,
        Path::new(Repository::CONFIG_FILE),
        root.join(Repository::CONFIG_FILE),
        validation_cache,
    )?;
    let mut configured = vec![changelog, fragments, next];
    configured.extend(sections);
    for path in &configured {
        validate_path_overlap(path, &config_path)?;
    }
    Ok(configured)
}

pub(crate) fn validate_pattern_section_directories(
    root: &Path,
    fragment_directory: &Path,
    next_file: &Path,
    directories: &[PathBuf],
    lock_path: &Path,
) -> std::result::Result<(), ConfigError> {
    let mut validation_cache = PathValidationCache::default();
    let root_identity =
        filesystem_path_identity(root).map_err(|source| ConfigError::ConfigPathResolution {
            key: String::from("repository root"),
            path: root.to_path_buf(),
            effective_path: root.to_path_buf(),
            source,
        })?;
    let fragments = configured_path_identity(
        "fragments.directory",
        fragment_directory,
        root.join(fragment_directory),
        &mut validation_cache,
    )?;
    validate_strict_descendant(&fragments, "repository root", root, &root_identity)?;
    let next = configured_path_identity(
        "fragments.next-file",
        next_file,
        root.join(fragment_directory).join(next_file),
        &mut validation_cache,
    )?;
    validate_next_file_is_not_markdown(&next.identity, next_file)?;
    validate_strict_descendant(
        &next,
        "fragments.directory",
        fragment_directory,
        &fragments.identity,
    )?;

    let mut sections = Vec::with_capacity(directories.len());
    for directory in directories {
        let section = validate_pattern_section_directory_with_cache(
            root,
            fragment_directory,
            directory,
            &fragments,
            &next,
            &sections,
            lock_path,
            &mut validation_cache,
        )?;
        sections.push(section);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_pattern_section_directory_with_cache(
    root: &Path,
    fragment_directory: &Path,
    directory: &Path,
    fragments: &ConfiguredPathIdentity,
    next: &ConfiguredPathIdentity,
    sections: &[ConfiguredPathIdentity],
    lock_path: &Path,
    validation_cache: &mut PathValidationCache,
) -> std::result::Result<ConfiguredPathIdentity, ConfigError> {
    let section = configured_path_identity(
        "resolved section-patterns[].directory",
        directory,
        root.join(fragment_directory).join(directory),
        validation_cache,
    )?;
    validate_strict_descendant(
        &section,
        "fragments.directory",
        fragment_directory,
        &fragments.identity,
    )?;
    if section.identity != fragments.identity.join(directory) {
        return Err(ConfigError::InvalidPath {
            key: String::from("resolved section-patterns[].directory"),
            path: directory.to_path_buf(),
            reason: "must resolve to exactly its rendered path below fragments.directory",
        });
    }
    validate_path_overlap(next, &section)?;
    for other in sections {
        if configured_paths_equal(other, &section) {
            return Err(ConfigError::ConfigPathOverlap {
                first_key: other.key.clone(),
                first_path: other.configured.clone(),
                second_key: section.key.clone(),
                second_path: section.configured.clone(),
            });
        }
    }
    validate_configured_paths_against_reserved_with_cache(
        std::slice::from_ref(&section),
        "repository mutation lock",
        lock_path,
        validation_cache,
    )?;
    Ok(section)
}

fn configured_paths_equal(first: &ConfiguredPathIdentity, second: &ConfiguredPathIdentity) -> bool {
    configured_path_starts_with(first, second) && configured_path_starts_with(second, first)
}

/// Rejects any configured path that overlaps a reserved filesystem path.
pub(crate) fn validate_configured_paths_against_reserved_with_cache(
    configured: &[ConfiguredPathIdentity],
    reserved_key: &str,
    reserved_path: &Path,
    validation_cache: &mut PathValidationCache,
) -> std::result::Result<(), ConfigError> {
    let reserved = configured_path_identity(
        reserved_key,
        reserved_path,
        reserved_path.to_path_buf(),
        validation_cache,
    )?;
    for path in configured {
        validate_path_overlap(path, &reserved)?;
    }
    Ok(())
}

pub(crate) fn normalize_repository_config_paths(
    root: &Path,
    config: &mut Config,
    configured: &[ConfiguredPathIdentity],
) {
    let root_identity = filesystem_path_identity(root)
        .expect("repository root identity was resolved during path validation");
    let [changelog, fragments, next, sections @ ..] = configured else {
        unreachable!("repository path validation always returns its three base paths");
    };
    config.changelog.path = changelog
        .identity
        .strip_prefix(&root_identity)
        .expect("validated changelog remains below the repository root")
        .to_path_buf();
    config.fragments.directory = fragments
        .identity
        .strip_prefix(&root_identity)
        .expect("validated fragment directory remains below the repository root")
        .to_path_buf();
    config.fragments.next_file = next
        .identity
        .strip_prefix(&fragments.identity)
        .expect("validated next-version path remains below the fragment directory")
        .to_path_buf();
    for (section, configured) in config.sections.iter_mut().zip(sections) {
        section.directory = configured
            .identity
            .strip_prefix(&fragments.identity)
            .expect("validated section remains below the fragment directory")
            .to_path_buf();
    }
}

fn configured_path_identity(
    key: &str,
    configured: &Path,
    effective: PathBuf,
    validation_cache: &mut PathValidationCache,
) -> std::result::Result<ConfiguredPathIdentity, ConfigError> {
    let identity = filesystem_path_identity(&effective).map_err(|source| {
        ConfigError::ConfigPathResolution {
            key: key.to_owned(),
            path: configured.to_path_buf(),
            effective_path: effective.clone(),
            source,
        }
    })?;
    let component_case_sensitivity = validation_cache
        .path_component_case_sensitivity(&identity)
        .map_err(|source| ConfigError::ConfigPathResolution {
            key: key.to_owned(),
            path: configured.to_path_buf(),
            effective_path: effective,
            source,
        })?;
    Ok(ConfiguredPathIdentity {
        key: key.to_owned(),
        configured: configured.to_path_buf(),
        identity,
        component_case_sensitivity,
    })
}

fn validate_strict_descendant(
    path: &ConfiguredPathIdentity,
    boundary_key: &str,
    boundary: &Path,
    boundary_identity: &Path,
) -> std::result::Result<(), ConfigError> {
    if path.identity == boundary_identity || !path.identity.starts_with(boundary_identity) {
        return Err(ConfigError::ConfigPathOutsideBoundary {
            key: path.key.clone(),
            path: path.configured.clone(),
            resolved: path.identity.clone(),
            boundary_key: boundary_key.to_owned(),
            boundary: boundary.to_path_buf(),
        });
    }
    Ok(())
}

fn validate_path_overlap(
    first: &ConfiguredPathIdentity,
    second: &ConfiguredPathIdentity,
) -> std::result::Result<(), ConfigError> {
    if configured_path_starts_with(first, second) || configured_path_starts_with(second, first) {
        return Err(ConfigError::ConfigPathOverlap {
            first_key: first.key.clone(),
            first_path: first.configured.clone(),
            second_key: second.key.clone(),
            second_path: second.configured.clone(),
        });
    }
    Ok(())
}

fn configured_path_starts_with(
    path: &ConfiguredPathIdentity,
    base: &ConfiguredPathIdentity,
) -> bool {
    let mut path_components = path.identity.components().enumerate();
    base.identity
        .components()
        .enumerate()
        .all(|(base_index, base_component)| {
            path_components
                .next()
                .is_some_and(|(path_index, path_component)| {
                    let case_sensitive = path.component_case_sensitivity[path_index]
                        && base.component_case_sensitivity[base_index];
                    if case_sensitive {
                        path_component == base_component
                    } else {
                        filesystem_case_insensitive_component_eq(
                            path_component.as_os_str(),
                            base_component.as_os_str(),
                        )
                    }
                })
        })
}

pub(crate) fn filesystem_paths_overlap(first: &Path, second: &Path, case_sensitive: bool) -> bool {
    path_starts_with(first, second, case_sensitive)
        || path_starts_with(second, first, case_sensitive)
}

#[cfg(test)]
fn filesystem_paths_equal(first: &Path, second: &Path, case_sensitive: bool) -> bool {
    path_starts_with(first, second, case_sensitive)
        && path_starts_with(second, first, case_sensitive)
}

fn path_starts_with(path: &Path, base: &Path, case_sensitive: bool) -> bool {
    if case_sensitive {
        return path.starts_with(base);
    }
    let mut path_components = path.components();
    base.components().all(|base_component| {
        path_components.next().is_some_and(|path_component| {
            filesystem_case_insensitive_component_eq(
                path_component.as_os_str(),
                base_component.as_os_str(),
            )
        })
    })
}

#[cfg(not(windows))]
fn filesystem_case_insensitive_component_eq(first: &OsStr, second: &OsStr) -> bool {
    match (first.to_str(), second.to_str()) {
        (Some(first), Some(second)) => caseless::default_caseless_match_str(first, second),
        _ => first == second,
    }
}

#[cfg(windows)]
#[cfg_attr(test, mutants::skip)]
fn filesystem_case_insensitive_component_eq(first: &OsStr, second: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

    let first = first.encode_wide().collect::<Vec<_>>();
    let second = second.encode_wide().collect::<Vec<_>>();
    let (Ok(first_len), Ok(second_len)) = (i32::try_from(first.len()), i32::try_from(second.len()))
    else {
        return false;
    };
    // SAFETY: the slices remain alive for the call and the explicit lengths
    // describe their complete UTF-16 contents.
    unsafe {
        CompareStringOrdinal(first.as_ptr(), first_len, second.as_ptr(), second_len, 1)
            == CSTR_EQUAL
    }
}

#[cfg(test)]
fn filesystem_path_case_sensitive(path: &Path) -> std::io::Result<bool> {
    PathValidationCache::default().filesystem_path_case_sensitive(path)
}

impl PathValidationCache {
    fn path_component_case_sensitivity(&mut self, path: &Path) -> std::io::Result<Vec<bool>> {
        let mut parent = PathBuf::new();
        path.components()
            .map(|component| {
                let case_sensitive = match component {
                    Component::Normal(_) => self.filesystem_path_case_sensitive(&parent)?,
                    Component::Prefix(_) | Component::RootDir => true,
                    Component::CurDir | Component::ParentDir => true,
                };
                parent.push(component.as_os_str());
                Ok(case_sensitive)
            })
            .collect()
    }

    pub(crate) fn filesystem_path_case_sensitive(&mut self, path: &Path) -> std::io::Result<bool> {
        let directory = path
            .ancestors()
            .find(|ancestor| ancestor.is_dir())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{} has no existing directory ancestor", path.display()),
                )
            })?;
        if let Some(case_sensitive) = self.case_sensitivity.get(directory) {
            return Ok(*case_sensitive);
        }
        if let Some(case_sensitive) = platform_directory_case_sensitivity(directory)? {
            self.case_sensitivity
                .insert(directory.to_path_buf(), case_sensitive);
            return Ok(case_sensitive);
        }
        if let Some(case_sensitive) = case_sensitivity_from_entries(directory)? {
            self.case_sensitivity
                .insert(directory.to_path_buf(), case_sensitive);
            return Ok(case_sensitive);
        }
        for ancestor in directory.ancestors().skip(1) {
            if let Some(case_sensitive) = case_sensitivity_from_entries(ancestor)? {
                self.case_sensitivity
                    .insert(directory.to_path_buf(), case_sensitive);
                return Ok(case_sensitive);
            }
        }
        let case_sensitive = default_filesystem_case_sensitive();
        self.case_sensitivity
            .insert(directory.to_path_buf(), case_sensitive);
        Ok(case_sensitive)
    }
}

#[cfg_attr(test, mutants::skip)]
fn default_filesystem_case_sensitive() -> bool {
    !cfg!(windows)
}

fn case_sensitivity_from_entries(directory: &Path) -> std::io::Result<Option<bool>> {
    #[cfg(test)]
    CASE_SENSITIVITY_DIRECTORY_SCANS.set(CASE_SENSITIVITY_DIRECTORY_SCANS.get() + 1);
    let names = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    for name in &names {
        let Some(alternate_name) = alternate_ascii_case(name) else {
            continue;
        };
        if names.contains(&alternate_name) {
            return Ok(Some(true));
        }
        match fs::symlink_metadata(directory.join(alternate_name)) {
            Ok(_) => return Ok(Some(false)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(true));
            }
            Err(source) => return Err(source),
        }
    }
    Ok(None)
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn platform_directory_case_sensitivity(directory: &Path) -> std::io::Result<Option<bool>> {
    use rustix::ffi::c_long;
    use rustix::io::Errno;
    use rustix::ioctl::{Getter, ioctl, opcode};

    const FS_CASEFOLD_FL: c_long = 0x4000_0000;
    const FS_IOC_GETFLAGS: rustix::ioctl::Opcode = opcode::read::<c_long>(b'f', 1);

    let file = fs::File::open(directory)?;
    // SAFETY: FS_IOC_GETFLAGS writes one c_long to the getter buffer and has
    // no filesystem side effects.
    match unsafe { ioctl(&file, Getter::<FS_IOC_GETFLAGS, c_long>::new()) } {
        Ok(flags) => Ok(Some(flags & FS_CASEFOLD_FL == 0)),
        Err(Errno::INVAL | Errno::NOTTY | Errno::OPNOTSUPP) => Ok(None),
        Err(source) => Err(source.into()),
    }
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(test, mutants::skip)]
fn platform_directory_case_sensitivity(directory: &Path) -> std::io::Result<Option<bool>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(directory.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory path contains a null byte",
        )
    })?;
    // SAFETY: path is a live, null-terminated pathname and pathconf only reads
    // filesystem metadata.
    let result = unsafe { libc::pathconf(path.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(Some(result != 0))
    }
}

#[cfg(windows)]
#[cfg_attr(test, mutants::skip)]
fn platform_directory_case_sensitivity(directory: &Path) -> std::io::Result<Option<bool>> {
    use std::mem::size_of;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FileCaseSensitiveInfo,
        GetFileInformationByHandleEx,
    };
    use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)?;
    let mut info = FILE_CASE_SENSITIVE_INFO::default();
    // SAFETY: the handle remains open and info is a correctly sized output
    // buffer for FileCaseSensitiveInfo.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&raw mut info).cast(),
            size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    };
    if success != 0 {
        Ok(Some(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0))
    } else {
        let source = std::io::Error::last_os_error();
        match source.raw_os_error() {
            Some(1 | 50 | 87) => Ok(None),
            _ => Err(source),
        }
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_vendor = "apple",
    windows,
)))]
#[cfg_attr(test, mutants::skip)]
fn platform_directory_case_sensitivity(_directory: &Path) -> std::io::Result<Option<bool>> {
    Ok(None)
}

fn alternate_ascii_case(name: &OsStr) -> Option<OsString> {
    let mut value = name.to_str()?.as_bytes().to_vec();
    let byte = value.iter_mut().find(|byte| byte.is_ascii_alphabetic())?;
    if byte.is_ascii_lowercase() {
        byte.make_ascii_uppercase();
    } else {
        byte.make_ascii_lowercase();
    }
    Some(
        String::from_utf8(value)
            .expect("changing ASCII case preserves UTF-8")
            .into(),
    )
}

/// Resolves existing components in filesystem order while retaining a missing suffix.
pub(crate) fn filesystem_path_identity(path: &Path) -> std::io::Result<PathBuf> {
    let unresolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    let mut missing = Vec::new();
    for component in unresolved.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if missing.pop().is_none() {
                    resolved = fs::canonicalize(resolved.join(".."))?;
                }
            }
            Component::Normal(name) if missing.is_empty() => {
                let candidate = resolved.join(name);
                match fs::canonicalize(&candidate) {
                    Ok(canonical) => resolved = canonical,
                    Err(source) => {
                        if source.kind() != std::io::ErrorKind::NotFound {
                            return Err(source);
                        }
                        match fs::symlink_metadata(&candidate) {
                            Ok(metadata) if metadata.file_type().is_symlink() => {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::NotFound,
                                    format!(
                                        "symbolic link {} has no resolvable target",
                                        candidate.display()
                                    ),
                                ));
                            }
                            Ok(_) => return Err(source),
                            Err(metadata_error)
                                if metadata_error.kind() == std::io::ErrorKind::NotFound =>
                            {
                                missing.push(name.to_os_string());
                            }
                            Err(metadata_error) => return Err(metadata_error),
                        }
                    }
                }
            }
            Component::Normal(name) => missing.push(name.to_os_string()),
        }
    }
    resolved.extend(missing);
    Ok(resolved)
}

fn create_missing_directories(path: &Path) -> Result<Vec<PathBuf>> {
    let mut missing = path
        .ancestors()
        .take_while(|ancestor| !ancestor.exists())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    missing.reverse();
    let mut created = Vec::with_capacity(missing.len());
    for directory in missing {
        match fs::create_dir(&directory) {
            Ok(()) => created.push(directory),
            Err(source) if directory_was_created_concurrently(&source) => {}
            Err(source) => {
                remove_created_directories(&mut created);
                return Err(crate::Error::CreateDirectory {
                    path: directory,
                    source,
                });
            }
        }
    }
    Ok(created)
}

fn directory_was_created_concurrently(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists
}

fn remove_created_directories(directories: &mut Vec<PathBuf>) {
    for directory in directories.drain(..).rev() {
        let _ = fs::remove_dir(directory);
    }
}

fn temporary_path_is_forbidden(canonical_parent: &Path, name: &str, forbidden: &[PathBuf]) -> bool {
    let candidate = canonical_parent.join(name);
    forbidden
        .iter()
        .any(|participant| participant.starts_with(&candidate))
}

fn is_temporary_path_collision(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
#[cfg_attr(test, mutants::skip)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file_ex(
        from,
        to,
        windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING,
    )
}

#[cfg(windows)]
#[cfg_attr(test, mutants::skip)]
fn move_file_ex(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from = from
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let to = to
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();

    // SAFETY: The path buffers are null-terminated and live for the duration of
    // the call. The caller selects whether an existing destination may be
    // replaced through `flags`.
    let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;

    fn path_segments() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("dir-[a-z][a-z0-9_-]{0,8}", 1..8)
    }

    #[test]
    fn opens_repository_from_nested_directory() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::create_dir_all(temp.path().join("a/b")).expect("nested dir");

        let repo = Repository::open_existing(temp.path().join("a/b")).expect("repository");

        assert_eq!(repo.root(), temp.path());
    }

    #[test]
    fn rejects_changelog_and_fragment_directory_overlap_when_opening_repository() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\npath = \"state/CHANGES.md\"\n[fragments]\ndirectory = \"state\"\n",
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("overlapping paths");
        let message = error.to_string();

        assert!(message.contains("changelog.path"), "{message}");
        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("overlap"), "{message}");
    }

    #[test]
    fn rejects_mutation_lock_overlap_when_opening_repository() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \".sacho.lock\"\n",
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("mutation lock overlap");
        let message = error.to_string();

        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("repository mutation lock"), "{message}");
        assert!(message.contains("overlap"), "{message}");
    }

    #[test]
    fn read_revalidation_uses_the_current_vcs_mutation_lock() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \".git\"\n",
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("markerless repository");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed with {status}");

        let error = crate::fragment::discover_fragment_candidates(&repo)
            .expect_err("new Git mutation lock overlap");
        let message = error.to_string();

        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("repository mutation lock"), "{message}");
        assert!(message.contains("overlap"), "{message}");
    }

    #[test]
    fn git_mutation_lock_lives_in_git_metadata() {
        let temp = TempDir::new().expect("tempdir");
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed with {status}");

        assert_eq!(
            mutation_lock_path(temp.path()),
            temp.path().join(".git/sacho.lock")
        );
    }

    #[test]
    fn normalizes_validated_paths_for_repository_operations() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
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
        .expect("config");

        let repo = Repository::from_root(temp.path()).expect("repository");

        assert_eq!(repo.config().changelog.path, Path::new("docs/CHANGES.md"));
        assert_eq!(
            repo.config().fragments.directory,
            Path::new("state/changes.d")
        );
        assert_eq!(
            repo.config().fragments.next_file,
            Path::new("metadata/next")
        );
        assert_eq!(
            repo.config().sections[0].directory,
            Path::new("packages/core")
        );
    }

    #[test]
    fn filesystem_path_overlap_honors_case_sensitivity_for_missing_paths() {
        let upper = Path::new("/repository/State");
        let lower = Path::new("/repository/state");
        let lower_child = Path::new("/repository/state/CHANGES.md");
        let greek_sigma = Path::new("/repository/\u{03a3}");
        let greek_final_sigma = Path::new("/repository/\u{03c2}");

        assert!(!filesystem_paths_overlap(upper, lower, true));
        assert!(filesystem_paths_overlap(upper, lower, false));
        assert!(filesystem_paths_overlap(upper, lower_child, false));
        assert!(!filesystem_paths_equal(upper, lower, true));
        assert!(filesystem_paths_equal(upper, lower, false));
        assert!(!filesystem_paths_equal(upper, lower_child, false));
        assert!(filesystem_paths_equal(
            greek_sigma,
            greek_final_sigma,
            false
        ));
    }

    #[cfg(unix)]
    #[test]
    fn case_insensitive_comparison_preserves_non_utf8_components() {
        use std::os::unix::ffi::OsStrExt;

        let first = OsStr::from_bytes(b"invalid-\xff");
        let same = OsStr::from_bytes(b"invalid-\xff");
        let different = OsStr::from_bytes(b"invalid-\xfe");

        assert!(filesystem_case_insensitive_component_eq(first, same));
        assert!(!filesystem_case_insensitive_component_eq(first, different));
    }

    #[cfg(windows)]
    #[test]
    fn case_insensitive_comparison_uses_windows_ordinal_rules() {
        assert!(filesystem_case_insensitive_component_eq(
            OsStr::new("I"),
            OsStr::new("\u{0131}")
        ));
    }

    #[test]
    fn overlap_validation_is_case_insensitive_if_either_filesystem_is() {
        let first = ConfiguredPathIdentity {
            key: String::from("first"),
            configured: PathBuf::from("State/CHANGES.md"),
            identity: PathBuf::from("/repository/State/CHANGES.md"),
            component_case_sensitivity: vec![true, true, true, true],
        };
        let second = ConfiguredPathIdentity {
            key: String::from("second"),
            configured: PathBuf::from("state"),
            identity: PathBuf::from("/repository/state"),
            component_case_sensitivity: vec![true, true, false],
        };

        assert!(matches!(
            validate_path_overlap(&first, &second),
            Err(ConfigError::ConfigPathOverlap { .. })
        ));
    }

    #[test]
    fn configured_path_equality_is_case_insensitive_if_either_filesystem_is() {
        let first = ConfiguredPathIdentity {
            key: String::from("first"),
            configured: PathBuf::from("State"),
            identity: PathBuf::from("/repository/State"),
            component_case_sensitivity: vec![true, true, true],
        };
        let second = ConfiguredPathIdentity {
            key: String::from("second"),
            configured: PathBuf::from("state"),
            identity: PathBuf::from("/repository/state"),
            component_case_sensitivity: vec![true, true, false],
        };

        assert!(configured_paths_equal(&first, &second));
    }

    #[test]
    fn sibling_directory_spelling_uses_the_parent_case_sensitivity() {
        let upper = ConfiguredPathIdentity {
            key: String::from("upper"),
            configured: PathBuf::from("Core"),
            identity: PathBuf::from("/repository/Core"),
            component_case_sensitivity: vec![true, true, true],
        };
        let lower = ConfiguredPathIdentity {
            key: String::from("lower"),
            configured: PathBuf::from("core"),
            identity: PathBuf::from("/repository/core"),
            component_case_sensitivity: vec![true, true, true],
        };

        assert!(!configured_paths_equal(&upper, &lower));
    }

    #[test]
    fn repository_validation_scans_each_case_sensitivity_directory_once() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("ProbeEntry"), "probe\n").expect("probe entry");
        let config = Config::parse(
            r#"
            [[sections]]
            id = "core"
            directory = "core"

            [[sections]]
            id = "cli"
            directory = "cli"
            "#,
        )
        .expect("config");
        CASE_SENSITIVITY_DIRECTORY_SCANS.set(0);

        validate_repository_config_paths(temp.path(), &config).expect("path validation");

        assert!(CASE_SENSITIVITY_DIRECTORY_SCANS.get() <= 1);
    }

    #[test]
    fn detects_the_containing_filesystem_case_sensitivity_without_writing_a_probe() {
        let temp = TempDir::new().expect("tempdir");
        let mixed_case = temp.path().join("Case-Sensitivity-Aa");
        let alternate_case = temp.path().join("case-sensitivity-aA");
        fs::write(&mixed_case, "probe\n").expect("mixed-case file");
        let expected = !alternate_case.exists();

        let detected = filesystem_path_case_sensitive(&temp.path().join("missing/path"))
            .expect("case sensitivity");

        assert_eq!(detected, expected);
        assert_eq!(
            fs::read_dir(temp.path()).expect("tempdir entries").count(),
            1,
            "case-sensitivity detection must be read-only"
        );
    }

    #[test]
    fn entry_fallback_detects_case_sensitivity_from_an_alternate_name() {
        let temp = TempDir::new().expect("tempdir");
        let mixed_case = temp.path().join("CaseProbe");
        let alternate_case = temp.path().join("caseProbe");
        fs::write(&mixed_case, "probe\n").expect("mixed-case probe");
        let expected = fs::symlink_metadata(&alternate_case).is_err();

        let detected = case_sensitivity_from_entries(temp.path()).expect("entry fallback");

        assert_eq!(detected, Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn detects_case_sensitivity_by_looking_up_entries_inside_the_directory() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let directory = temp.path().join("CaseLookup");
        let directory_alias = temp.path().join("caseLookup");
        fs::create_dir(&directory).expect("lookup directory");
        fs::write(directory.join("EntryProbe"), "probe\n").expect("lookup entry");
        if directory_alias.exists() {
            return;
        }
        symlink("CaseLookup", &directory_alias).expect("directory case alias");

        assert!(
            filesystem_path_case_sensitive(&directory.join("missing")).expect("case sensitivity")
        );
    }

    #[test]
    fn case_sensitivity_detection_compares_existing_case_aliases() {
        let temp = TempDir::new().expect("tempdir");
        let directory = temp.path().join("lookup");
        let mixed_case = directory.join("CaseProbe");
        let alternate_case = directory.join("caseProbe");
        fs::create_dir(&directory).expect("lookup directory");
        fs::write(&mixed_case, "probe\n").expect("mixed-case entry");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&alternate_case)
        {
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => panic!("alternate-case entry: {source}"),
        }
        let expected = fs::canonicalize(&mixed_case).expect("mixed-case identity")
            != fs::canonicalize(&alternate_case).expect("alternate-case identity");

        assert_eq!(
            filesystem_path_case_sensitive(&directory.join("missing")).expect("case sensitivity"),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn case_sensitivity_detection_handles_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let directory = temp.path().join("lookup");
        let mixed_case = directory.join("DanglingProbe");
        let alternate_case = directory.join("danglingProbe");
        fs::create_dir(&directory).expect("lookup directory");
        symlink("missing-target", &mixed_case).expect("dangling symlink");
        let expected = fs::symlink_metadata(&alternate_case).is_err();

        assert_eq!(
            filesystem_path_case_sensitive(&directory.join("missing")).expect("case sensitivity"),
            expected
        );
    }

    #[test]
    fn case_sensitivity_detection_matches_an_empty_directory() {
        let temp = TempDir::new().expect("tempdir");
        let empty = temp.path().join("empty");
        fs::create_dir(&empty).expect("empty directory");

        let detected =
            filesystem_path_case_sensitive(&empty.join("missing")).expect("case sensitivity");
        assert_eq!(
            fs::read_dir(&empty)
                .expect("empty directory entries")
                .count(),
            0,
            "case-sensitivity detection must clean up its probe"
        );

        let mixed_case = empty.join("CaseProbe");
        let alternate_case = empty.join("caseProbe");
        fs::write(&mixed_case, "probe\n").expect("mixed-case probe");
        let case_sensitive = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&alternate_case)
        {
            Ok(_) => true,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(source) => panic!("alternate-case probe: {source}"),
        };

        assert_eq!(detected, case_sensitive);
    }

    #[test]
    fn case_sensitivity_fallback_matches_the_platform_default() {
        assert_eq!(default_filesystem_case_sensitive(), !cfg!(windows));
    }

    #[test]
    fn empty_fragment_directory_accepts_distinct_case_only_section_names() {
        let temp = TempDir::new().expect("tempdir");
        let fragments = temp.path().join("fragments");
        let upper = fragments.join("Core");
        let lower = fragments.join("core");
        fs::create_dir(&fragments).expect("fragment directory");
        fs::write(&upper, "probe\n").expect("uppercase probe");
        let case_sensitive = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lower)
        {
            Ok(_) => true,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(source) => panic!("lowercase probe: {source}"),
        };
        fs::remove_file(&upper).expect("remove uppercase probe");
        if case_sensitive {
            fs::remove_file(&lower).expect("remove lowercase probe");
        }
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [fragments]
            directory = "fragments"

            [[sections]]
            id = "upper"
            directory = "Core"

            [[sections]]
            id = "lower"
            directory = "core"
            "#,
        )
        .expect("config");

        let result = Repository::from_root(temp.path());

        assert_eq!(result.is_ok(), case_sensitive, "{result:?}");
        assert_eq!(
            fs::read_dir(&fragments)
                .expect("fragment directory entries")
                .count(),
            0,
            "repository validation must clean up its probe"
        );
    }

    #[test]
    fn rejects_next_version_path_inside_a_section_directory() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [fragments]
            next-file = "core/next"

            [[sections]]
            id = "core"
            directory = "core"
            "#,
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("next/section overlap");
        let message = error.to_string();

        assert!(message.contains("fragments.next-file"), "{message}");
        assert!(message.contains("sections[0].directory"), "{message}");
    }

    #[test]
    fn inferred_pattern_validation_rejects_a_markdown_next_file() {
        let temp = TempDir::new().expect("tempdir");

        let error = validate_pattern_section_directories(
            temp.path(),
            Path::new("changes.d"),
            Path::new("next.md"),
            &[PathBuf::from("core")],
            &temp.path().join(MUTATION_LOCK_FILE),
        )
        .expect_err("Markdown next-file");

        assert!(matches!(
            error,
            ConfigError::InvalidPath {
                key,
                path,
                reason: "must not name a Markdown fragment",
            } if key == "fragments.next-file" && path == Path::new("next.md")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_markdown_next_file_after_resolving_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\nnext-file = \"next\"\n",
        )
        .expect("config");
        fs::create_dir(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(temp.path().join("changes.d/version.md"), " -  Fragment.\n")
            .expect("Markdown fragment");
        symlink("version.md", temp.path().join("changes.d/next")).expect("next-file symlink");

        let error = Repository::from_root(temp.path()).expect_err("resolved Markdown next-file");
        let message = error.to_string();

        assert!(message.contains("fragments.next-file"), "{message}");
        assert!(message.contains("Markdown fragment"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn opens_repository_with_an_empty_read_only_fragment_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().expect("tempdir");
        let fragments = temp.path().join("changes.d");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::create_dir(&fragments).expect("fragment directory");
        let original_permissions = fs::metadata(&fragments)
            .expect("fragment directory metadata")
            .permissions();
        fs::set_permissions(&fragments, fs::Permissions::from_mode(0o555))
            .expect("read-only fragment directory");

        let result = Repository::from_root(temp.path());

        fs::set_permissions(&fragments, original_permissions).expect("restore fragment directory");
        result.expect("read-only repository");
    }

    #[test]
    fn permits_nested_but_distinct_section_directories() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "packages"
            directory = "packages"

            [[sections]]
            id = "core"
            directory = "packages/core"
            "#,
        )
        .expect("config");

        Repository::from_root(temp.path()).expect("nested sections");
    }

    #[test]
    fn permits_a_generated_section_nested_below_an_explicit_section() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "packages"
            directory = "packages"

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "packages/{name}"
            "#,
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("repository");

        repo.validate_pattern_section_directory(Path::new("packages/core"))
            .expect("nested generated section");
    }

    #[test]
    fn rejects_a_generated_section_equal_to_an_explicit_section() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "core"
            directory = "packages/core"

            [[section-patterns]]
            source = "packages/{name}"
            id = "pkg/{name}"
            directory = "packages/{name}"
            "#,
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("repository");

        let error = repo
            .validate_pattern_section_directory(Path::new("packages/core"))
            .expect_err("equal generated section");
        let message = error.to_string();

        assert!(message.contains("sections[0].directory"), "{message}");
        assert!(
            message.contains("resolved section-patterns[].directory"),
            "{message}"
        );
    }

    #[test]
    fn rejects_a_generated_directory_shared_by_multiple_patterns() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[section-patterns]]
            source = "packages/{name}"
            id = "package/{name}"
            directory = "generated/{name}"

            [[section-patterns]]
            source = "tools/{name}"
            id = "tool/{name}"
            directory = "generated/{name}"
            "#,
        )
        .expect("config");
        let repo = Repository::from_root(temp.path()).expect("repository");

        let error = repo
            .validate_pattern_section_directory(Path::new("generated/core"))
            .expect_err("ambiguous generated directory");

        assert!(error.to_string().contains("ambiguous"), "{error}");
    }

    #[test]
    fn permits_nested_but_distinct_generated_section_directories() {
        let temp = TempDir::new().expect("tempdir");

        validate_pattern_section_directories(
            temp.path(),
            Path::new("changes.d"),
            Path::new("next"),
            &[PathBuf::from("packages"), PathBuf::from("packages/core")],
            &temp.path().join(MUTATION_LOCK_FILE),
        )
        .expect("nested generated sections");
    }

    #[test]
    fn rejects_configured_paths_overlapping_the_configuration_file() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\npath = \"sacho.toml\"\n",
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("config overlap");
        let message = error.to_string();

        assert!(message.contains("changelog.path"), "{message}");
        assert!(message.contains("sacho.toml"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn permits_internal_symlink_ancestors_resolved_before_parent_components() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("actual/nested")).expect("actual directory");
        fs::create_dir(temp.path().join("actual/changes.d")).expect("fragment directory");
        symlink(
            temp.path().join("actual/nested"),
            temp.path().join("linked"),
        )
        .expect("internal symlink");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"linked/../changes.d\"\n",
        )
        .expect("config");

        let repo = Repository::from_root(temp.path()).expect("internal symlink remains safe");

        assert_eq!(
            repo.config().fragments.directory,
            Path::new("actual/changes.d")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_ancestors_resolving_outside_the_repository() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        fs::create_dir(outside.path().join("changes.d")).expect("outside directory");
        symlink(outside.path(), temp.path().join("linked")).expect("outside symlink");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"linked/changes.d\"\n",
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("outside symlink");
        let message = error.to_string();

        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("linked/changes.d"), "{message}");
        assert!(message.contains("repository root"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_broken_symlink_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        symlink("missing-target", temp.path().join("broken")).expect("broken symlink");
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"broken/changes.d\"\n",
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("broken symlink");
        let message = error.to_string();

        assert!(message.contains("fragments.directory"), "{message}");
        assert!(message.contains("broken/changes.d"), "{message}");
        assert!(message.contains("symbolic link"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_section_directories_that_are_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("tempdir");
        fs::create_dir_all(temp.path().join("changes.d/actual")).expect("actual section");
        symlink("actual", temp.path().join("changes.d/alias")).expect("section alias");
        fs::write(
            temp.path().join("sacho.toml"),
            r#"
            [[sections]]
            id = "actual"
            directory = "actual"

            [[sections]]
            id = "alias"
            directory = "alias"
            "#,
        )
        .expect("config");

        let error = Repository::from_root(temp.path()).expect_err("section aliases");
        let message = error.to_string();

        assert!(message.contains("sections[0].directory"), "{message}");
        assert!(message.contains("sections[1].directory"), "{message}");
    }

    #[test]
    fn writes_files_atomically_under_repository_root() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        let repo = Repository::from_root(temp.path()).expect("repository");

        repo.atomic_write("nested/file.txt", b"hello\n")
            .expect("write");

        assert_eq!(
            fs::read_to_string(temp.path().join("nested/file.txt")).expect("read"),
            "hello\n"
        );
    }

    #[test]
    fn atomic_write_replaces_existing_files() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::create_dir_all(temp.path().join("nested")).expect("nested dir");
        fs::write(temp.path().join("nested/file.txt"), "old\n").expect("old file");
        let repo = Repository::from_root(temp.path()).expect("repository");

        repo.atomic_write("nested/file.txt", b"new\n")
            .expect("replace");

        assert_eq!(
            fs::read_to_string(temp.path().join("nested/file.txt")).expect("read"),
            "new\n"
        );
    }

    #[test]
    fn dropping_prepared_write_removes_temporary_file_without_replacing_destination() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::write(temp.path().join("file.txt"), "old\n").expect("old file");
        let repo = Repository::from_root(temp.path()).expect("repository");

        let prepared = repo
            .prepare_atomic_write("file.txt", b"new\n")
            .expect("prepare");
        let temporary = prepared.temporary.clone();
        assert!(temporary.exists());
        drop(prepared);

        assert!(!temporary.exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("file.txt")).expect("destination"),
            "old\n"
        );
    }

    #[test]
    fn dropping_prepared_write_removes_directories_created_for_the_destination() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        let repo = Repository::from_root(temp.path()).expect("repository");

        let prepared = repo
            .prepare_atomic_write("new/nested/file.txt", b"new\n")
            .expect("prepare");
        assert!(temp.path().join("new/nested").is_dir());

        drop(prepared);

        assert!(!temp.path().join("new").exists());
    }

    #[test]
    fn directory_creation_race_matches_only_an_existing_directory() {
        assert!(directory_was_created_concurrently(&std::io::Error::from(
            std::io::ErrorKind::AlreadyExists
        )));
        assert!(!directory_was_created_concurrently(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn atomic_write_does_not_touch_a_legacy_temporary_path() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::write(temp.path().join("file.txt.tmp"), "participant\n").expect("participant");
        let repo = Repository::from_root(temp.path()).expect("repository");

        repo.atomic_write("file.txt", b"new\n").expect("write");

        assert_eq!(
            fs::read_to_string(temp.path().join("file.txt.tmp")).expect("participant"),
            "participant\n"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("file.txt")).expect("destination"),
            "new\n"
        );
    }

    #[test]
    fn prepared_write_does_not_replace_an_existing_destination() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::write(temp.path().join("file.txt"), "old\n").expect("destination");
        let repo = Repository::from_root(temp.path()).expect("repository");
        let prepared = repo
            .prepare_atomic_write("file.txt", b"new\n")
            .expect("prepare");

        let error = prepared
            .commit_if_absent()
            .expect_err("existing destination");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(temp.path().join("file.txt")).expect("destination"),
            "old\n"
        );
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_vendor = "apple",
        target_os = "redox",
        windows,
    ))]
    #[test]
    fn no_replace_move_supports_directories() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::write(source.join("entry"), "preserved\n").expect("entry");

        move_path_if_absent(&source, &destination).expect("move directory");

        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(destination.join("entry")).expect("entry"),
            "preserved\n"
        );
    }

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_vendor = "apple",
        target_os = "redox",
        windows,
    ))]
    #[test]
    fn no_replace_move_does_not_replace_a_directory() {
        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source directory");
        fs::write(source.join("source-entry"), "source\n").expect("source entry");
        fs::create_dir(&destination).expect("destination directory");
        fs::write(destination.join("destination-entry"), "destination\n")
            .expect("destination entry");

        let error = move_path_if_absent(&source, &destination)
            .expect_err("existing destination must not be replaced");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(source.join("source-entry").is_file());
        assert!(destination.join("destination-entry").is_file());
    }

    #[test]
    fn temporary_path_forbiddenness_uses_the_complete_canonical_path() {
        let parent = Path::new("/repository/meta");
        let forbidden = vec![parent.join(".sacho-1-2.tmp")];

        assert!(temporary_path_is_forbidden(
            parent,
            ".sacho-1-2.tmp",
            &forbidden
        ));
        assert!(!temporary_path_is_forbidden(
            parent,
            ".sacho-1-3.tmp",
            &forbidden
        ));
        assert!(temporary_path_is_forbidden(
            parent,
            ".sacho-1-4.tmp",
            &[parent.join(".sacho-1-4.tmp/next")]
        ));
        assert!(!temporary_path_is_forbidden(
            Path::new("/repository/other"),
            ".sacho-1-2.tmp",
            &forbidden
        ));
    }

    #[test]
    fn temporary_path_collisions_are_retried_only_for_existing_paths() {
        assert!(is_temporary_path_collision(&std::io::Error::from(
            std::io::ErrorKind::AlreadyExists
        )));
        assert!(!is_temporary_path_collision(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    proptest! {
        #[test]
        fn discovers_config_from_any_nested_directory(segments in path_segments()) {
            let temp = TempDir::new().expect("tempdir");
            let config_path = temp.path().join("sacho.toml");
            fs::write(&config_path, "").expect("config");
            let nested = segments.iter().fold(temp.path().to_path_buf(), |path, segment| {
                path.join(segment)
            });
            fs::create_dir_all(&nested).expect("nested dir");

            let discovered = Repository::discover_config(&nested);

            prop_assert_eq!(discovered, Some(config_path));
        }

        #[test]
        fn discovers_config_from_files_inside_nested_directories(segments in path_segments()) {
            let temp = TempDir::new().expect("tempdir");
            let config_path = temp.path().join("sacho.toml");
            fs::write(&config_path, "").expect("config");
            let nested = segments.iter().fold(temp.path().to_path_buf(), |path, segment| {
                path.join(segment)
            });
            fs::create_dir_all(&nested).expect("nested dir");
            let file = nested.join("source.rs");
            fs::write(&file, "fn main() {}\n").expect("file");

            let discovered = Repository::discover_config(&file);

            prop_assert_eq!(discovered, Some(config_path));
        }
    }
}
