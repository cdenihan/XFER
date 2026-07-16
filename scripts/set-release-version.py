#!/usr/bin/env python3

import argparse
import re
from pathlib import Path


PUBLIC_VERSION = re.compile(
    r"^(?P<year>[0-9]{4})\.(?P<month>[0-9]{2})\."
    r"(?P<day>[0-9]{2})\.(?P<release>[1-9][0-9]*)$"
)


def replace_once(contents: str, pattern: str, replacement: str, name: str) -> str:
    updated, count = re.subn(pattern, replacement, contents, count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"could not update {name}")
    return updated


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Set XFER's public and Cargo package release versions."
    )
    parser.add_argument("version", help="Public release version in YYYY.MM.DD.N form")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help="Repository root (default: current directory)",
    )
    args = parser.parse_args()

    match = PUBLIC_VERSION.fullmatch(args.version)
    if match is None:
        raise SystemExit("version must use YYYY.MM.DD.N with a positive release number")

    month = int(match.group("month"))
    day = int(match.group("day"))
    if not 1 <= month <= 12 or not 1 <= day <= 31:
        raise SystemExit("version contains an invalid month or day")

    cargo_version = (
        f"{match.group('year')}.{month}.{day}-{match.group('release')}"
    )
    root = args.root.resolve()
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    version_path = root / "VERSION"

    manifest = manifest_path.read_text()
    manifest = replace_once(
        manifest,
        r'(^\[package\][\s\S]*?^version\s*=\s*)"[^"]+"',
        rf'\g<1>"{cargo_version}"',
        "Cargo.toml package version",
    )
    manifest_path.write_text(manifest)

    lock = lock_path.read_text()
    lock = replace_once(
        lock,
        r'(^\[\[package\]\]\nname = "xfer"\nversion = )"[^"]+"',
        rf'\g<1>"{cargo_version}"',
        "Cargo.lock xfer package version",
    )
    lock_path.write_text(lock)
    version_path.write_text(f"{args.version}\n")
    print(cargo_version)


if __name__ == "__main__":
    main()
