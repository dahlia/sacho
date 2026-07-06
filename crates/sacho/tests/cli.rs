use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_commands() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn command_errors_exit_with_usage_error_code() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["preview"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "preview command is not implemented yet",
        ));
}

#[test]
fn add_help_does_not_panic() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--section"));
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
