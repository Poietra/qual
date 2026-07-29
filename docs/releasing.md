# Releasing Qual

The release flow intentionally follows the shape used by Ruff: one reviewed
version fans out into native artifacts, Python wheels, source packages, and
installer packages. `Cargo.toml` is the only version source. Maturin reads it
for PyPI, and dist reads it for GitHub Releases and crates.io.

## What one release publishes

| Destination | Artifact |
| --- | --- |
| GitHub Releases | checksummed archives for macOS arm64/x64, Linux glibc arm64/x64, Linux musl arm64/x64, and Windows x64 |
| GitHub Releases | shell and PowerShell installers, application/LGPL source archives, manifest, and build attestations |
| PyPI (`qual-manim`) | platform wheels containing the `qual` executable plus a source distribution |
| crates.io | the source crate for `cargo install qual --locked` |

Prerelease versions create a GitHub prerelease but are not sent to package
registries. Registry versions are immutable, so a bad release is yanked or
deprecated and followed by a patch release; it is never overwritten.

## One-time repository and registry setup

1. Create a protected GitHub environment named `release`. Add required
   reviewers if the repository has more than one maintainer. The release gate
   and both trusted publishers target this environment.
2. On PyPI, configure the `qual-manim` trusted publisher for repository
   `Poietra/qual`, workflow `publish-pypi.yml`, environment `release`.
   PyPI pending publishers can create the project on the first publish. This
   top-level workflow follows a successful stable `Release` run and obtains
   the wheels from that exact run; dry runs, PRs, and prereleases are skipped.
3. crates.io trusted publishing needs an existing crate. Bootstrap the first
   source release once with a narrowly scoped owner token and
   `cargo publish --locked`, then configure the trusted publisher for
   `Poietra/qual`, workflow `release.yml`, environment `release`.
   Subsequent workflow runs obtain an ephemeral token through
   `rust-lang/crates-io-auth-action`.
4. Keep the default `GITHUB_TOKEN` permission to create releases and
   attestations. No long-lived PyPI or crates.io token is stored.

Package names are checked before the first release because registry ownership
is first-come, first-served. A PyPI pending publisher does not reserve the name,
and the first crates.io publish is the point at which the crate name is claimed.

Homebrew is deliberately not an active publisher yet: a stable tap repository
does not exist. To add it later, create (for example)
`Poietra/homebrew-tap`, add `homebrew` to `installers` and `publish-jobs`, set
`tap`, regenerate the workflow, and test a dry run. Linux packages for distro
repositories should be maintained in their native repositories rather than
claimed by this workflow.

## Preparing a release PR

Start from a clean `main` checkout after deciding whether the next version is
a SemVer patch or minor release. Public diagnostic/configuration/JSON changes
still follow the stronger contract-update rules in `AGENTS.md` and
`DESIGN.md`.

```bash
git pull --ff-only
python3 scripts/prepare_release.py 0.2.1
```

The script moves the current Unreleased notes under a dated version heading,
updates `Cargo.toml`, lets Cargo update the root package entry in `Cargo.lock`,
and checks all version consumers. Review those three files; edit the changelog
into user-facing release notes, then run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo publish --locked --dry-run
python3 scripts/check_release.py --tag v0.2.1
QUAL_MANIM_ROOT=../manim cargo test --test knowledge_drift -- --ignored
```

The DESIGN §11.4 benchmark gate must also be run on the pinned reference
machine before approving the release PR. The release workflow reruns the
portable quality gates and fetches the public pinned Manim commit for upstream
knowledge drift. Hosted Actions deliberately selects only the upstream drift
test because its checkout does not contain the private `Poietra/fast-manim`
working tree. The command above is therefore the release gate for both the
upstream profile and the local optimized-fork overlay.

Commit the release preparation separately (for example,
`release: prepare 0.2.1`) and merge it through the normal reviewed PR path.

## Dry run and publish

The generated `.github/workflows/release.yml` is dispatch-based, so pushing a
tag cannot publish by accident.

1. In GitHub Actions, select **Release** and run it from the prepared `main`
   commit with the default `dry-run`. This builds every archive and wheel but
   does not create a tag, release, or registry upload.
2. Inspect the workflow artifacts, install at least the native archive and
   wheel for the maintainer's platform, and check `qual --version`.
3. Run the same workflow from the exact prepared commit with tag `v0.2.1`.
4. Approve the `release` environment deployment. The gate checks that the tag,
   Cargo package, lockfile, changelog, PyPI metadata, publisher list, and LGPL
   material agree before builds proceed.
5. Confirm the GitHub release, PyPI, and crates.io pages all show the same
   version. Test `uv tool install qual-manim` and
   `cargo install qual --locked` in clean environments.

crates.io publishes through OIDC after all native builds and the GitHub host
phase succeed. After the `Release` workflow completes successfully, the
top-level `Publish to PyPI` workflow verifies that the host job succeeded and
that the plan is stable, then publishes the wheels through OIDC. A PyPI failure
therefore appears as its own failed workflow and must be resolved before the
release is considered complete.

## Updating dist

`release.yml` is derived from cargo-dist's generated workflow and then kept as
security-reviewed code. `allow-dirty = ["ci"]` prevents `dist generate` from
silently replacing its least-privilege permissions, environment-based input
handling, pinned Actions, and explicit reusable-workflow inputs.

To update dist, make the change in a temporary worktree or temporarily remove
`ci` from `allow-dirty`, update `cargo-dist-version`, and run `dist generate`.
Review the generated diff, reapply the documented security hardening, restore
`allow-dirty = ["ci"]`, and run all of the following before committing:

```bash
actionlint .github/workflows/*.yml
uvx zizmor==1.28.0 .github/workflows
python3 scripts/check_release.py
dist plan
```

`scripts/check_release.py` fails closed if an external Action is not pinned to
a full commit SHA, if reusable jobs inherit all secrets, if the release tag is
interpolated directly into shell code, or if the PyPI `workflow_run` loses its
trusted-source guards.

The custom reusable workflows are intentionally separate from generated code:

- `release-gate.yml` owns product quality and compliance checks;
- `build-wheels.yml` owns maturin's PyPI matrix and installed-wheel smoke test;
- `publish-crates.yml` owns crates.io publishing inside the generated release;
- the top-level `publish-pypi.yml` owns post-Release PyPI trusted publishing.

## Binary-distribution compliance

The current parser brings in statically linked LGPL-3.0-only `malachite`
components. Every binary format therefore includes the LGPL/GPL texts,
`THIRD-PARTY-LICENSES.md`, and `RELINKING.md`; every GitHub release also hosts
the exact application source archive, `Cargo.lock`, and a deterministic archive
of all locked `malachite` sources. `scripts/check_release.py`, the source-bundle
builder, and the wheel smoke test fail if this material drifts or disappears.
See `RELINKING.md` for the rebuild path. This is the repository's compliance
mechanism, not legal advice.
