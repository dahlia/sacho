//! Archived release regression fixtures from Sacho issue #11.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use sacho::commands::{CarryOptions, carry};
use sacho::fragment::parse_fragment;
use sacho::released::{carry_release, find_released_section};
use sacho::{Error, Repository};

const ARCHIVE: &str = include_str!("fixtures/section-identity/release.txt");
const LEGACY_CONFIG: &str = include_str!("fixtures/section-identity/legacy.toml");
const RENAMED_CONFIG: &str = include_str!("fixtures/section-identity/renamed.toml");

fn repository(config: &str) -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("sacho.toml"), config).unwrap();
    fs::write(temp.path().join("CHANGES.md"), ARCHIVE).unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    (temp, repo)
}

#[test]
fn legacy_archive_keeps_observable_priority_order_and_resolved_links() {
    let (_temp, repo) = repository(LEGACY_CONFIG);
    // Source directories are deliberately absent: historical carry uses IDs.
    let first = carry_release(&repo, ARCHIVE, "1.0.0").unwrap();
    let second = carry_release(&repo, ARCHIVE, "1.0.0").unwrap();
    assert_eq!(first.fragments.len(), 2);
    assert_eq!(first, second);
    for (fragment, id, directory, topic) in [
        (
            &first.fragments[0],
            "gukhanmun",
            "crates/gukhanmun",
            "crates/gukhanmun",
        ),
        (
            &first.fragments[1],
            "@gukhanmun/napi",
            "packages/napi",
            "packages/napi",
        ),
    ] {
        assert_eq!(fragment.section.as_deref(), Some(id));
        assert_eq!(
            fragment.path,
            PathBuf::from(format!("changes.d/{directory}/carried-from-1.0.0.md"))
        );
        let first_item = format!("Added {topic} first.");
        let second_item = format!("Added {topic} second.");
        assert_eq!(fragment.markdown.matches(&first_item).count(), 1);
        assert_eq!(fragment.markdown.matches(&second_item).count(), 1);
        assert!(
            fragment.markdown.find(&first_item).unwrap()
                < fragment.markdown.find(&second_item).unwrap()
        );
        let parsed = parse_fragment(
            fragment.path.clone(),
            &fragment.markdown,
            fragment.section.clone(),
            &repo.config().links,
        )
        .unwrap();
        assert_eq!(
            parsed.links,
            BTreeMap::from([
                (
                    "#47".into(),
                    "https://github.com/example/project/issues/47".into()
                ),
                (
                    "#54".into(),
                    "https://github.com/dahlia/gukhanmun/pull/54".into()
                ),
            ])
        );
        assert_eq!(parsed.items.len(), 2);
        for item in &parsed.items {
            assert_eq!(
                item.references
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>(),
                ["#47", "#54"]
            );
        }
    }
    assert_eq!(
        find_released_section(ARCHIVE, "1.0.0", None)
            .unwrap()
            .markdown,
        ARCHIVE
    );
}

#[test]
fn unmapped_renamed_archive_fails_before_writing_any_fragment() {
    for (config, unknown) in [
        (RENAMED_CONFIG.to_owned(), "gukhanmun"),
        (
            RENAMED_CONFIG.replace("id = \"{name} (Rust)\"", "id = \"{name}\""),
            "@gukhanmun/napi",
        ),
    ] {
        let (temp, repo) = repository(&config);
        for _ in 0..2 {
            let error = carry(
                &repo,
                CarryOptions {
                    runtime_maps: Vec::new(),
                    version: "1.0.0".into(),
                },
            )
            .unwrap_err();
            assert!(matches!(error, Error::UnknownSection { section } if section == unknown));
            assert_eq!(
                fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
                ARCHIVE
            );
            assert!(!temp.path().join("changes.d").exists());
        }
    }
}

#[test]
fn markerless_old_branch_carries_after_family_title_and_directory_changes() {
    use sacho::compile::{CompileOptions, compile_unreleased};

    let config = family_title_config();
    let (temp, repo) = repository(&config);
    let carried = carry_release(&repo, ARCHIVE, "1.0.0").unwrap();
    for fragment in &carried.fragments {
        let path = temp.path().join(&fragment.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &fragment.markdown).unwrap();
    }
    assert_eq!(
        carried
            .fragments
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>(),
        [
            PathBuf::from("changes.d/rust/gukhanmun/carried-from-1.0.0.md"),
            PathBuf::from("changes.d/javascript/napi/carried-from-1.0.0.md"),
        ]
    );
    let python = temp.path().join("changes.d/python/gukhanmun/new.md");
    fs::create_dir_all(python.parent().unwrap()).unwrap();
    fs::write(python, " -  Added the Python binding.\n").unwrap();
    let compiled = compile_unreleased(&repo, CompileOptions::default()).unwrap();
    assert!(compiled.markdown.contains("### gukhanmun (Rust)"));
    assert!(compiled.markdown.contains("### gukhanmun (Python)"));
    assert!(
        compiled
            .markdown
            .contains("<!-- sacho:section:v1 id=python%2Fgukhanmun -->")
    );
    assert_eq!(compiled.sections.len(), 3);
    assert!(
        compiled
            .markdown
            .contains("<!-- sacho:section:v1 id=gukhanmun -->")
    );
    assert!(
        compiled
            .markdown
            .contains("<!-- sacho:section:v1 id=%40gukhanmun%2Fnapi -->")
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
        ARCHIVE
    );
}

fn family_title_config() -> String {
    LEGACY_CONFIG
        .replace(
            "id = \"{name}\"",
            "id = \"{name}\"\ntitle = \"{name} (Rust)\"",
        )
        .replace(
            "directory = \"crates/{name}\"",
            "directory = \"rust/{name}\"",
        )
        .replace(
            "id = \"@gukhanmun/{name}\"",
            "id = \"@gukhanmun/{name}\"\ntitle = \"@gukhanmun/{name} (JavaScript/TypeScript)\"",
        )
        .replace(
            "directory = \"packages/{name}\"",
            "directory = \"javascript/{name}\"",
        )
        + "\n[[section-patterns]]\nsource = \"python/{name}\"\nid = \"python/{name}\"\ntitle = \"{name} (Python)\"\ndirectory = \"python/{name}\"\n"
}

#[test]
fn format_and_merge_preserve_frozen_identity_and_recompile_current_titles() {
    use sacho::commands::{FormatOptions, format_fragments};
    use sacho::merge::{MergeDriverOptions, MergeDriverResult, merge_driver};

    let (temp, repo) = repository(&family_title_config());
    let recent = "Version 1.1.0\n-------------\n\n### Before rename\n\n<!-- sacho:section:v1 id=gukhanmun -->\n\n -  Previous title.\n\n\n";
    let initial = format!(
        "Changelog\n=========\n\n<!-- sacho:unreleased:begin -->\n\n<!-- sacho:unreleased:end -->\n\n{recent}{ARCHIVE}"
    );
    let path = temp.path().join("CHANGES.md");
    fs::write(&path, &initial).unwrap();
    let frozen = ["1.1.0", "1.0.0"].map(|version| {
        find_released_section(&initial, version, None)
            .unwrap()
            .markdown
    });
    carry(
        &repo,
        CarryOptions {
            version: "1.0.0".into(),
            runtime_maps: Vec::new(),
        },
    )
    .unwrap();
    let fragment_path = temp
        .path()
        .join("changes.d/rust/gukhanmun/carried-from-1.0.0.md");
    let fragment = fs::read_to_string(&fragment_path).unwrap();
    let noncanonical = fragment.replace(
        " -  Added crates/gukhanmun first.",
        "- Added crates/gukhanmun first.",
    );
    assert_ne!(fragment, noncanonical);
    fs::write(&fragment_path, noncanonical).unwrap();
    let formatted = format_fragments(&repo, FormatOptions).unwrap();
    assert!(!formatted.changed.is_empty());
    let source = fs::read_to_string(&path).unwrap();
    let inputs = ["ancestor.md", "current.md", "other.md"].map(|name| {
        let path = temp.path().join(name);
        fs::write(&path, &source).unwrap();
        path
    });
    let [ancestor, current, other] = inputs;
    let result = merge_driver(
        &repo,
        MergeDriverOptions {
            ancestor,
            current,
            other,
            path: "CHANGES.md".into(),
        },
    )
    .unwrap();
    let MergeDriverResult::Clean { output, hints } = result else {
        panic!("identity-preserving merge should be clean")
    };
    assert!(hints.is_empty());
    for (version, original) in ["1.1.0", "1.0.0"].into_iter().zip(frozen) {
        assert_eq!(
            find_released_section(&source, version, None)
                .unwrap()
                .markdown,
            original
        );
        assert_eq!(
            find_released_section(&output, version, None)
                .unwrap()
                .markdown,
            original
        );
    }
    assert!(output.contains("### gukhanmun (Rust)\n\n<!-- sacho:section:v1 id=gukhanmun -->"));
    assert!(output.contains("<!-- sacho:section:v1 id=%40gukhanmun%2Fnapi -->"));
    let carried = carry_release(&repo, &output, "1.1.0").unwrap();
    assert_eq!(carried.fragments.len(), 1);
    assert_eq!(carried.fragments[0].section.as_deref(), Some("gukhanmun"));
    assert!(carried.fragments[0].markdown.contains("Previous title."));
    assert!(!carried.fragments[0].markdown.contains("sacho:section:"));
}
