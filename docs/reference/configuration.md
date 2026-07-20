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

| Field       | Required | Meaning                                                        |
| ----------- | -------- | -------------------------------------------------------------- |
| `id`        | Yes      | Rendered section identifier and value accepted by `--section`. |
| `directory` | Yes      | Fragment subdirectory, relative to `[fragments].directory`.    |
| `paths`     | No       | Globs whose changes require a fragment in this section.        |

Section ids and normalized directories must be unique. The array order controls
the rendered section order.


Path rules
----------

Configured paths must be relative to their anchor. Empty paths, absolute paths,
and paths that escape their anchor with `..` are rejected. Sacho normalizes `.`
and internal parent components before checking path uniqueness.
