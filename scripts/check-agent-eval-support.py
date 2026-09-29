#!/usr/bin/env python3
"""Check the agent-eval runner support contract without dependencies.

Support boundary (PER-13): `opencode` and generic `command` are local-only.
The manual workflow `.github/workflows/agent-eval.yml` intentionally remains
Codex/Claude-only. Rationale: the command runner accepts arbitrary argv plus
an allowlisted env, and OpenCode relies on provider model/auth configuration
while dynamically probing CLI capabilities, so workflow credentials and
installs would be less bounded and reproducible than the current pinned
Codex/Claude setup. Keeping them local-only preserves least-privilege
secrets, artifact redaction, and no paid model matrix on ordinary PR CI.

This checker links the machine surfaces so local runner additions cannot
silently imply workflow support:
  - local CLI choices in `scripts/run-agent-eval.py` (--runner),
  - normalized-result schema enum in `docs/ai/evals/schemas/run-result.schema.json`,
  - manual workflow dispatch choice list in `.github/workflows/agent-eval.yml`,
  - permitted workflow credential names (`secrets.*` in that workflow).

Parsing is narrow and fails closed: unrecognized formatting raises an error
instead of silently passing.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

DEFAULT_CLI = ROOT / "scripts" / "run-agent-eval.py"
DEFAULT_SCHEMA = ROOT / "docs" / "ai" / "evals" / "schemas" / "run-result.schema.json"
DEFAULT_WORKFLOW = ROOT / ".github" / "workflows" / "agent-eval.yml"
DEFAULT_README = ROOT / "docs" / "ai" / "evals" / "README.md"

WORKFLOW_RUNNERS = frozenset({"codex", "claude"})
LOCAL_ONLY_RUNNERS = frozenset({"opencode", "command"})
LOCAL_RUNNERS = WORKFLOW_RUNNERS | LOCAL_ONLY_RUNNERS
PERMITTED_SECRETS = frozenset({"OPENAI_API_KEY", "ANTHROPIC_API_KEY"})


def parse_cli_runners(cli_path: Path = DEFAULT_CLI) -> set[str]:
    text = Path(cli_path).read_text(encoding="utf-8")
    match = re.search(r'--runner[^)]*choices=\(\s*([^)]+?)\s*\)', text)
    if match is None:
        match = re.search(r'--runner[^\]]*choices=\[\s*([^\]]+?)\s*\]', text)
    if match is None:
        raise ValueError(f"{cli_path}: unrecognized --runner choices format")
    runners = set()
    for a, b in re.findall(r'"([^"]+)"|\'([^\']+)\'', match.group(1)):
        runners.add(a or b)
    if not runners:
        raise ValueError(f"{cli_path}: no runners parsed from --runner choices")
    return runners


def parse_schema_runners(schema_path: Path = DEFAULT_SCHEMA) -> set[str]:
    data = json.loads(Path(schema_path).read_text(encoding="utf-8"))
    properties = data.get("properties", {})
    runner_schema = None
    normalized = properties.get("normalized", {})
    if isinstance(normalized, dict):
        runner_schema = normalized.get("properties", {}).get("runner", {})
    if not isinstance(runner_schema, dict) or "enum" not in runner_schema:
        runner_schema = properties.get("runner", {})
    enum = runner_schema.get("enum") if isinstance(runner_schema, dict) else None
    if not isinstance(enum, list) or not enum or not all(isinstance(v, str) for v in enum):
        raise ValueError(f"{schema_path}: unrecognized runner enum format")
    return set(enum)


def _block_key(lines: list[str], start: int, parent_indent: int, name: str) -> tuple[int, int] | None:
    for j in range(start, len(lines)):
        stripped = lines[j].strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(lines[j]) - len(lines[j].lstrip())
        if indent <= parent_indent:
            return None
        if re.match(rf'{name}:\s*(#.*)?$', stripped):
            return j, indent
    return None


def parse_workflow_runners(workflow_path: Path = DEFAULT_WORKFLOW) -> set[str]:
    """Read on.workflow_dispatch.inputs.runner.options only.

    The lookup follows the actual YAML nesting
    `on:` -> `workflow_dispatch:` -> `inputs:` -> `runner:` -> `options:`,
    so `workflow_call.inputs` or any other earlier mapping is never selected.
    Other choice inputs (e.g. `case`) are ignored regardless of order.
    Both the current inline list and a block-style list are accepted.
    """
    text = Path(workflow_path).read_text(encoding="utf-8")
    lines = text.splitlines()
    on = _block_key(lines, 0, -1, "on")
    if on is None:
        raise ValueError(f"{workflow_path}: missing on stanza")
    dispatch = _block_key(lines, on[0] + 1, on[1], "workflow_dispatch")
    if dispatch is None:
        raise ValueError(f"{workflow_path}: missing on.workflow_dispatch stanza")
    inputs = _block_key(lines, dispatch[0] + 1, dispatch[1], "inputs")
    if inputs is None:
        raise ValueError(f"{workflow_path}: missing on.workflow_dispatch.inputs stanza")
    inputs_idx, inputs_indent = inputs
    runner = _block_key(lines, inputs_idx + 1, inputs_indent, "runner")
    if runner is None:
        raise ValueError(f"{workflow_path}: missing inputs.runner stanza")
    runner_idx, runner_indent = runner
    for k in range(runner_idx + 1, len(lines)):
        line = lines[k]
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if indent <= runner_indent:
            break
        inline = re.match(r'options:\s*\[([^\]]+)\]', stripped)
        if inline is not None:
            runners = {token.strip().strip('"\'') for token in inline.group(1).split(",")}
            runners.discard("")
            if not runners:
                raise ValueError(f"{workflow_path}: empty runner options list")
            return runners
        if re.match(r'options:\s*(#.*)?$', stripped):
            runners = set()
            for follower in lines[k + 1:]:
                follower_stripped = follower.strip()
                if not follower_stripped or follower_stripped.startswith("#"):
                    continue
                follower_indent = len(follower) - len(follower.lstrip())
                item = re.match(r'-\s*(\S+?)\s*(#.*)?$', follower_stripped)
                if item is not None and follower_indent > indent:
                    runners.add(item.group(1).strip('"\''))
                    continue
                break
            if not runners:
                raise ValueError(f"{workflow_path}: empty runner options list")
            return runners
    raise ValueError(f"{workflow_path}: missing or unrecognized inputs.runner options")


_DOT_SECRET_RE = re.compile(r'secrets\s*\.\s*([A-Za-z0-9_]+)', re.IGNORECASE)
_BRACKET_SECRET_RE = re.compile(
    r"secrets\s*\[\s*'([^'\n]+)'\s*\]"
    r"|"
    r'secrets\s*\[\s*"([^"\n]+)"\s*\]',
    re.IGNORECASE,
)
_RESIDUAL_SECRET_RE = re.compile(r'secrets\s*(?:\.|\[)', re.IGNORECASE)
_EXPRESSION_RE = re.compile(r'\$\{\{(.*?)\}\}', re.DOTALL)
_QUOTED_STRING_RE = re.compile(r"'[^'\n]*'|\"[^\"]*\"")
_BARE_SECRETS_RE = re.compile(r'\bsecrets\b', re.IGNORECASE)


def _mask_spans(text: str, spans: list[tuple[int, int]]) -> str:
    masked = list(text)
    for start, end in spans:
        for i in range(start, end):
            if masked[i] != "\n":
                masked[i] = " "
    return "".join(masked)


def parse_workflow_secrets(workflow_path: Path = DEFAULT_WORKFLOW) -> set[str]:
    """Collect literal secret names; fail closed on unclassifiable access.

    Recognized literal forms: `secrets.NAME`, `secrets['NAME']`,
    `secrets["NAME"]` (surrounding whitespace allowed). Matching is
    case-insensitive and names are normalized to uppercase because GitHub
    secret references are case-insensitive (secret names are stored
    uppercase). Any other `secrets.` / `secrets[` access, or any
    `${{ ... secrets ... }}` expression still mentioning `secrets` after
    recognized literals are masked, raises ValueError instead of silently
    passing.
    """
    text = Path(workflow_path).read_text(encoding="utf-8")
    if "workflow_dispatch" not in text:
        raise ValueError(f"{workflow_path}: unrecognized workflow format")
    secrets: set[str] = set()
    spans: list[tuple[int, int]] = []
    for match in _DOT_SECRET_RE.finditer(text):
        secrets.add(match.group(1).upper())
        spans.append(match.span())
    for match in _BRACKET_SECRET_RE.finditer(text):
        secrets.add((match.group(1) if match.group(1) is not None else match.group(2)).upper())
        spans.append(match.span())
    masked = _mask_spans(text, spans)
    residual = _RESIDUAL_SECRET_RE.search(masked)
    if residual is not None:
        raise ValueError(
            f"{workflow_path}: unrecognized or dynamic secrets access: "
            f"{text[max(0, residual.start() - 20):residual.end() + 20]!r}"
        )
    for expr in _EXPRESSION_RE.finditer(masked):
        body = _QUOTED_STRING_RE.sub(" ", expr.group(1))
        if _BARE_SECRETS_RE.search(body):
            raise ValueError(
                f"{workflow_path}: unrecognized or dynamic secrets access in "
                f"expression: {expr.group(0)[:80]!r}"
            )
    return secrets


BOUNDARY_TERMS = ("local-only", "opencode", "command", "Codex/Claude-only")


def check_readme(readme_path: Path = DEFAULT_README) -> list[str]:
    paragraphs = Path(readme_path).read_text(encoding="utf-8").split("\n\n")
    if any(all(term in paragraph for term in BOUNDARY_TERMS) for paragraph in paragraphs):
        return []
    return [f"{readme_path}: missing one support-boundary statement containing "
            f"{', '.join(BOUNDARY_TERMS)}"]


def check(cli_path: Path = DEFAULT_CLI, schema_path: Path = DEFAULT_SCHEMA,
          workflow_path: Path = DEFAULT_WORKFLOW,
          readme_path: Path = DEFAULT_README) -> list[str]:
    errors = []
    try:
        cli_runners = parse_cli_runners(cli_path)
    except ValueError as exc:
        return [str(exc)]
    if cli_runners != set(LOCAL_RUNNERS):
        errors.append(f"{cli_path}: --runner choices {sorted(cli_runners)} != {sorted(LOCAL_RUNNERS)}")
    try:
        schema_runners = parse_schema_runners(schema_path)
    except ValueError as exc:
        return errors + [str(exc)]
    if schema_runners != set(LOCAL_RUNNERS):
        errors.append(f"{schema_path}: runner enum {sorted(schema_runners)} != {sorted(LOCAL_RUNNERS)}")
    try:
        workflow_runners = parse_workflow_runners(workflow_path)
    except ValueError as exc:
        return errors + [str(exc)]
    if workflow_runners != set(WORKFLOW_RUNNERS):
        errors.append(f"{workflow_path}: dispatch options {sorted(workflow_runners)} != {sorted(WORKFLOW_RUNNERS)}")
    if not workflow_runners <= cli_runners:
        errors.append(f"{workflow_path}: workflow runners {sorted(workflow_runners)} not subset of local CLI {sorted(cli_runners)}")
    try:
        secrets = parse_workflow_secrets(workflow_path)
    except ValueError as exc:
        return errors + [str(exc)]
    unapproved = secrets - set(PERMITTED_SECRETS)
    if unapproved:
        errors.append(f"{workflow_path}: unapproved workflow secrets: {sorted(unapproved)}")
    errors += check_readme(readme_path)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    errors = check()
    for error in errors:
        print(error, file=sys.stderr)
    if not errors:
        print(f"agent eval support: local={sorted(LOCAL_RUNNERS)} "
              f"workflow={sorted(WORKFLOW_RUNNERS)} "
              f"local-only={sorted(LOCAL_ONLY_RUNNERS)} "
              f"secrets={sorted(PERMITTED_SECRETS)}")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
