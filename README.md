# manim-lint

**Static analysis for [Manim Community](https://www.manim.community/) scenes — catch definite runtime errors, silent mis-rendering, performance multipliers, and non-determinism before you render.**

English | [日本語](README.ja.md)

`manim-lint` is a standalone static analyzer for Manim Community **0.20**
projects, written in Rust. It parses your Python source and checks it against
a curated, versioned model of Manim's semantics — it **never imports or
executes** Manim or your code. Instead of pattern-matching API names, it runs
a lifecycle abstract interpreter that models what `Scene.play` actually does
(argument compilation, auto-add, introducers/removers, updaters) and a
symbolic cost model that knows which code runs once and which code runs every
frame.

## Example

`scenes/demo.py`:

```python
from manim import *


class TrackerDemo(Scene):
    def construct(self):
        title = Text("Tracking x", font_size=0)
        square = Square()
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f"x={tracker.get_value():.2f}"))
        self.add(title, square, label)
        self.play(square.shift(RIGHT))
        square.add_updater(lambda m: m.rotate(0.05))
        self.play(tracker.animate.set_value(8), run_time=8)
        self.wait(0)
```

```console
$ manim-lint check .
scenes/demo.py:6:46: MLR115 error `Text(font_size=0)` is not positive; text sizing requires font_size > 0
scenes/demo.py:9:39: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, and this f-string key varies per frame: every rendered frame can mint a distinct Text/TeX cache key and disk asset (`K_resource ≈ F`).
scenes/demo.py:11:19: MLC102 error `square.shift(...)` mutates the mobject immediately and returns the mobject itself, not an Animation; use `.animate` (e.g. `square.animate.shift(...)`) inside `Scene.play()`.
scenes/demo.py:12:38: MLD301 warning Updater lambda applies `rotate` with a fixed step every frame but declares no `dt` parameter; the motion speed depends on the profile frame rate
scenes/demo.py:14:19: MLC104 error Use a positive `duration`: the literal `0` is non-positive and `Scene` rejects it with `ValueError` before rendering.
```

Two of these would crash the render (`MLC102`, `MLC104`), one renders an
invisible title (`MLR115`), one silently changes speed with the frame rate
(`MLD301`), and one launches the external TeX compiler for a fresh cache key
on every rendered frame (`MLP226`).

## What it checks

Rules come in four families:

- **MLC — lifecycle / correctness.** Definite runtime errors and lifecycle
  mistakes that render the wrong picture: non-Animation arguments to
  `Scene.play` (`MLC102`), `MoveToTarget` without `generate_target()` on any
  path (`MLC107`), `Restore` without `save_state()` (`MLC120`), two
  animations writing the same channel of the same mobject in one play
  (`MLC108`), a `Scene.remove(child)` undone by re-adding the surviving
  parent (`MLC115`).
- **MLR — rendering.** Code that renders, but not what you meant: a Python
  escape corrupting a TeX command in a non-raw `MathTex` literal (`MLR103`),
  asset paths that fail Manim's exact runtime search (`MLR104`), Pango markup
  passed to plain `Text` (`MLR124`), `Transform(mob, mob)` (`MLR113`).
- **MLP — performance.** Cost multipliers with machine-readable evidence:
  `Text`/`MathTex`/`SVGMobject` construction inside an updater or
  `always_redraw` (`MLP201`), frame-varying TeX cache keys that mint one disk
  asset per frame (`MLP226`), scene graphs growing every frame (`MLP204`),
  `TracedPath` without `dissipating_time` (`MLP220`).
- **MLD — determinism / portability.** Renders that differ between machines,
  frame rates, or renderers: fixed per-frame steps without `dt` scaling
  (`MLD301`), unseeded global randomness in frame callbacks (`MLD302`),
  case-only asset path mismatches on case-sensitive targets (`MLD305`).

### Semantic depth, not name matching

The analyzer's core principle (DESIGN §1): never warn on an API name alone.
`FadeOut(mob)` is fine on a mobject that was never added — play's preparation
auto-adds it and the remover deletes it afterwards. So the pipeline builds
real facts first:

- a **lifecycle abstract interpreter**: intra-function CFG, interprocedural
  helper summaries, per-Scene MRO composition with `super()` dispatch,
  allocation-site identity, scene membership/order/updater tracking, and
  play-group semantics;
- a **symbolic cost model**: hot-context propagation (updaters,
  `always_redraw`, stop conditions, interpolate overrides) and frame-count
  intervals derived only from literal durations — the cost report above says
  `duration 8 s -> frames ~480` because `run_time=8` at 60 FPS is provable,
  and prints `unknown` otherwise, never a fabricated number.

Every diagnostic separates **severity** (`error`/`warning`/`info`) from
**confidence** (`certain`/`high`/`medium`/`low`), and state-dependent rules
fire only on definite, all-paths evidence. When a value cannot be resolved
statically, it degrades to `Unknown` and the linter stays **silent rather
than guessing** — a deliberate design stance carried through every rule.

## Installation

Requires a Rust toolchain (1.85+). There is no crates.io release yet;
install from source:

```bash
git clone <this repository>
cd manim-lint
cargo install --path .
```

## Quickstart

```bash
manim-lint check .                      # analyze, concise output
manim-lint check scenes --format full   # explanations + evidence
manim-lint check . --format json       # schemas/diagnostics-v1.json
manim-lint check . --format sarif      # SARIF 2.1.0
manim-lint check . --format github     # GitHub Actions annotations
manim-lint explain MLC102               # full documentation for a rule
manim-lint rules                        # every rule ID, phase, and status
manim-lint config                       # resolved effective configuration
manim-lint cost scenes/demo.py          # per-scene cost breakdown
```

Exit codes: `0` — no reported diagnostic reaches `fail-level`; `1` — at
least one does; `2` — command-line, configuration, or internal error.

Useful `check` options: `--select` / `--ignore`, `--min-confidence`,
`--fail-level`, `--profile`, `--renderer`, `--fps`,
`--resolution WIDTHxHEIGHT`, `--statistics`, and the baseline/fix options
described below. `--select` also narrows the analysis itself: fact layers
no selected rule needs (the lifecycle interpreter, the symbolic cost
model) are skipped, so a narrow select is faster than a full run. The
reported diagnostics are identical either way — rules superseding a
selected rule still run so a narrow select never resurrects a superseded
diagnostic.

`--format full` prints the explanation and machine-readable evidence under
each diagnostic:

```text
scenes/demo.py:9:39: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, ...
    A frame-varying key defeats the `MathTex` cache: instead of one shaping/compile job reused every frame,
    each frame pays construction plus a cache miss, and for TeX classes each distinct key also launches the
    external TeX compiler and `dvisvgm`, leaving one disk asset per key. ...
    evidence.distinct_resource_keys: "per-frame"
    evidence.invocation_context: "frame-callback"
    evidence.multiplicity: ["frames"]
    evidence.state_path: ["construct","always_redraw:9"]
    applies to profiles: production
```

## Configuration

Configuration lives in `[tool.manim-lint]` in `pyproject.toml`, found by
walking up from the checked path. Render profiles are
`[[tool.manim-lint.profile]]` entries:

```toml
[tool.manim-lint]
manim-version = "0.20"
target-python = "3.11"
select = ["MLC", "MLR", "MLP", "MLD"]
ignore = []
min-confidence = "high"
fail-level = "warning"
default-profile = "production"
knowledge-profile = "upstream_0_20"
respect-manim-cfg = true
exclude = [".venv/**", "media/**"]
per-file-ignores = { "tests/fixtures/**" = ["MLP", "MLD"] }

[[tool.manim-lint.profile]]
name = "production"
renderer = "cairo"
platform = "linux"
pixel-width = 1920
pixel-height = 1080
frame-rate = 60
assets-dir = "."
allowed-fonts = ["Noto Sans", "Noto Sans CJK JP"]
```

Precedence, highest first:

```text
CLI > selected profile > pyproject base > manim.cfg > builtin defaults
```

When `respect-manim-cfg` is enabled (the default), a `manim.cfg` supplies
resolution/fps/renderer defaults below the pyproject settings. Unknown keys,
unknown rule selectors, duplicate profile names, and unknown profile
references are configuration errors (exit 2). `--profile all` analyzes every
defined profile and merges same-evidence diagnostics, listing the affected
profiles per diagnostic.

Configuration is validated honestly (exit 2 on violation):

- A declared `manim-version` must fall inside the Manim range supported by
  the configured knowledge profile (e.g. `upstream_0_20` supports
  `>=0.20,<0.21`); when absent, nothing is validated.
- `target-python` must be `MAJOR.MINOR` between 3.6 and 3.12. The upper
  bound is the Python grammar the bundled parser (rustpython-parser 0.4)
  implements; the lower bound is the floor below which syntax gating can
  no longer be guaranteed (older targets are refused with exit 2 instead
  of being silently unenforced). The grammar is fixed (no
  `feature_version` pinning), so parsing itself never changes; instead a
  post-parse gate over the AST, the token stream, and f-string text
  reports every construct newer than the target as `MLC000`:
  `async`/`await` syntax outside `async def` (3.7), `:=`, positional-only
  `/`, and f-string self-documenting `=` (3.8), relaxed decorators and
  parenthesized context managers with `as` (3.9), `match` (3.10),
  `except*` and PEP 646 `*` unpacking in subscripts (3.11), `type`
  aliases, PEP 695 type parameters, and PEP 701 f-string expressions
  (3.12). A file the gate passes silently is guaranteed parseable by the
  target's own parser. The gated file is still fully analyzed, and a
  `--fix` that would introduce such syntax is rolled back. See
  `manim-lint explain MLC000` for the full coverage table.
- A frame rate that is zero, negative, or non-finite, and a resolution
  with a zero dimension, are rejected wherever they come from (`--fps` /
  `--resolution`, a profile, or `manim.cfg`).
- `stub-paths` is not implemented yet; a non-empty list is rejected
  instead of being silently ignored.

`manim-lint config` prints the resolved configuration plus an
`enforcement` section stating which settings are enforced and which are
informational.

## Suppressions

```python
self.play(square.shift(RIGHT))  # manim-lint: ignore[MLC102]   # same statement

# manim-lint: ignore[MLP201]                                   # next statement
label = always_redraw(...)

# manim-lint: file-ignore[MLP]   # whole file; must appear in the file header
```

Suppressions target **whole statements**, not single lines: an end-of-line
comment (or a standalone comment directly above) covers the entire
statement, including continuation lines of a multi-line call, so a
diagnostic anchored anywhere inside the statement is suppressed. For
compound statements (`def`, `for`, `if`, `with`, ...) the suppression
covers only the header up to its colon — one comment can never silence an
entire suite.

An unknown rule ID inside an inline suppression does **not** suppress
anything; it is reported as a dedicated warning:

```text
scene.py:8:23: MLC001 warning unknown rule ID in suppression: MLC999
```

For whole directories, use `per-file-ignores` in `pyproject.toml` (see
above).

## Gradual adoption: baselines

Adopt the linter on an existing project without fixing everything first:

```bash
manim-lint check . --write-baseline .manim-lint-baseline.json  # record today's findings
manim-lint check . --baseline .manim-lint-baseline.json        # report only new findings
```

Baseline fingerprints (`schemas/baseline-v1.json`) contain **no line
numbers** — they are built from rule ID, relative path, qualified scene name,
and a surrounding token hash — so inserting unrelated lines elsewhere in a
file does not invalidate entries. The `scene` field records the qualified
enclosing Scene class (empty outside any scene), so identical findings in
different scenes get distinct fingerprints. Written files carry a
`scene_attribution: "attributed"` provenance marker: their empty `scene`
means literally "outside any Scene" and matches exactly. Baselines written
before scene attribution (no marker) are still read, and only there an
empty `scene` matches as a wildcard. A corrupt or wrong-schema baseline
file exits 2 with a clear message.

## Autofix

```bash
manim-lint check . --fix            # apply SAFE fixes only
manim-lint check . --fix --unsafe-fixes  # also apply UNSAFE fixes
```

Safe and unsafe fixes are strictly separated: `--fix` alone applies only
edits that preserve behavior (e.g. `MLC127` removes a duplicate child from
one `add()`/`VGroup()` call, `MLR104` corrects a case-only asset path).
Unsafe fixes can change runtime semantics (e.g. rewriting
`play(mob.shift(...))` to `play(mob.animate.shift(...))` for `MLC102`) and
require the explicit extra flag. Every fixed file is re-parsed for
validation; a file whose fix does not survive re-parsing is rolled back.

```console
$ manim-lint check . --fix
scene.py:8:37: MLC127 info Remove the duplicate `square` from this `VGroup(...)` call: Manim warns and ignores repeated children of a single add.
fixed 1 issue(s) in 1 file(s)
```

## Cost command

`manim-lint cost` prints the symbolic cost breakdown per scene — play list
with frame intervals, hot contexts with provenance, per-frame constructions,
and resource-key growth. Unknown durations are printed as unknown, never as
fabricated numbers:

```console
$ manim-lint cost scenes/demo.py
profiles: production (cairo, 1920x1080, 60 fps)

scene scenes.demo.TrackerDemo (scenes/demo.py)
  plays:
    scenes/demo.py:11:9 play duration unknown -> frames per-frame
    scenes/demo.py:13:9 play duration 8 s -> frames ~480
    scenes/demo.py:14:9 wait duration 0 s -> frames ~0
  hot contexts:
    scenes/demo.py:9:31 entry always_redraw; path construct -> always_redraw:9; factors frames
    scenes/demo.py:12:28 entry updater; path construct -> updater:12; factors frames
  per-frame constructions:
    scenes/demo.py:9:39 MathTex construction x per-frame
  resource-key growth:
    scenes/demo.py:9:39 MathTex distinct cache keys: one per rendered frame (f-string key varies per frame)
```

## CI integration

GitHub Actions annotations directly on the PR diff:

```yaml
name: manim-lint
on: [push, pull_request]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --path . --locked
        working-directory: manim-lint   # path to your manim-lint checkout
      - run: manim-lint check . --format github
```

Or upload SARIF so findings appear in the GitHub code-scanning UI:

```yaml
      - run: manim-lint check . --format sarif > manim-lint.sarif
        continue-on-error: true
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: manim-lint.sarif
```

## Rule catalog

92 rule IDs are reserved across the four families; **79 are implemented and
13 are reserved**:

| Family | Implemented | Reserved |
| --- | --- | --- |
| MLC lifecycle / correctness | 28 | 3 |
| MLR rendering | 23 | 4 |
| MLP performance | 21 | 6 |
| MLD determinism / portability | 7 | 0 |

A reserved ID **never fires**: `manim-lint rules` lists it honestly as
`reserved`, and `check` does not register it. Each reserved rule is blocked
on a named capability the fact layers do not provide yet — for example
post-Transform identity facts (`MLC116`), cross-registration updater
read-after-write ordering facts (`MLR109`), SVG asset content facts
(`MLR118`), an alias-safe cross-object `z_index` stacking proof (`MLR122`),
a curated mesh / `Object3D` class in the knowledge profile (`MLR123`),
curated process-global SVG cache semantics (`MLP217`), and a local fork
overlay profile (`MLP214`/`MLP225`).

The full index with per-rule status, severity, and confidence is in
[docs/rules/README.md](docs/rules/README.md); each implemented rule has a
documentation page there, also available via `manim-lint explain <ID>`.

## Architecture

```text
Python sources
   |
SourceManager ............ encoding (PEP 263), newlines, Unicode columns
   |
knowledge profile ........ versioned Manim 0.20 semantics (no import, ever)
   |
frontend ................. imports/aliases, project index, qualified call facts
   |
semantic ................. lifecycle abstract interpreter -> LifecycleFacts
   |
cost ..................... hot contexts, frame intervals -> CostFacts
   |
rules .................... MLC / MLR / MLP / MLD over the fact layers
   |
suppressions, supersedes, baseline
   |
output ................... concise | full | json | sarif | github, fixes, cost report
```

[`DESIGN.md`](DESIGN.md) is the authoritative specification for the semantic
model, the rule catalog, and every public contract. JSON output follows
[`schemas/diagnostics-v1.json`](schemas/diagnostics-v1.json); baselines
follow [`schemas/baseline-v1.json`](schemas/baseline-v1.json). Output is
deterministic and byte-stable for the same input.

## Known limitations

- **Target version.** The shipped knowledge profile covers Manim Community
  **0.20 only**. Other versions have no profile yet.
- **Asset checks probe the linting machine.** `MLR104` resolves literal
  asset paths with Manim's own runtime search, on the machine running the
  lint. For absolute paths outside the project tree that is evidence about
  the lint host, not necessarily the render host (e.g. CI linting a repo
  rendered elsewhere); those diagnostics carry `environment_dependent: true`
  as evidence. Case-only mismatches are reported only against case-sensitive
  target platforms (`linux`); when every affected profile targets
  windows/macos, the declared renders resolve the file as written and the
  linter stays silent.
- **Source encodings.** PEP 263 declarations resolve through WHATWG labels
  plus a CPython codec-alias table (`latin-1`, `cp932`, `koi8_r`, ...). A
  rare Python codec the linter cannot represent is skipped with an explicit
  `MLC000` "not supported by manim-lint" notice — never a claim that the
  target Python could not decode the file.
- **Durations come from literals only.** A play whose duration rests on
  Manim's *defaults* (`self.play(m.animate.shift(RIGHT))` with no
  `run_time` anywhere, `self.wait()`) is reported as unknown — frame counts
  use per-frame wording instead of numbers (conservative: missing, never
  fabricated). A literal play-level `run_time` decides the whole-play
  duration exactly — including plays inside Scene helpers, per call site,
  even when the call sites pass different (or untracked) mobjects to a
  `.animate` builder on the parameter — and a *non-literal* `run_time`
  (or a `**kwargs` splat) honestly widens constructor literals it
  overrides.
- **Deliberately conservative silences.** Some detections are narrower than
  their catalog prose and stay silent rather than guess: `MLR106` sees
  NaN/inf only in literal form, not through `float("nan")` calls; `MLD301`
  proves FPS dependence only for updaters that lack a `dt` parameter (a
  declared-but-unused `dt` is not flagged); `MLC113`/`MLC124` recognize
  their documented call shapes only; `MLR102` needs the interpreter to prove
  the played bare builder's target unchanged; `MLR105` validates a verified
  Pango subset (a bare `&` is allowed); `MLD304` implements only the
  ThreeDScene fixed-object cleanup divergence. `manim-lint explain <RULE>`
  states each rule's exact scope.
- **Not yet implemented.** The 13 reserved rules (see above); the SQLite
  result cache (`--no-cache` is an accepted no-op); threshold calibration
  against rendered baselines; a nightly render-comparison CI.

## Development

```bash
cargo fmt --check
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

All four gates must pass.

Knowledge-profile maintenance: the `sync_manim_knowledge` binary statically
reads a Manim checkout, generates reviewable profile candidates, and checks
the shipped profiles for drift (exit 1 on contradictions) — see
[src/knowledge/profiles/README.md](src/knowledge/profiles/README.md).
Provenance is split: `upstream_0_20` describes the **clean** upstream base
commit `4d25c031` (read via `git archive`, never the working tree), and the
`local_0_20_1_4d25c031` overlay carries what the sibling fork's working
tree adds on top:

```bash
# working tree (fork) — informational against upstream
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --diff
# clean upstream base — must be contradiction-free
cargo run --bin sync_manim_knowledge -- --manim-root ../manim --manim-ref 4d25c031 --diff
cargo test --test knowledge_drift -- --ignored   # layer-9 drift gate (both)
```

### Release quality gates (DESIGN §11.4)

Three additional gates guard releases:

```bash
# Labeled corpus gate — runs automatically inside `cargo test`.
# tests/corpus/manifest-v1.json pins sha256 + exact expected diagnostics
# (true positives and false-positive guards) for every corpus case,
# including pinned real-Manim example_scenes snapshots and the
# adversarial review probes.
cargo test --test corpus_gate

# Benchmark gate — explicit, release build, quiet machine.
# Cold ≤ 2 s / peak RSS < 300 MiB over the pinned 10k-LOC fixture
# (tests/corpus/benchmark_10kloc); thresholds assert only on the machine
# matching benchmarks/reference-machine.json, informational elsewhere.
# The warm ≤ 0.5 s budget is recorded but not enforced until the on-disk
# cache exists (see `enforced` in that file).
cargo test --release --test benchmark_gate -- --ignored benchmark

# Knowledge drift gate — needs the sibling Manim checkout; in CI it runs
# on schedule/dispatch against a shallow clone of the pinned base commit.
cargo test --test knowledge_drift -- --ignored
```

Corpus cases are never re-recorded mechanically: a mismatch means
re-adjudication under the labeling protocol in
[CONTRIBUTING.md](CONTRIBUTING.md#corpus-labeling).

See [CONTRIBUTING.md](CONTRIBUTING.md) for the
repository layout, the step-by-step guide to adding a rule, and the
invariants every change must keep. `DESIGN.md` is authoritative; changes to
public contracts must update it, its schema tests, and the rule docs
together.

## License

[MIT](LICENSE).
