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
LEGACY_CAPABILITY_SOURCE = REPO_ROOT / "dsh-builtin/src/capability.rs"
CAPABILITY_TRAITS = (
    "ShellExecution",
    "ShellNavigation",
    "ShellEnvironment",
    "ShellScheduling",
    "ShellSessionData",
    "ShellDiagnostics",
    "ShellAiIntegration",
)
# capability.rs is legacy (see its module doc): everything it once declared
# that collided with one of the CAPABILITY_TRAITS names above (same method
# name, different trait) was deleted, because a file that `use`d both traits
# would fail to compile with an ambiguous-method error the moment it needed
# one method from each side. These are the traits still living there; check
# that neither reintroduces a name shell_capabilities.rs already claims.
LEGACY_CAPABILITY_TRAITS = (
    "ExecutionCapability",
    "AiCapability",
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

    # Deliberately one-directional: every ShellProxy method must be classified
    # into exactly one capability trait (checked above and below), but a
    # capability trait may carry extra methods that ShellProxy does not have.
    # That is how a capability trait grows past the frozen facade - a new
    # method with a default body compiles fine under the blanket
    # `impl<T: ShellProxy + ?Sized>` and needs no matching ShellProxy method.
    # Requiring the reverse (every capability method must also exist on
    # ShellProxy) would force every such addition through the 73-method
    # ceiling below, which contradicts AGENTS.md's instruction to grow
    # capability traits instead of ShellProxy.

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

    legacy_source = LEGACY_CAPABILITY_SOURCE.read_text(encoding="utf-8")
    legacy_methods = {
        trait_name: set(trait_methods(legacy_source, trait_name))
        for trait_name in LEGACY_CAPABILITY_TRAITS
    }
    capability_method_sets = {
        trait_name: set(methods) for trait_name, methods in capability_methods.items()
    }
    for legacy_trait, methods in legacy_methods.items():
        for capability_trait, other_methods in capability_method_sets.items():
            colliding = sorted(methods & other_methods)
            if colliding:
                failures.append(
                    f"capability.rs::{legacy_trait} and "
                    f"shell_capabilities.rs::{capability_trait} declare the same "
                    f"method name(s) ({', '.join(colliding)}); a file that `use`s "
                    "both traits fails to compile with an ambiguous-method error "
                    "the moment it needs one method from each side - rename one "
                    "side instead of leaving the collision"
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
