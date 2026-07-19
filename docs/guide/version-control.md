Version control
===============

Sacho uses version control for two jobs: determining whether changes need a
fragment and resolving generated changelog conflicts. Built-in presets support
Git, Jujutsu, and Mercurial. The `none` preset keeps content checks while
disabling repository queries.

~~~~ toml
[vcs]
preset = "git" # "jj", "hg", or "none"
~~~~


Git
---

`sacho init` adds merge attributes for *CHANGES.md* and *changes.d/next*, then
registers the corresponding drivers in the repository's local Git config.
Concurrent fragment additions usually merge without conflict because they are
different files. When the generated changelog conflicts, the driver recompiles
its unreleased region from the merged fragments.

The driver also keeps frozen released sections from both sides. Incoming
released sections with semantic versions are inserted at their descending
version position. If it imports a released section that is absent from the
current branch, it prints a
`sacho carry <version>` hint for projects that repeat those notes in the next
release. The ours-style rule for *changes.d/next* keeps the version currently
being prepared on the receiving branch. If both sides contain different text
for the same released version, the driver leaves a conflict for a person to
resolve.

Git also supports `sacho check --staged`, `sacho check --base`, and the optional
commit-hook chain described in [*CI and hooks*](./ci-and-hooks).


Jujutsu
-------

The Jujutsu preset supplies commands for revision and changed-path queries, so
`sacho check --base` can enforce fragment coverage. Jujutsu does not have a
per-path merge-driver hook. After resolving a concurrent fragment merge, run:

~~~~ sh
sacho sync --force
~~~~

Inspect the resulting changelog before committing the resolution.


Mercurial
---------

Initialization writes an idempotent block to *.hg/hgrc*. It configures the
changelog merge driver and, when materialization is enabled, an update hook that
refreshes the changelog after a successful merge.

The Mercurial preset handles parent comparisons inside its built-in query
commands. Use `sacho check --base <revision>` for coverage checks.


Custom query commands
---------------------

Each preset provides three queries: list commits after a base, list paths
changed by one commit, and read a commit message. Override only the query that
needs a repository-specific wrapper:

~~~~ toml
[vcs]
preset = "git"

[vcs.commands]
message = ["my-vcs-wrapper", "message", "${commit}"]
~~~~

Each array contains an executable followed by literal arguments. Sacho never
passes the command through a shell. The `commits` query requires `${base}`;
`changed-paths` and `message` require `${commit}`.

See [*Configuration reference*](../reference/configuration#vcs) for the full
output contracts.
