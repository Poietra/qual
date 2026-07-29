#!/usr/bin/env python3
"""Build a deterministic archive of the locked LGPL-covered crate sources."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import subprocess
import tarfile
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LGPL_PACKAGES = (
    "malachite",
    "malachite-base",
    "malachite-bigint",
    "malachite-nz",
    "malachite-q",
)


def normalized(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    info.mtime = 0
    info.pax_headers = {}
    return info


def add_bytes(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    archive.addfile(normalized(info), io.BytesIO(data))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "target/release-assets/malachite-sources.tar.gz",
    )
    args = parser.parse_args()

    with (ROOT / "Cargo.lock").open("rb") as stream:
        lock = tomllib.load(stream)
    locked = {
        package["name"]: package
        for package in lock["package"]
        if package["name"] in LGPL_PACKAGES
    }
    missing = sorted(set(LGPL_PACKAGES) - set(locked))
    if missing:
        raise SystemExit(f"locked LGPL packages missing: {', '.join(missing)}")

    raw_metadata = subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        text=True,
    )
    metadata = json.loads(raw_metadata)
    crate_archives: dict[str, Path] = {}
    for package in metadata["packages"]:
        name = package["name"]
        if name not in locked:
            continue
        if package["version"] != locked[name]["version"]:
            raise SystemExit(f"metadata version drift for {name}")
        if not str(package.get("source", "")).startswith("registry+"):
            raise SystemExit(f"{name} is no longer a registry source")
        source_dir = Path(package["manifest_path"]).parent
        registry_dir = source_dir.parents[2]
        archive = (
            registry_dir
            / "cache"
            / source_dir.parent.name
            / f"{name}-{package['version']}.crate"
        )
        if not archive.is_file():
            raise SystemExit(f"registry archive missing for {name}: {archive}")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != locked[name].get("checksum"):
            raise SystemExit(f"registry archive checksum mismatch for {name}")
        crate_archives[name] = archive
    missing_sources = sorted(set(LGPL_PACKAGES) - set(crate_archives))
    if missing_sources:
        raise SystemExit(f"LGPL source directories missing: {', '.join(missing_sources)}")

    manifest_lines = [
        "Locked LGPL-3.0-only sources for manim-lint",
        "Generated from Cargo.lock; each directory is an unmodified registry source.",
        "",
    ]
    for name in LGPL_PACKAGES:
        package = locked[name]
        manifest_lines.append(
            f"{name} {package['version']} {package.get('checksum', 'no-checksum')}"
        )
    manifest = ("\n".join(manifest_lines) + "\n").encode()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                add_bytes(archive, "malachite-sources/MANIFEST.txt", manifest)
                for relative in (
                    "LICENSES/GPL-3.0-only.txt",
                    "LICENSES/LGPL-3.0-only.txt",
                    "RELINKING.md",
                ):
                    add_bytes(
                        archive,
                        f"malachite-sources/{relative}",
                        (ROOT / relative).read_bytes(),
                    )
                for name in LGPL_PACKAGES:
                    version = locked[name]["version"]
                    add_bytes(
                        archive,
                        f"malachite-sources/crates/{name}-{version}.crate",
                        crate_archives[name].read_bytes(),
                    )
    print(args.output)


if __name__ == "__main__":
    main()
