#!/bin/sh

set -eu

PROGRAM="xfer"
DEFAULT_REPOSITORY="cdenihan/XFER"

usage() {
    cat <<'EOF'
Install XFER on Linux or macOS.

Usage:
  install.sh [--version VERSION] [--install-dir DIRECTORY]

Options:
  --version VERSION        Release tag, such as v2026.07.16.42 (default: latest)
  --install-dir DIRECTORY  Destination directory (default: $HOME/.local/bin)
  -h, --help               Show this help

Environment:
  XFER_VERSION             Alternative to --version
  XFER_INSTALL_DIR         Alternative to --install-dir
  XFER_REPOSITORY          GitHub owner/repository (default: cdenihan/XFER)
  XFER_RELEASE_BASE_URL    Release base URL for mirrors or testing
EOF
}

log() {
    printf '%s\n' "xfer-install: $*"
}

die() {
    printf '%s\n' "xfer-install: error: $*" >&2
    exit 1
}

normalize_version() {
    case "$1" in
        latest) printf '%s\n' "latest" ;;
        v*) printf '%s\n' "$1" ;;
        *) printf 'v%s\n' "$1" ;;
    esac
}

normalize_arch() {
    case "$1" in
        x86_64 | amd64) printf '%s\n' "x86_64" ;;
        aarch64 | arm64) printf '%s\n' "aarch64" ;;
        *) return 1 ;;
    esac
}

detect_libc() {
    if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
        printf '%s\n' "musl"
        return
    fi
    for loader in /lib/ld-musl-*.so.1 /usr/lib/ld-musl-*.so.1; do
        if [ -e "$loader" ]; then
            printf '%s\n' "musl"
            return
        fi
    done
    printf '%s\n' "gnu"
}

artifact_for() {
    os=$1
    arch=$2
    libc_kind=${3:-gnu}

    case "$os:$arch:$libc_kind" in
        Linux:x86_64:gnu) printf '%s\n' "xfer-linux-x86_64" ;;
        Linux:x86_64:musl) printf '%s\n' "xfer-linux-x86_64-musl" ;;
        Linux:aarch64:gnu) printf '%s\n' "xfer-linux-aarch64" ;;
        Linux:aarch64:musl) printf '%s\n' "xfer-linux-aarch64-musl" ;;
        Darwin:x86_64:*) printf '%s\n' "xfer-macos-x86_64" ;;
        Darwin:aarch64:*) printf '%s\n' "xfer-macos-aarch64" ;;
        *) return 1 ;;
    esac
}

download_file() {
    url=$1
    destination=$2

    case "$url" in
        file://*)
            cp "${url#file://}" "$destination"
            return
            ;;
        https://*) ;;
        *) die "refusing non-HTTPS download URL: $url" ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error \
            --retry 3 --proto '=https' --proto-redir '=https' --tlsv1.2 \
            --output "$destination" "$url"
        return
    fi
    if command -v wget >/dev/null 2>&1; then
        wget -q -O "$destination" "$url"
        return
    fi
    die "curl or wget is required"
}

sha256_file() {
    path=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
        return
    fi
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{print $1}'
        return
    fi
    if command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$path" | awk '{print $NF}'
        return
    fi
    die "sha256sum, shasum, or openssl is required for checksum verification"
}

verify_checksum() {
    artifact=$1
    checksum_file=$2
    expected=$(awk 'NR == 1 {print $1}' "$checksum_file")
    if ! printf '%s\n' "$expected" | grep -Eq '^[0-9A-Fa-f]{64}$'; then
        die "release checksum file is malformed"
    fi
    actual=$(sha256_file "$artifact")
    if [ "$(printf '%s' "$actual" | tr 'A-F' 'a-f')" != \
        "$(printf '%s' "$expected" | tr 'A-F' 'a-f')" ]; then
        die "SHA-256 checksum verification failed"
    fi
}

verify_binary_version() {
    binary=$1
    requested_version=$2
    reported=$("$binary" --version 2>/dev/null) ||
        die "downloaded binary could not run on this machine"
    case "$reported" in
        "xfer "*) ;;
        *) die "downloaded file did not identify itself as XFER" ;;
    esac
    if [ "$requested_version" != "latest" ] &&
        [ "$reported" != "xfer ${requested_version#v}" ]; then
        die "downloaded binary version does not match requested release $requested_version"
    fi
}

main() {
    version=${XFER_VERSION:-latest}
    install_dir=${XFER_INSTALL_DIR:-"${HOME:?HOME is not set}/.local/bin"}

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version)
                [ "$#" -ge 2 ] || die "--version requires a value"
                version=$2
                shift 2
                ;;
            --install-dir)
                [ "$#" -ge 2 ] || die "--install-dir requires a value"
                install_dir=$2
                shift 2
                ;;
            -h | --help)
                usage
                return
                ;;
            *)
                die "unknown option: $1"
                ;;
        esac
    done

    version=$(normalize_version "$version")
    repository=${XFER_REPOSITORY:-$DEFAULT_REPOSITORY}
    release_base=${XFER_RELEASE_BASE_URL:-"https://github.com/$repository/releases"}
    os=$(uname -s)
    arch=$(normalize_arch "$(uname -m)") ||
        die "unsupported CPU architecture: $(uname -m)"
    libc_kind="gnu"
    if [ "$os" = "Linux" ]; then
        libc_kind=$(detect_libc)
    fi
    artifact=$(artifact_for "$os" "$arch" "$libc_kind") ||
        die "unsupported platform: $os/$arch/$libc_kind"

    if [ "$version" = "latest" ]; then
        download_base="$release_base/latest/download"
    else
        download_base="$release_base/download/$version"
    fi

    temporary=$(mktemp -d "${TMPDIR:-/tmp}/xfer-install.XXXXXX") ||
        die "could not create a temporary directory"
    staging=""
    trap 'rm -rf "$temporary"; if [ -n "$staging" ]; then rm -f "$staging"; fi' EXIT HUP INT TERM

    binary_path="$temporary/$artifact"
    checksum_path="$temporary/$artifact.sha256"
    log "downloading $artifact ($version)"
    download_file "$download_base/$artifact" "$binary_path"
    download_file "$download_base/$artifact.sha256" "$checksum_path"
    verify_checksum "$binary_path" "$checksum_path"
    chmod 0755 "$binary_path"
    verify_binary_version "$binary_path" "$version"

    mkdir -p "$install_dir" ||
        die "could not create install directory: $install_dir"
    staging="$install_dir/.xfer-install.$$"
    cp "$binary_path" "$staging" ||
        die "could not write to install directory: $install_dir"
    chmod 0755 "$staging"
    mv -f "$staging" "$install_dir/$PROGRAM"
    staging=""

    log "installed $("$install_dir/$PROGRAM" --version) to $install_dir/$PROGRAM"
    case ":${PATH:-}:" in
        *":$install_dir:"*) ;;
        *)
            log "add $install_dir to PATH to run xfer from any directory"
            ;;
    esac
}

if [ "${XFER_INSTALLER_SOURCE_ONLY:-0}" != "1" ]; then
    main "$@"
fi
