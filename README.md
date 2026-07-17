# manim-lint

`manim-lint` is a static analyzer for Manim Community projects, implemented in
Rust. It reads Python source with a Rust Python parser and never imports Manim
or executes the analyzed project.

The current implementation covers the **Phase 0 foundation**, the **Phase 1
name resolution and direct-call rules**, the **Phase 2/3 semantic
foundations and their state-dependent rules**, and the **Phase 4
determinism/portability rules** described in [`DESIGN.md`](DESIGN.md):

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
- the **Phase 2 state-dependent rules** over those facts, firing only on
  definite (all-paths) evidence:
  - lifecycle: `MLC107`, `MLC108`, `MLC110`, `MLC113` (safe kwargs-move
    fix), `MLC115`, `MLC117`, `MLC119`–`MLC121`, `MLC124`, `MLC125`,
    `MLC128`, `MLC129`
  - rendering: `MLR102`, `MLR113`, `MLR114`, `MLR125`, `MLR127`
- the **first Phase 3 performance tranche**: `MLP201`, `MLP204`–`MLP206`,
  `MLP220`, `MLP226`, plus the `manim-lint cost` command (per-scene play
  list with frame intervals, hot contexts with provenance, per-frame
  constructions, resource-key growth; unknown durations are printed as
  unknown, never as fabricated numbers)
- the **Phase 4 determinism/portability rules**: `MLD301`–`MLD307`
- specificity dedup per DESIGN §7.3: when two diagnostics share a primary
  span and one rule declares `supersedes` over the other (`MLP226` >
  `MLP201`, `MLP220` > `MLP204`/`MLP211`), only the more specific one is
  reported

All remaining catalog rules are **reserved** and never presented as
checked (`manim-lint rules` lists their status honestly). They wait on
capabilities the fact layers do not provide yet, for example: callback
body summaries (`MLC123`, `MLP218`, `MLC112`), family/points cardinality
bridged into `CostFacts` (`MLP202`/`MLP203`/`MLP207`/`MLP208`/`MLP211`/
`MLP216`), tuple literal facts and curated `ParametricFunction`
(`MLP221`), interpreter tracking of `point_count` (`MLR116`), post-
Transform identity facts (`MLC116`), and a local fork overlay profile
(`MLP214`/`MLP225`). Still unimplemented beyond rules: the SQLite result
cache (`--no-cache` is an accepted no-op), threshold calibration against
rendered baselines, and a nightly render-comparison CI.

## Known limitations

- **Asset checks probe the linting machine.** `MLR104` resolves literal
  asset paths with Manim's own runtime search, on the machine running the
  lint. For absolute paths outside the project tree that is evidence about
  the lint host, not necessarily the render host (e.g. CI linting a repo
  rendered elsewhere); those diagnostics carry
  `environment_dependent: true` as evidence. Case-only mismatches are
  reported only against case-sensitive target platforms (`linux`); when
  every affected profile targets windows/macos, the declared renders
  resolve the file as written and the linter stays silent.
- **Source encodings.** PEP 263 declarations resolve through WHATWG labels
  plus a CPython codec-alias table (`latin-1`, `cp932`, `koi8_r`, ...). A
  rare Python codec the linter cannot represent is skipped with an
  explicit `MLC000` "not supported by manim-lint" notice — never a claim
  that the target Python could not decode the file.
- **Deliberately conservative silences.** Some catalog detections are
  narrower than their prose and stay silent rather than guess (AGENTS.md
  rule 4): `MLR106` sees NaN/inf only in literal form, not through
  `float("nan")` calls; `MLD301` proves FPS dependence only for updaters
  that lack a `dt` parameter (a declared-but-unused `dt` is not flagged);
  `MLC113`/`MLC124` recognize their documented call shapes only; `MLR102`
  needs the interpreter to prove the played bare builder's target
  unchanged; `MLR105` validates a verified Pango subset (a bare `&` is
  allowed); `MLD304` implements only the ThreeDScene fixed-object cleanup
  divergence. `manim-lint explain RULE` states each rule's exact scope.

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
manim-lint cost PATH [--scene NAME]  # per-scene cost breakdown
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
