# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **`manim.cfg` `quality` is no longer silently dropped.** `quality` is the
  usual way to set resolution and frame rate in Manim, but qual read only
  `pixel_width`, `pixel_height`, `frame_rate`, and `renderer`; a project
  configured with `quality = fourk_quality` was analyzed at 1920x1080/60
  while `qual config` reported `respect_manim_cfg: true`, so every frame
  estimate and cost class was computed against the wrong profile. The five
  named presets and `example_quality` are now interpreted, by name and by
  their `-q` flags, and an unknown value is a configuration error rather
  than a guess. Within `manim.cfg`, `quality` overrides the individual keys,
  matching Manim's `digest_parser`, which applies it last.
- A `manim.cfg` `[CLI]` key that affects the render profile but that qual
  does not interpret is now reported in the new `manim_cfg_warnings` field
  of `qual config` and on stderr during `qual check`, instead of being
  dropped without a word.

### Added

- A searchable Material for MkDocs site that brings installation, configuration,
  adoption, cost/coverage, all 92 rule pages, CLI reference, JSON contracts,
  RFCs, architecture, and research evidence into one GitHub Pages deployment.

### Changed

- README, CLI, GitHub, crates.io, and PyPI-facing copy now lead with Qual as
  the Manim-aware linter: render-time errors, visual bugs, and per-frame
  performance traps before rendering. The README is a concise product entry
  point, while detailed operation and integration material lives in the docs.

## [0.3.0] - 2026-07-30

### Added

- **A Ruff-style, single-version release pipeline.** A reviewed
  `Cargo.toml` version is now distributed by cargo-dist as checksummed,
  attested Linux/macOS/Windows archives plus shell and PowerShell installers;
  maturin builds matching PyPI wheels and an sdist; and a custom
  trusted-publishing job publishes the source crate to crates.io. A manual
  `release` environment gate re-runs formatting, clippy, tests, package
  verification, and knowledge drift before any registry write.
- Release compliance material for the statically linked LGPL-3.0-only
  `malachite` dependency: verbatim LGPL/GPL texts, relinking instructions,
  source archives, and fail-closed metadata/wheel checks.
- **A `rich` output format, and it is the default in a terminal.** Each finding
  gets a severity banner, the offending source line with its span underlined,
  two lines of context, the explanation, and a run summary
  (`✖ 2 errors  ⚠ 1 warning  in 1 file`). Redirected or piped output stays
  `concise` — one stable line per diagnostic, no escape sequences — so scripts
  and CI keep parsing what they parse today. `--format` overrides the choice
  either way.
- **`--color auto|always|never`.** `auto` styles only a terminal, `NO_COLOR`
  disables styling regardless, and `--color always` styles even when
  redirected. Only `rich` is ever styled. `COLUMNS` sets the rendered width.

  Frames are read lazily from disk, so a cache hit still answers without
  building a `SourceManager`. A file that changed or vanished since the
  analysis renders without its frame rather than failing.
- `THIRD-PARTY-LICENSES.md` — dependency license survey, including the
  LGPL-3.0-only `malachite` family that `rustpython-parser` links statically
  and what it obliges when distributing a prebuilt binary.

### Performance

- **Cold analysis of a 393-file project: ~5.9 s to ~4.9 s**, warm cache
  unchanged at 0.05 s. Source decoding, tokenizing, and parsing ran one file
  at a time (1.92 s wall at 1.65 s of CPU) and now run in parallel (1.00 s),
  registering in the same order so `FileId`s and every ordering derived from
  one are identical. The remaining cost is summarizing ~5,600 callables and is
  allocation-bound rather than a scheduling problem — the dependency scan is
  3 ms and the worst-parallelizing rounds are 22 ms of 4,067 ms — so the
  binary now uses mimalloc, which also cuts the run-to-run spread from 0.91 s
  to 0.06 s at the same peak memory. Findings are unchanged on all four
  measured corpora.

### Fixed

- **Release workflows now fail closed under security linting.** External
  Actions are pinned to reviewed commit SHAs, release tags enter shell steps
  through environment variables, reusable jobs no longer inherit unrelated
  secrets, and job permissions are limited to the release phase that needs
  them. CI runs zizmor to prevent these controls from drifting.
- The declared Rust 1.85 MSRV is now enforced in CI and works in practice;
  request validation no longer uses let-chain syntax that requires Rust 1.88.
- **Untrusted input can no longer abort the process.** The 0.2.0 limits missed
  every chain that nests without nesting brackets — `a()()()`, `a.b.b.b`,
  `1 + 1 + 1`, `lambda: lambda:` — each of which overflowed the stack. Tokens
  per logical line are now bounded (8,192; the largest real logical line
  measured is 1,361), which bounds tree depth for all of them at once. A FIFO
  no longer blocks forever and a character device no longer reads without end:
  file type and size are checked on the directory entry, before opening.
- **`--fix` can no longer write outside the project.** Symlinks were followed
  wherever they led, so a link committed to a repository let
  `qual check --fix` rewrite an arbitrary file elsewhere on the machine.
  Paths that resolve outside the project root are skipped at discovery and
  refused again immediately before the write.
- **`MLC109` no longer claims certainty it does not have.** An empty
  `AnimationGroup()` is built with run time 0 and is harmless when nested;
  only reaching `Scene.play` as the whole animation fails. It is now a
  `warning` at `high` confidence, and the message says what is actually true.
  On ManimML this converted 29 certain errors into warnings.

### Changed

- **The project is now named Qual.** The CLI and Cargo crate move from
  `manim-lint` to `qual`; the PyPI distribution moves to `qual-manim` because
  PyPI rejects the shorter name as too similar to an existing project;
  configuration moves from
  `[tool.manim-lint]` to `[tool.qual]`; inline suppressions use `# qual:`;
  the cache moves to `.qual-cache`; and maintainer builds use the
  `QUAL_MANIM_ROOT` and `QUAL_BUILD_ID` environment variables. Rule IDs and
  diagnostic semantics are unchanged.
- The knowledge-drift gate reads `QUAL_MANIM_ROOT` (default `../manim`)
  instead of a hardcoded path on the author's machine, which CI had been
  recreating on the runner with `sudo` and which no contributor could match.
- Package metadata is publishable: repository, homepage, keywords, categories,
  and an `exclude` that keeps fixture corpora out of the crate. The README
  install step names the actual repository.

## [0.2.0] - 2026-07-28

Everything since 0.1.0: two external-review waves verified and fixed
point-by-point, structural hardening (knowledge provenance split, release
quality gates, frontend fact promotion, interpreter modularization),
helper-analysis completion, analysis-coverage reporting, the fork-first
analysis layer, and the untrusted-input work that makes the analyzer usable
as a pre-execution admission check. Rule catalog now **92 implemented / 0
reserved** (was 79 / 13 at 0.1.0): `MLC114`, `MLC116`, `MLC118`,
`MLR109`, `MLR118`, `MLR122`, `MLR123`, `MLP212`, `MLP213`, `MLP214`,
`MLP217`, `MLP223`, and `MLP225` were implemented.

### Added

- **Pre-parse admission limits for untrusted sources**: source size (4 MiB),
  bracket/indentation nesting depth (96), and consecutive prefix-operator runs
  (64) are checked before parsing and reported as `MLC000`. Deeply nested calls
  or blocks, long unary/`not` runs (including runs split across line
  continuations), and multi-megabyte generated files previously aborted the
  process with a stack overflow, which no per-file error handling can contain;
  they are now ordinary skipped-file diagnostics while every other file is still
  analyzed. Bounds sit 6–10× above the Manim Community sources' own maxima, and
  re-running the corpus produced identical findings with zero `MLC000`.

- **Corpus and adversarial evidence** (`docs/research/corpus-evidence.md`):
  measured results over 393 third-party Python files (46 findings, no crash, no
  false positive; every error-severity finding triaged against its source) and
  the eight adversarial inputs above, before and after the limits. Records what
  is *not* yet evidenced: corpus breadth, fuzzing, and per-invocation time
  budgets.

- **Pre-execution admission guidance** (README): how a hosted render service
  runs the analyzer before spending sandbox and GPU time — observe mode first,
  block on `--min-confidence certain` only, and why error severity alone is the
  wrong gate (correct source can assert its own failure cases).

- **SourceBridge/rematching v0 (RFC 0004)**: `qual source-bridge`
  generates non-writing, hash-guarded literal/shift patch candidates with
  rollback text, virtually reparses and fully reanalyzes each edit, reports
  `match | ambiguous | missing`, and rejects parse failures or new coverage
  frontiers. Request/output schemas cover ambiguity, preconditions, structured
  Unknowns, and accepted/rejected validation.

- **ChangeImpact v0 (RFC 0003)**: `qual change-impact --before OLD
  --after NEW` compares two full source snapshots and emits schema-validated
  added/removed/modified definitions, conservative Scene/play/object
  candidates, deterministic reverse dependency reason paths, and structured
  Unknown frontiers. Traversing both graphs preserves deleted and renamed
  relationships without guessing cross-snapshot identity.

- **SemanticDependencyGraph v0 (RFC 0002)**: a cache-independent fact layer
  now owns deterministic dependent-to-dependency edges and reverse indexes
  across files, definitions, Scenes, plays, objects, animation targets, and
  updater hosts. Dynamic relationships remain anchored Unknown frontiers;
  cache v2 consumes only the graph's weak file-component view, leaving the
  reverse reason paths available to ChangeImpact.

- **StaticFacts v0 contract and producer (RFC 0001)**: `qual
  static-facts` emits the Draft 2020-12-schema-validated Poietra/fast-manim
  semantic bridge, with snapshot-scoped public
  IDs, encoding-aware source anchors, reason-carrying unknown values, renderer
  risks, and coverage frontiers. The projection is rule-selection independent,
  uses one immutable raw-source snapshot, and is byte-stable across worker
  counts.

- **Incremental analysis cache v2**: an exact whole-project SQLite/WAL entry
  remains the fastest unchanged warm path; after a source edit, the frontend
  rebuilds the static project graph and reuses JSON method summaries and
  filtered diagnostics from unaffected weak dependency components. Only
  changed components rerun summaries, Scene lifecycle, and cost analysis.
  Component keys cover the analyzer build, semantic config, knowledge
  profile, complete source layout, and component source bytes; asset
  manifests are validated per component. Entries are bounded to 16 project
  snapshots and 256 component snapshots, concurrent writers use one atomic
  batch transaction, and partial output is tested byte-for-byte against a
  cache-disabled full analysis.

- Phase-2 lifecycle completion: `MLC114` models project-local and curated
  `@override_animate` methods and rejects unsupported chains; `MLC116`
  follows normal Transform source/target membership into a later auto-add;
  `MLC118` intersects active updater and animation write channels across
  Manim's suspend/resume/final-updater sequence.

- Phase-3 performance completion: `MLP212` combines exact full-screen
  coverage, stable translucent style, duration, frames, and profile pixels;
  `MLP213` gates large moving Cairo Surfaces on the versioned 1,024-face
  calibration evidence; `MLP223` proves a transparent positive-width stroke
  remains invisible across every later style/opacity write before reporting
  its Cairo per-frame path cost.

- Phase-4 rendering completion: `MLR109` proves a direct leaf-updater
  dependency runs before its frame-varying writer; `MLR118` fail-closed
  scans conclusive project-local literal SVGs for unsupported content and
  missing local href targets; `MLR122` proves a Cairo `bring_to_front`
  re-add is defeated by a strictly higher exact `z_index`.

- **Analysis-coverage reporting** (the review's "trust feature"): a new
  `qual coverage [PATH...] [--format text|json]` subcommand and a
  `check --analysis-summary` flag (same report on stderr after the
  diagnostics; stdout and the exit code are untouched) that surface what
  the conservative analysis could NOT resolve — unresolved imports
  (unknown-module star imports, relative imports escaping the project
  tree), calls with empty candidate sets (with top offending names),
  plays with unknown durations, `.animate` builders with untracked
  targets, constructs above `target-python` (MLC000), resolved `manim.*`
  candidates absent from the knowledge profile, scenes with unknown
  constructor state, and helper call sites where inlining fell back to an
  effect summary (`helper_inline_fallbacks`, recorded project-wide and
  deduplicated across scenes sharing a helper chain) — per file, per
  scene, and as project totals with a count-based confidence line.
  Numbers are counts of computed facts; output is deterministic; the JSON
  keys are documented in the README.
- **Helper-analysis completion** (DESIGN §2.1, §5.1 step 5, §5.7):
  - Plays and waits inside `self.<helper>()` / `super().<helper>()`
    methods **and** project module-level helpers (same-module or imported
    from other project files, with the scene argument flowing as live
    scene state in any parameter position) materialize as real
    per-call-site lifecycle play facts anchored in the helper's file,
    with a recorded `call_path`, call-site branch/loop certainty, and
    exact per-animation argument facts. Lifecycle state queries are
    method-scoped, so liveness resolves the correct pre-play state at
    helper play sites. Third-party imports are never inlined and keep the
    DESIGN §5.3 widening semantics.
  - Summary-derived play records: method summaries carry the play/wait
    sites of the summarized body, and summary application rehydrates them
    as conservative `Maybe`-certainty `PlayFact`s with open repetitions —
    recursion and the inlining frontier no longer lose plays entirely
    (literal-duration checks such as MLC104 still fire; every
    caller-state-dependent judgment stays degraded and is listed on
    `SceneLifecycle::summary_derived_plays`).
  - `LifecycleFacts::inline_fallbacks`: every scene-run call site where
    helper inlining fell back to the effect summary, with its reason
    (`Recursion` / `DepthCap` / `Unresolvable`) — the analysis-coverage
    frontier.
  - Loop-aware play repetition facts: a `play`/`wait` inside a loop with
    a literal `range(...)` trip count multiplies its frame contribution
    by the trip-count interval; an unknown trip count opens the upper
    bound instead of counting the play once. Repetition bounds compose
    through helper inlining (call-site loops × helper-internal loops).
- **Fork-first analysis layer** over the curated `fork_capabilities`
  block of the local knowledge profile `local_0_20_1_4d25c031` (all of it
  inert under `upstream_0_20`, whose accessors return `None`):
  - The local overlay declares the fork's TeX parallel compilation (entry
    points `MathTex.precompile` / `tex_to_svg_file_async`, with in-flight
    futures forcing the Cairo fork serial fallback), the Cairo
    fork-per-play pipeline (eligibility gates, exact-type animation
    allowlist, curated blockers, and the renderer-wide monotonic disable
    after the first parent-encoded serial play), static-layer retention,
    packed interpolation thresholds, and the process-global unbounded SVG
    cache — every fact cited to the fork source; both ignored drift gates
    (upstream clean-base and overlay-vs-fork-working-tree) hold.
  - `qual cost` gains a per-scene "fork fast paths" section under
    the fork profile: per-play verdicts for the fork-per-play,
    static-layer, and packed-interpolation gates with the exact blocker
    and cause span, the monotonic-disable causal chain, and measured A/B
    evidence citations; the section never advises removing a feature.
  - `MLP214` (info/high): four or more distinct literal-provable TeX
    compile keys constructed serially before a scene's first play, with
    the fork's precompile APIs cited; duplicates count once, dynamic keys
    never count.
  - `MLP217` (warning/high): frame-varying `use_svg_cache=True` keys in
    hot callbacks growing the declared process-global unbounded SVG cache
    every frame (O(frames × family) memory growth evidence).
  - `MLP225` (info/high, opt-in): the cost report's fast-path blocker
    explanations as per-play diagnostics; `default_enabled: false`, only
    an exact `--select MLP225` evaluates it, and the registry supports
    opt-in rules cleanly (prefix selects never enable them).
  - README (en/ja) "Using the optimized fork profile" section with the
    pyproject template.
- New rule `MLR123` (error/high, phase 4): a curated OpenGL-only mesh
  mobject (`Object3D` / `Mesh` / `FullScreenQuad` from
  `manim.renderer.shader`, the `OpenGLSurface` family, or a project
  subclass provably rooted at one) definitely added to a scene while an
  active profile targets Cairo — `Camera.type_or_raise` raises
  `TypeError` at the first captured frame. Silent on OpenGL-only profile
  sets, Maybe adds, Unknown kinds, and mixed mesh/Mobject inheritance.
  Seven upstream mesh classes were curated into
  `src/knowledge/profiles/v0_20.json` (verified against the pinned clean
  base commit); as a side effect `MLR101`/`MLR126` now correctly see the
  `OpenGLSurface` family's kinds.
- **`target-python` gates syntax completely, with an honest floor**:
  after parsing, a gate over the AST, the token stream, and f-string
  token text emits `MLC000` (error) for every parse-level construct newer
  than the configured target — `async`/`await` syntax outside `async def`
  (3.7); walrus `:=`, positional-only `/`, and f-string self-documenting
  `=` (3.8); relaxed decorators and parenthesized context managers with
  `as` (3.9); `match` (3.10); `except*` and PEP 646 `*` unpacking in
  subscripts/annotations (3.11); `type` alias statements, PEP 695
  type-parameter lists, and PEP 701 f-string expressions with
  backslashes/newlines/comments (3.12) — while the gated file keeps its
  AST and is still fully analyzed. A file the gate passes silently is
  guaranteed parseable by the target's own parser; each minimum version
  is oracle-checked against CPython `ast.parse(feature_version=...)`
  where that oracle gates the construct. `--fix` rolls a file back when
  its edits would introduce gated syntax, token-detected constructs
  included. The accepted `target-python` range is **3.6–3.12**: below 3.6
  the guarantee cannot be kept (Python 3.6's own additions include
  oracle-unverifiable constructs), so older targets are explicit exit-2
  configuration errors instead of silently unenforced promises. A
  reverse-direction hint: when a file fails to parse, the configured
  target is below 3.7, and the failing line mentions `async`/`await` as a
  word, the `MLC000` message notes the source may be valid Python 3.6
  (where the two were still soft keywords).
- **Honest configuration enforcement** (exit 2 on violation):
  zero/negative/non-finite frame rates and zero-dimension resolutions
  from any tier (CLI, profile, `manim.cfg`), non-empty `stub-paths`
  (unimplemented), `manim-version` outside the loaded knowledge profile's
  supported range, and malformed or out-of-range `target-python`.
  `qual config` gained an `enforcement` section stating which
  settings are enforced and which are informational.
- **Release quality gates** (DESIGN §11.4): a labeled corpus gate
  (`tests/corpus/manifest-v1.json`, 35 cases pinning sha256 and exact
  expected diagnostics — true positives and false-positive guards across
  all four rule families, including pinned real-Manim `example_scenes`
  snapshots and the adversarial review probes) that runs inside
  `cargo test`; an explicit benchmark gate over a pinned 10k-LOC fixture
  (cold ≤ 2 s, peak RSS < 300 MiB, asserted only on the machine matching
  `benchmarks/reference-machine.json`; the warm ≤ 0.5 s budget is
  recorded but unenforced until the on-disk cache exists); and a
  scheduled knowledge-drift CI job against the pinned upstream base
  commit. Corpus re-adjudication follows the labeling protocol in
  CONTRIBUTING.md.
- **Knowledge provenance split**: `sync_manim_knowledge --manim-ref
  <commit>` generates candidates from a clean `git archive` of a commit
  instead of the working tree, so profile provenance can be checked
  against pristine upstream; the shipped `local_0_20_1_4d25c031` overlay
  carries the sibling fork's working-tree additions on top of
  `upstream_0_20` (the three fork-only `manim.constants` names, the two
  fork TeX API symbols, and the curated `fork_capabilities` block).
- **Per-updater execution liveness** (`src/cost/liveness.rs`): every
  hot-context performance fact is gated on plays/waits where the callback
  **provably** executes per frame (registration live in the heap, host
  present and not suspended by the play's own animations —
  `suspend_mobject_updating` honored at constructor and play level — and
  frames actually rendered, including `frozen_frame` handling), with
  three-valued verdicts and auditable `execution` play-span evidence on
  diagnostics.
- **Frontend statement and binding facts**
  (`src/frontend/statements.rs`): per-call enclosing-statement spans and
  roles (bare expression, `with` context, assignment RHS, return value,
  decorator), and per-file import binding facts with unified rebind
  poisoning and canonical dotted-path resolution, exposed to rules via
  `RuleContext::statement_facts()` / `binding_facts()`.
- **Scene-aware baselines**: fingerprints record the qualified enclosing
  Scene class in the `scene` field, so identical findings in different
  scenes no longer collide; files carry a
  `scene_attribution: "attributed"` provenance marker (additive;
  `schema_version` stays 1) under which an empty `scene` means literally
  "outside any Scene" and matches exactly. Files without the marker are
  read as legacy pre-attribution baselines and keep their empty-scene
  wildcard behavior.

### Changed

- `Cargo.toml` now points at the real repository instead of a placeholder URL.
- The `upstream_0_20` knowledge profile describes only the clean upstream
  base commit `4d25c031`: the three fork-only `manim.constants` names
  (`CAIRO_ANTIALIAS_MODES`, `VIDEO_ENCODERS`, `X264_PRESETS`) moved to
  the `local_0_20_1_4d25c031` overlay and resolve to Unknown under
  `knowledge-profile = "upstream_0_20"`.
- The lifecycle interpreter (`src/semantic/interpreter.rs`, 7,602 lines)
  is split into nine cohesive modules under
  `src/semantic/interpreter/`; the public API is frozen via `pub use`
  re-exports and lint output is byte-identical.
- Helper inlining during scene interpretation is bounded by call-cycle
  detection (a helper already on the active inlining path falls back to
  its effect summary) instead of a fixed depth limit, so deep
  non-recursive helper chains inline fully.
- Cost execution facts are keyed by the helper `call_path` of the play
  they prove, so the same helper reached through different call sites no
  longer merges its per-frame execution evidence across paths.
- Frontend fact computation is gated on the selected rules'
  `required_capabilities`: a run that selects no rule needing statement,
  binding, or lifecycle facts skips computing them; rules superseding a
  selected rule still run, so a narrow `--select` never resurrects a
  superseded diagnostic.
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

- Static semantic-toolchain review hardening: SourceBridge now proves that an
  existing `.shift(...)` receiver still denotes the requested object before
  proposing a patch; starred `play` arguments publish an incomplete-animation
  frontier and conservatively connect every reachable Scene object; and
  StaticFacts unknown reasons now come from explicit projection provenance,
  falling back to `unsupported-semantics` when no cause fact was retained.
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
- Configuration from `[tool.qual]` in `pyproject.toml` with render
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
