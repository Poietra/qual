# Knowledge profiles

Reviewed, versioned JSON descriptions of Manim semantics (DESIGN §5.4),
embedded into the binary at compile time and loaded via
`crate::knowledge::load(name)`.

## Shipped profiles

- `v0_20.json` — profile name **`upstream_0_20`** (alias: `v0_20`): curated
  upstream Manim Community 0.20 public semantics. There is no separate
  `upstream_0_20.json` file; `v0_20.json` *is* the upstream profile, named
  to match the `knowledge-profile` config examples in DESIGN §8.2.
- `local_*.json` (future): local fork overlays. An overlay names its base
  by `base_profile` (`name` **and** `source_digest` must match exactly),
  replaces whole symbol entries by qualified key, deletes base symbols via
  `deleted_symbols` and base exports via `deleted_exports`. There is no
  recursive deep merge, and overlay chains (an overlay whose base is itself
  an overlay) are rejected.

## Source digest

`source_digest` of `v0_20.json` is a SHA-256 over the Python sources of the
sibling Manim checkout's package directory (working tree as of 2026-07-17,
0.20.1 lineage, base commit `4d25c031` plus uncommitted fork changes),
computed as:

```sh
cd /home/hosi/manim
find manim -name '*.py' -not -path '*__pycache__*' \
  | LC_ALL=C sort | xargs sha256sum | sha256sum
```

i.e. the digest of the `sha256sum` manifest (per-file hash + path lines) of
all `manim/**/*.py` files in byte-wise sorted order. It covers Python
sources only — no assets, docs, or build metadata.

## Review rules

- Profiles are generated/curated from static Manim source, reviewed by a
  human, and committed; they are never regenerated during a lint run.
- Absent optional facts mean "not curated", never "false".
- No timestamps; the same source must produce a byte-identical profile.
- Keys are kept sorted for reviewable diffs.
