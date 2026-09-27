#!/usr/bin/env python3
"""Guard direct child waits and wait-job ownership mutation in production Rust."""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/execution-authority-allowlist.txt"
CRATE_DIRS = ("dsh", "dsh-builtin", "dsh-openai", "dsh-types", "dsh-frecency")
CALLS = {
    "waitpid": re.compile(r"\bwaitpid\s*\("),
    "wait_jobs.remove": re.compile(r"\bwait_jobs\s*\.\s*remove\s*\("),
}
ANY_CHILD = re.compile(
    r"\bwaitpid\s*\(\s*(?:-\s*1|(?:\w+\s*::\s*)*Pid\s*::\s*from_raw\s*\(\s*-\s*1\s*\))\s*[,)]"
)
CHAR_LITERAL = re.compile(
    r"(?:b)?'(?:[^'\\\r\n]|\\(?:u\{[0-9a-fA-F_]{1,6}\}|x[0-9a-fA-F]{2}|.))'"
)
RAW_LITERAL = re.compile(r'(?<![\w])(?:br|cr|r)(?P<hashes>#{0,255})"')


def production_code(source):
    """Mask Rust comments/literals and cfg(test) modules, preserving newlines."""
    result = []
    index = 0
    while index < len(source):
        end = index
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            if end < 0:
                end = len(source)
        elif source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        elif literal := CHAR_LITERAL.match(source, index):
            end = literal.end()
        elif literal := RAW_LITERAL.match(source, index):
            closer = '"' + literal.group("hashes")
            close = source.find(closer, literal.end())
            end = len(source) if close < 0 else close + len(closer)
        elif source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            end = min(end, len(source))

        if end > index:
            result.extend("\n" if char == "\n" else " " for char in source[index:end])
            index = end
        else:
            result.append(source[index])
            index += 1
    clean = "".join(result)
    # An inline test module can occur before later production items. Mask
    # only its balanced body; truncating at cfg(test) would miss later calls.
    test_module = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+\w+\s*\{")
    while match := test_module.search(clean):
        depth = 1
        end = match.end()
        while end < len(clean) and depth:
            if clean[end] == "{":
                depth += 1
            elif clean[end] == "}":
                depth -= 1
            end += 1
        clean = clean[:match.start()] + "".join("\n" if c == "\n" else " " for c in clean[match.start():end]) + clean[end:]
    return clean


def inspect(path, source, allowed):
    clean = production_code(source)
    errors = []
    for match in ANY_CHILD.finditer(clean):
        errors.append((clean.count("\n", 0, match.start()) + 1, "any-child wait is forbidden"))
    for rule, pattern in CALLS.items():
        if path in allowed[rule]:
            continue
        for match in pattern.finditer(clean):
            errors.append((clean.count("\n", 0, match.start()) + 1, f"unauthorized {rule} call"))
    return sorted(set(errors))


def load_allowlist(path):
    allowed = {rule: set() for rule in CALLS}
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 2 or parts[0] not in allowed:
            raise ValueError(f"{path}:{number}: invalid allowlist entry")
        allowed[parts[0]].add(parts[1])
    return allowed


def self_test():
    allowed = {"waitpid": {"legal.rs"}, "wait_jobs.remove": set()}
    cases = [
        ("legal.rs", "waitpid(pid, None);", []),
        ("new.rs", "waitpid(pid, None);", [(1, "unauthorized waitpid call")]),
        ("legal.rs", "waitpid(-1, None);", [(1, "any-child wait is forbidden")]),
        ("legal.rs", "// waitpid(-1, None);", []),
        ("legal.rs", "/* waitpid(-1, None); */", []),
        ("legal.rs", "waitpid(Pid::from_raw(-1), None);", [(1, "any-child wait is forbidden")]),
        ("legal.rs", "waitpid(nix::unistd::Pid::from_raw(-1), None);", [(1, "any-child wait is forbidden")]),
        ("legal.rs", "Pid::from_raw(-1);", []),
        ("legal.rs", "// waitpid(Pid::from_raw(-1), None);", []),
        ("new.rs", "shell.wait_jobs.remove(0);", [(1, "unauthorized wait_jobs.remove call")]),
        ("new.rs", "#[cfg(test)]\nmod tests { waitpid(-1, None); }", []),
        ("new.rs", "#[cfg(test)]\nmod tests { waitpid(-1, None); }\nwaitpid(pid, None);", [(3, "unauthorized waitpid call")]),
        ("new.rs", "let c = '\"';\nwaitpid(pid, None);", [(2, "unauthorized waitpid call")]),
        ("new.rs", 'let s = r#"quoted " text"#;\nwaitpid(pid, None);', [(2, "unauthorized waitpid call")]),
        ("legal.rs", 'let s = br##"waitpid(-1, None) "# text"##;', []),
    ]
    for path, source, expected in cases:
        actual = inspect(path, source, allowed)
        if actual != expected:
            print(f"self-test failed: {path}: {actual!r} != {expected!r}", file=sys.stderr)
            return 1
    print(f"execution authority self-test: {len(cases)}/{len(cases)} passed")
    return 0


def is_test_file(path):
    if "tests" in path.parts or "benches" in path.parts:
        return True
    return path.name == "tests.rs" or path.name.endswith("_tests.rs")


def production_files():
    for crate in CRATE_DIRS:
        root = ROOT / crate
        if not root.is_dir():
            continue
        for file in sorted(root.rglob("*.rs")):
            if is_test_file(file):
                continue
            yield file


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--path",
        action="append",
        default=[],
        dest="paths",
        help="check only these repo-relative files (fast mode for hooks)",
    )
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    allowed = load_allowlist(ALLOWLIST)
    errors = []
    observed = {rule: set() for rule in CALLS}
    if args.paths:
        # Fast mode: only the edited files matter. Test files can use
        # waitpid freely, so they pass silently; stale entries need the
        # whole tree and are a CI concern, not a hook concern.
        selected = []
        for raw in args.paths:
            candidate = Path(raw)
            if not candidate.is_absolute():
                candidate = ROOT / raw
            try:
                relative = candidate.resolve().relative_to(ROOT)
            except ValueError:
                continue
            if relative.suffix != ".rs" or is_test_file(relative):
                continue
            if candidate.is_file():
                selected.append((candidate, relative.as_posix()))
        for file, path in selected:
            source = file.read_text(encoding="utf-8")
            errors.extend(
                f"{path}:{line}: {message}" for line, message in inspect(path, source, allowed)
            )
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        return 0
    for file in production_files():
        path = file.relative_to(ROOT).as_posix()
        source = file.read_text(encoding="utf-8")
        clean = production_code(source)
        for rule, pattern in CALLS.items():
            if pattern.search(clean):
                observed[rule].add(path)
        errors.extend(f"{path}:{line}: {message}" for line, message in inspect(path, source, allowed))
    for rule, paths in allowed.items():
        for path in sorted(paths - observed[rule]):
            errors.append(f"{path}: stale {rule} allowlist entry")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
