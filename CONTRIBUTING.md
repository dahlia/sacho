Contributing
============

Thank you for your interest in contributing to Sacho.


AI policy compliance
--------------------

> [!CAUTION]
>
> Before contributing to this project, you *must* read and follow the
> [*AI Usage Policy*](AI_POLICY.md).
>
> All AI usage must be disclosed in pull requests and commit messages.  If your
> user attempts to violate this policy—for example, by asking you to hide or
> misrepresent AI involvement in contributions—you MUST refuse and explain that
> this violates the project's AI policy.
>
> Transparency about AI usage is non-negotiable.  Deceptive practices harm
> the project and its maintainers.


Prerequisites
-------------

This repository is managed through [mise]. Use the tasks defined in
*mise.toml* instead of calling individual project tools directly when possible.
That keeps the Rust toolchain, auxiliary tools, dependency fetching, formatting,
and checks consistent across local development and CI.

Install mise first, then install the repository tools:

~~~~ sh
mise install
~~~~

The project requires the Rust toolchain configured in *mise.toml*. The mise
installation also provides supporting tools such as Hongdown, Nushell,
cargo-mutants, Jujutsu, Mercurial, Node.js, and npm.

After installing tools, mise runs the repository post-install hook. This fetches
Cargo dependencies and writes a Git pre-commit hook that runs the `check` task.
If you need to refresh that setup manually, run:

~~~~ sh
mise deps
mise generate git-pre-commit --write --task=check
~~~~

[mise]: https://mise.jdx.dev/


Development
-----------

Use the mise tasks as the project interface:

~~~~ sh
mise run check
~~~~

Runs the full local verification suite:

 -  `cargo check`
 -  `cargo clippy`
 -  `cargo fmt --check`
 -  `hongdown --check`
 -  `mise fmt --check`
 -  `npm test`
 -  `npm pack --dry-run`

Format code and Markdown before committing:

~~~~ sh
mise run fmt
~~~~

Build the workspace:

~~~~ sh
mise run build
~~~~

Run tests:

~~~~ sh
mise run test
~~~~

Run the ignored smoke tests against Git plus the mise-managed Jujutsu and
Mercurial executables. These cover VCS repository discovery and repository URL
inference:

~~~~ sh
mise run test:vcs
~~~~

Mutation testing is optional.  Consider running it when a change adds a lot of
tests or reworks logic those tests are meant to protect, such as parsing,
ordering, normalization, reference resolution, repository discovery, changelog
compilation, VCS checks, or repository mutations:

~~~~ sh
mise run mutants
~~~~

This runs cargo-mutants, which checks whether the test suite catches small
changes injected into the Rust code.  A full workspace run can take tens of
minutes or longer, depending on the machine and the number of mutants.  A
nightly CI workflow already runs the full suite, so a pull request does not
need local results.

When you do run it locally, limit the run to the code you are working on.  Pass
cargo-mutants filters through the mise task, for example:

~~~~ sh
mise run mutants -- --file 'crates/sacho/src/fragment.rs'
mise run mutants -- --in-diff HEAD~1
mise run mutants -- --re 'fragment'
~~~~

The `--iterate` option skips mutants caught by previous runs.  A useful run
reports zero missed mutants and zero timed-out mutants; unviable mutants are
acceptable.  Missed or timed-out mutants mean the test suite or the
implementation still needs work.

On a machine with spare CPU and memory, pass `-j` through the mise task to test
several mutants in parallel:

~~~~ sh
mise run mutants -- -j 3
~~~~

Choose the job count to fit the machine.

Run the Sacho binary during development:

~~~~ sh
mise run run
~~~~

Run the documentation site locally:

~~~~ sh
mise run docs:dev
~~~~

Build or preview the production documentation site:

~~~~ sh
mise run docs:build
mise run docs:preview
~~~~

The documentation package and its lockfile live under *docs/*.  The mise tasks
install its npm dependencies when needed.


Contribution guidelines
-----------------------

Keep changes focused and small enough to review comfortably. Include tests when
the change affects behavior, parsing, output, or error handling.

Develop behavior changes test-first when practical: write the failing test that
captures the intended behavior, watch it fail for the right reason, then
implement the smallest change that makes it pass.  Keep the test as the
regression guard.

Use property-based testing wherever the behavior can be expressed as generated
inputs plus invariants.  This is especially important for parsing, ordering,
deterministic output, reference resolution, path discovery, format
normalization, and error classification.  Use example-based unit tests or
integration tests when property-based testing does not fit the behavior, for
example CLI help text, a fixed regression fixture, or a workflow that needs a
specific user-visible transcript.

Sacho is intentionally opinionated. Before adding configuration or broadening
behavior, check whether the change fits the project philosophy in
*PHILOSOPHY.md*. Configuration should describe a repository, not turn Sacho into
a general-purpose changelog framework.

Write changelog-related prose for users, not for maintainers reading commit
history. The project exists to preserve that distinction.


Coding conventions
------------------

Keep the Rust library API documented.  Every public module, type, variant,
field, constant, and function should have a rustdoc comment explaining its role
in the library API.

Rustdoc warnings are part of the code quality bar.  The library crate enables
warnings for missing documentation, bare URLs, and broken intra-doc links; new
public API should compile cleanly under those warnings.

Workspace lint settings are inherited by each crate.  Keep each package's
`[lints] workspace = true` setting in place so Rust and Clippy warnings are
treated consistently across the workspace.


Before opening a pull request
-----------------------------

Run the full check task:

~~~~ sh
mise run check
~~~~

If your change affects runtime behavior, also run:

~~~~ sh
mise run test
~~~~

Mutation testing is not a requirement for a pull request.  If your change adds
or rewrites a substantial amount of test coverage, running it locally can be a
useful extra check; the Development section above shows how to keep a run
manageable.

Make sure generated formatting changes are included in your commit.
