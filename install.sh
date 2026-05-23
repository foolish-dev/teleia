#!/usr/bin/env sh
# Telia (τέλεια) installer — auto-detects platform, downloads the latest
# prebuilt binary from GitHub Releases, falls back to a cargo source
# build if no release exists for this platform.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | sh
# Overrides:
#   PREFIX=/usr/local/bin   # default: $HOME/.local/bin
#   TAG=v0.2.0              # pin a release tag (default: latest)
#   FROM_SOURCE=1           # skip prebuilt download; cargo build instead
#   BRANCH=main             # source-build branch (default: dev)

set -eu

PREFIX="${PREFIX:-$HOME/.local/bin}"
BRANCH="${BRANCH:-dev}"
TAG="${TAG:-latest}"
REPO="https://github.com/foolish-dev/telia"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

need() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'error: %s not found in PATH\n%s\n' "$1" "$2" >&2
        exit 1
    }
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64)       echo "x86_64-unknown-linux-musl";        return 0 ;;
                aarch64|arm64)      echo "aarch64-unknown-linux-musl";       return 0 ;;
                armv7l|armv7|armhf) echo "armv7-unknown-linux-musleabihf";   return 0 ;;
                i686|i386)          echo "i686-unknown-linux-musl";          return 0 ;;
                riscv64)            echo "riscv64gc-unknown-linux-gnu";      return 0 ;;
                ppc64le)            echo "powerpc64le-unknown-linux-gnu";    return 0 ;;
                s390x)              echo "s390x-unknown-linux-gnu";          return 0 ;;
            esac ;;
        Darwin)
            case "$arch" in
                x86_64)             echo "x86_64-apple-darwin";              return 0 ;;
                arm64|aarch64)      echo "aarch64-apple-darwin";             return 0 ;;
            esac ;;
        FreeBSD)
            case "$arch" in
                x86_64|amd64)       echo "x86_64-unknown-freebsd";           return 0 ;;
            esac ;;
        NetBSD)
            case "$arch" in
                x86_64|amd64)       echo "x86_64-unknown-netbsd";            return 0 ;;
            esac ;;
        SunOS)
            case "$arch" in
                i86pc|x86_64|amd64) echo "x86_64-unknown-illumos";           return 0 ;;
            esac ;;
    esac
    return 1
}

try_prebuilt() {
    target="$1"
    command -v curl >/dev/null 2>&1 || return 1
    if [ "$TAG" = "latest" ]; then
        url="$REPO/releases/latest/download/telia-$target"
    else
        url="$REPO/releases/download/$TAG/telia-$target"
    fi
    echo "fetching τέλεια binary ($target)..."
    curl -fsSL --output "$TMP/telia" "$url"
}

from_source() {
    need cargo "install Rust via https://rustup.rs"
    need git   "install git via your package manager"
    echo "fetching τέλεια source ($BRANCH)..."
    git clone --depth 1 --branch "$BRANCH" "$REPO.git" "$TMP/src"
    echo "building..."
    (cd "$TMP/src" && cargo build --release --bin telia)
    cp "$TMP/src/target/release/telia" "$TMP/telia"
}

if [ "${FROM_SOURCE:-0}" = "1" ]; then
    from_source
elif target="$(detect_target)" && try_prebuilt "$target" 2>/dev/null; then
    :
else
    echo "no prebuilt for this platform — falling back to source build"
    from_source
fi

mkdir -p "$PREFIX"
chmod +x "$TMP/telia"
mv "$TMP/telia" "$PREFIX/telia"
echo "installed: $PREFIX/telia"

case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) printf 'note: %s is not on your PATH\n' "$PREFIX" ;;
esac
