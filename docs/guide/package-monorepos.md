Package monorepos
=================

A package monorepo should not need one Sacho table per package. Use a section
pattern when package paths, changelog headings, and fragment directories share
the same names.


Start with one package family
-----------------------------

Suppose the repository has this layout:

~~~~ text
packages/
├── cli/
├── core/
└── parser/
~~~~

Configure the package family once:

~~~~ toml
[check]
paths = ["packages/**"]

[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "{name}"
paths = [
  "packages/{name}/src/**",
  "packages/{name}/package.json",
]
~~~~

A first-time interactive `sacho init` can suggest this shape when an existing
changelog has headings such as `@acme/core` and `@acme/parser`, and both names
map to sibling directories below *packages/*. The suggestion is made only when
there is one unambiguous common parent. You can decline it and configure the
selected headings as explicit sections instead.

A change to *packages/parser/src/lib.rs* now requires a fragment in the
`@acme/parser` section. Create it with:

~~~~ sh
sacho add --section @acme/parser faster-lookahead
~~~~

Sacho writes *changes.d/parser/faster-lookahead.md*. The compiled changelog
uses `@acme/parser` as its section heading.

Adding *packages/formatter/* needs no *sacho.toml* change. The same command
shape works immediately:

~~~~ sh
sacho add --section @acme/formatter stable-output
~~~~

The section name is derived from the argument, not from an inventory of
directories. A misspelled but syntactically valid package name can therefore
create a new section. Check the generated path before committing it.


Capture part of a directory name
--------------------------------

Captures do not need to occupy a whole segment. A scoped plugin layout can use
two captures and a literal prefix:

~~~~ text
packages/
└── acme/
    ├── plugin-http/
    └── plugin-sql/
~~~~

~~~~ toml
[[section-patterns]]
source = "packages/{scope}/plugin-{name}"
id = "@{scope}/{name}"
directory = "{scope}/plugin-{name}"
~~~~

`packages/acme/plugin-http` maps to section `@acme/http` and fragment directory
*changes.d/acme/plugin-http/*.


Keep an exceptional package explicit
------------------------------------

An old package may not fit the common layout. Declare that exception as an
ordinary section and keep the general pattern:

~~~~ toml
[[sections]]
id = "@acme/legacy"
directory = "retired"
paths = ["vendor/legacy/**"]

[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "{name}"
~~~~

The explicit section wins over the generated `@acme/legacy` instance. Changes
under *vendor/legacy/* require a fragment in *changes.d/retired/*, while other
packages continue to use the pattern.


Use more than one package family
--------------------------------

Declare separate patterns when the repository has unrelated layouts:

~~~~ toml
[[section-patterns]]
source = "packages/{name}"
id = "@acme/{name}"
directory = "packages/{name}"

[[section-patterns]]
source = "tools/{name}"
id = "tool:{name}"
directory = "tools/{name}"
~~~~

Package sections render before tool sections because their pattern appears
first. Within each family, Sacho sorts the concrete section ids.

See [*Section pattern syntax*](../reference/section-patterns) for the complete
grammar, field constraints, `paths` behavior, and ambiguity rules.
