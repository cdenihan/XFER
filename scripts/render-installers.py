#!/usr/bin/env python3

import argparse
import re
from pathlib import Path


SAFE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")
SAFE_REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def checked(value: str, description: str, pattern: re.Pattern[str] = SAFE_NAME) -> str:
    if not pattern.fullmatch(value):
        raise SystemExit(f"invalid {description}: {value!r}")
    return value


def main() -> None:
    parser = argparse.ArgumentParser(description="Render branded Rust CLI installers")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--display-name", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--environment-prefix", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    values = {
        "__BINARY_NAME__": checked(args.binary, "binary name"),
        "__DISPLAY_NAME__": args.display_name.strip(),
        "__REPOSITORY__": checked(args.repository, "repository", SAFE_REPOSITORY),
        "__ENVIRONMENT_PREFIX__": checked(
            args.environment_prefix, "environment prefix"
        ),
    }
    if not values["__DISPLAY_NAME__"] or "\n" in values["__DISPLAY_NAME__"]:
        raise SystemExit("display name must be a non-empty single line")

    root = Path(__file__).resolve().parent.parent
    args.output.mkdir(parents=True, exist_ok=True)
    for name in ("install.sh", "install.ps1"):
        template = (root / "templates" / f"{name}.tmpl").read_text()
        for marker, value in values.items():
            template = template.replace(marker, value)
        if "__" in template:
            raise SystemExit(f"unresolved template marker in {name}")
        destination = args.output / name
        destination.write_text(template)
        if name.endswith(".sh"):
            destination.chmod(0o755)


if __name__ == "__main__":
    main()
