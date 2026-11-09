---
links:
  '#11': https://github.com/dahlia/sacho/issues/11
  '#12': https://github.com/dahlia/sacho/pull/12
---
 -  Added optional section titles independent of stable IDs and fragment
    directories.  Renaming a heading now preserves `carry` behavior for older
    releases, including documents without section metadata.  [[#11], [#12]]
 -  Added historical section maps with family patterns and one-time
    `carry --map` and `--map-pattern` overrides.  Multiple old headings can
    merge into a current section without rewriting released changelog text.
    [[#11], [#12]]
