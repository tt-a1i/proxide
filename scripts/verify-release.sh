#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

echo "[verify] rust fmt"
cargo fmt --manifest-path connector-rs/Cargo.toml -- --check

echo "[verify] rust clippy"
cargo clippy --manifest-path connector-rs/Cargo.toml -- -D warnings

echo "[verify] rust tests"
cargo test --manifest-path connector-rs/Cargo.toml

echo "[verify] rust build"
cargo build --manifest-path connector-rs/Cargo.toml

echo "[verify] python reference tests"
python3 -m unittest discover -s connector/tests -t .

echo "[verify] diff whitespace"
git diff --check

echo "[verify] connector wrapper"
./bin/codex-connector --help >/dev/null

echo "[verify] connector installer"
tmp_install="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-install.XXXXXX")"
tmp_package=""
tmp_unpack=""
tmp_package_project=""
tmp_release_install=""
tmp_release_package=""
cleanup() {
  rm -rf "$tmp_install" "${tmp_package:-}" "${tmp_unpack:-}" "${tmp_package_project:-}" "${tmp_release_install:-}" "${tmp_release_package:-}"
}
trap cleanup EXIT
./scripts/install-connector.sh --prefix "$tmp_install" >/dev/null
"$tmp_install/bin/codex-connector" --help >/dev/null
./scripts/install-release.sh --help >/dev/null

echo "[verify] connector package"
tmp_package="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-package.XXXXXX")"
./scripts/package-connector.sh --out-dir "$tmp_package" >/dev/null
package_tar="$(find "$tmp_package" -maxdepth 1 -name 'codex-web-bridge-connector-*.tar.gz' -print -quit)"
test -n "$package_tar"
test -f "$package_tar.sha256"
package_file="$(basename "$package_tar")"
package_version="${package_file#codex-web-bridge-connector-}"
package_version="${package_version%%-*}"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp_package" && sha256sum -c "$(basename "$package_tar").sha256" >/dev/null)
else
  (cd "$tmp_package" && shasum -a 256 -c "$(basename "$package_tar").sha256" >/dev/null)
fi
tar -tzf "$package_tar" | grep -q '/bin/codex-connector$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/SKILL.md$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/agents/openai.yaml$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/references/chatgpt-pro-mcp-setup.md$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/references/mcp-connector-mode.md$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/references/providers.md$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/references/response-capture.md$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/scripts/bridge_handoff.py$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/scripts/build_context_packet.py$'
tar -tzf "$package_tar" | grep -q '/skills/codex-web-bridge/scripts/scrub_context.py$'
tar -tzf "$package_tar" | grep -q '/connector-rs/Cargo.toml$'
tar -tzf "$package_tar" | grep -q '/connector-rs/README.md$'
tar -tzf "$package_tar" | grep -q '/connector-rs/connector.example.json$'
tar -tzf "$package_tar" | grep -q '/connector-rs/src/main.rs$'
tar -tzf "$package_tar" | grep -q '/connector/config.py$'
tar -tzf "$package_tar" | grep -q '/connector/protocol.py$'
tar -tzf "$package_tar" | grep -q '/connector/server.py$'
tar -tzf "$package_tar" | grep -q '/connector/tools.py$'
tar -tzf "$package_tar" | grep -q '/connector/workspace.py$'
tar -tzf "$package_tar" | grep -q '/connector/tests/test_connector.py$'
tar -tzf "$package_tar" | grep -q '/connector/tests/test_protocol.py$'
tar -tzf "$package_tar" | grep -q '/connector/tests/test_server.py$'
tar -tzf "$package_tar" | grep -q '/scripts/install-connector.sh$'
tar -tzf "$package_tar" | grep -q '/scripts/install-release.sh$'
tar -tzf "$package_tar" | grep -q '/scripts/package-connector.sh$'
tar -tzf "$package_tar" | grep -q '/docs/devspace-parity-roadmap.md$'
tar -tzf "$package_tar" | grep -q '/docs/release.md$'
tar -tzf "$package_tar" | grep -q '/PACKAGE-MANIFEST.json$'
tar -tzf "$package_tar" | grep -q '/SECURITY.md$'
tar -tzf "$package_tar" | grep -q '/FAQ_ZH.md$'
package_listing="$tmp_package/package-listing.txt"
tar -tzf "$package_tar" > "$package_listing"
if grep -E '(^|/)(\.git|target|\.codex|\.codex-web-bridge|__pycache__|review-notes\.jsonl|workspace_state\.json|audit\.jsonl|pr-bodies)(/|$)|(^|/)\.env(\..*)?$|\.pem$|\.key$|\.p12$|\.pfx$|(^|/)id_(rsa|dsa|ecdsa|ed25519)$|(^|/)secrets\.[^/]+$|(^|/)credentials\.[^/]+$|\.pyc$|\.local\.json$|connector\.local\.json$|oauth_tokens\.json$|oauth_owner\.local\.json$' "$package_listing"; then
  echo "package contains forbidden local/build/state paths" >&2
  exit 1
fi
tmp_unpack="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-unpack.XXXXXX")"
tar -xzf "$package_tar" -C "$tmp_unpack"
package_root="$(find "$tmp_unpack" -mindepth 1 -maxdepth 1 -type d -name 'codex-web-bridge-connector-*' -print -quit)"
test -n "$package_root"
"$package_root/bin/codex-connector" --help >/dev/null
tmp_release_install="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-release-bin.XXXXXX")"
tmp_release_package="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-release-package.XXXXXX")"
./scripts/install-release.sh \
  --version "$package_version" \
  --base-url "file://$tmp_package" \
  --prefix "$tmp_release_install" \
  --install-dir "$tmp_release_package" >/dev/null
"$tmp_release_install/bin/codex-connector" --help >/dev/null
release_package_root="$(find "$tmp_release_package" -mindepth 1 -maxdepth 1 -type d -name 'codex-web-bridge-connector-*' -print -quit)"
test -n "$release_package_root"
test -f "$release_package_root/skills/codex-web-bridge/SKILL.md"
python3 -m unittest discover -s "$package_root/connector/tests" -t "$package_root"
tmp_package_project="$(mktemp -d "${TMPDIR:-/tmp}/codex-connector-package-project.XXXXXX")"
"$package_root/bin/codex-connector" \
  init \
  --config "$tmp_package/connector.local.json" \
  --root "$tmp_package_project" \
  --skill-root "$package_root/skills" \
  --state-dir "$tmp_package/state" \
  --force >/dev/null
python3 - "$tmp_package/connector.local.json" "$package_root/skills" "$tmp_package_project" <<'PY'
import json
import os
import sys
from pathlib import Path
config = json.loads(Path(sys.argv[1]).read_text())
assert [os.path.realpath(path) for path in config["skill_roots"]] == [os.path.realpath(sys.argv[2])]
assert [os.path.realpath(path) for path in config["allowed_roots"]] == [os.path.realpath(sys.argv[3])]
PY
python3 - "$package_root/PACKAGE-MANIFEST.json" <<'PY'
import json
import sys
from pathlib import Path
manifest = json.loads(Path(sys.argv[1]).read_text())
required = {
    "name": "codex-web-bridge-connector",
    "binary": "bin/codex-connector",
    "skill": "skills/codex-web-bridge/SKILL.md",
    "mcp_server": "connector-rs",
    "python_reference": "connector",
}
for key, value in required.items():
    assert manifest.get(key) == value, (key, manifest.get(key))
assert manifest.get("version")
assert manifest.get("os")
assert manifest.get("arch")
PY

echo "[verify] local MCP smoke"
CODEX_PRO_ROOT="$REPO_ROOT" python3 - <<'PY'
import json
import os
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

root = Path(os.environ["CODEX_PRO_ROOT"])
bin_path = root / "bin" / "codex-connector"
tmp = Path(tempfile.mkdtemp(prefix="codex-release-smoke-"))
project = tmp / "project"
state = tmp / "state"
config = tmp / "connector.local.json"
project.mkdir()

try:
    (project / "README.md").write_text("# Smoke\n")
    subprocess.run(
        [
            str(bin_path),
            "init",
            "--config",
            str(config),
            "--root",
            str(project),
            "--skill-root",
            str(root / "skills"),
            "--trust-level",
            "review",
            "--state-dir",
            str(state),
            "--force",
        ],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    token = json.loads(config.read_text())["owner_token"]
    proc = subprocess.Popen(
        [str(bin_path), "serve", "--config", str(config)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        def rpc(payload, session_id=None):
            data = json.dumps(payload).encode()
            request = urllib.request.Request(
                "http://127.0.0.1:8765/mcp", data=data, method="POST"
            )
            request.add_header("Content-Type", "application/json")
            request.add_header("Authorization", "Bearer " + token)
            if session_id:
                request.add_header("Mcp-Session-Id", session_id)
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.headers, json.loads(response.read())

        last_error = None
        for _ in range(80):
            if proc.poll() is not None:
                stderr = proc.stderr.read() if proc.stderr else ""
                raise RuntimeError(f"connector exited early: {stderr}")
            try:
                headers, init = rpc(
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {"protocolVersion": "2025-06-18"},
                    }
                )
                break
            except urllib.error.URLError as err:
                last_error = err
                time.sleep(0.25)
        else:
            raise RuntimeError(f"connector did not become ready: {last_error}")

        session_id = headers["Mcp-Session-Id"]
        assert init["result"]["serverInfo"]["name"] == "codex-web-bridge-connector-rs"
        _, listed = rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, session_id)
        tools = {tool["name"] for tool in listed["result"]["tools"]}
        assert "open_workspace" in tools
        assert "show_changes" in tools
        assert "render_changes" in tools
        assert "list_worktrees" in tools
        assert "list_pull_requests" in tools
        assert "create_note" in tools
        assert "write" not in tools
        render_tool = next(tool for tool in listed["result"]["tools"] if tool["name"] == "render_changes")
        assert render_tool["_meta"]["openai/outputTemplate"] == "ui://codex-web-bridge/changes.html"
        _, resources = rpc(
            {"jsonrpc": "2.0", "id": 21, "method": "resources/list"},
            session_id,
        )
        assert resources["result"]["resources"][0]["uri"] == "ui://codex-web-bridge/changes.html"
        _, resource = rpc(
            {
                "jsonrpc": "2.0",
                "id": 22,
                "method": "resources/read",
                "params": {"uri": "ui://codex-web-bridge/changes.html"},
            },
            session_id,
        )
        assert "Workspace Changes" in resource["result"]["contents"][0]["text"]
        _, prs = rpc(
            {
                "jsonrpc": "2.0",
                "id": 23,
                "method": "tools/call",
                "params": {"name": "list_pull_requests", "arguments": {}},
            },
            session_id,
        )
        assert prs["result"]["structuredContent"]["pull_requests"] == []
        _, opened = rpc(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "open_workspace",
                    "arguments": {"path": str(project)},
                },
            },
            session_id,
        )
        workspace_id = opened["result"]["structuredContent"]["workspace_id"]
        assert opened["result"]["_meta"]["codex-web-bridge/tool"] == "open_workspace"
        _, read = rpc(
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "read",
                    "arguments": {"workspace_id": workspace_id, "path": "README.md"},
                },
            },
            session_id,
        )
        assert read["result"]["structuredContent"]["content"] == "# Smoke\n"
        assert read["result"]["_meta"]["codex-web-bridge/tool"] == "read"
        assert read["result"]["_meta"]["codex-web-bridge/summary"]["content_chars"] == 8
        _, shown = rpc(
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "show_session", "arguments": {}},
            },
            session_id,
        )
        assert shown["result"]["structuredContent"]["workspace_count"] == 1
        assert shown["result"]["_meta"]["codex-web-bridge/tool"] == "show_session"
        _, note = rpc(
            {
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "create_note",
                    "arguments": {
                        "workspace_id": workspace_id,
                        "title": "Smoke review",
                        "body": "secret smoke note",
                        "severity": "low",
                        "path": "README.md",
                    },
                },
            },
            session_id,
        )
        assert note["result"]["structuredContent"]["severity"] == "low"
        assert note["result"]["_meta"]["codex-web-bridge/tool"] == "create_note"
        assert (state / "workspace_state.json").exists()
        assert "secret smoke note" in (state / "review-notes.jsonl").read_text()
        assert "secret smoke note" not in (state / "audit.jsonl").read_text()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
finally:
    shutil.rmtree(tmp, ignore_errors=True)
PY

cat <<'EOF'
[verify] manual ChatGPT MCP smoke prompt
Use only the Codex Pro Workspace connector. Do not use web browsing or memory.
First call open_workspace with path /absolute/path/to/the/project, then call read
for README.md. Reply with only the first heading line from README.md and the MCP
tool names you used.
EOF

echo "[verify] ok"
