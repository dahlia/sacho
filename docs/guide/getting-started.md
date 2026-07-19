Getting started
===============

This guide adds Sacho to a repository, writes one changelog fragment, and
previews the next release.


Install Sacho
-------------

The recommended installation uses mise's GitHub backend:

~~~~ sh
mise use -g github:dahlia/sacho
~~~~

npm installs the same prebuilt binary:

~~~~ sh
npm install -g @sacho/sacho
~~~~

Cargo can build the published crate from source:

~~~~ sh
cargo install sacho
~~~~

Without mise, npm, or Cargo, download the archive for your platform from
[GitHub Releases], then place
*sacho* (*sacho.exe* on Windows) in a directory on your `PATH`.

Run `sacho --version` to confirm that the executable is on your `PATH`.

[GitHub Releases]: https://github.com/dahlia/sacho/releases


Initialize a repository
-----------------------

From the repository root, run:

~~~~ sh
sacho init
~~~~

Sacho creates *sacho.toml*, *changes.d/*, and *CHANGES.md*. In a Git repository
it also registers merge drivers in *.gitattributes* and the local Git config.
Interactive setup can infer issue links from the repository remote and offer to
install commit hooks.

Use `sacho init --interactive` to ask the setup questions even when automatic
detection would otherwise be enough. For scripts, `--no-interactive` prevents
prompts.

Set the version you are preparing:

~~~~ sh
sacho next 1.2.0
~~~~


Write a fragment
----------------

Create a fragment named after the change:

~~~~ sh
sacho add clear-function
~~~~

The command prints a path such as *changes.d/clear-function.md*. The new file
contains an empty list item. Complete it with prose for the people who use the
project:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.
~~~~

Describe the released behavior. Leave out private type names, refactoring
details, and intermediate designs that users will never encounter.


Format and inspect the release
------------------------------

Format the fragments and generated changelog:

~~~~ sh
sacho fmt
~~~~

Print the release as Sacho would compile it:

~~~~ sh
sacho preview
~~~~

Finally, check the repository:

~~~~ sh
sacho check
~~~~

Commit the fragment with the source change. It will now follow that change
through merges, rebases, cherry-picks, and reverts.

Continue with [*Everyday workflow*](./everyday-workflow), or read [*How Sacho
fits together*](../concepts/) for the underlying model.
