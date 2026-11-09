Sections
========

A section divides one release into named groups. It is useful when a repository
ships several packages or when users need stable component headings inside each
release.

A single-package repository usually does not need sections. Its fragments live
directly in *changes.d/*, and their entries appear directly under the version
heading.


Configuring sections
--------------------

Sections are ordered arrays in *sacho.toml*:

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

The order in the configuration is the order in the compiled changelog. Each
section needs a unique `id` and fragment subdirectory.

`id` is the stable key for grouping, ordering, and `--section`. An optional
`title` supplies the plain heading shown to readers. `directory` is relative to
the configured fragment directory. The example above stores entries under
*changes.d/core/* and *changes.d/cli/*.

`paths` connects source changes to a section for missing-fragment checks. It
does not select the section when creating a fragment.

When many packages follow one layout, use a `[[section-patterns]]` table
instead of repeating an explicit section for each package. The
[*Package monorepos*](../guide/package-monorepos) guide works through a complete
setup. The [syntax reference](../reference/section-patterns) defines capture
names, partial-segment captures, path attribution, and precedence.


Adding a sectioned fragment
---------------------------

Once any sections are configured, every new fragment must name one:

~~~~ sh
sacho add --section core clear-function
~~~~

The command creates *changes.d/core/clear-function.md*. Passing an unknown
section or omitting `--section` is an error.

Preview one section while retaining the version heading and release date line:

~~~~ sh
sacho preview --section core
~~~~


Coverage checks
---------------

Section paths make missing-fragment checks more precise. A commit that changes
_packages/core/\*\*_ needs a new or meaningfully changed fragment in the `core`
section. A fragment in `cli` does not satisfy that requirement.

Use the top-level `[check].paths` list for repository-wide paths that require a
fragment in any section. See [*CI and hooks*](../guide/ci-and-hooks) for staged
and revision-based checks.


Changing a section title
------------------------

Keep the `id` when the same component gets a new display name:

~~~~ toml
[[sections]]
id = "core"
title = "Core API (Rust)"
directory = "rust/core"
paths = ["crates/core/**"]
~~~~

Continue using `sacho add --section core`. New output uses the title and records
its stable identity directly below the heading:

~~~~ markdown
### Core API (Rust)

<!-- sacho:section:v1 id=core -->

 -  Added an API.
~~~~

Sacho escapes Markdown syntax in explicit titles so they remain plain text.
Titles must be nonempty, have no surrounding whitespace, and contain no control
characters or line breaks. Unicode spelling and case are preserved.

Omitting `title` retains the old ID heading and output without an identity
comment. Setting `title` equal to `id` still requests the comment. Existing
comments remain readable if you later remove `title`. IDs are opaque keys:
Sacho rejects wholly blank IDs but does not trim or normalize them. The comment
encodes bytes that cannot appear literally, preserving the exact ID.

Different sections must have different effective titles (`title`, or `id` when
omitted). Explicit conflicts fail at configuration load; conflicts involving
generated or legacy fragment sections fail against the active fragment set,
even for a preview filtered to one section. Changing `directory` does not move
existing fragments. Do that separately when moving unreleased entries.

Identity comments belong immediately after their direct level-three heading,
before entries. Damaged, nested, orphaned, or repeated comments are errors in
the region being imported or carried. To include an identity-comment example in
a fragment, release note, or introduction, use a code span, code block, or
escaped text. Reserved `sacho:section:` HTML is rejected in those inputs.
`preview` retains the comments; `show` hides valid attached identity comments.

Older Sacho versions cannot safely read the new identity format. They may also
ignore new configuration fields and interpret a title as an ID. Upgrade every
automation sharing the configuration before adding titles or section maps.
