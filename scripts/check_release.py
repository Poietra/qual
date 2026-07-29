#!/usr/bin/env python3
"""Fail closed when release metadata or compliance material drifts."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(
    r"^(?P<major>0|[1-9]\d*)\."
    r"(?P<minor>0|[1-9]\d*)\."
    r"(?P<patch>0|[1-9]\d*)"
    r"(?:-(?:0|[1-9A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9A-Za-z-][0-9A-Za-z-]*))*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
COMPLIANCE_FILES = (
    "LICENSE",
    "LICENSES/GPL-3.0-only.txt",
    "LICENSES/LGPL-3.0-only.txt",
    "RELINKING.md",
    "THIRD-PARTY-LICENSES.md",
)
REQUIRED_RELEASE_FILES = COMPLIANCE_FILES + (
    "scripts/build_lgpl_source_bundle.py",
)
MALACHITE_PACKAGES = {
    "malachite",
    "malachite-base",
    "malachite-bigint",
    "malachite-nz",
    "malachite-q",
}


def load_toml(path: str) -> dict[str, object]:
    with (ROOT / path).open("rb") as stream:
        return tomllib.load(stream)


def fail(message: str) -> None:
    print(f"release check: {message}", file=sys.stderr)
    raise SystemExit(1)


def normalized_tag(tag: str) -> str:
    return tag[1:] if tag.startswith("v") else tag


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--tag",
        help="release tag (vX.Y.Z or X.Y.Z); must match Cargo.toml",
    )
    args = parser.parse_args()

    cargo = load_toml("Cargo.toml")
    package = cargo.get("package")
    if not isinstance(package, dict):
        fail("Cargo.toml has no [package] table")
    version = package.get("version")
    if not isinstance(version, str) or not SEMVER.fullmatch(version):
        fail(f"Cargo.toml package version is not SemVer: {version!r}")
    if args.tag and normalized_tag(args.tag) != version:
        fail(f"tag {args.tag!r} does not match Cargo.toml version {version!r}")

    lock = load_toml("Cargo.lock")
    packages = lock.get("package")
    if not isinstance(packages, list):
        fail("Cargo.lock has no package list")
    own_versions = {
        item.get("version")
        for item in packages
        if isinstance(item, dict) and item.get("name") == "manim-lint"
    }
    if own_versions != {version}:
        fail(f"Cargo.lock manim-lint version is {own_versions}, expected {version}")
    locked_names = {
        item.get("name") for item in packages if isinstance(item, dict)
    }
    missing_malachite = sorted(MALACHITE_PACKAGES - locked_names)
    if missing_malachite:
        fail(f"LGPL dependency set changed; missing {', '.join(missing_malachite)}")
    locked_malachite = {
        item["name"]: item["version"]
        for item in packages
        if isinstance(item, dict) and item.get("name") in MALACHITE_PACKAGES
    }
    relinking = (ROOT / "RELINKING.md").read_text(encoding="utf-8")
    stale_relink_paths = sorted(
        name
        for name, package_version in locked_malachite.items()
        if f"malachite-sources/{name}-{package_version}" not in relinking
    )
    if stale_relink_paths:
        fail(
            "RELINKING.md paths do not match Cargo.lock for: "
            + ", ".join(stale_relink_paths)
        )

    pyproject = load_toml("pyproject.toml")
    project = pyproject.get("project")
    if not isinstance(project, dict):
        fail("pyproject.toml has no [project] table")
    if project.get("name") != "manim-lint":
        fail("PyPI project name must remain manim-lint")
    if project.get("dynamic") != ["version"] or "version" in project:
        fail("PyPI version must be dynamic and sourced only from Cargo.toml")
    license_files = set(project.get("license-files", []))
    missing_license_files = sorted(set(COMPLIANCE_FILES) - license_files)
    if missing_license_files:
        fail(f"PyPI metadata omits: {', '.join(missing_license_files)}")

    dist_package = load_toml("dist.toml").get("package")
    if not isinstance(dist_package, dict):
        fail("dist.toml has no [package] table")
    if dist_package.get("binaries") != ["manim-lint"]:
        fail("standalone releases must contain only the manim-lint binary")

    dist = load_toml("dist-workspace.toml").get("dist")
    if not isinstance(dist, dict):
        fail("dist-workspace.toml has no [dist] table")
    included = set(dist.get("include", []))
    missing_includes = sorted(
        set(COMPLIANCE_FILES) - {"LICENSE"} - included
    )
    if missing_includes:
        fail(f"standalone archives omit: {', '.join(missing_includes)}")
    installers = set(dist.get("installers", []))
    if "npm" in installers:
        fail("npm is not an active installer")
    publish_jobs = set(dist.get("publish-jobs", []))
    if publish_jobs != {"./publish-crates"}:
        fail("cargo-dist must publish only the crates.io package")
    pypi_workflow = ROOT / ".github/workflows/publish-pypi.yml"
    pypi_text = pypi_workflow.read_text(encoding="utf-8")
    if "workflow_run:" not in pypi_text or "workflows: [\"Release\"]" not in pypi_text:
        fail("PyPI must publish from its top-level post-Release workflow")
    extra_artifacts = dist.get("extra-artifacts", [])
    if not any(
        isinstance(entry, dict)
        and entry.get("artifacts")
        == ["target/release-assets/malachite-sources.tar.gz"]
        for entry in extra_artifacts
    ):
        fail("release no longer publishes the LGPL corresponding-source bundle")

    for relative in REQUIRED_RELEASE_FILES:
        path = ROOT / relative
        if not path.is_file() or path.stat().st_size == 0:
            fail(f"required release file is missing or empty: {relative}")

    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    if not re.search(
        rf"^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}$",
        changelog,
        flags=re.MULTILINE,
    ):
        fail(f"CHANGELOG.md has no dated [{version}] release heading")

    print(f"release check: manim-lint {version} metadata is consistent")


if __name__ == "__main__":
    main()
