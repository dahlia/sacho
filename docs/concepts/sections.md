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

`id` becomes the rendered heading. `directory` is relative to the configured
fragment directory. The example above stores entries under
*changes.d/core/* and *changes.d/cli/*.

`paths` connects source changes to a section for missing-fragment checks. It
does not select the section when creating a fragment.


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
