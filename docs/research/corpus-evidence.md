# Corpus and adversarial evidence

Measurements taken before promoting manim-lint from a developer tool to a
pre-execution admission check for a hosted render service. They record what the
analyzer actually did on real third-party sources and on inputs chosen to break
it — not what its own test suite asserts.

## Snapshot

- Captured: 2026-07-28 (Asia/Tokyo)
- Analyzer: `feat/admission-readiness`, built from the tree that adds
  `src/source_limits.rs`
- Corpus: `Poietra/fast-manim` at `main` (an independent performance fork of
  Manim Community 0.20), 393 Python files, 5.6 MB
- Also analyzed: the four Manim sources in `Poietra/studio-lab`
  (`examples/relativity.py`, `server/test-fixtures/real-manim-smoke.py`,
  `fixtures/preview-harness/shared_circle_opacity.py`,
  `fixtures/real-preview-harness/scene.py`)
- Machine: WSL2, Linux 6.6.87.2, x86-64, 16 logical CPUs

## Corpus results

| Target | Files | Findings | Wall time | Process outcome |
| --- | --- | --- | --- | --- |
| `example_scenes/` | 4 | 1 (`MLP208`, info) | 0.18 s | clean exit |
| `tests/` | 200 | 22 | 5.99 s | clean exit |
| whole repository | 393 | 46 | 26.9 s | clean exit |
| studio-lab Manim sources | 4 | 0 | < 0.2 s | clean exit |

No crash, no panic, no stderr output, and no `MLC000` on any corpus file.

### Triage of every error-severity finding

All 17 error-severity findings in `tests/` were inspected against their source
lines. **None is a false positive**, but only some are actionable:

- 13 are inside `pytest.raises` / `try ... except` blocks that deliberately
  construct the invalid call the rule describes (`VGroup(3.0)`,
  `obj.add(Mobject())`, `AnimationGroup()`): the diagnostic is factually
  correct, and the test proves it by asserting the same failure.
- 2 (`MLR105`, unbalanced Pango markup) are the same pattern written with a
  bare `try` instead of `pytest.raises`.
- 1 (`MLC105`, an updater whose parameters cannot bind) is in a scene fixture.
- 1 (`MLP206`) flags a 1 ns `wait`, which is a test artifact.

The 5 library-side `MLC126` findings (`probability.py`, `code_mobject.py`,
`three_dimensions.py`, `debug.py`) each sit on a call whose child is a value the
analyzer cannot conclusively type, and each is reported at `high` — not
`certain` — confidence.

**Consequence for admission use:** a gate keyed on severity alone would reject
correct source that intentionally exercises failure paths. An admission check
must key on `--min-confidence certain` and observe `high` findings before ever
blocking on them. That is the gradual path documented in the README.

### False-positive probe

A scene using project-local subclasses (`class LocalBox(VGroup)`), an unresolved
third-party import, and helper-returned mobjects produced no findings — the
`conclusive` requirement on negative claims held: unresolved bases stay silent
rather than guessing.

## Adversarial results

Inputs were generated to attack the parser and analysis passes, not the rules.
Before the limits in `src/source_limits.rs`, four of eight aborted the process:

| Input | Before | After |
| --- | --- | --- |
| 100 000 nested parentheses | clean exit | `MLC000` bracket depth |
| 4 000 nested `VGroup(` calls | **stack overflow, SIGABRT** | `MLC000` bracket depth |
| 20 000 nested `VGroup(` calls | **stack overflow, SIGABRT** | `MLC000` bracket depth |
| 5 000 nested `if` blocks (50 MB) | **stack overflow, SIGABRT** | `MLC000` source size |
| 12 MB / 2 000 000 statements | **> 30 s, no result** | `MLC000` source size |
| 50 000 unary `-` operators | **stack overflow, SIGABRT** | `MLC000` prefix run |
| 30 000 `-` split by line continuations | **stack overflow, SIGABRT** | `MLC000` prefix run |
| 50 000 chained `not` | **stack overflow, SIGABRT** | `MLC000` prefix run |

A stack overflow in a Rust process is an abort, not a catchable error: no
`catch_unwind` and no per-file isolation can contain it. The only safe response
is to refuse the input before the recursion starts, which is what the pre-parse
check does.

Nesting thresholds were located by bisection (1 000 nested calls survived, 4 000
did not; 2 000 nested blocks survived, 5 000 did not). They are stack-size and
build dependent, so the enforced bounds are set an order of magnitude below the
lowest observed failure rather than at it.

### Headroom against real code

| Measure | Corpus maximum | Enforced limit | Headroom |
| --- | --- | --- | --- |
| bracket nesting depth | 15 | 96 | 6.4× |
| indentation depth | 13 (53 leading spaces) | 96 | 7.4× |
| file size | 419 KB | 4 MiB | 10× |

Re-running the whole corpus after the limits produced **the identical 46
findings and zero `MLC000`**, so no real file is affected.

## Not yet evidenced

- Corpus breadth is one fork plus one consumer. Third-party Manim projects
  written by other authors have not been measured.
- No fuzzing campaign: the adversarial inputs are hand-constructed classes of
  attack, not a coverage-guided search. `cargo-fuzz` over `SourceManager` remains
  open work.
- The prefix-run and depth bounds are empirical for this build. A release-mode
  binary, another platform, or a thread with a different stack size shifts the
  true failure point; the bounds are conservative but not derived from a proof.
- Analysis time is bounded only by the source-size limit. A wall-clock budget
  per invocation is the caller's responsibility.
