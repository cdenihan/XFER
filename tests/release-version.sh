#!/bin/sh

set -eu

script="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/scripts/next-release-version.sh"
repository="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

assert_version() {
    expected=$1
    references=$2
    actual=$(printf '%s' "$references" | sh "$script" 2026.07.16)
    if [ "$actual" != "$expected" ]; then
        printf '%s\n' "expected $expected, got $actual" >&2
        exit 1
    fi
}

assert_version "2026.07.16.1" ""
assert_version "2026.07.16.2" "refs/tags/v2026.07.16.1"
assert_version "2026.07.16.4" "refs/tags/v2026.07.16.1
refs/tags/v2026.07.16.3
refs/tags/v2026.07.15.99
refs/tags/v2026.07.16.not-a-number"

if sh "$script" invalid-date >/dev/null 2>&1; then
    printf '%s\n' "invalid dates must be rejected" >&2
    exit 1
fi

fixture=$(mktemp -d "${TMPDIR:-/tmp}/xfer-release-version.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
cp "$repository/Cargo.toml" "$fixture/Cargo.toml"
cp "$repository/Cargo.lock" "$fixture/Cargo.lock"
cp "$repository/VERSION" "$fixture/VERSION"

cargo_version=$(
    python3 "$repository/scripts/set-release-version.py" \
        2026.07.16.12 --root "$fixture"
)
assert_equal() {
    expected=$1
    actual=$2
    label=$3
    if [ "$actual" != "$expected" ]; then
        printf '%s\n' "$label: expected $expected, got $actual" >&2
        exit 1
    fi
}
assert_equal "2026.7.16-12" "$cargo_version" "Cargo version"
assert_equal \
    'version = "2026.7.16-12"' \
    "$(sed -n '3p' "$fixture/Cargo.toml")" \
    "Cargo.toml version"
assert_equal \
    'version = "2026.7.16-12"' \
    "$(sed -n '/^name = "xfer"$/ { n; p; }' "$fixture/Cargo.lock")" \
    "Cargo.lock version"
assert_equal "2026.07.16.12" "$(cat "$fixture/VERSION")" "public version"

if python3 "$repository/scripts/set-release-version.py" \
    2026.07.16.0 --root "$fixture" >/dev/null 2>&1; then
    printf '%s\n' "zero release numbers must be rejected" >&2
    exit 1
fi

printf '%s\n' "release version tests passed"
