# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

External-review fixes (two waves).

### Added

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
- `target-python` now gates syntax: after parsing, constructs newer than
  the configured target emit `MLC000` (error) — walrus `:=` and
  positional-only `/` (3.8), `match` (3.10), `except*` (3.11), `type`
  alias statements and PEP 695 type-parameter lists (3.12) — while the
  gated file keeps its AST and is still fully analyzed. `--fix` rolls a
  file back when its edits would introduce gated syntax. Parenthesized
  context managers and f-string `=` are not representable in the bundled
  AST and are documented as ungated.

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
