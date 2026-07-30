# Configuration

Qual searches upward from the checked path for `pyproject.toml`. Base policy
lives under `[tool.qual]`; named render environments use
`[[tool.qual.profile]]`.

```toml
[tool.qual]
manim-version = "0.20"
target-python = "3.11"
select = ["MLC", "MLR", "MLP", "MLD"]
ignore = []
min-confidence = "high"
fail-level = "warning"
default-profile = "production"
knowledge-profile = "upstream_0_20"
respect-manim-cfg = true
exclude = [".venv/**", "media/**"]
per-file-ignores = { "tests/fixtures/**" = ["MLP", "MLD"] }

[[tool.qual.profile]]
name = "production"
renderer = "cairo"
platform = "linux"
pixel-width = 1920
pixel-height = 1080
frame-rate = 60
assets-dir = "."
allowed-fonts = ["Noto Sans", "Noto Sans CJK JP"]
```

## Precedence

Settings resolve in this order, from strongest to weakest:

```text
CLI > selected profile > pyproject base > manim.cfg > builtin defaults
```

`qual config` prints the final values and an `enforcement` section showing
which settings affect analysis and which are informational.

## Profiles

Profiles let CI evaluate the same source under its real render targets:

```toml
default-profile = "preview"

[[tool.qual.profile]]
name = "preview"
renderer = "cairo"
pixel-width = 854
pixel-height = 480
frame-rate = 30

[[tool.qual.profile]]
name = "production"
renderer = "opengl"
pixel-width = 3840
pixel-height = 2160
frame-rate = 60
```

```bash
qual check . --profile production
qual check . --profile all
```

With `--profile all`, Qual merges diagnostics with the same evidence and lists
the profiles to which each one applies.

## Command-line policy

Common overrides:

```bash
qual check . --select MLC,MLR
qual check . --ignore MLP
qual check . --min-confidence certain
qual check . --fail-level error
qual check . --renderer cairo --fps 60 --resolution 1920x1080
```

Unknown keys, selectors, profiles, and invalid numeric values are hard
configuration errors. Qual does not silently ignore policy it cannot enforce.

## Knowledge profiles

`knowledge-profile` selects a versioned static model of Manim semantics. The
normal profile for Manim Community 0.20 is:

```toml
[tool.qual]
knowledge-profile = "upstream_0_20"
```

The local optimized-fork overlay is intentionally separate:

```toml
[tool.qual]
knowledge-profile = "local_0_20_1_4d25c031"
```

Qual never imports the installed Manim package to discover behavior at lint
time. Version-sensitive facts come from the selected reviewed profile.
