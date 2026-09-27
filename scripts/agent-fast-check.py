#!/usr/bin/env python3
"""Small deterministic checks after a changed path; suitable for any agent hook."""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def validate_schema(value, rule: dict, schema: dict, where: str = "$", depth: int = 0) -> list[str]:
    """Validate the constructs used by command-completion-schema.json."""
    if depth > 64:
        return [f"{where}: schema nesting limit exceeded"]
    if "$ref" in rule:
        ref = rule["$ref"]
        if not ref.startswith("#/definitions/"):
            return [f"{where}: unsupported schema reference {ref}"]
        return validate_schema(value, schema["definitions"][ref.removeprefix("#/definitions/")], schema, where, depth + 1)
    if "oneOf" in rule:
        matches = sum(not validate_schema(value, branch, schema, where, depth + 1) for branch in rule["oneOf"])
        return [] if matches == 1 else [f"{where}: expected exactly one schema variant, found {matches}"]
    if "anyOf" in rule and not any(not validate_schema(value, branch, schema, where, depth + 1) for branch in rule["anyOf"]):
        return [f"{where}: expected at least one schema variant"]
    expected = rule.get("type")
    type_checks = {"object": lambda x: isinstance(x, dict), "array": lambda x: isinstance(x, list),
                   "string": lambda x: isinstance(x, str), "boolean": lambda x: isinstance(x, bool),
                   "integer": lambda x: isinstance(x, int) and not isinstance(x, bool),
                   "null": lambda x: x is None}
    expected_types = [expected] if isinstance(expected, str) else expected or []
    if expected_types and not any(type_checks[name](value) for name in expected_types):
        return [f"{where}: expected {expected}"]
    errors = []
    if "enum" in rule and value not in rule["enum"]:
        errors.append(f"{where}: value is outside enum")
    if isinstance(value, str):
        if len(value) < rule.get("minLength", 0):
            errors.append(f"{where}: string is too short")
        if "pattern" in rule and not re.search(rule["pattern"], value):
            errors.append(f"{where}: string does not match pattern")
    if isinstance(value, dict):
        for name in rule.get("required", []):
            if name not in value:
                errors.append(f"{where}: missing {name}")
        properties = rule.get("properties", {})
        for name, child in value.items():
            if name in properties:
                errors.extend(validate_schema(child, properties[name], schema, f"{where}.{name}", depth + 1))
            elif rule.get("additionalProperties") is False:
                errors.append(f"{where}: unknown property {name}")
    if isinstance(value, list) and "items" in rule:
        for index, child in enumerate(value):
            errors.extend(validate_schema(child, rule["items"], schema, f"{where}[{index}]", depth + 1))
    return errors


def changed_paths() -> list[str]:
    tracked = subprocess.run(["git", "diff", "--name-only", "-z", "HEAD"], cwd=ROOT, capture_output=True, check=True).stdout
    untracked = subprocess.run(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return [entry.decode() for entry in (tracked + untracked).split(b"\0") if entry]


def completion_errors(path: Path) -> list[str]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        schema = json.loads((ROOT / "command-completion-schema.json").read_text(encoding="utf-8"))
    except (ValueError, OSError) as exc:
        return [f"{path}: {exc}"]
    errors = validate_schema(data, schema, schema, str(path))
    if not isinstance(data, dict) or data.get("command") != path.stem:
        errors.append(f"{path}: command must match filename")
    dynamic = next(item for item in schema["definitions"]["ArgumentType"]["oneOf"] if item.get("title") == "Dynamic Type")
    allowed = set(dynamic["properties"]["data"]["properties"]["provider"]["enum"])

    def walk(value):
        if isinstance(value, list):
            for item in value:
                walk(item)
        elif isinstance(value, dict):
            kind = value.get("type")
            if kind == "Script":
                errors.append(f"{path}: Script completion is forbidden")
            if kind == "Choice" and (not isinstance(value.get("data"), list) or not all(isinstance(x, str) for x in value["data"])):
                errors.append(f"{path}: Choice data must be strings")
            if kind == "Dynamic" and (not isinstance(value.get("data"), dict) or value["data"].get("provider") not in allowed):
                errors.append(f"{path}: unknown Dynamic provider")
            for child in value.values():
                walk(child)

    walk(data)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changed", action="store_true")
    parser.add_argument("--path", action="append", default=[])
    args = parser.parse_args()
    if not args.changed and not args.path:
        parser.error("supply --changed or --path")
    paths = set(args.path + (changed_paths() if args.changed else []))
    relative = set()
    for raw in paths:
        path = Path(raw)
        try:
            rel = path.resolve().relative_to(ROOT) if path.is_absolute() else path
        except ValueError:
            parser.error(f"path outside repository: {raw}")
        relative.add(rel.as_posix())

    checks = set()
    for path in relative:
        if path.endswith(".rs"):
            checks.update(("check-runtime-authority.py", "check-portability.py"))
            if path.startswith(("dsh/src/process/", "dsh/src/proxy/builtin/jobs/", "dsh/src/shell/process_substitution/")) or path in ("dsh/src/shell/job.rs", "dsh/src/shell/job_exit.rs"):
                checks.add("check-execution-authority.py")
            if path.startswith("dsh-builtin/") or "shell_capabilities" in path:
                checks.add("check-shell-proxy-capabilities.py")
        if path.startswith(("docs/ai/", ".agents/", ".claude/", ".github/workflows/")) or path in ("AGENTS.md", "CLAUDE.md") or path.startswith(("scripts/agent_", "scripts/agent-", "scripts/check-agent-", "scripts/check-project-skill-")):
            checks.update(("check-project-skill-surface.py", "check-agent-context-budget.py", "eval-agent-routing.py"))
    errors = []
    if "command-completion-schema.json" in relative:
        relative.update(path.relative_to(ROOT).as_posix() for path in (ROOT / "completions").glob("*.json"))
    for path in sorted(relative):
        if path.startswith("completions/") and path.endswith(".json") and (ROOT / path).is_file():
            errors.extend(completion_errors(ROOT / path))
    for name in sorted(checks):
        result = subprocess.run([sys.executable, str(ROOT / "scripts" / name)], cwd=ROOT)
        if result.returncode:
            errors.append(f"{name} failed")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"ok agent fast check ({len(relative)} paths, {len(checks)} checkers)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
