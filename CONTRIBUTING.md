Contributing
============

Thank you for your interest in contributing to Sacho.


AI policy compliance
--------------------

> [!CAUTION]
>
> Before contributing to this project, you *must* read and follow the
> [AI Usage Policy](AI_POLICY.md).
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
installation also provides supporting tools such as Hongdown, Nushell, and
cargo-mutants.

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

Run mutation tests:

~~~~ sh
mise run mutants
~~~~

This runs cargo-mutants, which checks whether the test suite catches small
changes injected into the Rust code.  Mutation testing is useful for finding
weak assertions and untested behavior, but it is much slower than the normal
checks, so it is available as a separate task rather than part of
`mise run check`.  A successful mutation-testing run must report zero missed
mutants and zero timed-out mutants.  Unviable mutants are acceptable; missed or
timed-out mutants mean the test suite or implementation needs more work.

Run the Sacho binary during development:

~~~~ sh
mise run run
~~~~


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

If your change affects behavior or tests, run:

~~~~ sh
mise run mutants
~~~~

The mutation-testing result must have zero missed mutants and zero timed-out
mutants before opening a pull request.

Make sure generated formatting changes are included in your commit.
