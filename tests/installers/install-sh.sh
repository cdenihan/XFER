#!/bin/sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
XFER_INSTALLER_SOURCE_ONLY=1
export XFER_INSTALLER_SOURCE_ONLY
. "$ROOT/scripts/install.sh"

assert_equal() {
    expected=$1
    actual=$2
    description=$3
    if [ "$expected" != "$actual" ]; then
        printf '%s\n' "FAIL: $description: expected $expected, got $actual" >&2
        exit 1
    fi
}

assert_equal "xfer-linux-x86_64" "$(artifact_for Linux x86_64 gnu)" "Linux x86_64 GNU"
assert_equal "xfer-linux-x86_64-musl" "$(artifact_for Linux x86_64 musl)" "Linux x86_64 musl"
assert_equal "xfer-linux-aarch64" "$(artifact_for Linux aarch64 gnu)" "Linux ARM64 GNU"
assert_equal "xfer-linux-aarch64-musl" "$(artifact_for Linux aarch64 musl)" "Linux ARM64 musl"
assert_equal "xfer-macos-x86_64" "$(artifact_for Darwin x86_64 gnu)" "macOS Intel"
assert_equal "xfer-macos-aarch64" "$(artifact_for Darwin aarch64 gnu)" "macOS Apple Silicon"
assert_equal "v2026.07.16.42" "$(normalize_version 2026.07.16.42)" "version normalization"
assert_equal "v2026.07.16.42" "$(normalize_version v2026.07.16.42)" "tag preservation"

if artifact_for FreeBSD x86_64 gnu >/dev/null 2>&1; then
    printf '%s\n' "FAIL: unsupported OS was accepted" >&2
    exit 1
fi
if (
    download_file "http://example.invalid/xfer" "/tmp/xfer-should-not-exist"
) >/dev/null 2>&1; then
    printf '%s\n' "FAIL: insecure network URL was accepted" >&2
    exit 1
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/xfer-installer-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
fixture="$temporary/release"
install_dir="$temporary/install dir"
mkdir -p "$fixture/latest/download"

os=$(uname -s)
arch=$(normalize_arch "$(uname -m)")
libc_kind=gnu
if [ "$os" = Linux ]; then
    libc_kind=$(detect_libc)
fi
artifact=$(artifact_for "$os" "$arch" "$libc_kind")
fake_binary="$fixture/latest/download/$artifact"
cat >"$fake_binary" <<'EOF'
#!/bin/sh
printf '%s\n' "xfer 4.0.0"
EOF
chmod 0755 "$fake_binary"
checksum=$(sha256_file "$fake_binary")
printf '%s  %s\n' "$checksum" "$artifact" >"$fake_binary.sha256"

XFER_RELEASE_BASE_URL="file://$fixture" \
    XFER_INSTALL_DIR="$install_dir" \
    XFER_INSTALLER_SOURCE_ONLY=0 \
    sh "$ROOT/scripts/install.sh" >/dev/null

assert_equal "xfer 4.0.0" "$("$install_dir/xfer" --version)" "installed binary"

before=$(sha256_file "$install_dir/xfer")
printf '%064d  %s\n' 0 "$artifact" >"$fake_binary.sha256"
if XFER_RELEASE_BASE_URL="file://$fixture" \
    XFER_INSTALL_DIR="$install_dir" \
    XFER_INSTALLER_SOURCE_ONLY=0 \
    sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    printf '%s\n' "FAIL: checksum mismatch was accepted" >&2
    exit 1
fi
after=$(sha256_file "$install_dir/xfer")
assert_equal "$before" "$after" "failed install preserves existing binary"

mkdir -p "$fixture/download/v9.9.9"
cp "$fake_binary" "$fixture/download/v9.9.9/$artifact"
checksum=$(sha256_file "$fixture/download/v9.9.9/$artifact")
printf '%s  %s\n' "$checksum" "$artifact" >"$fixture/download/v9.9.9/$artifact.sha256"
if XFER_RELEASE_BASE_URL="file://$fixture" \
    XFER_INSTALL_DIR="$temporary/version-mismatch" \
    XFER_INSTALLER_SOURCE_ONLY=0 \
    sh "$ROOT/scripts/install.sh" --version v9.9.9 >/dev/null 2>&1; then
    printf '%s\n' "FAIL: mismatched pinned binary version was accepted" >&2
    exit 1
fi

sh -n "$ROOT/scripts/install.sh"
printf '%s\n' "install.sh tests passed"
