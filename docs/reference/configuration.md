Configuration reference
=======================

Sacho reads *sacho.toml* from the repository root. All fields use kebab-case.
Missing tables and fields take the defaults shown below.


`[changelog]`
-------------

~~~~ toml
[changelog]
path = "CHANGES.md"
title = "Changelog"
unreleased-heading = "To be released."
materialize = true
region-detection = "heading"
~~~~

| Field                | Default             | Meaning                                                               |
| -------------------- | ------------------- | --------------------------------------------------------------------- |
| `path`               | `"CHANGES.md"`      | Changelog path relative to the repository root.                       |
| `title`              | `"Changelog"`       | Top-level title used when Sacho creates a changelog.                  |
| `unreleased-heading` | `"To be released."` | Date-line text that identifies an unreleased heading region.          |
| `materialize`        | `true`              | Keep the compiled unreleased region in the changelog.                 |
| `region-detection`   | `"heading"`         | Find the generated region by `heading` or explicit `marker` comments. |

Marker detection expects exactly one pair around the generated region:

~~~~ markdown
<!-- sacho:unreleased:begin -->


Unreleased
----------

To be released.

<!-- sacho:unreleased:end -->
~~~~

Use markers when surrounding Markdown makes heading detection ambiguous.


`[fragments]`
-------------

~~~~ toml
[fragments]
directory = "changes.d"
next-file = "next"
~~~~

`directory` is relative to the repository root. `next-file` is relative to that
directory and must not have a *.md* extension, because Markdown files are
discovered as fragments.


`[links]`
---------

~~~~ toml
[links]
"#" = "https://github.com/acme/widget/issues/{n}"
"GH-" = "https://github.com/acme/widget/issues/{n}"
~~~~

Each key is a shortcut-reference sigil. Each value is a URL template and must
contain `{n}`, which Sacho replaces with the reference number.

A fragment using `[[#842]]` can then compile a reference definition for issue
842. Links without a matching sigil fail `sacho check`.


`[link-resolution]`
-------------------

~~~~ toml
[link-resolution]
enabled = false
~~~~

When enabled, `preview`, `sync`, and `release` resolve unpinned reference URLs
through HTTP redirects by default. The command-line `--resolve-links` and
`--no-resolve-links` options override this setting. `check`, hooks, and `fmt`
never make network requests, and unpinned references remain valid.

Link templates and their redirect targets are trusted repository inputs.
Resolution can access private network addresses so that repositories can use
self-hosted trackers. Do not enable it for untrusted repository configuration
in a privileged network environment. HTTPS redirects may not downgrade to
HTTP.

Resolved URLs are stored in fragment frontmatter. Existing pins are
authoritative, and every pin key must name a configured reference used by that
fragment. Remove a pin before resolving again when its destination needs to be
refreshed.


`[vcs]`                                                                   {#vcs}
--------------------------------------------------------------------------------

~~~~ toml
[vcs]
preset = "git"
~~~~

`preset` accepts `git`, `jj`, `hg`, or `none`. The configuration default is
`git`; `sacho init` detects the repository and can choose another preset.
`none` disables VCS-backed coverage checks while leaving fragment and changelog
validation enabled.

### Command overrides

Override one or more preset queries under `[vcs.commands]`:

~~~~ toml
[vcs.commands]
commits = ["my-vcs", "commits", "${base}"]
changed-paths = ["my-vcs", "changed-paths", "${commit}"]
message = ["my-vcs", "message", "${commit}"]
~~~~

The first array element is the executable. Remaining elements are literal
arguments. Sacho executes the array directly without a shell and expands
placeholders only in arguments.

The query contracts are:

 -  `commits` emits zero or more nonempty UTF-8 identifiers, oldest first and
    one per line. LF and CRLF line endings are accepted.
 -  `changed-paths` emits NUL-terminated name-status records. Ordinary records
    are `A\0PATH\0`, `D\0PATH\0`, `M\0PATH\0`, or `T\0PATH\0`. Copy and rename
    records are `C[SIMILARITY]\0OLD_PATH\0NEW_PATH\0` and
    `R[SIMILARITY]\0OLD_PATH\0NEW_PATH\0`, with an optional decimal similarity
    from 0 through 100. Nonempty output ends in NUL.
 -  `message` emits the complete UTF-8 commit message without trimming.

The `commits` query requires `${base}`. The other two require `${commit}`.
Overrides are not allowed with the `none` preset.


`[check]`
---------

~~~~ toml
[check]
paths = ["src/**", "packages/**"]
~~~~

`paths` is an array of glob patterns. A changed path matching any pattern
requires a new or meaningfully changed fragment when running a VCS-backed
check. The default is an empty array, which disables repository-wide coverage
requirements.


`[[sections]]`
--------------

~~~~ toml
[[sections]]
id = "core"
directory = "core"
paths = ["packages/core/**"]

[[sections]]
id = "cli"
directory = "cli"
paths = ["packages/cli/**"]
~~~~

| Field       | Required | Meaning                                                     |
| ----------- | -------- | ----------------------------------------------------------- |
| `id`        | Yes      | Stable section identifier accepted by `--section`.          |
| `title`     | No       | Plain output heading; omission retains the ID heading.      |
| `directory` | Yes      | Fragment subdirectory, relative to `[fragments].directory`. |
| `paths`     | No       | Globs whose changes require a fragment in this section.     |

Section ids and normalized directories must be unique. The array order controls
the rendered section order.


`[[section-patterns]]`
----------------------

~~~~ toml
[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "{name}"
paths = ["packages/{name}/src/**"]
~~~~

Section patterns derive any number of sections from a regular repository
layout. `source` captures values from a repository subtree. `id` and
`directory` reuse those captures for the stable identifier and fragment
subdirectory. Optional `title` renders a separate plain output heading. Omit
`paths` to attribute the complete source subtree, or set it to a list of
source-prefixed glob templates. An explicit empty list disables
section-specific attribution.

See [*Section pattern syntax*](./section-patterns) for the grammar, field
constraints, ordering, and precedence rules.
[*Package monorepos*](../guide/package-monorepos) shows complete configurations
for common layouts.


`[[section-maps]]`
------------------

Carry maps retain historical names while keeping the current ID stable:

~~~~ toml
[[section-maps]]
kind = "title"
pattern = true
from = "{name} (Rust crate)"
to = "{name}"

[[section-maps]]
kind = "id"
from = "old-core"
to = "core"
~~~~

`kind` is required and is either `title` or `id`. `pattern` defaults to `false`;
in that form, both values are literal, including braces. With `pattern = true`,
`from` uses the reversible section-pattern grammar and must contain at least one
capture, each used once. `to` can substitute those captures, repeat them, or
omit them to merge several historical sections into one current section.
Undeclared captures and unknown fields such as `patern` are errors.

A target resolves directly through the current ID resolver. Targets are never
titles or another map's source. Literal targets are checked at configuration
load, so remove or update their history rules when deleting a target section.
Pattern targets are checked when a selected rule matches concrete input.

Title sources must be valid plain titles. Literal ID sources and targets use
opaque ID spelling and are never trimmed. Maps require configured sections and
apply only to `carry`; they do not create aliases for `--section` or affect
`import-unreleased`. See the [carry reference](./commands#sacho-carry) for
precedence and one-time overrides.


Path rules
----------

Configured paths must be relative to their anchor. Empty paths, absolute paths,
and paths that escape their anchor with `..` are rejected. Sacho normalizes `.`
and internal parent components before checking path uniqueness.


Rust API migration in 0.4
-------------------------

The section identity changes add fields to public structs. Update struct
literals when upgrading the library:

| Type                                  | Added field                            | Legacy-compatible value |
| ------------------------------------- | -------------------------------------- | ----------------------- |
| `Config`                              | `section_maps: Vec<SectionMapConfig>`  | `Vec::new()`            |
| `SectionConfig`                       | `title: Option<String>`                | `None`                  |
| `SectionPatternConfig`                | `title: Option<SectionTitleTemplate>`  | `None`                  |
| `commands::CarryOptions`              | `runtime_maps: Vec<RuntimeSectionMap>` | `Vec::new()`            |
| `compile::CompiledSection`            | `title: Option<String>`                | `None`                  |
| `released::ChangelogSectionCandidate` | `title: String`                        | Observed plain heading  |

Public error enums also gain variants: `Error::SectionMapping` and
`Error::SectionMetadata`; `ConfigError::InvalidSectionMaps`,
`ConfigError::InvalidSectionTitle` and `ConfigError::DuplicateSectionTitle`;
and `FragmentError::ReservedSectionMetadata`. Exhaustive matches need new arms
or a wildcard arm when upgrading.

`CompiledSection.id` remains the canonical ID. Its optional `title` records an
explicit display title; when present, compiled Markdown also contains identity
metadata. A sectionless compiled region has neither ID nor title. Discovery
candidates now retain a recorded ID independently of the observed heading.
`commands::infer_section_pattern()` accepts `(String, String)` ID/title pairs
instead of an ID list; inconsistent titles keep explicit sections rather than
being replaced with an inferred ID template.

`SectionTitleTemplate` implements parsing, display, and string serde. Use
`render()` with capture values for plain output and `reversible_pattern()` to
check whether a template supports automatic recognition. Preserve `None` versus
`Some` when serializing configuration: an explicit title equal to the ID still
requests identity metadata.

`released::carry_release()` remains the default-options wrapper. Use
`released::carry_release_with_options()` and `CarryReleaseOptions.runtime_maps`
for one-time library overrides. `RuntimeSectionMap::Literal { from, to }` and
`RuntimeSectionMap::Pattern { from, to }` share the CLI evaluator; the default
runtime list is empty. Configuration maps use `SectionMapConfig` with
`SectionMapKind::Title` or `SectionMapKind::Id`.
