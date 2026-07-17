# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
