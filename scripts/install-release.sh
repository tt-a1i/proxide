#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-tt-a1i/proxide}"
VERSION="${VERSION:-latest}"
PREFIX="${PREFIX:-$HOME/.local}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/share/codex-web-bridge/connector}"
RELEASE_BASE_URL="${RELEASE_BASE_URL:-}"
BIN_DIR="$PREFIX/bin"

usage() {
  cat <<'EOF'
Usage: scripts/install-release.sh [--repo OWNER/REPO] [--version TAG|latest] [--prefix DIR] [--install-dir DIR]

Download a published codex-web-bridge connector release package from GitHub,
verify its SHA-256 checksum, install codex-connector to DIR/bin, and keep the
unpacked connector package available for --skill-root.

Environment:
  REPO          GitHub repository, default tt-a1i/proxide.
  VERSION       Release tag or latest, default latest.
  PREFIX        Binary install prefix, default ~/.local.
  INSTALL_DIR   Directory for unpacked package files, default
                ~/.local/share/codex-web-bridge/connector.
  RELEASE_BASE_URL
                Override asset base URL for tests or mirrors.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      if [[ $# -lt 2 ]]; then
        echo "install-release: --repo requires OWNER/REPO" >&2
        exit 2
      fi
      REPO="$2"
      shift 2
      ;;
    --version)
      if [[ $# -lt 2 ]]; then
        echo "install-release: --version requires TAG or latest" >&2
        exit 2
      fi
      VERSION="$2"
      shift 2
      ;;
    --prefix)
      if [[ $# -lt 2 ]]; then
        echo "install-release: --prefix requires a directory" >&2
        exit 2
      fi
      PREFIX="$2"
      BIN_DIR="$PREFIX/bin"
      shift 2
      ;;
    --install-dir)
      if [[ $# -lt 2 ]]; then
        echo "install-release: --install-dir requires a directory" >&2
        exit 2
      fi
      INSTALL_DIR="$2"
      shift 2
      ;;
    --base-url)
      if [[ $# -lt 2 ]]; then
        echo "install-release: --base-url requires a URL" >&2
        exit 2
      fi
      RELEASE_BASE_URL="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "install-release: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$INSTALL_DIR" || "$INSTALL_DIR" == "/" ]]; then
  echo "install-release: refusing unsafe install directory: $INSTALL_DIR" >&2
  exit 1
fi

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "install-release: $1 is required" >&2
    exit 1
  fi
}

need curl
need tar

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
esac

if [[ -n "$RELEASE_BASE_URL" ]]; then
  base_url="${RELEASE_BASE_URL%/}"
elif [[ "$VERSION" == "latest" ]]; then
  base_url="https://github.com/$REPO/releases/latest/download"
else
  base_url="https://github.com/$REPO/releases/download/$VERSION"
fi

asset_prefix="codex-web-bridge-connector-"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-release.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

download() {
  local url="$1"
  local output="$2"
  curl -fsSL "$url" -o "$output"
}

if [[ "$VERSION" == "latest" && -z "$RELEASE_BASE_URL" ]]; then
  latest_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")"
  tag="${latest_url##*/}"
  if [[ -z "${tag:-}" ]]; then
    echo "install-release: could not resolve latest release tag" >&2
    exit 1
  fi
  VERSION="$tag"
  base_url="https://github.com/$REPO/releases/download/$VERSION"
fi

tarball_name="${asset_prefix}${VERSION#v}-${os}-${arch}.tar.gz"
checksum_name="$tarball_name.sha256"
tarball="$tmp_dir/$tarball_name"
checksum="$tmp_dir/$checksum_name"

download "$base_url/$tarball_name" "$tarball"
download "$base_url/$checksum_name" "$checksum"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp_dir" && sha256sum -c "$checksum_name" >/dev/null)
else
  (cd "$tmp_dir" && shasum -a 256 -c "$checksum_name" >/dev/null)
fi

mkdir -p "$INSTALL_DIR" "$BIN_DIR"
unpack_dir="$tmp_dir/unpack"
mkdir -p "$unpack_dir"
tar -xzf "$tarball" -C "$unpack_dir"
unpacked_root="$(find "$unpack_dir" -mindepth 1 -maxdepth 1 -type d -name 'codex-web-bridge-connector-*' -print -quit)"
if [[ -z "$unpacked_root" ]]; then
  echo "install-release: unpacked package root not found" >&2
  exit 1
fi
package_name="$(basename "$unpacked_root")"
case "$package_name" in
  codex-web-bridge-connector-*) ;;
  *)
    echo "install-release: unexpected package root: $package_name" >&2
    exit 1
    ;;
esac
package_root="$INSTALL_DIR/$package_name"
rm -rf "$package_root"
mv "$unpacked_root" "$package_root"
if [[ -z "$package_root" ]]; then
  echo "install-release: unpacked package root not found" >&2
  exit 1
fi

install -m 0755 "$package_root/bin/codex-connector" "$BIN_DIR/codex-connector"
"$BIN_DIR/codex-connector" --help >/dev/null

cat <<EOF
installed: $BIN_DIR/codex-connector
package: $package_root
skill root: $package_root/skills

Add this to PATH if needed:
  export PATH="$BIN_DIR:\$PATH"

Use this skill root when initializing MCP Connector Mode:
  codex-connector init --root /path/to/project --skill-root "$package_root/skills"
EOF
