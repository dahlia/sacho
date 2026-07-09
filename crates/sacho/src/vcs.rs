use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Identifier for a commit returned by a VCS integration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitId(String);

impl CommitId {
    /// Creates a commit identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Contract required by the missing-fragment check.
pub trait Vcs {
    /// Returns commits from `base` to the working revision, oldest first.
    fn commits(&self, base: &str) -> Result<Vec<CommitId>>;

    /// Returns repository-relative paths changed by `commit`.
    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>>;

    /// Returns the raw commit message for `commit`.
    fn message(&self, commit: &CommitId) -> Result<String>;
}

/// Git implementation of the VCS contract.
#[derive(Debug, Clone)]
pub struct GitVcs {
    root: PathBuf,
}

impl GitVcs {
    /// Creates a Git VCS adapter rooted at a repository path.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl Vcs for GitVcs {
    fn commits(&self, base: &str) -> Result<Vec<CommitId>> {
        let range = format!("{base}..HEAD");
        let output = self.git(["rev-list", "--reverse", &range])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(CommitId::new)
            .collect())
    }

    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        let output = if self.parent_count(commit)? > 1 {
            self.git([
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "-z",
                "--cc",
                "-r",
                commit.as_str(),
            ])?
        } else {
            self.git([
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "--find-renames",
                "--find-copies",
                "--find-copies-harder",
                "-z",
                "-r",
                "--root",
                commit.as_str(),
            ])?
        };
        Ok(parse_name_status_paths(&output))
    }

    fn message(&self, commit: &CommitId) -> Result<String> {
        let output = self.git(["log", "-1", "--format=%B", commit.as_str()])?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }
}

impl GitVcs {
    fn parent_count(&self, commit: &CommitId) -> Result<usize> {
        let output = self.git(["rev-list", "--parents", "-n", "1", commit.as_str()])?;
        let line = String::from_utf8_lossy(&output);
        Ok(line.split_whitespace().count().saturating_sub(1))
    }

    fn git<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        let command = command_line("git", &args);
        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.root)
            .output()
            .map_err(|source| Error::VcsCommandIo {
                command: command.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(Error::VcsCommandFailed {
                command,
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }
}

/// Kind of change reported for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Path was added.
    Added,

    /// Path was copied.
    Copied,

    /// Path was deleted.
    Deleted,

    /// Path was modified.
    Modified,

    /// Path was renamed.
    Renamed,

    /// Path had another Git status that still represents a surviving path.
    Other,
}

impl ChangeKind {
    /// Returns true when the reported path is present after the commit.
    pub fn path_survives(self) -> bool {
        self != Self::Deleted
    }
}

/// One path changed by a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    /// Repository-relative path.
    pub path: PathBuf,

    /// Previous path for VCS records that carry one, such as renames.
    pub old_path: Option<PathBuf>,

    /// Git similarity percentage for copy or rename records when available.
    pub similarity: Option<u8>,

    /// Kind of path change.
    pub kind: ChangeKind,
}

impl ChangedPath {
    /// Creates a changed path.
    pub fn new(path: impl Into<PathBuf>, kind: ChangeKind) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            similarity: None,
            kind,
        }
    }

    /// Creates a changed path with an old path.
    pub fn with_old_path(
        path: impl Into<PathBuf>,
        kind: ChangeKind,
        old_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            path: path.into(),
            old_path: Some(old_path.into()),
            similarity: None,
            kind,
        }
    }

    /// Creates a changed path with an old path and a similarity percentage.
    pub fn with_old_path_and_similarity(
        path: impl Into<PathBuf>,
        kind: ChangeKind,
        old_path: impl Into<PathBuf>,
        similarity: u8,
    ) -> Self {
        Self {
            path: path.into(),
            old_path: Some(old_path.into()),
            similarity: Some(similarity),
            kind,
        }
    }
}

/// Parses Git's NUL-delimited `--name-status -z` output.
pub fn parse_name_status_paths(output: &[u8]) -> Vec<ChangedPath> {
    let mut fields = output
        .split(|byte| *byte == b'\0')
        .filter(|field| !field.is_empty());
    let mut paths = Vec::new();

    while let Some(status) = fields.next() {
        let status = String::from_utf8_lossy(status);
        let Some(path) = fields.next() else {
            break;
        };
        let kind = change_kind(&status);
        if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
            let Some(new_path) = fields.next() else {
                break;
            };
            paths.push(ChangedPath::with_old_path_and_similarity(
                path_from_bytes(new_path),
                kind,
                path_from_bytes(path),
                change_similarity(&status),
            ));
        } else {
            paths.push(ChangedPath::new(path_from_bytes(path), kind));
        }
    }

    paths
}

fn change_kind(status: &str) -> ChangeKind {
    match status.as_bytes().first().copied() {
        Some(b'A') => ChangeKind::Added,
        Some(b'C') => ChangeKind::Copied,
        Some(b'D') => ChangeKind::Deleted,
        Some(b'M') => ChangeKind::Modified,
        Some(b'R') => ChangeKind::Renamed,
        _ => ChangeKind::Other,
    }
}

fn change_similarity(status: &str) -> u8 {
    status
        .get(1..)
        .and_then(|value| value.parse().ok())
        .unwrap_or(100)
}

fn path_from_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

fn command_line<S>(program: &str, args: &[S]) -> String
where
    S: AsRef<OsStr>,
{
    let mut output = String::from(program);
    for arg in args {
        output.push(' ');
        output.push_str(&arg.as_ref().to_string_lossy());
    }
    output
}

/// Commit information returned by a VCS integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Commit identifier.
    pub id: String,

    /// Paths changed by the commit.
    pub changed_paths: Vec<PathBuf>,

    /// Raw commit message.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use std::process::Command as ProcessCommand;

    use super::*;

    #[test]
    fn parses_name_status_paths_with_spaces_and_newlines() {
        let paths =
            parse_name_status_paths(b"M\0src/main.rs\0A\0docs/file name.md\0D\0weird\nname.rs\0");

        assert_eq!(
            paths,
            vec![
                ChangedPath::new("src/main.rs", ChangeKind::Modified),
                ChangedPath::new("docs/file name.md", ChangeKind::Added),
                ChangedPath::new("weird\nname.rs", ChangeKind::Deleted),
            ]
        );
    }

    #[test]
    fn parses_rename_status_as_surviving_new_path() {
        let paths = parse_name_status_paths(b"R100\0changes.d/old.md\0changes.d/new.md\0");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/new.md",
                ChangeKind::Renamed,
                "changes.d/old.md",
                100
            )]
        );
    }

    #[test]
    fn parses_copy_status_as_surviving_new_path_with_similarity() {
        let paths = parse_name_status_paths(b"C85\0changes.d/old.md\0changes.d/new.md\0");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/new.md",
                ChangeKind::Copied,
                "changes.d/old.md",
                85
            )]
        );
    }

    #[test]
    fn deleted_paths_do_not_survive() {
        assert!(!ChangeKind::Deleted.path_survives());
        assert!(ChangeKind::Added.path_survives());
        assert!(ChangeKind::Modified.path_survives());
    }

    #[test]
    fn git_vcs_reports_paths_for_root_commit() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn initial() {}\n").expect("source");
        git(temp.path(), ["init"]);
        git(temp.path(), ["config", "user.email", "test@example.com"]);
        git(temp.path(), ["config", "user.name", "Test User"]);
        git(temp.path(), ["add", "src/lib.rs"]);
        git(temp.path(), ["commit", "-m", "Initial source"]);
        let root_commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(root_commit.trim()))
            .expect("changed paths");

        assert_eq!(
            paths,
            vec![ChangedPath::new("src/lib.rs", ChangeKind::Added)]
        );
    }

    #[test]
    fn git_vcs_reports_fragment_renames() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        std::fs::write(
            temp.path().join("changes.d/old.md"),
            " -  Added old behavior.\n",
        )
        .expect("old fragment");
        git(temp.path(), ["add", "changes.d/old.md"]);
        git(temp.path(), ["commit", "-m", "Add old fragment"]);
        git(temp.path(), ["mv", "changes.d/old.md", "changes.d/new.md"]);
        git(temp.path(), ["commit", "-m", "Rename fragment"]);
        let commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("changed paths");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/new.md",
                ChangeKind::Renamed,
                "changes.d/old.md",
                100
            )]
        );
    }

    #[test]
    fn git_vcs_reports_fragment_copies_from_unchanged_files() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(
            temp.path().join("changes.d/old.md"),
            " -  Added old behavior.\n",
        )
        .expect("old fragment");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn old() {}\n").expect("source");
        git(temp.path(), ["add", "changes.d/old.md", "src/lib.rs"]);
        git(temp.path(), ["commit", "-m", "Initial files"]);
        std::fs::copy(
            temp.path().join("changes.d/old.md"),
            temp.path().join("changes.d/new.md"),
        )
        .expect("copy fragment");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn new() {}\n").expect("source");
        git(temp.path(), ["add", "changes.d/new.md", "src/lib.rs"]);
        git(
            temp.path(),
            ["commit", "-m", "Copy fragment and change source"],
        );
        let commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("changed paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes.d/new.md",
            ChangeKind::Copied,
            "changes.d/old.md",
            100
        )));
        assert!(paths.contains(&ChangedPath::new("src/lib.rs", ChangeKind::Modified)));
    }

    #[test]
    fn git_vcs_reports_paths_changed_by_merge_resolution() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 0 }\n",
        )
        .expect("source");
        git(temp.path(), ["add", "src/lib.rs"]);
        git(temp.path(), ["commit", "-m", "Initial source"]);
        let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);
        git(temp.path(), ["checkout", "-b", "feature"]);
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("feature source");
        git(temp.path(), ["commit", "-am", "Change feature"]);
        git(temp.path(), ["checkout", initial_branch.trim()]);
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .expect("master source");
        git(temp.path(), ["commit", "-am", "Change master"]);
        git_expect_failure(temp.path(), ["merge", "feature"]);
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 3 }\n",
        )
        .expect("resolved source");
        git(temp.path(), ["add", "src/lib.rs"]);
        git(temp.path(), ["commit", "-m", "Merge feature"]);
        let merge_commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(merge_commit.trim()))
            .expect("changed paths");

        assert!(paths.contains(&ChangedPath::new("src/lib.rs", ChangeKind::Modified)));
    }

    #[test]
    fn git_vcs_ignores_paths_merged_unchanged_from_upstream_parent() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(temp.path().join("src/initial.rs"), "pub fn initial() {}\n")
            .expect("initial source");
        git(temp.path(), ["add", "src/initial.rs"]);
        git(temp.path(), ["commit", "-m", "Initial source"]);
        let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);
        git(temp.path(), ["checkout", "-b", "feature"]);
        std::fs::write(temp.path().join("src/branch.rs"), "pub fn branch() {}\n")
            .expect("branch source");
        git(temp.path(), ["add", "src/branch.rs"]);
        git(temp.path(), ["commit", "-m", "Change feature"]);
        git(temp.path(), ["checkout", initial_branch.trim()]);
        std::fs::write(
            temp.path().join("src/upstream.rs"),
            "pub fn upstream() {}\n",
        )
        .expect("upstream source");
        git(temp.path(), ["add", "src/upstream.rs"]);
        git(temp.path(), ["commit", "-m", "Change upstream"]);
        git(temp.path(), ["checkout", "feature"]);
        git(temp.path(), ["merge", initial_branch.trim(), "--no-edit"]);
        let merge_commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(merge_commit.trim()))
            .expect("changed paths");

        assert!(paths.is_empty());
    }

    fn init_repo(dir: &std::path::Path) {
        git(dir, ["init"]);
        git(dir, ["config", "user.email", "test@example.com"]);
        git(dir, ["config", "user.name", "Test User"]);
    }

    fn git<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
        let output = git_command(dir, args);
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_expect_failure<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
        let output = git_command(dir, args);
        assert!(
            !output.status.success(),
            "git unexpectedly succeeded: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn git_output<const N: usize>(dir: &std::path::Path, args: [&str; N]) -> String {
        let output = git_command(dir, args);
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn git_command<const N: usize>(dir: &std::path::Path, args: [&str; N]) -> std::process::Output {
        ProcessCommand::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git command")
    }
}
