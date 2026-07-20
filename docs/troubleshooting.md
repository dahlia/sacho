Troubleshooting
===============

`materialized changelog is out of sync`
---------------------------------------

The generated unreleased region no longer matches the fragments. Run:

~~~~ sh
sacho sync
~~~~

If Sacho warns that replacement would discard edits, inspect *CHANGES.md*.
Move intentional prose into the corresponding fragment, then run
`sacho sync --force`.


`invalid fragment`
------------------

A fragment must contain exactly one top-level unordered list after optional
frontmatter. A paragraph or heading at the top level, a second list, malformed
YAML, or an invalid `priority` value causes this error.

Start with the smallest valid form:

~~~~ markdown
 -  Described the user-visible change.
~~~~

Then run `sacho fmt` and `sacho check`.


`missing changelog fragment`
----------------------------

A staged change or checked commit matched `[check].paths` or a section's
`paths`, but no matching fragment changed with it.

Create and stage a fragment:

~~~~ sh
sacho add change-topic
git add changes.d/change-topic.md
~~~~

For a sectioned repository, pass `--section <id>`. If the commit has no
user-visible effect, add `Changelog: none` to its commit message. This exemption
cannot affect `sacho check --staged`, which runs before the final message is
known. [*CI and hooks*](./guide/ci-and-hooks#install-git-hooks) lists the other
accepted commit-message markers.


`no changelog entries to release`
---------------------------------

After the first release, `sacho release` requires at least one substantive
changelog item by default. Empty list items, whitespace, and complete HTML
comments do not satisfy that requirement. Add or finish a fragment, then
inspect it with `sacho preview` before releasing. If the empty release is
intentional, pass `--allow-empty` explicitly.

When the changelog has no released version sections, Sacho allows the first
release without fragments. Scaffold-only fragments are still rejected so they
are not consumed accidentally; finish or remove them before releasing, or use
`--allow-empty` if consuming them is intentional.


The unreleased region cannot be found
-------------------------------------

After `sacho release` without `--next`, a heading-based changelog intentionally
has no unreleased region until `sacho next`, `sacho add`, or `sacho carry`
starts another cycle. That closed state is valid when neither a next-version
file nor fragments exist.

The configured `region-detection` strategy does not match *CHANGES.md*. With
`heading`, check the version heading and the configured `unreleased-heading`
date line if a next version or fragments do exist. With `marker`, make sure
exactly one marker pair encloses the generated region, for example:

~~~~ markdown
<!-- sacho:unreleased:begin -->

Unreleased
----------

To be released.

<!-- sacho:unreleased:end -->
~~~~

Do not place the end marker inside a fenced code block.


A released version cannot be found
----------------------------------

`sacho show` and `sacho carry` read dated, released sections from the changelog.
They do not read the unreleased region. Check the exact version label shown in
*CHANGES.md* and make sure the release has already been compiled.


Hooks run the wrong Sacho executable
------------------------------------

Installed integrations use the executable that ran `sacho init`. Reinstall them
with an explicit managed path:

~~~~ sh
/managed/path/sacho init \
  --install-hook \
  --integration-executable /managed/path/sacho
~~~~

This is useful when a temporary development binary originally created the
repository integration.


A VCS command fails
-------------------

Run the failing query command outside Sacho and check its exit status and output
against the contracts in
[*Configuration reference*](./reference/configuration#vcs). Configured arrays
are executed without a shell, so pipes, redirects, quoting, and environment
expansion do not have shell semantics. Put that behavior in a wrapper
executable when it is required.
