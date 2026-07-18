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

## Curated decisions

- `register_font` (`manim.mobject.text.text_mobject.register_font`) is
  star-exported (`text_mobject.__all__`, re-exported by
  `manim/__init__.py` line `from .mobject.text.text_mobject import *`) and
  is curated as a `function` so `from manim import register_font` resolves
  and `MLR117` fires on bare calls.
- `SingleStringMathTex` is star-exported (`tex_mobject.__all__`); the
  export entry backs the already-curated symbol so the `MLR103` / `MLR115`
  constructor lists see explicit imports of it.
- **`font_size` mutation is constructor-only in `MLR115` on purpose.** The
  Text/TeX families expose no font-size *method*: the runtime mutator is
  the `font_size` **property setter** (`text_mobject.py Text.font_size`,
  `tex_mobject.py SingleStringMathTex.font_size`), i.e. an attribute
  assignment (`text.font_size = x`), which is outside call facts. That
  setter also raises `ValueError` itself for values `<= 0`, while the
  constructors store the value unchecked — so the constructor keyword is
  exactly the silent-failure surface the rule must cover, and no curated
  font-size mutator method exists to add.

## Candidate generation and drift check

`sync_manim_knowledge` (DESIGN §5.4; `src/bin/sync_manim_knowledge.rs`,
library API in `crate::knowledge::generator`) statically reads a Manim
checkout — never importing or executing it — and extracts what a parser can
safely know: public classes and base chains, method definitions,
returns-`self` evidence, `__all__` lists, and the star-export closure of
`manim/__init__.py`.

```sh
# reviewable candidates (byte-identical for identical input)
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --emit candidates.json

# drift check against the shipped profile (default upstream_0_20);
# exit 1 when the profile contradicts the source
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --diff --report drift.json
```

Every generated entry is marked `"generated": true`; curated-only semantic
fields (`effects`, introducer / remover, renderer notes) are never emitted —
the generator does not invent semantics. The diff reports (a) curated
symbols missing from the source, (b) curated `bases` / `returns_self` /
`exports` facts the source contradicts (the DESIGN §11.2 layer-9 gate,
also run by `cargo test --test knowledge_drift -- --ignored`), and
(c) per-module coverage gaps. Unverifiable facts stay warnings. A
`source_digest` mismatch is informational only; the digest itself follows
the manifest recipe above. Humans review candidates and edit profiles by
hand — the tool never writes into this directory.

## Review rules

- Profiles are generated/curated from static Manim source, reviewed by a
  human, and committed; they are never regenerated during a lint run.
- Absent optional facts mean "not curated", never "false".
- No timestamps; the same source must produce a byte-identical profile.
- Keys are kept sorted for reviewable diffs.
