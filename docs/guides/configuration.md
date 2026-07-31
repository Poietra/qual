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

## `manim.cfg`

When `respect-manim-cfg` is true (the default), qual reads the `[CLI]`
section of `manim.cfg` in the project root and takes `pixel_width`,
`pixel_height`, `frame_rate`, `renderer`, and `quality` from it.

`quality` accepts the Manim preset names and their `-q` flags:

| value | flag | resolution | fps |
| --- | --- | --- | --- |
| `low_quality` | `l` | 854x480 | 15 |
| `medium_quality` | `m` | 1280x720 | 30 |
| `high_quality` | `h` | 1920x1080 | 60 |
| `production_quality` | `p` | 2560x1440 | 60 |
| `fourk_quality` | `k` | 3840x2160 | 60 |
| `example_quality` | — | 854x480 | 30 |

Within `manim.cfg`, `quality` **overrides** `pixel_width`, `pixel_height`,
and `frame_rate` set in the same file. This matches Manim: `digest_parser`
(`manim/_config/utils.py`) reads the individual keys first and applies
`quality` last, and the `quality` setter assigns `frame_size` and
`frame_rate` unconditionally. The override is reported in
`manim_cfg_warnings` rather than applied silently.

`quality` only wins inside `manim.cfg`. The outer chain above is unchanged:
a CLI flag or a pyproject profile still outranks it.

A value that is not a preset is a configuration error (exit code 2), because
Manim itself raises `KeyError` for it.

A `[CLI]` key that affects the render profile but that qual does not
interpret — `resolution`, `frame_size`, `from_animation_number`,
`upto_animation_number`, `save_last_frame`, `dry_run`, `transparent`,
`format` — is listed in the `manim_cfg_warnings` field of `qual config` and
printed to stderr during `qual check`. Reporting `respect_manim_cfg: true`
while quietly dropping such a key would be a confident answer derived from a
render profile the project never asked for.

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
