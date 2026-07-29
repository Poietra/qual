# Third-party licenses

`qual` itself is [MIT](LICENSE) licensed. It links third-party Rust
crates whose licenses are listed here. Regenerate the survey with
`cargo license` or `cargo deny list`.

## Summary of the dependency tree

| License | Crates |
| --- | --- |
| MIT OR Apache-2.0 (and equivalent dual forms) | the large majority, including `clap`, `serde`, `rayon`, `rusqlite`, `toml`, `sha2` |
| Unicode-3.0 | the `icu_*` family, via `idna` |
| Unlicense OR MIT | `aho-corasick`, `globset`, `memchr` |
| **LGPL-3.0-only** | **`malachite`, `malachite-base`, `malachite-bigint`, `malachite-nz`, `malachite-q`** |

## The LGPL-3.0 component matters for binary distribution

`rustpython-parser`, the Python parser this analyzer is built on, depends on
`rustpython-ast`, which uses `malachite-bigint` for Python integer literals.
The `malachite` family is **LGPL-3.0-only**, and Rust links it statically, so
its object code is present in any compiled `qual` binary.

What this means in practice:

- **Source distribution (crates.io, `cargo install qual`, building from
  a checkout) is unaffected.** Each user's toolchain fetches and compiles
  `malachite` itself; nothing LGPL-licensed is redistributed by this project.
  `qual`'s own source stays MIT.
- **Distributing a prebuilt binary triggers LGPL-3.0 §4.** A statically linked
  executable is a "Combined Work". Shipping it obliges the distributor to let
  recipients relink the executable against a modified `malachite` — in
  practice by publishing the exact source and build instructions (a pinned
  `Cargo.lock`, the toolchain version, and the build command), and by stating
  that the work uses `malachite` under the LGPL with a copy of that license.

Prebuilt releases now carry that material in every distribution format:
`LICENSES/LGPL-3.0-only.txt`, the GNU GPL text it incorporates,
`RELINKING.md`, the exact `Cargo.lock`, the application source archive, and a
deterministic `malachite-sources.tar.gz` containing the locked LGPL-covered
sources beside the binary archives. The release metadata check, source-bundle
builder, and installed-wheel test fail closed if this material disappears.
This describes the project's compliance mechanism; it is not legal advice,
and distributors should still review their own obligations.

## Removing the obligation

The LGPL dependency enters only through the Python parser. Alternatives that
would make the whole tree permissive:

- `ruff_python_parser` (MIT) — Ruff's fork of the same parser lineage, with
  the big-integer dependency removed.
- Any parser that represents Python integer literals without `malachite`.

Swapping the parser is a substantial change and is deliberately out of scope
for the 0.2.0 release; it is recorded here so the decision is explicit rather
than accidental.

## Manim Community

The bundled knowledge profiles under `src/knowledge/profiles/` describe the
public API surface of [Manim Community](https://www.manim.community/)
(MIT licensed): class names, base chains, method kinds, and export lists,
extracted statically by `sync_manim_knowledge` from a Manim checkout. They
contain no Manim source code. Manim Community is a separate project and is
not affiliated with, nor an endorser of, `qual`.
