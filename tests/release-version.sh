#!/bin/sh

set -eu

script="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/scripts/next-release-version.sh"

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

printf '%s\n' "release version tests passed"
