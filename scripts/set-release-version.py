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
    parser = argparse.ArgumentParser(description="Set a Rust CLI calendar release version")
    parser.add_argument("version")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--package", required=True)
    parser.add_argument("--version-file", default="VERSION")
    args = parser.parse_args()
    match = PUBLIC_VERSION.fullmatch(args.version)
    if match is None:
        raise SystemExit("version must use YYYY.MM.DD.N with a positive release number")
    month = int(match.group("month"))
    day = int(match.group("day"))
    if not 1 <= month <= 12 or not 1 <= day <= 31:
        raise SystemExit("version contains an invalid month or day")
    cargo_version = f"{match.group('year')}.{month}.{day}-{match.group('release')}"

    root = args.root.resolve()
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    version_path = root / args.version_file
    manifest_path.write_text(
        replace_once(
            manifest_path.read_text(),
            r'(^\[package\][\s\S]*?^version\s*=\s*)"[^"]+"',
            rf'\g<1>"{cargo_version}"',
            "Cargo.toml package version",
        )
    )
    lock_path.write_text(
        replace_once(
            lock_path.read_text(),
            rf'(^\[\[package\]\]\nname = "{re.escape(args.package)}"\nversion = )"[^"]+"',
            rf'\g<1>"{cargo_version}"',
            f"Cargo.lock {args.package} package version",
        )
    )
    desktop_manifest = root / "desktop" / "Cargo.toml"
    if desktop_manifest.exists():
        desktop_manifest.write_text(replace_once(desktop_manifest.read_text(),
            r'(^\[package\][\s\S]*?^version\s*=\s*)"[^"]+"',
            rf'\g<1>"{cargo_version}"', "desktop package version"))
        lock_path.write_text(replace_once(lock_path.read_text(),
            r'(^\[\[package\]\]\nname = "xfer-desktop"\nversion = )"[^"]+"',
            rf'\g<1>"{cargo_version}"', "desktop lockfile version"))
    version_path.write_text(f"{args.version}\n")
    print(cargo_version)


if __name__ == "__main__":
    main()
