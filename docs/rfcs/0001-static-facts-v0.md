# RFC 0001: StaticFacts v0 public contract

- Status: implemented (initial full-analysis producer)
- Contract: `schemas/static-facts-v0.json`
- First producer: `manim-lint static-facts [PATH...]`
- Follow-ups: StaticFacts projection, semantic dependency graph, ChangeImpact

## Summary

`StaticFacts v0` is a stable, versioned projection of manim-lint's static
semantic model for Poietra and fast-manim. It is not a serialization of
`LifecycleFacts`, the abstract heap, cache entries, or internal handles. The
producer converts those implementation details into snapshot-scoped public
identifiers, self-contained source anchors, reason-carrying unknown values,
and deterministically ordered records.

The v0 surface is deliberately limited to:

- discovered Scenes;
- reachable objects and their provenance;
- play/wait groups and their animations;
- updater registrations;
- membership and render order at play boundaries;
- renderer risks/blockers; and
- coverage frontiers where the analysis lost precision.

This RFC defines the contract boundary and the minimum schema. The initial
producer is implemented by `src/static_facts.rs` and exposed by the
`static-facts` CLI command.

## Goals and non-goals

The contract must let a consumer answer “which static candidates might this
runtime or source operation refer to?” without depending on manim-lint's Rust
types. In particular, two calls to the same helper from different call sites
remain distinct candidates, and an allocation in a loop is never represented
as a singleton.

The following remain outside manim-lint's responsibility:

- runtime object or play-execution identifiers;
- final StaticFacts/RenderTrace matching;
- gesture timing, follower semantics, or a final TracePlan;
- render-cache, checkpoint, or fork decisions;
- visual validation; and
- `safe_to_skip_render`, `safe_to_fork`, or any equivalent optimization
  permission.

StaticFacts reports evidence that may block an optimization. Only a renderer's
runtime proof may grant one.

## Envelope and semantic inputs

A document has `schema_version: 0` and contains:

- the producing tool version and semantic build digest;
- the source snapshot identity and resolved semantic configuration;
- the selected, versioned Manim knowledge profile and its source digest;
- render profiles considered by the analysis;
- source-file records;
- Scenes, objects, plays, animations, and updaters;
- renderer risks; and
- coverage frontiers.

Rule selection, suppression, confidence thresholds, baselines, output format,
and fail level are not semantic inputs. The producer requests every
fact capability needed by the projection regardless of `--select` or
`--ignore`. Consequently, those options do not enter `semantic_config_hash` or
the snapshot ID.

The project root is intentionally absent. Paths are project-relative POSIX
paths, so moving the same source snapshot to another machine does not change
the output.

## Source anchors

Every source-backed public fact uses a `SourceAnchor` containing:

- project-relative POSIX `path`;
- SHA-256 of the exact on-disk bytes, before decoding;
- the normalized source `encoding` and whether a UTF-8 BOM was present;
- an end-exclusive byte range in the parser-visible Unicode text encoded as
  UTF-8; and
- an end-exclusive line/column span.

Lines and columns are one-based. A column counts Unicode scalar values from
the start of its physical line; it is neither a UTF-8 byte column nor a
terminal display-cell/grapheme column. The UTF-8 byte range addresses the
decoded parser text after BOM handling. `files[].decoded_utf8_hash` hashes
exactly that text. These rules make Shift-JIS/CP932 and Japanese anchors
round-trip without confusing raw-file offsets with parser offsets.

An anchor's `raw_content_hash` must equal the corresponding file record. A
consumer must reject or re-request facts when the hash does not match its
source bytes.

## Snapshot-scoped identifiers

Internal `FileId`, `ObjectId`, `PlayGroupId`, allocation indices, and cache
keys never cross the contract boundary. Public IDs are opaque strings with a
kind prefix and SHA-256 payload:

```text
snapshot:sf0:<64 lowercase hex digits>
scene:sf0:<64 lowercase hex digits>
object:sf0:<64 lowercase hex digits>
play:sf0:<64 lowercase hex digits>
animation:sf0:<64 lowercase hex digits>
updater:sf0:<64 lowercase hex digits>
risk:sf0:<64 lowercase hex digits>
frontier:sf0:<64 lowercase hex digits>
```

The hash input is a compact UTF-8 JSON array, serialized by
`serde_json::to_vec`. Arrays, rather than JSON objects, fix field order. Hash
inputs contain only strings, integers, booleans, and nested arrays. A source
anchor in an ID preimage is represented as:

```text
[path, raw_content_hash, encoding, bom,
 utf8_start, utf8_end, start_line, start_column, end_line, end_column]
```

The domain-separated preimages are:

```text
snapshot = ["static-facts-v0", "snapshot", tool_version,
            semantic_build_hash, target_python, semantic_config_hash,
            [knowledge_name, knowledge_schema_version, knowledge_source_digest],
            sorted [[path, raw_hash, encoding, bom, decoded_utf8_hash], ...]]

scene     = ["static-facts-v0", "scene", snapshot_id,
            qualified_name, definition_anchor]

object    = ["static-facts-v0", "object", snapshot_id, scene_id,
            allocation_anchor, bounded_call_context, cardinality]

play      = ["static-facts-v0", "play", snapshot_id, scene_id,
            call_anchor, bounded_helper_call_path, cardinality]

animation = ["static-facts-v0", "animation", snapshot_id, play_id,
            source_argument_ordinal, source_anchor]

updater   = ["static-facts-v0", "updater", snapshot_id, scene_id,
            registration_anchor, bounded_call_context, registration_ordinal]
```

Risk and frontier IDs use the same domain prefix followed by the snapshot ID,
entity kind, risk/frontier kind, primary anchor, and sorted related entity IDs.

Object call contexts keep the most recent two call sites, oldest first, which
matches the v0 `k = 2` allocation identity. Play helper paths contain at most
32 call sites, outermost first. Reaching the helper depth cap produces a
coverage frontier; the producer must not fabricate the missing suffix.

IDs are stable only within one source snapshot and one v0 semantic producer.
They are not edit-stable IDs. Identity across edits belongs to the later
rematching contract and is reported as `Match | Ambiguous | Missing`.

## Cardinality and execution certainty

`cardinality` is one of `singleton`, `many`, or `maybe-many`. It describes how
many runtime instances an abstract candidate may represent and participates
in object and play IDs. `maybe-many` is a known conservative abstraction, not
a reason-less null.

Play `repetitions` separately retains exact, interval, or symbolic numeric
information. `execution_certainty` says whether the source operation executes
on all analyzed paths. A branch-dependent operation therefore has unknown
execution certainty with a `branch-join` reason even if its candidate identity
and repetition interval are otherwise known.

## Unknown values and provenance

No missing semantic fact is represented by `null`. A field-specific known
shape is paired with this unknown shape:

```json
{
  "status": "unknown",
  "reasons": [
    {
      "kind": "dynamic-call-target",
      "anchor": {
        "path": "scene.py",
        "raw_content_hash": "sha256:...",
        "encoding": "utf-8",
        "byte_order_mark": false,
        "utf8_byte_range": { "start": 120, "end": 132 },
        "unicode_span": {
          "start": { "line": 8, "column": 9 },
          "end": { "line": 8, "column": 21 }
        }
      }
    }
  ]
}
```

An unknown candidate set may retain conservative `candidates`; unknown write
channels may retain `known_channels`; an unknown order may retain the
unordered `candidate_object_ids`. Those partial facts never turn an unknown
status into a proof.

The producer may keep the existing internal `Num`, `Truth`, and `Presence`
lattices. It records provenance in a sidecar keyed by internal fact identity
and field, then joins/deduplicates reasons during projection. Reasons are
sorted by kind, anchor, and related IDs. If provenance is unexpectedly absent,
the projection uses `unsupported-semantics` at the owning source anchor; it
never emits a reason-less unknown.

The v0 reason taxonomy is closed by the JSON Schema. A new reason kind is a
contract change and requires a schema-version decision.

The projection also distinguishes `non-literal-expression` (a numeric value
is not statically reducible), `untracked-value` (the semantic heap has no
projectable value for a reachable candidate), and `unknown-callback` (an
updater registration is known but its callback target is not). These are
precision boundaries, not permission to guess a value.

## Entity records

### Scenes and objects

A Scene record identifies the qualified class and definition anchor, then
lists its public object, play, and updater IDs. Constructor availability is a
reason-carrying truth fact.

An object is reachable when it appears in a Scene/play membership snapshot,
is an animation/updater candidate, remains in the final heap, or is connected
to one of those objects by a parent/child, copy, generated-target, or
replacement relation. The projection does not dump every interpreter
temporary. Each object exposes allocation anchor, bounded call context,
cardinality, kind candidates, binding/name candidates, relations, and final
membership.

### Plays and animations

A play record covers one abstract `Scene.play`, `wait`, or `pause` execution
candidate. It exposes its source call, helper path, cardinality, certainty,
repetition count, duration, ordered animation IDs, and membership/render-order
facts at three boundaries:

- `before`: immediately before Manim compiles/begins the group;
- `during`: after begin/introducer/auto-add effects and before finish cleanup;
- `after`: after remover/replacement cleanup.

For a `known` membership boundary, `entries` is the exact set of objects that
may be present at that boundary; an omitted Scene object is known absent. For
an `unknown` boundary, entries are only the proven subset and omission carries
no meaning. A `known` render-order list is the exact effective display order;
an unknown order exposes only an unordered conservative candidate set.

An animation record distinguishes its resolved kind candidates, so
`Transform` and `ReplacementTransform` cannot collapse merely because they
share targets. It reports live target candidates, write channels, and
introducer/remover/replacement effects. Unknown targets and write channels are
reason-carrying unknown facts and also generate renderer risks.

### Updaters

An updater record identifies the registration site and host/callback
candidates. Hosts may be the Scene itself or reachable objects. It reports
time-based behavior, possible write channels, and the
play candidates during which it may be active. A possible active updater is a
risk; StaticFacts does not conclude that a renderer optimization is unsafe or
safe from that fact alone.

## Renderer risks and coverage frontiers

`renderer_risks` is a one-way list of possible blockers:

- dynamic call;
- unknown animation target;
- active updater;
- `always_redraw`;
- dynamic wait or stop condition;
- camera mutation;
- external state or I/O;
- randomness;
- unknown write channel; and
- unknown render order.

Each record has `certain` or `possible` certainty, a primary anchor, related
candidate IDs, and applicable render profiles. The schema intentionally has no
optimization-permission field.

`coverage_frontiers` reports where exact projection stopped: decode/parse
failure, unresolved or dynamic resolution, recursion/depth fallback, branch or
loop widening, candidate caps, and unsupported semantics. Unknown fields refer
to the same structured reason taxonomy. A top-level completeness value is
`complete` only when no frontier exists; otherwise it is `partial`.

## Deterministic serialization

For identical inputs, output must be byte-identical across runs and worker
counts. When the semantic dependency graph adds an incremental path, it must
also be byte-identical across cache hits, incremental analysis, and full
analysis. The producer therefore:

1. emits no timestamps, host paths, process IDs, or nondeterministic map order;
2. sorts files by path and every entity array by public ID;
3. sorts ID/reference sets lexicographically and preserves only semantically
   ordered arrays such as animations and call paths;
4. sorts/deduplicates unknown reasons and risk profile names;
5. serializes with the repository's single fixed pretty-print policy and one
   trailing newline; and
6. constructs the projection from the same immutable source-byte snapshot used
   for parsing and hashing.

The initial CLI always performs full analysis, so cache state cannot affect
facts. Incremental and full producers must eventually feed the same projection
and pass byte-stability tests before an incremental route is enabled.

## Security and execution boundary

Generating StaticFacts reads project source and versioned knowledge profiles.
It must never import or execute Manim, plugins, analyzed user code, module-level
expressions, callbacks, or patch results. Any unavailable dynamic behavior
becomes a risk/frontier with reasons.

## Follow-up contracts

The semantic dependency graph is a fact layer, not the cache partition. Cache
components may consume its forward edges, while ChangeImpact consumes its
reverse edges and projects reason paths through StaticFacts IDs and anchors.
ChangeImpact compares two source snapshots so deleted and renamed definitions
remain discoverable from the base graph.

Source patching and rematching are defined by RFC 0004. They consume these
anchors and raw hash preconditions through separate request/output schemas.
Arbitrary Python rewrites, complete CST preservation, and dynamic-call
rewrites are not implied by either contract.

## Acceptance and rollout

This contract-definition change is accepted when:

- the schema itself parses as Draft 2020-12;
- a representative fixture validates;
- unknown values without reasons and unknown extra fields are rejected; and
- the fixture demonstrates Japanese/Shift-JIS anchor metadata, distinct helper
  call contexts, non-singleton loop allocation, Transform versus
  ReplacementTransform, updater/risk facts, and a dynamic-call frontier.

The initial producer additionally proves:

- worker counts produce byte-identical JSON;
- rule selection does not affect StaticFacts;
- source anchors round-trip against raw Shift-JIS and Japanese source; and
- neither Manim nor analyzed code is imported or executed.

Incremental/full equality remains the acceptance gate for the later semantic
dependency graph integration; the initial `static-facts` command has no
incremental execution path.
