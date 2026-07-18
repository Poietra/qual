# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

External-review fixes.

### Added

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
  `--fix` round-trips the matching encode.
- MLP206 message and docs corrected: such a play renders a single frame
  sampled near the start state (not the final state).

### Fixed

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
- 52 implemented rules across four families — 25 MLC lifecycle/correctness,
  14 MLR rendering, 6 MLP performance, 7 MLD determinism/portability — each
  with golden fixture tests and a documentation page; the remaining 40
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
