Design
======

This document states the design principles for the Sacho documentation site
and everything else that carries the project's visual identity: the landing
page, social preview images, and future material. It exists so that gradual
changes to the site do not erode the identity one small decision at a time.
When a change conflicts with this document, either the change is wrong or this
document should be revised first.


Brand essence
-------------

Sacho is built around a discipline: changelog entries written by humans, for
users, at the time of the change. The tool exists because discipline does not
scale. The site's job is to argue for the practice before it presents the
features.

The audience is maintainers of open source projects and other developers who
care how their releases read. They are skeptical of marketing and allergic to
hype. The site persuades them the way [*Philosophy*](../PHILOSOPHY.md) does:
by argument, not by enthusiasm.

The visual identity comes from the project's namesake. Sacho (史草) were the
draft records of Joseon court historians: ink on paper, written as events
happened, compiled into sealed annals. The site takes its materials from those
records: paper-colored backgrounds, ink-colored text, and a single cinnabar
accent based on the red of official seal stamps (印朱). The register is
documentary, not decorative.


Voice and tone
--------------

Site copy follows the same rules Sacho imposes on changelog entries. Write for
the reader, say what a thing does, and stop.

 -  State positions plainly. “Sacho refuses to generate changelogs from
    commits” is on brand. Softening it into a feature bullet is not.

 -  No marketing superlatives. Never “blazingly fast,” “powerful,” “seamless,”
    “delightful,” or any adjective doing the work an example should do. If a
    claim matters, show the mechanism or the output instead.

 -  No exclamation marks. No emoji in prose. Decorative icons are acceptable
    only where the default theme requires them.

 -  Comparisons with other tools are welcome when they are concrete and fair,
    as in [*Philosophy*](../PHILOSOPHY.md). Name the trade-off, not the loser.

 -  No em dashes. Rewrite with commas, colons, parentheses, or separate
    sentences.

 -  Sentence case for headings and titles.

The landing page may be declarative to the point of bluntness. Short sentences
that take a position are the house style; the philosophy is the pitch, and the
tool enters only at the end.


Color
-----

Both modes use warm neutrals with a single cinnabar accent. Light mode is dark
brown ink on paper tones drawn from hanji. Dark mode keeps the same warmth:
deep brown backgrounds, never a cold blue-gray. Cinnabar is the only hue on
the site.

| Token          | Light                     | Dark                       | Used for                              |
| -------------- | ------------------------- | -------------------------- | ------------------------------------- |
| Paper          | `#faf7f0`                 | `#1c1915`                  | Page background (`--vp-c-bg`)         |
| Paper, alt     | `#f4f0e6`                 | `#16130f`                  | Sidebar, nav (`--vp-c-bg-alt`)        |
| Paper, soft    | `#f1ece0`                 | `#24201a`                  | Code blocks, cards (`--vp-c-bg-soft`) |
| Ink            | `#2b2620`                 | `#ece5d8`                  | Primary text (`--vp-c-text-1`)        |
| Ink, faded     | `#5f584c`                 | `#b3aa99`                  | Secondary text (`--vp-c-text-2`)      |
| Rule           | `#e5dfd0`                 | `#3a342b`                  | Dividers, borders (`--vp-c-divider`)  |
| Cinnabar       | `#b23a24`                 | `#d26a4a`                  | Links, brand (`--vp-c-brand-1`)       |
| Cinnabar, deep | `#9d3018`                 | `#dd7f60`                  | Hover states (`--vp-c-brand-2`)       |
| Cinnabar, fill | `#b23a24`                 | `#b34a2e`                  | Solid buttons (`--vp-c-brand-3`)      |
| Cinnabar, soft | `rgba(178, 58, 36, 0.14)` | `rgba(210, 106, 74, 0.16)` | Tints, badges (`--vp-c-brand-soft`)   |

Exact values may be adjusted for contrast, but adjustments stay inside the
material: warm paper, warm ink, seal red. Text and interactive colors must
meet WCAG AA contrast on their backgrounds in both modes.

Gradients are banned, including the default theme's hero gradient. Cinnabar is
an accent, not a theme: it marks links, actions, and the seal, and it should
stay scarce enough that a stamp of it still draws the eye.


Typography
----------

Three font roles, all self-hosted. No third-party font CDNs.

 -  Headings, the hero, and the landing-page credo are set in *Source Serif
    4*. The serif appears wherever the site speaks in declarations.

 -  Body text is set in *Inter*. Reference material, guides, and tables are
    read for information, and a plain sans keeps them fast to scan.

 -  Code is set in the system monospace stack. Fragments and command output
    are the product; they should look like the reader's own terminal, not like
    a styled exhibit.

Both families ship as variable fonts through their Fontsource packages and are
declared in the theme's CSS, replacing the default theme's font loading.


Logo
----

The mark is a seal: a rounded square of solid cinnabar with the character 史
cut out in the paper color, in the manner of a stamped nakkwan (落款). The
character is the first of 史草 and reads as “history.” Readers who cannot read
the glyph still recognize a seal, which suits a tool whose released history is
sealed.

 -  The mark is a single-color SVG with the glyph outlined as a path. It must
    never depend on an installed font.

 -  On paper backgrounds the seal is cinnabar with a paper knockout. In dark
    mode the same shape uses the dark-mode cinnabar. No other colorways.

 -  The wordmark is “Sacho” in Source Serif 4. Nav bar shows seal plus
    wordmark; the favicon and other small squares use the seal alone.

 -  Do not rotate the seal, add texture or shadows to it, or place it over
    imagery. Leave clear space around it of at least a quarter of its width.


Imagery
-------

The site shows the product's actual material: fragment files, terminal
commands, compiled changelog sections, and diagrams of the workflow. Mermaid
diagrams and folder trees are part of the house style.

Stock photography, 3D renders, glassmorphism, glow effects, and decorative
illustration are all banned. If a section feels bare, the fix is better prose
or a better example, not ornament. The one permitted decorative motif is the
seal itself and thin rules echoing the ruled columns of manuscript paper.


Layout
------

Documentation pages keep the default theme's layout: sidebar, prose column,
outline. The theme changes their material (color, type), not their structure.

The landing page is a document, read top to bottom, and its order is fixed:
installation first, then the argument for the practice, then what the tool
does, then the name, then the invitation to start. The argument is the heart
of the page and gets the most room. Generosity with whitespace is preferred
over boxes; when in doubt, remove the container and keep the text.
