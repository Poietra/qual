# manim-lint

`manim-lint` is a static analyzer for Manim Community projects, implemented in
Rust. It reads Python source with a Rust Python parser and never imports Manim
or executes the analyzed project.

The current implementation covers the **Phase 0 foundation**, the **Phase 1
name resolution and direct-call rules**, and the **Phase 2/3 semantic
foundations** described in [`DESIGN.md`](DESIGN.md):

- source loading with PEP 263 encoding declarations, newline preservation,
  and UTF-8-byte-column to Unicode-character-column conversion (Japanese
  source is a first-class test case)
- `MLC000` syntax-error diagnostics; a broken file never stops analysis of
  the other files
- configuration from `[tool.manim-lint]` in `pyproject.toml`, a minimal
  `manim.cfg` reader, and the explicit precedence chain
  `CLI > selected profile > pyproject base > manim.cfg > builtin defaults`
- inline suppressions (`# manim-lint: ignore[...]`, standalone comments,
  header-only `file-ignore[...]`); invalid suppressions warn as `MLC001`
- deterministic, byte-stable output: `concise`, `full`, `github`
  annotations, JSON matching `schemas/diagnostics-v1.json`, and SARIF 2.1.0
  (`--format sarif`, generated without external SARIF dependencies)
- baselines (`--write-baseline` / `--baseline`) whose fingerprints follow
  `schemas/baseline-v1.json` and never contain line numbers, so inserting
  unrelated lines does not invalidate entries
- fix application (`--fix`, `--unsafe-fixes`): safe/unsafe separation,
  overlap rejection, re-parse validation with per-file rollback, and
  Unicode-correct span editing
- a versioned Manim 0.20 knowledge profile (no Manim import, ever), import
  and alias resolution including `from manim import *`, a project-wide
  symbol/class-hierarchy index, and qualified Manim call facts with
  candidate sets that degrade to `Unknown` instead of guessing
- the **Phase 1 rule set**, implemented and enforced through golden fixture
  tests with alias-import parity and suppression coverage:
  - lifecycle: `MLC101`–`MLC106`, `MLC109`, `MLC122`, `MLC126`, `MLC127`
  - rendering: `MLR101`, `MLR103`–`MLR106`, `MLR115`, `MLR117`, `MLR124`,
    `MLR126`
  - several emit fixes (safe: `MLC127`, `MLR104`; unsafe: e.g. `MLC102`,
    `MLC106`, `MLC122`, `MLR103`, `MLR105`, `MLR117`, `MLR124`)
- the **lifecycle abstract interpreter** (DESIGN §3/§5.6): intra-function
  CFG, interprocedural helper summaries with SCC fixpoints, per-Scene MRO
  composition with `super()` dispatch, allocation-site identity, Scene
  membership/order/updater tracking, and play-group semantics — exposed to
  rules as `LifecycleFacts` on the `RuleContext`
- the **symbolic cost model** (DESIGN §4): hot-context propagation
  (updaters, `always_redraw`, stop conditions, interpolate overrides),
  frame-count intervals from literal durations (never fabricated numbers),
  and machine-readable evidence — exposed to rules as `CostFacts`

All other catalog rules (`MLC107`–`MLC129`, the remaining `MLR` rules, all
`MLP` and `MLD` rules) are still **reserved**: `manim-lint rules` lists them
as such and they are never presented as checked. The cache and the
`manim-lint cost` command belong to later phases and report clear "not
implemented" errors.

## Build and run

A Rust toolchain (1.85+) is required.

```bash
cargo build --release
cargo run -- check .
cargo run -- check scenes --format json
cargo install --path .   # installs the `manim-lint` binary
```

## Commands

```text
manim-lint check [PATH...]   # analyze Python sources
manim-lint explain RULE      # print rule documentation
manim-lint rules             # list all rule IDs with phase and status
manim-lint config            # print the resolved effective configuration
manim-lint cost PATH         # Phase 3; exits 2 with a clear message
```

`check` options include `--select` / `--ignore`, `--min-confidence`,
`--fail-level`, `--profile`, `--renderer`, `--fps`,
`--resolution WIDTHxHEIGHT`, `--format concise|full|json|sarif|github`, and
`--statistics`. `--write-baseline PATH` records the current diagnostics
(the run's exit code is unchanged); `--baseline PATH` filters already-known
diagnostics out before rendering and exit-code computation, and a corrupt
or wrong-schema baseline file exits 2 with a clear message. `--fix` applies
safe fixes (`--unsafe-fixes` also applies unsafe ones). `--no-cache` is an
accepted no-op because no cache exists yet.

Exit status is `0` when no reported diagnostic reaches `fail-level`, `1` when
one does, and `2` for command-line, configuration, or internal errors.

## Configuration

Configuration is read from `[tool.manim-lint]` in `pyproject.toml` (found by
walking up from the checked path); render profiles are
`[[tool.manim-lint.profile]]` entries. When `respect-manim-cfg` is enabled
(the default), `manim.cfg` supplies resolution/fps/renderer defaults below
the pyproject settings. Unknown keys, unknown rule selectors, duplicate
profile names, and unknown profile references are configuration errors
(exit 2).

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`DESIGN.md` is the authoritative specification; `AGENTS.md` describes the
implementation order. Rule documentation lives in `docs/rules/`, and
`tests/fixtures/smoke/` is a minimal end-to-end fixture project.
