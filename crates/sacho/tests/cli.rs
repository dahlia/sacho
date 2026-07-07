use assert_cmd::Command;
use predicates::prelude::*;

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
fn add_accepts_section_flag() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["add", "--section", "core", "clear-function"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "add command is not implemented yet",
        ));
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
