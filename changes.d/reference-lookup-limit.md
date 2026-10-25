---
links:
  '#10': https://github.com/dahlia/sacho/pull/10
  '#9': https://github.com/dahlia/sacho/issues/9
---
 -  Fixed false errors about changes to released links from `sacho sync` and
    `sacho fmt` in changelogs whose Markdown reference links exceed the
    parser's reference expansion limit. Validation also catches destination
    and title changes that the same limit previously hid.  [[#9], [#10]]
