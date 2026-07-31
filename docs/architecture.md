# Architecture overview

This document is the contributor's map of qual: how a Python source
tree becomes diagnostics, which fact layer owns what, and where each piece
lives. Together with [`docs/rules/`](rules/README.md) (the rule catalog),
[`docs/reference/`](reference/cli.md) plus `schemas/` (the CLI and JSON
contracts), and [CONTRIBUTING.md](https://github.com/Poietra/qual/blob/main/CONTRIBUTING.md)
(the workflow), it is what a contributor needs.

[`DESIGN.md`](https://github.com/Poietra/qual/blob/main/DESIGN.md) is **not**
a specification. It is a Japanese-language design record written before the
Rust implementation existed, when the plan was to write qual in Python; it is
not kept in sync with the code, and where the two disagree the code is right.
Older citations of the form "DESIGN §3.2" refer to the semantic model, which
now lives below in English, with the legacy section numbers kept in the
headings so those references still resolve. "DESIGN §15" is the invariant list
at the end of this page.

## The Manim semantic model

Everything qual reports rests on modelling what Manim actually does, rather
than on matching API names. `FadeOut(mob)` auto-adds `mob` during play setup
even when it was never added to the scene, and removes it afterwards as a
remover; conversely a one-argument updater is no reason to call a plain
`wait()` dynamic. The analyzer reproduces these state transitions first and
diagnoses second.

### Scene lifecycle (§3.1)

```text
module import
  |
Scene.__init__ / renderer, camera, file writer setup
  |
Scene.render()
  |- setup()
  |- construct()
  |    |- object construction / family mutation
  |    |- add / remove / foreground / fixed-in-frame
  |    `- zero or more play / wait
  |- tear_down()
  `- renderer.scene_finished() / partial movie concatenation
```

`Scene.render` calls `setup -> construct -> tear_down` in that order. The base
`setup` and `tear_down` are empty, so omitting `super()` in an override only
*means* something when a user-defined intermediate base Scene actually does
work there. Requiring `super()` in every `setup` override would be name
matching; the rule only fires when the resolved base method has an effect.

### The exact `Scene.play` state machine (§3.2)

```text
compile arguments
  |- an Animation passes through
  |- mob.animate... becomes _AnimationBuilder -> Animation
  `- anything else is a runtime TypeError
      |
apply play kwargs to the animations
      |
auto-add non-introducer animation targets not in the Scene family
      |
duration = max(animation.run_time)
      |
decide whether a Wait is static or dynamic
      |
animation._setup_scene(scene)     # introducers Scene.add if needed
      |
animation.begin()
  |- copy starting_mobject
  |- normally suspend the live mobject's updaters
  |- Transform copies the target and aligns data
  `- interpolate(0)
      |
determine the moving / static object scope
      |
for each time-grid sample
  |- updaters of the animation's private objects
  |- animation.interpolate(alpha)
  |- recursive updaters of the Scene mobjects
  |- mesh updaters
  |- Scene updaters
  |- raster / readback / encode handoff
  `- stop_condition
      |
animation.finish()                # interpolate(1), resume suspended updaters
      |
animation.clean_up_from_scene(scene)
  |- removers are Scene.remove'd
  `- ReplacementTransform does Scene.replace
      |
Scene.update_mobjects(0)
```

Consequences the rules depend on:

- `Transform(mob, target)` normally leaves **`mob` itself** in the target's
  shape. It does not leave `target` in the scene.
- `ReplacementTransform(source, target)` swaps source for target during
  cleanup.
- Introducer and remover membership effects happen at play setup and cleanup,
  **not** when the Animation object is constructed.
- Two animations writing the same live family in one play can overwrite each
  other; the later interpolation wins.
- An animation's live-mobject updaters are normally suspended, but Scene
  updaters, other scene mobjects, and the starting/target copies each follow
  different rules.
- `.animate` is not merely deferred syntax. Taking the builder runs
  `generate_target()` immediately, so mutating the live object — or building a
  second builder for it — between that point and the `play` call can animate a
  stale or overwritten target.

### Frame time and updaters (§3.3)

The frame times of a normal render are those of
`np.arange(0, run_time, 1 / frame_rate)`. A static estimate may report
`ceil(run_time * fps)`, but never as an *exact* frame count: the boundary is
floating-point. `finish()`'s `alpha=1` produces the final geometry and
normally writes no extra video frame.

Within one frame the order is:

```text
Animation.update_mobjects(dt)
Animation.interpolate(alpha)
Scene.update_mobjects(dt)   # top-level, recursing into submobjects
Scene.update_meshes(dt)
Scene.update_self(dt)       # Scene updaters run last
Renderer.render(...)
```

The mobject-updater calling convention has a trap. Manim inspects the
callback's signature for a parameter *named* `dt`; if there is one it calls
`(mobject, dt)`, otherwise `(mobject)`. Merely taking two arguments does not
make a callback time-based.

`MLC105` mirrors that branch rather than approximating it by parameter count,
and validates positional binding the way `inspect.Signature.bind` would. So
`lambda dt:`, a keyword-only `dt`, and `lambda mob, delta:` are errors, while
defaults, positional-only parameters, and `*args` are accepted as long as the
call really binds. `Scene.add_updater` is a separate contract and always
passes a single `dt`.

A `wait()` is dynamic — rather than auto-freezing — when any of these is
present:

- `always_update_mobjects`
- a Scene updater
- a `stop_condition`
- a time-based updater in the Scene family

With only one-argument updaters, a default `wait()` can render as a still
frame. That is a Manim-specific wrong-picture case the linter reports
explicitly.

### Scene membership, family, and draw order (§3.4)

- `Scene.add` is not a set insert. It removes the object first and appends it,
  which changes draw order.
- Adding a parent makes its submobjects visible as part of the family.
- `Mobject.add` ignores a duplicate child and refuses direct self-addition.
- A `VMobject` family normally holds only VMobjects; mixing kinds needs a
  `Group`.
- `Scene.remove(child)` does **not** edit the parent's `submobjects`. It
  rebuilds the Scene's root list without that child, so re-adding or animating
  the original parent later makes the child reappear. Temporary removal and
  structural removal are distinct.
- Cairo's z-order and source-over compositing make it redraw the suffix from
  the first moving or updater-bearing object onward, so a scene-list position
  is also a performance fact.
- Foreground objects can widen the moving scope.
- OpenGL costs differ between a retained render plan and the immediate
  fallback a custom subclass or callback forces.

### 3D and fixed objects (§3.5)

`ThreeDScene.add_fixed_orientation_mobjects` and `add_fixed_in_frame_mobjects`
add the object to the Scene implicitly. The matching remove APIs differ by
renderer: Cairo only unregisters the camera fixing, while the OpenGL branch
also does `Scene.remove` after unfixing. Code assuming the object stays
visible after unfixing is a renderer-portability finding.

### Geometry storage is not renderer-independent (§3.6)

The public `set_points_*` APIs and a raw `.points` assignment are not the same
thing. Cairo's `VMobject` stores cubic Béziers at 4 points per curve, while
`OpenGLVMobject` has paths storing quadratic Béziers at 3 points per curve. So
`points.reshape((-1, 4, 3))`, four-point slicing, and a `set_points` that
assumes the Cairo layout can all be OpenGL portability bugs. The point layout
per renderer lives in the knowledge profile, and raw access is only diagnosed
strongly when both the shape and the target renderer are established.

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
dependency graph ........... src/semantic/dependency.rs — definitions,
   |                         calls, Scenes; deterministic forward/reverse
lifecycle interpreter ...... src/semantic/ — abstract interpretation of
   |                         independent Scene runs -> LifecycleFacts
   |                         and play/object dependency edges
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
*false*). See [the profiles README](https://github.com/Poietra/qual/blob/main/src/knowledge/profiles/README.md)
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
  candidates and drift-check the shipped profiles
  (`cargo test --test knowledge_drift -- --ignored`).
  Humans curate; the tool never writes into the profiles directory.

### Lifecycle interpreter (`src/semantic/`)

The core of the analyzer, and where the semantic model above is enforced:
an abstract interpreter
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
`LifecycleFacts::inline_fallbacks` and surfaces in `qual coverage`
as `helper calls summarized, not inlined`.

### Semantic dependency graph (`src/semantic/dependency.rs`)

RFC 0002 defines the cache-independent `SemanticDependencyGraph`. Every edge
is stored as dependent to dependency: callers point to callees, Scenes to
lifecycle entrypoints, plays to their defining callable, and possible target
objects to their play. The same ordered edge set backs deterministic forward
and reverse indexes. Unresolved dynamic calls, bases, imports, and lifecycle
attribution become anchored Unknown frontiers, never guessed edges.

The frontend-only graph is available before lifecycle interpretation and is
the single source for cache file components. After lifecycle analysis it can
be enriched with Scene/play/object/animation/updater relationships for
ChangeImpact. Graph handles are snapshot-local internal facts; external JSON
must project them through StaticFacts IDs and source anchors. See
[`docs/rfcs/0002-semantic-dependency-graph-v0.md`](rfcs/0002-semantic-dependency-graph-v0.md).

`qual change-impact --before OLD --after NEW` constructs this full graph
for both snapshots. Raw file and definition changes seed reverse traversal in
each graph, preserving deleted edges on the base side and added edges on the
target side. `src/change_impact.rs` projects reached Scene/play/object nodes
through each snapshot's StaticFacts IDs and emits reason paths plus relevant
Unknown frontiers. RFC 0003 and `schemas/change-impact-v0.json` define the
external contract; the bounded patch/rematching layer is described next.

`src/source_bridge.rs` completes that P1 boundary for three bounded local
templates. The command verifies a StaticFacts snapshot/entity request,
generates rollback-carrying hash-guarded edits, and leaves the working tree
untouched. `src/application.rs` applies each candidate to an in-memory raw
snapshot, reruns the full semantic stack, then accepts it only after one
semantic rematch and no coverage regression. RFC 0004 plus the request/output
schemas define the external contract.

### Cost model (`src/cost/` and `src/render_order.rs`)

`CostFacts`: symbolic, evidence-carrying — never fabricated
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
frontend facts instead.

### Reporting (`src/reporting/`)

Suppressions (statement-scoped, `MLC001` for unknown IDs), baselines
(line-number-independent fingerprints with `scene_attribution`
provenance), fixes (SAFE/UNSAFE separation, re-parse validation, per-file
rollback), the output formats, and `coverage.rs` — the analysis-coverage
report (`qual coverage`, `check --analysis-summary`) that counts
everything the analysis could *not* resolve. All output is deterministic
and byte-stable for identical input.

### Public semantic projection

RFC 0001 defines `StaticFacts v0`, a versioned projection for Poietra and
fast-manim. It sits beside diagnostic reporting rather than exposing
`LifecycleFacts`, heap records, cache entries, or internal IDs directly. The
contract uses snapshot-scoped hashed IDs, encoding-aware source anchors, and
reason-carrying unknown values; it includes Scenes, reachable objects,
plays/animations, updaters, play-boundary membership/render order, renderer
risks, and coverage frontiers. See
[`docs/rfcs/0001-static-facts-v0.md`](rfcs/0001-static-facts-v0.md) and
[`schemas/static-facts-v0.json`](https://github.com/Poietra/qual/blob/main/schemas/static-facts-v0.json).

`qual static-facts [PATH...]` reads each source into one immutable raw
byte snapshot, runs the frontend and lifecycle fact layers independently of
selected lint rules, and projects those facts through `src/static_facts.rs`.
The projection never serializes internal analyzer structs. It emits sorted,
pretty JSON with one trailing newline and is byte-identical across worker
counts. The initial command deliberately uses full analysis; a future
incremental route must feed the same projector and pass full/incremental byte
equality before it can be enabled. It may report optimization blockers, but
never grants renderer optimization permission.

### Persistent cache (`src/cache.rs`)

Cache v2 is a disposable `SQLite` WAL database with two layers. The exact
whole-project entry contains filtered diagnostic JSON for the no-frontend
warm path. On a project miss, the freshly rebuilt semantic dependency graph
partitions files by its resolved import/call/base/collision file-edge view;
dependency-closed entries
contain JSON method summaries, diagnostics, and filesystem manifests. Hit
summaries seed the project table and only miss-component definitions and
Scenes are interpreted. Component ownership avoids reusing helper-anchored
diagnostics independently from their callers. ASTs are never persisted.

Keys cover the `build.rs` fingerprint, schema/tool version, resolved semantic
configuration, knowledge profile, full source layout, and the relevant exact
source bytes. Asset files, missing candidates, and case-scan directory
listings are re-stamped per entry. WAL permits concurrent cold writers; one
transaction stores a cold component batch. Access-sequence pruning retains
16 project and 256 component snapshots. Corruption rebuilds with a warning;
other cache failures leave full analysis available.

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
`qual coverage` then reports that frontier instead of hiding it.

## The invariants that govern contributions

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

See [CONTRIBUTING.md](https://github.com/Poietra/qual/blob/main/CONTRIBUTING.md) for the workflow built on top
of these: how to add a rule, the corpus labeling protocol, and the
knowledge-profile review rules.
