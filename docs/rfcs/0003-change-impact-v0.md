# RFC 0003: ChangeImpact v0

- Status: implemented
- Command: `manim-lint change-impact --before OLD --after NEW`
- Schema: `schemas/change-impact-v0.json`
- Depends on: RFC 0001 StaticFacts v0 and RFC 0002 SemanticDependencyGraph v0

## Summary

ChangeImpact compares two immutable source snapshots and reports the Scenes,
plays, and objects that may be affected by their source differences. The
result is conservative static evidence, not an editing-intent decision or a
runtime trace plan.

Both snapshots are required. A target-only graph cannot retain a call or
definition that was deleted or renamed; therefore v0 analyzes both source
trees and traverses both graphs. Removed relationships are visible in the
base candidates and newly introduced relationships in the target candidates.

## Inputs and analysis boundary

`--before` and `--after` each name one Python file or project directory. Each
tree independently discovers its project root and configuration, reads every
selected source exactly once, and runs the frontend, lifecycle interpreter,
StaticFacts projection, and semantic dependency graph. ChangeImpact does not
run lint rules and does not read or write the analysis cache.

The optional `--profile`, `--renderer`, `--fps`, and `--resolution` overrides
apply to both snapshots. If the resolved semantic configuration or knowledge
profile differs between the snapshots, v0 conservatively seeds every source
definition in both graphs, reports all reachable candidates, and emits a
`semantic-config-changed` frontier.

Analysis reads parsed source and versioned knowledge only. It never imports or
executes Manim, plugins, or analyzed user code.

## Changed source facts

`changed_files` compares project-relative POSIX paths and raw-content hashes.
It distinguishes `added`, `removed`, and `modified` files and retains the
available base/target hashes.

`changed_definitions` compares qualified project class/function/method names.
Its source fingerprint covers definition kind, relative path, and the exact
decoded definition range. Definition anchors include raw hashes, so output
preconditions still refer to the exact source snapshot. A rename is represented
as one removed definition and one added definition; v0 does not guess that
they are the same entity.

Changed files and definitions both seed impact traversal. The file seed is
intentionally conservative: module-level imports, re-exports, assignments,
decorators, or parse changes can alter every definition in the file even when
an individual definition's text is unchanged.

## Affected candidates

Affected Scene, play, and object records contain:

- `snapshot`: `base` or `target`;
- the exact snapshot-scoped StaticFacts ID;
- the owning Scene ID for plays and objects, or qualified Scene name for a
  Scene; and
- the StaticFacts-compatible source anchor.

IDs are never compared across snapshots. Base candidates preserve evidence
for deleted relations; target candidates describe relations present after the
edit. Cross-snapshot identity and patch-result rematching belong to the
[`SourceBridge v0`](0004-source-bridge-v0.md) rematching contract.

## Reason paths

Every reason path names its snapshot, changed file/definition origin, affected
public entity, and the ordered semantic dependency reasons traversed from the
origin. Stored graph edges retain RFC 0002's dependent-to-dependency direction;
the path array is in reverse traversal order from changed dependency toward
affected dependent.

One deterministic shortest path is emitted for each `(origin, affected)` pair.
Multiple changed origins are not collapsed, because callers may need to know
that two independent edits both reach one candidate.

## Completeness and Unknown

`unknown_frontiers` is an array of structured frontier records. Every frontier
has an owning node and a non-empty `reasons` array. v0 reasons are dynamic call
target, unresolved base/import, unavailable lifecycle attribution, decode or
parse failure, starred play arguments, and semantic configuration/profile
change. A reached `star-arguments` frontier keeps completeness at `candidates`;
`starred-animation-target` edges widen affected objects to the owning Scene.

`completeness` is `complete` only when no relevant frontier was reached.
Otherwise it is `candidates`: the listed entities remain valid conservative
candidates, but a consumer must widen selection or obtain runtime evidence.
Absence of a guessed dynamic edge is never proof that no impact exists.

## Determinism

Identical before/after raw snapshots and semantic inputs produce byte-identical
JSON. Records are sorted by stable path/name/snapshot/public ID keys, reason
paths and frontiers are sorted and deduplicated by their complete JSON value,
and serialization uses the same fixed pretty-print policy and trailing newline
as StaticFacts. The output contains no host paths, timestamps, process IDs,
worker ordering, cache state, or internal analyzer handles.

## Non-goals

ChangeImpact v0 does not decide:

- editing intent such as “after the click” or follower semantics;
- cross-snapshot object/play identity;
- source patches or whether a patch preserves arbitrary Python meaning;
- RuntimeObjectId, PlayExecutionId, StaticFacts/RenderTrace matching;
- TracePlan, render skipping/fork permission, checkpointing; or
- pixel/object-aware visual validation.

## Acceptance

The contract is accepted when tests prove that:

- a shared helper change reaches every caller Scene;
- a deleted or renamed helper remains traceable through the base graph;
- dynamic calls produce reason-carrying Unknown and `candidates`
  completeness;
- affected IDs all come from their snapshot's StaticFacts document;
- representative output validates against Draft 2020-12;
- a frontier with an empty `reasons` array and an unknown field are rejected;
  and
- repeated runs emit byte-identical JSON.
