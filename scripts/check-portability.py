#!/usr/bin/env python3
"""Catch Linux/macOS portability regressions from either host.

doge-shell supports exactly two platforms, but `cargo check` only ever sees the
arm belonging to the host it runs on, and cross-compiling to Darwin is not an
option here (`rusqlite` bundles SQLite and `mac-notification-sys` builds
Objective-C, so both want the Apple SDK). These checks are what a Linux host can
still prove:

  1. OS-specific path literals stay inside the files allowed to read them.
  2. A `target_os` arm is never written without its counterpart.
  3. Tests reach external commands through `common::first_existing`, not a
     hardcoded `/bin/true` that macOS does not ship.
  4. Linker tuning stays scoped to the target that accepts it.
  5. The two-platform guard itself is still in place.

See docs/ai/skills/doge-shell-repo/references/platform-support.md.
"""

from __future__ import annotations

from pathlib import Path
import re
import sys


REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / "scripts/portability-allowlist.txt"
CARGO_CONFIG = REPO_ROOT / ".cargo/config.toml"
CRATE_DIRS = ("dsh", "dsh-builtin", "dsh-openai", "dsh-types", "dsh-frecency")
GUARDED_CRATE_ROOTS = ("dsh/src/lib.rs", "dsh-builtin/src/lib.rs")

# Sources that exist on one platform only, or exist on both and mean different
# things. `/usr/share/man`, `/usr/share/zoneinfo` and `/bin/sh` are on both and
# mean the same thing, so they are deliberately absent.
LINUX_ONLY_PREFIXES = (
    "/proc",
    "/sys/",
    "/run/",
    "/lib/modules",
    "/var/log/journal",
    "/usr/share/kbd",
    "/usr/lib/systemd",
    "/etc/passwd",
    "/etc/shadow",
    "/etc/group",
    "/etc/shells",
    "/etc/fstab",
    "/etc/hostname",
    "/etc/selinux",
    "/etc/audit",
    "/etc/iproute2",
    "/etc/firewalld",
    "/etc/ipset",
    "/etc/wireguard",
    "/etc/pacman",
    "/etc/mkinitcpio",
    "/etc/snapper",
)
MACOS_ONLY_PREFIXES = (
    "/System/Library",
    "/Library/",
)
# Command locations that are identical on Linux and macOS. Anything else is a
# claim that has to be recorded in the allowlist, so that the next reader can see
# somebody checked -- or routed through dsh/tests/common/mod.rs instead.
PORTABLE_COMMAND_PATHS = (
    "/bin/sh",
    "/bin/echo",
    "/bin/ls",
    "/bin/cat",
)

# Absolute paths inside a double-quoted Rust string. Raw strings are covered too,
# since the inner quote still delimits the path.
PATH_LITERAL = re.compile(r'"(/[^"\\\n]*)"')
CFG_ATTRIBUTE = re.compile(r"#!?\[cfg\(")
CFG_MACRO = re.compile(r"\bcfg!\(")
TARGET_OS_VALUE = re.compile(r'target_os\s*=\s*"([a-z0-9_]+)"')
TEST_MODULE = re.compile(r"^\s*#\[cfg\(test\)\]", re.MULTILINE)
# An absolute command path anywhere in the text, including inside a whole shell
# line like "/bin/echo hi | /usr/bin/wc -c". Matching the token rather than the
# surrounding literal keeps allowlist entries stable when the test text changes.
COMMAND_TOKEN = re.compile(r"/(?:usr/)?s?bin/[A-Za-z0-9_.+-]+")
GUARD_PATTERN = re.compile(
    r'#\[cfg\(not\(any\(target_os\s*=\s*"linux"\s*,\s*target_os\s*=\s*"macos"\)\)\)\]'
    r"\s*compile_error!"
)


def strip_comment_lines(source: str) -> str:
    """Drop whole-line comments so prose is never mistaken for code.

    A doc comment that quotes `#[cfg(not(target_os = "macos"))]` to explain the
    convention is not an arm, and a path named in prose is not a read. Only
    lines whose first non-space characters are `//` go, so a `"https://.."` in
    real code survives.
    """
    return "\n".join(
        "" if line.lstrip().startswith("//") else line
        for line in source.splitlines()
    )


def rust_sources() -> list[Path]:
    sources: list[Path] = []
    for crate in CRATE_DIRS:
        sources.extend(sorted((REPO_ROOT / crate).rglob("*.rs")))
    return sources


def balanced_span(text: str, open_index: int) -> str:
    """Return the contents of the parenthesised group starting at `open_index`."""
    depth = 0
    for index in range(open_index, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return text[open_index + 1 : index]
    return text[open_index + 1 :]


def os_specific_literals(source: str) -> set[str]:
    found = set()
    for literal in PATH_LITERAL.findall(source):
        if literal.startswith(LINUX_ONLY_PREFIXES) or literal.startswith(
            MACOS_ONLY_PREFIXES
        ):
            found.add(literal)
    return found


def command_literals(source: str, whole_file_is_test: bool) -> set[str]:
    """Non-portable absolute command paths reached from test code."""
    if whole_file_is_test:
        region = source
    else:
        match = TEST_MODULE.search(source)
        if match is None:
            return set()
        region = source[match.start() :]

    return {
        command
        for command in COMMAND_TOKEN.findall(region)
        if command not in PORTABLE_COMMAND_PATHS
    }


def cfg_arm_counts(source: str) -> tuple[dict[str, int], dict[str, int]]:
    """Positive and negative `target_os` arms declared by cfg attributes.

    A predicate that negates both supported targets is the unsupported-platform
    guard rather than an arm, so it is skipped: it needs no counterpart.
    """
    positive: dict[str, int] = {"linux": 0, "macos": 0}
    negative: dict[str, int] = {"linux": 0, "macos": 0}

    for match in CFG_ATTRIBUTE.finditer(source):
        predicate = balanced_span(source, match.end() - 1)
        mentioned = {
            value
            for value in TARGET_OS_VALUE.findall(predicate)
            if value in positive
        }
        if not mentioned:
            continue

        negated = "not(" in predicate.replace(" ", "")
        if negated and mentioned == {"linux", "macos"}:
            continue

        bucket = negative if negated else positive
        for value in mentioned:
            bucket[value] += 1

    return positive, negative


def unpaired_cfg_arms(source: str) -> list[str]:
    positive, negative = cfg_arm_counts(source)
    problems: list[str] = []

    if positive["macos"] and not negative["macos"] and not positive["linux"]:
        problems.append(
            'has #[cfg(target_os = "macos")] with no Linux arm; add the matching '
            '#[cfg(not(target_os = "macos"))]'
        )
    if positive["linux"] and not negative["linux"] and not positive["macos"]:
        problems.append(
            'has #[cfg(target_os = "linux")] with no macOS arm; add the matching '
            '#[cfg(target_os = "macos")]'
        )
    if negative["macos"] and not positive["macos"]:
        problems.append(
            'has #[cfg(not(target_os = "macos"))] with no macOS arm; macOS silently '
            "loses the item"
        )
    if negative["linux"] and not positive["linux"] and not positive["macos"]:
        problems.append(
            'has #[cfg(not(target_os = "linux"))] with no Linux arm; Linux silently '
            "loses the item"
        )

    return problems


def unguarded_cfg_macros(source: str) -> int:
    """`cfg!(target_os = ...)` expressions with no `else` on the same `if`."""
    unguarded = 0
    for match in CFG_MACRO.finditer(source):
        predicate = balanced_span(source, match.end() - 1)
        if not TARGET_OS_VALUE.search(predicate):
            continue
        # The other branch has to appear before the enclosing item ends; eight
        # lines is generous for the `if cfg!(..) { .. } else { .. }` shape.
        tail = source[match.end() : match.end() + 400]
        if "else" not in tail.split("\n\n")[0]:
            unguarded += 1
    return unguarded


def collect_sites() -> list[tuple[str, str]]:
    sites: set[tuple[str, str]] = set()
    for path in rust_sources():
        relative = path.relative_to(REPO_ROOT).as_posix()
        source = strip_comment_lines(path.read_text(encoding="utf-8"))
        whole_file_is_test = "/tests/" in f"/{relative}"
        for literal in os_specific_literals(source) | command_literals(
            source, whole_file_is_test
        ):
            sites.add((relative, literal))
    return sorted(sites)


def read_allowlist() -> set[tuple[str, str]]:
    if not ALLOWLIST_PATH.exists():
        return set()

    entries = set()
    for line in ALLOWLIST_PATH.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        relative, _, literal = line.partition("\t")
        if not literal:
            raise ValueError(f"allowlist entry is not path<TAB>literal: {line!r}")
        entries.add((relative, literal))
    return entries


def write_allowlist(sites: list[tuple[str, str]]) -> None:
    header = (
        "# OS-specific path literals this repository accepts, as path<TAB>literal.\n"
        "#\n"
        "# An entry means: this file reads a source that only one platform has, and\n"
        "# that is the intended behaviour there -- either the file is a documented\n"
        "# Linux-only provider whose candidates are meant to be empty elsewhere, or\n"
        "# the literal is test data that is never opened.\n"
        "#\n"
        "# scripts/check-portability.py fails on any pair not listed here, so adding a\n"
        "# new Linux-only source is a reviewable diff rather than a silent macOS gap.\n"
        "# Regenerate with: scripts/check-portability.py --update\n"
        "#\n"
        "# See docs/ai/skills/doge-shell-repo/references/platform-support.md\n"
    )
    body = "".join(f"{relative}\t{literal}\n" for relative, literal in sites)
    ALLOWLIST_PATH.write_text(header + body, encoding="utf-8")


def check_cargo_config() -> list[str]:
    """`rustflags` must sit under a `[target.'cfg(..)']` section.

    A bare `[build] rustflags = [..]` applies the Linux mold flag to macOS too,
    where clang rejects `-fuse-ld=mold` and every link fails.
    """
    if not CARGO_CONFIG.exists():
        return []

    failures: list[str] = []
    section = ""
    for number, line in enumerate(
        CARGO_CONFIG.read_text(encoding="utf-8").splitlines(), start=1
    ):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped[1:-1]
            continue
        if not stripped.startswith("rustflags"):
            continue
        if not section.startswith("target."):
            failures.append(
                f".cargo/config.toml:{number}: rustflags under [{section}] applies to "
                "every target; scope it to [target.'cfg(..)']"
            )
    return failures


# `dirs::config_dir()` is `~/.config` on Linux and `~/Library/Application Support`
# on macOS, so it is a platform branch with no `cfg` to notice. Mixing it with
# `xdg::BaseDirectories` is how an installed runtime skill became invisible to
# the chat agent on macOS: the installer wrote one directory and the loader read
# the other. These two files own the resolution; everything else asks them.
CONFIG_DIR_CALL = re.compile(r"\bdirs::config_dir\s*\(")
CONFIG_DIR_OWNERS = (
    "dsh-builtin/src/config_paths.rs",
    "dsh/src/environment/mod.rs",
)


def check_config_dir_resolution() -> list[str]:
    failures: list[str] = []
    for path in rust_sources():
        relative = path.relative_to(REPO_ROOT).as_posix()
        if relative in CONFIG_DIR_OWNERS:
            continue
        source = strip_comment_lines(path.read_text(encoding="utf-8"))
        hits = len(CONFIG_DIR_CALL.findall(source))
        if hits:
            failures.append(
                f"{relative}: {hits} direct dirs::config_dir() call(s); it means a "
                "different directory on macOS. Use dsh_builtin::config_paths, or "
                "crate::environment::get_config_file in the dsh crate"
            )
    return failures


def check_platform_guard() -> list[str]:
    failures: list[str] = []
    for relative in GUARDED_CRATE_ROOTS:
        path = REPO_ROOT / relative
        if not path.exists():
            failures.append(f"missing crate root: {relative}")
            continue
        if not GUARD_PATTERN.search(
            strip_comment_lines(path.read_text(encoding="utf-8"))
        ):
            failures.append(
                f"{relative}: missing the two-platform guard "
                '(#[cfg(not(any(target_os = "linux", target_os = "macos")))] '
                "compile_error!), which is what makes "
                '#[cfg(not(target_os = "macos"))] mean "Linux"'
            )
    return failures


def main() -> int:
    argv = sys.argv[1:]
    sites = collect_sites()

    if "--update" in argv:
        write_allowlist(sites)
        print(f"wrote {ALLOWLIST_PATH.relative_to(REPO_ROOT)}: {len(sites)} entries")
        return 0

    if "--list" in argv:
        for relative, literal in sites:
            print(f"{relative}\t{literal}")
        return 0

    if argv:
        print(f"usage: {Path(sys.argv[0]).name} [--update | --list]", file=sys.stderr)
        return 2

    failures: list[str] = []

    allowed = read_allowlist()
    for relative, literal in sorted(set(sites) - allowed):
        failures.append(
            f"{relative}: reads the OS-specific path {literal!r} without an "
            "allowlist entry; give the other platform a source (see "
            "dsh/src/completion/generators/user.rs) or record the exception with "
            "scripts/check-portability.py --update"
        )
    for relative, literal in sorted(allowed - set(sites)):
        failures.append(
            f"{relative}: allowlisted OS-specific path {literal!r} is gone; drop the "
            "stale entry with scripts/check-portability.py --update"
        )

    paired_files = 0
    for path in rust_sources():
        relative = path.relative_to(REPO_ROOT).as_posix()
        source = strip_comment_lines(path.read_text(encoding="utf-8"))
        problems = unpaired_cfg_arms(source)
        for problem in problems:
            failures.append(f"{relative}: {problem}")
        unguarded = unguarded_cfg_macros(source)
        if unguarded:
            failures.append(
                f"{relative}: {unguarded} cfg!(target_os = ..) expression(s) with no "
                "else branch; the other platform falls through silently"
            )
        positive, negative = cfg_arm_counts(source)
        if not problems and (any(positive.values()) or any(negative.values())):
            paired_files += 1

    failures.extend(check_cargo_config())
    failures.extend(check_platform_guard())
    failures.extend(check_config_dir_resolution())

    if failures:
        for failure in failures:
            print(f"error: {failure}", file=sys.stderr)
        print(
            f"portability lint failed: {len(failures)} issue(s)",
            file=sys.stderr,
        )
        return 1

    print(
        f"ok portability: {len(allowed)} allowlisted OS-specific literals, "
        f"{paired_files} file(s) with paired target_os arms"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
