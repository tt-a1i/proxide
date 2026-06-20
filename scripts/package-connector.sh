#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
MANIFEST="$REPO_ROOT/connector-rs/Cargo.toml"
DEFAULT_OUT_DIR="$REPO_ROOT/dist"

usage() {
  cat <<'EOF'
Usage: scripts/package-connector.sh [--out-dir DIR]

Build a release binary and create a distributable connector package containing:
  - bin/codex-connector release binary
  - skills/codex-web-bridge/
  - connector-rs source, README, and example config
  - connector/ Python protocol reference tests
  - core README/docs and helper scripts

The package is written as:
  DIR/codex-web-bridge-connector-<version>-<os>-<arch>.tar.gz
  DIR/codex-web-bridge-connector-<version>-<os>-<arch>.tar.gz.sha256

Environment:
  OUT_DIR    Output directory, equivalent to --out-dir.
EOF
}

OUT_DIR="${OUT_DIR:-$DEFAULT_OUT_DIR}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir)
      if [[ $# -lt 2 ]]; then
        echo "package-connector: --out-dir requires a directory" >&2
        exit 2
      fi
      OUT_DIR="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "package-connector: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -f "$MANIFEST" ]]; then
  echo "package-connector: missing Rust connector manifest: $MANIFEST" >&2
  exit 1
fi

if [[ ! -x "$(command -v cargo)" ]]; then
  echo "package-connector: cargo is required" >&2
  exit 1
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$MANIFEST" | head -1)"
if [[ -z "$version" ]]; then
  echo "package-connector: could not read version from $MANIFEST" >&2
  exit 1
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
esac

package_name="codex-web-bridge-connector-${version}-${os}-${arch}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-package.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
stage="$tmp_dir/$package_name"

cd "$REPO_ROOT"
cargo build --release --manifest-path "$MANIFEST"

mkdir -p "$stage/bin" "$stage/connector-rs" "$stage/scripts" "$stage/docs" "$stage/skills"
install -m 0755 "$REPO_ROOT/connector-rs/target/release/codex-connector" "$stage/bin/codex-connector"
cp "$REPO_ROOT/README.md" "$REPO_ROOT/LICENSE" "$REPO_ROOT/CHANGELOG.md" "$REPO_ROOT/CONTRIBUTING.md" "$REPO_ROOT/SECURITY.md" "$REPO_ROOT/FAQ_ZH.md" "$stage/"
cp "$REPO_ROOT/connector-rs/Cargo.toml" "$REPO_ROOT/connector-rs/Cargo.lock" "$REPO_ROOT/connector-rs/README.md" "$REPO_ROOT/connector-rs/connector.example.json" "$stage/connector-rs/"
cp -R "$REPO_ROOT/connector-rs/src" "$stage/connector-rs/src"
cp -R "$REPO_ROOT/connector" "$stage/connector"
cp -R "$REPO_ROOT/skills/codex-web-bridge" "$stage/skills/codex-web-bridge"
cp "$REPO_ROOT/scripts/install-connector.sh" "$REPO_ROOT/scripts/install-release.sh" "$REPO_ROOT/scripts/verify-release.sh" "$REPO_ROOT/scripts/package-connector.sh" "$stage/scripts/"
cp "$REPO_ROOT/docs/devspace-parity-roadmap.md" "$REPO_ROOT/docs/release.md" "$stage/docs/"

find "$stage" \( -name '.git' -o -name 'target' -o -name '.codex' -o -name '.codex-web-bridge' -o -name '__pycache__' \) -prune -exec rm -rf {} +
find "$stage" \( \
  -name '*.pyc' \
  -o -name '.env' \
  -o -name '.env.*' \
  -o -name '*.pem' \
  -o -name '*.key' \
  -o -name '*.p12' \
  -o -name '*.pfx' \
  -o -name 'id_rsa' \
  -o -name 'id_dsa' \
  -o -name 'id_ecdsa' \
  -o -name 'id_ed25519' \
  -o -name 'secrets.*' \
  -o -name 'credentials.*' \
  -o -name '*.local.json' \
  -o -name 'connector.local.json' \
  -o -name 'oauth_tokens.json' \
  -o -name 'oauth_owner.local.json' \
\) -delete

cat > "$stage/PACKAGE-MANIFEST.json" <<EOF
{
  "name": "codex-web-bridge-connector",
  "version": "$version",
  "os": "$os",
  "arch": "$arch",
  "binary": "bin/codex-connector",
  "skill": "skills/codex-web-bridge/SKILL.md",
  "mcp_server": "connector-rs",
  "python_reference": "connector",
  "created_by": "scripts/package-connector.sh"
}
EOF

mkdir -p "$OUT_DIR"
tarball="$OUT_DIR/$package_name.tar.gz"
tar -C "$tmp_dir" -czf "$tarball" "$package_name"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$OUT_DIR" && sha256sum "$(basename "$tarball")" > "$(basename "$tarball").sha256")
else
  (cd "$OUT_DIR" && shasum -a 256 "$(basename "$tarball")" > "$(basename "$tarball").sha256")
fi

"$stage/bin/codex-connector" --help >/dev/null

cat <<EOF
package: $tarball
checksum: $tarball.sha256
EOF
