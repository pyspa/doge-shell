#!/usr/bin/env python3
"""Inspect persisted deterministic grade without rerunning an agent."""

import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result", type=Path)
    args = parser.parse_args()
    result = json.loads(args.result.read_text(encoding="utf-8"))
    print(json.dumps(result["grade"], indent=2))
    return int(not result["grade"]["outcome"]["pass"])


if __name__ == "__main__":
    raise SystemExit(main())
