# Installation and first check

Qual is distributed as a native Rust executable. The PyPI package installs
that executable; using it does not require Manim, LaTeX, or a Python runtime.

## Install

=== "uv"

    ```bash
    uv tool install qual-manim
    ```

=== "pipx"

    ```bash
    pipx install qual-manim
    ```

=== "Cargo"

    ```bash
    cargo install qual --locked
    ```

=== "Standalone installer"

    ```bash
    curl --proto '=https' --tlsv1.2 -LsSf \
      https://github.com/Poietra/qual/releases/latest/download/qual-installer.sh | sh
    ```

Checksummed archives for Linux, macOS, and Windows are available from the
[latest GitHub release](https://github.com/Poietra/qual/releases/latest).

## Run your first check

From a Manim project:

```bash
qual check .
```

In a terminal, Qual shows source context, an explanation, and a summary. When
stdout is redirected it uses a stable one-line format. Select a format
explicitly when another tool consumes the result:

```bash
qual check . --format concise
qual check . --format json
qual check . --format sarif
qual check . --format github
```

Exit codes are designed for CI:

| Code | Meaning |
| --- | --- |
| `0` | No reported diagnostic reaches `fail-level` |
| `1` | At least one reported diagnostic reaches `fail-level` |
| `2` | Command-line, configuration, input, or internal error |

## Understand a finding

```text
✖ MLC102 scenes/demo.py:12:19

  `square.shift(...)` returns the mobject itself, not an Animation.
  Use `square.animate.shift(...)` inside `Scene.play()`.
```

The rule ID is stable public vocabulary. Open its full documentation with:

```bash
qual explain MLC102
```

Every finding has two independent dimensions:

- **severity** — `error`, `warning`, or `info`;
- **confidence** — `certain`, `high`, `medium`, or `low`.

The default confidence threshold is conservative. If a value cannot be
resolved, Qual records `Unknown` and avoids upgrading the uncertainty into a
high-confidence error.

## Next steps

- Set renderer, FPS, resolution, and policy in [configuration](guides/configuration.md).
- Introduce Qual gradually with [baselines and suppressions](guides/adoption.md).
- See all supported checks in the [rule catalog](rules/README.md).
