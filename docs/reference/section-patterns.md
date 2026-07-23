Section pattern syntax
======================

Section patterns describe a family of sections whose ids and fragment
directories come from the same repository path. They are useful when a
repository has many packages with a regular layout:

~~~~ toml
[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "{name}"
~~~~

This one table covers `packages/core`, `packages/cli`, and every package added
later. A concrete package name is captured from one field and substituted into
the others.


Pattern grammar
---------------

A pattern is a nonempty sequence of slash-delimited segments:

~~~~ text
pattern = segment ("/" segment)*
~~~~

A segment is literal text or literal text with one named capture:

~~~~ text
segment = literal
        | literal? "{" identifier "}" literal?

identifier = ("A"–"Z" | "a"–"z" | "_")
             ("A"–"Z" | "a"–"z" | "0"–"9" | "_")*
~~~~

Capture names are not reserved keywords. `{name}`, `{scope}`, and `{package}`
all work. The name connects corresponding captures across the fields in one
`[[section-patterns]]` table.

Each segment may contain at most one capture. Literal text can appear on either
side, which permits partial-segment captures:

~~~~ text
packages/{scope}/plugin-{name}
~~~~

This pattern matches `packages/acme/plugin-http` with `scope = "acme"` and
`name = "http"`.

Use doubled braces for literal brace characters. `{{draft}}-{name}` matches a
segment such as `{draft}-core`.

A captured value is one nonempty path segment. It cannot be `.`, `..`, or
contain `/` or `\`. Sacho applies these rules when matching and rendering, so a
capture cannot escape its fragment directory.

Generated fragment directories must resolve to their rendered path exactly.
This rejects symbolic-link and case aliases that could make one physical
fragment appear in two generated sections. Use an explicit `[[sections]]`
entry when a section intentionally needs a configured alias.


The four fields
---------------

`source` is a repository-relative subtree pattern. It establishes the capture
names for the table and must contain at least one capture. Each capture name
must appear once.

`id` renders the section heading and supplies the value accepted by
`--section`. It must contain every capture from `source` exactly once and
cannot introduce another capture.

`directory` renders a path below `[fragments].directory`. It follows the same
capture rule as `id` and must also be a safe relative path.

`paths` controls missing-fragment attribution. Its three states are distinct:

| Configuration | Attribution                                           |
| ------------- | ----------------------------------------------------- |
| Field omitted | The matched `source` subtree and all descendants      |
| Nonempty list | Only descendants matched by one of the rendered globs |
| `paths = []`  | No changes attributed specifically to the section     |

Every `paths` entry must start with the complete `source` pattern. Later
segments use Sacho's ordinary glob syntax. They may reuse captures established
by `source`:

~~~~ toml
[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "{name}"
paths = [
  "packages/{name}/src/**",
  "packages/{name}/package.json",
]
~~~~

The captured package name is treated as literal text even if it contains glob
characters.


Resolution and precedence
-------------------------

Sacho resolves a concrete section when it sees a matching source path,
fragment directory, changelog heading, or `--section` argument. It does not
need to scan the source tree first. This also lets `carry` and
`import-unreleased` resolve a historical section after its package has been
deleted.

Explicit `[[sections]]` entries take precedence when an id or fragment
directory would collide with a generated section. Use an explicit entry for an
exception to an otherwise regular layout.

If two patterns match the same concrete value and neither is overridden,
Sacho reports an ambiguity instead of choosing one. Exact duplicate tables are
rejected while loading *sacho.toml*.

Rendered headings follow this order:

1.  Explicit sections, in declaration order.
2.  Generated sections, grouped by pattern declaration order and sorted by id
    within each pattern.
3.  Unknown legacy fragment directories, sorted by id.
