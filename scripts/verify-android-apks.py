#!/usr/bin/env python3
"""Validate APK ZIP integrity, the packaged arm64-v8a Agate native library and APK signatures.

Signature checks run only when --apksigner is given:
- APKs under a "debug" path component must carry exactly the shared debug
  keystore fingerprint (guards against accidental keystore regeneration,
  which would silently break signature-matched reinstalls);
- other APKs are release builds: unsigned passes while release signing is
  not configured, and once --release-cert-sha256 is provided the release
  certificate fingerprint is asserted.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import zipfile
from pathlib import Path

EXPECTED = "lib/arm64-v8a/libagate_server.so"
ELF_MAGIC = b"\x7fELF"

# app/agate-debug.jks (alias agate, committed to the repo) is a shared debug
# credential, NOT a production key: its fingerprint is public by design and
# exists so local and CI debug builds reinstall over each other without
# losing data. Asserting it here catches keystore regeneration.
DEBUG_CERT_SHA256 = "372d51dc45dc4a73c329065f4301a066384fcaa8fbff1316217e6d1664ca0ed3"

_CERT_DIGEST_RE = re.compile(r"certificate SHA-256 digest:\s*([0-9A-Fa-f:]+)", re.IGNORECASE)


def _normalize_fingerprint(raw: str) -> str:
    return raw.strip().replace(":", "").lower()


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


def signature_check(apksigner: str, path: Path, release_cert_sha256: str | None) -> bool:
    is_debug = "debug" in path.parts
    try:
        proc = subprocess.run(
            [apksigner, "verify", "--print-certs", str(path)],
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        print(f"ERROR: {path.name}: cannot run apksigner {apksigner}: {exc}", file=sys.stderr)
        return False
    output = (proc.stdout + proc.stderr).strip()
    digests = {_normalize_fingerprint(raw) for raw in _CERT_DIGEST_RE.findall(output)}
    if proc.returncode != 0:
        if is_debug:
            print(f"ERROR: {path.name}: debug APK must be signed with the shared debug keystore:", file=sys.stderr)
            print(output, file=sys.stderr)
            return False
        if release_cert_sha256:
            print(f"ERROR: {path.name}: release APK expected to be signed (--release-cert-sha256 set):", file=sys.stderr)
            print(output, file=sys.stderr)
            return False
        print(f"OK: {path.name}: unsigned (release signing not configured yet)")
        return True
    if not digests:
        print(f"ERROR: {path.name}: apksigner verify passed but printed no SHA-256 digest", file=sys.stderr)
        return False
    if is_debug:
        if digests != {DEBUG_CERT_SHA256}:
            print(
                f"ERROR: {path.name}: debug certificate fingerprint mismatch: "
                f"got {sorted(digests)}, expected {DEBUG_CERT_SHA256}",
                file=sys.stderr,
            )
            return False
        print(f"OK: {path.name}: signed with the shared debug certificate")
        return True
    if release_cert_sha256 and digests != {_normalize_fingerprint(release_cert_sha256)}:
        print(
            f"ERROR: {path.name}: release certificate fingerprint mismatch: "
            f"got {sorted(digests)}, expected {release_cert_sha256}",
            file=sys.stderr,
        )
        return False
    for digest in sorted(digests):
        print(f"OK: {path.name}: release certificate SHA-256 {digest}")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate APK ZIP/native-library/signature invariants.")
    parser.add_argument("apks", nargs="+", type=Path, help="APK files; a 'debug' path component marks debug APKs")
    parser.add_argument("--apksigner", metavar="BIN", help="apksigner binary; enables signature checks")
    parser.add_argument(
        "--release-cert-sha256",
        metavar="FPR",
        help="expected release certificate SHA-256 (release APKs must then be signed)",
    )
    args = parser.parse_args()
    success = True
    for path in args.apks:
        success = verify(path) and success
        if args.apksigner:
            success = signature_check(args.apksigner, path, args.release_cert_sha256) and success
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
