# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

External-review fixes (two waves) and structural hardening (knowledge
provenance split, release quality gates, frontend fact promotion,
interpreter modularization).

### Added

- Release quality gates (DESIGN §11.4): a labeled corpus gate
  (`tests/corpus/manifest-v1.json`, 35 cases pinning sha256 and exact
  expected diagnostics — true positives and false-positive guards across
  all four rule families, including pinned real-Manim `example_scenes`
  snapshots and the adversarial review probes) that runs inside
  `cargo test`; an explicit benchmark gate over a pinned 10k-LOC fixture
  (cold ≤ 2 s, peak RSS < 300 MiB, asserted only on the reference
  machine; the warm ≤ 0.5 s budget is recorded but unenforced until the
  on-disk cache exists); and a scheduled knowledge-drift CI job against
  the pinned upstream base commit.
- A shipped `local_0_20_1_4d25c031` knowledge-profile overlay carrying
  the sibling fork's working-tree additions on top of `upstream_0_20`
  (currently the three fork-only `manim.constants` names
  `CAIRO_ANTIALIAS_MODES`, `VIDEO_ENCODERS`, `X264_PRESETS`).
- `sync_manim_knowledge --manim-ref <commit>` generates candidates from a
  clean `git archive` of a commit instead of the working tree, so
  profile provenance can be checked against pristine upstream.
- Frontend statement and binding facts (`src/frontend/statements.rs`):
  per-call enclosing-statement spans and roles (bare expression, `with`
  context, assignment RHS, return value, decorator), and per-file import
  binding facts with unified rebind poisoning and canonical dotted-path
  resolution, exposed to rules via
  `RuleContext::statement_facts()`/`binding_facts()`.
- Baseline files now carry a `scene_attribution: "attributed"` provenance
  marker (additive; `schema_version` stays 1). In attributed files an
  empty `scene` means literally "outside any Scene" and matches exactly,
  so a module-level entry can no longer wildcard-suppress a
  same-fingerprint diagnostic inside a scene. Files without the marker are
  read as legacy pre-attribution baselines and keep their empty-scene
  wildcard behavior.
- Loop-aware play repetition facts: a `play`/`wait` inside a loop whose
  trip count is a literal `range(...)` multiplies its frame contribution
  by the trip-count interval; an unknown trip count opens the upper bound
  instead of counting the play once. Repetition bounds compose through
  helper inlining (call-site loops × helper-internal loops).
- Plays and waits inside `self.<helper>()` (and `super().<helper>()`)
  methods now materialize as real lifecycle play facts: resolvable helper
  calls are inlined during scene interpretation (bounded depth, recursion
  falls back to the effect summary), so MLC104/MLC108/MLC117/MLR102/
  MLP226 and the `cost` command see helper-reached plays with their sites
  in the helper body, a recorded `call_path` of the inlining call sites,
  call-site branch/loop certainty, and exact per-animation argument
  facts. Lifecycle state queries are method-scoped, so liveness resolves
  the correct pre-play state at helper play sites.
- `target-python` now gates syntax completely, with an honest floor:
  after parsing, a gate over the AST, the token stream, and f-string
  token text emits `MLC000` (error) for every parse-level construct
  newer than the configured target — `async`/`await` syntax outside
  `async def` (3.7); walrus `:=`, positional-only `/`, and f-string
  self-documenting `=` (3.8); relaxed decorators and parenthesized
  context managers with `as` (3.9); `match` (3.10); `except*` and
  PEP 646 `*` unpacking in subscripts/annotations (3.11); `type` alias
  statements, PEP 695 type-parameter lists, and PEP 701 f-string
  expressions with backslashes/newlines/comments (3.12) — while the
  gated file keeps its AST and is still fully analyzed. A file the gate
  passes silently is guaranteed parseable by the target's own parser;
  each minimum version is oracle-checked against CPython
  `ast.parse(feature_version=...)` where that oracle gates the
  construct. `--fix` rolls a file back when its edits would introduce
  gated syntax, token-detected constructs included. The accepted
  `target-python` range shrank from 3.0–3.12 to 3.6–3.12: below 3.6 the
  guarantee cannot be kept (Python 3.6's own additions include
  oracle-unverifiable constructs), so older targets are now explicit
  exit-2 configuration errors instead of silently unenforced promises.

- Per-updater execution liveness (`src/cost/liveness.rs`): every
  hot-context performance fact is now gated on plays/waits where the
  callback **provably** executes per frame (registration live in the heap,
  host present and not suspended by the play's own animations —
  `suspend_mobject_updating` honored at constructor and play level —
  and frames actually rendered, including `frozen_frame` handling), with
  three-valued verdicts and auditable `execution` play-span evidence on
  diagnostics.
- Configuration is validated honestly (exit 2): zero/negative/non-finite
  frame rates and zero-dimension resolutions from any tier (CLI, profile,
  `manim.cfg`), non-empty `stub-paths` (unimplemented), `manim-version`
  outside the loaded knowledge profile's range, and malformed or
  out-of-range `target-python` (3.0–3.12, the bundled parser's grammar).
  `manim-lint config` gained an `enforcement` section.
- Baseline fingerprints now record the qualified enclosing Scene class in
  the `scene` field, so identical findings in different scenes no longer
  collide; old baselines with an empty `scene` still match as a wildcard.

### Changed

- The `upstream_0_20` knowledge profile now describes only the clean
  upstream base commit `4d25c031`: the three fork-only `manim.constants`
  names (`CAIRO_ANTIALIAS_MODES`, `VIDEO_ENCODERS`, `X264_PRESETS`)
  moved to the new `local_0_20_1_4d25c031` overlay, and resolve to
  Unknown under `knowledge-profile = "upstream_0_20"`.
- Helper inlining during scene interpretation is bounded by call-cycle
  detection (a helper already on the active inlining path falls back to
  its effect summary) instead of a fixed depth limit, so deep
  non-recursive helper chains inline fully.
- Cost execution facts are keyed by the helper `call_path` of the play
  they prove, so the same helper reached through different call sites no
  longer merges its per-frame execution evidence across paths.
- Frontend fact computation is gated on the selected rule capabilities:
  a run that selects no rule needing statement, binding, or lifecycle
  facts skips computing them.
- The lifecycle interpreter (`src/semantic/interpreter.rs`, 7,602 lines)
  is split into nine cohesive modules under
  `src/semantic/interpreter/`; the public API is frozen via `pub use`
  re-exports and lint output is byte-identical.
- Rule-layer private AST walks are gone (DESIGN §5.6): MLR106/107/112/
  117/119/121/127 and MLD301/302/304/307 now consume promoted frontend
  facts instead of re-walking the module tree; the remaining two local
  traversals are fact-anchored and reuse the canonical frontend walker.
- MLP201/MLP202/MLP203/MLP204/MLP224/MLP226 fire only with at least one
  proven execution play and quantify over proven plays only;
  MLP211/MLP216 use qualitative wording when execution is unproven; all
  hot-context rules are silent when the callback provably never runs.
  The `cost` command reports proven execution plays per callback and
  never fabricates invocation counts.
- MLC104 diagnoses from lifecycle play facts: fires on played
  `wait`/`pause`/`play` with literal non-positive durations (including
  per-animation `run_time` literals inside `play(...)`), stays silent on
  unplayed constructions and when a play-level `run_time` kwarg overrides
  constructor literals.
- Inline suppressions cover whole statements (all continuation lines of a
  multi-line statement); compound-statement suppressions cover only the
  header up to its colon.
- PEP 263 handling matches CPython: a UTF-8 BOM with a conflicting
  non-UTF-8 coding cookie is reported as an `MLC000` decode diagnostic;
  latin-1-family cookies decode as true Latin-1 (not windows-1252) and
  `--fix` round-trips the matching encode. Cookie placement follows
  CPython's exact rule (line 2 only after a blank/comment line 1,
  including `blank_re`/`cookie_re` backtracking); `iso-8859-9` and
  `iso-8859-11` decode with the 0x80–0x9F range as C1 controls (the only
  divergence between the WHATWG codecs and CPython, verified
  byte-for-byte) with a symmetric `--fix` re-encode; CPython-only alias
  spellings (`latin5`, `thai`, `iso_ir_148`, ...) now resolve.
- MLP206 message and docs corrected: such a play renders a single frame
  sampled near the start state (not the final state).

### Fixed

- A literal play-level `run_time` kwarg now decides the whole-play
  duration exactly regardless of animation identity (scene.py
  `compile_animations` `setattr`s the kwarg onto every animation): a
  `.animate` builder on an untracked helper argument — a loop-carried
  parameter, an unresolvable factory result — no longer widens the play
  duration to unknown, so cost frame estimates keep their numbers per
  execution. The reverse direction is fixed too: a *non-literal*
  `run_time` (or a `**kwargs` splat that may carry one) overrides
  constructor literals with an unknowable value, so a play can no longer
  report an exact duration its kwarg overrides at run time.
- `.animate` builder facts are keyed by execution context (builder site ×
  helper call path, like per-call-site play facts), so joins happen only
  within one execution; the per-site view merges executions soundly — a
  builder site reached with different live targets drops its identity
  and epoch claims instead of pairing one execution's creation epoch
  with another's play epoch.
- A latent false-positive class in name-rebind detection: the four
  hand-rolled binder collectors are unified into one canonical
  poisoning definition, so MLD307 no longer claims the builtin when a
  hot-callback callee name was rebound via a relative import
  (`from . import open`), and MLR106/MLR121/MLR127 now see
  lambda-parameter, walrus, comprehension, `except`, and `match` binders
  they previously missed (fire → silence in those corners).
- Frame totals across multiple plays now apply `ceil(duration × fps)` per
  play and sum the counts (Manim renders one `np.arange` grid per play),
  instead of ceiling the summed duration once — two 1 ms plays at 60 fps
  are 2 frames, not 1. Applies to proven-execution frame estimates,
  the `cost` report quantities, and the MLP219/MLP220 frame evidence.
- Directory scanning surfaces unreadable directory entries as errors
  (exit 2) instead of silently skipping them; an unreadable directory was
  already an error, per-entry read failures now are too.
- Directory walking no longer hangs on symlink cycles (canonicalized
  visited set; each file reported once).

## [0.1.0] - 2026-07-18

First release: a standalone Rust static analyzer for Manim Community 0.20
projects that never imports or executes the analyzed code.

### Added

- `check`, `explain`, `rules`, `config`, and `cost` commands.
- Source loading with PEP 263 encoding declarations, newline preservation,
  and UTF-8-byte to Unicode-character column conversion; `MLC000` syntax
  diagnostics that never stop analysis of other files.
- Versioned Manim 0.20 knowledge profile (`upstream_0_20`), import/alias
  resolution including `from manim import *`, a project-wide symbol and
  class-hierarchy index, and qualified Manim call facts that degrade to
  `Unknown` instead of guessing.
- Lifecycle abstract interpreter: intra-function CFG, interprocedural helper
  summaries, per-Scene MRO composition with `super()` dispatch,
  allocation-site identity, scene membership/order/updater tracking, and
  play-group semantics, exposed to rules as `LifecycleFacts`.
- Symbolic cost model: hot-context propagation, frame-count intervals from
  literal durations only, machine-readable evidence, exposed as `CostFacts`;
  the `cost` command reports per-scene plays, hot contexts, per-frame
  constructions, and resource-key growth.
- 79 implemented rules across four families — 28 MLC lifecycle/correctness,
  23 MLR rendering, 21 MLP performance, 7 MLD determinism/portability — each
  with golden fixture tests and a documentation page; the remaining 13
  catalog IDs are listed as reserved and never fire.
- Severity/confidence separation, definite (all-paths) evidence gating, and
  specificity dedup via rule `supersedes` metadata.
- Configuration from `[tool.manim-lint]` in `pyproject.toml` with render
  profiles, a minimal `manim.cfg` reader, and the precedence chain
  `CLI > profile > pyproject > manim.cfg > defaults`; `per-file-ignores`.
- Inline suppressions (same-statement, next-statement, header `file-ignore`)
  with the `MLC001` unknown-ID warning.
- Output formats: `concise`, `full`, `json` (`schemas/diagnostics-v1.json`),
  SARIF 2.1.0, and GitHub Actions annotations; deterministic, byte-stable
  output; exit codes 0/1/2.
- Baselines (`--write-baseline` / `--baseline`) with line-number-independent
  fingerprints (`schemas/baseline-v1.json`).
- Fix application: `--fix` for safe fixes and `--unsafe-fixes` for unsafe
  ones, with overlap rejection, re-parse validation, and per-file rollback.
