How Sacho fits together
=======================

Sacho keeps unreleased changelog entries beside the changes they describe. Each
entry starts as a small Markdown file called a *fragment*. The fragments are the
source of truth until release day.

~~~~ mermaid
flowchart TD
  source["Source change"] ---|"committed together"| fragment["Fragment<br/><i>changes.d/clear-function.md</i>"]
  fragment -->|"fmt · check · preview"| next["Next-release view<br/><i>CHANGES.md</i> when materialized"]
  fragment --> release{{"sacho release"}}
  release --> frozen["Dated, frozen section<br/><i>CHANGES.md</i>"]
  release --> removed["Consumed fragment removed"]
~~~~

This split gives the two forms different jobs. Fragments travel with commits
through version-control operations. The changelog is the document users read.


Repository state
----------------

A typical repository contains these files:

~~~~ tree
options:
  defaultOpen: true
  showToolbar: false
  showBadges: false
  interactive: true
tree:
  - sacho.toml
  - name: changes.d
    children:
      - next
      - clear-function.md
  - CHANGES.md
~~~~

*sacho.toml* describes the repository. It names the changelog and fragment
paths, chooses a version-control preset, and can define sections or missing
fragment checks.

*changes.d/next* holds the version being prepared. The other Markdown files in
*changes.d/* are [*fragments*](./fragments).

*CHANGES.md* contains released history. By default, it also contains a generated
preview of the next release. That preview is called the *unreleased region*.


Two kinds of prose
------------------

A commit message records work for other contributors. A changelog entry records
the difference a user will encounter when upgrading. A refactoring may deserve
a careful commit message and no changelog entry. A one-line security fix may
need a prominent changelog entry.

Sacho checks and assembles prose, but it does not write it. Read
[*Philosophy*](../philosophy) for the reasoning behind that boundary.


What to read next
-----------------

Read [*Fragments*](./fragments) for the input format and naming conventions.
[*The changelog lifecycle*](./changelog-lifecycle) explains materialization,
release, and carrying entries between branches. Repositories with several
packages or components should also read [*Sections*](./sections).
