#!/usr/bin/env sh
# Telia (τέλεια) installer — clones the repo, builds with cargo, drops
# `telia` into $PREFIX. The GitHub repository is still named "Teleia";
# only the binary and crate names are short-form Telia.
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

echo "fetching τέλεια ($BRANCH)..."
git clone --depth 1 --branch "$BRANCH" "$REPO" "$TMP/telia"

echo "building..."
(cd "$TMP/telia" && cargo build --release --bin telia)

mkdir -p "$PREFIX"
install -m 0755 "$TMP/telia/target/release/telia" "$PREFIX/telia"

echo "installed: $PREFIX/telia"

case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) printf 'note: %s is not on your PATH\n' "$PREFIX" ;;
esac
