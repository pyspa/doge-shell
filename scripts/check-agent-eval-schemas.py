#!/usr/bin/env python3
"""Validate curated case JSON and optional run/review results without dependencies."""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCHEMAS = ROOT / "docs/ai/evals/schemas"


def validate(value, schema, where="$", errors=None):
    errors = [] if errors is None else errors
    types = schema.get("type", [])
    if isinstance(types, str):
        types = [types]
    predicates = {
        "object": lambda v: isinstance(v, dict),
        "array": lambda v: isinstance(v, list),
        "string": lambda v: isinstance(v, str),
        "integer": lambda v: isinstance(v, int) and not isinstance(v, bool),
        "boolean": lambda v: isinstance(v, bool),
        "null": lambda v: v is None,
    }
    if types and not any(predicates[t](value) for t in types):
        errors.append(f"{where}: expected {types}")
        return errors
    if "const" in schema and value != schema["const"]:
        errors.append(f"{where}: unexpected constant")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{where}: invalid enum value")
    if isinstance(value, str):
        if "pattern" in schema and not re.fullmatch(schema["pattern"], value):
            errors.append(f"{where}: invalid pattern")
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{where}: too short")
    if isinstance(value, int) and not isinstance(value, bool) and value < schema.get("minimum", value):
        errors.append(f"{where}: below minimum")
    if isinstance(value, dict):
        for key in schema.get("required", []):
            if key not in value:
                errors.append(f"{where}: missing {key}")
        properties = schema.get("properties", {})
        for key, child in value.items():
            if key in properties:
                validate(child, properties[key], f"{where}.{key}", errors)
            elif schema.get("additionalProperties") is False:
                errors.append(f"{where}: unexpected {key}")
    if isinstance(value, list) and "items" in schema:
        for i, child in enumerate(value):
            validate(child, schema["items"], f"{where}[{i}]", errors)
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--result", type=Path, action="append", default=[])
    parser.add_argument("--review-result", type=Path, action="append", default=[])
    args = parser.parse_args()
    schemas = {path.stem: json.loads(path.read_text()) for path in SCHEMAS.glob("*.schema.json")}
    errors = []
    cases = list((ROOT / "docs/ai/evals/tasks").glob("*.json"))
    for path in cases:
        case = json.loads(path.read_text())
        errors += [f"{path}: {error}" for error in validate(case, schemas["task-case.schema"])]
        if case.get("id") != path.stem:
            errors.append(f"{path}: id and filename differ")
    for name in ("run-result.schema", "review-result.schema"):
        if not isinstance(schemas[name].get("required"), list):
            errors.append(f"{name}: missing required list")
    for kind, paths in (("run-result.schema", args.result), ("review-result.schema", args.review_result)):
        for path in paths:
            value = json.loads(path.read_text())
            errors += [f"{path}: {error}" for error in validate(value, schemas[kind])]
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"agent eval schemas: {len(cases)} cases valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
