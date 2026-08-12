#!/usr/bin/env python3
"""Verify that the ShellProxy compatibility facade stays capability-complete."""

from __future__ import annotations

from collections import Counter
from pathlib import Path
import re
import sys


REPO_ROOT = Path(__file__).resolve().parent.parent
SHELL_PROXY_SOURCE = REPO_ROOT / "dsh-builtin/src/lib.rs"
CAPABILITY_SOURCE = REPO_ROOT / "dsh-builtin/src/shell_capabilities.rs"
CAPABILITY_TRAITS = (
    "ShellExecution",
    "ShellNavigation",
    "ShellEnvironment",
    "ShellScheduling",
    "ShellSessionData",
    "ShellDiagnostics",
    "ShellAiIntegration",
)
MAX_COMPATIBILITY_METHODS = 73
METHOD_PATTERN = re.compile(r"^\s*fn\s+([A-Za-z_][A-Za-z0-9_]*)\b", re.MULTILINE)


def trait_body(source: str, trait_name: str) -> str:
    marker = f"pub trait {trait_name}"
    marker_start = source.find(marker)
    if marker_start < 0:
        raise ValueError(f"missing public trait: {trait_name}")

    body_start = source.find("{", marker_start + len(marker))
    if body_start < 0:
        raise ValueError(f"missing trait body: {trait_name}")

    depth = 0
    for index in range(body_start, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[body_start + 1 : index]

    raise ValueError(f"unterminated trait body: {trait_name}")


def trait_methods(source: str, trait_name: str) -> list[str]:
    return METHOD_PATTERN.findall(trait_body(source, trait_name))


def main() -> int:
    proxy_source = SHELL_PROXY_SOURCE.read_text(encoding="utf-8")
    capability_source = CAPABILITY_SOURCE.read_text(encoding="utf-8")

    proxy_methods = trait_methods(proxy_source, "ShellProxy")
    capability_methods = {
        trait_name: trait_methods(capability_source, trait_name)
        for trait_name in CAPABILITY_TRAITS
    }
    classified = Counter(
        method
        for methods in capability_methods.values()
        for method in methods
    )

    failures: list[str] = []
    duplicate_proxy_methods = sorted(
        method for method, count in Counter(proxy_methods).items() if count > 1
    )
    if duplicate_proxy_methods:
        failures.append(
            "ShellProxy contains duplicate methods: " + ", ".join(duplicate_proxy_methods)
        )

    missing = sorted(set(proxy_methods) - set(classified))
    if missing:
        failures.append(
            "ShellProxy methods missing a capability trait: " + ", ".join(missing)
        )

    unexpected = sorted(set(classified) - set(proxy_methods))
    if unexpected:
        failures.append(
            "capability methods missing from ShellProxy compatibility facade: "
            + ", ".join(unexpected)
        )

    multiply_classified = sorted(
        method for method, count in classified.items() if count > 1
    )
    if multiply_classified:
        failures.append(
            "methods assigned to multiple capability traits: "
            + ", ".join(multiply_classified)
        )

    if len(proxy_methods) > MAX_COMPATIBILITY_METHODS:
        failures.append(
            "ShellProxy grew beyond the compatibility ceiling "
            f"({len(proxy_methods)} > {MAX_COMPATIBILITY_METHODS}); "
            "add the operation to a capability trait instead"
        )

    if failures:
        for failure in failures:
            print(f"error: {failure}", file=sys.stderr)
        return 1

    print(
        "ok ShellProxy capability coverage: "
        f"{len(proxy_methods)} compatibility methods across "
        f"{len(CAPABILITY_TRAITS)} traits"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
