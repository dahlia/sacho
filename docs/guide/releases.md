Making releases
===============

A Sacho release turns the current fragments into a dated, permanent section of
the changelog.


Review the release
------------------

Set or confirm the next version:

~~~~ sh
sacho next 1.2.0
sacho preview
sacho check
~~~~

Read the preview as a user upgrading from the previous release. Combine entries
that describe the same change, remove development history, and check that each
entry names the public behavior rather than its implementation.

Sacho refuses to release when the compiled fragments contain no substantive
items. Empty list items and complete HTML comments are scaffolding, not release
notes. There is no option to create an empty release.


Compile the release
-------------------

When *changes.d/next* already contains the release version, run:

~~~~ sh
sacho release --next 1.3.0
~~~~

Sacho uses the current local calendar date, consumes the released fragments,
and starts the next version at `1.3.0`.

If no next version has been set, provide the release version:

~~~~ sh
sacho release 1.2.0 --next 1.3.0
~~~~

Supply the date when reproducibility or a release process requires it:

~~~~ sh
sacho release 1.2.0 --date 2026-07-19 --next 1.3.0
~~~~

Commit the changed changelog, removed fragments, and updated *changes.d/next*
together.


Publish release notes
---------------------

After the release commit, `sacho show` prints the frozen section for one
version:

~~~~ sh
sacho show 1.2.0
~~~~

Use `--skip-heading` when the release platform already displays the version,
and `--output-file` to write the result directly:

~~~~ sh
sacho show 1.2.0 --skip-heading --output-file release-notes.md
~~~~

A GitHub Actions job triggered by a `v1.2.0` tag can publish that file with the
GitHub CLI:

~~~~ sh
version="${GITHUB_REF_NAME#v}"
sacho show "$version" --skip-heading --output-file release-notes.md
gh release create "$GITHUB_REF_NAME" --notes-file release-notes.md
~~~~

`show` reads only released sections. It does not compile current fragments or
return an unreleased preview.


Forward-port release notes
--------------------------

The simplest forward port happens before the maintenance release. Merge the
maintenance branch while its fragment still exists, and the VCS carries that
fragment into the receiving branch. Each branch later consumes its own copy;
Sacho needs no extra command.

Merging a release tag is different. The incoming release commit has already
consumed the fragment, so the merge brings only the frozen released section.
The merge driver inserts that section into *CHANGES.md* and prints a
`sacho carry` hint. If the receiving branch's next release should repeat those
notes, run:

~~~~ sh
sacho carry 1.2.0
~~~~

This decompiles the released section into *carried-from-1.2.0.md* fragments.
Edit those fragments if the later branch presents the change differently, then
handle them like any other unreleased entry. If the imported released section
is sufficient for the project's policy, no carry is needed.
