# CI, baselines, and fixes

## Suppress one intentional finding

```python
self.play(square.shift(RIGHT))  # qual: ignore[MLC102]

# qual: ignore[MLP201]
label = always_redraw(...)

# qual: file-ignore[MLP]
```

An end-of-line suppression covers its statement. A standalone suppression
covers the next statement. File-wide suppressions must be in the file header.
Unknown rule IDs do not suppress anything and produce `MLC001`.

For directories or generated fixtures, use `per-file-ignores`:

```toml
[tool.qual]
per-file-ignores = { "tests/fixtures/**" = ["MLP", "MLD"] }
```

## Adopt an existing project with a baseline

```bash
qual check . --write-baseline .qual-baseline.json
qual check . --baseline .qual-baseline.json
```

The second command reports only new findings. Baseline fingerprints use the
rule, relative path, qualified Scene, and surrounding token structure rather
than a line number, so unrelated edits do not invalidate the whole file.

## Apply fixes

```bash
qual check . --fix
qual check . --fix --unsafe-fixes
```

`--fix` applies only behavior-preserving edits. `--unsafe-fixes` additionally
allows suggestions that can change runtime semantics. Every changed file is
parsed again; a failing edit is rolled back.

## GitHub Actions

Install the released binary and emit annotations on the pull request diff:

```yaml
name: qual

on:
  pull_request:
  push:

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: pipx install qual-manim
      - run: qual check . --format github
```

For GitHub code scanning, upload SARIF:

```yaml
      - run: qual check . --format sarif > qual.sarif
        continue-on-error: true
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: qual.sarif
```

## Admission checks for render services

Because analysis never imports or executes submitted code, a rendering
service can run Qual before allocating render capacity. Start by observing
findings against real outcomes; if blocking is appropriate, use the strictest
confidence threshold:

```bash
qual check "$SCENE_DIR" --format json \
  --min-confidence certain --fail-level error
```

Exit code `2` indicates an analysis/configuration failure and must not be
treated as a rejected scene.
