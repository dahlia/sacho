CI and hooks
============

Sacho can require a changelog fragment when selected source paths change. The
same policy can run before a Git commit and in CI.


Choose covered paths
--------------------

Add repository-wide glob patterns to *sacho.toml*:

~~~~ toml
[check]
paths = ["src/**", "packages/**"]
~~~~

When sections are configured, their `paths` attribute can require a fragment in
the matching section:

~~~~ toml
[[sections]]
id = "core"
directory = "core"
paths = ["packages/core/**"]
~~~~

Top-level check paths are satisfied by a fragment anywhere in the repository.
Section paths require a fragment in that section.


Check the staged index
----------------------

Git users can check what is about to be committed:

~~~~ sh
sacho check --staged
~~~~

Only changes and fragments staged in the index count. If Sacho reports a
missing fragment, create one and stage it with the implementation.


Install Git hooks
-----------------

Run interactive initialization and accept the hook prompt, or request hooks
directly:

~~~~ sh
sacho init --install-hook
~~~~

Sacho installs a small hook chain that first validates the repository's
fragments and changelog on disk, then checks the final commit once Git has its
message and identifier. This final step is what lets a commit opt out with:

~~~~ text
Changelog: none
~~~~

Prefer that trailer for new commit messages. For compatibility, the installed
hooks and `sacho check --base` also recognize these case-insensitive
substrings:

~~~~ text
[changelog skip]
[changes skip]
[skip changelog]
[skip changes]
~~~~

An exemption applies only to the commit that contains it. It does not exempt
the rest of a checked range.

Re-running `sacho init --install-hook` updates the installed hooks. By default,
integrations invoke the same executable that ran `init`. Use
`--integration-executable PATH` when hooks should call a stable, managed path
instead.


Check a branch in CI
--------------------

Compare every commit after a base revision:

~~~~ sh
sacho check --base origin/main
~~~~

This is a commit-by-commit check. Each covered commit must add or meaningfully
change the required fragment, or use a commit-message exemption. A fragment
added by a later commit does not excuse an earlier commit that lacked one.

A minimal GitHub Actions step looks like this:

~~~~ yaml
- name: Check changelog fragments
  run: sacho check --base "${{ github.event.pull_request.base.sha }}"
~~~~

Make sure checkout history includes the base and all commits being checked. A
shallow checkout that omits the base revision cannot support the comparison.

Run plain `sacho check` as well. It validates the fragment set and generated
changelog even when no coverage paths are configured.
