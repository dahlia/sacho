use std::fs;

use sacho::Repository;
use sacho::commands::{ReleaseDate, ReleaseOptions, apply_release, plan_release};

#[test]
fn unmatched_intro_cannot_shadow_a_later_release() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    let source = "# Changelog\n\n## Version 2.0.0\n\nReleased today.\n\n<!-- sacho:release-intro:begin -->\n\n### Version 1.0.0\n\n -  Introduction example.\n\n## Version 1.0.0\n\nReleased yesterday.\n\n -  Actual release item.\n";
    let carried = sacho::released::carry_release(&repo, source, "1.0.0").unwrap();
    assert_eq!(carried.fragments.len(), 1);
    assert!(
        carried.fragments[0]
            .markdown
            .contains("Actual release item.")
    );
    assert!(
        !carried.fragments[0]
            .markdown
            .contains("Introduction example.")
    );
    fs::write(temp.path().join("CHANGES.md"), source).unwrap();
    let shown = sacho::commands::show(
        &repo,
        sacho::commands::ShowOptions {
            version: "1.0.0".into(),
            skip_heading: false,
            word_wrap: false,
        },
    )
    .unwrap();
    assert!(shown.markdown.contains("Actual release item."));
    assert!(!shown.markdown.contains("Introduction example."));
    assert!(matches!(
        sacho::released::carry_release(&repo, source, "2.0.0"),
        Err(sacho::Error::InvalidReleaseContext { .. })
    ));
}

#[test]
fn introduction_line_endings_preserve_the_released_context() {
    let mut expected = None;
    for ending in ["\n", "\r", "\r\n"] {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("sacho.toml"),
            "[changelog]\nmaterialize = false\n",
        )
        .unwrap();
        fs::create_dir(temp.path().join("changes.d")).unwrap();
        let repo = Repository::from_root(temp.path()).unwrap();
        let intro = [
            "First paragraph.",
            "",
            "### Details",
            "",
            " -  Introduction item.",
            "",
            "> Read this.",
            "",
        ]
        .join(ending);
        let plan = plan_release(
            &repo,
            ReleaseOptions {
                version: Some("1.0.0".into()),
                date: ReleaseDate::parse("2026-10-13").unwrap(),
                next: None,
                allow_empty: false,
                note: None,
                intro: Some(intro),
            },
        )
        .unwrap();
        apply_release(&repo, plan).unwrap();
        let output = fs::read_to_string(temp.path().join("CHANGES.md")).unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&output, expected);
        } else {
            expected = Some(output);
        }
    }
}

#[test]
fn release_context_is_captured_before_its_input_file_disappears() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .unwrap();
    fs::create_dir(temp.path().join("changes.d")).unwrap();
    let input = temp.path().join("intro.md");
    fs::write(&input, "Read the captured message.\n").unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    let plan = plan_release(
        &repo,
        ReleaseOptions {
            version: Some("1.0.0".into()),
            date: ReleaseDate::parse("2026-10-13").unwrap(),
            next: None,
            allow_empty: false,
            note: Some("Initial release.".into()),
            intro: Some(fs::read_to_string(&input).unwrap()),
        },
    )
    .unwrap();
    fs::remove_file(input).unwrap();
    apply_release(&repo, plan).unwrap();
    let output = fs::read_to_string(temp.path().join("CHANGES.md")).unwrap();
    assert!(output.contains("Read the captured message."));
    assert!(output.contains("Initial release."));
}

#[test]
fn library_release_context_validation_precedes_repository_writes() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = false\n",
    )
    .unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    let error = plan_release(
        &repo,
        ReleaseOptions {
            version: Some("1.0.0".into()),
            date: ReleaseDate::parse("2026-10-13").unwrap(),
            next: None,
            allow_empty: false,
            note: Some("first\nsecond".into()),
            intro: None,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("--note"));
    assert!(!temp.path().join("CHANGES.md").exists());
}

#[test]
fn merge_preserves_an_introduction_with_version_and_fence_examples() {
    use sacho::merge::{MergeDriverOptions, MergeDriverResult, merge_driver};
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("sacho.toml"),
        "[changelog]\nmaterialize = true\n",
    )
    .unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    let old = "Changelog\n=========\n\nVersion 0.9.0\n-------------\n\nReleased before.\n\n -  Old change.\n";
    let intro = "Version 1.0.0\n-------------\n\nReleased on October 13, 2026.  Initial release.\n\n<!-- sacho:release-intro:begin -->\n\n### Version 8.0\n\n<!--\n```\n-->\n\n<!-- sacho:release-intro:end -->\n\n -  New change.\n";
    let other = format!(
        "Changelog\n=========\n\n{intro}\n\n{}",
        &old["Changelog\n=========\n\n".len()..]
    );
    let ancestor_path = temp.path().join("ancestor.md");
    let current_path = temp.path().join("current.md");
    let other_path = temp.path().join("other.md");
    fs::write(&ancestor_path, old).unwrap();
    fs::write(&current_path, old).unwrap();
    fs::write(&other_path, other).unwrap();
    let result = merge_driver(
        &repo,
        MergeDriverOptions {
            ancestor: ancestor_path,
            current: current_path,
            other: other_path,
            path: "CHANGES.md".into(),
        },
    )
    .unwrap();
    let MergeDriverResult::Clean { output, .. } = result else {
        panic!("expected clean merge")
    };
    assert!(output.contains(intro));
    assert!(
        output
            .trim_end()
            .ends_with(old["Changelog\n=========\n\n".len()..].trim_end())
    );
}
