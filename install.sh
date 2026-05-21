#!/usr/bin/env sh
# Teleia installer — clones the repo, builds with cargo, drops `teleia` into $PREFIX.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/foolish-dev/Teleia/dev/install.sh | sh
# Overrides:
#   PREFIX=/usr/local/bin   # default: $HOME/.local/bin
#   BRANCH=main             # default: dev

set -eu

PREFIX="${PREFIX:-$HOME/.local/bin}"
BRANCH="${BRANCH:-dev}"
REPO="https://github.com/foolish-dev/Teleia.git"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'error: %s not found in PATH\n%s\n' "$1" "$2" >&2
        exit 1
    }
}

need cargo "install Rust via https://rustup.rs"
need git   "install git via your package manager"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

echo "fetching Teleia ($BRANCH)..."
git clone --depth 1 --branch "$BRANCH" "$REPO" "$TMP/Teleia"

echo "building..."
(cd "$TMP/Teleia" && cargo build --release --bin teleia)

mkdir -p "$PREFIX"
install -m 0755 "$TMP/Teleia/target/release/teleia" "$PREFIX/teleia"

echo "installed: $PREFIX/teleia"

case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) printf 'note: %s is not on your PATH\n' "$PREFIX" ;;
esac
