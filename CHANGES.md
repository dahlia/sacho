Sacho changelog
===============

Version 0.3.0
-------------

To be released.

 -  Added `--no-word-wrap` to `sacho preview` and `sacho show` for publishing
    release notes without Sacho's canonical 80-column wrapping.  [[#3]]
 -  Added section patterns for deriving changelog sections from repository
    layouts, so package-oriented repositories no longer need to list every
    package separately.  [[#4]]

[#3]: https://github.com/dahlia/sacho/pull/3
[#4]: https://github.com/dahlia/sacho/pull/4


Version 0.2.0
-------------

Released on July 23, 2026.

 -  Added optional HTTP redirect resolution for fragment references, including
    persistent per-fragment URL pins and command-line overrides for previews,
    synchronization, and releases.  [[#2]]
 -  Added `sacho import-unreleased` for converting an existing materialized
    unreleased region into fragments without rewriting released history.  [[#1]]
 -  Added interactive section suggestions to `sacho init` based on headings
    found across an existing changelog.  [[#1]]

[#1]: https://github.com/dahlia/sacho/pull/1
[#2]: https://github.com/dahlia/sacho/pull/2


Version 0.1.0
-------------

Initial release.  Released on July 21, 2026.
