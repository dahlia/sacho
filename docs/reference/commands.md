Command reference
=================

Sacho reads *sacho.toml* from the repository root discovered from the current
directory. Run `sacho <command> --help` for the installed version's exact usage.


`sacho init`
------------

~~~~ text
sacho init [OPTIONS]
~~~~

Creates the configuration, fragment directory, initial changelog, and
version-control integration that do not already exist.

During first-time interactive setup with an existing changelog, Sacho offers
level-three headings found across the changelog as section candidates. For each
selected candidate, it suggests a fragment directory and source path globs. If
at least two selections map to sibling directories under one unambiguous
parent, Sacho first offers to replace their repeated `[[sections]]` entries
with one inferred `[[section-patterns]]` table. The prompts remain editable,
and selecting no candidates leaves both tables absent. A valid section identity
comment supplies the stable ID; the observed plain heading is retained as an
explicit title. Without a comment, that heading supplies both values. If one ID
has several titles, unreleased headings take precedence, then the first
occurrence in document order. Invalid plain titles must be edited before setup
can write a configuration.

| Option                            | Meaning                                                          |
| --------------------------------- | ---------------------------------------------------------------- |
| `--interactive`                   | Ask setup questions even when automatic detection is sufficient. |
| `--no-interactive`                | Never ask setup questions.                                       |
| `--install-hook`                  | Install or update Git commit hooks.                              |
| `--repository-url <URL>`          | Build the `#` reference template from this repository URL.       |
| `--integration-executable <PATH>` | Use this executable in installed VCS integrations.               |

`--interactive` and `--no-interactive` cannot be used together. Initialization
does not overwrite existing project files.


`sacho add`
-----------

~~~~ text
sacho add [--section <SECTION>] <NAME>
~~~~

Creates *NAME.md* in the fragment directory and prints its path. `NAME` is a
topic-based filename without the *.md* extension. `--section` is required when
sections are configured and rejected when its id is unknown.

The command also refreshes a materialized unreleased region. It does not
overwrite an existing fragment.


`sacho next`
------------

~~~~ text
sacho next <VERSION>
~~~~

Writes the version label to the configured next-version file. The value must
contain exactly one nonempty line. When materialization is enabled, the command
updates the unreleased heading in the changelog too.


`sacho check`
-------------

~~~~ text
sacho check [--base <BASE> | --staged] [--fix]
~~~~

Checks fragment parsing and formatting, next-version formatting, reference
resolution, the materialized changelog, and configured fragment coverage.

| Option          | Meaning                                                               |
| --------------- | --------------------------------------------------------------------- |
| `--base <BASE>` | Check each VCS commit after the base revision for required fragments. |
| `--staged`      | Check staged Git changes for required fragments.                      |
| `--fix`         | Format fragments and synchronize generated output before checking.    |

`--base` and `--staged` are mutually exclusive. For each commit, `--base` and
the installed hooks accept a `Changelog: none` trailer, compared
case-insensitively, or any of `[changelog skip]`, `[changes skip]`,
`[skip changelog]`, and `[skip changes]` as case-insensitive substrings. An
exemption affects only that commit.
`--staged` cannot use commit-message exemptions because no final message exists
yet.


`sacho fmt`
-----------

~~~~ text
sacho fmt
~~~~

Writes every fragment in Sacho's canonical Markdown and frontmatter format. It
also refreshes the materialized unreleased region. If that refresh would
discard hand edits, Sacho asks for confirmation in a terminal and refuses the
change in a noninteractive process.


`sacho resolve-links`
---------------------

~~~~ text
sacho resolve-links [FRAGMENT]...
~~~~

Follows redirects for every unpinned fragment reference and records each final
URL in that fragment's frontmatter. Existing pins are not requested again.
When materialization is enabled, the changelog is synchronized in the same
failure-safe operation. A network or validation failure leaves every file
unchanged.

One or more `FRAGMENT` arguments restrict the command to those fragment files,
interpreted like any other path (relative to the working directory, or
absolute). Without arguments it resolves every fragment. Selection limits which
fragments are fetched and rewritten; Sacho still parses and validates every
fragment, so an unselected fragment with an invalid shape or a conflicting pin
still fails the command. A path that does not match exactly one discovered
fragment is an error.


`sacho preview`
---------------

~~~~ text
sacho preview [--section <SECTION>] [--resolve-links | --no-resolve-links]
              [--no-word-wrap]
~~~~

Prints the compiled unreleased region to standard output without changing
files. `--section` prints only one configured section. Link resolution affects
the output in memory and never writes pins from `preview`. `--no-word-wrap`
removes automatic 80-column wrapping from the output while preserving explicit
Markdown hard breaks.


`sacho show`
------------

~~~~ text
sacho show [--skip-heading] [--output-file <PATH>] [--no-word-wrap] <VERSION>
~~~~

Reads a frozen released section from the changelog.

| Option                       | Meaning                                                                    |
| ---------------------------- | -------------------------------------------------------------------------- |
| `-H`, `--skip-heading`       | Omit the version heading while preserving the section body and references. |
| `-o`, `--output-file <PATH>` | Write to a file instead of standard output.                                |
| `--no-word-wrap`             | Remove automatic 80-column wrapping from the output.                       |

The command reports an error when the requested released version is absent. It
does not read the current fragments. `--no-word-wrap` applies equally to
standard output and `--output-file`, and preserves explicit Markdown hard
breaks.


`sacho sync`
------------

~~~~ text
sacho sync [--force] [--resolve-links | --no-resolve-links]
~~~~

Recompiles the materialized unreleased region from the fragments. It does
nothing when materialization is disabled or the changelog is already current,
unless link resolution is enabled and unpinned fragment references remain.

`--force` allows the replacement to discard edits inside the generated region.
Resolved fragment pins and changelog output are committed atomically.


`sacho release`
---------------

~~~~ text
sacho release [--date <DATE>] [--next <NEXT>] [--allow-empty]
              [--note <TEXT>] [--intro-file <PATH>]
              [--resolve-links | --no-resolve-links] [VERSION]
~~~~

Compiles and inserts a dated released section and removes the consumed
fragments. Without `--next`, it also removes the next-version file and closes
the materialized unreleased region. With `--next`, it opens the following cycle
in the same operation.

| Argument or option    | Meaning                                                                |
| --------------------- | ---------------------------------------------------------------------- |
| `VERSION`             | Version to release. Defaults to the next-version file.                 |
| `--date <DATE>`       | Release date in `YYYY-MM-DD` form. Defaults to the current local date. |
| `--next <NEXT>`       | Open this next version immediately after the release.                  |
| `--allow-empty`       | Allow a release without substantive changelog items.                   |
| `--note <TEXT>`       | Append inline Markdown to the release date paragraph.                  |
| `--intro-file <PATH>` | Insert UTF-8 Markdown before change items; `-` reads standard input.   |
| `--resolve-links`     | Resolve unpinned links before compiling the release.                   |
| `--no-resolve-links`  | Disable configured link resolution for this release.                   |

When both `VERSION` and the next-version file contain values, they must agree.
The first release may have no fragments when the changelog has no released
version sections. Otherwise, the command refuses to release when the compiled
fragments contain no substantive changelog items unless `--allow-empty` is
passed. Empty list items, whitespace, and complete HTML comments do not make a
release nonempty; scaffold-only fragments still require the explicit option,
even for the first release.

`--note` accepts one paragraph without embedded line breaks and preserves the
supplied punctuation. Use `--note=TEXT` for text beginning with a hyphen.
`--intro-file` accepts Markdown blocks, with top-level headings restricted to
level three or deeper. Relative paths use the process's working directory;
`./-` reads a file named `-`. Each option accepts one value and cannot be
repeated. Whitespace-only values are ignored.

Hongdown formats the new release section after both additions. Reserved Sacho
boundary comments and definitions that change existing content are errors,
reported before repository files are changed. Notes and introductions do not
satisfy the substantive-item requirement. See
[Making releases](../guide/releases.md) for examples.

`show` includes both additions and hides the stored introduction boundary
comments. `carry` excludes the introduction, including its lists and
section-like headings, and carries only the change items.

The default release output is suitable for committing and tagging as a
released-only changelog. Run `sacho next <version>` after the tag to open the
next development cycle.


`sacho carry`
-------------

~~~~ text
sacho carry <VERSION> [--map <FROM> <TO>] [--map-pattern <FROM> <TO>]
~~~~

Turns the entries from one released section back into editable fragments named
*carried-from-VERSION.md*. Sectioned releases produce one fragment in each
populated section. Existing carried fragment destinations are replaced.

Use stable IDs as targets for one-time overrides:

~~~~ sh
sacho carry 1.0.0 --map 'Old API' 'core' --map '-Legacy API' 'cli'
sacho carry 1.0.0 --map-pattern 'legacy/{name}' 'python/{name}'
~~~~

Both options accept two arguments and can be repeated. Literal `--map` treats
spaces, `=`, `->`, and braces as part of the value. Leading hyphens are allowed;
quote values as required by your shell. Only `--map-pattern` interprets
captures. Sources and targets are never automatically trimmed. A source is the
recorded ID when a heading has metadata, otherwise its trimmed plain heading.

Sacho validates syntax and every literal target before processing entries. Each
runtime rule must match at least one input heading, including headings without
items. A pattern counts as used when it matches even if a literal rule takes
precedence. Unused rules fail before writing and report the actual input keys.
Duplicate sources in the same literal/pattern category are errors even if their
targets agree. An empty plain section heading is a format error.

Interpretation uses the first matching step below. Errors in matching targets,
ambiguous current titles, and shadowed sections stop processing rather than
falling through:

| Step | Heading with recorded ID                 | Heading without metadata           |
| ---- | ---------------------------------------- | ---------------------------------- |
| 1    | CLI literal, otherwise CLI pattern       | CLI literal, otherwise CLI pattern |
| 2    | ID literal map, otherwise ID pattern map | Title literal map                  |
| 3    | Recorded current ID                      | Explicit current title             |
| 4    | Unknown-ID error                         | Title pattern map                  |
| 5    | No fallback                              | ID literal map                     |
| 6    | No fallback                              | ID pattern map                     |
| 7    | No fallback                              | Raw current ID                     |

Explicit current-title matches take precedence over reversible title patterns.
Only configured titles participate in that step; omitted titles reach the raw
ID step. Sacho never combines title and raw-ID candidates into one pool.
A broad `{name}` historical pattern explicitly opts into catching every valid
single-segment name, including typos and names that remain valid raw IDs.

Within a mapping stage, literals are exceptions to patterns. Overlapping
patterns can agree on the same canonical ID and directory; different targets
are ambiguous. Targets resolve in one hop through the current ID resolver,
without following other maps. Multiple old headings may merge into one fragment
in their observed document order. Entries are not deduplicated.

A recorded unknown ID has no title fallback. A damaged or misplaced identity
comment cannot be overridden with CLI maps: manually correct or delete it, then
use an explicit mapping if recovery needs one. Validation covers the selected
release body, including nested HTML and introductions. Damage in another
structurally separate release does not block the selected release.

The command preserves released history and validates pins, destinations, and
active output-title collisions before writing, including when
`materialize = false`. Existing carried files are still replaced on rerun;
changing maps does not clean up earlier output files automatically.


`sacho import-unreleased`
-------------------------

~~~~ text
sacho import-unreleased [--force]
~~~~

Converts the materialized `Unreleased` or `Version X` region into deterministic
*imported-unreleased.md* fragments. A sectioned repository receives one file
per populated configured section. The `X` from a `Version X` heading is also
written to the next-version file, which must be absent, empty, or already equal
to `X`.

The repository must use `materialize = true` and contain no Markdown fragments.
Sacho rejects top-level content that fragments cannot represent and never
changes released history. If normalized fragment output changes the unreleased
region, an interactive invocation shows the diff and asks for confirmation. A
noninteractive invocation exits with status 2; `--force` applies the reviewed
normalization without a prompt. It does not bypass safety errors.

Section identity comments and explicitly configured current titles resolve to
stable IDs. Historical configuration maps and carry overrides do not apply to
import. With no configured sections, reserved section metadata, valid or
damaged, is an error rather than permission to discard identity. Restore the
configuration or manually remove metadata and section structure before choosing
a flat import compatible with the command's input rules.


`sacho merge-driver`
--------------------

~~~~ text
sacho merge-driver <ORIGINAL> <CURRENT> <OTHER> <PATH>
~~~~

Implements the changelog merge driver installed by `sacho init`. The arguments
are the common ancestor, current side, other side, and repository path supplied
by the VCS. Users normally do not invoke this command by hand.
