#!/usr/bin/env python3
"""Prepare a reviewable release commit from the Unreleased changelog."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import subprocess
import sys
from pathlib import Path

from check_release import ROOT, SEMVER


def abort(message: str) -> None:
    print(f"prepare release: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version", help="new SemVer version without a v prefix")
    parser.add_argument(
        "--date",
        default=dt.date.today().isoformat(),
        help="release date in YYYY-MM-DD form (default: today)",
    )
    args = parser.parse_args()

    if not SEMVER.fullmatch(args.version):
        abort(f"invalid SemVer version: {args.version!r}")
    try:
        dt.date.fromisoformat(args.date)
    except ValueError as error:
        abort(str(error))

    status = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    if status:
        abort("working tree must be clean before preparing a release")

    cargo_path = ROOT / "Cargo.toml"
    cargo = cargo_path.read_text(encoding="utf-8")
    updated_cargo, count = re.subn(
        r'(?m)^(\[package\]\nname = "qual"\nversion = ")[^"]+("\n)',
        rf"\g<1>{args.version}\g<2>",
        cargo,
        count=1,
    )
    if count != 1:
        abort("could not locate the package version in Cargo.toml")

    changelog_path = ROOT / "CHANGELOG.md"
    changelog = changelog_path.read_text(encoding="utf-8")
    if f"## [{args.version}]" in changelog:
        abort(f"CHANGELOG.md already contains [{args.version}]")
    marker = "## [Unreleased]\n"
    if changelog.count(marker) != 1:
        abort("CHANGELOG.md must contain exactly one Unreleased heading")
    updated_changelog = changelog.replace(
        marker,
        f"{marker}\n## [{args.version}] - {args.date}\n",
        1,
    )

    cargo_path.write_text(updated_cargo, encoding="utf-8")
    changelog_path.write_text(updated_changelog, encoding="utf-8")
    subprocess.run(["cargo", "check"], cwd=ROOT, check=True)
    subprocess.run(
        [sys.executable, str(ROOT / "scripts/check_release.py")],
        cwd=ROOT,
        check=True,
    )
    print("prepare release: review Cargo.toml, Cargo.lock, and CHANGELOG.md")


if __name__ == "__main__":
    main()
