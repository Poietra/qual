# Architecture overview

This document is the contributor's map of manim-lint: how a Python source
tree becomes diagnostics, which fact layer owns what, and where each piece
lives. [`DESIGN.md`](../DESIGN.md) is the authoritative specification (the
section numbers below refer to it); this page is the guided tour.

## The pipeline

```text
Python sources
   |
SourceManager .............. src/source.rs — PEP 263 encodings, newline
   |                         preservation, UTF-8-byte -> Unicode columns
knowledge profile .......... src/knowledge/ — versioned Manim 0.20
   |                         semantics, embedded JSON; never imports Manim
frontend facts ............. src/frontend/ — parse, imports/aliases,
   |                         project index, qualified calls, CFG,
   |                         statement/binding facts, target-python gate
lifecycle interpreter ...... src/semantic/ — abstract interpretation of
   |                         independent Scene runs -> LifecycleFacts
cost facts ................. src/cost/ (+ src/render_order.rs) — hot
   |                         contexts, frame intervals, execution
   |                         liveness, fork gates -> CostFacts
rules ...................... src/rules/ — MLC / MLR / MLP / MLD query the
   |                         fact layers via RuleContext; independent rules
supersedes / suppressions .. src/rules/registry.rs, src/reporting/
   |                         suppressions.rs, baseline.rs
reporting .................. src/reporting/ — concise | full | json |
                             sarif | github, fixes, cost report, coverage
```

`src/application.rs` orchestrates the commands (`check`, `explain`,
`rules`, `config`, `cost`, `coverage`) over this pipeline; `src/cli.rs`
is the clap surface. Every stage degrades to `Unknown` instead of
guessing, and every downstream consumer must treat `Unknown` as "stay
silent", never as evidence (invariant 2 below).

For a cache-eligible `check`, `src/application.rs` first reads one source
snapshot and asks `src/cache.rs` for a whole-project diagnostic entry. A
dependency-validated hit skips the fact/rule stages and goes directly to
reporting; a miss follows the pipeline above and stores the pre-baseline
diagnostics only after asset dependencies remain stable across the rule run.
The cache is an optimization boundary, never a semantic fact layer.

Fact computation is gated on what the selected rules declare: each rule's
`RuleMetadata::required_capabilities` names the fact layers it needs
(`qualified-calls`, `lifecycle`, `cost-facts`, `statement-facts`, ...),
and a `--select` that needs no lifecycle or cost facts skips computing
them. Rules superseding a selected rule still run, so narrowing the
selection never resurrects a superseded diagnostic.

Cold analysis uses a bounded Rayon worker pool at three deterministic
fan-out points: independent non-recursive summary SCCs in the same
bottom-up layer, independent Scene lifecycle runs, and independent rules.
Recursive SCC fixpoints and frontend/project-index construction remain
sequential. Indexed collection plus the final stable diagnostic sort make
the output byte-identical across worker counts.

## Fact-layer inventory

### Frontend (`src/frontend/`)

- `parser.rs` — rustpython-parser 0.4 (fixed Python 3.12 grammar); a
  parse failure is an `MLC000` diagnostic, never a stop.
- `features.rs` — the post-parse `target-python` syntax gate: every
  parse-level construct newer than the configured target (3.6–3.12) is
  reported as `MLC000`, with the oracle-checked coverage table in the
  module header. A silently passing file is guaranteed parseable by the
  target's own parser.
- `imports.rs` / `names.rs` — import and alias resolution including
  `from manim import *` through the knowledge profile's export closure.
- `index.rs` — the project-wide symbol/class-hierarchy index and
  **qualified call facts**: every call site resolved to qualified
  candidates, or honestly to an empty candidate set.
- `cfg.rs` — the intra-function control-flow graph the interpreter walks.
- `statements.rs` — statement-position facts (per-call enclosing-statement
  spans and roles) and per-file binding facts with unified rebind
  poisoning. This module exists so that **rules never walk the module
  AST**: a rule that needs "where is this call, what binds this name"
  queries `RuleContext::statement_facts()` / `binding_facts()` instead.

### Knowledge profiles (`src/knowledge/`)

`model.rs` + the JSON under `profiles/` are the only source of Manim
semantics — introducers/removers, auto-add, constructor signatures,
`returns_self`, renderer notes, fork capabilities. Nothing in the
analyzer hardcodes "what `FadeIn` does"; if the profile does not say it,
the analyzer does not know it (absent facts mean *not curated*, never
*false*). See [the profiles README](../src/knowledge/profiles/README.md)
for provenance:

- `upstream_0_20` describes the **clean** upstream base commit
  `4d25c031`, verified drift-free via `sync_manim_knowledge --manim-ref
  4d25c031 --diff` (a `git archive` read, never the working tree).
- `local_0_20_1_4d25c031` is the fork overlay: fork-only symbols plus the
  curated `fork_capabilities` block (TeX parallel compile, the Cairo
  fork-per-play gate, static layers, packed interpolation, the
  process-global SVG cache). Everything fork-gated is inert under
  `upstream_0_20`.
- `generator.rs` / `src/bin/sync_manim_knowledge.rs` generate reviewable
  candidates and drift-check the shipped profiles (the DESIGN §11.2
  layer-9 gate, also `cargo test --test knowledge_drift -- --ignored`).
  Humans curate; the tool never writes into the profiles directory.

### Lifecycle interpreter (`src/semantic/`)

The core of the analyzer (DESIGN §3, §5.5–§5.7): an abstract interpreter
that runs each discovered Scene subclass's
`__init__ → setup → construct → tear_down` lifecycle and produces
`LifecycleFacts` — per-scene membership/order/updater state, play facts,
allocation-site identities, and statement-boundary snapshots. Support
modules: `values.rs` (abstract values), `heap.rs` (abstract heap),
`events.rs` (recorded facts), `summaries.rs` (parameter-relative effect
summaries; independent bottom-up components run in parallel). Independent
Scene classes also run in parallel. The interpreter itself is a directory of nine modules under
`src/semantic/interpreter/`:

| Module | Owns |
| --- | --- |
| `mod.rs` | public API, `SceneLifecycle`, the fact types |
| `exec.rs` | the abstract state lattice and the CFG fixpoint walk |
| `dispatch.rs` | qualified-call dispatch into curated knowledge effects |
| `play.rs` | the exact `Scene.play`/`wait` pipeline: compile args, kwargs, auto-add, duration, introducer/remover effects |
| `inline.rs` | helper inlining, summary application and extraction |
| `mro.rs` | C3 linearization, `super()` dispatch, the per-scene lifecycle run |
| `heap_ops.rs` | allocation, add/remove/replace, family recomputation |
| `callbacks.rs` | updater signatures, registration/removal identity, updater-body dataflow |
| `snapshots.rs` | statement-boundary state snapshots and per-statement queries |

**The summary-vs-inline duality invariant** (documented at the top of
`inline.rs`): for any call resolving to a project definition, *exactly
one* of two execution modes applies — never both, never neither. Either
the body is **inlined** against the live caller state (bounded by cycle
detection plus a depth safety cap; plays materialize as real per-call-site
`PlayFact`s with a `call_path`), or its parameter-relative **effect
summary is applied** (the fallback frontier — recursion, the cap,
non-helper calls — with combined certainty and summary-derived
`Maybe`-certainty play records). Every fallback site is recorded on
`LifecycleFacts::inline_fallbacks` and surfaces in `manim-lint coverage`
as `helper calls summarized, not inlined`.

### Cost model (`src/cost/` and `src/render_order.rs`)

`CostFacts` (DESIGN §4): symbolic, evidence-carrying — never fabricated
numbers.

- `contexts.rs` — hot-context propagation: updaters, `always_redraw`,
  stop conditions, `TracedPath(traced_point_func=...)`, interpolate
  overrides, with provenance paths.
- `estimator.rs` / `model.rs` — frame-count intervals from literal
  durations only (`ceil(duration x fps)` per play, summed per play;
  unknown stays unknown), loop repetition composition.
- `liveness.rs` — per-updater **execution liveness**: the plays/waits
  where a callback *provably* executes per frame (registration live, host
  present, not suspended, frames actually rendered), keyed by helper
  `call_path`. Hot-context performance rules quantify over proven plays
  only.
- `sizes.rs` / `geometry.rs` / `thresholds.rs` — confirmed-size facts and
  the documented thresholds.
- `fork.rs` — fork fast-path gate evaluation (fork-per-play, static
  layers, packed interpolation) against the overlay's
  `fork_capabilities`; feeds the `cost` command's fork section and
  `MLP225`.
- `src/render_order.rs` — Cairo effective display order and moving-suffix
  estimation (`MLP209`, `MLR111`).

### Rules (`src/rules/`)

Four group directories (`lifecycle/`, `rendering/`, `performance/`,
`portability/`) plus `registry.rs`, which composes each group's `rules()`
list, applies selection/capability gating, and deduplicates via
`supersedes` metadata. Enabled rules run independently in parallel, then
their findings enter the shared deterministic supersession/filter/sort pass.
Rules have **no visitors of their own**: they
query `RuleContext` fact layers. The canonical traversal rule: no
module-root AST walks in rule code — needed positions are promoted into
frontend facts instead (DESIGN §5.6).

### Reporting (`src/reporting/`)

Suppressions (statement-scoped, `MLC001` for unknown IDs), baselines
(line-number-independent fingerprints with `scene_attribution`
provenance), fixes (SAFE/UNSAFE separation, re-parse validation, per-file
rollback), the output formats, and `coverage.rs` — the analysis-coverage
report (`manim-lint coverage`, `check --analysis-summary`) that counts
everything the analysis could *not* resolve. All output is deterministic
and byte-stable for identical input.

### Persistent cache (`src/cache.rs`)

Cache v1 is a disposable `SQLite` WAL database containing JSON diagnostics
and JSON filesystem-dependency manifests. Its project key covers the build
fingerprint emitted by `build.rs`, schema/tool version, resolved semantic
configuration, serialized knowledge profile, sorted source paths, and the
exact source bytes fed to `SourceManager`. Asset files, missing candidates,
and case-scan directory listings are re-stamped before a hit is accepted.
Corruption rebuilds with a warning; any other cache failure disables caching
for that run and leaves full analysis available.

## One diagnostic, end to end

`scenes/demo.py` contains
`label = always_redraw(lambda: MathTex(f"x={tracker.get_value():.2f}"))`
followed by `self.play(tracker.animate.set_value(8), run_time=8)` at
60 FPS. How `MLP226` (frame-varying TeX cache key) fires:

1. **Frontend**: the call index resolves `always_redraw` and `MathTex` to
   qualified Manim symbols (the profile's export closure makes
   `from manim import *` precise) and records the f-string argument.
2. **Knowledge**: the profile says `MathTex` construction performs a
   cache-key lookup keyed by its source string, and what
   `always_redraw` does (re-invoke the factory every frame).
3. **Lifecycle interpreter**: the scene run allocates the label at its
   site, tracks `self.add(...)` membership, and materializes the plays as
   `PlayFact`s — `run_time=8` is a literal, so the play's duration is
   exactly 8 s.
4. **Cost**: the lambda becomes a hot context
   (`construct -> always_redraw:9`, factor `frames`); liveness proves the
   callback executes during the 8 s play (host in the scene, not
   suspended, frames rendered), so the frame interval is
   `ceil(8 x 60) = 480` as a lower bound; the f-string key with a
   frame-varying interpolation yields a resource-key-growth fact.
5. **Rule**: `MLP226` requires a hot construction of a cache-keyed class
   with a frame-varying key **and** at least one proven-execution play —
   all present — and emits a warning with machine-readable evidence
   (`distinct_resource_keys: {"lower":480,...}`, the execution play list,
   `state_path`).
6. **Reporting**: suppressions and baseline filtering get a chance, the
   supersedes graph deduplicates more-specific findings, and the
   formatter prints the diagnostic (severity `warning`, confidence
   `high`) with its profile applicability.

Silence flows the same way: if `run_time` were a variable, the duration
would be unknown, liveness could still prove execution but the key-count
evidence would say "per frame" without a number — and if the factory's
target could not be proven frame-varying, the rule would stay silent.
`manim-lint coverage` then reports that frontier instead of hiding it.

## The invariants that govern contributions (DESIGN §15)

1. Never import or execute the analyzed code or Manim.
2. Never emit a certain/high diagnostic from `Unknown` facts.
3. Scene membership and visibility are distinct — never one boolean.
4. Animation construction and play lifecycle effects are distinct points
   in time.
5. Introducer/remover/replacement and auto-add behavior come explicitly
   from the knowledge profile.
6. Live mobjects and their starting/target copies keep distinct
   identities.
7. Frame-callback frequency and play-start frequency are never confused.
8. Renderer-specific diagnostics always carry their applicable profiles.
9. Never fabricate precise numbers from unknown performance values.
10. Autofixes are parse-validated; SAFE and UNSAFE never mix.
11. Source spans are Unicode-correct and round-trip (Japanese source is a
    first-class test case).
12. Diagnostic order and serialized output are deterministic —
    byte-stable for the same input.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the workflow built on top
of these: how to add a rule, the corpus labeling protocol, and the
knowledge-profile review rules.
