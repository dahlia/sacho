//! Changelog merge driver core.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

use crate::changelog::{ChangelogError, find_unreleased_region, version_heading_spans};
use crate::commands::{CompileOptions, compile_unreleased};
use crate::error::{Error, Result};
use crate::repo::{Repository, mutation_lock_path};

/// Options for running Sacho's changelog merge driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDriverOptions {
    /// Common ancestor file passed by the VCS merge driver.
    pub ancestor: PathBuf,

    /// Current-side file passed by the VCS merge driver.
    pub current: PathBuf,

    /// Other-side file passed by the VCS merge driver.
    pub other: PathBuf,

    /// Repository path being merged.
    pub path: PathBuf,
}

/// Result of merging a materialized changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDriverResult {
    /// The changelog was resolved without conflict.
    Clean {
        /// Changelog contents that should be written to the current-side file.
        output: String,

        /// Human-facing hints to print on standard error.
        hints: Vec<MergeHint>,
    },

    /// The released sections diverged and need human resolution.
    Conflict {
        /// Changelog contents with conflict markers.
        output_with_markers: String,

        /// Machine-readable conflict reason.
        reason: MergeConflictReason,
    },
}

/// Hint produced by a clean merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeHint {
    /// A released section was inserted from the other side of the merge.
    InsertedOtherRelease {
        /// Version heading text that was inserted.
        heading: String,
    },
}

impl std::fmt::Display for MergeHint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsertedOtherRelease { heading } => {
                let version = heading.strip_prefix("Version ").unwrap_or(heading);
                write!(
                    formatter,
                    "Inserted {heading} from the other side. Run `sacho carry {version}` if this branch should list those entries in its next release too."
                )
            }
        }
    }
}

/// Reason a merge driver result contains conflict markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeConflictReason {
    /// Both sides contain the same released version heading with different
    /// block bytes.
    DivergentReleasedSection {
        /// Version heading whose released block differed.
        heading: String,
    },

    /// Both sides edited the text before the generated unreleased region.
    DivergentPrefix,

    /// Both sides edited the text after the generated unreleased region and
    /// before the released sections.
    DivergentAfterUnreleased,
}

/// Merges a materialized changelog by recompiling unreleased fragments and
/// unioning frozen released sections.
pub fn merge_driver(repo: &Repository, options: MergeDriverOptions) -> Result<MergeDriverResult> {
    let ancestor = read_input(&options.ancestor)?;
    let current = read_input(&options.current)?;
    let other = read_input(&options.other)?;
    let compiled = compile_unreleased_for_merge(repo)?;
    let ancestor_parsed = parse_changelog(repo, &ancestor, options.ancestor.clone())?;
    let current_parsed = parse_changelog(repo, &current, options.current.clone())?;
    let other_parsed = parse_changelog(repo, &other, options.other.clone())?;

    merge_parsed_changelogs(
        ancestor_parsed,
        current_parsed,
        other_parsed,
        &compiled.markdown,
    )
}

fn read_input(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.clone(),
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedChangelog {
    prefix: String,
    after_unreleased: String,
    released: Vec<ReleasedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleasedBlock {
    heading: String,
    version: String,
    body: String,
}

fn parse_changelog(repo: &Repository, source: &str, path: PathBuf) -> Result<ParsedChangelog> {
    let config = repo.config();
    let unreleased = find_unreleased_region(
        source,
        config.changelog.region_detection,
        &config.changelog.unreleased_heading,
    )
    .map_err(|source| changelog_error(path, source))?;
    let prefix = source[..unreleased.start].to_owned();
    let tail = &source[unreleased.end..];
    let first_released = first_released_heading_start(tail).unwrap_or(tail.len());
    let after_unreleased = tail[..first_released].to_owned();
    let released = parse_released_blocks(&tail[first_released..]);

    Ok(ParsedChangelog {
        prefix,
        after_unreleased,
        released,
    })
}

fn merge_parsed_changelogs(
    ancestor: ParsedChangelog,
    current: ParsedChangelog,
    other: ParsedChangelog,
    compiled_unreleased: &str,
) -> Result<MergeDriverResult> {
    let prefix = merge_text_region(&ancestor.prefix, &current.prefix, &other.prefix);
    let after_unreleased = merge_text_region(
        &ancestor.after_unreleased,
        &current.after_unreleased,
        &other.after_unreleased,
    );
    let ancestor_blocks = ancestor
        .released
        .into_iter()
        .map(|block| (block.heading.clone(), block))
        .collect::<HashMap<_, _>>();
    let mut by_heading = HashMap::<String, ReleasedBlock>::new();
    let mut current_order = Vec::<String>::new();
    for block in current.released {
        current_order.push(block.heading.clone());
        by_heading.insert(block.heading.clone(), block);
    }

    let mut other_only_order = Vec::<String>::new();
    let mut hints = Vec::new();
    let mut conflict = None;
    for block in other.released {
        match by_heading.get(&block.heading) {
            Some(current_block) if current_block.body == block.body => {}
            Some(current_block) => {
                match merge_divergent_block(current_block, &block, &ancestor_blocks) {
                    DivergentBlockMerge::TakeOther => {
                        by_heading.insert(block.heading.clone(), block);
                    }
                    DivergentBlockMerge::KeepCurrent => {}
                    DivergentBlockMerge::Conflict if conflict.is_none() => {
                        conflict = Some((current_block.clone(), block));
                    }
                    DivergentBlockMerge::Conflict => {}
                }
            }
            None => {
                hints.push(MergeHint::InsertedOtherRelease {
                    heading: block.heading.clone(),
                });
                other_only_order.push(block.heading.clone());
                by_heading.insert(block.heading.clone(), block);
            }
        }
    }

    let ordered = order_released_blocks(&by_heading, &current_order, &other_only_order);
    let prefix = match prefix {
        MergedTextRegion::Clean(prefix) => prefix,
        MergedTextRegion::Conflict {
            current: current_prefix,
            other: other_prefix,
        } => {
            return Ok(MergeDriverResult::Conflict {
                output_with_markers: reconstruct_with_prefix_conflict(
                    &current_prefix,
                    &other_prefix,
                    compiled_unreleased,
                    merged_or_current_text(after_unreleased, &current.after_unreleased),
                    ordered.iter().copied(),
                ),
                reason: MergeConflictReason::DivergentPrefix,
            });
        }
    };
    let after_unreleased = match after_unreleased {
        MergedTextRegion::Clean(after_unreleased) => after_unreleased,
        MergedTextRegion::Conflict {
            current: current_after,
            other: other_after,
        } => {
            return Ok(MergeDriverResult::Conflict {
                output_with_markers: reconstruct_with_after_unreleased_conflict(
                    prefix,
                    compiled_unreleased,
                    &current_after,
                    &other_after,
                    ordered.iter().copied(),
                ),
                reason: MergeConflictReason::DivergentAfterUnreleased,
            });
        }
    };
    if let Some((current_block, other_block)) = conflict {
        let output_with_markers = reconstruct_with_conflict(
            prefix,
            compiled_unreleased,
            after_unreleased,
            &current_block,
            &other_block,
            ordered.iter().copied(),
        );
        return Ok(MergeDriverResult::Conflict {
            output_with_markers,
            reason: MergeConflictReason::DivergentReleasedSection {
                heading: other_block.heading,
            },
        });
    }

    Ok(MergeDriverResult::Clean {
        output: reconstruct(
            prefix,
            compiled_unreleased,
            after_unreleased,
            ordered.iter().copied(),
        ),
        hints,
    })
}

enum MergedTextRegion<'a> {
    Clean(&'a str),
    Conflict { current: String, other: String },
}

fn merge_text_region<'a>(
    ancestor: &'a str,
    current: &'a str,
    other: &'a str,
) -> MergedTextRegion<'a> {
    if current == other || other == ancestor {
        MergedTextRegion::Clean(current)
    } else if current == ancestor {
        MergedTextRegion::Clean(other)
    } else {
        MergedTextRegion::Conflict {
            current: current.to_owned(),
            other: other.to_owned(),
        }
    }
}

fn merged_or_current_text<'a>(merged: MergedTextRegion<'a>, current: &'a str) -> &'a str {
    match merged {
        MergedTextRegion::Clean(text) => text,
        MergedTextRegion::Conflict { .. } => current,
    }
}

enum DivergentBlockMerge {
    TakeOther,
    KeepCurrent,
    Conflict,
}

fn merge_divergent_block(
    current: &ReleasedBlock,
    other: &ReleasedBlock,
    ancestor_blocks: &HashMap<String, ReleasedBlock>,
) -> DivergentBlockMerge {
    let Some(ancestor) = ancestor_blocks.get(&current.heading) else {
        return DivergentBlockMerge::Conflict;
    };
    match (current.body == ancestor.body, other.body == ancestor.body) {
        (true, false) => DivergentBlockMerge::TakeOther,
        (false, true) => DivergentBlockMerge::KeepCurrent,
        (false, false) => DivergentBlockMerge::Conflict,
        (true, true) => DivergentBlockMerge::KeepCurrent,
    }
}

fn reconstruct_with_conflict<'a>(
    prefix: &str,
    compiled_unreleased: &str,
    after_unreleased: &str,
    current_block: &ReleasedBlock,
    other_block: &ReleasedBlock,
    blocks: impl Iterator<Item = &'a ReleasedBlock>,
) -> String {
    let conflict = conflict_block(current_block, other_block);
    let mut replaced = HashSet::new();

    let mut output = start_output(prefix, compiled_unreleased, after_unreleased);
    for block in blocks {
        append_block_separator(&mut output);
        if block.heading == current_block.heading && replaced.insert(block.heading.clone()) {
            output.push_str(conflict.trim_end());
            output.push('\n');
        } else {
            output.push_str(block.body.trim_end());
            output.push('\n');
        }
    }
    output
}

fn reconstruct_with_prefix_conflict<'a>(
    current_prefix: &str,
    other_prefix: &str,
    compiled_unreleased: &str,
    after_unreleased: &str,
    blocks: impl Iterator<Item = &'a ReleasedBlock>,
) -> String {
    let mut output = conflict_text(current_prefix, other_prefix);
    output.push_str(compiled_unreleased.trim_end());
    output.push('\n');
    output.push_str(after_unreleased);
    for block in blocks {
        append_block_separator(&mut output);
        output.push_str(block.body.trim_end());
        output.push('\n');
    }
    output
}

fn reconstruct_with_after_unreleased_conflict<'a>(
    prefix: &str,
    compiled_unreleased: &str,
    current_after: &str,
    other_after: &str,
    blocks: impl Iterator<Item = &'a ReleasedBlock>,
) -> String {
    let mut output = String::new();
    output.push_str(prefix);
    output.push_str(compiled_unreleased.trim_end());
    output.push('\n');
    output.push_str(&conflict_text(current_after, other_after));
    for block in blocks {
        append_block_separator(&mut output);
        output.push_str(block.body.trim_end());
        output.push('\n');
    }
    output
}

fn conflict_block(current: &ReleasedBlock, other: &ReleasedBlock) -> String {
    conflict_text(&current.body, &other.body)
}

fn conflict_text(current: &str, other: &str) -> String {
    format!(
        "<<<<<<< current\n{}=======\n{}>>>>>>> other\n",
        ensure_trailing_newline(current),
        ensure_trailing_newline(other)
    )
}

fn order_released_blocks<'a>(
    by_heading: &'a HashMap<String, ReleasedBlock>,
    current_order: &[String],
    other_only_order: &[String],
) -> Vec<&'a ReleasedBlock> {
    let mut output = current_order
        .iter()
        .filter_map(|heading| by_heading.get(heading))
        .collect::<Vec<_>>();

    let mut semver_incoming = other_only_order
        .iter()
        .filter_map(|heading| by_heading.get(heading))
        .filter(|block| block.semver().is_some())
        .collect::<Vec<_>>();
    semver_incoming.sort_by(|left, right| {
        right
            .semver()
            .expect("filtered to semver")
            .cmp(&left.semver().expect("filtered to semver"))
    });

    let mut seen = output
        .iter()
        .map(|block| block.heading.as_str())
        .collect::<HashSet<_>>();
    for block in semver_incoming {
        if seen.contains(block.heading.as_str()) {
            continue;
        }
        let incoming = block.semver().expect("filtered to semver");
        let insertion = output
            .iter()
            .position(|existing| match existing.semver() {
                Some(existing) => incoming.cmp(&existing).is_gt(),
                None => true,
            })
            .unwrap_or(output.len());
        output.insert(insertion, block);
        seen.insert(block.heading.as_str());
    }
    for block in other_only_order
        .iter()
        .filter_map(|heading| by_heading.get(heading))
        .filter(|block| block.semver().is_none())
    {
        if seen.insert(block.heading.as_str()) {
            output.push(block);
        }
    }
    output
}

fn reconstruct<'a>(
    prefix: &str,
    compiled_unreleased: &str,
    after_unreleased: &str,
    blocks: impl Iterator<Item = &'a ReleasedBlock>,
) -> String {
    let mut output = start_output(prefix, compiled_unreleased, after_unreleased);
    for block in blocks {
        append_block_separator(&mut output);
        output.push_str(block.body.trim_end());
        output.push('\n');
    }
    output
}

fn start_output(prefix: &str, compiled_unreleased: &str, after_unreleased: &str) -> String {
    let mut output = String::new();
    output.push_str(prefix);
    output.push_str(compiled_unreleased.trim_end());
    output.push('\n');
    output.push_str(after_unreleased);
    output
}

fn append_block_separator(output: &mut String) {
    if !output.ends_with("\n\n") {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }
}

impl ReleasedBlock {
    fn semver(&self) -> Option<Version> {
        Version::parse(&self.version).ok()
    }
}

fn parse_released_blocks(source: &str) -> Vec<ReleasedBlock> {
    let candidates = version_heading_spans(source);
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let end = candidates
                .get(index + 1)
                .map(|next| next.start)
                .unwrap_or(source.len());
            version_from_heading(&candidate.text).map(|version| ReleasedBlock {
                heading: candidate.text.clone(),
                version,
                body: source[candidate.start..end].to_owned(),
            })
        })
        .collect()
}

fn first_released_heading_start(source: &str) -> Option<usize> {
    version_heading_spans(source)
        .into_iter()
        .find(|candidate| version_from_heading(&candidate.text).is_some())
        .map(|candidate| candidate.start)
}

fn version_from_heading(text: &str) -> Option<String> {
    text.strip_prefix("Version ").map(ToOwned::to_owned)
}

fn compile_unreleased_for_merge(repo: &Repository) -> Result<crate::compile::CompiledRegion> {
    if repo.config().vcs.preset == crate::config::VcsPreset::Git
        && let Some(other_head) = git_other_head(repo)?
    {
        let temp = merged_fragment_temp_root(repo, &other_head)?;
        let temp_repo = temp.repository()?;
        return compile_unreleased(&temp_repo, CompileOptions::default());
    }

    compile_unreleased(repo, CompileOptions::default())
}

struct TempRoot {
    path: PathBuf,
    mutation_lock_path: PathBuf,
}

impl TempRoot {
    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn repository(&self) -> Result<Repository> {
        Repository::from_root_with_mutation_lock(self.path(), &self.mutation_lock_path)
    }
}

#[cfg_attr(test, mutants::skip)]
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn merged_fragment_temp_root(repo: &Repository, other_head: &str) -> Result<TempRoot> {
    let path = temp_root_path();
    fs::create_dir_all(&path).map_err(|source| Error::CreateDirectory {
        path: path.clone(),
        source,
    })?;
    let source_lock_path = mutation_lock_path(repo.root());
    let mutation_lock_path = source_lock_path
        .strip_prefix(repo.root())
        .map(|relative| path.join(relative))
        .unwrap_or(source_lock_path);
    let temp = TempRoot {
        path,
        mutation_lock_path,
    };
    let config_path = temp.path().join(Repository::CONFIG_FILE);
    let snapshot_config = serializable_snapshot_config(repo);
    let config =
        toml::to_string(&snapshot_config).map_err(|source| Error::SerializeConfig { source })?;
    fs::write(&config_path, config).map_err(|source| Error::WriteFile {
        path: config_path,
        source,
    })?;
    let fragment_dir = repo.config().fragments.directory.clone();
    copy_directory(repo.resolve(&fragment_dir), temp.path().join(&fragment_dir))?;
    overlay_other_fragment_changes(repo, temp.path(), other_head)?;
    let snapshot_fragment_dir = &snapshot_config.fragments.directory;
    if snapshot_fragment_dir != &fragment_dir {
        copy_directory(
            temp.path().join(&fragment_dir),
            temp.path().join(snapshot_fragment_dir),
        )?;
    }
    materialize_snapshot_fallback_paths(repo, &snapshot_config, temp.path())?;
    Ok(temp)
}

fn materialize_snapshot_fallback_paths(
    repo: &Repository,
    snapshot_config: &crate::config::Config,
    temp_root: &std::path::Path,
) -> Result<()> {
    let operational_fragment_root = temp_root.join(&repo.config().fragments.directory);
    let snapshot_fragment_root = temp_root.join(&snapshot_config.fragments.directory);

    let operational_next = operational_fragment_root.join(&repo.config().fragments.next_file);
    let snapshot_next = snapshot_fragment_root.join(&snapshot_config.fragments.next_file);
    if operational_next != snapshot_next && operational_next.is_file() {
        copy_file(operational_next, snapshot_next)?;
    }

    for (operational, snapshot) in repo.config().sections.iter().zip(&snapshot_config.sections) {
        let operational_directory = operational_fragment_root.join(&operational.directory);
        let snapshot_directory = snapshot_fragment_root.join(&snapshot.directory);
        if operational_directory != snapshot_directory {
            copy_directory(operational_directory, snapshot_directory)?;
        }
    }
    Ok(())
}

fn serializable_snapshot_config(repo: &Repository) -> crate::config::Config {
    let mut config = repo.config().clone();
    let source = repo.source_config();
    preserve_serializable_path(&mut config.changelog.path, &source.changelog.path);
    preserve_serializable_path(&mut config.fragments.directory, &source.fragments.directory);
    preserve_serializable_path(&mut config.fragments.next_file, &source.fragments.next_file);
    for (section, source_section) in config.sections.iter_mut().zip(&source.sections) {
        preserve_serializable_path(&mut section.directory, &source_section.directory);
    }
    config
}

fn preserve_serializable_path(path: &mut PathBuf, source: &std::path::Path) {
    if path.to_str().is_none() {
        *path = source.to_path_buf();
    }
}

fn temp_root_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("sacho-merge-{}-{nanos}", std::process::id()))
}

fn copy_directory(from: PathBuf, to: PathBuf) -> Result<()> {
    match fs::read_dir(&from) {
        Ok(entries) => {
            fs::create_dir_all(&to).map_err(|source| Error::CreateDirectory {
                path: to.clone(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::ReadFile {
                    path: from.clone(),
                    source,
                })?;
                let source_path = entry.path();
                let target_path = to.join(entry.file_name());
                let file_type = entry.file_type().map_err(|source| Error::ReadFile {
                    path: source_path.clone(),
                    source,
                })?;
                if file_type.is_dir() {
                    copy_directory(source_path, target_path)?;
                } else if file_type.is_file() {
                    copy_file(source_path, target_path)?;
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::ReadFile { path: from, source }),
    }
}

fn copy_file(from: PathBuf, to: PathBuf) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::copy(&from, &to)
        .map(|_| ())
        .map_err(|source| Error::ReadFile { path: from, source })
}

fn overlay_other_fragment_changes(
    repo: &Repository,
    temp_root: &std::path::Path,
    other_head: &str,
) -> Result<()> {
    let base = git_output(repo, ["merge-base", "HEAD", other_head])?;
    let base = base.trim();
    if base.is_empty() {
        return Ok(());
    }
    let fragment_dir = &repo.config().fragments.directory;
    let current_changed = git_changed_fragment_paths(repo, base, "HEAD", fragment_dir)?;
    let copied =
        git_changed_fragment_paths_with_filter(repo, base, other_head, "ACMR", fragment_dir)?;
    for path in copied {
        if !is_fragment_markdown_path(&path) {
            continue;
        }
        if current_changed.contains(&path) {
            merge_shared_fragment(repo, temp_root, base, other_head, &path)?;
        } else {
            write_temp_fragment(temp_root, &path, git_blob(repo, other_head, &path)?)?;
        }
    }

    let deleted =
        git_changed_fragment_paths_with_filter(repo, base, other_head, "D", fragment_dir)?;
    for path in deleted {
        if is_fragment_markdown_path(&path) && !current_changed.contains(&path) {
            let _ = fs::remove_file(temp_root.join(path));
        }
    }
    Ok(())
}

fn git_changed_fragment_paths(
    repo: &Repository,
    base: &str,
    revision: &str,
    fragment_dir: &std::path::Path,
) -> Result<HashSet<PathBuf>> {
    Ok(
        git_changed_fragment_paths_with_filter(repo, base, revision, "ACMR", fragment_dir)?
            .into_iter()
            .filter(|path| is_fragment_markdown_path(path))
            .collect(),
    )
}

fn git_changed_fragment_paths_with_filter(
    repo: &Repository,
    base: &str,
    revision: &str,
    filter: &str,
    fragment_dir: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    let filter_arg = format!("--diff-filter={filter}");
    let output = Command::new("git")
        .current_dir(repo.root())
        .args([
            "diff",
            "--name-only",
            "-z",
            &filter_arg,
            base,
            revision,
            "--",
        ])
        .arg(fragment_dir)
        .output()
        .map_err(|source| Error::VcsCommandIo {
            command: format!(
                "git diff --name-only -z {filter_arg} {base} {revision} -- {}",
                fragment_dir.display()
            ),
            source,
        })?;
    if output.status.success() {
        Ok(nul_paths(&output.stdout))
    } else {
        Err(Error::VcsCommandFailed {
            command: format!(
                "git diff --name-only -z {filter_arg} {base} {revision} -- {}",
                fragment_dir.display()
            ),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn merge_shared_fragment(
    repo: &Repository,
    temp_root: &std::path::Path,
    base: &str,
    other_head: &str,
    path: &std::path::Path,
) -> Result<()> {
    let current_path = temp_root.join(path);
    let other = git_blob(repo, other_head, path)?;
    let base = match git_blob_or_missing(repo, base, path)? {
        OptionalBlob::Present(base) => base,
        OptionalBlob::Missing { status } => {
            let current = fs::read(&current_path).map_err(|source| Error::ReadFile {
                path: current_path,
                source,
            })?;
            if current == other {
                return write_temp_fragment(temp_root, path, current);
            }
            return Err(Error::MergeFragment {
                path: path.to_path_buf(),
                status,
                stderr: "fragment was added differently on both sides".to_owned(),
            });
        }
    };
    let base_path = write_merge_blob(temp_root, "base", path, base)?;
    let other_path = write_merge_blob(temp_root, "other", path, other)?;
    let output = Command::new("git")
        .current_dir(repo.root())
        .args(["merge-file", "-p"])
        .arg(&current_path)
        .arg(&base_path)
        .arg(&other_path)
        .output()
        .map_err(|source| Error::VcsCommandIo {
            command: format!("git merge-file -p {}", path.display()),
            source,
        })?;
    if output.status.success() {
        write_temp_fragment(temp_root, path, output.stdout)
    } else {
        Err(Error::MergeFragment {
            path: path.to_path_buf(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

enum OptionalBlob {
    Present(Vec<u8>),
    Missing { status: std::process::ExitStatus },
}

fn git_blob_or_missing(
    repo: &Repository,
    revision: &str,
    path: &std::path::Path,
) -> Result<OptionalBlob> {
    match git_blob(repo, revision, path) {
        Ok(blob) => Ok(OptionalBlob::Present(blob)),
        Err(Error::VcsCommandFailed { status, .. }) => Ok(OptionalBlob::Missing { status }),
        Err(error) => Err(error),
    }
}

fn write_merge_blob(
    temp_root: &std::path::Path,
    side: &str,
    path: &std::path::Path,
    contents: Vec<u8>,
) -> Result<PathBuf> {
    let blob_path = temp_root.join(".sacho-merge").join(side).join(path);
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&blob_path, contents).map_err(|source| Error::WriteFile {
        path: blob_path.clone(),
        source,
    })?;
    Ok(blob_path)
}

fn write_temp_fragment(
    temp_root: &std::path::Path,
    path: &std::path::Path,
    contents: Vec<u8>,
) -> Result<()> {
    let target = temp_root.join(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(&target, contents).map_err(|source| Error::WriteFile {
        path: target,
        source,
    })
}

fn is_fragment_markdown_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("md")
}

fn nul_paths(output: &[u8]) -> Vec<PathBuf> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(git_path_from_bytes)
        .collect()
}

#[cfg(unix)]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
#[cfg_attr(test, mutants::skip)]
fn git_path_from_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

fn git_other_head(repo: &Repository) -> Result<Option<String>> {
    if let Some(head) =
        std::env::vars().find_map(|(key, _)| key.strip_prefix("GITHEAD_").map(str::to_owned))
    {
        return Ok(Some(head));
    }
    match git_output(repo, ["rev-parse", "-q", "--verify", "MERGE_HEAD"]) {
        Ok(head) => Ok(Some(head.trim().to_owned())),
        Err(Error::VcsCommandFailed { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn git_blob(repo: &Repository, revision: &str, path: &std::path::Path) -> Result<Vec<u8>> {
    let mut spec = OsString::from(revision);
    spec.push(":");
    spec.push(path.as_os_str());
    let output = Command::new("git")
        .current_dir(repo.root())
        .arg("show")
        .arg(&spec)
        .output()
        .map_err(|source| Error::VcsCommandIo {
            command: format!("git show {}", spec.to_string_lossy()),
            source,
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::VcsCommandFailed {
            command: format!("git show {}", spec.to_string_lossy()),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn git_output<const N: usize>(repo: &Repository, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo.root())
        .args(args)
        .output()
        .map_err(|source| Error::VcsCommandIo {
            command: format!("git {}", args.join(" ")),
            source,
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(Error::VcsCommandFailed {
            command: format!("git {}", args.join(" ")),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn ensure_trailing_newline(source: &str) -> String {
    let mut output = source.trim_end().to_owned();
    output.push('\n');
    output
}

fn changelog_error(path: PathBuf, source: ChangelogError) -> Error {
    match source {
        ChangelogError::RegionNotFound => Error::RegionNotFound { path },
        source => Error::Changelog { path, source },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use proptest::prop_assert;

    use super::*;
    use crate::Repository;

    fn repo_with_fragment(fragment: &str) -> (tempfile::TempDir, Repository) {
        let temp = tempfile::TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        fs::write(temp.path().join("changes.d/change.md"), fragment).expect("fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");
        (temp, repo)
    }

    fn repo_with_config_and_fragment(
        config: &str,
        fragment: &str,
    ) -> (tempfile::TempDir, Repository) {
        let temp = tempfile::TempDir::new().expect("tempdir");
        fs::write(temp.path().join("sacho.toml"), config).expect("config");
        fs::create_dir_all(temp.path().join("changes.d")).expect("fragments");
        fs::write(temp.path().join("changes.d/change.md"), fragment).expect("fragment");
        let repo = Repository::from_root(temp.path()).expect("repo");
        (temp, repo)
    }

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false"])
            .args(args)
            .current_dir(root)
            .status()
            .expect("run Git");
        assert!(
            status.success(),
            "git {} failed with {status}",
            args.join(" ")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn git_stdout(root: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "git {} failed with {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("ASCII Git output")
            .trim()
            .to_owned()
    }

    fn write_inputs(
        root: &std::path::Path,
        current: &str,
        other: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        write_three_inputs(root, current, current, other)
    }

    fn write_three_inputs(
        root: &std::path::Path,
        ancestor: &str,
        current: &str,
        other: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let ancestor_path = root.join("ancestor.md");
        let current_path = root.join("current.md");
        let other_path = root.join("other.md");
        fs::write(&ancestor_path, ancestor).expect("ancestor");
        fs::write(&current_path, current).expect("current");
        fs::write(&other_path, other).expect("other");
        (ancestor_path, current_path, other_path)
    }

    fn merge(
        repo: &Repository,
        ancestor: PathBuf,
        current: PathBuf,
        other: PathBuf,
    ) -> MergeDriverResult {
        merge_driver(
            repo,
            MergeDriverOptions {
                ancestor,
                current,
                other,
                path: PathBuf::from("CHANGES.md"),
            },
        )
        .expect("merge")
    }

    fn parsed_releases(source: &str) -> ParsedChangelog {
        ParsedChangelog {
            prefix: String::new(),
            after_unreleased: String::new(),
            released: parse_released_blocks(source),
        }
    }

    fn parsed_parts(prefix: &str, after_unreleased: &str, released: &str) -> ParsedChangelog {
        ParsedChangelog {
            prefix: prefix.to_owned(),
            after_unreleased: after_unreleased.to_owned(),
            released: parse_released_blocks(released),
        }
    }

    #[test]
    fn recompiles_unreleased_region_from_fragments() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Project changes
===============

Unreleased
----------

To be released.

 -  Current stale entry.

Version 1.0.0
-------------

Released on July 1, 2026.
";
        let other = current.replace("Current stale", "Other stale");
        let (ancestor, current, other) = write_inputs(temp.path(), current, &other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, hints } = result else {
            panic!("expected clean merge");
        };
        assert!(hints.is_empty());
        assert!(output.contains(" -  Fixed merged fragment.\n"));
        assert!(!output.contains("Current stale"));
        assert!(!output.contains("Other stale"));
    }

    #[cfg(unix)]
    #[test]
    fn merged_snapshot_preserves_a_resolved_fragment_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"alias\"\n",
        )
        .expect("config");
        fs::create_dir(temp.path().join("actual")).expect("fragment directory");
        fs::write(
            temp.path().join("actual/change.md"),
            " -  Preserved in merged snapshot.\n",
        )
        .expect("fragment");
        symlink("actual", temp.path().join("alias")).expect("fragment alias");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Initial"]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot = merged_fragment_temp_root(&repo, "HEAD").expect("merged snapshot");
        let snapshot_repo = snapshot.repository().expect("snapshot repo");
        let discovered = crate::fragment::discover_fragment_candidates(&snapshot_repo)
            .expect("snapshot fragments");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(
            discovered.candidates[0].relative_path,
            PathBuf::from("actual/change.md")
        );
    }

    #[test]
    fn merged_snapshot_preserves_the_source_git_lock_location() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\npath = \".sacho.lock\"\n",
        )
        .expect("config");
        fs::create_dir(temp.path().join("changes.d")).expect("fragment directory");
        fs::write(
            temp.path().join("changes.d/change.md"),
            " -  Preserved in merged snapshot.\n",
        )
        .expect("fragment");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Initial"]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot = merged_fragment_temp_root(&repo, "HEAD").expect("merged snapshot");

        let snapshot_repo = snapshot
            .repository()
            .expect("snapshot must retain the source Git lock location");
        crate::fragment::discover_fragment_candidates(&snapshot_repo)
            .expect("snapshot reads must retain the source Git lock location");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn merged_snapshot_preserves_a_non_utf8_resolved_fragment_directory() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"alias\"\n",
        )
        .expect("config");
        let actual = PathBuf::from(OsString::from_vec(b"actual-\xff".to_vec()));
        fs::create_dir(temp.path().join(&actual)).expect("fragment directory");
        fs::write(
            temp.path().join(&actual).join("change.md"),
            " -  Preserved in merged snapshot.\n",
        )
        .expect("fragment");
        symlink(&actual, temp.path().join("alias")).expect("fragment alias");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Initial"]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot = merged_fragment_temp_root(&repo, "HEAD").expect("merged snapshot");
        let snapshot_repo = snapshot.repository().expect("snapshot repo");
        let discovered = crate::fragment::discover_fragment_candidates(&snapshot_repo)
            .expect("snapshot fragments");

        assert_eq!(discovered.candidates.len(), 1);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn merged_snapshot_materializes_a_non_utf8_next_file_alias() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(temp.path().join("sacho.toml"), "").expect("config");
        fs::create_dir(temp.path().join("changes.d")).expect("fragment directory");
        let actual = PathBuf::from(OsString::from_vec(b"next-\xff".to_vec()));
        fs::write(temp.path().join("changes.d").join(&actual), "2.0.0\n").expect("next version");
        symlink(&actual, temp.path().join("changes.d/next")).expect("next-file alias");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Initial"]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot = merged_fragment_temp_root(&repo, "HEAD").expect("merged snapshot");
        let snapshot_repo = snapshot.repository().expect("snapshot repo");
        let compiled = crate::compile::compile_unreleased(
            &snapshot_repo,
            crate::compile::CompileOptions::default(),
        )
        .expect("compiled snapshot");

        assert_eq!(
            compiled.version_label,
            crate::compile::VersionLabel::Version(String::from("2.0.0"))
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn merged_snapshot_materializes_a_non_utf8_section_alias() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(
            temp.path().join("sacho.toml"),
            "[[sections]]\nid = \"core\"\ndirectory = \"alias\"\n",
        )
        .expect("config");
        fs::create_dir(temp.path().join("changes.d")).expect("fragment directory");
        let actual = PathBuf::from(OsString::from_vec(b"actual-\xff".to_vec()));
        fs::create_dir(temp.path().join("changes.d").join(&actual)).expect("section directory");
        fs::write(
            temp.path()
                .join("changes.d")
                .join(&actual)
                .join("change.md"),
            " -  Section fragment.\n",
        )
        .expect("fragment");
        symlink(&actual, temp.path().join("changes.d/alias")).expect("section alias");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Initial"]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot = merged_fragment_temp_root(&repo, "HEAD").expect("merged snapshot");
        let snapshot_repo = snapshot.repository().expect("snapshot repo");
        let discovered = crate::fragment::discover_fragment_candidates(&snapshot_repo)
            .expect("snapshot fragments");

        assert_eq!(discovered.candidates.len(), 1);
        assert_eq!(discovered.candidates[0].section.as_deref(), Some("core"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn merged_snapshot_overlays_changes_under_a_non_utf8_fragment_directory() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        git(temp.path(), &["config", "user.name", "Test User"]);
        fs::write(
            temp.path().join("sacho.toml"),
            "[fragments]\ndirectory = \"alias\"\n",
        )
        .expect("config");
        let actual = PathBuf::from(OsString::from_vec(b"actual-\xff".to_vec()));
        fs::create_dir(temp.path().join(&actual)).expect("fragment directory");
        fs::write(
            temp.path().join(&actual).join("base.md"),
            " -  Base fragment.\n",
        )
        .expect("base fragment");
        symlink(&actual, temp.path().join("alias")).expect("fragment alias");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Base"]);
        let primary_branch = git_stdout(temp.path(), &["branch", "--show-current"]);
        git(temp.path(), &["checkout", "--quiet", "-b", "other"]);
        fs::write(
            temp.path().join(&actual).join("incoming.md"),
            " -  Incoming fragment.\n",
        )
        .expect("incoming fragment");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "Incoming"]);
        let other_head = git_stdout(temp.path(), &["rev-parse", "HEAD"]);
        git(temp.path(), &["checkout", "--quiet", &primary_branch]);
        let repo = Repository::from_root(temp.path()).expect("repo");

        let snapshot =
            merged_fragment_temp_root(&repo, &other_head).expect("merged fragment snapshot");
        let snapshot_repo = snapshot.repository().expect("snapshot repo");
        let discovered = crate::fragment::discover_fragment_candidates(&snapshot_repo)
            .expect("snapshot fragments");

        assert_eq!(discovered.candidates.len(), 2);
        assert!(
            discovered
                .candidates
                .iter()
                .any(|candidate| candidate.path.ends_with("incoming.md"))
        );
    }

    #[test]
    fn inserts_other_only_released_section_and_hints_carry() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Unreleased
----------

To be released.

Version 1.2.0
-------------

Released on July 2, 2026.
";
        let other = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, hints } = result else {
            panic!("expected clean merge");
        };
        assert!(
            output.find("Version 1.2.0").expect("1.2.0")
                < output.find("Version 1.1.5").expect("1.1.5")
        );
        assert_eq!(
            hints,
            vec![MergeHint::InsertedOtherRelease {
                heading: String::from("Version 1.1.5")
            }]
        );
    }

    #[test]
    fn identical_released_section_is_clean() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, current);

        let result = merge(&repo, ancestor, current, other);

        assert!(matches!(result, MergeDriverResult::Clean { .. }));
    }

    #[test]
    fn clean_merge_preserves_current_released_order() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Unreleased
----------

To be released.

Version 1.0.0
-------------

Released on July 1, 2026.

Version 2.0.0
-------------

Released on July 2, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, current);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(
            output.find("Version 1.0.0").expect("1.0.0")
                < output.find("Version 2.0.0").expect("2.0.0")
        );
    }

    #[test]
    fn clean_merge_inserts_other_only_semver_without_reordering_current() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Unreleased
----------

To be released.

Version 3.0.0
-------------

Released on July 3, 2026.

Version 1.0.0
-------------

Released on July 1, 2026.
";
        let other = "\
Unreleased
----------

To be released.

Version 2.0.0
-------------

Released on July 2, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        let version_3 = output.find("Version 3.0.0").expect("3.0.0");
        let version_2 = output.find("Version 2.0.0").expect("2.0.0");
        let version_1 = output.find("Version 1.0.0").expect("1.0.0");
        assert!(version_3 < version_2);
        assert!(version_2 < version_1);
    }

    #[test]
    fn divergent_same_released_section_is_conflict() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let ancestor = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.

 -  Ancestor entry.
";
        let current = ancestor.replace("Ancestor entry", "Current entry");
        let other = ancestor.replace("Ancestor entry", "Other entry");
        let (ancestor, current, other) =
            write_three_inputs(temp.path(), ancestor, &current, &other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Conflict {
            output_with_markers,
            reason,
        } = result
        else {
            panic!("expected conflict");
        };
        assert!(output_with_markers.contains("<<<<<<< current"));
        assert!(output_with_markers.contains("======="));
        assert!(output_with_markers.contains(">>>>>>> other"));
        assert!(matches!(
            reason,
            MergeConflictReason::DivergentReleasedSection { .. }
        ));
    }

    #[test]
    fn other_only_released_section_edit_is_taken_without_conflict() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let ancestor = "\
Unreleased
----------

To be released.

Version 1.0.0
-------------

Released on July 1, 2026.

 -  Fixed typoo.
";
        let current = ancestor;
        let other = ancestor.replace("typoo", "typo");
        let (ancestor, current, other) = write_three_inputs(temp.path(), ancestor, current, &other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.contains(" -  Fixed typo.\n"));
        assert!(!output.contains("<<<<<<< current"));
    }

    #[test]
    fn current_only_released_section_edit_is_kept_without_conflict() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let ancestor = "\
Unreleased
----------

To be released.

Version 1.0.0
-------------

Released on July 1, 2026.

 -  Fixed typoo.
";
        let current = ancestor.replace("typoo", "typo");
        let other = ancestor;
        let (ancestor, current, other) = write_three_inputs(temp.path(), ancestor, &current, other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.contains(" -  Fixed typo.\n"));
        assert!(!output.contains("<<<<<<< current"));
    }

    #[test]
    fn other_only_prefix_edit_is_taken_without_conflict() {
        let parsed_ancestor = parsed_parts("Project changes\n===============\n\n", "", "");
        let parsed_current = parsed_parts("Project changes\n===============\n\n", "", "");
        let parsed_other = parsed_parts("Renamed changes\n===============\n\n", "", "");

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.starts_with("Renamed changes\n===============\n\n"));
        assert!(!output.contains("Project changes"));
    }

    #[test]
    fn identical_prefix_edit_on_both_sides_is_clean() {
        let parsed_ancestor = parsed_parts("Project changes\n===============\n\n", "", "");
        let parsed_current = parsed_parts("Renamed changes\n===============\n\n", "", "");
        let parsed_other = parsed_parts("Renamed changes\n===============\n\n", "", "");

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.starts_with("Renamed changes\n===============\n\n"));
        assert!(!output.contains("<<<<<<< current"));
    }

    #[test]
    fn other_only_after_unreleased_edit_is_taken_without_conflict() {
        let parsed_ancestor = parsed_parts("", "\n<!-- generated by sacho -->\n", "");
        let parsed_current = parsed_parts("", "\n<!-- generated by sacho -->\n", "");
        let parsed_other = parsed_parts("", "\n<!-- generated by sacho merge driver -->\n", "");

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.contains("<!-- generated by sacho merge driver -->"));
        assert!(!output.contains("<!-- generated by sacho -->"));
    }

    #[test]
    fn divergent_prefix_edits_are_conflict() {
        let parsed_ancestor = parsed_parts("Project changes\n===============\n\n", "", "");
        let parsed_current = parsed_parts("Current changes\n===============\n\n", "", "");
        let parsed_other = parsed_parts("Other changes\n===============\n\n", "", "");

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Conflict {
            output_with_markers,
            reason,
        } = result
        else {
            panic!("expected conflict");
        };
        assert_eq!(reason, MergeConflictReason::DivergentPrefix);
        assert!(output_with_markers.contains("<<<<<<< current"));
        assert!(output_with_markers.contains("Current changes"));
        assert!(output_with_markers.contains("Other changes"));
    }

    #[test]
    fn divergent_after_unreleased_edits_are_conflict() {
        let parsed_ancestor = parsed_parts("", "\n<!-- generated by sacho -->\n", "");
        let parsed_current = parsed_parts("", "\n<!-- current -->\n", "");
        let parsed_other = parsed_parts("", "\n<!-- other -->\n", "");

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Conflict {
            output_with_markers,
            reason,
        } = result
        else {
            panic!("expected conflict");
        };
        assert_eq!(reason, MergeConflictReason::DivergentAfterUnreleased);
        assert!(output_with_markers.contains("<<<<<<< current"));
        assert!(output_with_markers.contains("<!-- current -->"));
        assert!(output_with_markers.contains("<!-- other -->"));
    }

    #[test]
    fn conflict_preserves_current_side_released_order_and_unrelated_blocks() {
        let ancestor = "\
Version 2.0.0
-------------

Released on July 2, 2026.

 -  Ancestor conflict.

Version 1.0.0
-------------

Released on July 1, 2026.

 -  Stable entry.
";
        let current = ancestor.replace("Ancestor conflict", "Current conflict");
        let other = ancestor.replace("Ancestor conflict", "Other conflict");
        let parsed_ancestor = parsed_releases(ancestor);
        let parsed_current = parsed_releases(&current);
        let parsed_other = parsed_releases(&other);

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Conflict {
            output_with_markers,
            ..
        } = result
        else {
            panic!("expected conflict");
        };
        let conflict = output_with_markers
            .find("<<<<<<< current")
            .expect("conflict");
        let stable = output_with_markers
            .find("Version 1.0.0")
            .expect("stable block");
        assert!(conflict < stable);
        assert!(output_with_markers.contains(" -  Stable entry.\n"));
        assert_eq!(output_with_markers.matches("<<<<<<< current").count(), 1);
    }

    #[test]
    fn multiple_divergent_sections_keep_first_conflict_marker() {
        let ancestor = "\
Version 2.0.0
-------------

Released on July 2, 2026.

 -  Ancestor first conflict.

Version 1.0.0
-------------

Released on July 1, 2026.

 -  Ancestor second conflict.
";
        let current = ancestor
            .replace("Ancestor first conflict", "Current first conflict")
            .replace("Ancestor second conflict", "Current second conflict");
        let other = ancestor
            .replace("Ancestor first conflict", "Other first conflict")
            .replace("Ancestor second conflict", "Other second conflict");
        let parsed_ancestor = parsed_releases(ancestor);
        let parsed_current = parsed_releases(&current);
        let parsed_other = parsed_releases(&other);

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Conflict {
            output_with_markers,
            ..
        } = result
        else {
            panic!("expected conflict");
        };
        let conflict = output_with_markers
            .find("<<<<<<< current")
            .expect("conflict");
        let first = output_with_markers
            .find("Version 2.0.0")
            .expect("first block");
        let second = output_with_markers
            .find("Version 1.0.0")
            .expect("second block");
        assert!(conflict < first);
        assert!(first < second);
        assert!(output_with_markers.contains(" -  Current first conflict.\n"));
        assert!(output_with_markers.contains(" -  Other first conflict.\n"));
        assert!(!output_with_markers.contains(" -  Other second conflict.\n"));
    }

    #[test]
    fn conflict_output_includes_other_only_releases_after_divergence() {
        let ancestor = "\
Version 2.0.0
-------------

Released on July 2, 2026.

 -  Ancestor conflict.
";
        let current = ancestor.replace("Ancestor conflict", "Current conflict");
        let other = "\
Version 2.0.0
-------------

Released on July 2, 2026.

 -  Other conflict.

Version 1.5.0
-------------

Released on July 1, 2026.

 -  Other-only entry.
";
        let parsed_ancestor = parsed_releases(ancestor);
        let parsed_current = parsed_releases(&current);
        let parsed_other = parsed_releases(other);

        let result = merge_parsed_changelogs(
            parsed_ancestor,
            parsed_current,
            parsed_other,
            "Unreleased\n----------\n\nTo be released.\n",
        )
        .expect("merge");

        let MergeDriverResult::Conflict {
            output_with_markers,
            ..
        } = result
        else {
            panic!("expected conflict");
        };
        assert!(output_with_markers.contains("<<<<<<< current"));
        assert!(output_with_markers.contains("Version 1.5.0"));
        assert!(output_with_markers.contains(" -  Other-only entry.\n"));
    }

    #[test]
    fn reconstruction_uses_single_blank_line_between_blocks() {
        let block = ReleasedBlock {
            heading: String::from("Version 1.0.0"),
            version: String::from("1.0.0"),
            body: String::from("Version 1.0.0\n-------------\n\nReleased on July 1, 2026.\n"),
        };

        let output = reconstruct(
            "Project changes\n===============\n\n",
            "Unreleased\n----------\n\nTo be released.\n",
            "",
            [&block].into_iter(),
        );

        assert_eq!(
            output,
            "\
Project changes
===============

Unreleased
----------

To be released.

Version 1.0.0
-------------

Released on July 1, 2026.
"
        );
    }

    #[test]
    fn marker_mode_preserves_end_marker() {
        let (temp, repo) = repo_with_config_and_fragment(
            "[changelog]\nregion-detection = \"marker\"\n",
            " -  Fixed marker merge.\n",
        );
        let current = "\
Project changes
===============

<!-- sacho:unreleased:begin -->
Unreleased
----------

To be released.
<!-- sacho:unreleased:end -->

Version 1.0.0
-------------

Released on July 1, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, current);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        assert!(output.contains("<!-- sacho:unreleased:begin -->"));
        assert!(output.contains("<!-- sacho:unreleased:end -->"));
        assert!(
            output
                .find("<!-- sacho:unreleased:end -->")
                .expect("end marker")
                < output.find("Version 1.0.0").expect("released")
        );
    }

    #[test]
    fn copying_missing_fragment_directory_is_no_op() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let missing = temp.path().join("missing");
        let target = temp.path().join("target");

        copy_directory(missing, target.clone()).expect("copy");

        assert!(!target.exists());
    }

    #[test]
    fn non_semver_sections_stay_after_semver_in_relative_order() {
        let (temp, repo) = repo_with_fragment(" -  Fixed merged fragment.\n");
        let current = "\
Unreleased
----------

To be released.

Version alpha
-------------

Released on July 1, 2026.
";
        let other = "\
Unreleased
----------

To be released.

Version 2.0.0
-------------

Released on July 3, 2026.

Version beta
------------

Released on July 2, 2026.
";
        let (ancestor, current, other) = write_inputs(temp.path(), current, other);

        let result = merge(&repo, ancestor, current, other);

        let MergeDriverResult::Clean { output, .. } = result else {
            panic!("expected clean merge");
        };
        let semver = output.find("Version 2.0.0").expect("semver");
        let alpha = output.find("Version alpha").expect("alpha");
        let beta = output.find("Version beta").expect("beta");
        assert!(semver < alpha);
        assert!(alpha < beta);
    }

    proptest::proptest! {
        #[test]
        fn identical_version_bytes_are_clean(version in "[0-9]+\\.[0-9]+\\.[0-9]+") {
            let heading = format!("Version {version}");
            let block = format!("{heading}\n{}\n\nReleased on July 1, 2026.\n", "-".repeat(heading.len()));
            let current = format!("Unreleased\n----------\n\nTo be released.\n\n{block}");
            let parsed_ancestor = parsed_releases(&block);
            let parsed_current = parsed_releases(&block);
            let parsed_other = parsed_releases(&block);

            let result =
                merge_parsed_changelogs(parsed_ancestor, parsed_current, parsed_other, &current)
                    .expect("merge");

            let clean = matches!(result, MergeDriverResult::Clean { .. });
            prop_assert!(clean);
        }

        #[test]
        fn divergent_version_bytes_conflict(version in "[0-9]+\\.[0-9]+\\.[0-9]+") {
            let heading = format!("Version {version}");
            let underline = "-".repeat(heading.len());
            let current_block = format!("{heading}\n{underline}\n\nReleased on July 1, 2026.\n\n -  Current.\n");
            let other_block = format!("{heading}\n{underline}\n\nReleased on July 1, 2026.\n\n -  Other.\n");
            let ancestor_block = format!("{heading}\n{underline}\n\nReleased on July 1, 2026.\n\n -  Ancestor.\n");
            let parsed_ancestor = parsed_releases(&ancestor_block);
            let parsed_current = parsed_releases(&current_block);
            let parsed_other = parsed_releases(&other_block);

            let result = merge_parsed_changelogs(
                parsed_ancestor,
                parsed_current,
                parsed_other,
                "Unreleased\n----------\n\nTo be released.\n",
            )
            .expect("merge");

            let conflict = matches!(result, MergeDriverResult::Conflict { .. });
            prop_assert!(conflict);
        }
    }
}
