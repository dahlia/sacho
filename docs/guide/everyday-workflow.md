Everyday workflow
=================

Most changes need four Sacho commands: `add`, `fmt`, `preview`, and `check`.


Start with the user-visible change
----------------------------------

Create a fragment when a change affects people who use the project:

~~~~ sh
sacho add clearer-errors
~~~~

Write what changed and, when useful, what the reader should do about it:

~~~~ markdown
 -  Included the invalid field name in configuration errors, making a bad
    setting easier to locate.
~~~~

Keep the fragment on the same branch and in the same commit series as the
implementation. If the implementation changes before release, edit the same
fragment.

Sectioned repositories must select the section explicitly:

~~~~ sh
sacho add --section cli clearer-errors
~~~~


Format before review
--------------------

Run:

~~~~ sh
sacho fmt
~~~~

This normalizes every fragment. When the unreleased changelog is materialized,
it refreshes that generated region too. Review both the fragment and the
resulting *CHANGES.md* change before committing.

If *CHANGES.md* has drifted without any fragment formatting to do, run:

~~~~ sh
sacho sync
~~~~

Sacho asks before replacing hand-edited content. `sacho sync --force` accepts
the generated replacement without that protection, so inspect the difference
first.


Preview what users will read
----------------------------

`sacho preview` prints the compiled unreleased region without changing files:

~~~~ sh
sacho preview
~~~~

In a sectioned repository, focus on one section with
`sacho preview --section <id>`.

Previewing catches editorial problems that fragment-by-fragment review can
miss: repeated entries, a security fix buried in the sort order, or two notes
that describe different stages of the same feature.


Check before committing
-----------------------

Run the repository checks:

~~~~ sh
sacho check
~~~~

The command validates fragment structure and formatting, reference links, the
next-version file, and the materialized changelog. With coverage paths
configured, it can also compare changes against a base revision or the staged
Git index.

~~~~ sh
sacho check --staged
sacho check --base origin/main
~~~~

Use `sacho check --fix` to repair fragment formatting and generated output that
can be fixed mechanically. It does not write changelog prose for you.


Changes with no changelog entry
-------------------------------

Not every commit changes the user-facing product. The installed Git hooks and
`sacho check --base` recognize this commit-message trailer:

~~~~ text
Changelog: none
~~~~

Use it for work such as internal refactoring or test-only changes when the
configured coverage paths would otherwise require a fragment.
`sacho check --staged` cannot apply a commit-message exemption because the
final message does not exist yet.
