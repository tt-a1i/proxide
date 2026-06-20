# Release Runbook

This project publishes the Rust MCP connector as GitHub Release assets. The
Codex skill remains installable from the repository path, while Connector Mode
users can install the release package without a source checkout.

## Preconditions

- `main` is clean and pushed.
- `./scripts/verify-release.sh` passes locally.
- `connector-rs/Cargo.toml` has the version you intend to publish.
- The release tag uses `v<semver>`, for example `v0.1.0`.

## Publish

```bash
./scripts/verify-release.sh
git tag v0.1.0
git push origin v0.1.0
```

The `Release` GitHub Actions workflow builds and uploads:

- `codex-web-bridge-connector-<version>-linux-x86_64.tar.gz`
- `codex-web-bridge-connector-<version>-darwin-x86_64.tar.gz`
- `codex-web-bridge-connector-<version>-darwin-aarch64.tar.gz`
- matching `.sha256` files

Manual retry is available from GitHub Actions with `workflow_dispatch`; pass the
same tag, for example `v0.1.0`.

## Verify Assets

After the workflow completes, install from the published release:

```bash
tmp_prefix="$(mktemp -d)"
tmp_package="$(mktemp -d)"
./scripts/install-release.sh \
  --repo tt-a1i/codex-web-bridge \
  --version v0.1.0 \
  --prefix "$tmp_prefix" \
  --install-dir "$tmp_package"
"$tmp_prefix/bin/codex-connector" --help
```

For latest-release verification:

```bash
curl -fsSL https://raw.githubusercontent.com/tt-a1i/codex-web-bridge/main/scripts/install-release.sh | bash
```

## MCP Smoke Test

Create a temporary project and config:

```bash
project="$(mktemp -d)"
printf '# Smoke\n' > "$project/README.md"
codex-connector init \
  --root "$project" \
  --skill-root "$HOME/.local/share/codex-web-bridge/connector"/codex-web-bridge-connector-*/skills \
  --trust-level readonly \
  --force
codex-connector doctor
codex-connector serve
```

Expose `http://127.0.0.1:8765` through a trusted HTTPS tunnel and configure
ChatGPT with:

```text
https://<tunnel-host>/mcp
```

Ask ChatGPT:

```text
Use only the Codex Pro Workspace connector. Do not use web browsing or memory.
First call open_workspace with path <project>, then call read for README.md.
Reply with only the first heading line from README.md and the MCP tool names
you used.
```

Expected result: ChatGPT replies with `# Smoke` and mentions `open_workspace`
and `read`.

## Rollback

If a bad release was published:

```bash
gh release delete v0.1.0
git push origin :refs/tags/v0.1.0
git tag -d v0.1.0
```

Prefer publishing a follow-up patch release when users may already have
installed the tag.
