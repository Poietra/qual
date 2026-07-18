# Pinned Manim example_scenes snapshots

These files are unmodified snapshots of `example_scenes/` from the
[Manim Community](https://github.com/ManimCommunity/manim) repository at
commit `4d25c031ffe71c602e20935afd54a96f33545a6e` (the same base commit
the shipped knowledge profile `upstream_0_20` is pinned to; the files are
byte-identical to that commit).

They are the human-adjudicated real-world corpus verdict this repository's
review history is anchored on: across all four files, the default check
produces exactly one diagnostic — MLP208 (info) on `basic.py` — and
nothing else. Every other diagnostic on these files is by definition a
false positive.

## License and attribution

Manim is distributed under the MIT license:

- Copyright (c) 2018 3Blue1Brown LLC (`LICENSE` in the Manim repository)
- Copyright (c) 2024, the Manim Community Developers (`LICENSE.community`)

These snapshots are redistributed here under those MIT terms, unmodified,
solely as linter test fixtures. See the upstream repository for the full
license texts.

Do not edit these files. If upstream examples change, take a fresh
snapshot at a new pinned commit, re-adjudicate every diagnostic, and bump
the affected `label_revision` values in `tests/corpus/manifest-v1.json`
(protocol: CONTRIBUTING.md, "Corpus labeling").
