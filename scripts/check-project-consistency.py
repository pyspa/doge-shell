#!/usr/bin/env python3
"""Check workspace metadata and public project-policy declarations."""

from __future__ import annotations

import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Optional


REPO_ROOT = Path(__file__).resolve().parent.parent
ROOT_MANIFEST = REPO_ROOT / "Cargo.toml"
README = REPO_ROOT / "README.md"
LICENSE = REPO_ROOT / "LICENSE"


def fail(message: str, failures: list[str]) -> None:
    failures.append(message)


def workspace_package_value(manifest: str, key: str) -> Optional[str]:
    section = re.search(
        r"^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|\Z)",
        manifest,
        flags=re.MULTILINE,
    )
    if section is None:
        return None
    value = re.search(
        rf'^\s*{re.escape(key)}\s*=\s*"([^"]+)"\s*$',
        section.group(1),
        flags=re.MULTILINE,
    )
    return value.group(1) if value else None


def main() -> int:
    failures: list[str] = []
    root_manifest = ROOT_MANIFEST.read_text()
    expected_license = workspace_package_value(root_manifest, "license")
    expected_msrv = workspace_package_value(root_manifest, "rust-version")

    if expected_license != "MIT":
        fail(f"workspace.package.license must be MIT, found {expected_license!r}", failures)
    if not isinstance(expected_msrv, str) or not expected_msrv:
        fail("workspace.package.rust-version must be a non-empty string", failures)

    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail(f"cargo metadata failed: {result.stderr.strip()}", failures)
        metadata = {"packages": [], "workspace_members": []}
    else:
        metadata = json.loads(result.stdout)

    members = set(metadata.get("workspace_members", []))
    packages = [package for package in metadata.get("packages", []) if package["id"] in members]
    if not packages:
        fail("cargo metadata returned no workspace packages", failures)

    for package in packages:
        name = package["name"]
        if package.get("license") != expected_license:
            fail(
                f"{name}: license must inherit {expected_license!r}, found {package.get('license')!r}",
                failures,
            )
        if package.get("rust_version") != expected_msrv:
            fail(
                f"{name}: rust-version must inherit {expected_msrv!r}, found {package.get('rust_version')!r}",
                failures,
            )

        manifest = Path(package["manifest_path"]).read_text()
        if not re.search(r"^license\.workspace\s*=\s*true\s*$", manifest, re.MULTILINE):
            fail(f"{name}: Cargo.toml must use license.workspace = true", failures)
        if not re.search(
            r"^rust-version\.workspace\s*=\s*true\s*$", manifest, re.MULTILINE
        ):
            fail(f"{name}: Cargo.toml must use rust-version.workspace = true", failures)

    readme = README.read_text()
    if "licensed under the MIT license" not in readme:
        fail("README must declare the MIT license", failures)
    if "Apache-2.0" in readme:
        fail("README must not advertise Apache-2.0", failures)
    if "[LICENSE](LICENSE)" not in readme:
        fail("README must link to LICENSE", failures)
    if not LICENSE.is_file() or "MIT License" not in LICENSE.read_text():
        fail("LICENSE must contain the MIT License text", failures)

    if failures:
        for message in failures:
            print(f"error: {message}", file=sys.stderr)
        print(f"project consistency check failed: {len(failures)} issue(s)", file=sys.stderr)
        return 1

    print(
        f"ok project consistency: packages={len(packages)} license={expected_license} msrv={expected_msrv}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
