use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;
use crate::error::{ConfigSnafu, ReadFileSnafu, RenameFileSnafu, Result};
use snafu::ResultExt;

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Repository handle containing the root path and parsed configuration.
#[derive(Debug, Clone)]
pub struct Repository {
    root: PathBuf,
    config: Config,
}

/// A file whose contents have been written beside its final destination.
pub(crate) struct PreparedAtomicWrite {
    temporary: PathBuf,
    destination: PathBuf,
    created_directories: Vec<PathBuf>,
}

impl PreparedAtomicWrite {
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
        let config_path = root.join(Self::CONFIG_FILE);
        let contents = fs::read_to_string(&config_path).context(ReadFileSnafu {
            path: config_path.clone(),
        })?;
        let config = Config::parse(&contents).context(ConfigSnafu { path: config_path })?;

        Ok(Self { root, config })
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

    /// Returns the parsed repository configuration.
    pub fn config(&self) -> &Config {
        &self.config
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
