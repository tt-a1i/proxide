#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
MANIFEST="$REPO_ROOT/connector-rs/Cargo.toml"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
TARGET="$BIN_DIR/codex-connector"

usage() {
  cat <<'EOF'
Usage: scripts/install-connector.sh [--prefix DIR]

Build the Rust MCP connector in release mode and install the codex-connector
binary to DIR/bin. Defaults to ~/.local/bin.

Environment:
  PREFIX    Install prefix, equivalent to --prefix.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      if [[ $# -lt 2 ]]; then
        echo "install-connector: --prefix requires a directory" >&2
        exit 2
      fi
      PREFIX="$2"
      BIN_DIR="$PREFIX/bin"
      TARGET="$BIN_DIR/codex-connector"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "install-connector: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -f "$MANIFEST" ]]; then
  echo "install-connector: missing Rust connector manifest: $MANIFEST" >&2
  exit 1
fi

if [[ ! -x "$(command -v cargo)" ]]; then
  echo "install-connector: cargo is required" >&2
  exit 1
fi

cargo build --release --manifest-path "$MANIFEST"
mkdir -p "$BIN_DIR"
install -m 0755 "$REPO_ROOT/connector-rs/target/release/codex-connector" "$TARGET"
"$TARGET" --help >/dev/null

cat <<EOF
installed: $TARGET

Add this to PATH if needed:
  export PATH="$BIN_DIR:\$PATH"
EOF
