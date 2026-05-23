#!/usr/bin/env sh
# Teleia (τέλεια) installer — auto-detects platform, downloads the latest
# prebuilt binary from GitHub Releases, falls back to a cargo source
# build if no release exists for this platform.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/foolish-dev/teleia/dev/install.sh | sh
# Overrides:
#   PREFIX=/usr/local/bin   # default: $HOME/.local/bin
#   TAG=v0.2.0              # pin a release tag (default: latest)
#   FROM_SOURCE=1           # skip prebuilt download; cargo build instead
#   BRANCH=main             # source-build branch (default: dev)
#   NO_PATH=1               # don't touch the shell rc
#   NO_OLLAMA_HINT=1        # suppress the post-install Ollama nudge

set -eu

PREFIX="${PREFIX:-$HOME/.local/bin}"
BRANCH="${BRANCH:-dev}"
TAG="${TAG:-latest}"
REPO="https://github.com/foolish-dev/teleia"

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
    esac
    return 1
}

try_prebuilt() {
    target="$1"
    command -v curl >/dev/null 2>&1 || return 1
    if [ "$TAG" = "latest" ]; then
        url="$REPO/releases/latest/download/teleia-$target"
    else
        url="$REPO/releases/download/$TAG/teleia-$target"
    fi
    echo "fetching τέλεια binary ($target)..."
    curl -fsSL --output "$TMP/teleia" "$url"
}

from_source() {
    need cargo "install Rust via https://rustup.rs"
    need git   "install git via your package manager"
    echo "fetching τέλεια source ($BRANCH)..."
    git clone --depth 1 --branch "$BRANCH" "$REPO.git" "$TMP/src"
    echo "building..."
    (cd "$TMP/src" && cargo build --release --bin teleia)
    cp "$TMP/src/target/release/teleia" "$TMP/teleia"
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
chmod +x "$TMP/teleia"
mv "$TMP/teleia" "$PREFIX/teleia"
echo "installed: $PREFIX/teleia"

# --- automatic setup -----------------------------------------------------

# verify: catch arch / libc / linker mismatches early.
if ! "$PREFIX/teleia" --version >/dev/null 2>&1; then
    echo "warn: '$PREFIX/teleia --version' didn't run cleanly; binary may not match this platform" >&2
fi

# PATH setup: append a sourceable line to the user's shell rc, idempotent
# via a marker comment. NO_PATH=1 to opt out.
shell_rc() {
    case "${SHELL##*/}" in
        bash) [ "$(uname -s)" = "Darwin" ] && [ -f "$HOME/.bash_profile" ] \
                && echo "$HOME/.bash_profile" || echo "$HOME/.bashrc" ;;
        zsh)  echo "$HOME/.zshrc" ;;
        fish) echo "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ;;
        *)    echo "" ;;
    esac
}

case ":$PATH:" in *":$PREFIX:"*) on_path=1 ;; *) on_path=0 ;; esac
if [ "$on_path" = "0" ] && [ "${NO_PATH:-0}" != "1" ]; then
    rc="$(shell_rc)"
    if [ -z "$rc" ]; then
        printf 'note: %s is not on your PATH and your shell is unrecognized; add it manually\n' "$PREFIX"
    elif [ -f "$rc" ] && grep -Fq 'teleia install: added to PATH' "$rc"; then
        echo "PATH already set in $rc — restart your shell or 'source $rc'"
    else
        mkdir -p "$(dirname "$rc")"
        case "${SHELL##*/}" in
            fish) line="set -gx PATH \"$PREFIX\" \$PATH" ;;
            *)    line="export PATH=\"$PREFIX:\$PATH\"" ;;
        esac
        printf '\n# teleia install: added to PATH\n%s\n' "$line" >> "$rc"
        echo "added $PREFIX to PATH in $rc — restart your shell or 'source $rc'"
    fi
fi

# Ollama hint: teleia's default model is local; nudge toward setup.
if [ "${NO_OLLAMA_HINT:-0}" != "1" ]; then
    if command -v ollama >/dev/null 2>&1; then
        echo "next: 'ollama pull hf.co/FoolDev/Thanatos-27B:Q4_K_M' to grab the default local model, or run 'teleia --model claude-opus-4-7'"
    else
        echo "next: install Ollama (https://ollama.com) for local models, or run 'teleia --model claude-opus-4-7'"
    fi
fi
