#!/usr/bin/env python3
"""Fail closed when release metadata or compliance material drifts."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_NAME = "qual"
PYPI_PROJECT_NAME = "qual-manim"
REPOSITORY_URL = "https://github.com/Poietra/qual"
DOCUMENTATION_URL = "https://poietra.github.io/qual/"
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
PINNED_ACTION = re.compile(
    r"^\s*-?\s*uses:\s+[\"']?(?!\./)[^\s@\"']+@(?P<ref>[^\s#\"']+)",
    flags=re.MULTILINE,
)
COMMIT_SHA = re.compile(r"[0-9a-f]{40}")


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
    if package.get("name") != PACKAGE_NAME:
        fail(f"Cargo package name must remain {PACKAGE_NAME}")
    if package.get("default-run") != PACKAGE_NAME:
        fail(f"default Cargo binary must remain {PACKAGE_NAME}")
    if package.get("repository") != REPOSITORY_URL:
        fail(f"Cargo repository must remain {REPOSITORY_URL}")
    for field in ("homepage", "documentation"):
        if package.get(field) != DOCUMENTATION_URL:
            fail(f"Cargo {field} must point to {DOCUMENTATION_URL}")
    bins = cargo.get("bin")
    if not isinstance(bins, list) or not any(
        isinstance(binary, dict)
        and binary.get("name") == PACKAGE_NAME
        and binary.get("path") == "src/main.rs"
        for binary in bins
    ):
        fail(f"Cargo.toml must expose src/main.rs as the {PACKAGE_NAME} binary")
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
        if isinstance(item, dict) and item.get("name") == PACKAGE_NAME
    }
    if own_versions != {version}:
        fail(f"Cargo.lock {PACKAGE_NAME} version is {own_versions}, expected {version}")
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
    if project.get("name") != PYPI_PROJECT_NAME:
        fail(f"PyPI project name must remain {PYPI_PROJECT_NAME}")
    urls = project.get("urls")
    if not isinstance(urls, dict):
        fail("pyproject.toml has no [project.urls] table")
    if urls.get("Homepage") != DOCUMENTATION_URL:
        fail(f"PyPI homepage must point to {DOCUMENTATION_URL}")
    if urls.get("Documentation") != DOCUMENTATION_URL:
        fail(f"PyPI documentation must point to {DOCUMENTATION_URL}")
    if urls.get("Repository") != REPOSITORY_URL:
        fail("PyPI repository must point to the Qual repository")
    if urls.get("Changelog") != f"{REPOSITORY_URL}/blob/main/CHANGELOG.md":
        fail("PyPI changelog URL must point to the Qual changelog")
    if project.get("dynamic") != ["version"] or "version" in project:
        fail("PyPI version must be dynamic and sourced only from Cargo.toml")
    license_files = set(project.get("license-files", []))
    missing_license_files = sorted(set(COMPLIANCE_FILES) - license_files)
    if missing_license_files:
        fail(f"PyPI metadata omits: {', '.join(missing_license_files)}")

    dist_package = load_toml("dist.toml").get("package")
    if not isinstance(dist_package, dict):
        fail("dist.toml has no [package] table")
    if dist_package.get("binaries") != [PACKAGE_NAME]:
        fail(f"standalone releases must contain only the {PACKAGE_NAME} binary")

    dist_workspace = load_toml("dist-workspace.toml")
    workspace = dist_workspace.get("workspace")
    if not isinstance(workspace, dict) or workspace.get("packages") != [PACKAGE_NAME]:
        fail(f"cargo-dist workspace must publish only {PACKAGE_NAME}")
    dist = dist_workspace.get("dist")
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
    if dist.get("allow-dirty") != ["ci"]:
        fail("cargo-dist must preserve the security-hardened release workflow")

    workflow_dir = ROOT / ".github/workflows"
    workflow_text = {
        path.name: path.read_text(encoding="utf-8")
        for path in sorted(workflow_dir.glob("*.yml"))
    }
    for name, text in workflow_text.items():
        for match in PINNED_ACTION.finditer(text):
            action_ref = match.group("ref")
            if not COMMIT_SHA.fullmatch(action_ref):
                fail(f"{name} has an action that is not pinned to a commit: {action_ref}")
        if "secrets: inherit" in text:
            fail(f"{name} inherits secrets instead of declaring its requirements")

    release_text = workflow_text.get("release.yml", "")
    required_release_hardening = (
        '\nname: Release\npermissions:\n  "contents": "read"\n',
        "RELEASE_TAG: ${{ inputs.tag }}",
        'dist host --steps=create "--tag=$RELEASE_TAG"',
        'gh release create "$RELEASE_TAG"',
        '    permissions:\n      "attestations": "write"\n      "contents": "write"\n      "id-token": "write"',
    )
    if any(marker not in release_text for marker in required_release_hardening):
        fail("release.yml is missing required permission or input hardening")
    forbidden_release_fragments = (
        "format('host --steps=create --tag={0}', inputs.tag)",
        'gh release create "${{ needs.plan.outputs.tag }}"',
    )
    if any(fragment in release_text for fragment in forbidden_release_fragments):
        fail("release.yml interpolates a release tag directly into shell code")
    global_job_start = release_text.find("\n  build-global-artifacts:\n")
    global_job_end = release_text.find("\n  host:\n", global_job_start)
    if global_job_start == -1 or global_job_end == -1:
        fail("release.yml has no bounded build-global-artifacts job")
    global_job = release_text[global_job_start:global_job_end]
    required_global_python = (
        "actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97",
        'python-version: "3.11"',
    )
    if any(marker not in global_job for marker in required_global_python):
        fail("build-global-artifacts must install pinned Python 3.11 for tomllib")

    gate_text = workflow_text.get("release-gate.yml", "")
    if (
        "RELEASE_TAG: ${{ inputs.tag }}" not in gate_text
        or "github.event.inputs.tag" in gate_text
    ):
        fail("release gate must pass its tag through the environment")
    upstream_drift_command = (
        "cargo test --test knowledge_drift -- --ignored "
        "upstream_profile_matches_clean_base_commit"
    )
    for workflow_name in ("ci.yml", "release-gate.yml"):
        text = workflow_text.get(workflow_name, "")
        if upstream_drift_command not in text:
            fail(
                f"{workflow_name} must select the upstream-only knowledge drift test"
            )
        if re.search(
            r"(?m)^\s*run:\s*cargo test --test knowledge_drift -- --ignored\s*$",
            text,
        ):
            fail(
                f"{workflow_name} runs private-fork drift against the upstream checkout"
            )

    pypi_workflow = ROOT / ".github/workflows/publish-pypi.yml"
    pypi_text = workflow_text.get(pypi_workflow.name, "")
    if "workflow_run:" not in pypi_text or "workflows: [\"Release\"]" not in pypi_text:
        fail("PyPI must publish from its top-level post-Release workflow")
    pypi_guards = (
        "github.event.workflow_run.conclusion == 'success'",
        "github.event.workflow_run.event == 'workflow_dispatch'",
        "github.event.workflow_run.head_branch == github.event.repository.default_branch",
    )
    if any(guard not in pypi_text for guard in pypi_guards):
        fail("PyPI workflow_run is missing a trusted Release-source guard")
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

    print(f"release check: {PACKAGE_NAME} {version} metadata is consistent")


if __name__ == "__main__":
    main()
