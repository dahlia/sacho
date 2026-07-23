Fragments
=========

A fragment is a Markdown file containing the unreleased notes for one
user-visible change. It lives in the fragment directory, *changes.d/* by
default, and should be committed with the code it describes.

Create one with a topic-based name:

~~~~ sh
sacho add clear-function
~~~~

The command prints the new path. Open that file and write the entry:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.
~~~~

The filename is part of the workflow, not part of the published changelog. A
name such as *clear-function.md* tells a later contributor which fragment to
edit. An issue number such as *842.md* does not.


One top-level list
------------------

After optional YAML frontmatter, a fragment must contain exactly one top-level
unordered list. It may have one item or several related items:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.
 -  Added `is_empty()` for checking whether a collection has entries.
~~~~

List items may contain paragraphs, nested lists, block quotes, and fenced code
blocks. Headings, paragraphs, and a second list are not allowed at the top
level. This constraint lets Sacho combine fragments without guessing where one
entry ends and another begins.

Run `sacho fmt` after editing. It puts frontmatter and Markdown into Sacho's
normal form, then refreshes the materialized changelog if materialization is
enabled.


Edit the story before release
-----------------------------

Suppose a release first adds `remove_all()` and later renames it to `clear()`.
Users never encounter `remove_all()`. Edit the original fragment so the next
changelog says only:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.
~~~~

Do not add a second entry describing the rename. The fragments should tell the
history users will experience, not every intermediate state in development.


Priority
--------

Entries normally sort by their text. A fragment can use a numeric `priority`
when one group must appear earlier or later:

~~~~ markdown
---
priority: -10
---

 -  Fixed a vulnerability that could expose session tokens.
~~~~

Lower priorities sort first. All items in the fragment share its priority and
keep their order relative to each other. Most fragments should omit
frontmatter.

Unknown frontmatter keys produce warnings rather than becoming part of Sacho's
sorting model. Keeping frontmatter small makes the released order predictable.


References
----------

References can stay in the prose as shortcut links:

~~~~ markdown
 -  Added `clear()` to remove every entry at once.  [[#842]]
~~~~

The corresponding URL template belongs in *sacho.toml*:

~~~~ toml
[links]
"#" = "https://github.com/acme/widget/issues/{n}"
~~~~

Sacho emits the link definitions in the compiled changelog. `sacho check`
reports a reference whose sigil has no configured template.

For trackers that redirect a shared reference route to a more specific target,
run `sacho resolve-links`. Sacho follows the redirect chain once and pins the
final URL without moving the reference out of the entry text:

~~~~ markdown
---
links:
  "#842": https://github.com/acme/widget/pull/842
---

 -  Added `clear()` to remove every entry at once.  [[#842]]
~~~~

Pins travel with their fragments and take precedence over the repository URL
template. Each pin key must be a configured reference label used by that
fragment. Delete a pin to request that reference again.
