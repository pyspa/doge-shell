#!/usr/bin/env python3
"""Deterministic pre-edit router: task text/paths -> narrow skills.

No LLM, no embeddings, no network. This module is pure logic (no I/O
except ``load_routes`` reading the manifest); the CLI wrapper lives in
``scripts/agent-context.py``.

Scoring (deterministic):

  - exact path match   +120
  - path prefix match  +100
  - multi-word term     +30
  - single token term   +10

Single generic words (``job``, ``process``, ``shell``, ...) score nothing
on their own, so a vague topic falls back to ``repo-general`` instead of
over-routing. Results sort by score descending, ties by route id lexical
order; the top 2 win, stretching to 3 only on a tie at the boundary.
"""

from __future__ import annotations

import json
from pathlib import Path

EXACT_PATH_SCORE = 120
PATH_PREFIX_SCORE = 100
MULTI_WORD_TERM_SCORE = 30
SINGLE_TERM_SCORE = 10
MAX_ROUTES = 2

# Generic words that must not route on their own. They appear in almost
# every shell task ("shell exits with an error"), so matching them would
# fan every vague topic out to several skills.
STOPWORDS = frozenset(
    {
        "job",
        "jobs",
        "process",
        "processes",
        "shell",
        "command",
        "commands",
        "test",
        "tests",
        "code",
        "file",
        "files",
        "error",
        "errors",
        "bug",
        "fix",
        "fail",
        "doge",
        "dogesh",
        "doge-shell",
    }
)

FALLBACK_ROUTE = {
    "id": "repo-general",
    "risk": "normal",
    "skills": ["doge-shell-repo"],
    "references": ["docs/ai/skills/doge-shell-repo/references/task-map.md"],
}


def skill_path(name: str) -> str:
    """Canonical repository path, independent of agent skill discovery."""
    return f"docs/ai/skills/{name}/SKILL.md"


def skill_entry(name: str) -> dict[str, str]:
    return {"name": name, "path": skill_path(name)}


def load_routes(manifest_path: str | Path) -> dict:
    """Read and JSON-parse the routing manifest."""
    with open(manifest_path, encoding="utf-8") as handle:
        return json.load(handle)


def normalize_topic(topic: str) -> str:
    """Case-fold a topic for substring matching."""
    return topic.casefold()


def normalize_path(raw: str) -> str:
    """Repo-relative forward-slash path (strips leading ./ and /)."""
    text = raw.replace("\\", "/").strip()
    while text.startswith("./"):
        text = text[2:]
    return text.lstrip("/")


def is_multi_word(term: str) -> bool:
    return " " in term.strip() and len(term.strip().split()) > 1


def term_hit(folded_topic: str, term: str) -> bool:
    """True when a term matches the casefolded topic.

    Short ASCII alphabetic terms (``wait``, ``fg``, ``AST``, ``PTY``)
    only match on word boundaries, so prose like ``config`` or ``debug``
    never routes to execution semantics. Longer, symbolic, and non-ASCII
    terms (``wait -n``, ``$!``, ``ジョブ制御``) use plain substring
    matching, since CJK prose has no word separators.
    """
    folded = term.casefold()
    if not folded:
        return False
    if term.isascii() and term.isalpha() and len(term) <= 4:
        # Boundary-aware scan without regex word classes, which misbehave
        # around non-ASCII prose.
        for index in range(len(folded_topic) - len(folded) + 1):
            if folded_topic[index : index + len(folded)] != folded:
                continue
            before = folded_topic[index - 1] if index > 0 else " "
            after = folded_topic[index + len(folded)] if index + len(folded) < len(folded_topic) else " "
            if not before.isalnum() and before != "_" and not after.isalnum() and after != "_":
                return True
        return False
    return folded in folded_topic


def score_route(route: dict, topic: str, paths: list[str]) -> tuple[int, list[str], list[str]]:
    """Score one route. Returns (score, matched_terms, matched_paths)."""
    score = 0
    matched_terms: list[str] = []
    matched_paths: list[str] = []
    wanted = [normalize_path(path) for path in paths]

    for exact in route.get("exact_paths", []):
        norm = normalize_path(exact)
        if norm and norm in wanted:
            score += EXACT_PATH_SCORE
            matched_paths.append(norm)

    for prefix in route.get("path_prefixes", []):
        norm = normalize_path(prefix)
        if not norm:
            continue
        if not norm.endswith("/"):
            norm += "/"
        for candidate in wanted:
            if candidate.startswith(norm) and candidate not in matched_paths:
                score += PATH_PREFIX_SCORE
                matched_paths.append(candidate)

    for term in route.get("terms", []):
        if not term_hit(topic, term):
            continue
        stripped = term.casefold().strip()
        if not is_multi_word(term) and stripped in STOPWORDS:
            continue
        if is_multi_word(term):
            score += MULTI_WORD_TERM_SCORE
            matched_terms.append(term)
        else:
            score += SINGLE_TERM_SCORE
            matched_terms.append(term)

    return score, matched_terms, matched_paths


def route_context(
    manifest: dict,
    topic: str = "",
    paths: list[str] | tuple[str, ...] = (),
    limit: int = MAX_ROUTES,
) -> dict:
    """Route a task to narrow skills. Pure function of its inputs."""
    folded = normalize_topic(topic)
    wanted = [normalize_path(path) for path in paths if normalize_path(path)]

    scored = []
    for route in manifest.get("routes", []):
        score, terms, matched = score_route(route, folded, wanted)
        if score > 0:
            scored.append(
                {
                    "id": route["id"],
                    "score": score,
                    "matched_terms": sorted(set(terms)),
                    "matched_paths": sorted(set(matched)),
                    "skills": [skill_entry(name) for name in route.get("skills", [])],
                    "references": list(route.get("references", [])),
                    "risk": route.get("risk", "normal"),
                }
            )

    scored.sort(key=lambda entry: (-entry["score"], entry["id"]))
    picked = list(scored[:limit])
    # A tie at the boundary stretches to one extra route, never more.
    if len(scored) > limit and scored[limit]["score"] == scored[limit - 1]["score"]:
        picked.append(scored[limit])

    if not picked:
        picked = [
            {
                "id": FALLBACK_ROUTE["id"],
                "score": 0,
                "matched_terms": [],
                "matched_paths": [],
                "skills": [skill_entry(name) for name in FALLBACK_ROUTE["skills"]],
                "references": list(FALLBACK_ROUTE["references"]),
                "risk": FALLBACK_ROUTE["risk"],
            }
        ]

    risk = "high" if any(entry["risk"] == "high" for entry in picked) else "normal"
    return {"risk": risk, "routes": picked}


def validate_manifest(manifest: dict, repo_root: str | Path) -> list[str]:
    """Check manifest shape and that skills/references/paths still exist."""
    errors: list[str] = []
    root = Path(repo_root)

    if manifest.get("version") != 1:
        errors.append(f"unsupported manifest version: {manifest.get('version')!r}")

    routes = manifest.get("routes")
    if not isinstance(routes, list) or not routes:
        return errors + ["manifest has no routes"]

    seen: set[str] = set()
    for index, route in enumerate(routes):
        where = f"routes[{index}]"
        route_id = route.get("id", f"<missing #{index}>")
        if route.get("id") in seen:
            errors.append(f"duplicate route id: {route.get('id')!r}")
        seen.add(route.get("id"))

        for key in ("terms", "exact_paths", "path_prefixes", "skills", "references"):
            if not isinstance(route.get(key), list):
                errors.append(f"{where} ({route_id}): {key!r} must be a list")
        if route.get("risk") not in ("high", "normal"):
            errors.append(f"{where} ({route_id}): risk must be high|normal")

        for skill in route.get("skills", []):
            skill_file = root / skill_path(skill)
            if not skill_file.is_file():
                errors.append(f"{where} ({route_id}): unknown skill {skill!r}")
        for reference in route.get("references", []):
            if (root / reference).is_file():
                continue
            errors.append(f"{where} ({route_id}): missing reference {reference!r}")
        for exact in route.get("exact_paths", []):
            if (root / exact).exists():
                continue
            errors.append(f"{where} ({route_id}): missing exact path {exact!r}")
        for prefix in route.get("path_prefixes", []):
            if (root / prefix).is_dir():
                continue
            errors.append(f"{where} ({route_id}): path prefix is not a directory {prefix!r}")

    return errors
