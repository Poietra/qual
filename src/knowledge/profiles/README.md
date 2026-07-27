# Knowledge profiles

Reviewed, versioned JSON descriptions of Manim semantics (DESIGN §5.4),
embedded into the binary at compile time and loaded via
`crate::knowledge::load(name)`.

## Shipped profiles

- `v0_20.json` — profile name **`upstream_0_20`** (alias: `v0_20`): curated
  upstream Manim Community 0.20 public semantics. There is no separate
  `upstream_0_20.json` file; `v0_20.json` *is* the upstream profile, named
  to match the `knowledge-profile` config examples in DESIGN §8.2.
  **Provenance:** the clean upstream base commit `4d25c031`
  (`v0.20.1-49-g4d25c031`, 0.20.1 lineage) of the sibling checkout at
  a local Manim checkout, materialized read-only via `git archive 4d25c031` —
  never the working tree, which carries uncommitted fork changes. Verified
  drift-free against that clean tree by
  `sync_manim_knowledge --manim-ref 4d25c031 --diff`: 0 missing symbols,
  0 contradictions, 0 warnings.
- `local_0_20_1_4d25c031.json` — profile name **`local_0_20_1_4d25c031`**:
  the local-fork overlay on `base_profile = upstream_0_20`. It carries what
  the fork's working tree adds on top of the base commit:
  - the three fork-only `manim.constants` names (`CAIRO_ANTIALIAS_MODES`,
    `VIDEO_ENCODERS`, `X264_PRESETS`) and their star exports, which were
    moved out of the upstream profile because the clean base tree disproves
    them;
  - the fork-only parallel-TeX API symbols
    (`manim.mobject.text.tex_mobject.MathTex.precompile`,
    `manim.utils.tex_file_writing.tex_to_svg_file_async` — the latter is in
    `tex_file_writing.__all__` but *not* star-exported from
    `manim/__init__.py`, so it carries no export entry);
  - the curated `fork_capabilities` block (see below).

  Its own `source_digest` covers the fork working tree it describes
  (as of 2026-07-19). Overlay semantics: an overlay names its base by
  `base_profile` (`name` **and** `source_digest` must match exactly),
  replaces whole symbol entries by qualified key, deletes base symbols via
  `deleted_symbols` and base exports via `deleted_exports`, and — if it
  declares one — replaces the base's `fork_capabilities` block wholesale.
  There is no recursive deep merge, and overlay chains (an overlay whose
  base is itself an overlay) are rejected.

## Fork fast-path capabilities

The overlay's top-level `fork_capabilities` block (model:
`ForkCapabilities`, accessor `KnowledgeProfile::fork_capabilities()` plus
per-capability accessors) is the curated description of the fork's
lint-relevant fast paths (DESIGN §7.3). The upstream profile has **no**
such block, which keeps every fork-gated interpretation (MLP214, MLP217's
shared-cache gate, MLP225, the `cairo_fork_workers` /
`cairo_static_layers` fast-path semantics) inert under `upstream_0_20`.

- `tex_parallel_compile` — the submit-all/collect Future API
  (`tex_file_writing.py`: process-global `ThreadPoolExecutor`, same-key
  future coalescing; `tex_mobject.py MathTex.precompile`). Its
  `entry_points` are validated to be curated symbols of the resolved
  profile, so precompile advice can never cite an API the selected profile
  does not have. `in_flight_blocks_cairo_fork` records that live TeX
  futures force the fork pipeline into serial fallback
  (`cairo_renderer.py _try_start_cairo_fork_job` →
  `_shutdown_tex_compilation_pool_for_fork`).
- `cairo_fork_gate` — the fork-per-play pipeline: `min_workers` (0..1 is
  *unrequested*, never a reported loss), `monotonic_disable: true` (the
  first written play that opens the parent encoder permanently closes the
  renderer to forking — `_begin_parent_cairo_animation`), the exact-type
  `animation_allowlist` / `composition_allowlist`
  (`_cairo_fork_animation_type_is_supported`), identity-checked
  `trusted_rate_functions`, and the structured `blockers` list read from
  `_cairo_fork_pipeline_is_requested` / `_cairo_fork_pre_begin_is_eligible`
  / `_cairo_fork_post_begin_is_eligible`.
- `cairo_static_layers` — layer-plan retention (`save_static_frame_data`,
  `_build_cairo_static_layer_plan`,
  `_cairo_static_layer_inputs_are_trusted`): `min_play_frames: 3`,
  trailing static runs retained above moving objects (MLP209 severity
  input), blockers as curated.
- `cairo_bulk_interpolation` — the packed interpolation fast path
  (`_try_arm_cairo_bulk_interpolation_recipes`): thresholds 12 frames / 4
  members / 192 member×frame amortization, Transform-or-`.animate`-only
  allowlist, updater-bearing families and custom rate/path functions gate
  it out.
- `svg_cache` — MLP217's gate: the process-global `SVG_HASH_TO_MOB_MAP`
  (`svg_mobject.py`), its key components, unbounded growth, and
  copy-on-hit. The map also exists upstream, but only a profile that
  *declares* the semantics enables the rule.
- `continuous_movie_stream` — MLP210's OutputState input: uncached Cairo
  plays merge into one encoder stream only under
  `scene_file_writer.py _continuous_movie_stream_is_safe` conditions.
- `config_keys` — the fork's new `default.cfg` options, for config display
  (local-only keys are inert, not rejected, under profiles lacking them).

Blocker vocabulary is the shared `ForkBlocker` enum (snake_case in JSON,
`ForkBlocker::as_str` matches the wire form). Every capability carries a
`note` citing the fork source it was read from; calibration numbers quoted
there are machine-specific evidence (`docs/research/perf-evidence.md`),
never portable truth. These facts are reviewer-curated — the generator
neither emits nor drift-checks `fork_capabilities`; the drift gate still
verifies the overlay's *symbols* (including the two TeX API entries)
against the fork working tree.

## Source digest

Every `source_digest` is a SHA-256 over the Python sources of the tree the
profile was curated against, computed as:

```sh
find manim -name '*.py' -not -path '*__pycache__*' \
  | LC_ALL=C sort | xargs sha256sum | sha256sum
```

i.e. the digest of the `sha256sum` manifest (per-file hash + path lines) of
all `manim/**/*.py` files in byte-wise sorted order. It covers Python
sources only — no assets, docs, or build metadata.

For `upstream_0_20` the tree is the clean base commit (`git -C
<manim-checkout> archive 4d25c031 | tar -x` into an empty directory, or
equivalently `sync_manim_knowledge --manim-ref 4d25c031`, which reads the
archive in memory). For `local_0_20_1_4d25c031` the tree is the fork's
working tree of that checkout.

## Curated decisions

- `register_font` (`manim.mobject.text.text_mobject.register_font`) is
  star-exported (`text_mobject.__all__`, re-exported by
  `manim/__init__.py` line `from .mobject.text.text_mobject import *`) and
  is curated as a `function` so `from manim import register_font` resolves
  and `MLR117` fires on bare calls.
- `SingleStringMathTex` is star-exported (`tex_mobject.__all__`); the
  export entry backs the already-curated symbol so the `MLR103` / `MLR115`
  constructor lists see explicit imports of it.
- **Generator-backlog widening (wave 6).** The star surfaces of
  `manim.utils.color.manim_colors` (all 89 color constants),
  `manim.utils.color.core`, `manim.constants` (completed), the
  `manim.animation.transform` star-exports, and `manim.utils.bezier` are
  curated from `sync_manim_knowledge` evidence: kinds, bases, and
  signature params are machine-verified; no effects were invented.
  Non-mobject classes (`ManimColor`, `HSV`, `RGBA`,
  `RandomColorGenerator`, the `constants.py` enum types) are curated as
  `constant`: a known module-level name whose call yields Unknown —
  adding a dedicated class kind is not needed by any rule.
- **Transform-family chain-inheritance soundness.** An `animation` entry
  with curated `bases` lets the interpreter chain-inherit
  introducer/remover/cleanup facts, so bases are only curated where the
  subclass keeps its ancestors' played-mobject and cleanup contract.
  Deliberate exceptions (kind curated, bases omitted, reason in each
  entry note): `TransformFromCopy` (constructor swaps its arguments),
  `FadeTransform` (cleanup removes the played group and appends the
  target — neither plain Transform cleanup nor an order-preserving
  replacement), `CyclicReplace` (wraps `*mobjects` into a played `Group`,
  so positional args are family members, not roots). `Swap` keeps its
  `CyclicReplace` base (that chain carries no facts).
  `TransformAnimations` is **not curated at all**: its positional
  arguments are animations, and curating it as an `animation` would turn
  on composition modeling that joins child introducer facts into a wrong
  certainty about the end animation's target (only the start animation's
  mobject is played, `transform.py TransformAnimations.__init__`).
- **`Mobject.align_data` stays uncurated on purpose.** Curated `method`
  dispatch evaluates call arguments without widening them, and
  `align_data` mutates the point data of its mobject *argument*
  (`mobject.py Mobject.align_data`); leaving it uncurated keeps the
  unknown-call widening of both receiver and argument. The other
  family/geometry getters (`get_family`, `family_members_with_points`,
  `get_all_points`, `get_arc_length`) are pure and carry
  `returns_self: false` as a positive no-mutation fact;
  `VMobject.insert_n_curves` returns `self` (machine-verified).
- **`Axes.plot`** is curated as a method entry on `Axes` (defined on
  `CoordinateSystem`, `coordinate_systems.py CoordinateSystem.plot`),
  and `Axes.bases` carries both source bases (`VGroup`,
  `CoordinateSystem`) so chain resolution can reach future
  `CoordinateSystem` helpers; `ParametricFunction` is curated with its
  constructor signature for `MLP221`.
- **Renderer-cost identities.** `ScreenRectangle` and
  `FullScreenRectangle` are curated through their exact frame-module base
  chain so `MLP212` can combine the inherited default 16:9 geometry with a
  matching render profile instead of guessing from a class name. `Surface`
  remains the Cairo-capable class; its note links the
  1,024-face `MLP213` boundary to `docs/research/perf-evidence.md` rather
  than embedding machine timing in rule logic.
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

# drift check against a committed tree instead of the working tree:
# `git archive` materializes it in memory (read-only) — this is how the
# upstream profile is checked against the clean base commit
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --manim-ref 4d25c031 --diff

# fork overlay vs the working tree it describes
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --diff local_0_20_1_4d25c031
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

The ignored drift tests check both provenances: `upstream_0_20` against the
clean base commit must be fully contradiction-free, while its drift against
the fork working tree is informational — the gate only fails on facts the
clean base **also** disproves (i.e. facts that were never true upstream).
The `local_0_20_1_4d25c031` overlay must itself be drift-free against the
working tree it describes.

## Review rules

- Profiles are generated/curated from static Manim source, reviewed by a
  human, and committed; they are never regenerated during a lint run.
- Absent optional facts mean "not curated", never "false".
- No timestamps; the same source must produce a byte-identical profile.
- Keys are kept sorted for reviewable diffs.
