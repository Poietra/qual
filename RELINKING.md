# Relinking the LGPL components

Official prebuilt `qual` artifacts contain the `malachite` family of
crates, licensed under LGPL-3.0-only. Rust links those crates statically. This
file explains how a recipient can rebuild the combined executable with a
modified version of those libraries. It is provided to support the relinking
right described by LGPL-3.0 section 4; it is not legal advice.

Every release publishes `source.tar.gz` beside the binary archives on GitHub
Releases. It contains the complete MIT-licensed application source,
`Cargo.lock`, this notice, and the build metadata used by the release. The
same source is available from the Git tag named `v<version>` and from the
crates.io source package. `malachite-sources.tar.gz` on the same release
contains the exact source of every locked LGPL-covered `malachite` component.

To reproduce the normal executable from a release source archive:

```console
$ tar -xzf source.tar.gz
$ cd qual-<version>
$ rustup toolchain install stable
$ cargo build --release --locked
$ ./target/release/qual --version
```

To relink against modified `malachite` sources, unpack
`malachite-sources.tar.gz`, edit or replace the desired package directories,
and unpack each `.crate` file (they are ordinary gzip-compressed tar archives).
Then add path overrides for the five locked packages to the application source
tree's `Cargo.toml`:

```console
$ tar -xzf malachite-sources.tar.gz
$ cd malachite-sources
$ for crate in crates/*.crate; do tar -xzf "$crate"; done
```

After modifying those extracted sources, add:

```toml
[patch.crates-io]
malachite = { path = "/absolute/path/to/malachite-sources/malachite-0.4.22" }
malachite-base = { path = "/absolute/path/to/malachite-sources/malachite-base-0.4.22" }
malachite-bigint = { path = "/absolute/path/to/malachite-sources/malachite-bigint-0.2.3" }
malachite-nz = { path = "/absolute/path/to/malachite-sources/malachite-nz-0.4.22" }
malachite-q = { path = "/absolute/path/to/malachite-sources/malachite-q-0.4.22" }
```

Run `cargo update` so the lockfile records those path overrides, then rebuild:

```console
$ cargo update -p malachite -p malachite-base -p malachite-bigint \
    -p malachite-nz -p malachite-q
$ cargo build --release
```

The resulting `target/release/qual` is a relinked executable containing
the modified libraries. Cargo may require the replacement packages to retain
the locked package versions or compatible dependency relationships. No keys,
signatures, or authorization checks prevent a locally rebuilt executable from
running.

The LGPL-3.0-only text and the GNU GPL text it incorporates are included at
`LICENSES/LGPL-3.0-only.txt` and `LICENSES/GPL-3.0-only.txt`. Exact component
versions are recorded in `Cargo.lock`; notices for the rest of the dependency
tree are in `THIRD-PARTY-LICENSES.md`.
