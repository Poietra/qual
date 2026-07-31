# Qual

**The Manim-aware linter. Catch bad renders before they happen.**

[![Release](https://img.shields.io/github/v/release/Poietra/qual)](https://github.com/Poietra/qual/releases/latest)
[![PyPI](https://img.shields.io/pypi/v/qual-manim)](https://pypi.org/project/qual-manim/)
[![crates.io](https://img.shields.io/crates/v/qual)](https://crates.io/crates/qual)
[![CI](https://github.com/Poietra/qual/actions/workflows/ci.yml/badge.svg)](https://github.com/Poietra/qual/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/Poietra/qual)](LICENSE)

[Documentation](https://poietra.github.io/qual/) ·
[Rule catalog](https://poietra.github.io/qual/rules/) ·
[日本語](README.ja.md)

Think Ruff for Manim scenes, with an understanding of `Scene.play`, mobject
lifecycles, updaters, Cairo/OpenGL behavior, and per-frame render cost. Qual
finds render-time crashes, silent visual bugs, and performance traps without
importing Manim or running your scene.

```bash
uv tool install qual-manim
qual check .
```

> Ruff and Pyright check the Python. Qual checks what Manim will do with it.

## A general Python linter cannot see this

```python
from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        tracker = ValueTracker(0)
        label = always_redraw(
            lambda: MathTex(f"x={tracker.get_value():.2f}")
        )
        self.add(square, label)
        self.play(square.shift(RIGHT))
        self.play(tracker.animate.set_value(8), run_time=8)
```

```console
$ qual check . --format concise
scene.py:9:21: MLP226 warning Each invocation constructs a `MathTex` and performs a cache-key lookup, and this f-string key varies per frame: every rendered frame can mint a distinct Text/TeX cache key and disk asset (`K_resource ≈ F`). Across the 1 play(s) where this callback provably executes it may create at least ~480 distinct keys.
scene.py:12:19: MLC102 error `square.shift(...)` mutates the mobject immediately and returns the mobject itself, not an Animation; use `.animate` (e.g. `square.animate.shift(...)`) inside `Scene.play()`.
```

`MLC102` aborts a render. `MLP226` can launch TeX work and create a new cached
asset on every frame. Qual finds both because it follows Manim's
lifecycle and distinguishes code that runs once from code that runs per frame.
It reports a number only when source and render-profile evidence prove it.

## What Qual checks

Qual 0.3 ships **92 implemented Manim-specific rules** in four families:

| Family | Finds |
| --- | --- |
| **MLC — lifecycle and correctness** | Invalid animations, missing targets/state, updater mistakes, conflicting writes, and Scene-membership errors |
| **MLR — rendering** | Silent visual bugs in TeX, assets, geometry, ordering, cameras, and Cairo/OpenGL compatibility |
| **MLP — performance** | Per-frame construction, growing scene graphs, expensive callbacks, raster multipliers, and resource-key growth |
| **MLD — determinism and portability** | FPS-dependent motion, unseeded randomness, platform paths, fonts, and external state in frame callbacks |

Every finding separates **severity** (`error`, `warning`, `info`) from
**confidence** (`certain`, `high`, `medium`, `low`). State-dependent rules
fire only with sufficient evidence. Unknown behavior stays `Unknown`; Qual
does not turn uncertainty into a high-confidence guess.

[Browse all rules and their exact scope →](https://poietra.github.io/qual/rules/)

## Install

The PyPI package installs the native Rust executable. Qual does not require
Manim, LaTeX, or a Python runtime after installation.

```bash
# Python tooling
uv tool install qual-manim
# or
pipx install qual-manim

# Rust tooling (builds from source; Rust 1.85+)
cargo install qual --locked
```

Standalone installers and checksummed archives for Linux, macOS, and Windows
are attached to each [GitHub release](https://github.com/Poietra/qual/releases/latest).

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Poietra/qual/releases/latest/download/qual-installer.sh | sh
```

## Use it locally and in CI

```bash
qual check .                         # rich terminal output
qual check . --format concise        # one stable line per finding
qual check . --format github         # GitHub Actions annotations
qual check . --format sarif          # SARIF 2.1.0
qual check . --fix                    # safe fixes only
qual cost scenes/demo.py             # symbolic per-Scene render cost
qual coverage .                      # unresolved analysis frontiers
qual explain MLC102                  # full documentation for one rule
```

Exit codes are CI-friendly: `0` means the failure threshold was not reached,
`1` means it was reached, and `2` means an input, configuration, or internal
error.

Existing projects can record current findings and fail only on new ones:

```bash
qual check . --write-baseline .qual-baseline.json
qual check . --baseline .qual-baseline.json
```

Configuration lives in `[tool.qual]` in `pyproject.toml`. Named profiles model
the renderer, platform, resolution, and FPS used by preview and production
renders. See the [configuration guide](https://poietra.github.io/qual/guides/configuration/)
for selectors, suppressions, profiles, and `manim.cfg` precedence.

## Works alongside Ruff and Pyright

Qual deliberately does not reimplement general Python linting or type
checking:

```bash
ruff check .
pyright
qual check .
```

- Ruff handles style, imports, and general Python lint rules.
- Pyright handles Python types.
- Qual handles Manim lifecycle, rendering semantics, and render cost.

Qual is a standalone analyzer because Manim bugs often span statements,
helpers, Scene membership changes, animation setup/cleanup, and render
profiles. It is not a Ruff plugin and does not imply affiliation with Ruff.

## Documentation and machine APIs

The [documentation site](https://poietra.github.io/qual/) provides searchable
guides and references for:

- installation, configuration, CI, baselines, suppressions, and fixes;
- all 92 rules, including evidence and near-miss behavior;
- `qual cost` and `qual coverage`;
- architecture, research evidence, and contributor guidance;
- versioned machine interfaces and JSON schemas.

Qual's public integration surface is CLI plus versioned JSON—not a network
service or public Rust library:

```bash
qual check . --format json
qual static-facts .
qual change-impact --before old-tree --after new-tree
qual source-bridge . --request request.json
```

Start with the [machine API overview](https://poietra.github.io/qual/reference/machine-api/)
and validate output against the checked-in schemas.

## Safety and scope

- Qual never imports or executes Manim, plugins, or analyzed user code.
- The current knowledge profiles target **Manim Community 0.20**.
- Asset checks inspect the linting machine's filesystem using Manim's modeled
  search order; environment-dependent evidence is marked as such.
- Dynamic Python is handled conservatively. Use `qual coverage` to inspect
  what could not be resolved.
- Python style and general type errors remain the job of Ruff and Pyright.

## Contributing

[`docs/architecture.md`](docs/architecture.md) is the map: the pipeline, the
Manim semantic model the rules rest on, and the invariants every change must
keep. [`docs/rules/`](docs/rules/README.md) is the rule catalog and
[`docs/reference/cli.md`](docs/reference/cli.md) the CLI and JSON contracts.
[`CONTRIBUTING.md`](CONTRIBUTING.md) explains the repository layout, adding a
rule, test gates, and knowledge-profile updates.

[`DESIGN.md`](DESIGN.md) is a historical design record written in Japanese
before the implementation existed. It is not a specification and is not kept
in sync with the code.

```bash
cargo fmt --check
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

## License

Qual is distributed under the [MIT License](LICENSE). Binary distributions
also contain relinkable LGPL-covered dependencies; see
[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md) and
[`RELINKING.md`](RELINKING.md).

Qual is an independent project. Manim Community and Ruff are not affiliated
with or responsible for it.
