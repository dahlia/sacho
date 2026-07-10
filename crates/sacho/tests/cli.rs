use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as ProcessCommand;

#[test]
fn help_lists_commands() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Manage unreleased changelog fragments",
        ))
        .stdout(predicate::str::contains(
            "Print the compiled unreleased region",
        ));
}

#[test]
fn init_scaffolds_empty_git_repository() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created sacho.toml"))
        .stdout(predicate::str::contains("created changes.d"))
        .stdout(predicate::str::contains("created CHANGES.md"))
        .stdout(predicate::str::contains("created .gitattributes"))
        .stdout(predicate::str::contains("configured merge.sacho.driver"));

    assert!(temp.path().join("sacho.toml").is_file());
    assert!(temp.path().join("changes.d").is_dir());
    assert!(temp.path().join("CHANGES.md").is_file());
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".gitattributes")).expect("attributes"),
        "CHANGES.md merge=sacho\nchanges.d/next merge=ours\n"
    );
    assert_eq!(
        git_output(temp.path(), ["config", "--get", "merge.sacho.driver"]).trim(),
        "sacho merge-driver %O %A %B %P"
    );
    assert!(!temp.path().join(".git/hooks/pre-commit").exists());

    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already initialized"))
        .stdout(predicate::str::contains("configured").not());
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".gitattributes")).expect("attributes"),
        "CHANGES.md merge=sacho\nchanges.d/next merge=ours\n"
    );
}

#[test]
fn init_install_hook_writes_marked_hook() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive", "--install-hook"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created .git/hooks/pre-commit"));

    assert_eq!(
        std::fs::read_to_string(temp.path().join(".git/hooks/pre-commit")).expect("hook"),
        "#!/bin/sh\n# sacho pre-commit begin\nsacho check\n# sacho pre-commit end\n"
    );
}

#[test]
fn init_install_hook_honors_core_hooks_path() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "core.hooksPath", ".githooks"]);
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive", "--install-hook"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created .githooks/pre-commit"));

    assert!(temp.path().join(".githooks/pre-commit").is_file());
    assert!(!temp.path().join(".git/hooks/pre-commit").exists());
}

#[test]
fn init_install_hook_works_from_git_worktree() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let main = temp.path().join("main");
    let worktree = temp.path().join("worktree");
    std::fs::create_dir(&main).expect("main dir");
    git(&main, ["init"]);
    git(&main, ["config", "user.email", "test@example.com"]);
    git(&main, ["config", "user.name", "Test User"]);
    std::fs::write(main.join("README.md"), "test\n").expect("readme");
    git(&main, ["add", "README.md"]);
    git(&main, ["commit", "-m", "Initial"]);
    git(
        &main,
        ["worktree", "add", worktree.to_str().expect("utf8 path")],
    );
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(&worktree)
        .args(["init", "--no-interactive", "--install-hook"])
        .assert()
        .success();

    assert!(main.join(".git/hooks/pre-commit").is_file());
    assert!(!worktree.join(".git/hooks/pre-commit").exists());
}

#[test]
fn init_preserves_existing_config() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let config = "[changelog]\ntitle = \"Custom changelog\"\nmaterialize = false\n";
    std::fs::write(temp.path().join("sacho.toml"), config).expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join("sacho.toml")).expect("config"),
        config
    );
}

#[test]
fn init_appends_existing_gitattributes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(temp.path().join(".gitattributes"), "*.md text\n").expect("attributes");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join(".gitattributes")).expect("attributes"),
        "*.md text\nCHANGES.md merge=sacho\nchanges.d/next merge=ours\n"
    );
}

#[test]
fn init_reports_empty_existing_gitattributes_as_modified() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(temp.path().join(".gitattributes"), "").expect("attributes");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("modified .gitattributes"))
        .stdout(predicate::str::contains("created .gitattributes").not());

    assert_eq!(
        std::fs::read_to_string(temp.path().join(".gitattributes")).expect("attributes"),
        "CHANGES.md merge=sacho\nchanges.d/next merge=ours\n"
    );
}

#[test]
fn init_quotes_gitattributes_paths_with_spaces() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
path = \"docs/change log.md\"

[fragments]
directory = \"changes dir\"
",
    )
    .expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join(".gitattributes")).expect("attributes"),
        "\"docs/change log.md\" merge=sacho\n\"changes dir/next\" merge=ours\n"
    );
    assert!(
        git_output(
            temp.path(),
            ["check-attr", "merge", "--", "docs/change log.md"]
        )
        .contains("merge: sacho")
    );
    assert!(
        git_output(
            temp.path(),
            ["check-attr", "merge", "--", "changes dir/next"]
        )
        .contains("merge: ours")
    );
}

#[test]
fn init_quotes_gitattributes_paths_that_start_like_comments() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
path = \"#changes.md\"
",
    )
    .expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert!(
        std::fs::read_to_string(temp.path().join(".gitattributes"))
            .expect("attributes")
            .contains("\"#changes.md\" merge=sacho")
    );
    assert!(
        git_output(temp.path(), ["check-attr", "merge", "--", "#changes.md"])
            .contains("merge: sacho")
    );
}

#[test]
fn init_reports_incompatible_gitattributes_rule() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::write(
        temp.path().join(".gitattributes"),
        "CHANGES.md merge=union\n",
    )
    .expect("attributes");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("incompatible merge attribute"));
}

#[test]
fn init_interactive_requires_terminal() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--interactive"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--interactive requires terminal"));
}

#[test]
fn init_rejects_conflicting_interactive_flags() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--interactive", "--no-interactive"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--interactive and --no-interactive cannot be used together",
        ));
}

#[test]
fn init_install_hook_rejects_existing_unmarked_hook_non_interactively() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::write(
        temp.path().join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho existing\n",
    )
    .expect("hook");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive", "--install-hook"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "already exists without a Sacho marker",
        ));
}

#[test]
fn preview_prints_empty_unreleased_region() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["preview"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Unreleased\n----------"))
        .stdout(predicate::str::contains("To be released."));
}

#[test]
fn preview_rejects_unknown_section() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["preview", "--section", "missing"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown section \"missing\""));
}

#[test]
fn add_help_does_not_panic() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create a new changelog fragment"))
        .stdout(predicate::str::contains(
            "Section id to place the fragment under",
        ))
        .stdout(predicate::str::contains("Topic-based fragment file name"));
}

#[test]
fn preview_help_describes_section_option() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["preview", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Print the compiled unreleased region",
        ))
        .stdout(predicate::str::contains("Section id to preview by itself"));
}

#[test]
fn add_rejects_section_flag_without_sections() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["add", "--section", "core", "clear-function"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("section must not be supplied"));
}

#[test]
fn add_prints_created_fragment_path() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
materialize = false
",
    )
    .expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["add", "clear-function"])
        .assert()
        .success()
        .stdout(predicate::str::contains("changes.d/clear-function.md"));

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/clear-function.md")).expect("fragment"),
        " -\n"
    );
}

#[test]
fn add_updates_materialized_changelog_and_leaves_check_clean() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n",
    )
    .expect("changelog");
    let mut add = Command::cargo_bin("sacho").expect("binary");

    add.current_dir(temp.path())
        .args(["add", "clear-function"])
        .assert()
        .success();

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -\n"));

    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .args(["check"])
        .assert()
        .success();
}

#[test]
fn check_warning_only_exits_success() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
materialize = false
",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/warning.md"),
        "---\nowner: docs\n---\n -  Added docs.\n",
    )
    .expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["check"])
        .assert()
        .success()
        .stderr(predicate::str::contains("warning:"));
}

#[test]
fn sync_force_rewrites_materialized_changelog() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changes
===============

Unreleased
----------

To be released.

Version 0.1.0
-------------

Released on July 1, 2026.
",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["sync", "--force"])
        .assert()
        .success();

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Fixed sync.\n"));
    assert!(changelog.contains("Version 0.1.0\n-------------"));
}

#[test]
fn sync_without_force_exits_two_and_leaves_changelog_unchanged() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
    let original = "Unreleased\n----------\n\nTo be released.\n";
    std::fs::write(temp.path().join("CHANGES.md"), original).expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["sync"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--force"));

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert_eq!(changelog, original);
}

#[test]
fn check_reports_mismatch_and_passes_after_sync() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n",
    )
    .expect("changelog");

    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .args(["check"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "materialized changelog is out of sync",
        ));

    let mut sync = Command::cargo_bin("sacho").expect("binary");
    sync.current_dir(temp.path())
        .args(["sync", "--force"])
        .assert()
        .success();

    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .args(["check"])
        .assert()
        .success();
}

#[test]
fn merge_driver_rewrites_current_file_and_prints_carry_hint() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/merge.md"),
        " -  Fixed merged fragment.\n",
    )
    .expect("fragment");
    let current = temp.path().join("current.md");
    let ancestor = temp.path().join("ancestor.md");
    let other = temp.path().join("other.md");
    let current_source = "\
Unreleased
----------

To be released.

Version 1.2.0
-------------

Released on July 2, 2026.
";
    let other_source = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.
";
    std::fs::write(&ancestor, current_source).expect("ancestor");
    std::fs::write(&current, current_source).expect("current");
    std::fs::write(&other, other_source).expect("other");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "merge-driver",
            ancestor.to_str().expect("ancestor path"),
            current.to_str().expect("current path"),
            other.to_str().expect("other path"),
            "CHANGES.md",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("sacho carry 1.1.5"));

    let merged = std::fs::read_to_string(current).expect("merged current");
    assert!(merged.contains(" -  Fixed merged fragment.\n"));
    assert!(
        merged.find("Version 1.2.0").expect("1.2.0") < merged.find("Version 1.1.5").expect("1.1.5")
    );
}

#[test]
fn merge_driver_writes_conflict_markers_and_exits_one() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/merge.md"),
        " -  Fixed merged fragment.\n",
    )
    .expect("fragment");
    let current = temp.path().join("current.md");
    let ancestor = temp.path().join("ancestor.md");
    let other = temp.path().join("other.md");
    let ancestor_source = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.

 -  Ancestor entry.
";
    let current_source = ancestor_source.replace("Ancestor entry", "Current entry");
    let other_source = ancestor_source.replace("Ancestor entry", "Other entry");
    std::fs::write(&ancestor, ancestor_source).expect("ancestor");
    std::fs::write(&current, current_source).expect("current");
    std::fs::write(&other, other_source).expect("other");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "merge-driver",
            ancestor.to_str().expect("ancestor path"),
            current.to_str().expect("current path"),
            other.to_str().expect("other path"),
            "CHANGES.md",
        ])
        .assert()
        .code(1);

    let merged = std::fs::read_to_string(current).expect("merged current");
    assert!(merged.contains("<<<<<<< current"));
    assert!(merged.contains("======="));
    assert!(merged.contains(">>>>>>> other"));
}

#[test]
fn merge_driver_takes_one_sided_other_release_edit() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/merge.md"),
        " -  Fixed merged fragment.\n",
    )
    .expect("fragment");
    let current = temp.path().join("current.md");
    let ancestor = temp.path().join("ancestor.md");
    let other = temp.path().join("other.md");
    let ancestor_source = "\
Unreleased
----------

To be released.

Version 1.1.5
-------------

Released on July 1, 2026.

 -  Fixed typoo.
";
    let current_source = ancestor_source;
    let other_source = ancestor_source.replace("typoo", "typo");
    std::fs::write(&ancestor, ancestor_source).expect("ancestor");
    std::fs::write(&current, current_source).expect("current");
    std::fs::write(&other, other_source).expect("other");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "merge-driver",
            ancestor.to_str().expect("ancestor path"),
            current.to_str().expect("current path"),
            other.to_str().expect("other path"),
            "CHANGES.md",
        ])
        .assert()
        .success();

    let merged = std::fs::read_to_string(current).expect("merged current");
    assert!(merged.contains(" -  Fixed typo.\n"));
    assert!(!merged.contains("<<<<<<< current"));
}

#[test]
fn merge_driver_compile_failure_leaves_current_file_unchanged() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/bad.md"), "not a list\n").expect("fragment");
    let current = temp.path().join("current.md");
    let ancestor = temp.path().join("ancestor.md");
    let other = temp.path().join("other.md");
    let source = "Unreleased\n----------\n\nTo be released.\n";
    std::fs::write(&ancestor, source).expect("ancestor");
    std::fs::write(&current, source).expect("current");
    std::fs::write(&other, source).expect("other");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "merge-driver",
            ancestor.to_str().expect("ancestor path"),
            current.to_str().expect("current path"),
            other.to_str().expect("other path"),
            "CHANGES.md",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("invalid fragment"));

    assert_eq!(std::fs::read_to_string(current).expect("current"), source);
}

#[test]
fn git_merge_driver_compiles_with_incoming_fragments_not_yet_in_worktree() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let binary = Command::cargo_bin("sacho")
        .expect("binary")
        .get_program()
        .to_owned();
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n",
    )
    .expect("changelog");
    std::fs::write(
        temp.path().join(".gitattributes"),
        "CHANGES.md merge=sacho\n",
    )
    .expect("attributes");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "user.email", "test@example.com"]);
    git(temp.path(), ["config", "user.name", "Test User"]);
    let driver = format!("{} merge-driver %O %A %B %P", binary.display());
    git(
        temp.path(),
        ["config", "merge.sacho.driver", driver.as_str()],
    );
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Initial"]);
    let initial_branch = git_output(temp.path(), ["rev-parse", "--abbrev-ref", "HEAD"]);
    let initial_branch = initial_branch.trim();

    git(temp.path(), ["checkout", "-qb", "feature"]);
    std::fs::write(
        temp.path().join("changes.d/other.md"),
        " -  Added other feature.\n",
    )
    .expect("other fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Added other feature.
",
    )
    .expect("other changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Other feature"]);

    git(temp.path(), ["checkout", "-q", initial_branch]);
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/current.md"),
        " -  Added current feature.\n",
    )
    .expect("current fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Added current feature.
",
    )
    .expect("current changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Current feature"]);

    git(temp.path(), ["merge", "feature"]);

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Added current feature.\n"));
    assert!(changelog.contains(" -  Added other feature.\n"));
    assert!(temp.path().join("changes.d/other.md").is_file());
}

#[test]
fn git_merge_driver_compiles_merged_shared_fragment_file() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let binary = Command::cargo_bin("sacho")
        .expect("binary")
        .get_program()
        .to_owned();
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Base entry.\n -  Unchanged first context.\n -  Unchanged second context.\n -  Shared entry.\n",
    )
    .expect("base fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Base entry.
 -  Unchanged first context.
 -  Unchanged second context.
 -  Shared entry.
",
    )
    .expect("changelog");
    std::fs::write(
        temp.path().join(".gitattributes"),
        "CHANGES.md merge=sacho\n",
    )
    .expect("attributes");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "user.email", "test@example.com"]);
    git(temp.path(), ["config", "user.name", "Test User"]);
    let driver = format!("{} merge-driver %O %A %B %P", binary.display());
    git(
        temp.path(),
        ["config", "merge.sacho.driver", driver.as_str()],
    );
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Initial"]);
    let initial_branch = git_output(temp.path(), ["rev-parse", "--abbrev-ref", "HEAD"]);
    let initial_branch = initial_branch.trim();

    git(temp.path(), ["checkout", "-qb", "feature"]);
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Base entry.\n -  Unchanged first context.\n -  Unchanged second context.\n -  Other entry.\n",
    )
    .expect("other fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Base entry.
 -  Unchanged first context.
 -  Unchanged second context.
 -  Other entry.
",
    )
    .expect("other changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Other shared edit"]);

    git(temp.path(), ["checkout", "-q", initial_branch]);
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Current entry.\n -  Unchanged first context.\n -  Unchanged second context.\n -  Shared entry.\n",
    )
    .expect("current fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Current entry.
 -  Unchanged first context.
 -  Unchanged second context.
 -  Shared entry.
",
    )
    .expect("current changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Current shared edit"]);

    git(temp.path(), ["merge", "feature"]);

    let fragment =
        std::fs::read_to_string(temp.path().join("changes.d/shared.md")).expect("fragment");
    assert!(fragment.contains(" -  Current entry.\n"));
    assert!(fragment.contains(" -  Other entry.\n"));
    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Current entry.\n"));
    assert!(changelog.contains(" -  Other entry.\n"));
    assert!(!changelog.contains(" -  Shared entry.\n"));
}

#[test]
fn git_merge_driver_keeps_current_fragment_deleted_on_other_side() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let binary = Command::cargo_bin("sacho")
        .expect("binary")
        .get_program()
        .to_owned();
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/shared.md"), " -  Base entry.\n")
        .expect("base fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Base entry.
",
    )
    .expect("changelog");
    std::fs::write(
        temp.path().join(".gitattributes"),
        "CHANGES.md merge=sacho\n",
    )
    .expect("attributes");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "user.email", "test@example.com"]);
    git(temp.path(), ["config", "user.name", "Test User"]);
    let driver = format!("{} merge-driver %O %A %B %P", binary.display());
    git(
        temp.path(),
        ["config", "merge.sacho.driver", driver.as_str()],
    );
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Initial"]);
    let initial_branch = git_output(temp.path(), ["rev-parse", "--abbrev-ref", "HEAD"]);
    let initial_branch = initial_branch.trim();

    git(temp.path(), ["checkout", "-qb", "feature"]);
    std::fs::remove_file(temp.path().join("changes.d/shared.md")).expect("delete fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.
",
    )
    .expect("other changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Delete shared fragment"]);

    git(temp.path(), ["checkout", "-q", initial_branch]);
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Current entry.\n",
    )
    .expect("current fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Current entry.
",
    )
    .expect("current changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Edit shared fragment"]);

    let output = ProcessCommand::new("git")
        .current_dir(temp.path())
        .args(["merge", "feature"])
        .output()
        .expect("git merge");
    assert!(
        !output.status.success(),
        "modify/delete merge unexpectedly succeeded"
    );

    let fragment =
        std::fs::read_to_string(temp.path().join("changes.d/shared.md")).expect("fragment");
    assert!(fragment.contains(" -  Current entry.\n"));
    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Current entry.\n"));
}

#[test]
fn git_merge_driver_keeps_identical_fragment_added_on_both_sides() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let binary = Command::cargo_bin("sacho")
        .expect("binary")
        .get_program()
        .to_owned();
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n",
    )
    .expect("changelog");
    std::fs::write(
        temp.path().join(".gitattributes"),
        "CHANGES.md merge=sacho\n",
    )
    .expect("attributes");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "user.email", "test@example.com"]);
    git(temp.path(), ["config", "user.name", "Test User"]);
    let driver = format!("{} merge-driver %O %A %B %P", binary.display());
    git(
        temp.path(),
        ["config", "merge.sacho.driver", driver.as_str()],
    );
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Initial"]);
    let initial_branch = git_output(temp.path(), ["rev-parse", "--abbrev-ref", "HEAD"]);
    let initial_branch = initial_branch.trim();

    git(temp.path(), ["checkout", "-qb", "feature"]);
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Added shared feature.\n",
    )
    .expect("other fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Other stale shared feature.
",
    )
    .expect("other changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Other shared addition"]);

    git(temp.path(), ["checkout", "-q", initial_branch]);
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/shared.md"),
        " -  Added shared feature.\n",
    )
    .expect("current fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

 -  Current stale shared feature.
",
    )
    .expect("current changelog");
    git(temp.path(), ["add", "."]);
    git(temp.path(), ["commit", "-m", "Current shared addition"]);

    git(temp.path(), ["merge", "feature"]);

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Added shared feature.\n"));
    assert_eq!(changelog.matches(" -  Added shared feature.\n").count(), 1);
    assert!(!changelog.contains("stale shared feature"));
}

#[test]
fn check_base_reports_git_missing_fragment_violation() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
materialize = false

[check]
paths = [\"src/**\"]
",
    )
    .expect("config");
    git(temp.path(), ["init"]);
    git(temp.path(), ["config", "user.email", "test@example.com"]);
    git(temp.path(), ["config", "user.name", "Test User"]);
    git(temp.path(), ["add", "sacho.toml"]);
    git(temp.path(), ["commit", "-m", "Initial config"]);
    std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
    std::fs::write(temp.path().join("src/lib.rs"), "pub fn changed() {}\n").expect("source");
    git(temp.path(), ["add", "src/lib.rs"]);
    git(temp.path(), ["commit", "-m", "Change API"]);
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["check", "--base", "HEAD~1"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("missing changelog fragment"))
        .stderr(predicate::str::contains("src/lib.rs"))
        .stderr(predicate::str::contains("Changelog: none"));
}

fn git<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
    let output = ProcessCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output<const N: usize>(dir: &std::path::Path, args: [&str; N]) -> String {
    let output = ProcessCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn release_command_writes_changelog_and_consumes_fragments() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "\
[changelog]
materialize = false
",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/release.md"),
        " -  Fixed release.\n",
    )
    .expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Changelog\n=========\n\nVersion 0.1.0\n-------------\n\nReleased on July 1, 2026.\n",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["release", "0.2.0", "--date", "2026-07-08"])
        .assert()
        .success();

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains("Version 0.2.0\n-------------"));
    assert!(changelog.contains("Released on July 8, 2026."));
    assert!(!temp.path().join("changes.d/release.md").exists());
}
