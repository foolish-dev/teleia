#!/usr/bin/env bash
# install.sh — build available Teleia impls into $PREFIX (default ~/.local/bin).
# Skips any impl whose toolchain is missing. Idempotent; re-run to rebuild.
set -eu

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local/bin}"
mkdir -p "$PREFIX"

have() { command -v "$1" >/dev/null 2>&1; }
report() { printf '→ %-7s %s\n' "$1" "$2"; }

ok=0

build_rust() {
  if ! have cargo; then
    report rust 'skipped (cargo not in PATH)'
    return
  fi
  if ( cd "$REPO/rust" && cargo build --release --locked --quiet ); then
    cp -f "$REPO/rust/target/release/teleia" "$PREFIX/teleia-rust"
    chmod +x "$PREFIX/teleia-rust"
    report rust "✓ built $PREFIX/teleia-rust"
    ok=$((ok+1))
  else
    report rust '✗ build failed'
  fi
}

build_python() {
  if ! have python3; then
    report python 'skipped (python3 not in PATH)'
    return
  fi
  cat > "$PREFIX/teleia-python" <<EOF
#!/usr/bin/env bash
export PYTHONPATH="$REPO/python\${PYTHONPATH:+:\$PYTHONPATH}"
exec python3 -m teleia "\$@"
EOF
  chmod +x "$PREFIX/teleia-python"
  report python "✓ installed $PREFIX/teleia-python"
  ok=$((ok+1))
}

build_go() {
  if ! have go; then
    report go 'skipped (go not in PATH)'
    return
  fi
  if ( cd "$REPO/go" && go build -o "$PREFIX/teleia-go" ./cmd/teleia ); then
    report go "✓ built $PREFIX/teleia-go"
    ok=$((ok+1))
  else
    report go '✗ build failed'
  fi
}

build_lua() {
  local lua_bin=""
  if have lua5.4; then
    lua_bin=lua5.4
  elif have lua; then
    lua_bin=lua
  else
    report lua 'skipped (lua5.4 not in PATH)'
    return
  fi
  cat > "$PREFIX/teleia-lua" <<EOF
#!/usr/bin/env bash
exec $lua_bin "$REPO/lua/teleia.lua" "\$@"
EOF
  chmod +x "$PREFIX/teleia-lua"
  report lua "✓ installed $PREFIX/teleia-lua"
  ok=$((ok+1))
}

build_bun() {
  if ! have bun; then
    report bun 'skipped (bun not in PATH)'
    return
  fi
  if ( cd "$REPO/bun" && bun install --frozen-lockfile >/dev/null 2>&1 ); then
    cat > "$PREFIX/teleia-bun" <<EOF
#!/usr/bin/env bash
exec bun "$REPO/bun/src/index.ts" "\$@"
EOF
    chmod +x "$PREFIX/teleia-bun"
    report bun "✓ installed $PREFIX/teleia-bun"
    ok=$((ok+1))
  else
    report bun '✗ bun install failed'
  fi
}

build_rust
build_python
build_go
build_lua
build_bun

echo
echo "$ok/5 impls installed."

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo "Note: $PREFIX is not on PATH. Add it to your shell init.";;
esac
