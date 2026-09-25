#!/usr/bin/env python3
"""Catch runtime-authority regressions: shell state must stay authoritative.

doge-shell unifies runtime command resolution, child environments, and
subprocess spawning on the logical shell state:

  - executable lookup resolves through `Environment.variable_state.paths`
    (via `CommandRuntimeSnapshot`), never the process-global `PATH`;
  - children receive exactly `Environment::child_process_env()`, so a
    logically unset variable is never resurrected from `std::env`;
  - `/bin/sh` is the fixed internal interpreter, never PATH-resolved.

The compiler cannot see which `PATH` a spawn searches or which environment
a child inherits, so this script makes new violations a reviewable diff:

  - Rule 1 (`process-env`): `std::env::var` / `var_os` / `vars` / `vars_os`
    reads in production code. Runtime consumers must read shell variables;
    only intentional process/integration boundaries stay, with a reasoned
    allowlist entry.
  - Rule 2 (`which`): `which::which` / `which_in` external PATH resolvers.
    Logical runtime consumers must use the snapshot, never `which`.
  - Rule 3 (`command`): direct `Command::new` / `std::process::Command::new`
    / `tokio::process::Command::new` construction. Bare literals such as
    `Command::new("git")` leave lookup to the OS and break unexported
    logical PATHs; constructions stay with their approved owners.
  - Rule 4 (`set-cwd`): `std::env::set_current_dir`. Production owner is
    the canonical directory-change path only.

Test-only regions (`tests/` directories, `tests.rs` / `*_tests.rs` files,
inline `#[cfg(test)] mod ...` blocks) are out of scope.

Usage: scripts/check-runtime-authority.py [--list]
"""

from __future__ import annotations

from pathlib import Path
import re
import sys


REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / "scripts/runtime-authority-allowlist.txt"
CRATE_DIRS = ("dsh", "dsh-builtin", "dsh-openai", "dsh-types", "dsh-frecency")

# `std::env::X(` plus the `use std::env;` short spelling. Only value reads:
# path helpers (`split_paths`, `join_paths`), `current_dir` / `current_exe`,
# and test-harness mutation (`set_var`, `remove_var`) are separate rules or
# out of scope.
PROCESS_ENV_CALL = re.compile(
    r"(?:std::env::|env::)(var|var_os|vars|vars_os)\s*\("
)
WHICH_CALL = re.compile(r"which::(which(?:_in)?)\s*\(")
COMMAND_CALL = re.compile(
    r"(tokio::process::Command::new|std::process::Command::new|Command::new)\s*\("
)
SET_CWD_CALL = re.compile(r"std::env::set_current_dir\s*\(")
STRING_ARG = re.compile(r'\s*"((?:[^"\\]|\\.)*)"')
CFG_TEST_MOD = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\(crate\))?\s+)?mod\s+(\w+)\s*\{")


def strip_line_comments(source: str) -> str:
    """Drop whole-line comments so prose never counts as a call site."""
    return "\n".join(
        "" if line.lstrip().startswith("//") else line
        for line in source.splitlines()
    )


def lexical_blank(source: str) -> str:
    """Blank string/char literals and comments, preserving length and lines.

    Brace matching for test-module stripping must not see braces inside
    `"{"`, `'}'`, `// }`, or lifetimes (`'a`): a lone `{` in test prose
    would otherwise extend the stripped span into production code and hide
    real sites from the gate (fail-open). Same length as the input, so spans
    map back onto the original text.
    """
    out = list(source)
    index, length = 0, len(source)
    while index < length:
        char = source[index]
        if char == "/" and index + 1 < length and source[index + 1] == "/":
            end = source.find("\n", index)
            end = length if end < 0 else end
            for pos in range(index, end):
                out[pos] = " "
            index = end
        elif char == "/" and index + 1 < length and source[index + 1] == "*":
            end = source.find("*/", index + 2)
            end = length if end < 0 else end + 2
            for pos in range(index, end):
                if out[pos] != "\n":
                    out[pos] = " "
            index = end
        elif char == '"':
            end = index + 1
            while end < length and source[end] != '"':
                end += 2 if source[end] == "\\" else 1
            end = min(end + 1, length)
            for pos in range(index, end):
                if out[pos] != "\n":
                    out[pos] = " "
            index = end
        elif char == "r" and index + 1 < length and source[index + 1] in '#"':
            raw = re.match(r"r(#+)\"", source[index:])
            if raw:
                close = '"' + raw.group(1)
                end = source.find(close, index + raw.end())
                end = length if end < 0 else end + len(close)
                for pos in range(index, end):
                    if out[pos] != "\n":
                        out[pos] = " "
                index = end
            else:
                index += 1
        elif char == "'":
            # A char literal closes quickly (`'x'`, `'\n'`); anything else
            # is a lifetime (`'a`, `'static`) and not a literal at all.
            literal = re.match(r"'(?:[^'\\]|\\(?:.|u\{[0-9a-fA-F]*\}))'", source[index:])
            if literal:
                for pos in range(index, index + len(literal.group(0))):
                    out[pos] = " "
                index += len(literal.group(0))
            else:
                index += 1
        else:
            index += 1
    return "".join(out)


def strip_test_modules(source: str) -> str:
    """Remove inline `#[cfg(test)] mod name { ... }` spans by brace matching.

    Matching runs on the lexically blanked text so braces inside strings,
    chars, or comments cannot shift the span; the spans are cut from the
    original text, so string-literal call arguments stay intact for
    reporting.
    """
    reference = lexical_blank(source)
    result: list[str] = []
    position = 0
    for match in CFG_TEST_MOD.finditer(reference):
        body_start = match.end() - 1
        depth = 0
        index = body_start
        while index < len(reference):
            if reference[index] == "{":
                depth += 1
            elif reference[index] == "}":
                depth -= 1
                if depth == 0:
                    break
            index += 1
        result.append(source[position : match.start()])
        # Keep line numbers stable for diagnostics.
        result.append("\n" * reference.count("\n", match.start(), index + 1))
        position = index + 1
    result.append(source[position:])
    return "".join(result)


def is_test_file(relative: str) -> bool:
    if "/tests/" in f"/{relative}":
        return True
    if "/benches/" in f"/{relative}":
        return True
    name = relative.rsplit("/", 1)[-1]
    return name == "tests.rs" or name.endswith("_tests.rs")


def production_sources() -> list[Path]:
    sources: list[Path] = []
    for crate in CRATE_DIRS:
        root = REPO_ROOT / crate
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            relative = path.relative_to(REPO_ROOT).as_posix()
            if is_test_file(relative):
                continue
            sources.append(path)
    return sources


def normalize_arg(source: str, open_paren_end: int) -> str:
    """`<literal>` for a string-literal argument, `<expr>` otherwise."""
    match = STRING_ARG.match(source, open_paren_end)
    if match:
        return f'"{match.group(1)}"'
    return "<expr>"


def collect_sites() -> list[tuple[str, str, str]]:
    """All (path, rule, detail) production call sites."""
    sites: list[tuple[str, str, str]] = []
    for path in production_sources():
        relative = path.relative_to(REPO_ROOT).as_posix()
        source = strip_test_modules(
            strip_line_comments(path.read_text(encoding="utf-8"))
        )
        for match in PROCESS_ENV_CALL.finditer(source):
            sites.append((relative, "process-env", f"std::env::{match.group(1)}(...)"))
        for match in WHICH_CALL.finditer(source):
            sites.append((relative, "which", f"which::{match.group(1)}(...)"))
        for match in COMMAND_CALL.finditer(source):
            callee = match.group(1)
            arg = normalize_arg(source, match.end())
            sites.append((relative, "command", f"{callee}({arg})"))
        for _ in SET_CWD_CALL.finditer(source):
            sites.append((relative, "set-cwd", "std::env::set_current_dir(...)"))
    return sorted(sites)


def read_allowlist() -> dict[tuple[str, str, str], int]:
    """Parse path<TAB>rule<TAB>detail<TAB>count entries; `#` lines are reasons."""
    entries: dict[tuple[str, str, str], int] = {}
    if not ALLOWLIST_PATH.exists():
        return entries
    for line in ALLOWLIST_PATH.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 4:
            raise ValueError(f"allowlist entry is not path<TAB>rule<TAB>detail<TAB>count: {line!r}")
        relative, rule, detail, count = parts
        entries[(relative, rule, detail)] = int(count)
    return entries


def main() -> int:
    argv = sys.argv[1:]

    if "--list" in argv:
        for relative, rule, detail in collect_sites():
            print(f"{relative}\t{rule}\t{detail}")
        return 0

    if argv:
        print(f"usage: {Path(sys.argv[0]).name} [--list]", file=sys.stderr)
        return 2

    from collections import Counter

    actual = Counter(collect_sites())
    try:
        allowed = read_allowlist()
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    failures: list[str] = []

    for site, count in sorted(actual.items()):
        relative, rule, detail = site
        if site not in allowed:
            failures.append(
                f"{relative}: new {rule} site `{detail}` without an allowlist entry; "
                "route runtime consumers through CommandRuntimeSnapshot / "
                "ProcessEnvironmentCapability, or record a reasoned exception in "
                "scripts/runtime-authority-allowlist.txt"
            )
        elif allowed[site] != count:
            failures.append(
                f"{relative}: {rule} site `{detail}` count changed "
                f"(allowlisted {allowed[site]}, found {count}); "
                "new same-shape calls need review, then update the entry"
            )

    for site in sorted(set(allowed) - set(actual)):
        relative, rule, detail = site
        failures.append(
            f"{relative}: allowlisted {rule} site `{detail}` is gone; "
            "drop the stale entry from scripts/runtime-authority-allowlist.txt"
        )

    if failures:
        for failure in failures:
            print(f"error: {failure}", file=sys.stderr)
        print(
            f"runtime-authority lint failed: {len(failures)} issue(s)",
            file=sys.stderr,
        )
        return 1

    print(
        f"ok runtime-authority: {len(actual)} production site(s) "
        f"in {len(allowed)} allowlisted group(s)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
