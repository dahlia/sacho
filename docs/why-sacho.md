Why Sacho?
==========

Changelogs tend to fail in two quiet ways. A user-visible change reaches a
release without an entry, or the entry reads like a commit message and tells
users little about the release. Sacho treats both as workflow problems rather
than something to repair on release day.


Write the note with the change
------------------------------

Each user-visible change gets a small Markdown fragment under *changes.d/*.
The fragment travels with the implementation through merges, rebases,
cherry-picks, and reverts. A reviewer sees the code and its proposed release
note together, while the reason for the change is still fresh.

This also avoids the usual conflict at the top of *CHANGES.md*. Concurrent
changes create separate files instead of editing the same unreleased list.
Sacho compiles those fragments into one changelog view for review and release.


Keep the audience clear
-----------------------

A commit message explains a change to contributors. A changelog entry explains
the released difference to users. Those audiences need different details, so
Sacho does not generate one form from the other.

The distinction matters during development. If a new function is renamed
before release, users experienced one addition, not an addition followed by a
rename. The fragment can be edited to tell that final story without preserving
an intermediate state that never shipped.


Automate presence, not prose
----------------------------

Hooks and CI can check that covered source changes include a fragment. Sacho
also validates fragment structure, references, and the generated region in the
changelog. The machine catches omissions and drift; reviewers decide whether
the words are useful.

That boundary is deliberate. Sacho will not infer release notes from commits,
choose the next version, or turn repository history into a development diary.


When Sacho fits
---------------

Sacho fits projects that keep a curated changelog, want release notes written
for users, and prefer missing entries to fail in the normal development
workflow. It is especially useful when changes move between branches or when
several contributors would otherwise edit one unreleased section.


About the name
--------------

Sacho (史草) was the name for draft records written independently by the
official historians of Joseon Korea. Those drafts were later compiled into the
*Veritable Records* (sillok, 實錄), whose completed volumes could not be
revised. The used drafts were then washed of their ink in a process called
secho (洗草).

The name also describes the workflow. Fragments are records written as changes
happen; a release compiles them into permanent history; deleting the consumed
fragments echoes secho. The drafts, rather than a reconstruction from commit
messages, remain the source of truth. [The historical records][veritable] give
the analogy its original context.

Read [*Philosophy*](./philosophy) for the full argument behind these choices.
If the approach fits your project, continue with
[*Getting started*](./guide/getting-started).

[veritable]: https://en.wikipedia.org/wiki/Veritable_Records_of_the_Joseon_Dynasty#Compilation_process
