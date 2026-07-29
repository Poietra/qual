# Contributing to manim-lint

Thank you for contributing. Two documents outrank this one:

- [`DESIGN.md`](DESIGN.md) is the **authoritative specification** — product
  scope, Manim semantic model, rule catalog, CLI/JSON contracts, and the
  implementation invariants. Read it before changing implementation code.
- [`AGENTS.md`](AGENTS.md) states the standing repository rules. The most
  important one: when you change a public diagnostic, configuration, or JSON
  contract, update `DESIGN.md`, its schema tests, and the rule documentation
  **in the same change**.

For a guided tour of the pipeline and the fact layers before diving into
code, read [docs/architecture.md](docs/architecture.md).

## Development environment

- Rust **1.85+** (stable). No other runtime dependency; the analyzer never
  imports Python or Manim.
- Optional, for knowledge-profile work only: a **read-only** sibling checkout
  of the Manim source (this repository's history used `../manim`, a 0.20.1
  lineage). It is the reference you verify semantic facts against — the
  linter itself never reads it at runtime.

## Quality gates

All four must pass before a change is done:

```bash
cargo fmt --check
cargo build
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

`cargo test` already includes the labeled corpus gate
(`tests/corpus_gate.rs`, see [Corpus labeling](#corpus-labeling)). The
two explicit release gates — the benchmark gate and the knowledge-drift
gate — are described in the
[README's release quality gates section](README.md#release-quality-gates-design-114).

Release preparation and the registry/permission bootstrap are documented in
[docs/releasing.md](docs/releasing.md). `Cargo.toml` is the single version
source; PyPI and npm metadata are derived from it.

## Repository layout

```text
src/source.rs             SourceManager: encodings (PEP 263), newlines, Unicode column mapping
src/config/               config model + loader: pyproject/manim.cfg precedence, profiles, enforcement
src/frontend/             parsing, imports/aliases, project symbol index, CFG, qualified call facts,
                          statement/binding facts (statements.rs), target-python gate (features.rs)
src/knowledge/            versioned Manim knowledge profiles (embedded JSON), loader, generator
src/semantic/             lifecycle abstract interpreter -> LifecycleFacts; the interpreter itself is
                          a directory (src/semantic/interpreter/: exec, dispatch, play, inline, mro,
                          heap_ops, callbacks, snapshots) with values/heap/events/summaries beside it
src/cost/                 symbolic cost model: hot contexts, frame intervals, execution liveness
                          (liveness.rs), fork fast-path gates (fork.rs), evidence -> CostFacts
src/render_order.rs       Cairo effective display order / moving-suffix estimation
src/rules/                rule group directories (lifecycle/, rendering/, performance/, portability/)
                          + registry (selection, capability gating, supersedes dedup)
src/reporting/            suppressions, text/json/sarif output, fixes, baseline, coverage report
src/application.rs        command orchestration (check/explain/rules/config/cost/coverage) + DOCS table
src/bin/                  sync_manim_knowledge (profile candidates + drift check)
docs/rules/               one document per implemented rule + the catalog index
docs/architecture.md      pipeline and fact-layer overview for contributors
docs/research/            versioned calibration evidence (machine-specific measurements)
schemas/                  diagnostics-v1.json, baseline-v1.json (public contracts)
benchmarks/               reference-machine.json (benchmark-gate thresholds + fingerprint)
tests/                    integration tests, golden rule fixtures (tests/fixtures/), labeled corpus
                          (tests/corpus/ + manifest-v1.json), benchmark and drift gates
```

## How to add a rule

Every rule ID already exists in the DESIGN §7 catalog with a fixed meaning,
default severity, and minimum confidence. Implementing one:

1. **Read its catalog row** in DESIGN §7.x (and any prose notes below the
   table). The `RuleMetadata` you write must match that row exactly: `id`,
   `summary`, `default_severity`, `minimum_confidence`,
   `implementation_phase`, `required_profiles`, `required_capabilities`,
   `supersedes`. Do not invent a new ID and do not change the meaning of an
   existing one — rule splits get a new ID.
   `required_capabilities` is load-bearing, not decorative: it names the
   fact layers the rule consumes (`qualified-calls`, `lifecycle`,
   `cost-facts`, `statement-facts`, ...), and `--select` gating uses it to
   skip computing fact layers no selected rule needs. A rule that queries
   a layer it does not declare will silently see nothing under a narrow
   select. Opt-in rules set `default_enabled: false` (e.g. `MLP225`);
   prefix selects never enable them — only an exact `--select <ID>` does.
2. **Implement the rule** in the matching group directory
   (`src/rules/lifecycle/`, `rendering/`, `performance/`, or
   `portability/`). Rules have no visitors of their own; they query the
   `RuleContext` fact layers (qualified calls, `LifecycleFacts`,
   `CostFacts`, statement/binding facts, profiles).
   **The canonical traversal rule (DESIGN §5.6): no module-root AST walks
   in rule code.** If your rule needs a position or binding the facts do
   not carry yet, promote it into a frontend fact
   (`src/frontend/statements.rs` or `index.rs`) instead of re-walking the
   tree; the only acceptable local traversals are fact-anchored (starting
   at a node a fact already located) and reuse the canonical frontend
   walker.
3. **Register it in your group module's `rules()` function only.** The
   registry (`src/rules/registry.rs`) composes the group lists; nothing else
   needs to change for registration.
4. **Add fixtures** under `tests/fixtures/rules/<ID>/`:
   - `invalid.py` — true positives;
   - `valid.py` — near misses that must stay silent;
   - `alias.py` — the same true positive through `import manim as mn`
     (import style must not change results);
   - `branch.py` — branch/Unknown cases where the rule must stay silent
     (or fire only where evidence is definite on all paths);
   - `suppressed.py` — an inline suppression that must win.
5. **Add a golden test** in the group's integration test file
   (`tests/rules_*.rs`) asserting the exact
   `path:line:column rule severity confidence` rows the whole fixture
   directory produces. The shared helpers also enforce non-empty
   machine-readable evidence, an explanation, alias parity, and that the
   suppressed fixture stays silent.
6. **Write `docs/rules/<ID>.md`** — what fires, what deliberately does not,
   default severity / minimum confidence / phase / fix status, and
   wrong/right code examples. Add a row link in `docs/rules/README.md`.
7. **Add the doc to the `explain` table**: the `DOCS` array in
   `src/application.rs` embeds every implemented rule's markdown via
   `include_str!`, so `manim-lint explain <ID>` works offline.
8. If the rule ships a fix, mark it SAFE only when it preserves behavior;
   anything that can change runtime semantics is UNSAFE. Fixes are re-parse
   validated with per-file rollback, and fixed-then-relinted code must be a
   no-op (idempotence).

**The honesty rule:** an ID you cannot implement soundly *stays reserved*.
`manim-lint rules` lists it as `reserved`, `check` never registers it, and
no documentation pretends otherwise. Prefer a conservative `Unknown` — and
silence — over a high-confidence false positive (AGENTS.md rule 4). A
narrower-than-catalog implementation is acceptable if its documentation
states the exact scope; a guessing one is not.

## Knowledge-profile contributions

The files in `src/knowledge/profiles/` are curated, reviewed data — not
generated dumps (see the README in that directory):

- **Every fact must be verified against the Manim source** (the sibling
  checkout / the version the profile names). Record the `source_digest` the
  profile was verified against.
- Profiles are versioned and reviewed like code; changes need the same
  scrutiny as a rule change, because wrong facts become wrong diagnostics.
- The JSON must stay **byte-stable** (stable key order, stable formatting) so
  profile diffs are reviewable and output stays deterministic.
- Overlays (`local_*.json`) name their base via `base_profile` (`name` and
  `source_digest` must match exactly), replace whole symbol entries by
  qualified key, and delete via `deleted_symbols` / `deleted_exports`. There
  is no recursive deep merge, and overlay chains are rejected. Keep upstream
  semantics and local-fork overlays strictly separate (AGENTS.md rule 5).
- Never propose an API or setting that does not exist in the targeted
  profile.

Fork-overlay curation (the `fork_capabilities` block of
`local_0_20_1_4d25c031.json`) has additional rules:

- The block is **reviewer-curated only**: the generator neither emits nor
  drift-checks `fork_capabilities`. Every capability carries a `note`
  citing the exact fork source it was read from.
- Overlay *symbols* (e.g. the fork's TeX precompile APIs) are still
  drift-checked against the fork working tree
  (`sync_manim_knowledge --manim-root ../manim --diff
  local_0_20_1_4d25c031` must be clean).
- Everything fork-gated must be provably inert under `upstream_0_20`:
  the upstream profile has no `fork_capabilities` block and its accessors
  return `None` — a fork-gated rule or cost section must key off that,
  never off the profile name.
- Capability `entry_points` must be curated symbols of the resolved
  profile, so advice can never cite an API the selected profile lacks.
- Calibration numbers quoted in capability notes are machine-specific
  evidence (`docs/research/perf-evidence.md`), never portable truth.

Calibration measurements belong in versioned evidence under
`docs/research/`, not in machine-independent rule logic.

## Corpus labeling

`tests/corpus/manifest-v1.json` is the labeled release corpus (DESIGN
§11.4), enforced by `tests/corpus_gate.rs` on every `cargo test`. Each
case pins:

- `path` — the case source under `tests/corpus/`;
- `sha256` — digest of that exact source (label-revision safety: labels
  describe one byte-exact input);
- `label_revision` — bumped on every re-adjudication;
- `classification` — `true-positive` (all expected diagnostics are
  adjudicated real defects), `false-positive-guard` (adjudicated silent:
  any diagnostic is a false positive), or `mixed` (expected true
  positives plus rules that must stay silent, listed under `guards`);
- `expected` — the exact diagnostics (`rule`, `line`, `column`,
  `severity`, `confidence`), each itself classified `true-positive`;
- `provenance` — where the adjudication happened (a golden test, a review
  probe, or the real-Manim example_scenes verdict).

### Adding a new case

1. Put the standalone source under `tests/corpus/cases/` (external
   snapshots keep a license note — see
   `tests/corpus/cases/manim_example_scenes/README.md`).
2. Run the default check over the file **in isolation** and adjudicate
   every diagnostic by hand against Manim semantics (the DESIGN §3
   model / the pinned Manim source). A diagnostic you cannot justify as a
   true positive is a bug to fix first, not a label to record.
3. Add the manifest entry with `label_revision: 1`, the source sha256,
   and the adjudicated expectations; state the provenance.
4. `cargo test --test corpus_gate` must pass.

### Re-adjudication (label revision bumping)

The gate fails in two distinct ways, and neither may be answered by
mechanically re-recording observed output:

- **sha256 mismatch** — the case source changed after labeling. Restore
  the pinned source, or re-adjudicate from scratch: repeat step 2 above
  on the new source, then update `sha256`, the expectations, and bump
  `label_revision` in the same change.
- **diagnostic mismatch** — analyzer behavior changed. If the new
  behavior is wrong, fix the analyzer. If it is intentionally better,
  re-adjudicate each changed diagnostic by hand, update `expected`, and
  bump `label_revision`; the PR must say *why* every changed line is now
  the correct verdict.

Deleting or weakening a `false-positive-guard` case needs the same
justification as deleting a regression test: these cases are the pinned
form of real review findings.

## Contributor checklist — the DESIGN §15 invariants

Every change must keep all of these:

1. Never import or execute the analyzed code or Manim.
2. **Never emit a certain/high diagnostic from `Unknown` facts.**
3. Do not collapse scene membership and visibility into one boolean.
4. Keep Animation construction and play lifecycle effects as distinct points
   in time.
5. Introducer / remover / replacement and auto-add behavior come from the
   knowledge profile, explicitly.
6. Keep live mobjects and their starting/target copies as distinct
   identities.
7. Do not confuse frame-callback frequency with play-start frequency.
8. Renderer-specific diagnostics always carry their applicable profiles.
9. Never fabricate precise numbers from unknown performance values.
10. Autofixes are parse-validated; SAFE and UNSAFE are never mixed.
11. **Source spans are Unicode-correct** and round-trip (Japanese source is
    a first-class test case).
12. **Diagnostic order and serialized output are deterministic** —
    byte-stable for the same input.

## Public contracts

The JSON envelope (`schemas/diagnostics-v1.json`), the baseline format
(`schemas/baseline-v1.json`), SARIF output, rule IDs and their meanings,
exit codes, and the configuration schema are public contracts. A released
rule ID never changes meaning. If your change touches any of these, update
`DESIGN.md`, the schema tests, and the affected rule docs in the same
change — a PR that changes a contract in code only will not be accepted.
