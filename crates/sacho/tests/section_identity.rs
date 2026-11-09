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
