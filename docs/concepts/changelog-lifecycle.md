The changelog lifecycle
=======================

Fragments describe the next release. Released sections in the changelog record
past releases. Sacho moves prose from the first form to the second without
rewriting released history.


Choosing the next version
-------------------------

Set the version under development with:

~~~~ sh
sacho next 1.2.0
~~~~

This writes the label to *changes.d/next*. When the file is absent or empty,
the compiled heading falls back to `Unreleased` and `sacho release` requires an
explicit version.

Sacho treats version labels as text. It does not impose semantic versioning.


The unreleased region
---------------------

With the default `materialize = true` setting, *CHANGES.md* contains a generated
unreleased region above the released history:

~~~~ markdown
Version 1.2.0
-------------

To be released.

 -  Added `clear()` to remove every entry at once.
~~~~

Fragments remain the source of truth. Do not edit this region by hand.

Several commands keep it current:

 -  `sacho add` creates a fragment and refreshes the region.
 -  `sacho next` changes the version and refreshes the heading.
 -  `sacho fmt` formats every fragment and refreshes the region.
 -  `sacho sync` refreshes only the generated region.
 -  `sacho check` reports a region that differs from the fragments.

`sacho sync` protects hand edits by default. If the generated replacement would
discard them, inspect the proposed change before choosing `sacho sync --force`.

Set `materialize = false` when the repository should keep only released history
in *CHANGES.md*. `sacho preview` still compiles the unreleased region on demand.


Fragments-only repositories
---------------------------

With `materialize = false`, fragment edits do not change *CHANGES.md* between
releases.
`sacho preview` is the way to read the next release as one document before it
is cut, while `sacho check` validates the fragments without comparing a stored
unreleased region. At release time, `sacho release` compiles the fragments and
inserts the new dated section directly into *CHANGES.md*.

This policy suits projects whose users read published release notes rather than
browsing the repository for upcoming changes. It also avoids generated
changelog edits on every fragment commit.


Releasing
---------

`sacho release` compiles the current fragments into a dated version section,
inserts it before the older released sections, and removes the consumed
fragments. Released sections are then left untouched by normal Sacho commands.

~~~~ sh
sacho release --next 1.3.0
~~~~

The command above releases the version in *changes.d/next* with the current
local date, then writes `1.3.0` as the next version. Pass a version when no next
version has been set, or use `--date YYYY-MM-DD` when the date must be explicit.

The release mutation is transactional. Sacho checks that its inputs have not
changed between planning and writing, and rolls back already-applied changes if
a later write fails.


Carrying released entries
-------------------------

Maintenance branches sometimes need notes that were already released
elsewhere. `sacho carry 1.2.0` reads that frozen section and writes its entries
back as *carried-from-1.2.0.md* fragments. Repositories with sections receive
one carried fragment per populated section.

Carrying does not remove or alter the released section. It creates editable
unreleased input for a later release on the current branch.
