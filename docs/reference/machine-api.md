# Machine APIs

Qual is a command-line application, not a network service or a public Rust
library. Its supported integration surface is a set of versioned CLI and JSON
contracts. An integration should invoke the executable, check the exit code,
and validate JSON against the matching schema version.

## Diagnostics

```bash
qual check . --format json > diagnostics.json
```

Use diagnostics when a person or CI system needs actionable findings. Rule
selection, suppressions, confidence thresholds, and baselines apply.

[Diagnostics schema](schemas.md#diagnostics-and-operational-output)

## StaticFacts v0

```bash
qual static-facts . > static-facts.json
```

StaticFacts exposes a stable projection of Scenes, reachable objects, plays,
animations, updaters, membership/order boundaries, renderer risks, and
reason-carrying unknown frontiers. It is intentionally independent of rule
selection and suppression.

[Contract](../rfcs/0001-static-facts-v0.md) ·
[Schema](schemas.md#semantic-toolchain)

## ChangeImpact v0

```bash
qual change-impact --before old-tree --after new-tree > impact.json
```

ChangeImpact analyzes both snapshots and returns conservative Scene, play,
and object candidates affected through the semantic dependency graph. It
retains deleted edges from the base snapshot rather than guessing renames.

[Contract](../rfcs/0003-change-impact-v0.md) ·
[Schema](schemas.md#semantic-toolchain)

## SourceBridge v0

```bash
qual source-bridge . --request request.json > candidates.json
```

SourceBridge generates a deliberately bounded set of hash-guarded patch
candidates, applies each one only in memory, reanalyzes the source, and emits
accepted or rejected rematching results. The command never writes the
analyzed project.

[Contract](../rfcs/0004-source-bridge-v0.md) ·
[Request and response schemas](schemas.md#semantic-toolchain)

## Compatibility rules

1. Read `schema_version` before interpreting a document.
2. Treat IDs as snapshot-scoped unless the contract explicitly says otherwise.
3. Preserve reason-carrying `Unknown` values; do not coerce them to false.
4. Do not deserialize undocumented internal Rust types.
5. Pin a Qual version when consuming a contract in production.
