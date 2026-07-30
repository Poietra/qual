# Repository instructions

Before planning or editing implementation code, read `docs/architecture.md` completely. It is the map of the implementation: the pipeline, the fact layers, the Manim semantic model the rules rest on, and the invariants every change must keep. The rule catalog is `docs/rules/`; the CLI, configuration and JSON contracts are `docs/reference/`, `docs/guides/configuration.md` and `schemas/`. `CONTRIBUTING.md` is the workflow.

`DESIGN.md` is a historical design record written in Japanese before the implementation existed, when the plan was to write qual in Python. It is not a specification, it is not synchronized with the code, and where the two disagree the code is right. Do not treat it as authoritative and do not update it to match a change. Authoritative documentation in this repository is written in English.

A local Manim checkout (`../manim`, or the path in `QUAL_MANIM_ROOT`) is the Manim source reference. Treat it as read-only while working in this repository unless the user separately asks to change Manim itself. Runtime linting must never import or execute Manim or analyzed user code; use static source and versioned knowledge profiles.

Standing rules:

1. The rule catalog is finished: 92 implemented, 0 reserved. There is no implementation phase left to work through and no reserved ID waiting to be claimed. A catalog change is a new rule ID or a fix to an existing one.
2. A released rule ID never changes meaning. Splitting a rule means a new ID.
3. Prefer a conservative `Unknown` state over a high-confidence false positive.
4. Keep upstream Manim semantics and the local optimized-fork overlay separate.

When changing a public diagnostic, configuration or JSON contract, update the affected documentation under `docs/`, its schema tests and rule documentation in the same change. Calibration measurements belong in versioned evidence under `docs/research/`, not in machine-independent rule logic.
