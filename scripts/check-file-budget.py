#!/usr/bin/env python3
"""Keep individual files small and readable enough for an agent to load whole.

This repo optimizes for an AI agent's token budget as much as for a human's
screen: `AGENTS.md` and `docs/ai/` exist specifically to keep what an agent
must read for a given task small. That discipline erodes silently if a single
source file or reference doc is left to grow without limit. These checks are
the mechanical backstop:

  1. Non-test `.rs` files stay under 800 lines, and files over 400 lines carry
     a `//!` module doc so an agent can learn what a file holds without
     reading it. Both are tracked by an allowlist that records today's debt
     and shrinks as Phase 2 of the AI-agent refactor splits files along their
     existing seams - mirrors scripts/check-portability.py's allowlist, so a
     *new* violation fails immediately while existing ones are a tracked,
     visible, shrinking debt rather than a wall no one can pass.
  2. `docs/ai/**/references/**/*.md` stays under 20KB, so a single reference
     read does not blow an agent's context on its own.
  3. Repo-root-relative paths named in `AGENTS.md`, `CLAUDE.md`, and
     `docs/ai/**/*.md` actually exist, so a stale rename does not send an
     agent looking for a file that moved.

See docs/ai/skills/doge-shell-repo/references/task-map.md and AGENTS.md.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / "scripts/file-budget-allowlist.txt"
CRATE_DIRS = ("dsh", "dsh-builtin", "dsh-openai", "dsh-types", "dsh-frecency")

MAX_SOURCE_LINES = 800
MODULE_DOC_MIN_LINES = 400
MODULE_DOC_SCAN_LINES = 15
MAX_REFERENCE_BYTES = 20 * 1024

OVERSIZED = "oversized"
NO_DOC = "no-doc"
DOC_PATH_OK = "doc-path-ok"
CATEGORIES = (OVERSIZED, NO_DOC, DOC_PATH_OK)

# `doc-path-ok<TAB>doc/file.md::mentioned/path` allows a doc to name a path
# that intentionally does not exist - e.g. invariants/completion.md naming
# `dsh/completions/` as the old duplicate directory this repo stopped using.
# Prefer fixing the doc; use this only for genuinely historical prose.

GUIDANCE_DOCS = (REPO_ROOT / "AGENTS.md", REPO_ROOT / "CLAUDE.md")
GUIDANCE_DIR = REPO_ROOT / "docs/ai"

# Only a backtick span starting with one of these is unambiguously a
# repo-root-relative path. Prose elsewhere routinely writes crate-relative
# fragments ("completion/integrated.rs", meaning dsh/src/completion/...) or
# runtime paths a project owns (".dsh/hooks.json"), and neither resolves from
# the repo root - flagging those would be false positives, not real staleness.
ROOT_PREFIXES = tuple(f"{crate}/" for crate in CRATE_DIRS) + (
    "docs/",
    "scripts/",
    "completions/",
    "output-schemas/",
    "AGENTS.md",
    "CLAUDE.md",
    "README.md",
)


def is_test_file(path: Path) -> bool:
    if path.name == "tests.rs" or path.name.endswith("_tests.rs"):
        return True
    return "tests" in path.relative_to(REPO_ROOT).parts


def rust_sources() -> list[Path]:
    paths: list[Path] = []
    for crate in CRATE_DIRS:
        crate_dir = REPO_ROOT / crate
        if not crate_dir.is_dir():
            continue
        for path in crate_dir.rglob("*.rs"):
            if "target" in path.relative_to(REPO_ROOT).parts:
                continue
            paths.append(path)
    return sorted(paths)


def has_module_doc(path: Path) -> bool:
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    return any(
        line.lstrip().startswith("//!") for line in lines[:MODULE_DOC_SCAN_LINES]
    )


def read_allowlist() -> set[tuple[str, str]]:
    if not ALLOWLIST_PATH.exists():
        return set()
    entries = set()
    for line in ALLOWLIST_PATH.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        category, _, relative = line.partition("\t")
        if not relative or category not in CATEGORIES:
            raise ValueError(f"allowlist entry is not {{{'|'.join(CATEGORIES)}}}<TAB>path: {line!r}")
        entries.add((category, relative))
    return entries


def write_allowlist(entries: set[tuple[str, str]]) -> None:
    header = (
        "# Files this repository still accepts over the token-budget limits in\n"
        "# scripts/check-file-budget.py, as category<TAB>path.\n"
        "#\n"
        "# An entry is tracked debt, not an endorsement: Phase 2 of the AI-agent\n"
        "# refactor splits `oversized` files along their existing seams and adds a\n"
        "# `//!` module doc to `no-doc` files. A file dropping out of a category\n"
        "# should leave this list; a *new* file entering one should not be added\n"
        "# here without first checking whether it can be split or documented\n"
        "# instead of merely allowlisted.\n"
        "#\n"
        "# Regenerate with: scripts/check-file-budget.py --update\n"
    )
    body = "".join(f"{category}\t{relative}\n" for category, relative in sorted(entries))
    ALLOWLIST_PATH.write_text(header + body, encoding="utf-8")


def collect_entries() -> set[tuple[str, str]]:
    entries = set()
    for path in rust_sources():
        if is_test_file(path):
            continue
        relative = path.relative_to(REPO_ROOT).as_posix()
        line_count = sum(1 for _ in path.open(encoding="utf-8", errors="replace"))
        if line_count > MAX_SOURCE_LINES:
            entries.add((OVERSIZED, relative))
        if line_count > MODULE_DOC_MIN_LINES and not has_module_doc(path):
            entries.add((NO_DOC, relative))
    return entries


def check_reference_sizes() -> list[str]:
    failures = []
    for path in sorted(GUIDANCE_DIR.rglob("references/**/*.md")):
        size = path.stat().st_size
        if size > MAX_REFERENCE_BYTES:
            relative = path.relative_to(REPO_ROOT).as_posix()
            failures.append(
                f"{relative}: {size} bytes, over the {MAX_REFERENCE_BYTES}-byte "
                "reference budget; split it by topic the way ai-architecture.md "
                "and invariants.md were split (see docs/ai/README.md)"
            )
    return failures


BACKTICK_SPAN = re.compile(r"`([^`\n]+)`")


TRAILING_LOCATION = re.compile(r":\d+(-\d+)?$")


def candidate_paths(text: str) -> set[str]:
    candidates = set()
    for span in BACKTICK_SPAN.findall(text):
        span = span.strip()
        if not span.startswith(ROOT_PREFIXES):
            continue
        if any(marker in span for marker in ("<", ">", "*", " ")):
            continue
        # `path/to/file.rs:123` or `:123-456` (a line or range reference).
        span = TRAILING_LOCATION.sub("", span)
        candidates.add(span.rstrip("/"))
    return candidates


def check_guidance_paths(allowed: set[tuple[str, str]]) -> list[str]:
    doc_path_ok = {relative for category, relative in allowed if category == DOC_PATH_OK}
    failures = []
    docs = list(GUIDANCE_DOCS) + sorted(GUIDANCE_DIR.rglob("*.md"))
    for doc in docs:
        if not doc.exists():
            continue
        relative_doc = doc.relative_to(REPO_ROOT).as_posix()
        text = doc.read_text(encoding="utf-8", errors="replace")
        for candidate in sorted(candidate_paths(text)):
            if (REPO_ROOT / candidate).exists():
                continue
            if f"{relative_doc}::{candidate}" in doc_path_ok:
                continue
            failures.append(f"{relative_doc}: references missing path `{candidate}`")
    return failures


def main() -> int:
    argv = sys.argv[1:]
    entries = collect_entries()
    # `doc-path-ok` entries are hand-curated (not auto-detected like the other
    # two categories), so --update must preserve whatever is already there.
    existing_doc_path_ok = {
        (category, relative)
        for category, relative in read_allowlist()
        if category == DOC_PATH_OK
    }

    if "--update" in argv:
        write_allowlist(entries | existing_doc_path_ok)
        print(f"wrote {ALLOWLIST_PATH.relative_to(REPO_ROOT)}: {len(entries) + len(existing_doc_path_ok)} entries")
        return 0

    if "--list" in argv:
        for category, relative in sorted(entries | existing_doc_path_ok):
            print(f"{category}\t{relative}")
        return 0

    if argv:
        print(f"usage: {Path(sys.argv[0]).name} [--update | --list]", file=sys.stderr)
        return 2

    failures: list[str] = []

    allowed = read_allowlist()
    tracked_allowed = {
        (category, relative) for category, relative in allowed if category != DOC_PATH_OK
    }
    for category, relative in sorted(entries - tracked_allowed):
        if category == OVERSIZED:
            failures.append(
                f"{relative}: over {MAX_SOURCE_LINES} lines without an allowlist "
                "entry; split it along its existing seams, or record the debt "
                "with scripts/check-file-budget.py --update"
            )
        else:
            failures.append(
                f"{relative}: over {MODULE_DOC_MIN_LINES} lines with no `//!` "
                f"module doc in the first {MODULE_DOC_SCAN_LINES} lines; add a "
                "3-8 line summary of what the file holds (see "
                "dsh/src/completion/dynamic/local.rs), or record the debt with "
                "scripts/check-file-budget.py --update"
            )
    for category, relative in sorted(tracked_allowed - entries):
        failures.append(
            f"{relative}: allowlisted as {category!r} but no longer qualifies; "
            "drop the stale entry with scripts/check-file-budget.py --update"
        )

    failures.extend(check_reference_sizes())
    failures.extend(check_guidance_paths(allowed))

    if failures:
        for failure in failures:
            print(f"error: {failure}", file=sys.stderr)
        print(f"file budget lint failed: {len(failures)} issue(s)", file=sys.stderr)
        return 1

    oversized_count = sum(1 for category, _ in allowed if category == OVERSIZED)
    no_doc_count = sum(1 for category, _ in allowed if category == NO_DOC)
    print(
        f"ok file budget: {oversized_count} allowlisted oversized file(s), "
        f"{no_doc_count} allowlisted missing-module-doc file(s), reference "
        "sizes and guidance paths within budget"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
