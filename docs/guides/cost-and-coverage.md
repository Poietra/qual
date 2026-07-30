# Cost and coverage

Diagnostics answer “what should I inspect?” The cost and coverage commands
answer two related questions: “why might this render be expensive?” and “what
could the analyzer not prove?”

## Symbolic render cost

```bash
qual cost scenes/demo.py
```

The report groups evidence by Scene:

```text
profiles: production (cairo, 1920x1080, 60 fps)

scene scenes.demo.TrackerDemo (scenes/demo.py)
  plays:
    scenes/demo.py:13:9 play duration 8 s -> frames ~480
  hot contexts:
    scenes/demo.py:9:31 entry always_redraw; factors frames
  per-frame constructions:
    scenes/demo.py:9:39 MathTex construction x at least ~480 invocations
  resource-key growth:
    scenes/demo.py:9:39 MathTex distinct cache keys: at least ~480
```

Qual tracks dimensions such as frames, family members, points, curves,
pixels, and distinct resource keys. It does not turn unknown values into a
fabricated wall-clock estimate. An unresolved duration remains `per-frame` or
`unknown`.

## Analysis coverage

A clean lint run can mean either “no findings” or “some relevant behavior was
not statically resolvable.” Inspect that boundary explicitly:

```bash
qual coverage .
qual coverage . --format json
qual check . --analysis-summary
```

Coverage reports include parse success, unresolved imports and calls, unknown
play durations, unknown animation targets, and helper-summary fallbacks. The
counts describe computed analysis facts, not statistical estimates.

## Cache behavior

Normal checks keep a disposable SQLite cache in:

```text
.qual-cache/cache-v2.sqlite3
```

Add `.qual-cache/` to the analyzed project's ignore file. The cache is never
required for correctness; corruption or I/O failure falls back to analysis.
Use `--no-cache` to disable all cache reads, writes, and directory creation.
