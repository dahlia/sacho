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
