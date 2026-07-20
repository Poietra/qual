# RFC 0002: SemanticDependencyGraph v0

- Status: implemented
- Scope: internal semantic fact layer and public Rust API
- Consumers: incremental cache partitioning and ChangeImpact v0
- Depends on: RFC 0001 StaticFacts v0

## Summary

`SemanticDependencyGraph` records why one snapshot-local semantic entity
depends on another. It is owned by `src/semantic/dependency.rs`, not by the
cache. The canonical edge direction is always:

```text
dependent -> dependency
caller    -> callee
Scene     -> lifecycle entrypoint
play      -> enclosing definition
object    -> target play
```

The graph builds deterministic forward and reverse indexes over the same edge
set. Cache partitioning consumes only its file-to-file projection and ignores
direction. ChangeImpact starts at changed definitions in the base and target
snapshots and traverses reverse edges toward candidate Scenes, plays, and
objects.

## Boundary

Graph nodes use analyzer handles such as `FileId` and `ObjectId` because this
is an in-process fact layer. They are valid only for the source snapshot that
created the graph and must never appear in StaticFacts or ChangeImpact JSON.
External projections use RFC 0001 snapshot IDs, content hashes, source anchors,
bounded call paths, cardinality, and Scene identity.

The graph reports possible semantic influence. It does not decide editing
intent, gesture timing, follower behavior, trace plans, render skipping,
checkpoints, runtime identity, or visual equivalence.

## Nodes

The v0 graph contains:

- project source files;
- project class, function, and method definitions;
- discovered Scene classes;
- lifecycle play/wait candidates, distinguished by Scene, ordinal, source
  site, and bounded helper call path; and
- reachable object candidates, distinguished by Scene and abstract allocation
  identity (including call context and cardinality).

Definitions are looked up by qualified name only within the current snapshot.
Editing across snapshots is deliberately handled by ChangeImpact/rematching,
not by promising stable graph handles.

## Edges and reasons

Every edge stores its dependent, dependency, stable reason label, and the
source operation establishing the relationship when available. v0 reasons
are:

- `module-collision`, `import`, `namespace-binding`, `base-class`, and `call`;
- `defined-in`, `scene-class`, and `scene-entrypoint`;
- `play-definition`, `object-definition`, and `scene-ownership`; and
- `animation-target` and `updater-host`.

Module-collision edges are symmetric because either competing module can
affect ownership. All other edges retain their natural dependent-to-dependency
direction. Duplicate facts collapse by the complete ordered edge value, not
by reason alone.

The cache view contains only file-to-file edges with one of the first five
reasons. It forms weak connected components from this view. Lifecycle
ownership and target edges can therefore improve impact precision without
changing cache invalidation boundaries.

## Unknown frontier

An unresolved relationship never becomes a guessed edge. The graph stores a
`DependencyUnknown` with the owning dependent node, an exact source anchor,
and one of these stable reason labels:

- `dynamic-call-target`;
- `unresolved-base`;
- `unresolved-import`; or
- `unavailable-definition`.

ChangeImpact must carry reachable frontiers into its completeness result and
reason paths. A consumer may widen candidates or request runtime evidence; it
may not interpret absence of a guessed edge as proof of no impact.

## Construction

`SemanticDependencyGraph::from_frontend` builds file, definition, call,
inheritance, and Scene-entrypoint facts from immutable `SourceManager`,
`ProjectIndex`, and qualified-call facts. This is sufficient for cache
partitioning and does not run the lifecycle interpreter.

`attach_lifecycle` enriches the same graph with Scene executions, plays,
objects, animation targets, and updater hosts. Construction only reads parsed
source and versioned semantic facts; it never imports or executes Manim or
analyzed user code.

## Determinism and traversal

Nodes, edges, unknowns, origins, and adjacency lists use stable ordered
collections. `dependencies_of` follows canonical forward edges.
`dependents_of` follows the reverse index. `reverse_paths_from` returns one
deterministic shortest path from an origin to each affected node; ties use the
stable edge order. Multi-origin traversal preserves a path for every
`(origin, affected)` pair so one changed definition cannot hide another one's
reason path.

This Rust representation is not a serialization contract. ChangeImpact v0
will define the versioned external envelope and project graph entities through
RFC 0001 IDs and anchors.

## Acceptance

The graph is accepted when tests prove that:

- a shared helper reaches every caller Scene through reverse `call` and
  `scene-entrypoint` edges;
- plays and animation target candidates remain attributable to both caller
  Scene executions;
- a dynamic call becomes an anchored frontier instead of a guessed edge;
- forward and reverse indexes expose the same canonical edge; and
- cache component/full-output tests remain byte-identical after dependency
  discovery moves out of the cache module.
