#!/usr/bin/env python3
"""Redact known runner credentials before a manual CI artifact upload."""

import argparse
from pathlib import Path

from agent_eval.runner import redact_artifacts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()
    if args.output_dir.exists():
        for artifact in args.output_dir.iterdir():
            if artifact.is_dir():
                redact_artifacts(artifact)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
