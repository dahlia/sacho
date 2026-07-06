use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{
    ConfigSnafu, CreateDirectorySnafu, ReadFileSnafu, RenameFileSnafu, Result, WriteFileSnafu,
};
use snafu::ResultExt;

/// Repository handle containing the root path and parsed configuration.
#[derive(Debug, Clone)]
pub struct Repository {
    root: PathBuf,
    config: Config,
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
        let path = self.resolve(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(CreateDirectorySnafu {
                path: parent.to_path_buf(),
            })?;
        }

        let tmp_path = path.with_extension(format!(
            "{}tmp",
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!("{extension}."))
                .unwrap_or_default()
        ));
        fs::write(&tmp_path, contents).context(WriteFileSnafu {
            path: tmp_path.clone(),
        })?;
        replace_file(&tmp_path, &path).context(RenameFileSnafu {
            from: tmp_path,
            to: path,
        })?;
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

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
    // the call.  MOVEFILE_REPLACE_EXISTING gives Windows the overwrite behavior
    // that Unix rename has for existing destination files.
    let replaced = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

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
}
