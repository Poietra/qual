# RFC 0004: SourceBridge and rematching v0

- Status: implemented
- Command: `manim-lint source-bridge PATH --request REQUEST.json`
- Request schema: `schemas/source-bridge-request-v0.json`
- Output schema: `schemas/source-bridge-v0.json`
- Depends on: RFC 0001 StaticFacts v0

## Summary

SourceBridge converts one exact StaticFacts entity request into bounded local
source-edit candidates, then applies every candidate only to an in-memory copy,
reparses and fully reanalyzes that copy, and reports semantic rematching as
`match`, `ambiguous`, or `missing`.

The command never writes project files. Each edit contains its original text,
replacement, exact raw-content hash, and source anchor, so a caller can verify
the precondition and construct the inverse edit before performing any external
write.

## Supported templates

v0 supports exactly three operations:

1. `replace-literal-argument`: replace one positional or explicit keyword
   argument at a hash-guarded call anchor. The current argument and replacement
   must both be static literals. Dynamic current expressions are unavailable.
2. `modify-existing-shift`: require one known binding for the target object,
   find existing `binding.shift(ARG)` calls with one unsplatted positional
   argument, and produce one candidate per call. Multiple calls remain
   `ambiguous`; SourceBridge does not choose one.
3. `insert-shift-chain`: require one known binding and a clear allocation-call
   anchor, then insert `.shift(ARG)` directly after that call.

Shift arguments may be arbitrary syntactically valid Python expressions, but
post-edit coverage validation rejects a candidate that introduces new unknown
semantics. This permits normal Manim expressions such as `RIGHT * 2` without
claiming arbitrary expression-level meaning preservation.

No general CST rewrite, dynamic-call rewrite, statement motion, arbitrary
method insertion, or Python equivalence proof is part of v0.

## Request preconditions

Every request identifies:

- schema version zero;
- the exact StaticFacts snapshot ID;
- one snapshot-scoped Scene/play/object ID; and
- one versioned operation.

Literal replacement additionally supplies the call path, raw SHA-256, and
decoded UTF-8 byte range. A snapshot, path, raw hash, target, call, argument,
binding, or allocation mismatch yields structured Unknown and no candidate.
The call must be the target object's allocation anchor or the target play's
call anchor; an unrelated call is never patched. JSON that violates the
request schema is a command error (exit 2). The analyzer never falls back to
fuzzy text search.

## Candidate edit sets

Candidates have content-derived `patch:sb0:` IDs and `high` confidence for one
structural source match. Multiple structurally valid source matches receive
`medium` confidence and remain separate candidates.

Each v0 candidate contains one edit, represented as an array for future
templates. An edit carries:

- project-relative path and exact raw-content hash precondition;
- the full encoding-aware source anchor;
- exact `original_text`; and
- replacement text.

The original text is both a second application guard and the rollback payload.
Candidate generation and validation do not mutate the working tree.

## Virtual validation

For each candidate, SourceBridge:

1. verifies raw hash, byte boundaries, and original text;
2. edits decoded text in memory;
3. re-encodes it using the original PEP 263 encoding;
4. reparses every source from the modified immutable raw snapshot;
5. reruns frontend, lifecycle, dependency graph, and StaticFacts projection;
6. rematches the requested entity; and
7. compares structured coverage frontiers before and after.

A candidate is `accepted` only if parsing succeeds, rematching is exactly one
`match`, and coverage is `preserved`. Parse failure, `ambiguous`/`missing`
rematch, or new frontier kinds rejects it with structured reasons. The known
frontend-only dynamic-call frontier introduced by the inserted Mobject
`.shift(...)` syntax is discounted once only when its post-edit anchor exactly
ends at the generated chain; additional dynamic calls inside its argument
still decrease coverage and reject the candidate.

## Rematching

IDs are intentionally different after every source snapshot edit. Rematching
therefore uses semantic attributes rather than ID equality:

- Scene qualified identity;
- entity kind and owning Scene identity;
- path and edit-adjusted allocation/play source range;
- bounded helper call path;
- object cardinality, kind candidates, and binding candidates; or
- play kind and cardinality.

Zero candidates is `missing`, one is `match`, and more than one is
`ambiguous`. SourceBridge returns every candidate ID for the latter and never
chooses by source order.

## Security and responsibility boundary

Generation and virtual validation statically parse source and use versioned
knowledge profiles. They never import or execute Manim, plugins, module-level
expressions, callbacks, or user code.

The caller owns external file writes, multi-request conflict resolution,
gesture/time/follower semantics, runtime identity, TracePlan, rendering,
checkpoints, and visual validation.

## Determinism and acceptance

Identical source and request bytes produce byte-identical candidate order,
patch IDs, rematching, and JSON across worker counts. Acceptance tests cover:

- all three rewrite templates;
- positional and keyword literal selection;
- multiple shift candidates without selection;
- lexical-context isolation for same-named bindings;
- dynamic literal and hash mismatch refusal;
- target/call association and request-contract validation;
- in-memory reparse/reanalysis and `match`;
- direct `match`, `ambiguous`, and `missing` rematch states;
- new Unknown/coverage regression rejection;
- rollback text and non-writing behavior;
- Shift-JIS/Japanese anchors and re-encoding;
- byte-identical output across worker counts and user-code non-execution;
- request/output Draft 2020-12 schema validation; and
- reasonless Unknown and extra-field rejection.
