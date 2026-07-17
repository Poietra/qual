# manim-lint

`manim-lint` is a static analyzer for Manim Community projects, implemented in
Rust. It reads Python source with a Rust Python parser and never imports Manim
or executes the analyzed project.

The current implementation is the **Phase 0 foundation** described in
[`DESIGN.md`](DESIGN.md):

- source loading with PEP 263 encoding declarations, newline preservation,
  and UTF-8-byte-column to Unicode-character-column conversion (Japanese
  source is a first-class test case)
- `MLC000` syntax-error diagnostics; a broken file never stops analysis of
  the other files
- configuration from `[tool.manim-lint]` in `pyproject.toml`, a minimal
  `manim.cfg` reader, and the explicit precedence chain
  `CLI > selected profile > pyproject base > manim.cfg > builtin defaults`
- inline suppressions (`# manim-lint: ignore[...]`, standalone comments,
  header-only `file-ignore[...]`); invalid suppressions warn as `MLC001`
- deterministic, byte-stable output: `concise`, `full`, `github`
  annotations, and JSON matching `schemas/diagnostics-v1.json`
- the rule engine contract (`Rule`, `RuleContext`) and the reserved rule
  catalog; every later module exists as a documented, compile-clean stub

No lifecycle, rendering, performance, or portability rule is implemented yet.
`manim-lint rules` lists them all as `reserved`; they are never presented as
checked. SARIF, baselines, autofix application, the cache, and `manim-lint
cost` belong to later phases and report clear "not implemented" errors.

## Build and run

A Rust toolchain (1.85+) is required.

```bash
cargo build --release
cargo run -- check .
cargo run -- check scenes --format json
cargo install --path .   # installs the `manim-lint` binary
```

## Commands

```text
manim-lint check [PATH...]   # analyze Python sources
manim-lint explain RULE      # print rule documentation
manim-lint rules             # list all rule IDs with phase and status
manim-lint config            # print the resolved effective configuration
manim-lint cost PATH         # Phase 3; exits 2 with a clear message
```

`check` options include `--select` / `--ignore`, `--min-confidence`,
`--fail-level`, `--profile`, `--renderer`, `--fps`,
`--resolution WIDTHxHEIGHT`, `--format concise|full|json|sarif|github`, and
`--statistics`. `--baseline` / `--write-baseline` are Phase 5 and exit 2 for
now; `--fix` / `--no-cache` are accepted no-ops because no Phase 0 rule
produces fixes and no cache exists yet.

Exit status is `0` when no reported diagnostic reaches `fail-level`, `1` when
one does, and `2` for command-line, configuration, or internal errors.

## Configuration

Configuration is read from `[tool.manim-lint]` in `pyproject.toml` (found by
walking up from the checked path); render profiles are
`[[tool.manim-lint.profile]]` entries. When `respect-manim-cfg` is enabled
(the default), `manim.cfg` supplies resolution/fps/renderer defaults below
the pyproject settings. Unknown keys, unknown rule selectors, duplicate
profile names, and unknown profile references are configuration errors
(exit 2).

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`DESIGN.md` is the authoritative specification; `AGENTS.md` describes the
implementation order. Rule documentation lives in `docs/rules/`, and
`tests/fixtures/smoke/` is a minimal end-to-end fixture project.
