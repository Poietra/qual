---
title: Qual — the Manim-aware linter
description: Catch render-time errors, silent visual bugs, and per-frame performance traps before you render.
---

<div class="qual-hero" markdown>

# Lint Manim before you render

**Qual is the Manim-aware linter.** It catches render-time errors, silent
visual bugs, and per-frame performance traps without importing Manim or
running your scene.

[Get started](getting-started.md){ .md-button .md-button--primary }
[Browse all 92 rules](rules/README.md){ .md-button }

</div>

```bash
uv tool install qual-manim
qual check .
```

Think Ruff for Manim scenes, with an understanding of `Scene.play`, mobject
lifecycles, updaters, Cairo/OpenGL behavior, and render cost. Ruff and Pyright
still check Python itself; Qual checks what Manim will do with it.

<div class="grid cards" markdown>

-   **Catch crashes early**

    Invalid `Scene.play` arguments, missing targets or saved state, invalid
    callback signatures, and other errors that otherwise surface during a
    render.

-   **Find the wrong picture**

    Lifecycle, renderer, ordering, updater, TeX, asset, and geometry mistakes
    that can finish successfully while producing the wrong result.

-   **Explain render cost**

    Per-frame construction, growing scene graphs, expensive callbacks, and
    resource-key growth with conservative multiplicity evidence.

-   **Stay safe by default**

    Qual never imports or executes Manim, plugins, or analyzed user code. An
    unresolved value becomes `Unknown`, not a high-confidence guess.

</div>

## A general Python linter cannot see this

```python
self.play(square.shift(RIGHT))
#                  ^ MLC102: shift() returns the mobject, not an Animation
```

```python
label = always_redraw(
    lambda: MathTex(f"x={tracker.get_value():.2f}")
)
# MLP226: a frame-varying TeX key may create one disk asset per rendered frame
```

Qual resolves imports and aliases, follows project-local helpers, models Scene
membership and animation cleanup, and distinguishes code that runs once from
code that runs every frame. It reports a number only when the source and
selected render profile prove it.

## Choose your path

- **Scene authors:** start with [installation and your first check](getting-started.md),
  then browse the [rule catalog](rules/README.md).
- **Existing projects:** use [baselines, suppressions, and safe fixes](guides/adoption.md)
  for gradual adoption.
- **Performance work:** use the [cost and coverage reports](guides/cost-and-coverage.md).
- **Tool builders:** use the versioned [machine APIs](reference/machine-api.md)
  and [JSON schemas](reference/schemas.md).
- **Contributors:** read the [architecture](architecture.md) and authoritative
  [design specification](https://github.com/Poietra/qual/blob/main/DESIGN.md).

!!! info "Current support"

    Qual 0.3 targets Manim Community 0.20. It ships 92 implemented rules and
    native binaries for Linux, macOS, and Windows.
