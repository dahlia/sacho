Philosophy
==========

Sacho is a small tool built on strong opinions about changelogs. The opinions
came first; this document states them and argues for them. None of them
depend on Sacho, and a project could follow all of them with a text editor
and discipline. The tool exists because discipline does not scale.


Changelogs are for users
------------------------

A changelog answers two questions for a person who already runs your
software: what changed, and what should I do about it when I upgrade. That
is the whole job. It is not a development diary, not a record of effort, not
a place to justify decisions.

This sounds obvious until you notice what it rules out. A release without a
changelog gives users no reason to upgrade: from the outside, an
undocumented release is indistinguishable from no release. Publishing one is
therefore not a courtesy appended to the release process; it is part of what
releasing means. A version number with no accompanying prose is a claim that
something changed, with the substance withheld.


A changelog is not a commit log
-------------------------------

Commit messages and changelog entries look similar enough that an entire
generation of tooling treats one as raw material for the other. The
similarity is superficial.

A commit message addresses collaborators, present and future: people who
will read the diff, run `git blame`, and need to know why the change was
made. It reasons about internals in the vocabulary of internals. A changelog
entry addresses users, who see none of that. They interact with the public
surface and need to know what changed there, in the vocabulary of the
surface.

Audience determines voice, granularity, and even which changes exist. A
refactoring that touches forty files is a significant commit and a
nonexistent changelog entry. A one-line version bump of a vendored
dependency can be an invisible commit and a critical changelog entry, if it
closes a vulnerability users need to know about.

Tools that generate changelogs from commit messages (conventional-changelog,
semantic-release, git-cliff) accept a bad bargain: they get automation by
erasing the audience distinction. The result reads like what it is, a
commit log with formatting, and projects that adopt such tools tend to stop
writing for users at all, because the pipeline gives them a
changelog-shaped artifact without asking. It passes the release checklist
while leaving the user in the same place as before.

A changelog written for users has to be written by someone thinking about
users. Automation can check that an entry exists, collect the pieces,
format the result; it should stop before it starts choosing the words.


Entries travel with their commits
---------------------------------

An unreleased changelog entry describes a change that exists as a commit
somewhere. The two are one fact in two representations, and representations
of one fact must move together. When the commit is reverted, the entry must
vanish. When the commit is cherry-picked to a maintenance branch, the entry
must come along. When two branches merge, the surviving set of entries must
describe the surviving set of changes.

Keep entries anywhere other than the repository and every one of these
becomes a manual bookkeeping step, which is to say a step that will
eventually be missed. A wiki page, an issue tracker label, a draft release
note on a forge: all of them drift from the code the moment history is
rewritten, and history is always eventually rewritten.

Entries stored as files inside the repository get all of this behavior from
the version control system, for free, with the same guarantees the code
itself enjoys. This is why Sacho's unreleased entries are files (fragments)
and why the changelog's rendered form is derived from them rather than
edited directly.


One change, one file
--------------------

A single CHANGES.md accumulating unreleased entries has a mechanical
problem: every pair of branches that both add an entry will conflict at the
same location, the top of the file. The conflict is pure noise. Both
entries should survive; the merge machinery just cannot know that, because
it sees overlapping edits to one file.

Fragmenting entries into one file per change dissolves the problem.
Concurrent changes touch different files and merge cleanly. CPython's
NEWS.d directory demonstrated the approach at scale years ago, and
towncrier and changesets carried it to other ecosystems.

A fragment also gives the entry an identity. Once an entry is a file, it
can be edited or removed as the change itself changes, which is where the
next two principles meet.


Machines catch what reviewers miss
----------------------------------

More people avoid writing changelogs than embrace the practice, and even
the convinced forget. This is not a moral failing; the changelog is the
one artifact of a change with no test to fail and no compiler to complain.
Everything else in the change announces its own absence; a missing
changelog entry announces nothing.

Relying on reviewers to notice the absence assigns humans the kind of
vigilance task humans are worst at: checking, on every single change, for
the presence of something that is usually present. The miss rate is low
and the volume is high, and low miss rates on high volume still produce a
steady leak.

Presence checks are trivial for machines. A hook or CI job that fails when
a change touches source paths without touching a fragment converts the
vigilance task into a mechanical one, and frees reviewers for the part
machines cannot do: judging whether the entry is any good. Escape hatches
belong in the mechanism (some changes genuinely need no entry), but the
default must be that the machine asks.


Write the history users experienced, not the history that happened
------------------------------------------------------------------

Suppose one release's development adds a function `remove_all()`, and a
later commit in the same cycle renames it to `clear()`. The commit history
honestly contains two events. The released changelog must contain one
entry:

> Added `clear()` function.

Not two:

> Added `remove_all()` function.
> Renamed `remove_all()` to `clear()`.

The two-entry version is more faithful to what happened and strictly worse
as a changelog. It documents a function the user never saw, then documents
the disappearance of the thing it just documented. The user upgrading
across this release experiences exactly one change, the appearance of
`clear()`, and the changelog should describe the world as the user
experiences it. Intermediate states that never shipped are internal
history.

This principle is the sharpest argument against generating changelogs from
commits, because no generator can apply it. Collapsing the two events into
one requires knowing that they concern the same thing and that the first
never reached users, which is editorial judgment about the released
surface, not information in the commit graph.

It also shapes how unreleased entries should be stored. When entries live
in per-change files named after their topic, the natural response to the
rename is to edit the existing fragment, and the collapse happens at
writing time with no editorial pass needed at release time. When files are
named after issue numbers, the natural response is a second file, and the
mistake becomes the default. The layout should make editing the existing
entry feel more natural than adding a second one.


Don't confuse users
-------------------

Concision here is not a style preference; it is the same audience argument
again, applied at the sentence level. Every word in an entry costs every
user a moment of attention, multiplied across everyone who ever reads the
release notes.

Internal names are the most common leak. Class names of private types,
module paths, subsystem nicknames: to the maintainer these are just the
words for things, and to users they are noise that signals this document
is not really for you. If an internal mechanism genuinely must be
mentioned, describe what it does rather than naming it. A name the reader
cannot look up is worse than no name.

The same goes for narrating implementation effort (“rewrote the parser to
be more maintainable”) when nothing user-visible changed. If the honest
entry is that nothing changed for users, the honest entry is no entry.


Where the tool comes in
-----------------------

Each principle above implies mechanism. Entries for users, written by
humans: so Sacho refuses to generate. Entries travel with commits: so
fragments are files in the repository. Fragments per change: so merges
stay clean. Machines catch absence: so `check` exists and is built to run
in hooks and CI. One entry per user-visible change: so fragments are
topic-named files you edit rather than append-only records. Concision: so
the one house style keeps entries short, uniform, and easy to scan.

The tool only enforces the shape of the practice. A project that adopts
Sacho but writes entries for the wrong audience gets well-formatted noise.
A project that writes for users gets, from the tool, the guarantee that
the writing survives history rewrites and cannot slip out of a release
unnoticed.
