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


Version-control integration
---------------------------

Sacho can enforce that commits changing configured source paths also carry a
changelog fragment. Select the repository's VCS in *sacho.toml*:

~~~~ toml
[vcs]
preset = "git" # "jj", "hg", or "none"

[check]
paths = ["src/**"]
~~~~

Then check commits after a base revision with `sacho check --base <revision>`.
Git also supports `sacho check --staged`. The `none` preset explicitly skips
VCS-backed checks while leaving fragment validation and changelog consistency
checks enabled.

Each preset supplies three subprocess commands. Repositories can override one
command without repeating the other two:

~~~~ toml
[vcs]
preset = "git"

[vcs.commands]
message = ["my-vcs-wrapper", "message", "${commit}"]
~~~~

The first array element is the executable and every remaining element is one
literal argument; Sacho never invokes a shell. The commits command uses
`${base}`, while changed-paths and message use `${commit}`. An overridden query
is the complete query and does not invoke commands from its preset behind the
scenes. The built-in Mercurial preset handles parent comparisons internally.

The query output contracts are:

 -  `commits` emits zero or more nonempty UTF-8 commit identifiers, oldest
    first and one per line. The final line may end in LF; CRLF is also
    accepted.
 -  `changed-paths` emits concatenated NUL-terminated name-status records.
    Ordinary records are `A\0PATH\0`, `D\0PATH\0`, `M\0PATH\0`, or
    `T\0PATH\0`, where `T` denotes a file type change. Copy and rename records
    are `C[SIMILARITY]\0OLD_PATH\0NEW_PATH\0` and
    `R[SIMILARITY]\0OLD_PATH\0NEW_PATH\0`, where the optional similarity is a
    decimal percentage from 0 through 100. Status fields are UTF-8 and paths
    are nonempty repository-relative platform paths; path bytes need not be
    UTF-8 on Unix. Nonempty output ends in NUL. A copied or renamed fragment
    counts as new content only when similarity is present and below 100;
    omitting it does not satisfy fragment coverage.
 -  `message` emits the complete UTF-8 commit message verbatim. Sacho does not
    trim it.

`sacho init` installs Git merge attributes and drivers in Git repositories. In
Mercurial repositories it installs an idempotent block in *.hg/hgrc* containing
a successful-merge update hook and, when changelog materialization is enabled,
the changelog merge driver. Jujutsu has no per-path merge-driver hook; after
resolving a concurrent fragment merge, run `sacho sync --force`.


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
