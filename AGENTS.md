# Repository instructions

Before planning or editing implementation code, read `DESIGN.md` completely. It is the authoritative product, semantic-model, rule-catalog and rollout specification for this repository.

The sibling checkout `/home/hosi/manim` is the current Manim source reference. Treat it as read-only while working in this repository unless the user separately asks to change Manim itself. Runtime linting must never import or execute Manim or analyzed user code; use static source and versioned knowledge profiles.

Implementation order:

1. Start with Phase 0 in `DESIGN.md`.
2. Keep the first three commit themes separate: source/CLI contracts, Manim knowledge/name resolution, then high-confidence rules.
3. Do not claim a reserved rule is implemented until its fixtures and acceptance criteria pass.
4. Prefer conservative `Unknown` state over a high-confidence false positive.
5. Keep upstream Manim semantics and the local optimized-fork overlay separate.

When changing a public diagnostic, configuration or JSON contract, update `DESIGN.md`, its schema tests and rule documentation in the same change. Calibration measurements belong in versioned evidence under `docs/research/`, not in machine-independent rule logic.
