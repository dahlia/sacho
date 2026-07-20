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


`sacho preview`
---------------

~~~~ text
sacho preview [--section <SECTION>]
~~~~

Prints the compiled unreleased region to standard output without changing
files. `--section` prints only one configured section.


`sacho show`
------------

~~~~ text
sacho show [--skip-heading] [--output-file <PATH>] <VERSION>
~~~~

Reads a frozen released section from the changelog.

| Option                       | Meaning                                                                    |
| ---------------------------- | -------------------------------------------------------------------------- |
| `-H`, `--skip-heading`       | Omit the version heading while preserving the section body and references. |
| `-o`, `--output-file <PATH>` | Write to a file instead of standard output.                                |

The command reports an error when the requested released version is absent. It
does not read the current fragments.


`sacho sync`
------------

~~~~ text
sacho sync [--force]
~~~~

Recompiles the materialized unreleased region from the fragments. It does
nothing when materialization is disabled or the changelog is already current.

`--force` allows the replacement to discard edits inside the generated region.


`sacho release`
---------------

~~~~ text
sacho release [--date <DATE>] [--next <NEXT>] [--allow-empty] [VERSION]
~~~~

Compiles and inserts a dated released section and removes the consumed
fragments. Without `--next`, it also removes the next-version file and closes
the materialized unreleased region. With `--next`, it opens the following cycle
in the same operation.

| Argument or option | Meaning                                                                |
| ------------------ | ---------------------------------------------------------------------- |
| `VERSION`          | Version to release. Defaults to the next-version file.                 |
| `--date <DATE>`    | Release date in `YYYY-MM-DD` form. Defaults to the current local date. |
| `--next <NEXT>`    | Open this next version immediately after the release.                  |
| `--allow-empty`    | Allow a release without substantive changelog items.                   |

When both `VERSION` and the next-version file contain values, they must agree.
The first release may have no fragments when the changelog has no released
version sections. Otherwise, the command refuses to release when the compiled
fragments contain no substantive changelog items unless `--allow-empty` is
passed. Empty list items, whitespace, and complete HTML comments do not make a
release nonempty; scaffold-only fragments still require the explicit option,
even for the first release.

The default release output is suitable for committing and tagging as a
released-only changelog. Run `sacho next <version>` after the tag to open the
next development cycle.


`sacho carry`
-------------

~~~~ text
sacho carry <VERSION>
~~~~

Turns the entries from one released section back into editable fragments named
*carried-from-VERSION.md*. Sectioned releases produce one fragment in each
populated section. Existing carried fragment destinations are replaced.


`sacho merge-driver`
--------------------

~~~~ text
sacho merge-driver <ORIGINAL> <CURRENT> <OTHER> <PATH>
~~~~

Implements the changelog merge driver installed by `sacho init`. The arguments
are the common ancestor, current side, other side, and repository path supplied
by the VCS. Users normally do not invoke this command by hand.
