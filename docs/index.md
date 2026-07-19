---
layout: home

hero:
  name: Sacho
  text: Changelogs are for users.
  tagline: An opinionated changelog manager. Release notes written by humans, kept with the code, compiled at release.
  image:
    light: /logo.svg
    dark: /logo-dark.svg
    alt: A cinnabar seal bearing the character 史
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: Why Sacho?
      link: /why-sacho
---

Install
-------

::: code-group

~~~~ sh [mise]
mise use -g github:dahlia/sacho
~~~~

~~~~ sh [npm]
npm install -g @sacho/sacho
~~~~

~~~~ sh [Cargo]
cargo install sacho
~~~~

:::

Sacho is a single static binary with no runtime dependencies. It works in any
repository, whatever the language, and supports Git, Jujutsu, and Mercurial.
Prebuilt binaries are on
[GitHub Releases].

[GitHub Releases]: https://github.com/dahlia/sacho/releases


The opinions came first
-----------------------

Sacho is a small tool built on strong opinions about changelogs. None of them
depend on Sacho: a project could follow every one with a text editor and
discipline. They are worth stating before the tool is.

<ol class="sacho-credo">
  <li>
    <a href="/philosophy#changelogs-are-for-users">Changelogs are for
    users.</a>
    <p>A changelog answers two questions for a person who already runs your
    software: what changed, and what should I do about it when I upgrade. It
    is not a development diary and not a record of effort.</p>
  </li>
  <li>
    <a href="/philosophy#a-changelog-is-not-a-commit-log">A changelog is not
    a commit log.</a>
    <p>Commit messages address collaborators in the vocabulary of internals.
    Changelog entries address users at the public surface. Tools that
    generate one from the other erase that distinction, and the output is a
    commit log arranged to look like a changelog.</p>
  </li>
  <li>
    <a href="/philosophy#entries-travel-with-their-commits">Entries travel
    with their commits.</a>
    <p>An unreleased entry describes a change that exists as a commit
    somewhere. Kept as a file in the repository, the entry survives merges,
    reverts, and cherry-picks with the same guarantees the code enjoys.</p>
  </li>
  <li>
    <a href="/philosophy#one-change-one-file">One change, one file.</a>
    <p>A single accumulating changelog makes every pair of branches conflict
    at the same place. Fragments dissolve the problem: concurrent changes
    touch different files and merge cleanly.</p>
  </li>
  <li>
    <a href="/philosophy#machines-catch-what-reviewers-miss">Machines catch
    what reviewers miss.</a>
    <p>A missing entry announces nothing. No test fails and no compiler
    complains, so checking for absence is work for a machine, and reviewers
    are freed to judge whether the words are any good.</p>
  </li>
  <li>
    <a
    href="/philosophy#write-the-history-users-experienced-not-the-history-that-happened">Write
    the history users experienced, not the history that happened.</a>
    <p>A function added and renamed in the same release cycle is one change
    to users, not two. States that never shipped are internal history, and no
    commit-based generator can know that.</p>
  </li>
  <li>
    <a href="/philosophy#don-t-confuse-users">Don't confuse users.</a>
    <p>Every word in an entry costs every reader a moment of attention.
    Internal names and narrated effort tell users this document is not really
    for them.</p>
  </li>
</ol>

Read the full argument in [*Philosophy*](/philosophy).


What the practice looks like
----------------------------

Each user-visible change is one small Markdown file, named after the change
and committed with it. Here is *changes.d/clear-function.md*:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.
~~~~

If `clear()` is renamed before it ships, you edit this file. Users will
experience one change, and the changelog will say so.

When the release is ready, one command compiles every fragment into a dated
section of *CHANGES.md* and deletes the consumed files:

~~~~ sh
sacho release --next 1.3.0
~~~~

~~~~ markdown
Version 1.2.0
-------------

Released on July 19, 2026.

 -  Added `clear()` to remove every entry at once.
~~~~

The released section is sealed. Sacho never rewrites it.


Where the tool comes in
-----------------------

<div class="sacho-mechanisms">
  <div>
    <h3>It refuses to generate</h3>
    <p>Every entry is prose written by a human at the time of the change.
    Sacho collects, sorts, formats, and checks; it stops before choosing the
    words.</p>
  </div>
  <div>
    <h3>It keeps entries in the repository</h3>
    <p>Fragments follow their commits through every merge and history
    rewrite, so the changelog and the code cannot drift apart.</p>
  </div>
  <div>
    <h3>It fails the build on missing entries</h3>
    <p><code>sacho check</code> runs in hooks and CI and notices when covered
    source changes carry no fragment, with escape hatches for changes that
    genuinely need no entry.</p>
  </div>
  <div>
    <h3>It seals released history</h3>
    <p>A release compiles the fragments into a dated section and consumes
    them. Released sections are never touched again.</p>
  </div>
</div>

<div class="sacho-closing">
  <p class="sacho-closing-statement">None of this requires Sacho.</p>
  <p class="sacho-closing-statement">A text editor and discipline are
  enough.</p>
  <p class="sacho-closing-statement">Sacho exists because discipline does
  not scale.</p>
  <p class="sacho-closing-action"><a href="/guide/getting-started">Get
  started</a></p>
</div>

<div class="sacho-colophon">
  <p>Sacho (史草) were the draft records of Joseon Korea's court historians:
  written as events happened, compiled into sealed annals, then washed of
  their ink. <a href="/why-sacho#about-the-name">The name is a description of
  the workflow.</a></p>
  <p>If Sacho earns a place in your workflow, you can support its
  development through
  <a href="https://github.com/sponsors/dahlia" rel="external">GitHub
  Sponsors</a>.</p>
</div>
