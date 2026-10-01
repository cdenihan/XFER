#!/bin/sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TEMPORARY=$(mktemp -d "${TMPDIR:-/tmp}/xfer-installer.XXXXXX")
trap 'rm -rf "$TEMPORARY"' EXIT HUP INT TERM

python3 "$ROOT/scripts/render-installers.py" \
    --binary fixture \
    --display-name Fixture \
    --repository owner/fixture \
    --environment-prefix FIXTURE \
    --output "$TEMPORARY/rendered"

XFER_INSTALLER_SOURCE_ONLY=1
export XFER_INSTALLER_SOURCE_ONLY
. "$TEMPORARY/rendered/install.sh"

assert_equal() {
    expected=$1
    actual=$2
    message=$3
    [ "$expected" = "$actual" ] || {
        printf '%s\n' "FAIL: $message: expected '$expected', got '$actual'" >&2
        exit 1
    }
}

assert_equal fixture-linux-x86_64 "$(artifact_for Linux x86_64 gnu)" "Linux GNU"
assert_equal fixture-linux-x86_64-musl "$(artifact_for Linux x86_64 musl)" "Linux musl"
assert_equal fixture-linux-x86_64-musl "$(artifact_for Linux x86_64)" "Linux default"
assert_equal fixture-linux-aarch64 "$(artifact_for Linux aarch64 gnu)" "Linux ARM64"
assert_equal fixture-linux-aarch64-musl "$(artifact_for Linux aarch64 musl)" "Linux ARM64 musl"
assert_equal fixture-macos-x86_64 "$(artifact_for Darwin x86_64 gnu)" "macOS Intel"
assert_equal fixture-macos-aarch64 "$(artifact_for Darwin aarch64 gnu)" "macOS ARM64"

if (download_file http://example.invalid/file "$TEMPORARY/forbidden") >/dev/null 2>&1; then
    printf '%s\n' "FAIL: insecure URL was accepted" >&2
    exit 1
fi

# Exercise wget-only downloads without making network requests.
wget() {
    case "$*" in
        *--max-redirect=0*) ;;
        *) return 99 ;;
    esac
    while [ "$1" != '-O' ]; do shift; done
    wget_destination=$2
    wget_url=$3
    printf '%s\n' "$wget_url" >> "$TEMPORARY/wget-calls"
    case "$wget_url" in
        https://example.invalid/start|https://example.invalid/start*\#old)
            printf '  HTTP/1.1 302 Found\n  Location: %s\n' "$WGET_REDIRECT" >&2
            return 8 ;;
        https://example.invalid/final)
            printf 'verified artifact' > "$wget_destination" ;;
        *) return 9 ;;
    esac
}
WGET_REDIRECT=/final
wget_download https://example.invalid/start "$TEMPORARY/wget-result"
assert_equal 'verified artifact' "$(cat "$TEMPORARY/wget-result")" 'wget HTTPS redirect'
assert_equal '2' "$(wc -l < "$TEMPORARY/wget-calls" | tr -d ' ')" 'wget redirect hop count'
WGET_REDIRECT='#checksum'
if (wget_download 'https://example.invalid/start?download=1#old' "$TEMPORARY/fragment") >/dev/null 2>&1; then
    printf '%s\n' 'FAIL: unexpected success from fragment redirect fixture' >&2
    exit 1
fi
assert_equal 'https://example.invalid/start?download=1#checksum' \
    "$(tail -n 1 "$TEMPORARY/wget-calls")" 'wget fragment redirect resolution'
WGET_REDIRECT='?download=2'
if (wget_download 'https://example.invalid/start?download=1#old' "$TEMPORARY/query") >/dev/null 2>&1; then
    printf '%s\n' 'FAIL: unexpected success from query redirect fixture' >&2
    exit 1
fi
assert_equal 'https://example.invalid/start?download=2' \
    "$(tail -n 1 "$TEMPORARY/wget-calls")" 'wget query redirect resolution'
for WGET_REDIRECT in http://example.invalid/final ftp://example.invalid/final 'https://example.invalid\evil'; do
    if (wget_download https://example.invalid/start "$TEMPORARY/forbidden") >/dev/null 2>&1; then
        printf '%s\n' 'FAIL: invalid wget redirect accepted' >&2
        exit 1
    fi
done
unset -f wget

mkdir -p "$TEMPORARY/release/latest/download" "$TEMPORARY/install"
artifact=$(artifact_for "$(uname -s)" "$(normalize_arch "$(uname -m)")" musl)
binary="$TEMPORARY/release/latest/download/$artifact"
printf '%s\n' '#!/bin/sh' "printf 'fixture 9.9.9\\n'" > "$binary"
chmod 0755 "$binary"
checksum=$(sha256_file "$binary")
printf '%s  %s\n' "$checksum" "$artifact" > "$binary.sha256"

FIXTURE_RELEASE_BASE_URL="file://$TEMPORARY/release" \
FIXTURE_INSTALL_DIR="$TEMPORARY/install" \
XFER_INSTALLER_SOURCE_ONLY=0 \
sh "$TEMPORARY/rendered/install.sh" >/dev/null
assert_equal "fixture 9.9.9" "$("$TEMPORARY/install/fixture" --version)" "installed binary"

mkdir -p "$TEMPORARY/fake-linux-bin"
printf '%s\n' '#!/bin/sh' \
    'case "${1:-}" in -m) printf "x86_64\\n" ;; *) printf "Linux\\n" ;; esac' \
    > "$TEMPORARY/fake-linux-bin/uname"
chmod 0755 "$TEMPORARY/fake-linux-bin/uname"

linux_musl_artifact=$(artifact_for Linux x86_64 musl)
linux_musl_binary="$TEMPORARY/release/latest/download/$linux_musl_artifact"
printf '%s\n' '#!/bin/sh' "printf 'fixture 7.7.7\\n'" > "$linux_musl_binary"
chmod 0755 "$linux_musl_binary"
linux_musl_checksum=$(sha256_file "$linux_musl_binary")
printf '%s  %s\n' "$linux_musl_checksum" "$linux_musl_artifact" \
    > "$linux_musl_binary.sha256"

gnu_artifact=$(artifact_for Linux x86_64 gnu)
gnu_binary="$TEMPORARY/release/latest/download/$gnu_artifact"
printf '%s\n' '#!/bin/sh' "printf 'fixture 8.8.8\\n'" > "$gnu_binary"
chmod 0755 "$gnu_binary"
gnu_checksum=$(sha256_file "$gnu_binary")
printf '%s  %s\n' "$gnu_checksum" "$gnu_artifact" > "$gnu_binary.sha256"

PATH="$TEMPORARY/fake-linux-bin:$PATH" \
FIXTURE_RELEASE_BASE_URL="file://$TEMPORARY/release" \
XFER_INSTALLER_SOURCE_ONLY=0 \
sh "$TEMPORARY/rendered/install.sh" \
    --install-dir "$TEMPORARY/install-linux-default" >/dev/null
assert_equal "fixture 7.7.7" \
    "$("$TEMPORARY/install-linux-default/fixture" --version)" "default musl binary"

PATH="$TEMPORARY/fake-linux-bin:$PATH" \
FIXTURE_RELEASE_BASE_URL="file://$TEMPORARY/release" \
XFER_INSTALLER_SOURCE_ONLY=0 \
sh "$TEMPORARY/rendered/install.sh" \
    --libc gnu --install-dir "$TEMPORARY/install-gnu" >/dev/null
assert_equal "fixture 8.8.8" \
    "$("$TEMPORARY/install-gnu/fixture" --version)" "explicit GNU binary"

PATH="$TEMPORARY/fake-linux-bin:$PATH" \
FIXTURE_RELEASE_BASE_URL="file://$TEMPORARY/release" \
FIXTURE_INSTALL_DIR="$TEMPORARY/install-gnu-env" \
FIXTURE_LIBC=gnu \
XFER_INSTALLER_SOURCE_ONLY=0 \
sh "$TEMPORARY/rendered/install.sh" >/dev/null
assert_equal "fixture 8.8.8" \
    "$("$TEMPORARY/install-gnu-env/fixture" --version)" "GNU environment override"

if XFER_INSTALLER_SOURCE_ONLY=0 \
    sh "$TEMPORARY/rendered/install.sh" --libc glibc >/dev/null 2>&1; then
    printf '%s\n' "FAIL: invalid libc was accepted" >&2
    exit 1
fi

before=$(sha256_file "$TEMPORARY/install/fixture")
printf '%064d  %s\n' 0 "$artifact" > "$binary.sha256"
if FIXTURE_RELEASE_BASE_URL="file://$TEMPORARY/release" \
    FIXTURE_INSTALL_DIR="$TEMPORARY/install" \
    XFER_INSTALLER_SOURCE_ONLY=0 \
    sh "$TEMPORARY/rendered/install.sh" >/dev/null 2>&1; then
    printf '%s\n' "FAIL: invalid checksum was accepted" >&2
    exit 1
fi
assert_equal "$before" "$(sha256_file "$TEMPORARY/install/fixture")" "failed update preservation"
sh -n "$TEMPORARY/rendered/install.sh"
printf '%s\n' "installer tests passed"
