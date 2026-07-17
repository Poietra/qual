# manim-lint

`manim-lint` is a static analyzer for Manim Community projects. It reads Python
source without importing Manim or executing the analyzed project.

The current implementation is the Phase 0 foundation described in
[`DESIGN.md`](DESIGN.md): source loading, configuration, stable diagnostics,
concise/full text output, JSON v1 output, and syntax-error recovery across
multiple files.

## Install and run

Python 3.11 or newer is required.

```bash
python -m pip install -e .
manim-lint check .
manim-lint check scenes --format json
```

The command can also be run directly from a checkout:

```bash
PYTHONPATH=src python -m manim_lint check .
```

Exit status is `0` when no diagnostic reaches `fail-level`, `1` when one does,
and `2` for command-line, configuration, or internal errors. A syntax error in
one file is reported as `MLC000`; other files are still parsed.

Configuration is read from `[tool.manim-lint]` in `pyproject.toml`. Resolution
and output-profile settings follow the precedence documented in `DESIGN.md`:
CLI overrides the selected profile, then project base settings, then
`manim.cfg`, then built-in defaults.

