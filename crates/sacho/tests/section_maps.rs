//! Historical section mapping contracts across library, command, and CLI paths.

use std::{fs, path::PathBuf};

use assert_cmd::Command;
use proptest::prelude::*;
use sacho::commands::{CarryOptions, carry};
use sacho::released::{carry_release, carry_release_with_options, import_unreleased_region};
use sacho::{CarryReleaseOptions, Config, Repository, RuntimeSectionMap};
use tempfile::TempDir;

const SECTIONS: &str = "[[sections]]\nid='a'\ntitle='Current'\ndirectory='a'\n[[sections]]\nid='b'\ntitle='Other'\ndirectory='b'\n";

fn make_repo(config: &str) -> (TempDir, Repository) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("sacho.toml"), config).unwrap();
    let repo = Repository::from_root(temp.path()).unwrap();
    (temp, repo)
}

fn literal(from: &str, to: &str) -> RuntimeSectionMap {
    RuntimeSectionMap::Literal {
        from: from.into(),
        to: to.into(),
    }
}

fn pattern(from: &str, to: &str) -> RuntimeSectionMap {
    RuntimeSectionMap::Pattern {
        from: from.into(),
        to: to.into(),
    }
}

fn document(title: &str, marker: Option<&str>) -> String {
    format!(
        "## Version 1.0.0\n\n### {title}\n\n{} -  Fixed.\n",
        marker
            .map(|id| format!("<!-- sacho:section:v1 id={id} -->\n\n"))
            .unwrap_or_default()
    )
}

fn selected(
    config: &str,
    title: &str,
    id: Option<&str>,
    overrides: Vec<RuntimeSectionMap>,
) -> String {
    let (_temp, repo) = make_repo(config);
    carry_release_with_options(
        &repo,
        &document(title, id),
        "1.0.0",
        &CarryReleaseOptions {
            runtime_maps: overrides,
        },
    )
    .unwrap()
    .fragments[0]
        .section
        .clone()
        .unwrap()
}

#[test]
fn config_validates_kind_unknown_fields_domains_and_literal_targets_on_load() {
    for rule in [
        "[[section-maps]]\nfrom='Old'\nto='a'\n",
        "[[section-maps]]\nkind='title'\npatern=true\nfrom='Old'\nto='a'\n",
        "[[section-maps]]\nkind='id'\nfrom='Old'\nto='missing'\n",
        "[[section-maps]]\nkind='title'\nfrom=' Old'\nto='a'\n",
    ] {
        assert!(
            Config::parse(&format!("{SECTIONS}{rule}")).is_err(),
            "{rule}"
        );
    }
    assert!(
        Config::parse("[[section-maps]]\nkind='id'\nfrom='Old'\nto='a'\n")
            .unwrap_err()
            .to_string()
            .contains("maps require sections")
    );
    let config = format!(
        "{SECTIONS}[[section-maps]]\nkind='title'\nfrom='Old'\nto='a'\n[[section-maps]]\nkind='id'\nfrom='Old'\nto='b'\n"
    );
    let config = Config::parse(&config).unwrap();
    assert_eq!(
        Config::parse(&toml::to_string(&config).unwrap()).unwrap(),
        config
    );
    assert!(!config.section_maps[0].pattern);
}

#[test]
fn marked_and_legacy_inputs_follow_distinct_precedence_layers() {
    let idmap = "[[section-maps]]\nkind='id'\nfrom='legacy'\nto='a'\n";
    assert_eq!(
        selected(
            &format!("{SECTIONS}{idmap}"),
            "Other",
            Some("legacy"),
            vec![]
        ),
        "a"
    );
    assert_eq!(
        selected(
            &format!("{SECTIONS}{idmap}"),
            "Other",
            Some("legacy"),
            vec![literal("legacy", "b")]
        ),
        "b"
    );
    assert_eq!(
        selected(SECTIONS, "Current", Some("a"), vec![literal("a", "b")]),
        "b"
    );
    let current_override = "[[section-maps]]\nkind='id'\nfrom='a'\nto='b'\n";
    assert_eq!(
        selected(
            &format!("{SECTIONS}{current_override}"),
            "Current",
            Some("a"),
            vec![]
        ),
        "b"
    );
    let literal_title = "[[section-maps]]\nkind='title'\nfrom='Other'\nto='a'\n";
    assert_eq!(
        selected(&format!("{SECTIONS}{literal_title}"), "Other", None, vec![]),
        "a"
    );
    let title_pattern = "[[section-maps]]\nkind='title'\npattern=true\nfrom='{name}'\nto='b'\n";
    assert_eq!(
        selected(
            &format!("{SECTIONS}{title_pattern}"),
            "Current",
            None,
            vec![]
        ),
        "a"
    );
    let legacy_id = "[[section-maps]]\nkind='id'\nfrom='History'\nto='a'\n";
    assert_eq!(
        selected(
            &format!("{SECTIONS}{title_pattern}{legacy_id}"),
            "History",
            None,
            vec![]
        ),
        "b"
    );
    assert_eq!(
        selected(&format!("{SECTIONS}{current_override}"), "a", None, vec![]),
        "b"
    );
    assert_eq!(selected(SECTIONS, "a", None, vec![]), "a");
    assert_eq!(
        selected(
            &format!("{SECTIONS}{literal_title}"),
            "Other",
            None,
            vec![literal("Other", "b")]
        ),
        "b"
    );
}

#[test]
fn current_pattern_title_is_preferred_to_raw_id_but_omitted_title_allows_id_maps() {
    let config = "[[section-patterns]]\nsource='crates/{name}'\nid='{name}'\ntitle='{name} (Rust)'\ndirectory='{name}'\n";
    assert_eq!(selected(config, "core (Rust)", None, vec![]), "core");
    let config = "[[section-patterns]]\nsource='crates/{name}'\nid='{name}'\ndirectory='{name}'\n[[section-maps]]\nkind='id'\nfrom='old'\nto='new'\n";
    assert_eq!(selected(config, "old", None, vec![]), "new");
}

#[test]
fn opaque_marker_ids_and_cli_values_are_not_trimmed_or_templated() {
    assert_eq!(
        selected(
            SECTIONS,
            "Other",
            Some("%20key%0A%20"),
            vec![literal(" key\n ", "a")]
        ),
        "a"
    );
    assert_eq!(
        selected(
            SECTIONS,
            "Old = -> {name}",
            None,
            vec![literal("Old = -> {name}", "a")]
        ),
        "a"
    );
    let (_temp, repo) = make_repo(SECTIONS);
    let error = carry_release_with_options(
        &repo,
        &document("Other", None),
        "1.0.0",
        &CarryReleaseOptions {
            runtime_maps: vec![literal(" Other ", "a")],
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unused") && error.contains("Other"));
}

#[test]
fn empty_h3_and_shadowed_pattern_usage_are_counted_without_resolving_hidden_targets() {
    let (_temp, repo) = make_repo(SECTIONS);
    let source = "## Version 1.0.0\n\n### Empty\n\n### Other\n\n -  Fixed.\n";
    let result = carry_release_with_options(
        &repo,
        source,
        "1.0.0",
        &CarryReleaseOptions {
            runtime_maps: vec![
                literal("Empty", "a"),
                pattern("{name}", "missing"),
                literal("Other", "b"),
            ],
        },
    )
    .unwrap();
    assert_eq!(result.fragments[0].section.as_deref(), Some("b"));
    assert!(
        carry_release_with_options(
            &repo,
            &document("", None),
            "1.0.0",
            &CarryReleaseOptions {
                runtime_maps: vec![pattern("{name}", "a")]
            }
        )
        .unwrap_err()
        .to_string()
        .contains("empty")
    );
}

#[test]
fn selected_missing_or_conflicting_targets_and_marker_damage_never_fall_back() {
    let (_temp, repo) = make_repo(SECTIONS);
    for (source, maps, text) in [
        (document("Current", Some("missing")), vec![], "missing"),
        (
            document("Current", None),
            vec![literal("Unused", "missing")],
            "missing",
        ),
        (
            document("Current", None),
            vec![pattern("{name}", "missing")],
            "missing",
        ),
        (
            document("Current", None),
            vec![pattern("{name}", "a"), pattern("C{name}", "b")],
            "ambiguously",
        ),
        (
            document("Current", Some("bad/token")),
            vec![literal("bad/token", "a")],
            "unencoded",
        ),
    ] {
        let error = carry_release_with_options(
            &repo,
            &source,
            "1.0.0",
            &CarryReleaseOptions { runtime_maps: maps },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(text), "{error}");
    }
    let config = format!(
        "{SECTIONS}[[section-maps]]\nkind='title'\npattern=true\nfrom='{{name}}'\nto='missing'\n"
    );
    assert_eq!(selected(&config, "Current", None, vec![]), "a");
}

#[test]
fn historical_maps_are_carry_only_and_unknown_recorded_ids_do_not_use_titles() {
    let config = format!(
        "{SECTIONS}[[section-maps]]\nkind='title'\nfrom='Old'\nto='a'\n[[section-maps]]\nkind='id'\nfrom='legacy'\nto='a'\n"
    );
    let (_temp, repo) = make_repo(&config);
    assert!(carry_release(&repo, &document("Old", None), "1.0.0").is_ok());
    assert!(import_unreleased_region(&repo, "## Unreleased\n\n### Old\n\n -  Fixed.\n").is_err());
    assert!(
        import_unreleased_region(
            &repo,
            "## Unreleased\n\n### Current\n\n<!-- sacho:section:v1 id=legacy -->\n\n -  Fixed.\n"
        )
        .is_err()
    );
    assert!(carry_release(&repo, &document("Current", Some("missing")), "1.0.0").is_err());
    let (_temp, repo) = make_repo("");
    assert!(
        carry_release_with_options(
            &repo,
            &document("Old", None),
            "1.0.0",
            &CarryReleaseOptions {
                runtime_maps: vec![literal("Old", "a")]
            }
        )
        .unwrap_err()
        .to_string()
        .contains("maps require sections")
    );
}

#[test]
fn command_carry_merges_mapping_inputs_in_order_and_is_idempotent_for_both_modes() {
    for materialize in [false, true] {
        let (temp, repo) = make_repo(&format!(
            "[changelog]\nmaterialize={materialize}\n{SECTIONS}"
        ));
        let source = "Changelog\n=========\n\nVersion 1.0.0\n-------------\n\n### Old A\n\n -  First.\n\n### Old B\n\n -  Second.\n";
        fs::write(temp.path().join("CHANGES.md"), source).unwrap();
        let options = || CarryOptions {
            version: "1.0.0".into(),
            runtime_maps: vec![pattern("Old {name}", "a")],
        };
        let result = carry(&repo, options()).unwrap();
        assert_eq!(
            result.written_fragments,
            [PathBuf::from("changes.d/a/carried-from-1.0.0.md")]
        );
        let fragment = fs::read_to_string(temp.path().join(&result.written_fragments[0])).unwrap();
        assert!(fragment.find("First.").unwrap() < fragment.find("Second.").unwrap());
        let changelog = fs::read_to_string(temp.path().join("CHANGES.md")).unwrap();
        assert!(changelog.ends_with(source.split_once("Version 1.0.0").unwrap().1));
        carry(&repo, options()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
            changelog
        );
        assert_eq!(
            fs::read_to_string(temp.path().join(&result.written_fragments[0])).unwrap(),
            fragment
        );
    }
}

#[test]
fn nonmaterialized_title_collisions_fail_before_writes_without_parsing_existing_bodies() {
    for collision in [false, true] {
        let title = if collision { "Shared" } else { "{name} API" };
        let config = format!(
            "[changelog]\nmaterialize=false\n[[section-patterns]]\nsource='packages/{{name}}'\nid='{{name}}'\ntitle='{title}'\ndirectory='{{name}}'\n"
        );
        let (temp, repo) = make_repo(&config);
        fs::create_dir_all(temp.path().join("changes.d/b")).unwrap();
        let malformed = "not a fragment\n";
        fs::write(temp.path().join("changes.d/b/existing.md"), malformed).unwrap();
        let source = document("Old", None);
        fs::write(temp.path().join("CHANGES.md"), &source).unwrap();
        let result = carry(
            &repo,
            CarryOptions {
                version: "1.0.0".into(),
                runtime_maps: vec![literal("Old", "a")],
            },
        );
        if collision {
            assert!(result.unwrap_err().to_string().contains("shared"));
            assert!(
                !temp
                    .path()
                    .join("changes.d/a/carried-from-1.0.0.md")
                    .exists()
            );
        } else {
            result.unwrap();
        }
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b/existing.md")).unwrap(),
            malformed
        );
    }
}

#[test]
fn cli_accepts_repeated_pair_options_braces_arrows_and_leading_hyphens() {
    let sections = SECTIONS.replace("id='a'", "id='-a'");
    let (temp, _repo) = make_repo(&format!("[changelog]\nmaterialize=false\n{sections}"));
    fs::write(temp.path().join("CHANGES.md"), "## Version 1.0.0\n\n### -Old = -> {name}\n\n -  First.\n\n### Family/core\n\n -  Second.\n\n### Empty\n").unwrap();
    Command::new(env!("CARGO_BIN_EXE_sacho"))
        .current_dir(temp.path())
        .args([
            "carry",
            "1.0.0",
            "--map",
            "-Old = -> {name}",
            "-a",
            "--map",
            "Empty",
            "-a",
            "--map-pattern",
            "Family/{name}",
            "b",
            "--map-pattern",
            "E{name}",
            "-a",
        ])
        .assert()
        .success();
    assert!(
        temp.path()
            .join("changes.d/a/carried-from-1.0.0.md")
            .is_file()
    );
    assert!(
        temp.path()
            .join("changes.d/b/carried-from-1.0.0.md")
            .is_file()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn many_historical_headings_collapse_in_source_order(names in prop::collection::vec("[a-z]{1,12}", 1..12)) {
        let (_temp, repo) = make_repo(SECTIONS);
        let mut source = "## Version 1.0.0\n\n".to_owned();
        for (index, name) in names.iter().enumerate() {
            source.push_str(&format!("### Old {name}\n\n -  Entry {index}.\n\n"));
        }
        let result = carry_release_with_options(&repo, &source, "1.0.0", &CarryReleaseOptions { runtime_maps: vec![pattern("Old {name}", "a")] }).unwrap();
        prop_assert_eq!(result.fragments.len(), 1);
        let markdown = &result.fragments[0].markdown;
        let mut previous = 0;
        for index in 0..names.len() {
            let position = markdown.find(&format!("Entry {index}.")).unwrap();
            prop_assert!(position >= previous);
            previous = position;
        }
    }
}

#[test]
fn current_title_ambiguity_and_shadowed_mapping_targets_do_not_fall_through() {
    let config = format!(
        "{SECTIONS}[[section-patterns]]\nsource='rust/{{name}}'\nid='rust/{{name}}'\ntitle='{{name}} API'\ndirectory='rust/{{name}}'\n[[section-patterns]]\nsource='js/{{name}}'\nid='js/{{name}}'\ntitle='{{name}} API'\ndirectory='js/{{name}}'\n[[section-maps]]\nkind='id'\nfrom='core API'\nto='a'\n"
    );
    let (_temp, repo) = make_repo(&config);
    assert!(
        carry_release(&repo, &document("core API", None), "1.0.0")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    let config = "[[sections]]\nid='override'\ndirectory='taken'\n[[section-patterns]]\nsource='packages/{name}'\nid='pkg/{name}'\ndirectory='{name}'\n";
    let (_temp, repo) = make_repo(config);
    let error = carry_release_with_options(
        &repo,
        &document("override", None),
        "1.0.0",
        &CarryReleaseOptions {
            runtime_maps: vec![pattern("{name}", "pkg/taken")],
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("shadowed"), "{error}");
}

#[test]
fn mapping_and_reference_failures_leave_archived_bytes_and_existing_fragments_untouched() {
    let config = format!(
        "[changelog]\nmaterialize=false\n[links]\n'#'='https://example.com/issues/{{n}}'\n{SECTIONS}"
    );
    let (temp, repo) = make_repo(&config);
    fs::create_dir_all(temp.path().join("changes.d/b")).unwrap();
    let existing = "---\nlinks:\n  '#1': https://example.com/issues/1\n---\n -  Existing. [[#1]]\n";
    fs::write(temp.path().join("changes.d/b/existing.md"), existing).unwrap();
    let cases = [
        (document("Current", None), vec![literal("Unused", "a")]),
        (document("Current", None), vec![pattern("{name}", "a"), pattern("C{name}", "b")]),
        (document("Current", Some("bad/token")), vec![literal("bad/token", "a")]),
        ("## Version 1.0.0\n\n### First\n\n -  First. [[#1](https://example.com/pull/1)]\n\n### Second\n\n -  Second. [[#1](https://example.com/pull/1)]\n\n[#1]: https://example.com/pull/1\n".into(), vec![literal("First", "a"), literal("Second", "a")]),
    ];
    for (source, runtime_maps) in cases {
        fs::write(temp.path().join("CHANGES.md"), &source).unwrap();
        assert!(
            carry(
                &repo,
                CarryOptions {
                    version: "1.0.0".into(),
                    runtime_maps
                }
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("changes.d/b/existing.md")).unwrap(),
            existing
        );
        assert!(
            !temp
                .path()
                .join("changes.d/a/carried-from-1.0.0.md")
                .exists()
        );
        assert!(
            !temp
                .path()
                .join("changes.d/b/carried-from-1.0.0.md")
                .exists()
        );
    }
}

#[cfg(unix)]
#[test]
fn mapped_pattern_destination_symlink_is_rejected_before_writing() {
    use std::os::unix::fs::symlink;
    let (temp, repo) = make_repo(
        "[changelog]\nmaterialize=false\n[[section-patterns]]\nsource='packages/{name}'\nid='{name}'\ndirectory='{name}'\n",
    );
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("changes.d")).unwrap();
    symlink(outside.path(), temp.path().join("changes.d/a")).unwrap();
    let source = document("Old", None);
    fs::write(temp.path().join("CHANGES.md"), &source).unwrap();
    assert!(
        carry(
            &repo,
            CarryOptions {
                version: "1.0.0".into(),
                runtime_maps: vec![literal("Old", "a")]
            }
        )
        .is_err()
    );
    assert!(!outside.path().join("carried-from-1.0.0.md").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
        source
    );
}

#[test]
fn mapped_canonical_sections_cannot_share_a_fragment_destination() {
    let config = "[changelog]\nmaterialize=false\n[[section-patterns]]\nsource='rust/{name}'\nid='rust/{name}'\ndirectory='{name}'\n[[section-patterns]]\nsource='js/{name}'\nid='js/{name}'\ndirectory='{name}'\n";
    let (temp, repo) = make_repo(config);
    let source = "## Version 1.0.0\n\n### Old Rust\n\n -  First.\n\n### Old JS\n\n -  Second.\n";
    fs::write(temp.path().join("CHANGES.md"), source).unwrap();
    let error = carry(
        &repo,
        CarryOptions {
            version: "1.0.0".into(),
            runtime_maps: vec![
                literal("Old Rust", "rust/core"),
                literal("Old JS", "js/core"),
            ],
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ambiguous"), "{error}");
    assert!(
        !temp
            .path()
            .join("changes.d/core/carried-from-1.0.0.md")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("CHANGES.md")).unwrap(),
        source
    );
}

#[test]
fn mapped_targets_preserve_valid_opaque_ids_instead_of_applying_title_rules() {
    let config = "[[sections]]\nid=\" key\\n \"\ntitle='API'\ndirectory='core'\n";
    assert_eq!(
        selected(config, "Old", None, vec![literal("Old", " key\n ")]),
        " key\n "
    );
    let config = "[[section-patterns]]\nsource='packages/{name}'\nid=\" key\\n{name} \"\ntitle='{name} API'\ndirectory='{name}'\n";
    assert_eq!(
        selected(
            config,
            "old/core",
            None,
            vec![pattern("old/{name}", " key\n{name} ")]
        ),
        " key\ncore "
    );
}
