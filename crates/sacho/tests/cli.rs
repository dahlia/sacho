use assert_cmd::Command;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Command as ProcessCommand;
use std::thread;

struct OneRequestHandle {
    address: SocketAddr,
    handle: Option<thread::JoinHandle<String>>,
}

impl OneRequestHandle {
    fn join(mut self) -> thread::Result<String> {
        self.wake();
        self.handle.take().expect("handle").join()
    }

    fn wake(&self) {
        if let Ok(mut stream) = TcpStream::connect(self.address) {
            let _ = stream.write_all(b"HEAD /test-server-shutdown HTTP/1.1\r\n\r\n");
        }
    }
}

impl Drop for OneRequestHandle {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        self.wake();
        let _ = handle.join();
    }
}

fn one_request_http_server() -> (String, OneRequestHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let base = format!("http://{address}");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection");
        let request = BufReader::new(&stream)
            .lines()
            .next()
            .expect("request line")
            .expect("request contents");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .expect("response");
        request
    });
    (
        base,
        OneRequestHandle {
            address,
            handle: Some(handle),
        },
    )
}

fn redirecting_http_server() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let base = format!("http://{address}");
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            "HTTP/1.1 302 Found\r\nConnection: close\r\nLocation: /pull/3\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        ] {
            let (mut stream, _) = listener.accept().expect("connection");
            requests.push(
                BufReader::new(&stream)
                    .lines()
                    .next()
                    .expect("request line")
                    .expect("request contents"),
            );
            stream.write_all(response.as_bytes()).expect("response");
        }
        requests
    });
    (base, handle)
}

#[cfg(unix)]
#[test]
fn repository_commands_reject_an_external_configured_path_without_touching_it() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let outside = tempfile::TempDir::new().expect("outside tempdir");
    let sentinel = outside.path().join("protected.txt");
    std::fs::write(&sentinel, "unchanged\n").expect("sentinel");
    symlink(outside.path(), temp.path().join("linked")).expect("external symlink");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[fragments]\ndirectory = \"linked\"\n",
    )
    .expect("config");

    let invocations: &[&[&str]] = &[
        &["init", "--no-interactive"],
        &["add", "topic"],
        &["next", "1.2.0"],
        &["check", "--fix"],
        &["fmt"],
        &["resolve-links"],
        &["preview"],
        &["show", "1.1.0"],
        &["sync", "--force"],
        &["release", "1.2.0", "--date", "2026-07-15"],
        &["carry", "1.1.0"],
        &["import-unreleased", "--force"],
    ];

    for args in invocations {
        let mut command = Command::cargo_bin("sacho").expect("binary");
        command
            .current_dir(temp.path())
            .args(*args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("fragments.directory"))
            .stderr(predicate::str::contains("linked"));

        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("sentinel after command"),
            "unchanged\n",
            "command {args:?} changed the external sentinel"
        );
        assert_eq!(
            std::fs::read_dir(outside.path())
                .expect("outside directory")
                .count(),
            1,
            "command {args:?} created an external path"
        );
        assert!(
            !temp.path().join(".sacho.lock").exists(),
            "command {args:?} created the mutation lock before validation"
        );
    }
}

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
        ))
        .stdout(predicate::str::contains(
            "Print a released changelog section",
        ))
        .stdout(predicate::str::contains("Copyright (C) 2026 Hong Minhee"))
        .stdout(predicate::str::contains("GNU GPLv3 only"))
        .stdout(predicate::str::contains("ABSOLUTELY NO WARRANTY"))
        .stdout(predicate::str::contains(
            "Print copyright and license information",
        ))
        .stdout(predicate::str::contains("sacho --license"));
}

#[test]
fn license_option_prints_the_project_notice_and_full_license() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .arg("--license")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "Sacho  Copyright (C) 2026  Hong Minhee\n",
        ))
        .stdout(predicate::str::contains(
            "This program comes with ABSOLUTELY NO WARRANTY.",
        ))
        .stdout(predicate::str::contains("GNU GENERAL PUBLIC LICENSE"))
        .stdout(predicate::str::contains("Version 3, 29 June 2007"))
        .stdout(predicate::str::ends_with(include_str!("../LICENSE")));
}

#[test]
fn init_help_describes_repository_and_integration_options() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--repository-url <URL>"))
        .stdout(predicate::str::contains(
            "Repository URL for # links in new configuration",
        ))
        .stdout(predicate::str::contains("--integration-executable <PATH>"))
        .stdout(predicate::str::contains(
            "Executable used by installed VCS integrations",
        ));
}

#[test]
fn check_rejects_base_with_staged_mode() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["check", "--base", "main", "--staged"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the argument '--base <BASE>' cannot be used with '--staged'",
        ));
}

#[test]
fn mercurial_update_hook_tolerates_irrelevant_updates_and_absent_configuration() {
    let temp = tempfile::TempDir::new().expect("tempdir");

    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(temp.path())
        .env("HG_PARENT2", "other")
        .env("HG_ERROR", "0")
        .arg("hook-hg-update")
        .assert()
        .success();

    std::fs::write(temp.path().join("sacho.toml"), "[").expect("malformed configuration");
    for (parent2, hook_error) in [(None, "0"), (Some("other"), "1"), (Some(" "), "0")] {
        let mut command = Command::cargo_bin("sacho").expect("binary");
        command.current_dir(temp.path()).env("HG_ERROR", hook_error);
        if let Some(parent2) = parent2 {
            command.env("HG_PARENT2", parent2);
        } else {
            command.env_remove("HG_PARENT2");
        }
        command.arg("hook-hg-update").assert().success();
    }
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
    let merge_driver = git_output(temp.path(), ["config", "--get", "merge.sacho.driver"]);
    assert!(merge_driver.contains(env!("CARGO_BIN_EXE_sacho")));
    assert!(merge_driver.trim().ends_with("merge-driver %O %A %B %P"));
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
fn init_uses_an_explicit_integration_executable() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "init",
            "--no-interactive",
            "--integration-executable",
            "sacho-1",
        ])
        .assert()
        .success();

    assert_eq!(
        git_output(temp.path(), ["config", "--get", "merge.sacho.driver"]).trim(),
        "sacho-1 merge-driver %O %A %B %P"
    );
}

#[test]
fn init_non_interactive_only_uses_an_explicit_repository_url() {
    let inferred = tempfile::TempDir::new().expect("inferred tempdir");
    git(inferred.path(), ["init"]);
    git(
        inferred.path(),
        [
            "remote",
            "add",
            "origin",
            "ssh://git@codeberg.org/team/project.git",
        ],
    );
    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(inferred.path())
        .args(["init", "--no-interactive"])
        .assert()
        .success();
    assert!(
        !std::fs::read_to_string(inferred.path().join("sacho.toml"))
            .expect("inferred config")
            .contains("[links]")
    );

    let explicit = tempfile::TempDir::new().expect("explicit tempdir");
    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(explicit.path())
        .args([
            "init",
            "--no-interactive",
            "--repository-url",
            "https://user:password@gitlab.com/group/project.git?token=secret#branch",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("password").not())
        .stdout(predicate::str::contains("secret").not())
        .stderr(predicate::str::contains("password").not())
        .stderr(predicate::str::contains("secret").not());
    let config =
        std::fs::read_to_string(explicit.path().join("sacho.toml")).expect("explicit config");
    assert!(
        config.contains(r##""#" = "https://gitlab.com/group/project/-/issues/{n}""##),
        "{config}"
    );
    assert!(!config.contains("password"));
    assert!(!config.contains("secret"));
}

#[test]
fn init_rejects_an_invalid_explicit_repository_url_without_echoing_it() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let secret = "file:///private/token-secret/project";
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["init", "--no-interactive", "--repository-url", secret])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "repository URL is not a supported HTTPS or forge clone URL",
        ))
        .stderr(predicate::str::contains("token-secret").not());

    assert!(!temp.path().join("sacho.toml").exists());
    assert!(!temp.path().join("CHANGES.md").exists());
    assert!(!temp.path().join("changes.d").exists());
}

#[test]
fn init_interactive_accepts_edits_or_omits_the_inferred_repository_url() {
    let cases = [
        (
            "\n\n\n\n\n",
            Some("https://codeberg.org/team/project/issues/{n}"),
        ),
        (
            "\n\n\nn\nhttps://gitlab.com/group/project.git\n\n",
            Some("https://gitlab.com/group/project/-/issues/{n}"),
        ),
        ("\n\n\nn\n\n\n", None),
    ];

    for (input, expected_link) in cases {
        let temp = tempfile::TempDir::new().expect("tempdir");
        git(temp.path(), ["init"]);
        git(
            temp.path(),
            [
                "remote",
                "add",
                "origin",
                "ssh://git@codeberg.org/team/project.git",
            ],
        );

        let (success, output) = run_in_terminal(temp.path(), &["init"], input);

        assert!(success, "{output}");
        let config =
            std::fs::read_to_string(temp.path().join("sacho.toml")).expect("interactive config");
        if let Some(expected_link) = expected_link {
            assert!(config.contains(expected_link), "{config}");
        } else {
            assert!(!config.contains("[links]"), "{config}");
        }
    }
}

#[test]
fn init_interactive_infers_a_git_worktree_remote() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let main = temp.path().join("main");
    let worktree = temp.path().join("worktree");
    std::fs::create_dir(&main).expect("main directory");
    git(&main, ["init"]);
    git(&main, ["config", "user.email", "test@example.com"]);
    git(&main, ["config", "user.name", "Test User"]);
    std::fs::write(main.join("README.md"), "test\n").expect("readme");
    git(&main, ["add", "README.md"]);
    git(&main, ["commit", "-m", "Initial"]);
    git(
        &main,
        [
            "remote",
            "add",
            "origin",
            "ssh://git@github.com/team/project.git",
        ],
    );
    git(
        &main,
        [
            "worktree",
            "add",
            worktree.to_str().expect("UTF-8 worktree"),
        ],
    );

    let (success, output) = run_in_terminal(&worktree, &["init"], "\n\n\n\n\n");

    assert!(success, "{output}");
    let config =
        std::fs::read_to_string(worktree.join("sacho.toml")).expect("worktree configuration");
    assert!(
        config.contains("https://github.com/team/project/issues/{n}"),
        "{config}"
    );
}

#[test]
fn init_interactive_configures_selected_changelog_sections() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::create_dir_all(temp.path().join("packages/core")).expect("package directory");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Project changes\n===============\n\nVersion 2.0.0\n-------------\n\nTo be released.\n\n### core\n\n -  Added core.\n\nVersion 1.0.0\n-------------\n\nReleased on July 1, 2026.\n\n### core\n\n -  Shipped core.\n",
    )
    .expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["init"], "\n\n\n9\n1\n\n\n\n\n");

    assert!(success, "{output}");
    assert!(
        output.contains("section number 9 is outside 1..=1"),
        "{output}"
    );
    assert!(
        output.contains("1. core (2 occurrences, unreleased)"),
        "{output}"
    );
    let config = std::fs::read_to_string(temp.path().join("sacho.toml")).expect("configuration");
    assert!(config.contains("[[sections]]"), "{config}");
    assert!(config.contains("id = \"core\""), "{config}");
    assert!(config.contains("directory = \"core\""), "{config}");
    assert!(
        config.contains("paths = [\"packages/core/**\"]"),
        "{config}"
    );
}

#[test]
fn init_interactive_collapses_sibling_packages_into_a_section_pattern() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
    std::fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changes
===============

Version 2.0.0
-------------

To be released.

### @example/core

 -  Added core.

### @example/cli

 -  Added CLI.
",
    )
    .expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["init"], "\n\n\n1,2\n\n\n\n");

    assert!(success, "{output}");
    assert!(
        output.contains("Use inferred section pattern packages/{name} -> @example/{name}"),
        "{output}"
    );
    let config = std::fs::read_to_string(temp.path().join("sacho.toml")).expect("configuration");
    assert!(config.contains("[[section-patterns]]"), "{config}");
    assert!(config.contains("source = \"packages/{name}\""), "{config}");
    assert!(config.contains("id = \"@example/{name}\""), "{config}");
    assert!(config.contains("directory = \"{name}\""), "{config}");
    assert!(!config.contains("[[sections]]"), "{config}");
}

#[test]
fn init_interactive_keeps_sections_explicit_when_an_inferred_directory_is_unsafe() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
    std::fs::create_dir_all(temp.path().join("packages/next")).expect("next package");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changes
===============

Version 2.0.0
-------------

To be released.

### @example/core

 -  Added core.

### @example/next

 -  Added next.
",
    )
    .expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["init"], "\n\n\n1,2\n\n\n\n\n\n\n");

    assert!(success, "{output}");
    assert!(!output.contains("Use inferred section pattern"), "{output}");
    let config = std::fs::read_to_string(temp.path().join("sacho.toml")).expect("configuration");
    assert_eq!(config.matches("[[sections]]").count(), 2, "{config}");
    assert!(config.contains("id = \"@example/core\""), "{config}");
    assert!(config.contains("id = \"@example/next\""), "{config}");
    assert!(config.contains("directory = \"next-2\""), "{config}");
    assert!(!config.contains("[[section-patterns]]"), "{config}");
}

#[test]
fn init_interactive_can_decline_an_inferred_section_pattern() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    git(temp.path(), ["init"]);
    std::fs::create_dir_all(temp.path().join("packages/core")).expect("core package");
    std::fs::create_dir_all(temp.path().join("packages/cli")).expect("cli package");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changes
===============

Version 2.0.0
-------------

To be released.

### @example/core

 -  Added core.

### @example/cli

 -  Added CLI.
",
    )
    .expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["init"], "\n\n\n1,2\nn\n\n\n\n\n\n");

    assert!(success, "{output}");
    let config = std::fs::read_to_string(temp.path().join("sacho.toml")).expect("configuration");
    assert_eq!(config.matches("[[sections]]").count(), 2, "{config}");
    assert!(config.contains("id = \"@example/core\""), "{config}");
    assert!(config.contains("id = \"@example/cli\""), "{config}");
    assert!(!config.contains("[[section-patterns]]"), "{config}");
}

#[test]
fn init_discovers_a_markerless_git_worktree_from_the_environment() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let metadata_worktree = temp.path().join("metadata-worktree");
    let worktree = temp.path().join("selected-worktree");
    let nested = worktree.join("one/two");
    std::fs::create_dir_all(&nested).expect("nested worktree directory");
    git(
        temp.path(),
        ["init", metadata_worktree.to_str().expect("UTF-8 path")],
    );
    let git_dir = metadata_worktree.join(".git");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(&nested)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &worktree)
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert!(worktree.join("sacho.toml").is_file());
    assert!(!nested.join("sacho.toml").exists());
    assert!(
        std::fs::read_to_string(worktree.join("sacho.toml"))
            .expect("configuration")
            .contains("preset = \"git\"")
    );
}

#[test]
fn init_prefers_a_git_worktree_selected_by_the_environment_over_an_ambient_git_marker() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let ambient_worktree = temp.path().join("ambient-worktree");
    let selected_worktree = temp.path().join("selected-worktree");
    let nested = ambient_worktree.join("one/two");
    std::fs::create_dir_all(&nested).expect("nested ambient directory");
    std::fs::create_dir(&selected_worktree).expect("selected worktree directory");
    git(
        temp.path(),
        ["init", ambient_worktree.to_str().expect("UTF-8 path")],
    );
    git(
        temp.path(),
        ["init", selected_worktree.to_str().expect("UTF-8 path")],
    );
    let git_dir = selected_worktree.join(".git");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(&nested)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &selected_worktree)
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert!(selected_worktree.join("sacho.toml").is_file());
    assert!(!ambient_worktree.join("sacho.toml").exists());
}

#[test]
fn init_prefers_a_mercurial_marker_over_the_git_environment() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let metadata_worktree = temp.path().join("metadata-worktree");
    let selected_worktree = temp.path().join("selected-worktree");
    let hg_worktree = temp.path().join("hg-worktree");
    let nested = hg_worktree.join("one/two");
    std::fs::create_dir_all(&nested).expect("nested worktree directory");
    std::fs::create_dir(hg_worktree.join(".hg")).expect("Mercurial marker");
    std::fs::create_dir(&selected_worktree).expect("selected Git worktree");
    git(
        temp.path(),
        ["init", metadata_worktree.to_str().expect("UTF-8 path")],
    );
    let git_dir = metadata_worktree.join(".git");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(&nested)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &selected_worktree)
        .args(["init", "--no-interactive"])
        .assert()
        .success();

    assert!(hg_worktree.join("sacho.toml").is_file());
    assert!(!selected_worktree.join("sacho.toml").exists());
    assert!(
        std::fs::read_to_string(hg_worktree.join("sacho.toml"))
            .expect("configuration")
            .contains("preset = \"hg\"")
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
        format!(
            "#!/bin/sh\n# sacho pre-commit begin\n{} hook-pre-commit\n# sacho pre-commit end\n",
            env!("CARGO_BIN_EXE_sacho")
        )
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".git/hooks/commit-msg")).expect("commit hook"),
        format!(
            "#!/bin/sh\n# sacho commit-msg begin\n{} hook-commit-msg \"$1\"\n# sacho commit-msg end\n",
            env!("CARGO_BIN_EXE_sacho")
        )
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".git/hooks/reference-transaction"))
            .expect("reference hook"),
        format!(
            "#!/bin/sh\n# sacho reference-transaction begin\nif test \"$1\" = prepared\nthen\n    sacho_state=$(git rev-parse --git-path sacho-commit-state) || exit $?\n    if test -f \"$sacho_state\"\n    then\n        {} hook-reference-transaction \"$1\"\n    fi\nfi\n# sacho reference-transaction end\n",
            env!("CARGO_BIN_EXE_sacho")
        )
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
    assert!(temp.path().join(".githooks/commit-msg").is_file());
    assert!(
        temp.path()
            .join(".githooks/reference-transaction")
            .is_file()
    );
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
    assert!(main.join(".git/hooks/commit-msg").is_file());
    assert!(main.join(".git/hooks/reference-transaction").is_file());
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
fn preview_no_word_wrap_joins_soft_breaks_and_preserves_hard_breaks() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/no-word-wrap.md"),
        concat!(
            " -  Added a deliberately long release note that must stay on one physical line ",
            "when it is published through GitHub Releases.  ",
            "\n",
            "    This intentional hard break remains visible.\n",
        ),
    )
    .expect("fragment");
    let mut wrapped_command = Command::cargo_bin("sacho").expect("binary");
    let wrapped_output = wrapped_command
        .current_dir(temp.path())
        .arg("preview")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let wrapped_output = String::from_utf8(wrapped_output).expect("UTF-8 preview");
    assert!(
        wrapped_output.lines().all(|line| line.len() <= 80),
        "{wrapped_output}"
    );
    assert!(!wrapped_output.contains(
        " -  Added a deliberately long release note that must stay on one physical line when it is published through GitHub Releases."
    ));
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["preview", "--no-word-wrap"])
        .assert()
        .success()
        .stdout(concat!(
            "Unreleased\n",
            "----------\n",
            "\n",
            "To be released.\n",
            "\n",
            " -  Added a deliberately long release note that must stay on one physical line ",
            "when it is published through GitHub Releases.  ",
            "\n",
            "    This intentional hard break remains visible.\n",
        ));
}

#[test]
fn preview_uses_configured_link_resolution_without_writing_fragments() {
    let (base, request) = one_request_http_server();
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        format!(
            "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{base}/issues/{{n}}\"\n\n[link-resolution]\nenabled = true\n"
        ),
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    let path = temp.path().join("changes.d/change.md");
    let source = " -  Fixed preview links.  [[#1]]\n";
    std::fs::write(&path, source).expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .arg("preview")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("[#1]: {base}/issues/1")));

    assert_eq!(std::fs::read_to_string(path).expect("fragment"), source);
    assert_eq!(request.join().expect("server"), "HEAD /issues/1 HTTP/1.1");
}

#[test]
fn preview_no_resolve_links_overrides_enabled_configuration() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"http://127.0.0.1:1/issues/{n}\"\n\n[link-resolution]\nenabled = true\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/change.md"),
        " -  Fixed preview links.  [[#1]]\n",
    )
    .expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["preview", "--no-resolve-links"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[#1]: http://127.0.0.1:1/issues/1",
        ));
}

#[test]
fn sync_resolve_links_pins_fragments_without_materialization() {
    let (base, request) = one_request_http_server();
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        format!("[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{base}/issues/{{n}}\"\n"),
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    let path = temp.path().join("changes.d/change.md");
    std::fs::write(&path, " -  Fixed sync links.  [[#2]]\n").expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["sync", "--resolve-links"])
        .assert()
        .success()
        .stdout("changes.d/change.md\n");

    let fragment = std::fs::read_to_string(path).expect("fragment");
    assert!(fragment.contains("links:"));
    assert!(fragment.contains(&format!("{base}/issues/2")));
    assert_eq!(request.join().expect("server"), "HEAD /issues/2 HTTP/1.1");
}

#[test]
fn release_resolve_links_uses_the_resolving_plan() {
    let (base, requests) = redirecting_http_server();
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        format!("[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{base}/issues/{{n}}\"\n"),
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    let fragment = temp.path().join("changes.d/change.md");
    std::fs::write(&fragment, " -  Fixed release links.  [[#3]]\n").expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "release",
            "0.2.0",
            "--date",
            "2026-07-08",
            "--resolve-links",
        ])
        .assert()
        .success();

    assert!(!fragment.exists());
    assert!(
        std::fs::read_to_string(temp.path().join("CHANGES.md"))
            .expect("changelog")
            .contains(&format!("[#3]: {base}/pull/3"))
    );
    assert_eq!(
        requests.join().expect("server"),
        vec!["HEAD /issues/3 HTTP/1.1", "HEAD /pull/3 HTTP/1.1"]
    );
}

#[test]
fn resolve_links_command_pins_fragments_without_materialization() {
    let (base, request) = one_request_http_server();
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        format!("[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"{base}/issues/{{n}}\"\n"),
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    let path = temp.path().join("changes.d/change.md");
    std::fs::write(&path, " -  Fixed standalone links.  [[#4]]\n").expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .arg("resolve-links")
        .assert()
        .success()
        .stdout("changes.d/change.md\n");

    let fragment = std::fs::read_to_string(path).expect("fragment");
    assert!(fragment.contains("links:"));
    assert!(fragment.contains(&format!("{base}/issues/4")));
    assert_eq!(request.join().expect("server"), "HEAD /issues/4 HTTP/1.1");
}

#[test]
fn resolve_links_command_reports_the_materialized_changelog() {
    let (base, requests) = redirecting_http_server();
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        format!("[links]\n\"#\" = \"{base}/issues/{{n}}\"\n"),
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/change.md"),
        " -  Fixed reported links.  [[#5]]\n",
    )
    .expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        format!(
            "Unreleased\n----------\n\nTo be released.\n\n -  Fixed reported links.  [[#5]]\n\n[#5]: {base}/issues/5\n"
        ),
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .arg("resolve-links")
        .assert()
        .success()
        .stdout("changes.d/change.md\nCHANGES.md\n");

    assert_eq!(
        requests.join().expect("server"),
        vec!["HEAD /issues/5 HTTP/1.1", "HEAD /pull/3 HTTP/1.1"]
    );
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
        .stdout(predicate::str::contains("Section id to preview by itself"))
        .stdout(predicate::str::contains("--resolve-links"))
        .stdout(predicate::str::contains("--no-resolve-links"))
        .stdout(predicate::str::contains("--no-word-wrap"))
        .stdout(predicate::str::contains("Do not word-wrap the output"));
}

#[test]
fn resolve_links_help_describes_persistent_resolution() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["resolve-links", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Resolve and pin unpinned reference links",
        ));
}

#[test]
fn show_formats_the_released_section() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\npath = \"docs/NEWS.md\"\n",
    )
    .expect("config");
    std::fs::create_dir(temp.path().join("docs")).expect("docs directory");
    std::fs::write(
        temp.path().join("docs/NEWS.md"),
        "\
Project changelog
=================

Version 1.2.0
-------------

Released on July 19, 2026.

 -  Added show.

```markdown
Version 0.0.0
-------------
```

Version 1.1.0
-------------

Released on July 1, 2026.
",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "1.2.0"])
        .assert()
        .success()
        .stdout(
            "\
Version 1.2.0
-------------

Released on July 19, 2026.

 -  Added show.

<!-- end list -->

~~~~ markdown
Version 0.0.0
-------------
~~~~
",
        );
}

#[test]
fn show_can_skip_the_version_heading() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Version 1.2.0
-------------

Released on July 19, 2026.

 -  Added show.

Version 1.1.0
-------------

Released on July 1, 2026.
",
    )
    .expect("changelog");

    for flag in ["-H", "--skip-heading"] {
        let mut command = Command::cargo_bin("sacho").expect("binary");
        command
            .current_dir(temp.path())
            .args(["show", "1.2.0", flag])
            .assert()
            .success()
            .stdout(
                "\
Released on July 19, 2026.

 -  Added show.
",
            );
    }
}

#[test]
fn show_no_word_wrap_writes_unwrapped_release_notes_to_a_file() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        concat!(
            "Version 1.2.0\n",
            "-------------\n",
            "\n",
            "Released on July 19, 2026.\n",
            "\n",
            " -  Added a deliberately long release note that must stay on one physical line ",
            "when it is published through GitHub Releases.  ",
            "\n",
            "    This intentional hard break remains visible.\n",
        ),
    )
    .expect("changelog");
    let mut wrapped_command = Command::cargo_bin("sacho").expect("binary");
    let wrapped_output = wrapped_command
        .current_dir(temp.path())
        .args(["show", "1.2.0"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let wrapped_output = String::from_utf8(wrapped_output).expect("UTF-8 released section");
    assert!(
        wrapped_output.lines().all(|line| line.len() <= 80),
        "{wrapped_output}"
    );
    assert!(!wrapped_output.contains(
        " -  Added a deliberately long release note that must stay on one physical line when it is published through GitHub Releases."
    ));
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args([
            "show",
            "1.2.0",
            "--skip-heading",
            "--no-word-wrap",
            "--output-file",
            "release-notes.md",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    assert_eq!(
        std::fs::read_to_string(temp.path().join("release-notes.md")).expect("output"),
        concat!(
            "Released on July 19, 2026.\n",
            "\n",
            " -  Added a deliberately long release note that must stay on one physical line ",
            "when it is published through GitHub Releases.  ",
            "\n",
            "    This intentional hard break remains visible.\n",
        )
    );
}

#[test]
fn show_includes_reference_definitions_from_other_sections() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changelog
=================

Version 0.2.0
-------------

Released on July 18, 2026.

 -  It contains a [link].

[link]: https://example.com/


Version 0.1.0
-------------

Released on July 1, 2026.

 -  It also contains the same [link].
",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "0.1.0"])
        .assert()
        .success()
        .stdout(
            "\
Version 0.1.0
-------------

Released on July 1, 2026.

 -  It also contains the same [link].

[link]: https://example.com/
",
        );
}

#[test]
fn show_includes_footnote_definitions_from_other_sections() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Project changelog
=================

Version 0.2.0
-------------

Released on July 18, 2026.

[^note]: Shared footnote text.[^detail]

[^detail]: More detail.

Version 0.1.0
-------------

Released on July 1, 2026.

 -  It contains a footnote.[^note]
",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "0.1.0"])
        .assert()
        .success()
        .stdout(
            "\
Version 0.1.0
-------------

Released on July 1, 2026.

 -  It contains a footnote.[^note]

[^note]: Shared footnote text.[^detail]
[^detail]: More detail.
",
        );
}

#[test]
fn show_reports_a_missing_released_version() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Version 1.1.0\n-------------\n\nReleased on July 1, 2026.\n",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "1.2.0"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "released version \"1.2.0\" not found in changelog",
        ));
}

#[test]
fn show_rejects_the_materialized_unreleased_version() {
    let cases = [
        (
            "",
            "\
Version 1.2.0
-------------

To be released.

Version 1.1.0
-------------

Released on July 1, 2026.
",
        ),
        (
            "[changelog]\nregion-detection = \"marker\"\n",
            "\
<!-- sacho:unreleased:begin -->
Version 1.2.0
-------------

To be released.
<!-- sacho:unreleased:end -->

Version 1.1.0
-------------

Released on July 1, 2026.
",
        ),
    ];

    for (config, changelog) in cases {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(temp.path().join("sacho.toml"), config).expect("config");
        std::fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
        let mut command = Command::cargo_bin("sacho").expect("binary");

        command
            .current_dir(temp.path())
            .args(["show", "1.2.0"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "released version \"1.2.0\" not found in changelog",
            ));
    }
}

#[test]
fn show_prints_a_release_below_the_materialized_unreleased_region() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "\
Unreleased
----------

To be released.

Version 1.1.0
-------------

Released on July 1, 2026.
",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "1.1.0"])
        .assert()
        .success()
        .stdout(
            "\
Version 1.1.0
-------------

Released on July 1, 2026.
",
        );
}

#[test]
fn show_writes_the_released_section_to_an_output_file() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Version 1.2.0\n-------------\n\nReleased on July 19, 2026.\n",
    )
    .expect("changelog");
    std::fs::write(temp.path().join("release-notes.md"), "stale\n").expect("stale output");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "1.2.0", "-o", "release-notes.md"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    assert_eq!(
        std::fs::read_to_string(temp.path().join("release-notes.md")).expect("output"),
        "Version 1.2.0\n-------------\n\nReleased on July 19, 2026.\n"
    );
}

#[test]
fn show_reports_an_output_file_write_error() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Version 1.2.0\n-------------\n\nReleased on July 19, 2026.\n",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["show", "1.2.0", "--output-file", "missing/release-notes.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "failed to write missing/release-notes.md",
        ));
}

#[test]
fn show_help_describes_version_argument() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["show", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Print a released changelog section",
        ))
        .stdout(predicate::str::contains(
            "Released version whose section should be printed",
        ))
        .stdout(predicate::str::contains("-H, --skip-heading"))
        .stdout(predicate::str::contains("Do not print the version heading"))
        .stdout(predicate::str::contains("-o, --output-file <PATH>"))
        .stdout(predicate::str::contains(
            "Write the released section to a file",
        ))
        .stdout(predicate::str::contains("--no-word-wrap"))
        .stdout(predicate::str::contains("Do not word-wrap the output"));
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
fn check_rejects_an_unused_resolved_link_pin() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n\n[links]\n\"#\" = \"https://example.com/issues/{n}\"\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/fix.md"),
        "---\nlinks:\n  '#2': https://example.com/pull/2\n---\n -  Fixed thing.  [[#1]]\n",
    )
    .expect("fragment");

    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("resolved link label"))
        .stderr(predicate::str::contains("#2"))
        .stderr(predicate::str::contains("not used"));
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
fn import_unreleased_requires_force_non_interactively_then_imports() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir(temp.path().join("changes.d")).expect("fragment directory");
    let changelog = "Version 2.4.0\n-------------\n\nTo be released.\n\n- Added import support.\n\nVersion 2.3.0\n-------------\n\nReleased on July 1, 2026.\n\n -  Previous release.\n";
    std::fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");

    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(temp.path())
        .arg("import-unreleased")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "rerun with `sacho import-unreleased --force`",
        ));

    assert!(
        !temp
            .path()
            .join("changes.d/imported-unreleased.md")
            .exists()
    );
    assert!(!temp.path().join("changes.d/next").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("unchanged changelog"),
        changelog
    );

    let mut command = Command::cargo_bin("sacho").expect("binary");
    command
        .current_dir(temp.path())
        .args(["import-unreleased", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("changes.d/imported-unreleased.md"));

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/next")).expect("next"),
        "2.4.0\n"
    );
    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success();
}

#[test]
fn import_unreleased_in_terminal_applies_after_yes_confirmation() {
    let temp = import_unreleased_confirmation_repository();

    let (success, output) = run_in_terminal(temp.path(), &["import-unreleased"], "yes\n");

    assert!(success, "{output}");
    assert!(output.contains("normalize the materialized"), "{output}");
    assert!(output.contains("[y/N]"), "{output}");
    assert!(
        temp.path()
            .join("changes.d/imported-unreleased.md")
            .exists()
    );
}

#[test]
fn import_unreleased_in_terminal_keeps_repository_after_default_no() {
    let temp = import_unreleased_confirmation_repository();
    let original = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["import-unreleased"], "\n");

    assert!(!success, "{output}");
    assert!(output.contains("cancelled"), "{output}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        original
    );
    assert!(
        !temp
            .path()
            .join("changes.d/imported-unreleased.md")
            .exists()
    );
}

#[test]
fn sync_in_terminal_applies_after_yes_confirmation() {
    let temp = sync_confirmation_repository();

    let (success, output) = run_in_terminal(temp.path(), &["sync"], "yes\n");

    assert!(success, "{output}");
    assert!(output.contains("may discard hand edits"), "{output}");
    assert!(output.contains("[y/N]"), "{output}");
    assert!(
        std::fs::read_to_string(temp.path().join("CHANGES.md"))
            .expect("changelog")
            .contains(" -  Fixed sync.\n")
    );
}

#[cfg(unix)]
#[test]
fn sync_in_terminal_shows_safety_context_when_stderr_is_redirected() {
    let temp = sync_confirmation_repository();
    let stderr_path = temp.path().join("sync.log");
    let mut command = CommandBuilder::new("/bin/sh");
    command.cwd(temp.path());
    command.args(["-c", "exec \"$SACHO_BIN\" sync 2>\"$SACHO_STDERR\""]);
    command.env("SACHO_BIN", env!("CARGO_BIN_EXE_sacho"));
    command.env("SACHO_STDERR", &stderr_path);

    let (success, output) = run_command_in_terminal(command, "yes\n");

    assert!(success, "{output}");
    assert!(output.contains("--- current"), "{output}");
    assert!(output.contains("may discard hand edits"), "{output}");
    assert!(output.contains("[y/N]"), "{output}");
    assert_eq!(
        std::fs::read_to_string(stderr_path).expect("redirected stderr"),
        ""
    );
}

#[test]
fn sync_in_terminal_keeps_changelog_after_default_no_confirmation() {
    let temp = sync_confirmation_repository();
    let original = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["sync"], "\n");

    assert!(!success, "{output}");
    assert!(output.contains("cancelled"), "{output}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        original
    );
}

#[test]
fn sync_non_terminal_retry_preserves_explicit_link_resolution_policy() {
    for (flag, enabled, expected, unexpected) in [
        (
            "--resolve-links",
            false,
            "sacho sync --resolve-links --force",
            "sacho sync --no-resolve-links --force",
        ),
        (
            "--no-resolve-links",
            true,
            "sacho sync --no-resolve-links --force",
            "sacho sync --resolve-links --force",
        ),
    ] {
        let temp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            temp.path().join("sacho.toml"),
            format!(
                "[links]\n\"#\" = \"https://example.com/issues/{{n}}\"\n\n[link-resolution]\nenabled = {enabled}\n"
            ),
        )
        .expect("config");
        std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
        std::fs::write(
            temp.path().join("changes.d/fix.md"),
            "---\nlinks:\n  '#1': https://example.com/pull/1\n---\n -  Fixed sync.  [[#1]]\n",
        )
        .expect("fragment");
        std::fs::write(
            temp.path().join("CHANGES.md"),
            "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n",
        )
        .expect("changelog");

        Command::cargo_bin("sacho")
            .expect("binary")
            .current_dir(temp.path())
            .args(["sync", flag])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(expected))
            .stderr(predicate::str::contains(unexpected).not());
    }
}

#[test]
fn check_fix_non_terminal_refuses_risky_sync_without_formatting() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
    let original = "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n";
    std::fs::write(temp.path().join("CHANGES.md"), original).expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["check", "--fix"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("sacho sync --force"))
        .stderr(predicate::str::contains("sacho check --fix"));

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
        "- Fixed issue.\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        original
    );

    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .args(["sync", "--force"])
        .assert()
        .success();
    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .args(["check", "--fix"])
        .assert()
        .success();
    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
        " -  Fixed issue.\n"
    );
}

#[test]
fn fmt_non_terminal_refuses_risky_sync_without_formatting() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
    let changelog = "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n";
    std::fs::write(temp.path().join("CHANGES.md"), changelog).expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .arg("fmt")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("sacho sync --force"))
        .stderr(predicate::str::contains("sacho fmt"));

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
        "- Fixed issue.\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        changelog
    );

    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .args(["sync", "--force"])
        .assert()
        .success();
    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .arg("fmt")
        .assert()
        .success();
    Command::cargo_bin("sacho")
        .expect("binary")
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
        " -  Fixed issue.\n"
    );
}

#[test]
fn fmt_non_terminal_applies_formatting_when_materialized_output_is_current() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/fmt.md"), "- Fixed issue.\n").expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n\n -  Fixed issue.\n",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .arg("fmt")
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fmt.md")).expect("fragment"),
        " -  Fixed issue.\n"
    );
}

#[test]
fn fmt_in_terminal_applies_formatting_and_sync_after_confirmation() {
    let temp = sync_confirmation_repository();
    std::fs::write(temp.path().join("changes.d/sync.md"), "- Fixed sync.\n").expect("fragment");

    let (success, output) = run_in_terminal(temp.path(), &["fmt"], "yes\n");

    assert!(success, "{output}");
    assert!(output.contains("may discard hand edits"), "{output}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/sync.md")).expect("fragment"),
        " -  Fixed sync.\n"
    );
    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Fixed sync.\n"));
    assert!(!changelog.contains("Hand-edited note."));
}

#[test]
fn fmt_in_terminal_default_no_keeps_fragments_and_changelog() {
    let temp = sync_confirmation_repository();
    std::fs::write(temp.path().join("changes.d/sync.md"), "- Fixed sync.\n").expect("fragment");
    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["fmt"], "\n");

    assert!(!success, "{output}");
    assert!(output.contains("cancelled"), "{output}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/sync.md")).expect("fragment"),
        "- Fixed sync.\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        changelog
    );
}

#[test]
fn check_fix_in_terminal_applies_risky_sync_after_confirmation() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n",
    )
    .expect("changelog");

    let (success, output) = run_in_terminal(temp.path(), &["check", "--fix"], "y\n");

    assert!(success, "{output}");
    assert!(output.contains("may discard hand edits"), "{output}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
        " -  Fixed issue.\n"
    );
    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains(" -  Fixed issue.\n"));
    assert!(!changelog.contains("Hand-edited note."));
}

#[test]
fn check_fix_non_terminal_automatically_applies_formatting_only_fix() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/fix.md"), "- Fixed issue.\n").expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["check", "--fix"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join("changes.d/fix.md")).expect("fragment"),
        " -  Fixed issue.\n"
    );
}

fn sync_confirmation_repository() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/sync.md"), " -  Fixed sync.\n").expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n\nHand-edited note.\n",
    )
    .expect("changelog");
    temp
}

fn import_unreleased_confirmation_repository() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Unreleased\n----------\n\nTo be released.\n\n- Added import support.\n",
    )
    .expect("changelog");
    temp
}

fn run_in_terminal(dir: &std::path::Path, args: &[&str], input: &str) -> (bool, String) {
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sacho"));
    command.cwd(dir);
    command.args(args);
    run_command_in_terminal(command, input)
}

fn run_command_in_terminal(command: CommandBuilder, input: &str) -> (bool, String) {
    static PTY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _pty_guard = PTY_LOCK.lock().expect("PTY test lock");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pseudo-terminal");
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("spawn in terminal");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("terminal reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).expect("terminal output");
        output
    });
    let mut writer = pair.master.take_writer().expect("terminal writer");
    writer.write_all(input.as_bytes()).expect("terminal input");
    writer.flush().expect("flush terminal input");
    drop(writer);
    let status = child.wait().expect("terminal child");
    let output = reader_thread.join().expect("terminal reader thread");
    (status.success(), output)
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
fn merge_driver_honors_the_repository_mutation_lock() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "[vcs]\npreset = \"none\"\n").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/merge.md"),
        " -  Concurrent fragment.\n",
    )
    .expect("fragment");
    let source = "Unreleased\n----------\n\nTo be released.\n";
    let ancestor = temp.path().join("ancestor.md");
    let current = temp.path().join("current.md");
    let other = temp.path().join("other.md");
    std::fs::write(&ancestor, source).expect("ancestor");
    std::fs::write(&current, source).expect("current");
    std::fs::write(&other, source).expect("other");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temp.path().join(".sacho.lock"))
        .expect("mutation lock");
    lock.lock().expect("hold mutation lock");
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
        .stderr(predicate::str::contains(
            "another Sacho mutation is already running",
        ));
    assert_eq!(
        std::fs::read_to_string(current).expect("current remains readable"),
        source
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

#[cfg(unix)]
#[test]
fn installed_pre_commit_hook_enforces_staged_missing_fragments() {
    let temp = initialized_hook_repository();

    std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
    std::fs::write(temp.path().join("src/lib.rs"), "pub fn changed() {}\n").expect("source");
    git(temp.path(), ["add", "src/lib.rs"]);
    let missing = git_with_sacho(temp.path(), ["commit", "-m", "Change API"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("missing changelog fragment"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    std::fs::write(
        temp.path().join("changes.d/changed.md"),
        " -  Changed public behavior.\n",
    )
    .expect("fragment");
    assert!(
        !git_with_sacho(temp.path(), ["commit", "-m", "Change API"])
            .status
            .success()
    );

    git(temp.path(), ["add", "changes.d/changed.md"]);
    std::fs::write(
        temp.path().join("src/unstaged.rs"),
        "pub fn unrelated() {}\n",
    )
    .expect("unstaged source");
    let covered = git_with_sacho(temp.path(), ["commit", "-m", "Change API"]);
    assert!(
        covered.status.success(),
        "{}",
        String::from_utf8_lossy(&covered.stderr)
    );

    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn changed() { todo!() }\n",
    )
    .expect("amended source");
    git(temp.path(), ["add", "src/lib.rs"]);
    let amended = git_with_sacho(temp.path(), ["commit", "--amend", "--no-edit"]);
    assert!(
        amended.status.success(),
        "{}",
        String::from_utf8_lossy(&amended.stderr)
    );

    std::fs::write(temp.path().join("src/lib.rs"), "pub fn changed() {}\n")
        .expect("new source change");
    git(temp.path(), ["add", "src/lib.rs"]);
    assert!(
        !git_with_sacho(temp.path(), ["commit", "-m", "Change API again"])
            .status
            .success()
    );
    let exempt = git_with_sacho(
        temp.path(),
        ["commit", "-m", "Refactor API\n\nChangelog: none"],
    );
    assert!(
        exempt.status.success(),
        "{}",
        String::from_utf8_lossy(&exempt.stderr)
    );
}

#[cfg(unix)]
#[test]
fn installed_hooks_ignore_incoming_changes_in_a_clean_merge() {
    let temp = initialized_hook_repository();
    let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);
    git(temp.path(), ["switch", "-c", "feature"]);
    std::fs::create_dir_all(temp.path().join("src")).expect("source dir");
    std::fs::write(
        temp.path().join("src/incoming.rs"),
        "pub fn incoming() {}\n",
    )
    .expect("incoming source");
    git(temp.path(), ["add", "src/incoming.rs"]);
    assert!(
        git_with_sacho(
            temp.path(),
            ["commit", "-m", "Incoming API\n\nChangelog: none"],
        )
        .status
        .success()
    );

    git(temp.path(), ["switch", initial_branch.trim()]);
    std::fs::write(temp.path().join("README.md"), "main branch\n").expect("readme");
    git(temp.path(), ["add", "README.md"]);
    assert!(
        git_with_sacho(temp.path(), ["commit", "-m", "Update readme"])
            .status
            .success()
    );

    let merged = git_with_sacho(
        temp.path(),
        ["merge", "--no-ff", "feature", "-m", "Merge feature"],
    );

    assert!(
        merged.status.success(),
        "{}",
        String::from_utf8_lossy(&merged.stderr)
    );
}

#[cfg(unix)]
#[test]
fn installed_hooks_do_not_use_an_incoming_fragment_for_merge_resolution() {
    let temp = initialized_hook_repository();
    std::fs::create_dir_all(temp.path().join("src")).expect("source dir");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 0 }\n",
    )
    .expect("initial source");
    git(temp.path(), ["add", "src/lib.rs"]);
    assert!(
        git_with_sacho(
            temp.path(),
            ["commit", "-m", "Initial API\n\nChangelog: none"],
        )
        .status
        .success()
    );
    let initial_branch = git_output(temp.path(), ["branch", "--show-current"]);

    git(temp.path(), ["switch", "-c", "feature"]);
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("feature source");
    std::fs::write(
        temp.path().join("changes.d/feature.md"),
        " -  Changed the value API.\n",
    )
    .expect("feature fragment");
    git(temp.path(), ["add", "src/lib.rs", "changes.d/feature.md"]);
    assert!(
        git_with_sacho(temp.path(), ["commit", "-m", "Change value API"])
            .status
            .success()
    );

    git(temp.path(), ["switch", initial_branch.trim()]);
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\n",
    )
    .expect("main source");
    git(temp.path(), ["add", "src/lib.rs"]);
    assert!(
        git_with_sacho(
            temp.path(),
            ["commit", "-m", "Adjust value API\n\nChangelog: none"],
        )
        .status
        .success()
    );
    let merge = git_with_sacho(temp.path(), ["merge", "feature"]);
    assert!(!merge.status.success(), "merge should conflict");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 3 }\n",
    )
    .expect("merge resolution");
    git(temp.path(), ["add", "src/lib.rs"]);

    let commit = git_with_sacho(temp.path(), ["commit", "-m", "Resolve value API"]);

    assert!(!commit.status.success());
    assert!(
        String::from_utf8_lossy(&commit.stderr).contains("missing changelog fragment"),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

#[cfg(unix)]
#[test]
fn installed_hooks_do_not_treat_reused_head_message_as_amend() {
    let temp = initialized_hook_repository();
    std::fs::create_dir_all(temp.path().join("src")).expect("source dir");
    std::fs::write(temp.path().join("src/lib.rs"), "pub fn first() {}\n").expect("source");
    std::fs::write(
        temp.path().join("changes.d/first.md"),
        " -  Added the first API.\n",
    )
    .expect("fragment");
    git(temp.path(), ["add", "src/lib.rs", "changes.d/first.md"]);
    assert!(
        git_with_sacho(temp.path(), ["commit", "-m", "Add first API"])
            .status
            .success()
    );
    std::fs::write(temp.path().join("src/second.rs"), "pub fn second() {}\n")
        .expect("second source");
    git(temp.path(), ["add", "src/second.rs"]);

    let reused = git_with_sacho(temp.path(), ["commit", "-C", "HEAD"]);

    assert!(!reused.status.success());
    assert!(
        String::from_utf8_lossy(&reused.stderr).contains("missing changelog fragment"),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
}

#[cfg(unix)]
#[test]
fn installed_hooks_ignore_escape_markers_in_verbose_diff() {
    use std::os::unix::fs::PermissionsExt;

    let temp = initialized_hook_repository();
    std::fs::create_dir_all(temp.path().join("src")).expect("source dir");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "// [changelog skip]\npub fn changed() {}\n",
    )
    .expect("source");
    std::fs::write(
        temp.path().join("message.txt"),
        "# Commit message template\n",
    )
    .expect("template");
    let editor = temp.path().join("editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\ntmp=\"$1.sacho-editor\"\n{ printf 'Change API\\n'; cat \"$1\"; } > \"$tmp\"\nmv \"$tmp\" \"$1\"\n",
    )
    .expect("editor");
    let mut permissions = std::fs::metadata(&editor)
        .expect("editor metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&editor, permissions).expect("editor permissions");
    git(temp.path(), ["add", "src/lib.rs"]);

    let commit = git_with_sacho_env(
        temp.path(),
        ["commit", "-v", "-t", "message.txt"],
        [("GIT_EDITOR", editor.to_str().expect("editor path"))],
    );

    assert!(!commit.status.success());
    assert!(
        String::from_utf8_lossy(&commit.stderr).contains("missing changelog fragment"),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

#[cfg(unix)]
#[test]
fn installed_hooks_accept_configured_non_utf8_commit_encoding() {
    let temp = initialized_hook_repository();
    git(temp.path(), ["config", "i18n.commitEncoding", "ISO-8859-1"]);
    std::fs::create_dir_all(temp.path().join("src")).expect("source dir");
    std::fs::write(temp.path().join("src/lib.rs"), "pub fn cafe() {}\n").expect("source");
    std::fs::write(
        temp.path().join("changes.d/cafe.md"),
        " -  Added the cafe API.\n",
    )
    .expect("fragment");
    std::fs::write(
        temp.path().join("message.bin"),
        b"Add the caf\xe9 API\n" as &[u8],
    )
    .expect("encoded message");
    git(temp.path(), ["add", "src/lib.rs", "changes.d/cafe.md"]);

    let commit = git_with_sacho(temp.path(), ["commit", "-F", "message.bin"]);

    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    assert!(
        git_output(temp.path(), ["cat-file", "commit", "HEAD"]).contains("encoding ISO-8859-1")
    );
}

#[cfg(unix)]
#[test]
fn reference_transaction_hook_ignores_non_commit_ref_updates_without_sacho() {
    let temp = initialized_hook_repository();
    let output = ProcessCommand::new("git")
        .current_dir(temp.path())
        .args(["branch", "other"])
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("git branch");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn initialized_hook_repository() -> tempfile::TempDir {
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
    let mut init = Command::cargo_bin("sacho").expect("binary");
    init.current_dir(temp.path())
        .args(["init", "--no-interactive", "--install-hook"])
        .assert()
        .success();
    assert!(temp.path().join(".git/hooks/commit-msg").is_file());
    assert!(
        temp.path()
            .join(".git/hooks/reference-transaction")
            .is_file()
    );
    git(temp.path(), ["add", "."]);
    let initial = git_with_sacho(temp.path(), ["commit", "-m", "Initial config"]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    temp
}

#[cfg(unix)]
fn git_with_sacho<const N: usize>(dir: &std::path::Path, args: [&str; N]) -> std::process::Output {
    git_with_sacho_env(dir, args, [] as [(&str, &str); 0])
}

#[cfg(unix)]
fn git_with_sacho_env<const N: usize, const E: usize>(
    dir: &std::path::Path,
    args: [&str; N],
    env: [(&str, &str); E],
) -> std::process::Output {
    let mut command = ProcessCommand::new("git");
    command.current_dir(dir).args(args).envs(env);
    add_sacho_to_path(&mut command);
    command.output().expect("git command with Sacho hooks")
}

#[cfg(unix)]
fn add_sacho_to_path(command: &mut ProcessCommand) {
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_sacho"));
    let mut paths = vec![binary.parent().expect("binary directory").to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    command.env("PATH", std::env::join_paths(paths).expect("PATH"));
}

#[cfg(not(unix))]
fn add_sacho_to_path(_command: &mut ProcessCommand) {}

fn git<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
    let mut command = ProcessCommand::new("git");
    command.current_dir(dir).args(args);
    add_sacho_to_path(&mut command);
    let output = command.output().expect("git command");
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

fn git_output<const N: usize>(dir: &std::path::Path, args: [&str; N]) -> String {
    let mut command = ProcessCommand::new("git");
    command.current_dir(dir).args(args);
    add_sacho_to_path(&mut command);
    let output = command.output().expect("git command");
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
        .env("TZ", "Invalid/Timezone")
        .args(["release", "0.2.0", "--date", "2026-07-08"])
        .assert()
        .success();

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.contains("Version 0.2.0\n-------------"));
    assert!(changelog.contains("Released on July 8, 2026."));
    assert!(!temp.path().join("changes.d/release.md").exists());
}

#[test]
fn release_command_closes_materialized_region_until_next_cycle_starts() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(temp.path().join("sacho.toml"), "").expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(temp.path().join("changes.d/next"), "0.2.0\n").expect("next");
    std::fs::write(
        temp.path().join("changes.d/release.md"),
        " -  Fixed release.\n",
    )
    .expect("fragment");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Changelog\n=========\n\nVersion 0.2.0\n-------------\n\nTo be released.\n\n -  Fixed release.\n",
    )
    .expect("changelog");

    let mut release = Command::cargo_bin("sacho").expect("binary");
    release
        .current_dir(temp.path())
        .args(["release", "--date", "2026-07-08"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        "Changelog\n=========\n\nVersion 0.2.0\n-------------\n\nReleased on July 8, 2026.\n\n -  Fixed release.\n"
    );
    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success();

    let mut next = Command::cargo_bin("sacho").expect("binary");
    next.current_dir(temp.path())
        .args(["next", "0.3.0"])
        .assert()
        .success();

    let changelog = std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog");
    assert!(changelog.starts_with(
        "Changelog\n=========\n\nVersion 0.3.0\n-------------\n\nTo be released.\n\n\nVersion 0.2.0"
    ));
    let mut check = Command::cargo_bin("sacho").expect("binary");
    check
        .current_dir(temp.path())
        .arg("check")
        .assert()
        .success();
}

#[test]
fn release_command_allows_an_intentional_empty_release() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("CHANGES.md"),
        "Changelog\n=========\n\nVersion 0.1.0\n-------------\n\nReleased on July 1, 2026.\n",
    )
    .expect("changelog");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["release", "0.2.0", "--date", "2026-07-08", "--allow-empty"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(temp.path().join("CHANGES.md")).expect("changelog"),
        "Changelog\n=========\n\nVersion 0.2.0\n-------------\n\nReleased on July 8, 2026.\n\n\nVersion 0.1.0\n-------------\n\nReleased on July 1, 2026.\n"
    );
}

#[test]
fn release_help_describes_allow_empty() {
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .args(["release", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--allow-empty"))
        .stdout(predicate::str::contains(
            "Allow a release without changelog items",
        ))
        .stdout(predicate::str::contains("--resolve-links"))
        .stdout(predicate::str::contains("--no-resolve-links"));
}

#[test]
fn release_command_does_not_fall_back_to_utc_when_local_timezone_is_invalid() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/release.md"),
        " -  Fixed release.\n",
    )
    .expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .env("TZ", "Invalid/Timezone")
        .args(["release", "0.2.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not determine the local calendar date",
        ))
        .stderr(predicate::str::contains("--date YYYY-MM-DD"));

    assert!(temp.path().join("changes.d/release.md").exists());
    assert!(!temp.path().join("CHANGES.md").exists());
}

#[test]
fn release_command_creates_a_missing_changelog_from_the_relative_repository_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .expect("config");
    std::fs::create_dir_all(temp.path().join("changes.d")).expect("fragments dir");
    std::fs::write(
        temp.path().join("changes.d/release.md"),
        " -  Fixed release.\n",
    )
    .expect("fragment");
    let mut command = Command::cargo_bin("sacho").expect("binary");

    command
        .current_dir(temp.path())
        .args(["release", "0.2.0", "--date", "2026-07-08"])
        .assert()
        .success();

    assert!(temp.path().join("CHANGES.md").is_file());
    assert!(!temp.path().join("changes.d/release.md").exists());
}
