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
installation also provides supporting tools such as Hongdown and Nushell.

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

Run the Sacho binary during development:

~~~~ sh
mise run run
~~~~


Contribution guidelines
-----------------------

Keep changes focused and small enough to review comfortably. Include tests when
the change affects behavior, parsing, output, or error handling.

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

Make sure generated formatting changes are included in your commit.
