#!/usr/bin/env python3
"""Check that every private external method in Gateway.kt has a JNI export."""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kotlin", required=True, type=Path)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--nm", required=True, type=Path)
    args = parser.parse_args()

    for path in (args.kotlin, args.library, args.nm):
        if not path.exists():
            parser.error(f"required path does not exist: {path}")

    source = args.kotlin.read_text(encoding="utf-8")
    methods = re.findall(r"\bprivate\s+external\s+fun\s+(\w+)\s*\(", source)
    if not methods:
        print("ERROR: no private external methods found in Gateway.kt", file=sys.stderr)
        return 2
    if len(methods) != len(set(methods)):
        print("ERROR: duplicate native method declaration in Gateway.kt", file=sys.stderr)
        return 2

    expected = {f"Java_dev_agate_core_Gateway_{method}" for method in methods}
    result = subprocess.run(
        [str(args.nm), "-D", "--defined-only", str(args.library)],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        return result.returncode or 2

    exported = set()
    for line in result.stdout.splitlines():
        fields = line.split()
        if fields and fields[-1].startswith("Java_dev_agate_core_Gateway_"):
            exported.add(fields[-1])

    missing = sorted(expected - exported)
    extra = sorted(exported - expected)
    print(f"Gateway.kt native declarations: {len(methods)}")
    print(f"JNI exports found: {len(exported)}")
    if missing:
        print("MISSING JNI EXPORTS:", *missing, sep="\n  ", file=sys.stderr)
    if extra:
        print("UNDECLARED JNI EXPORTS:", *extra, sep="\n  ", file=sys.stderr)
    if missing or extra:
        return 1
    print("OK: Kotlin declarations and exported JNI symbols match exactly")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
