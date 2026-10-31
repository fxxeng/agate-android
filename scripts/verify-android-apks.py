#!/usr/bin/env python3
"""Validate APK ZIP integrity and the packaged arm64-v8a Agate native library."""
from __future__ import annotations

import sys
import zipfile
from pathlib import Path

EXPECTED = "lib/arm64-v8a/libagate_server.so"
ELF_MAGIC = b"\x7fELF"


def verify(path: Path) -> bool:
    print(f"Checking {path}")
    if not path.is_file():
        print(f"ERROR: APK does not exist: {path}", file=sys.stderr)
        return False
    try:
        with zipfile.ZipFile(path) as apk:
            corrupt = apk.testzip()
            if corrupt:
                raise ValueError(f"corrupt ZIP entry: {corrupt}")
            info = apk.getinfo(EXPECTED)
            if info.file_size < 4096:
                raise ValueError(f"native library is unexpectedly small: {info.file_size} bytes")
            with apk.open(info) as stream:
                header = stream.read(20)
            if header[:4] != ELF_MAGIC:
                raise ValueError("packaged native library is not an ELF file")
            # ELF64 little-endian, e_machine = EM_AARCH64 (183).
            if header[4] != 2 or header[5] != 1 or int.from_bytes(header[18:20], "little") != 183:
                raise ValueError("native library is not a 64-bit little-endian AArch64 ELF")
    except (OSError, zipfile.BadZipFile, KeyError, ValueError) as exc:
        print(f"ERROR: {path}: {exc}", file=sys.stderr)
        return False
    print(f"OK: {path.name}: ZIP intact; {EXPECTED} present and AArch64 ELF")
    return True


def main() -> int:
    paths = [Path(arg) for arg in sys.argv[1:]]
    if not paths:
        print("Usage: verify-android-apks.py APK [APK ...]", file=sys.stderr)
        return 2
    success = True
    for path in paths:
        success = verify(path) and success
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
