use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{VcsCommand, VcsConfig, VcsPreset, VcsQuery};
use crate::error::{Error, Result};
use crate::section_pattern::SectionPattern;

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
    queries: QueryVcs<ProcessRunner>,
}

impl GitVcs {
    /// Creates a Git VCS adapter rooted at a repository path.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self::configured(root, &VcsConfig::default())
    }

    /// Creates a Git adapter with command overrides from `config`.
    pub fn configured(root: impl AsRef<Path>, config: &VcsConfig) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            queries: QueryVcs::new(&root, VcsPreset::Git, config, ProcessRunner),
            root,
        }
    }

    /// Returns repository-relative paths changed in the staged index.
    pub fn staged_paths(&self) -> Result<Vec<ChangedPath>> {
        let head = self.head()?;
        let merge_heads = self.merge_heads()?;
        if merge_heads.is_empty() {
            self.staged_paths_against(head.as_deref(), true)
        } else {
            let mut parents = Vec::with_capacity(merge_heads.len() + 1);
            parents.extend(head);
            parents.extend(merge_heads);
            self.staged_paths_for_merge(&parents)
        }
    }

    pub(crate) fn head(&self) -> Result<Option<String>> {
        let command = String::from("git rev-parse --verify -q HEAD");
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .current_dir(&self.root)
            .output()
            .map_err(|source| Error::VcsCommandIo {
                command: command.clone(),
                source,
            })?;
        if output.status.success() {
            Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            ))
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(Error::VcsCommandFailed {
                command,
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    pub(crate) fn index_tree(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git(["write-tree"])?)
            .trim()
            .to_owned())
    }

    pub(crate) fn commit_tree(&self, commit: &str) -> Result<String> {
        let revision = format!("{commit}^{{tree}}");
        Ok(
            String::from_utf8_lossy(&self.git(["rev-parse", "--verify", &revision])?)
                .trim()
                .to_owned(),
        )
    }

    fn staged_paths_for_merge(&self, parents: &[String]) -> Result<Vec<ChangedPath>> {
        let Some((first, rest)) = parents.split_first() else {
            return Ok(Vec::new());
        };
        let mut paths = self.staged_paths_against(Some(first), false)?;
        for parent in rest {
            let changed = self
                .staged_paths_against(Some(parent), false)?
                .into_iter()
                .map(|path| path.path)
                .collect::<std::collections::HashSet<_>>();
            paths.retain(|path| changed.contains(&path.path));
        }
        Ok(paths)
    }

    fn staged_paths_against(
        &self,
        base: Option<&str>,
        detect_copies_and_renames: bool,
    ) -> Result<Vec<ChangedPath>> {
        let empty_tree;
        let base = match base {
            Some(base) => base,
            None => {
                empty_tree = self.empty_tree()?;
                empty_tree.trim()
            }
        };
        let mut args = vec!["diff", "--cached", "--name-status"];
        if detect_copies_and_renames {
            args.extend(["--find-renames", "--find-copies", "--find-copies-harder"]);
        }
        args.extend(["-z", base, "--"]);
        let output = self.git(args)?;
        parse_name_status_paths(&output).map_err(|reason| Error::VcsQueryMalformedOutput {
            query: VcsQuery::ChangedPaths,
            command: vec![String::from("git"), String::from("diff --cached")],
            reason,
            stdout: output,
        })
    }

    fn merge_heads(&self) -> Result<Vec<String>> {
        let output = self.git(["rev-parse", "--git-path", "MERGE_HEAD"])?;
        let path = PathBuf::from(String::from_utf8_lossy(&output).trim());
        let path = if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        };
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(Error::ReadFile { path, source }),
        }
    }

    fn empty_tree(&self) -> Result<String> {
        let command = String::from("git mktree");
        let output = Command::new("git")
            .arg("mktree")
            .current_dir(&self.root)
            .stdin(Stdio::null())
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
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Vcs for GitVcs {
    fn commits(&self, base: &str) -> Result<Vec<CommitId>> {
        self.queries.commits(base)
    }

    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        self.queries.changed_paths(commit)
    }

    fn message(&self, commit: &CommitId) -> Result<String> {
        self.queries.message(commit)
    }
}

impl GitVcs {
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

/// Jujutsu implementation of the VCS contract.
#[derive(Debug, Clone)]
pub struct JjVcs {
    queries: QueryVcs<ProcessRunner>,
}

impl JjVcs {
    /// Creates a Jujutsu adapter with command overrides from `config` and the
    /// default `changes.d` fragment directory.
    pub fn new(root: impl AsRef<Path>, config: &VcsConfig) -> Self {
        Self::with_fragment_directory(root, config, "changes.d")
    }

    /// Creates a Jujutsu adapter that detects unchanged fragment copies and
    /// renames within `fragment_directory`.
    pub fn with_fragment_directory(
        root: impl AsRef<Path>,
        config: &VcsConfig,
        fragment_directory: impl AsRef<Path>,
    ) -> Self {
        Self {
            queries: QueryVcs::new(root.as_ref(), VcsPreset::Jj, config, ProcessRunner)
                .with_fragment_directory(fragment_directory.as_ref()),
        }
    }
}

impl Vcs for JjVcs {
    fn commits(&self, base: &str) -> Result<Vec<CommitId>> {
        self.queries.commits(base)
    }

    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        self.queries.changed_paths(commit)
    }

    fn message(&self, commit: &CommitId) -> Result<String> {
        self.queries.message(commit)
    }
}

/// Mercurial implementation of the VCS contract.
#[derive(Debug, Clone)]
pub struct HgVcs {
    queries: QueryVcs<ProcessRunner>,
}

impl HgVcs {
    /// Creates a Mercurial adapter with command overrides from `config` and
    /// the default `changes.d` fragment directory.
    pub fn new(root: impl AsRef<Path>, config: &VcsConfig) -> Self {
        Self::with_fragment_directory(root, config, "changes.d")
    }

    /// Creates a Mercurial adapter that compares copy contents only for paths
    /// that can be fragments within `fragment_directory`.
    pub fn with_fragment_directory(
        root: impl AsRef<Path>,
        config: &VcsConfig,
        fragment_directory: impl AsRef<Path>,
    ) -> Self {
        Self::with_fragment_layout(
            root,
            config,
            fragment_directory,
            std::iter::empty::<PathBuf>(),
        )
    }

    /// Creates a Mercurial adapter that recognizes fragments directly under
    /// `fragment_directory`, under immediate unknown sections, and under the
    /// configured `section_directories`.
    pub fn with_fragment_layout<I, P>(
        root: impl AsRef<Path>,
        config: &VcsConfig,
        fragment_directory: impl AsRef<Path>,
        section_directories: I,
    ) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let section_directories = section_directories
            .into_iter()
            .map(|directory| directory.as_ref().to_path_buf())
            .collect();
        Self {
            queries: QueryVcs::new(root.as_ref(), VcsPreset::Hg, config, ProcessRunner)
                .with_fragment_layout(fragment_directory.as_ref(), section_directories, Vec::new()),
        }
    }

    /// Creates a Mercurial adapter that also recognizes fragment directories
    /// generated by `section_patterns`.
    pub fn with_patterned_fragment_layout<I, P>(
        root: impl AsRef<Path>,
        config: &VcsConfig,
        fragment_directory: impl AsRef<Path>,
        section_directories: I,
        section_patterns: &[SectionPattern],
    ) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let section_directories = section_directories
            .into_iter()
            .map(|directory| directory.as_ref().to_path_buf())
            .collect();
        Self {
            queries: QueryVcs::new(root.as_ref(), VcsPreset::Hg, config, ProcessRunner)
                .with_fragment_layout(
                    fragment_directory.as_ref(),
                    section_directories,
                    section_patterns.to_vec(),
                ),
        }
    }
}

impl Vcs for HgVcs {
    fn commits(&self, base: &str) -> Result<Vec<CommitId>> {
        self.queries.commits(base)
    }

    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        self.queries.changed_paths(commit)
    }

    fn message(&self, commit: &CommitId) -> Result<String> {
        self.queries.message(commit)
    }
}

#[derive(Debug, Clone)]
struct ResolvedCommands {
    commits: VcsCommand,
    changed_paths: VcsCommand,
    message: VcsCommand,
}

impl ResolvedCommands {
    fn new(preset: VcsPreset, config: &VcsConfig) -> Self {
        let defaults = default_commands(preset);
        Self {
            commits: config.commands.commits.clone().unwrap_or(defaults.commits),
            changed_paths: config
                .commands
                .changed_paths
                .clone()
                .unwrap_or(defaults.changed_paths),
            message: config.commands.message.clone().unwrap_or(defaults.message),
        }
    }
}

fn default_commands(preset: VcsPreset) -> ResolvedCommands {
    match preset {
        VcsPreset::Git => ResolvedCommands {
            commits: VcsCommand::new([
                "git",
                "rev-list",
                "--reverse",
                "--end-of-options",
                "${base}..HEAD",
            ]),
            changed_paths: VcsCommand::new([
                "git",
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "--find-renames",
                "--find-copies",
                "--find-copies-harder",
                "-z",
                "-r",
                "--root",
                "--diff-merges=combined",
                "${commit}",
                "--",
            ]),
            message: VcsCommand::new([
                "git",
                "log",
                "-1",
                "--format=%B",
                "--encoding=UTF-8",
                "--end-of-options",
                "${commit}",
            ]),
        },
        VcsPreset::Jj => ResolvedCommands {
            commits: VcsCommand::new([
                "jj",
                "--no-pager",
                "--color=never",
                "log",
                "--no-graph",
                "--reversed",
                "-r",
                "${base}..@",
                "-T",
                "commit_id ++ \"\\n\"",
            ]),
            changed_paths: VcsCommand::new([
                "jj",
                "--no-pager",
                "--color=never",
                "diff",
                "-r",
                "${commit}",
                "-T",
                "if(status_char == \"C\" || status_char == \"R\", status_char ++ \"\\0\" ++ source.path() ++ \"\\0\" ++ target.path() ++ \"\\0\", status_char ++ \"\\0\" ++ path ++ \"\\0\")",
            ]),
            message: VcsCommand::new([
                "jj",
                "--no-pager",
                "--color=never",
                "log",
                "--no-graph",
                "-r",
                "${commit}",
                "-T",
                "description",
            ]),
        },
        VcsPreset::Hg => ResolvedCommands {
            commits: VcsCommand::new([
                "hg",
                "log",
                "-r",
                "sort(only(., ${base}), rev)",
                "-T",
                "{node}\\n",
            ]),
            changed_paths: VcsCommand::new([
                "hg",
                "status",
                "--rev",
                "${parent}",
                "--rev",
                "${commit}",
                "--copies",
                "-T",
                "{if(source, \"C\\0{source}\\0{path}\\0\", \"{ifeq(status, 'R', 'D', status)}\\0{path}\\0\")}",
            ]),
            message: VcsCommand::new(["hg", "log", "-r", "${commit}", "-T", "{desc}"]),
        },
        VcsPreset::None => unreachable!("disabled VCS has no query commands"),
    }
}

trait Runner: Clone + std::fmt::Debug {
    fn run(&self, root: &Path, argv: &[OsString]) -> std::io::Result<std::process::Output>;
}

#[derive(Debug, Clone, Copy)]
struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, root: &Path, argv: &[OsString]) -> std::io::Result<std::process::Output> {
        let (program, arguments) = argv.split_first().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "VCS command must contain a program",
            )
        })?;
        Command::new(program)
            .args(arguments)
            .current_dir(root)
            .output()
    }
}

#[derive(Debug, Clone)]
struct QueryVcs<R> {
    root: PathBuf,
    preset: VcsPreset,
    commands: ResolvedCommands,
    commits_overridden: bool,
    changed_paths_overridden: bool,
    runner: R,
    fragment_directory: Option<PathBuf>,
    fragment_section_directories: Vec<PathBuf>,
    fragment_section_patterns: Vec<SectionPattern>,
}

impl<R: Runner> QueryVcs<R> {
    fn new(root: &Path, preset: VcsPreset, config: &VcsConfig, runner: R) -> Self {
        Self {
            root: root.to_path_buf(),
            preset,
            commands: ResolvedCommands::new(preset, config),
            commits_overridden: config.commands.commits.is_some(),
            changed_paths_overridden: config.commands.changed_paths.is_some(),
            runner,
            fragment_directory: None,
            fragment_section_directories: Vec::new(),
            fragment_section_patterns: Vec::new(),
        }
    }

    fn with_fragment_directory(mut self, directory: &Path) -> Self {
        self.fragment_directory = Some(directory.to_path_buf());
        self
    }

    fn with_fragment_layout(
        mut self,
        directory: &Path,
        sections: Vec<PathBuf>,
        patterns: Vec<SectionPattern>,
    ) -> Self {
        self.fragment_directory = Some(directory.to_path_buf());
        self.fragment_section_directories = sections;
        self.fragment_section_patterns = patterns;
        self
    }

    fn commits(&self, base: &str) -> Result<Vec<CommitId>> {
        let literal_base;
        let base = if self.preset == VcsPreset::Hg && !self.commits_overridden {
            literal_base = hg_revset_literal(base);
            &literal_base
        } else {
            base
        };
        let (output, command) = self.execute(
            VcsQuery::Commits,
            &self.commands.commits,
            &[Placeholder::Base(base)],
        )?;
        let text = strict_utf8(VcsQuery::Commits, &command, output)?;
        parse_commits(&text).map_err(|reason| Error::VcsQueryMalformedOutput {
            query: VcsQuery::Commits,
            command,
            reason,
            stdout: text.into_bytes(),
        })
    }

    fn changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        if self.preset == VcsPreset::Hg && !self.changed_paths_overridden {
            return self.hg_changed_paths(commit);
        }
        if self.preset == VcsPreset::Git {
            return self.git_changed_paths(commit);
        }
        let (output, command) = self.execute(
            VcsQuery::ChangedPaths,
            &self.commands.changed_paths,
            &[Placeholder::Commit(commit.as_str())],
        )?;
        let paths = parse_name_status_query(output, command)?;
        if self.preset == VcsPreset::Jj && !self.changed_paths_overridden {
            self.annotate_jj_fragment_copies(paths, commit)
        } else {
            Ok(paths)
        }
    }

    fn annotate_jj_fragment_copies(
        &self,
        mut paths: Vec<ChangedPath>,
        commit: &CommitId,
    ) -> Result<Vec<ChangedPath>> {
        let Some(fragment_directory) = &self.fragment_directory else {
            return Ok(paths);
        };
        let added = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| {
                path.kind == ChangeKind::Added && path.path.starts_with(fragment_directory)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let reported_copies = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| {
                matches!(path.kind, ChangeKind::Copied | ChangeKind::Renamed)
                    && path.similarity.is_none()
                    && path.path.starts_with(fragment_directory)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if added.is_empty() && reported_copies.is_empty() {
            return Ok(paths);
        }

        let parents = self.jj_parents(commit)?;
        let mut parent_contents = Vec::new();
        if !reported_copies.is_empty() {
            let old_filesets = reported_copies
                .iter()
                .map(|index| {
                    jj_fileset(
                        paths[*index]
                            .old_path
                            .as_deref()
                            .expect("copy and rename records have an old path"),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            for parent in &parents {
                parent_contents.extend(self.jj_files_contents(parent, &old_filesets)?);
            }
        }

        let current_paths = reported_copies
            .iter()
            .map(|index| paths[*index].path.clone())
            .collect::<Vec<_>>();
        let current_filesets = current_paths
            .iter()
            .map(|path| jj_fileset(path))
            .collect::<Result<Vec<_>>>()?;
        let current_contents = if current_filesets.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.jj_files_contents(commit.as_str(), &current_filesets)?
                .into_iter()
                .collect()
        };

        let current_file_ids = if added.is_empty() {
            std::collections::HashMap::new()
        } else {
            let filesets = added
                .iter()
                .map(|index| jj_fileset(&paths[*index].path))
                .collect::<Result<Vec<_>>>()?;
            self.jj_file_ids(commit.as_str(), &filesets)?
                .into_iter()
                .collect()
        };
        let mut parent_file_ids = Vec::new();
        if !added.is_empty() {
            for parent in &parents {
                parent_file_ids.extend(self.jj_file_ids(parent, &[String::from("all()")])?);
            }
        }

        for index in reported_copies {
            let old_path = paths[index]
                .old_path
                .as_deref()
                .expect("copy and rename records have an old path");
            let new = current_contents.get(&paths[index].path).ok_or_else(|| {
                Error::VcsQueryMalformedOutput {
                    query: VcsQuery::ChangedPaths,
                    command: Vec::new(),
                    reason: format!(
                        "Jujutsu did not return contents for changed fragment {:?}",
                        paths[index].path
                    ),
                    stdout: Vec::new(),
                }
            })?;
            paths[index].similarity = Some(
                if parent_contents
                    .iter()
                    .any(|(path, contents)| path == old_path && contents == new)
                {
                    100
                } else {
                    0
                },
            );
        }

        for index in added {
            let Some(file_id) = current_file_ids.get(&paths[index].path) else {
                // Symlinks and other non-regular entries have no FileId. Keep
                // them as additions instead of turning valid paths into query
                // failures.
                continue;
            };
            let deleted_match = parent_file_ids.iter().find(|(old_path, old_file_id)| {
                old_path != &paths[index].path
                    && old_file_id == file_id
                    && paths
                        .iter()
                        .any(|path| path.kind == ChangeKind::Deleted && path.path == *old_path)
            });
            let (old_path, renamed) = if let Some((old_path, _)) = deleted_match {
                (old_path, true)
            } else {
                let Some((old_path, _)) = parent_file_ids.iter().find(|(old_path, old_file_id)| {
                    old_path != &paths[index].path && old_file_id == file_id
                }) else {
                    continue;
                };
                (old_path, false)
            };
            paths[index].kind = if renamed {
                ChangeKind::Renamed
            } else {
                ChangeKind::Copied
            };
            paths[index].old_path = Some(old_path.clone());
            paths[index].rename_origins = if renamed {
                vec![old_path.clone()]
            } else {
                Vec::new()
            };
            paths[index].similarity = Some(100);
        }
        Ok(paths)
    }

    fn jj_parents(&self, commit: &CommitId) -> Result<Vec<String>> {
        let command = VcsCommand::new([
            "jj",
            "--no-pager",
            "--color=never",
            "--ignore-working-copy",
            "log",
            "--no-graph",
            "-r",
            "${commit}-",
            "-T",
            "commit_id ++ \"\\n\"",
        ]);
        let (output, expanded) = self.execute(
            VcsQuery::ChangedPaths,
            &command,
            &[Placeholder::Commit(commit.as_str())],
        )?;
        let text = strict_utf8(VcsQuery::ChangedPaths, &expanded, output)?;
        parse_commit_text(&text).map_err(|reason| Error::VcsQueryMalformedOutput {
            query: VcsQuery::ChangedPaths,
            command: expanded,
            reason,
            stdout: text.into_bytes(),
        })
    }

    fn jj_files_contents(
        &self,
        revision: &str,
        filesets: &[String],
    ) -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let mut list_command = vec![
            String::from("jj"),
            String::from("--no-pager"),
            String::from("--color=never"),
            String::from("--ignore-working-copy"),
            String::from("file"),
            String::from("list"),
            String::from("-r"),
            revision.to_owned(),
            String::from("-T"),
            String::from("\"\\0\" ++ path"),
        ];
        list_command.extend(filesets.iter().cloned());
        let list_command = VcsCommand::new(list_command);
        let (path_output, expanded) = self.execute(VcsQuery::ChangedPaths, &list_command, &[])?;
        let paths = parse_nul_prefixed_paths(&path_output).map_err(|reason| {
            Error::VcsQueryMalformedOutput {
                query: VcsQuery::ChangedPaths,
                command: expanded,
                reason,
                stdout: path_output,
            }
        })?;

        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let command = VcsCommand::new([
                String::from("jj"),
                String::from("--no-pager"),
                String::from("--color=never"),
                String::from("--ignore-working-copy"),
                String::from("file"),
                String::from("show"),
                String::from("-r"),
                revision.to_owned(),
                String::from("-T"),
                String::new(),
                jj_fileset(&path)?,
            ]);
            let (contents, _) = self.execute(VcsQuery::ChangedPaths, &command, &[])?;
            files.push((path, contents));
        }
        Ok(files)
    }

    fn jj_file_ids(&self, revision: &str, filesets: &[String]) -> Result<Vec<(PathBuf, String)>> {
        // `file list` preserves raw repository paths and entry types, while
        // `debug tree` exposes content IDs without materializing every blob.
        // Parse the debug records against that exact ordered metadata so text in
        // a filename cannot be mistaken for a content ID.
        let mut list_command = vec![
            String::from("jj"),
            String::from("--no-pager"),
            String::from("--color=never"),
            String::from("--ignore-working-copy"),
            String::from("file"),
            String::from("list"),
            String::from("-r"),
            revision.to_owned(),
            String::from("-T"),
            String::from(r#""\0" ++ path ++ "\0" ++ file_type"#),
        ];
        list_command.extend(filesets.iter().cloned());
        let list_command = VcsCommand::new(list_command);
        let (path_output, list_expanded) =
            self.execute(VcsQuery::ChangedPaths, &list_command, &[])?;
        let entries = parse_jj_tree_entries(&path_output).map_err(|reason| {
            Error::VcsQueryMalformedOutput {
                query: VcsQuery::ChangedPaths,
                command: list_expanded,
                reason,
                stdout: path_output,
            }
        })?;

        let mut id_command = vec![
            String::from("jj"),
            String::from("--no-pager"),
            String::from("--color=never"),
            String::from("--ignore-working-copy"),
            String::from("debug"),
            String::from("tree"),
            String::from("-r"),
            revision.to_owned(),
        ];
        id_command.extend(filesets.iter().cloned());
        let id_command = VcsCommand::new(id_command);
        let (id_output, id_expanded) = self.execute(VcsQuery::ChangedPaths, &id_command, &[])?;
        parse_jj_file_ids(&entries, &id_output).map_err(|reason| Error::VcsQueryMalformedOutput {
            query: VcsQuery::ChangedPaths,
            command: id_expanded,
            reason,
            stdout: id_output,
        })
    }

    fn hg_changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        let mut parents = self.hg_parents(commit)?;
        if parents.is_empty() {
            parents.push(String::from("null"));
        }
        let mut per_parent = Vec::new();
        for parent in parents {
            let (output, command) = self.execute(
                VcsQuery::ChangedPaths,
                &self.commands.changed_paths,
                &[
                    Placeholder::Commit(commit.as_str()),
                    Placeholder::Parent(&parent),
                ],
            )?;
            let mut paths = parse_name_status_query(output, command)?;
            normalize_hg_renames(&mut paths);
            self.annotate_hg_copy_similarity(&mut paths, &parent, commit)?;
            per_parent.push(paths);
        }
        Ok(intersect_parent_changes(per_parent))
    }

    fn git_changed_paths(&self, commit: &CommitId) -> Result<Vec<ChangedPath>> {
        if self.changed_paths_overridden {
            let (output, command) = self.execute(
                VcsQuery::ChangedPaths,
                &self.commands.changed_paths,
                &[Placeholder::Commit(commit.as_str())],
            )?;
            return parse_name_status_query(output, command);
        }
        let parents = self.git_parents(commit)?;
        if parents.len() < 2 {
            let (output, command) = self.execute(
                VcsQuery::ChangedPaths,
                &self.commands.changed_paths,
                &[Placeholder::Commit(commit.as_str())],
            )?;
            return parse_name_status_query(output, command);
        }

        let mut per_parent = Vec::with_capacity(parents.len());
        for parent in parents {
            let command = VcsCommand::new([
                String::from("git"),
                String::from("diff"),
                String::from("--name-status"),
                String::from("--find-renames"),
                String::from("--find-copies"),
                String::from("--find-copies-harder"),
                String::from("-z"),
                parent,
                commit.as_str().to_owned(),
                String::from("--"),
            ]);
            let (output, expanded) = self.execute(VcsQuery::ChangedPaths, &command, &[])?;
            per_parent.push(parse_name_status_query(output, expanded)?);
        }
        Ok(intersect_parent_changes(per_parent))
    }

    fn git_parents(&self, commit: &CommitId) -> Result<Vec<String>> {
        let command = VcsCommand::new([
            "git",
            "rev-list",
            "--parents",
            "-n",
            "1",
            "--end-of-options",
            "${commit}",
        ]);
        let (output, expanded) = self.execute(
            VcsQuery::ChangedPaths,
            &command,
            &[Placeholder::Commit(commit.as_str())],
        )?;
        let text = strict_utf8(VcsQuery::ChangedPaths, &expanded, output)?;
        let line = text.strip_suffix('\n').unwrap_or(&text);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut fields = line.split_whitespace();
        fields
            .next()
            .ok_or_else(|| Error::VcsQueryMalformedOutput {
                query: VcsQuery::ChangedPaths,
                command: expanded.clone(),
                reason: String::from("Git parent query returned no commit"),
                stdout: text.as_bytes().to_vec(),
            })?;
        Ok(fields.map(str::to_owned).collect())
    }

    fn annotate_hg_copy_similarity(
        &self,
        paths: &mut [ChangedPath],
        parent: &str,
        commit: &CommitId,
    ) -> Result<()> {
        let Some(fragment_directory) = &self.fragment_directory else {
            return Ok(());
        };
        for path in paths.iter_mut().filter(|path| {
            matches!(path.kind, ChangeKind::Copied | ChangeKind::Renamed)
                && path.similarity.is_none()
                && possible_fragment_path(
                    &path.path,
                    fragment_directory,
                    &self.fragment_section_directories,
                    &self.fragment_section_patterns,
                )
        }) {
            let old_path = path
                .old_path
                .as_deref()
                .expect("copy and rename records have an old path");
            let old = self.hg_file_contents(parent, old_path)?;
            let new = self.hg_file_contents(commit.as_str(), &path.path)?;
            path.similarity = Some(if old == new { 100 } else { 0 });
        }
        Ok(())
    }

    fn hg_file_contents(&self, revision: &str, path: &Path) -> Result<Vec<u8>> {
        self.execute_os(
            VcsQuery::ChangedPaths,
            vec![
                OsString::from("hg"),
                OsString::from("cat"),
                OsString::from("-r"),
                OsString::from(revision),
                OsString::from("--"),
                path.as_os_str().to_os_string(),
            ],
        )
    }

    fn hg_parents(&self, commit: &CommitId) -> Result<Vec<String>> {
        let command = VcsCommand::new(["hg", "log", "-r", "parents(${commit})", "-T", "{node}\\n"]);
        let (output, expanded) = self.execute(
            VcsQuery::ChangedPaths,
            &command,
            &[Placeholder::Commit(commit.as_str())],
        )?;
        let text = strict_utf8(VcsQuery::ChangedPaths, &expanded, output)?;
        parse_commit_text(&text).map_err(|reason| Error::VcsQueryMalformedOutput {
            query: VcsQuery::ChangedPaths,
            command: expanded,
            reason,
            stdout: text.into_bytes(),
        })
    }

    fn message(&self, commit: &CommitId) -> Result<String> {
        let (output, command) = self.execute(
            VcsQuery::Message,
            &self.commands.message,
            &[Placeholder::Commit(commit.as_str())],
        )?;
        strict_utf8(VcsQuery::Message, &command, output)
    }

    fn execute(
        &self,
        query: VcsQuery,
        command: &VcsCommand,
        placeholders: &[Placeholder<'_>],
    ) -> Result<(Vec<u8>, Vec<String>)> {
        let argv = expand_command(command, placeholders);
        let os_argv = argv.iter().map(OsString::from).collect::<Vec<_>>();
        let output =
            self.runner
                .run(&self.root, &os_argv)
                .map_err(|source| Error::VcsQueryCommandIo {
                    query,
                    command: argv.clone(),
                    source,
                })?;
        if !output.status.success() {
            return Err(Error::VcsQueryCommandFailed {
                query,
                command: argv,
                status: output.status,
                stderr: output.stderr,
            });
        }
        Ok((output.stdout, argv))
    }

    fn execute_os(&self, query: VcsQuery, argv: Vec<OsString>) -> Result<Vec<u8>> {
        let command = argv
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let output =
            self.runner
                .run(&self.root, &argv)
                .map_err(|source| Error::VcsQueryCommandIo {
                    query,
                    command: command.clone(),
                    source,
                })?;
        if !output.status.success() {
            return Err(Error::VcsQueryCommandFailed {
                query,
                command,
                status: output.status,
                stderr: output.stderr,
            });
        }
        Ok(output.stdout)
    }
}

fn intersect_parent_changes(per_parent: Vec<Vec<ChangedPath>>) -> Vec<ChangedPath> {
    let mut per_parent = per_parent.into_iter();
    let Some(mut paths) = per_parent.next() else {
        return Vec::new();
    };
    for other in per_parent {
        let changed = other
            .iter()
            .map(|path| (&path.path, path))
            .collect::<std::collections::HashMap<_, _>>();
        paths.retain_mut(|path| {
            let Some(other) = changed.get(&path.path) else {
                return false;
            };
            for origin in &other.rename_origins {
                if !path.rename_origins.contains(origin) {
                    path.rename_origins.push(origin.clone());
                }
            }
            let other_is_movement = matches!(other.kind, ChangeKind::Copied | ChangeKind::Renamed);
            let path_is_movement = matches!(path.kind, ChangeKind::Copied | ChangeKind::Renamed);
            if other_is_movement
                && (!path_is_movement
                    || (other.similarity == Some(100) && path.similarity != Some(100)))
            {
                path.kind = other.kind;
                path.old_path.clone_from(&other.old_path);
                path.similarity = other.similarity;
            } else if other.similarity == Some(100) {
                path.similarity = Some(100);
            }
            true
        });
    }
    paths
}

fn normalize_hg_renames(paths: &mut [ChangedPath]) {
    let deleted = paths
        .iter()
        .filter(|path| path.kind == ChangeKind::Deleted)
        .map(|path| path.path.clone())
        .collect::<std::collections::HashSet<_>>();
    for path in paths
        .iter_mut()
        .filter(|path| path.kind == ChangeKind::Copied)
    {
        let Some(old_path) = &path.old_path else {
            continue;
        };
        if deleted.contains(old_path) {
            path.kind = ChangeKind::Renamed;
            path.rename_origins = vec![old_path.clone()];
        }
    }
}

fn hg_revset_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for character in value.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            character => literal.push(character),
        }
    }
    literal.push('"');
    literal
}

enum Placeholder<'a> {
    Base(&'a str),
    Commit(&'a str),
    Parent(&'a str),
}

fn expand_command(command: &VcsCommand, placeholders: &[Placeholder<'_>]) -> Vec<String> {
    command
        .argv()
        .iter()
        .map(|argument| {
            placeholders
                .iter()
                .fold(argument.clone(), |argument, value| {
                    let (name, value) = match value {
                        Placeholder::Base(value) => ("${base}", *value),
                        Placeholder::Commit(value) => ("${commit}", *value),
                        Placeholder::Parent(value) => ("${parent}", *value),
                    };
                    argument.replace(name, value)
                })
        })
        .collect()
}

fn strict_utf8(query: VcsQuery, command: &[String], bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|error| Error::VcsQueryInvalidUtf8 {
        query,
        command: command.to_vec(),
        stream: "standard output",
        bytes: error.into_bytes(),
    })
}

fn parse_commits(text: &str) -> std::result::Result<Vec<CommitId>, String> {
    parse_commit_text(text).map(|commits| commits.into_iter().map(CommitId::new).collect())
}

fn parse_commit_text(text: &str) -> std::result::Result<Vec<String>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n')
        .map(|record| {
            let record = record.strip_suffix('\r').unwrap_or(record);
            if record.is_empty() {
                Err(String::from("commit records must not be empty"))
            } else {
                Ok(record.to_owned())
            }
        })
        .collect()
}

fn parse_name_status_query(output: Vec<u8>, command: Vec<String>) -> Result<Vec<ChangedPath>> {
    parse_name_status_paths(&output).map_err(|reason| Error::VcsQueryMalformedOutput {
        query: VcsQuery::ChangedPaths,
        command,
        reason,
        stdout: output,
    })
}

fn jj_fileset(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::VcsQueryMalformedOutput {
            query: VcsQuery::ChangedPaths,
            command: Vec::new(),
            reason: String::from("Jujutsu path cannot be represented as a fileset"),
            stdout: Vec::new(),
        })?;
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    Ok(format!("root:{path:?}"))
}

fn possible_fragment_path(
    path: &Path,
    directory: &Path,
    sections: &[PathBuf],
    patterns: &[SectionPattern],
) -> bool {
    if path.extension().and_then(OsStr::to_str) != Some("md") {
        return false;
    }
    let Ok(relative) = path.strip_prefix(directory) else {
        return false;
    };
    (1..=2).contains(&relative.components().count())
        || sections
            .iter()
            .any(|section| relative.parent() == Some(section.as_path()))
        || relative
            .parent()
            .and_then(section_pattern_path)
            .is_some_and(|parent| {
                patterns
                    .iter()
                    .any(|pattern| pattern.captures(&parent).is_some())
            })
}

fn section_pattern_path(path: &Path) -> Option<String> {
    let mut rendered = String::new();
    for component in path {
        let component = component.to_str()?;
        if !rendered.is_empty() {
            rendered.push('/');
        }
        rendered.push_str(component);
    }
    Some(rendered)
}

fn parse_nul_prefixed_paths(output: &[u8]) -> std::result::Result<Vec<PathBuf>, String> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    let paths = output
        .strip_prefix(b"\0")
        .ok_or_else(|| String::from("file list must start with a NUL-framed path"))?;
    paths
        .split(|byte| *byte == b'\0')
        .map(|path| {
            if path.is_empty() {
                Err(String::from("file list contains an empty path"))
            } else {
                repository_relative_path_from_bytes(path)
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct JjTreeEntry {
    path: PathBuf,
    raw_path: Vec<u8>,
    file_type: String,
}

fn parse_jj_tree_entries(output: &[u8]) -> std::result::Result<Vec<JjTreeEntry>, String> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    let records = output
        .strip_prefix(b"\0")
        .ok_or_else(|| String::from("file list must start with a NUL-framed path"))?;
    let mut fields = records.split(|byte| *byte == b'\0');
    let mut entries = Vec::new();
    while let Some(path) = fields.next() {
        if path.is_empty() {
            return Err(String::from("file list contains an empty path"));
        }
        let file_type = fields
            .next()
            .ok_or_else(|| String::from("file list path is missing its entry type"))?;
        let file_type = std::str::from_utf8(file_type)
            .map_err(|_| String::from("Jujutsu entry type is not valid UTF-8"))?;
        if file_type.is_empty() {
            return Err(String::from("Jujutsu entry type is empty"));
        }
        entries.push(JjTreeEntry {
            path: repository_relative_path_from_bytes(path)?,
            raw_path: path.to_vec(),
            file_type: file_type.to_owned(),
        });
    }
    Ok(entries)
}

fn parse_jj_file_ids(
    entries: &[JjTreeEntry],
    output: &[u8],
) -> std::result::Result<Vec<(PathBuf, String)>, String> {
    const PREFIX: &[u8] = b"Ok(Resolved(Some(File { id: FileId(\"";
    const SUFFIX: &[u8] = b"\"), executable: ";
    let mut remaining = output;
    let mut ids = Vec::new();
    for entry in entries {
        remaining = remaining
            .strip_prefix(entry.raw_path.as_slice())
            .ok_or_else(|| format!("debug tree is missing entry {:?}", entry.path))?;
        remaining = remaining
            .strip_prefix(b": ")
            .ok_or_else(|| format!("debug tree entry {:?} is missing metadata", entry.path))?;
        let line_end = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| format!("debug tree entry {:?} is not terminated", entry.path))?;
        let metadata = &remaining[..line_end];
        remaining = &remaining[line_end + 1..];
        if entry.file_type != "file" {
            continue;
        }
        let value = metadata
            .strip_prefix(PREFIX)
            .ok_or_else(|| format!("debug tree entry {:?} is not a file", entry.path))?;
        let Some(suffix) = find_bytes(value, SUFFIX) else {
            return Err(String::from("Jujutsu file ID is missing its terminator"));
        };
        let id = std::str::from_utf8(&value[..suffix])
            .map_err(|_| String::from("Jujutsu file ID is not valid UTF-8"))?;
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid Jujutsu file ID {id:?}"));
        }
        ids.push((entry.path.clone(), id.to_owned()));
    }
    if !remaining.is_empty() {
        return Err(String::from(
            "debug tree contains unexpected trailing output",
        ));
    }
    Ok(ids)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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

    /// Previous paths from every merge parent that reported this path as a
    /// rename destination.
    pub rename_origins: Vec<PathBuf>,

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
            rename_origins: Vec::new(),
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
        let old_path = old_path.into();
        let rename_origins = if kind == ChangeKind::Renamed {
            vec![old_path.clone()]
        } else {
            Vec::new()
        };
        Self {
            path: path.into(),
            old_path: Some(old_path),
            rename_origins,
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
        let mut changed = Self::with_old_path(path, kind, old_path);
        changed.similarity = Some(similarity);
        changed
    }
}

/// Parses the VCS contract's NUL-delimited name-status output.
pub fn parse_name_status_paths(output: &[u8]) -> std::result::Result<Vec<ChangedPath>, String> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    if !output.ends_with(b"\0") {
        return Err(String::from("changed-path output must end with NUL"));
    }
    let mut fields = output[..output.len() - 1].split(|byte| *byte == b'\0');
    let mut paths = Vec::new();

    while let Some(status) = fields.next() {
        if status.is_empty() {
            return Err(String::from("changed-path status must not be empty"));
        }
        let status = std::str::from_utf8(status)
            .map_err(|_| String::from("changed-path status is not valid UTF-8"))?;
        let path = fields
            .next()
            .ok_or_else(|| format!("status {status:?} is missing its path"))?;
        if path.is_empty() {
            return Err(format!("status {status:?} has an empty path"));
        }
        let (kind, similarity) = parse_change_status(status)?;
        if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
            let new_path = fields
                .next()
                .ok_or_else(|| format!("status {status:?} is missing its new path"))?;
            if new_path.is_empty() {
                return Err(format!("status {status:?} has an empty new path"));
            }
            let mut changed = ChangedPath::with_old_path(
                repository_relative_path_from_bytes(new_path)?,
                kind,
                repository_relative_path_from_bytes(path)?,
            );
            changed.similarity = similarity;
            paths.push(changed);
        } else {
            paths.push(ChangedPath::new(
                repository_relative_path_from_bytes(path)?,
                kind,
            ));
        }
    }

    Ok(paths)
}

fn parse_change_status(status: &str) -> std::result::Result<(ChangeKind, Option<u8>), String> {
    let (kind, value) = match status {
        "A" => return Ok((ChangeKind::Added, None)),
        "D" => return Ok((ChangeKind::Deleted, None)),
        "M" => return Ok((ChangeKind::Modified, None)),
        "T" => return Ok((ChangeKind::Other, None)),
        value if value.starts_with('C') => (ChangeKind::Copied, &value[1..]),
        value if value.starts_with('R') => (ChangeKind::Renamed, &value[1..]),
        _ => return Err(format!("invalid changed-path status {status:?}")),
    };
    if value.is_empty() {
        return Ok((kind, None));
    }
    let similarity = value
        .parse::<u8>()
        .map_err(|_| format!("invalid copy or rename similarity {value:?}"))?;
    if similarity > 100 {
        return Err(format!(
            "copy or rename similarity exceeds 100: {similarity}"
        ));
    }
    Ok((kind, Some(similarity)))
}

fn repository_relative_path_from_bytes(path: &[u8]) -> std::result::Result<PathBuf, String> {
    let path = path_from_bytes(path)?;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(String::from(
                    "changed path must be repository-relative without parent components",
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(String::from(
            "changed path must contain a repository-relative path component",
        ));
    }
    Ok(normalized)
}

#[cfg(unix)]
fn path_from_bytes(path: &[u8]) -> std::result::Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;

    Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
}

#[cfg(not(unix))]
#[cfg_attr(test, mutants::skip)]
fn path_from_bytes(path: &[u8]) -> std::result::Result<PathBuf, String> {
    let path = std::str::from_utf8(path)
        .map_err(|_| String::from("changed path is not valid platform text"))?;
    Ok(PathBuf::from(path))
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
    use std::collections::VecDeque;
    use std::process::Command as ProcessCommand;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::Config;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    struct FakeRunner {
        outputs: Arc<Mutex<VecDeque<std::process::Output>>>,
        commands: Arc<Mutex<Vec<Vec<OsString>>>>,
    }

    impl FakeRunner {
        fn successful(outputs: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                outputs: Arc::new(Mutex::new(
                    outputs
                        .into_iter()
                        .map(|stdout| std::process::Output {
                            status: successful_exit_status(),
                            stdout,
                            stderr: Vec::new(),
                        })
                        .collect(),
                )),
                commands: Arc::default(),
            }
        }
    }

    fn jj_debug_file_ids(entries: &[(&str, &str)]) -> Vec<u8> {
        entries
            .iter()
            .map(|(path, id)| {
                format!(
                    "{path}: Ok(Resolved(Some(File {{ id: FileId(\"{id}\"), executable: false, copy_id: CopyId(\"\") }})))\n"
                )
            })
            .collect::<String>()
            .into_bytes()
    }

    #[cfg(unix)]
    fn successful_exit_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn successful_exit_status() -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(0)
    }

    impl Runner for FakeRunner {
        fn run(&self, _root: &Path, argv: &[OsString]) -> std::io::Result<std::process::Output> {
            self.commands.lock().expect("commands").push(argv.to_vec());
            Ok(self
                .outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .expect("fake output"))
        }
    }

    #[test]
    fn parses_name_status_paths_with_spaces_and_newlines() {
        let paths =
            parse_name_status_paths(b"M\0src/main.rs\0A\0docs/file name.md\0D\0weird\nname.rs\0")
                .expect("valid records");

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
    fn parses_jj_regular_file_paths_and_content_ids() {
        assert_eq!(
            parse_nul_prefixed_paths(b"\0src/lib.rs\0docs/file name.md"),
            Ok(vec![
                PathBuf::from("src/lib.rs"),
                PathBuf::from("docs/file name.md")
            ])
        );
        let entries =
            parse_jj_tree_entries(b"\0src/lib.rs\0file\0docs/file name.md\0file\0link\0symlink")
                .expect("tree entries");
        let mut output = jj_debug_file_ids(&[
            ("src/lib.rs", "1111111111111111111111111111111111111111"),
            (
                "docs/file name.md",
                "2222222222222222222222222222222222222222",
            ),
        ]);
        output.extend_from_slice(
            b"link: Ok(Resolved(Some(Symlink(SymlinkId(\"3333333333333333333333333333333333333333\")))))\n",
        );
        assert_eq!(
            parse_jj_file_ids(&entries, &output),
            Ok(vec![
                (
                    PathBuf::from("src/lib.rs"),
                    String::from("1111111111111111111111111111111111111111")
                ),
                (
                    PathBuf::from("docs/file name.md"),
                    String::from("2222222222222222222222222222222222222222")
                ),
            ])
        );
        let entry = parse_jj_tree_entries(b"\0file\0file").expect("file entry");
        assert!(
            parse_jj_file_ids(
                &entry,
                b"file: Ok(Resolved(Some(File { id: FileId(\"\"), executable: false })))\n"
            )
            .is_err()
        );
        assert!(
            parse_jj_file_ids(
                &entry,
                b"file: Ok(Resolved(Some(File { id: FileId(\"not-hex\"), executable: false })))\n"
            )
            .is_err()
        );
    }

    #[test]
    fn jj_file_id_parser_does_not_match_debug_syntax_in_a_filename() {
        let entries = parse_jj_tree_entries(b"\0docs/Some(File { id: FileId(\"deadbeef\")\0file")
            .expect("tree entry");
        let output = b"docs/Some(File { id: FileId(\"deadbeef\"): Ok(Resolved(Some(File { id: FileId(\"1111111111111111111111111111111111111111\"), executable: false, copy_id: CopyId(\"\") })))\n";

        assert_eq!(
            parse_jj_file_ids(&entries, output),
            Ok(vec![(
                PathBuf::from("docs/Some(File { id: FileId(\"deadbeef\")"),
                String::from("1111111111111111111111111111111111111111")
            )])
        );
    }

    #[test]
    fn parses_rename_status_as_surviving_new_path() {
        let paths = parse_name_status_paths(b"R100\0changes.d/old.md\0changes.d/new.md\0")
            .expect("valid rename");

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
        let paths = parse_name_status_paths(b"C85\0changes.d/old.md\0changes.d/new.md\0")
            .expect("valid copy");

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
    fn parses_type_change_status_as_other() {
        assert_eq!(
            parse_name_status_paths(b"T\0src/link.rs\0"),
            Ok(vec![ChangedPath::new("src/link.rs", ChangeKind::Other)])
        );
    }

    #[test]
    fn rejects_status_tokens_outside_the_changed_path_contract() {
        for status in [
            "Agarbage", "D0", "M100", "T0", "X", "Cgarbage", "R-1", "R101",
        ] {
            let output = format!("{status}\0changes.d/foo.md\0");
            assert!(
                parse_name_status_paths(output.as_bytes()).is_err(),
                "accepted malformed status {status:?}"
            );
        }
        for status in ["X", "T0"] {
            let output = format!("{status}\0old.md\0new.md\0");
            assert!(
                parse_name_status_paths(output.as_bytes()).is_err(),
                "accepted unknown rename-shaped status {status:?}"
            );
        }
    }

    #[test]
    fn normalizes_current_directory_changed_path_components() {
        assert_eq!(
            parse_name_status_paths(b"M\0./src/file.rs\0R100\0./old.rs\0./new.rs\0"),
            Ok(vec![
                ChangedPath::new("src/file.rs", ChangeKind::Modified),
                ChangedPath::with_old_path_and_similarity(
                    "new.rs",
                    ChangeKind::Renamed,
                    "old.rs",
                    100,
                ),
            ])
        );
        assert!(parse_name_status_paths(b"M\0.\0").is_err());
    }

    #[test]
    fn rejects_a_lone_empty_commit_record() {
        for output in ["\n", "\r\n"] {
            assert_eq!(
                parse_commit_text(output),
                Err(String::from("commit records must not be empty"))
            );
        }
        assert_eq!(parse_commit_text(""), Ok(Vec::new()));
    }

    #[test]
    fn rejects_changed_paths_that_are_not_repository_relative() {
        for output in [
            b"M\0/absolute.rs\0".as_slice(),
            b"M\0../outside.rs\0".as_slice(),
            b"R100\0../old.rs\0new.rs\0".as_slice(),
            b"R100\0old.rs\0dir/../../new.rs\0".as_slice(),
        ] {
            assert!(
                parse_name_status_paths(output).is_err(),
                "accepted malformed record {output:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_changed_path_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let paths = parse_name_status_paths(b"M\0unrelated-\xff\0").expect("valid byte path");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].path.as_os_str().as_bytes(), b"unrelated-\xff");
    }

    #[test]
    fn deleted_paths_do_not_survive() {
        assert!(!ChangeKind::Deleted.path_survives());
        assert!(ChangeKind::Added.path_survives());
        assert!(ChangeKind::Modified.path_survives());
    }

    #[test]
    fn configured_runner_expands_arguments_without_a_shell() {
        let config = Config::parse(
            r#"
            [vcs.commands]
            commits = ["fake vcs", "--range=${base}", "literal;$(ignored)"]
            changed-paths = ["fake vcs", "--commit=${commit}"]
            message = ["fake vcs", "message", "${commit}"]
            "#,
        )
        .expect("config");
        let runner = FakeRunner::successful([
            b"one\ntwo\n".to_vec(),
            b"M\0src/lib.rs\0".to_vec(),
            b"raw message\n".to_vec(),
        ]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Git, &config.vcs, runner);

        assert_eq!(
            vcs.commits("base name").expect("commits"),
            vec![CommitId::new("one"), CommitId::new("two")]
        );
        assert_eq!(
            vcs.changed_paths(&CommitId::new("one")).expect("paths"),
            vec![ChangedPath::new("src/lib.rs", ChangeKind::Modified)]
        );
        assert_eq!(
            vcs.message(&CommitId::new("one")).expect("message"),
            "raw message\n"
        );
        assert_eq!(
            *commands.lock().expect("commands"),
            vec![
                vec!["fake vcs", "--range=base name", "literal;$(ignored)"],
                vec!["fake vcs", "--commit=one"],
                vec!["fake vcs", "message", "one"],
            ]
        );
    }

    #[test]
    fn empty_programmatic_command_returns_an_error_instead_of_panicking() {
        let mut config = VcsConfig::default();
        config.commands.commits = Some(VcsCommand::new(std::iter::empty::<String>()));
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Git, &config, ProcessRunner);

        let error = vcs.commits("base").expect_err("empty command must fail");

        assert!(matches!(
            error,
            Error::VcsQueryCommandIo { command, source, .. }
                if command.is_empty() && source.kind() == std::io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn query_adapter_rejects_invalid_utf8_and_malformed_records() {
        let config = VcsConfig::default();
        let invalid = QueryVcs::new(
            Path::new("."),
            VcsPreset::Git,
            &config,
            FakeRunner::successful([vec![0xff]]),
        );
        assert!(matches!(
            invalid.commits("base"),
            Err(Error::VcsQueryInvalidUtf8 { bytes, .. }) if bytes == vec![0xff]
        ));

        let malformed = QueryVcs::new(
            Path::new("."),
            VcsPreset::Git,
            &config,
            FakeRunner::successful([b"commit parent\n".to_vec(), b"M\0src/lib.rs".to_vec()]),
        );
        assert!(matches!(
            malformed.changed_paths(&CommitId::new("commit")),
            Err(Error::VcsQueryMalformedOutput { stdout, .. }) if stdout == b"M\0src/lib.rs"
        ));

        let invalid_status = QueryVcs::new(
            Path::new("."),
            VcsPreset::Git,
            &config,
            FakeRunner::successful([
                b"commit parent\n".to_vec(),
                b"Agarbage\0changes.d/foo.md\0".to_vec(),
            ]),
        );
        assert!(matches!(
            invalid_status.changed_paths(&CommitId::new("commit")),
            Err(Error::VcsQueryMalformedOutput { stdout, .. })
                if stdout == b"Agarbage\0changes.d/foo.md\0"
        ));
    }

    #[test]
    fn hg_adapter_computes_copy_similarity_from_revision_contents() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Hg,
            &config,
            FakeRunner::successful([
                b"parent\n".to_vec(),
                b"M\0src/lib.rs\0C\0changes.d/original.md\0changes.d/unchanged.md\0C\0changes.d/original.md\0changes.d/edited.md\0".to_vec(),
                b"same\n".to_vec(),
                b"same\n".to_vec(),
                b"same\n".to_vec(),
                b"edited\n".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("copy paths");

        assert_eq!(
            paths[0],
            ChangedPath::new("src/lib.rs", ChangeKind::Modified)
        );
        assert_eq!(paths[1].similarity, Some(100));
        assert_eq!(paths[2].similarity, Some(0));
    }

    #[test]
    fn hg_changed_path_override_is_the_complete_query() {
        let mut config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        config.commands.changed_paths = Some(VcsCommand::new([
            "custom-hg-query",
            "changed-paths",
            "${commit}",
        ]));
        let runner =
            FakeRunner::successful([b"C\0changes.d/original.md\0changes.d/copied.md\0".to_vec()]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Hg, &config, runner)
            .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("overridden paths");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path(
                "changes.d/copied.md",
                ChangeKind::Copied,
                "changes.d/original.md",
            )]
        );
        assert_eq!(
            *commands.lock().expect("commands"),
            vec![vec!["custom-hg-query", "changed-paths", "commit"]]
        );
    }

    #[test]
    fn hg_commits_override_receives_the_unmodified_base() {
        let mut config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        config.commands.commits = Some(VcsCommand::new(["custom-hg-query", "commits", "${base}"]));
        let runner = FakeRunner::successful([b"commit\n".to_vec()]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Hg, &config, runner);

        vcs.commits("foo bar) or all()")
            .expect("overridden commits");

        assert_eq!(
            *commands.lock().expect("commands"),
            vec![vec!["custom-hg-query", "commits", "foo bar) or all()"]]
        );
    }

    #[test]
    fn hg_adapter_does_not_probe_copies_outside_the_fragment_directory() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let runner = FakeRunner::successful([
            b"parent\n".to_vec(),
            b"C\0docs/original.md\0docs/copied.md\0".to_vec(),
        ]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Hg, &config, runner)
            .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("copy paths");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path(
                "docs/copied.md",
                ChangeKind::Copied,
                "docs/original.md",
            )]
        );
        assert_eq!(commands.lock().expect("commands").len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn hg_adapter_ignores_non_utf8_copy_paths_outside_fragments() {
        use std::os::unix::ffi::OsStrExt;

        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Hg,
            &config,
            FakeRunner::successful([
                b"parent\n".to_vec(),
                b"C\0docs/original-\xff\0docs/copied-\xff\0".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("copy paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].path.as_os_str().as_bytes(), b"docs/copied-\xff");
        assert_eq!(paths[0].similarity, None);
    }

    #[test]
    fn hg_adapter_compares_parentless_commits_with_null_revision() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let runner = FakeRunner::successful([Vec::new(), b"A\0initial.txt\0".to_vec()]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Hg, &config, runner);

        let paths = vcs
            .changed_paths(&CommitId::new("root"))
            .expect("root paths");

        assert_eq!(
            paths,
            vec![ChangedPath::new("initial.txt", ChangeKind::Added)]
        );
        assert!(commands.lock().expect("commands")[1].contains(&OsString::from("null")));
    }

    #[test]
    fn hg_adapter_preserves_an_exact_copy_seen_against_any_merge_parent() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Hg,
            &config,
            FakeRunner::successful([
                b"left\nright\n".to_vec(),
                b"C\0changes.d/original.md\0changes.d/copied.md\0".to_vec(),
                b"original\n".to_vec(),
                b"edited\n".to_vec(),
                b"C\0changes.d/original.md\0changes.d/copied.md\0".to_vec(),
                b"same\n".to_vec(),
                b"same\n".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("merge"))
            .expect("merge copy paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].similarity, Some(100));
    }

    #[test]
    fn hg_adapter_preserves_rename_sources_from_every_merge_parent() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Hg,
            &config,
            FakeRunner::successful([
                b"left\nright\n".to_vec(),
                b"C\0docs/b\0docs/c\0D\0docs/b\0".to_vec(),
                b"C\0src/a\0docs/c\0D\0src/a\0".to_vec(),
            ]),
        );

        let paths = vcs
            .changed_paths(&CommitId::new("merge"))
            .expect("merge rename paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, ChangeKind::Renamed);
        assert_eq!(
            paths[0].rename_origins,
            [PathBuf::from("docs/b"), PathBuf::from("src/a")]
        );
    }

    #[test]
    fn possible_fragment_path_accepts_nested_section_directories() {
        assert!(possible_fragment_path(
            Path::new("changes.d/nested/core/feature.md"),
            Path::new("changes.d"),
            &[PathBuf::from("nested/core")],
            &[],
        ));
        assert!(!possible_fragment_path(
            Path::new("changes.d/unconfigured/deep/feature.md"),
            Path::new("changes.d"),
            &[PathBuf::from("nested/core")],
            &[],
        ));
    }

    #[test]
    fn possible_fragment_path_accepts_patterned_section_directories() {
        let pattern = "packages/{name}".parse().expect("section pattern");

        assert!(possible_fragment_path(
            Path::new("changes.d/packages/core/feature.md"),
            Path::new("changes.d"),
            &[],
            &[pattern],
        ));
        assert!(!possible_fragment_path(
            Path::new("changes.d/crates/core/feature.md"),
            Path::new("changes.d"),
            &[],
            &["packages/{name}".parse().expect("section pattern")],
        ));
    }

    #[test]
    fn section_pattern_paths_use_forward_slashes() {
        let path = Path::new("packages").join("core");

        assert_eq!(
            section_pattern_path(&path).as_deref(),
            Some("packages/core")
        );
    }

    #[cfg(windows)]
    #[test]
    fn possible_fragment_path_accepts_patterned_windows_directories() {
        let pattern = "packages/{name}".parse().expect("section pattern");

        assert!(possible_fragment_path(
            Path::new(r"changes.d\packages\core\feature.md"),
            Path::new("changes.d"),
            &[],
            &[pattern],
        ));
    }

    #[cfg(unix)]
    #[test]
    fn hg_adapter_probes_non_utf8_fragment_paths() {
        use std::os::unix::ffi::OsStrExt;

        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Hg,
            &config,
            FakeRunner::successful([
                b"parent\n".to_vec(),
                b"C\0changes.d/original-\xff.md\0changes.d/copied-\xfe.md\0".to_vec(),
                b"original\n".to_vec(),
                b"edited\n".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("non-UTF-8 fragment paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0].path.as_os_str().as_bytes(),
            b"changes.d/copied-\xfe.md"
        );
        assert_eq!(paths[0].similarity, Some(0));
    }

    #[test]
    fn parent_intersection_prefers_movement_and_exact_originals_conservatively() {
        let paths = intersect_parent_changes(vec![
            vec![
                ChangedPath::new("added.md", ChangeKind::Added),
                ChangedPath::with_old_path_and_similarity(
                    "becomes-exact.md",
                    ChangeKind::Renamed,
                    "first.md",
                    0,
                ),
                ChangedPath::with_old_path_and_similarity(
                    "stays-exact.md",
                    ChangeKind::Renamed,
                    "exact.md",
                    100,
                ),
                ChangedPath::new("ordinary.md", ChangeKind::Added),
            ],
            vec![
                ChangedPath::with_old_path_and_similarity(
                    "added.md",
                    ChangeKind::Copied,
                    "source.md",
                    100,
                ),
                ChangedPath::with_old_path_and_similarity(
                    "becomes-exact.md",
                    ChangeKind::Renamed,
                    "exact.md",
                    100,
                ),
                ChangedPath::with_old_path_and_similarity(
                    "stays-exact.md",
                    ChangeKind::Renamed,
                    "second.md",
                    0,
                ),
                ChangedPath::new("ordinary.md", ChangeKind::Modified),
            ],
        ]);
        let mut becomes_exact = ChangedPath::with_old_path_and_similarity(
            "becomes-exact.md",
            ChangeKind::Renamed,
            "exact.md",
            100,
        );
        becomes_exact
            .rename_origins
            .insert(0, PathBuf::from("first.md"));
        let mut stays_exact = ChangedPath::with_old_path_and_similarity(
            "stays-exact.md",
            ChangeKind::Renamed,
            "exact.md",
            100,
        );
        stays_exact.rename_origins.push(PathBuf::from("second.md"));

        assert_eq!(
            paths,
            vec![
                ChangedPath::with_old_path_and_similarity(
                    "added.md",
                    ChangeKind::Copied,
                    "source.md",
                    100,
                ),
                becomes_exact,
                stays_exact,
                ChangedPath::new("ordinary.md", ChangeKind::Added),
            ]
        );
    }

    #[test]
    fn parent_intersection_preserves_every_rename_origin() {
        let paths = intersect_parent_changes(vec![
            vec![ChangedPath::with_old_path_and_similarity(
                "docs/c",
                ChangeKind::Renamed,
                "docs/b",
                100,
            )],
            vec![ChangedPath::with_old_path_and_similarity(
                "docs/c",
                ChangeKind::Renamed,
                "src/a",
                100,
            )],
        ]);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].path, Path::new("docs/c"));
        assert_eq!(paths[0].old_path.as_deref(), Some(Path::new("docs/b")));
        assert_eq!(paths[0].similarity, Some(100));
        assert_eq!(
            paths[0].rename_origins,
            [PathBuf::from("docs/b"), PathBuf::from("src/a")]
        );
    }

    #[test]
    fn hg_default_commits_literalizes_the_base_revision() {
        let config = VcsConfig {
            preset: VcsPreset::Hg,
            ..VcsConfig::default()
        };
        let runner = FakeRunner::successful([b"commit\n".to_vec()]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Hg, &config, runner);

        vcs.commits(r#"foo "bar" \ branch()"#)
            .expect("literal Mercurial base");

        assert!(
            commands.lock().expect("commands")[0].contains(&OsString::from(
                "sort(only(., \"foo \\\"bar\\\" \\\\ branch()\"), rev)"
            ))
        );
    }

    #[test]
    fn jj_adapter_marks_only_byte_identical_added_fragments_as_copies() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let runner = FakeRunner::successful([
            b"A\0changes.d/unchanged.md\0A\0changes.d/edited.md\0".to_vec(),
            b"parent\n".to_vec(),
            b"\0changes.d/edited.md\0file\0changes.d/unchanged.md\0file".to_vec(),
            jj_debug_file_ids(&[
                (
                    "changes.d/edited.md",
                    "2222222222222222222222222222222222222222",
                ),
                (
                    "changes.d/unchanged.md",
                    "1111111111111111111111111111111111111111",
                ),
            ]),
            b"\0changes.d/a.md\0file\0changes.d/z.md\0file\0docs/template.md\0file".to_vec(),
            jj_debug_file_ids(&[
                ("changes.d/a.md", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                ("changes.d/z.md", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                (
                    "docs/template.md",
                    "1111111111111111111111111111111111111111",
                ),
            ]),
        ]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Jj, &config, runner)
            .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("copy paths");

        assert_eq!(
            paths[0],
            ChangedPath::with_old_path_and_similarity(
                "changes.d/unchanged.md",
                ChangeKind::Copied,
                "docs/template.md",
                100,
            )
        );
        assert_eq!(
            paths[1],
            ChangedPath::new("changes.d/edited.md", ChangeKind::Added)
        );
        let commands = commands.lock().expect("commands");
        assert_eq!(commands.len(), 6);
        assert!(commands[4].contains(&OsString::from("all()")));
        assert!(!commands.iter().any(|command| {
            command.contains(&OsString::from("show")) && command.contains(&OsString::from("all()"))
        }));
    }

    #[test]
    fn jj_adapter_keeps_added_symlink_fragments_as_additions() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"A\0changes.d/link.md\0".to_vec(),
                b"parent\n".to_vec(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        assert_eq!(
            vcs.changed_paths(&CommitId::new("commit"))
                .expect("symlink fragment path"),
            vec![ChangedPath::new("changes.d/link.md", ChangeKind::Added)]
        );
    }

    #[test]
    fn jj_changed_path_override_is_the_complete_query() {
        let mut config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        config.commands.changed_paths = Some(VcsCommand::new([
            "custom-jj-query",
            "changed-paths",
            "${commit}",
        ]));
        let runner = FakeRunner::successful([b"A\0changes.d/feature.md\0".to_vec()]);
        let commands = Arc::clone(&runner.commands);
        let vcs = QueryVcs::new(Path::new("."), VcsPreset::Jj, &config, runner)
            .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("overridden paths");

        assert_eq!(
            paths,
            vec![ChangedPath::new("changes.d/feature.md", ChangeKind::Added)]
        );
        assert_eq!(
            *commands.lock().expect("commands"),
            vec![vec!["custom-jj-query", "changed-paths", "commit"]]
        );
    }

    #[cfg(unix)]
    #[test]
    fn jj_fileset_preserves_literal_backslashes() {
        assert_eq!(
            jj_fileset(Path::new(r"changes.d/foo\bar.md")).expect("fileset"),
            r#"root:"changes.d/foo\\bar.md""#
        );
    }

    #[test]
    fn jj_adapter_compares_fragment_copies_with_each_merge_parent() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"A\0changes.d/copied.md\0".to_vec(),
                b"left\nright\n".to_vec(),
                b"\0changes.d/copied.md\0file".to_vec(),
                jj_debug_file_ids(&[(
                    "changes.d/copied.md",
                    "1111111111111111111111111111111111111111",
                )]),
                b"\0changes.d/original.md\0file".to_vec(),
                jj_debug_file_ids(&[(
                    "changes.d/original.md",
                    "1111111111111111111111111111111111111111",
                )]),
                b"\0changes.d/original.md\0file".to_vec(),
                jj_debug_file_ids(&[(
                    "changes.d/original.md",
                    "2222222222222222222222222222222222222222",
                )]),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("merge"))
            .expect("merge copy paths");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/copied.md",
                ChangeKind::Copied,
                "changes.d/original.md",
                100,
            )]
        );
    }

    #[test]
    fn jj_adapter_computes_similarity_for_reported_renames() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"R\0changes.d/old.md\0changes.d/new.md\0".to_vec(),
                b"parent\n".to_vec(),
                b"\0changes.d/old.md".to_vec(),
                b"same\n".to_vec(),
                b"\0changes.d/new.md".to_vec(),
                b"same\n".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("rename paths");

        assert_eq!(
            paths,
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/new.md",
                ChangeKind::Renamed,
                "changes.d/old.md",
                100,
            )]
        );
    }

    #[test]
    fn jj_adapter_compares_reported_rename_contents_containing_nul() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"R\0changes.d/old.md\0changes.d/new.md\0".to_vec(),
                b"parent\n".to_vec(),
                b"\0changes.d/old.md".to_vec(),
                b"same\0contents".to_vec(),
                b"\0changes.d/new.md".to_vec(),
                b"same\0contents".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        assert_eq!(
            vcs.changed_paths(&CommitId::new("commit"))
                .expect("NUL-containing rename contents"),
            vec![ChangedPath::with_old_path_and_similarity(
                "changes.d/new.md",
                ChangeKind::Renamed,
                "changes.d/old.md",
                100,
            )]
        );
    }

    #[test]
    fn jj_adapter_matches_reported_rename_path_and_contents_in_the_same_parent_entry() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"R\0changes.d/old.md\0changes.d/new.md\0".to_vec(),
                b"parent\n".to_vec(),
                b"\0changes.d/old.md".to_vec(),
                b"different\n".to_vec(),
                b"\0changes.d/new.md".to_vec(),
                b"same\n".to_vec(),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("rename paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].similarity, Some(0));
    }

    #[test]
    fn jj_adapter_reconstructs_renames_reported_as_add_delete_pairs() {
        let config = VcsConfig {
            preset: VcsPreset::Jj,
            ..VcsConfig::default()
        };
        let vcs = QueryVcs::new(
            Path::new("."),
            VcsPreset::Jj,
            &config,
            FakeRunner::successful([
                b"D\0changes.d/old.md\0A\0changes.d/new.md\0".to_vec(),
                b"parent\n".to_vec(),
                b"\0changes.d/new.md\0file".to_vec(),
                jj_debug_file_ids(&[(
                    "changes.d/new.md",
                    "1111111111111111111111111111111111111111",
                )]),
                b"\0changes.d/copied.md\0file\0changes.d/old.md\0file".to_vec(),
                jj_debug_file_ids(&[
                    (
                        "changes.d/copied.md",
                        "1111111111111111111111111111111111111111",
                    ),
                    (
                        "changes.d/old.md",
                        "1111111111111111111111111111111111111111",
                    ),
                ]),
            ]),
        )
        .with_fragment_directory(Path::new("changes.d"));

        let paths = vcs
            .changed_paths(&CommitId::new("commit"))
            .expect("rename paths");

        assert_eq!(
            paths,
            vec![
                ChangedPath::new("changes.d/old.md", ChangeKind::Deleted),
                ChangedPath::with_old_path_and_similarity(
                    "changes.d/new.md",
                    ChangeKind::Renamed,
                    "changes.d/old.md",
                    100,
                ),
            ]
        );
    }

    proptest! {
        #[test]
        fn modified_path_records_round_trip(
            path in "[A-Za-z0-9_-]{1,20}(/[A-Za-z0-9_-]{1,20}){0,4}"
        ) {
            let mut record = Vec::from("M\0".as_bytes());
            record.extend(path.as_bytes());
            record.push(0);

            prop_assert_eq!(
                parse_name_status_paths(&record),
                Ok(vec![ChangedPath::new(path, ChangeKind::Modified)])
            );
        }

        #[test]
        fn current_directory_prefixed_paths_are_normalized(
            path in "[A-Za-z0-9_-]{1,20}(/[A-Za-z0-9_-]{1,20}){0,4}"
        ) {
            let record = format!("M\0./{path}\0");

            prop_assert_eq!(
                parse_name_status_paths(record.as_bytes()),
                Ok(vec![ChangedPath::new(path, ChangeKind::Modified)])
            );
        }

        #[test]
        fn ordinary_statuses_reject_every_nonempty_suffix(
            status in prop_oneof![Just("A"), Just("D"), Just("M")],
            suffix in "[A-Za-z0-9_+.-]{1,20}",
        ) {
            let record = format!("{status}{suffix}\0changes.d/foo.md\0");

            prop_assert!(parse_name_status_paths(record.as_bytes()).is_err());
        }

        #[test]
        fn parent_traversing_changed_paths_are_rejected(
            prefix in "[A-Za-z0-9_-]{1,20}(/[A-Za-z0-9_-]{1,20}){0,4}"
        ) {
            let record = format!("M\0{prefix}/../outside.rs\0");

            prop_assert!(parse_name_status_paths(record.as_bytes()).is_err());
        }
    }

    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_smoke_test() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        let base = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        process(temp.path(), "jj", ["new"]);
        std::fs::create_dir_all(temp.path().join("src")).expect("src");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn feature() {}\n").expect("source");
        std::fs::write(
            temp.path().join("changes.d/feature.md"),
            " -  Added feature.\n",
        )
        .expect("fragment");
        process(temp.path(), "jj", ["describe", "-m", "Feature"]);
        let vcs = JjVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Jj,
                ..VcsConfig::default()
            },
        );

        let commits = vcs.commits(base.trim()).expect("commits");
        assert_eq!(commits.len(), 1);
        assert_eq!(vcs.message(&commits[0]).expect("message"), "Feature\n");
        let paths = vcs.changed_paths(&commits[0]).expect("paths");
        assert!(
            paths
                .iter()
                .any(|path| path.path == Path::new("src/lib.rs"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.path == Path::new("changes.d/feature.md"))
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_handles_symlinks_debug_like_names_and_nul_contents() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        std::fs::create_dir_all(temp.path().join("docs")).expect("docs");
        std::fs::write(
            temp.path()
                .join(r#"docs/Some(File { id: FileId("deadbeef")"#),
            "filename resembles debug metadata\n",
        )
        .expect("debug-like filename");
        std::fs::write(temp.path().join("docs/target.md"), " -  Symlink target.\n")
            .expect("symlink target");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        process(temp.path(), "jj", ["new"]);
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        symlink("../docs/target.md", temp.path().join("changes.d/link.md"))
            .expect("fragment symlink");
        std::fs::write(temp.path().join("changes.d/nul.md"), b"before\0after")
            .expect("NUL contents");
        process(temp.path(), "jj", ["describe", "-m", "Special fragments"]);
        let commit = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        let vcs = JjVcs::new(temp.path(), &VcsConfig::default());

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("special fragment paths");
        assert!(paths.contains(&ChangedPath::new("changes.d/link.md", ChangeKind::Added)));
        assert!(paths.contains(&ChangedPath::new("changes.d/nul.md", ChangeKind::Added)));
        assert_eq!(
            vcs.queries
                .jj_files_contents(
                    commit.trim(),
                    &[jj_fileset(Path::new("changes.d/nul.md")).expect("fileset")],
                )
                .expect("raw contents"),
            vec![(PathBuf::from("changes.d/nul.md"), b"before\0after".to_vec())]
        );
    }

    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_rejects_unchanged_fragment_copies_and_renames() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::create_dir_all(temp.path().join("docs")).expect("documentation directory");
        std::fs::write(temp.path().join("changes.d/original.md"), " -  Original.\n")
            .expect("original fragment");
        std::fs::write(
            temp.path().join("docs/template.md"),
            " -  External template.\n",
        )
        .expect("external template");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        process(temp.path(), "jj", ["new"]);
        std::fs::copy(
            temp.path().join("changes.d/original.md"),
            temp.path().join("changes.d/unchanged.md"),
        )
        .expect("unchanged copy");
        std::fs::copy(
            temp.path().join("changes.d/original.md"),
            temp.path().join("changes.d/edited.md"),
        )
        .expect("edited copy");
        std::fs::write(
            temp.path().join("changes.d/edited.md"),
            " -  Edited copy.\n",
        )
        .expect("edit copy");
        std::fs::copy(
            temp.path().join("docs/template.md"),
            temp.path().join("changes.d/external.md"),
        )
        .expect("external copy");
        process(temp.path(), "jj", ["describe", "-m", "Copy fragments"]);
        let copy_commit = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        let vcs = JjVcs::new(temp.path(), &VcsConfig::default());

        let copy_paths = vcs
            .changed_paths(&CommitId::new(copy_commit.trim()))
            .expect("copy paths");

        assert!(
            copy_paths.contains(&ChangedPath::with_old_path_and_similarity(
                "changes.d/unchanged.md",
                ChangeKind::Copied,
                "changes.d/original.md",
                100,
            ))
        );
        assert!(
            copy_paths.contains(&ChangedPath::with_old_path_and_similarity(
                "changes.d/external.md",
                ChangeKind::Copied,
                "docs/template.md",
                100,
            )),
            "{copy_paths:?}"
        );
        assert!(copy_paths.contains(&ChangedPath::new("changes.d/edited.md", ChangeKind::Added,)));

        process(temp.path(), "jj", ["new"]);
        std::fs::rename(
            temp.path().join("changes.d/original.md"),
            temp.path().join("changes.d/renamed.md"),
        )
        .expect("rename fragment");
        process(temp.path(), "jj", ["describe", "-m", "Rename fragment"]);
        let rename_commit = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );

        let rename_paths = vcs
            .changed_paths(&CommitId::new(rename_commit.trim()))
            .expect("rename paths");

        assert!(
            rename_paths.contains(&ChangedPath::with_old_path_and_similarity(
                "changes.d/renamed.md",
                ChangeKind::Renamed,
                "changes.d/original.md",
                100,
            )),
            "{rename_paths:?}"
        );
    }

    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_treats_fragment_directory_glob_characters_literally() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        let fragment_directory = Path::new("changes[1].d");
        std::fs::create_dir_all(temp.path().join(fragment_directory)).expect("fragments");
        std::fs::write(
            temp.path().join(fragment_directory).join("original.md"),
            " -  Original.\n",
        )
        .expect("original fragment");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        process(temp.path(), "jj", ["new"]);
        std::fs::copy(
            temp.path().join(fragment_directory).join("original.md"),
            temp.path().join(fragment_directory).join("copied.md"),
        )
        .expect("copy fragment");
        process(temp.path(), "jj", ["describe", "-m", "Copy fragment"]);
        let commit = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        let vcs =
            JjVcs::with_fragment_directory(temp.path(), &VcsConfig::default(), fragment_directory);

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("copy paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes[1].d/copied.md",
            ChangeKind::Copied,
            "changes[1].d/original.md",
            100,
        )));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_preserves_literal_backslashes_in_fragment_paths() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::write(
            temp.path().join(r"changes.d/original\name.md"),
            " -  Original.\n",
        )
        .expect("original fragment");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        process(temp.path(), "jj", ["new"]);
        std::fs::copy(
            temp.path().join(r"changes.d/original\name.md"),
            temp.path().join(r"changes.d/copied\name.md"),
        )
        .expect("copy fragment");
        process(temp.path(), "jj", ["describe", "-m", "Copy fragment"]);
        let commit = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        let vcs = JjVcs::new(temp.path(), &VcsConfig::default());

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("copy paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            r"changes.d/copied\name.md",
            ChangeKind::Copied,
            r"changes.d/original\name.md",
            100,
        )));
    }

    #[test]
    #[ignore = "requires the jj executable"]
    fn jj_vcs_handles_fragment_copies_in_merge_commits() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.name", "Test User"],
        );
        process(
            temp.path(),
            "jj",
            ["config", "set", "--repo", "user.email", "test@example.com"],
        );
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::write(temp.path().join("changes.d/original.md"), " -  Original.\n")
            .expect("original fragment");
        process(temp.path(), "jj", ["describe", "-m", "Base"]);
        let base = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );

        process(temp.path(), "jj", ["new", base.trim()]);
        std::fs::write(temp.path().join("left.txt"), "left\n").expect("left");
        process(temp.path(), "jj", ["describe", "-m", "Left"]);
        let left = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );

        process(temp.path(), "jj", ["new", base.trim()]);
        std::fs::write(temp.path().join("right.txt"), "right\n").expect("right");
        process(temp.path(), "jj", ["describe", "-m", "Right"]);
        let right = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );

        process(temp.path(), "jj", ["new", left.trim(), right.trim()]);
        std::fs::copy(
            temp.path().join("changes.d/original.md"),
            temp.path().join("changes.d/copied.md"),
        )
        .expect("copy fragment");
        process(temp.path(), "jj", ["describe", "-m", "Merge"]);
        let merge = process_output(
            temp.path(),
            "jj",
            ["log", "--no-graph", "-r", "@", "-T", "commit_id"],
        );
        let vcs = JjVcs::new(temp.path(), &VcsConfig::default());

        let paths = vcs
            .changed_paths(&CommitId::new(merge.trim()))
            .expect("merge paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes.d/copied.md",
            ChangeKind::Copied,
            "changes.d/original.md",
            100,
        )));
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_smoke_test() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        hg_commit(temp.path(), "Base");
        let base = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        std::fs::create_dir_all(temp.path().join("src")).expect("src");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn feature() {}\n").expect("source");
        std::fs::write(
            temp.path().join("changes.d/feature.md"),
            " -  Added feature.\n",
        )
        .expect("fragment");
        hg_commit(temp.path(), "Feature");
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        let commits = vcs.commits(base.trim()).expect("commits");
        assert_eq!(commits.len(), 1);
        assert_eq!(vcs.message(&commits[0]).expect("message"), "Feature");
        let paths = vcs.changed_paths(&commits[0]).expect("paths");
        assert!(
            paths
                .iter()
                .any(|path| path.path == Path::new("src/lib.rs"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.path == Path::new("changes.d/feature.md"))
        );
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_accepts_a_base_branch_with_revset_syntax() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        hg_commit(temp.path(), "Base");
        let branch = r#"foo "bar" \ branch()"#;
        process(temp.path(), "hg", ["branch", branch]);
        std::fs::write(temp.path().join("README.md"), "changed\n").expect("readme change");
        hg_commit(temp.path(), "Feature");
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        assert_eq!(vcs.commits(branch).expect("commits").len(), 0);
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_distinguishes_edited_and_unchanged_fragment_copies() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        std::fs::write(temp.path().join("changes.d/original.md"), " -  Original.\n")
            .expect("original fragment");
        hg_commit(temp.path(), "Base");
        process(
            temp.path(),
            "hg",
            ["copy", "changes.d/original.md", "changes.d/unchanged.md"],
        );
        process(
            temp.path(),
            "hg",
            ["copy", "changes.d/original.md", "changes.d/edited.md"],
        );
        std::fs::write(
            temp.path().join("changes.d/edited.md"),
            " -  Edited copy.\n",
        )
        .expect("edited fragment");
        hg_commit(temp.path(), "Copy fragments");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("copy paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes.d/unchanged.md",
            ChangeKind::Copied,
            "changes.d/original.md",
            100,
        )));
        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes.d/edited.md",
            ChangeKind::Copied,
            "changes.d/original.md",
            0,
        )));
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_probes_fragments_in_nested_section_directories() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::create_dir_all(temp.path().join("changes.d/nested/core"))
            .expect("fragment section");
        std::fs::write(
            temp.path().join("changes.d/nested/core/original.md"),
            " -  Original.\n",
        )
        .expect("original fragment");
        hg_commit(temp.path(), "Base");
        process(
            temp.path(),
            "hg",
            [
                "copy",
                "changes.d/nested/core/original.md",
                "changes.d/nested/core/edited.md",
            ],
        );
        std::fs::write(
            temp.path().join("changes.d/nested/core/edited.md"),
            " -  Edited copy.\n",
        )
        .expect("edited fragment");
        hg_commit(temp.path(), "Copy fragment");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::with_fragment_layout(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
            "changes.d",
            ["nested/core"],
        );

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("copy paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "changes.d/nested/core/edited.md",
            ChangeKind::Copied,
            "changes.d/nested/core/original.md",
            0,
        )));
    }

    // APFS rejects the deliberately non-UTF-8 path before Mercurial can inspect it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_probes_non_utf8_fragment_paths() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        let old_path = PathBuf::from(OsString::from_vec(b"changes.d/original-\xff.md".to_vec()));
        let new_path = PathBuf::from(OsString::from_vec(b"changes.d/edited-\xfe.md".to_vec()));
        std::fs::write(temp.path().join(&old_path), " -  Original.\n").expect("original fragment");
        hg_commit(temp.path(), "Base");
        let copy = ProcessCommand::new("hg")
            .current_dir(temp.path())
            .arg("copy")
            .arg(&old_path)
            .arg(&new_path)
            .output()
            .expect("hg copy");
        assert!(
            copy.status.success(),
            "hg copy failed: {}",
            String::from_utf8_lossy(&copy.stderr)
        );
        std::fs::write(temp.path().join(&new_path), " -  Edited copy.\n").expect("edited fragment");
        hg_commit(temp.path(), "Copy fragment");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("copy paths");
        let copied = paths
            .iter()
            .find(|path| path.path.as_os_str().as_bytes() == new_path.as_os_str().as_bytes())
            .expect("non-UTF-8 copied path");

        assert_eq!(copied.similarity, Some(0));
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_reports_deletions_and_renames() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::write(temp.path().join("deleted.txt"), "deleted\n").expect("deleted file");
        std::fs::write(temp.path().join("old.txt"), "renamed\n").expect("renamed file");
        hg_commit(temp.path(), "Base");
        process(temp.path(), "hg", ["remove", "deleted.txt"]);
        process(temp.path(), "hg", ["rename", "old.txt", "new.txt"]);
        hg_commit(temp.path(), "Remove and rename");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("changed paths");

        assert!(paths.contains(&ChangedPath::new("deleted.txt", ChangeKind::Deleted)));
        assert!(paths.contains(&ChangedPath::new("old.txt", ChangeKind::Deleted)));
        assert!(paths.contains(&ChangedPath::with_old_path(
            "new.txt",
            ChangeKind::Renamed,
            "old.txt",
        )));
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_compares_root_commit_with_null_revision() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::write(temp.path().join("initial.txt"), "initial\n").expect("initial file");
        hg_commit(temp.path(), "Initial");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        let paths = vcs
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("root paths");

        assert_eq!(
            paths,
            vec![ChangedPath::new("initial.txt", ChangeKind::Added)]
        );
    }

    #[test]
    #[ignore = "requires the hg executable"]
    fn hg_vcs_ignores_clean_merge_inputs() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        process(temp.path(), "hg", ["init"]);
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        hg_commit(temp.path(), "Base");
        process(temp.path(), "hg", ["branch", "feature"]);
        std::fs::write(temp.path().join("feature.txt"), "feature\n").expect("feature");
        hg_commit(temp.path(), "Feature");
        process(temp.path(), "hg", ["update", "default"]);
        std::fs::write(temp.path().join("main.txt"), "main\n").expect("main");
        hg_commit(temp.path(), "Main");
        process(temp.path(), "hg", ["merge", "feature"]);
        hg_commit(temp.path(), "Merge feature");
        let commit = process_output(temp.path(), "hg", ["log", "-r", ".", "-T", "{node}"]);
        let vcs = HgVcs::new(
            temp.path(),
            &VcsConfig {
                preset: VcsPreset::Hg,
                ..VcsConfig::default()
            },
        );

        assert!(
            vcs.changed_paths(&CommitId::new(commit.trim()))
                .expect("merge paths")
                .is_empty()
        );
    }

    fn process<const N: usize>(dir: &Path, program: &str, args: [&str; N]) {
        let output = ProcessCommand::new(program)
            .current_dir(dir)
            .args(args)
            .output()
            .expect("process");
        assert!(
            output.status.success(),
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn process_output<const N: usize>(dir: &Path, program: &str, args: [&str; N]) -> String {
        let output = ProcessCommand::new(program)
            .current_dir(dir)
            .args(args)
            .output()
            .expect("process");
        assert!(
            output.status.success(),
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 output")
    }

    fn hg_commit(dir: &Path, message: &str) {
        process(
            dir,
            "hg",
            [
                "--config",
                "ui.username=Test User <test@example.com>",
                "commit",
                "-Am",
                message,
            ],
        );
    }

    #[test]
    fn git_vcs_reports_only_staged_paths_with_similarity_metadata() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(temp.path().join("src/old.rs"), "pub fn old() {}\n").expect("old");
        std::fs::write(temp.path().join("src/copy.rs"), "pub fn copy() {}\n").expect("copy");
        std::fs::write(temp.path().join("src/delete.rs"), "pub fn delete() {}\n").expect("delete");
        git(temp.path(), ["add", "src"]);
        git(temp.path(), ["commit", "-m", "Initial source"]);

        git(temp.path(), ["mv", "src/old.rs", "src/renamed.rs"]);
        std::fs::copy(
            temp.path().join("src/copy.rs"),
            temp.path().join("src/copied.rs"),
        )
        .expect("copy source");
        git(temp.path(), ["add", "src/copied.rs"]);
        git(temp.path(), ["rm", "src/delete.rs"]);
        std::fs::write(
            temp.path().join("src/unstaged.rs"),
            "pub fn unstaged() {}\n",
        )
        .expect("unstaged");
        let paths = GitVcs::new(temp.path())
            .staged_paths()
            .expect("staged paths");

        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "src/copied.rs",
            ChangeKind::Copied,
            "src/copy.rs",
            100
        )));
        assert!(paths.contains(&ChangedPath::with_old_path_and_similarity(
            "src/renamed.rs",
            ChangeKind::Renamed,
            "src/old.rs",
            100
        )));
        assert!(paths.contains(&ChangedPath::new("src/delete.rs", ChangeKind::Deleted)));
        assert!(
            !paths
                .iter()
                .any(|path| path.path == Path::new("src/unstaged.rs"))
        );
    }

    #[test]
    fn git_vcs_reports_staged_paths_in_an_unborn_repository() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn initial() {}\n").expect("source");
        git(temp.path(), ["add", "src/lib.rs"]);

        let paths = GitVcs::new(temp.path())
            .staged_paths()
            .expect("staged paths");

        assert_eq!(
            paths,
            vec![ChangedPath::new("src/lib.rs", ChangeKind::Added)]
        );
    }

    #[test]
    fn git_vcs_staged_paths_ignore_changes_unchanged_from_a_merge_parent() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        git(temp.path(), ["add", "README.md"]);
        git(temp.path(), ["commit", "-m", "Initial"]);
        let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);
        git(temp.path(), ["switch", "-c", "feature"]);
        std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("source");
        std::fs::write(
            temp.path().join("changes.d/value.md"),
            " -  Added value API.\n",
        )
        .expect("fragment");
        git(temp.path(), ["add", "src/lib.rs", "changes.d/value.md"]);
        git(temp.path(), ["commit", "-m", "Add value API"]);
        git(temp.path(), ["switch", initial_branch.trim()]);
        std::fs::write(temp.path().join("README.md"), "main\n").expect("main readme");
        git(temp.path(), ["commit", "-am", "Change main"]);
        git(temp.path(), ["merge", "--no-commit", "--no-ff", "feature"]);

        let paths = GitVcs::new(temp.path())
            .staged_paths()
            .expect("staged merge paths");

        assert!(paths.is_empty(), "{paths:?}");
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

    #[cfg(unix)]
    #[test]
    fn git_vcs_reports_file_type_changes() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir(temp.path().join("src")).expect("src directory");
        std::fs::write(temp.path().join("src/item"), "regular file\n").expect("regular file");
        git(temp.path(), ["add", "src/item"]);
        git(temp.path(), ["commit", "-m", "Add regular file"]);

        std::fs::remove_file(temp.path().join("src/item")).expect("remove regular file");
        symlink("target", temp.path().join("src/item")).expect("symlink");
        git(temp.path(), ["add", "src/item"]);
        git(temp.path(), ["commit", "-m", "Change file type"]);
        let commit = git_output(temp.path(), ["rev-parse", "HEAD"]);

        assert_eq!(
            GitVcs::new(temp.path())
                .changed_paths(&CommitId::new(commit.trim()))
                .expect("type-changed paths"),
            vec![ChangedPath::new("src/item", ChangeKind::Other)]
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn git_vcs_preserves_non_utf8_filename_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::write(temp.path().join("README.md"), "initial\n").expect("readme");
        git(temp.path(), ["add", "README.md"]);
        git(temp.path(), ["commit", "-m", "Initial"]);
        let filename = std::ffi::OsString::from_vec(b"unrelated-\xff".to_vec());
        std::fs::write(temp.path().join(&filename), "unrelated\n").expect("byte filename");
        git(temp.path(), ["add", "-A"]);
        git(temp.path(), ["commit", "-m", "Add byte filename"]);
        let commit = git_output(temp.path(), ["rev-parse", "HEAD"]);

        let paths = GitVcs::new(temp.path())
            .changed_paths(&CommitId::new(commit.trim()))
            .expect("changed paths");

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].path.as_os_str().as_bytes(), b"unrelated-\xff");
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
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        std::fs::write(
            temp.path().join("changes.d/value.md"),
            " -  Changed the value API.\n",
        )
        .expect("feature fragment");
        git(temp.path(), ["add", "src/lib.rs", "changes.d/value.md"]);
        git(temp.path(), ["commit", "-m", "Change feature"]);
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
        let staged = GitVcs::new(temp.path())
            .staged_paths()
            .expect("staged merge paths");
        assert!(staged.contains(&ChangedPath::new("src/lib.rs", ChangeKind::Modified)));
        assert!(
            !staged
                .iter()
                .any(|path| path.path == Path::new("changes.d/value.md"))
        );
        git(temp.path(), ["commit", "-m", "Merge feature"]);
        let merge_commit = git_output(temp.path(), ["rev-parse", "HEAD"]);
        let vcs = GitVcs::new(temp.path());

        let paths = vcs
            .changed_paths(&CommitId::new(merge_commit.trim()))
            .expect("changed paths");

        assert!(paths.contains(&ChangedPath::new("src/lib.rs", ChangeKind::Modified)));
    }

    #[test]
    fn git_vcs_handles_renames_changed_against_every_merge_parent() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragment dir");
        std::fs::write(temp.path().join("changes.d/original.md"), " -  Original.\n")
            .expect("fragment");
        git(temp.path(), ["add", "changes.d/original.md"]);
        git(temp.path(), ["commit", "-m", "Initial fragment"]);
        let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);

        git(temp.path(), ["switch", "-c", "feature"]);
        git(
            temp.path(),
            ["mv", "changes.d/original.md", "changes.d/feature.md"],
        );
        git(temp.path(), ["commit", "-m", "Rename on feature"]);

        git(temp.path(), ["switch", initial_branch.trim()]);
        git(
            temp.path(),
            ["mv", "changes.d/original.md", "changes.d/main.md"],
        );
        git(temp.path(), ["commit", "-m", "Rename on main"]);
        git_expect_failure(temp.path(), ["merge", "feature"]);
        std::fs::remove_file(temp.path().join("changes.d/feature.md")).ok();
        std::fs::remove_file(temp.path().join("changes.d/main.md")).ok();
        std::fs::write(temp.path().join("changes.d/resolved.md"), " -  Original.\n")
            .expect("resolved fragment");
        git(temp.path(), ["add", "-A"]);
        git(temp.path(), ["commit", "-m", "Resolve rename"]);
        let merge = git_output(temp.path(), ["rev-parse", "HEAD"]);

        let paths = GitVcs::new(temp.path())
            .changed_paths(&CommitId::new(merge.trim()))
            .expect("merge paths");

        assert!(
            paths.iter().any(|path| {
                path.path == Path::new("changes.d/resolved.md")
                    && path.kind == ChangeKind::Renamed
                    && path.similarity == Some(100)
            }),
            "{paths:?}"
        );
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
        if args.as_slice() == ["init"] {
            git(dir, ["config", "commit.gpgSign", "false"]);
            git(dir, ["config", "tag.gpgSign", "false"]);
        }
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
