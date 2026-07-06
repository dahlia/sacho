Sacho: an opinionated changelog manager
=======================================

Motivation
----------

Changelogs are not commit messages. A commit message explains why a change was
made and addresses collaborators; a changelog tells users what changed and what
they should or can do when they upgrade. Most changelog tooling ignores this
distinction. Tools like git-cliff, conventional-changelog, and semantic-release
generate changelogs *from* commit messages, which produces documents written
for the wrong audience in the wrong voice. Sacho rejects generation outright:
every changelog entry is prose written by a human, for users, at the time the
change is made.

The remaining tools that share this stance are tied to language ecosystems.
towncrier assumes Python; changesets assumes an npm monorepo. Sacho is a single
static binary with no runtime dependencies, usable in any repository regardless
of language, and it does not even assume Git.

[Sacho is opinionated.](./PHILOSOPHY.md) It enforces one fragment format,
one output style, and one set of invariants. Configuration exists to describe
your repository, not to customize the philosophy.


Etymology
---------

Sacho (史草) names the
[draft records kept by official historians of Joseon Korea.][1]
Historiographers wrote sacho independently, event by event, as things happened.
Only after a king's death did the Office for Annals Compilation collect the
sacho and compile them into the Veritable Records (sillok, 實錄). Once
compiled, the annals were sealed in archives and could not be revised; not even
the king was permitted to read the sacho or to interfere with their
compilation. The drafts themselves were then washed of their ink so the paper
could be reused, a step with its own name: secho (洗草).

The correspondence to this tool is close enough that the name doubles as a
design summary. Fragments are sacho: independent records written when the
change happens. A release is the compilation of the annals: fragments are
gathered into a permanent document. Released sections are sealed: Sacho never
rewrites them. Deleting consumed fragments after a release is secho. The
institutional insistence that drafts, not recollection, are the source of truth
is the same insistence this tool makes against generating changelogs from
commit messages after the fact.

[1]: https://en.wikipedia.org/wiki/Veritable_Records_of_the_Joseon_Dynasty#Compilation_process
