# ChatGPT Pro MCP Setup

Use this runbook when a first-time user wants ChatGPT Pro to connect to a local
workspace through the Rust `connector-rs/` MCP server. It is especially useful
when the local agent cannot operate a browser: the agent can still prepare the
local MCP server and give the user a short ChatGPT web checklist.

Last manual smoke test in this repo: 2026-06-19. ChatGPT labels move over time;
if the UI text differs, follow the current OpenAI docs for Developer mode and
connecting from ChatGPT.

Official references:

- https://developers.openai.com/apps-sdk/deploy/connect-chatgpt
- https://developers.openai.com/apps-sdk/quickstart
- https://help.openai.com/en/articles/12584461-developer-mode-and-mcp-apps-in-chatgpt

## Mental Model

- The local agent starts a readonly MCP server for first-time ChatGPT setup.
- ChatGPT Pro is the MCP host. It connects to the local server through an HTTPS
  endpoint and calls tools such as `open_workspace`, `read`, `search`, `list`,
  `git_status`, `git_diff`, `show_session`, and `list_skills`.
- `open_workspace` returns root project instructions, nested instruction file
  paths, and configured skill entrypoints. A skill entrypoint looks like
  `skill://<skill_id>/SKILL.md`; ChatGPT must read that entrypoint before the
  connector allows reading other files from the same skill directory.
- Persistent ChatGPT use should use the Rust connector's OAuth owner approval
  flow. The owner approval password stays in `state_dir`; ChatGPT receives only
  OAuth access and refresh tokens.
- The local agent does not need browser-control capability after it has started
  the local server and tunnel. A human can do the one-time ChatGPT web setup.
- Installing only `skills/codex-web-bridge` does not install the root-level
  `connector-rs/` crate. The user needs a checkout or package that includes
  `connector-rs/` and `bin/codex-connector`. The Python `connector/` package is
  a reference implementation, not the default production path.

## Preconditions

- The user has a ChatGPT account/workspace where Developer mode and custom MCP
  apps are available.
- The local checkout includes both `skills/codex-web-bridge/` and `connector-rs/`.
- `allowed_roots` points at a specific project directory, not `~`, `/`, or a
  broad profile directory.
- The connector stays `readonly` unless the user explicitly opts into a higher
  trust level. `preview_patch` is readonly so ChatGPT can validate a proposed
  unified diff before mutation, `list_notes` can recover prior review findings,
  and `list_edit_plans` can recover prior edit plans. `show_review` and
  Apps-compatible `render_review` aggregate those review artifacts for
  handoff. `create_edit_plan` and `update_edit_plan_status` are review-mode
  state only and do not mutate the workspace. `apply_patch` can optionally take
  an approved `plan_id` with a validated patch summary and mark that plan
  `applied` after a successful patch.
  `write`, `edit`, `apply_patch`, `move_path`, non-interactive `shell`,
  managed Git worktrees, branch publishing, PR creation, and PR status refresh
  are implemented only behind `trust_level=execute`.
- With `state_dir` configured, `show_session` can return a structured snapshot
  of opened workspaces and recent tool calls without file contents or shell
  output. Use it when the human asks what ChatGPT did through the connector.
- ChatGPT needs an HTTPS MCP endpoint. Prefer OpenAI Secure MCP Tunnel when
  available. For a temporary smoke test, ngrok or Cloudflare Tunnel can expose
  the loopback server, but the tunnel must be short-lived.

## Agent-Side Setup

From the repo root:

```bash
# Optional: install the Rust connector binary to ~/.local/bin/codex-connector.
./scripts/install-connector.sh
```

```bash
./bin/codex-connector \
  init --root /absolute/path/to/the/project \
  --skill-root /absolute/path/to/codex-pro/skills \
  --public-base-url https://example.trycloudflare.com
```

If `codex-connector` is on `PATH`, use it in place of `./bin/codex-connector`.
`--root` is the target project exposed to ChatGPT. `--skill-root` is the
`skills/` directory from this connector checkout or unpacked release package;
it is usually different from the target project. Keep the full connector
checkout/package available because the binary-only install does not include
skill files.

The command writes `connector-rs/connector.local.json`:

```json
{
  "allowed_roots": ["/absolute/path/to/the/project"],
  "skill_roots": ["/absolute/path/to/codex-pro/skills"],
  "trust_level": "readonly",
  "host": "127.0.0.1",
  "port": 8765,
  "owner_token": "<generated>",
  "public_base_url": "https://example.trycloudflare.com",
  "state_dir": "/Users/me/.local/share/codex-web-bridge/connector-rs"
}
```

`init` also creates `state_dir/oauth_owner.local.json` and prints the owner
approval password if it was newly generated. Give that password only to the
human completing the ChatGPT authorization page.

For a self-managed MCP client, set a strong `owner_token` and send it as
`Authorization: Bearer <token>`. For a one-off no-auth smoke test, you can still
create a temporary config with:

```bash
./bin/codex-connector \
  init --root /absolute/path/to/the/project \
  --skill-root /absolute/path/to/codex-pro/skills \
  --public-base-url https://example.trycloudflare.com \
  --no-owner-token \
  --force
```

Treat no-auth as a last-resort diagnostic mode: keep `readonly`, allow only one
specific repo, and stop the tunnel immediately after testing. Do not combine
no-auth public tunnels with `trust_level=execute`.

Check the setup:

```bash
./bin/codex-connector doctor
```

Start the local server:

```bash
./bin/codex-connector serve
```

Expose it through an HTTPS tunnel. Example with Cloudflare Tunnel:

```bash
cloudflared tunnel --url http://127.0.0.1:8765
```

The ChatGPT endpoint is the tunnel URL plus `/mcp`, for example:

```text
https://example.trycloudflare.com/mcp
```

Optional local sanity check:

```bash
curl -sS \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <owner_token>' \
  -d '{"jsonrpc":"2.0","id":1,"method":"ping"}' \
  http://127.0.0.1:8765/mcp
```

Expected result:

```json
{"jsonrpc":"2.0","id":1,"result":{}}
```

## ChatGPT Web Setup

The user does this part in ChatGPT web:

1. Open ChatGPT settings.
2. Enable Developer mode under `Apps & Connectors` / `Apps` /
   `Connectors` -> `Advanced settings`, depending on the current UI label.
3. Go to `Connectors` or `Apps & Connectors` and choose `Create`.
4. Name the connector, for example `Codex Pro Workspace`.
5. Use the HTTPS endpoint ending in `/mcp`.
6. Choose the OAuth/authenticated flow if ChatGPT offers one. When the approval
   page opens, enter the owner approval password printed by `init` or reported
   by `doctor`.
7. Save or connect the app.
8. Open a new chat and enable/select the connector from the tools/apps picker.

Only choose `No authentication` for a short readonly smoke test created with
`--no-owner-token`; shut that tunnel down immediately after testing.

## Golden Prompt

Use a small prompt that proves the host can call the local tools without asking
for broad access:

```text
Use only the Codex Pro Workspace connector. Do not use web browsing or memory.
First call open_workspace with path /absolute/path/to/the/project, then call
read for README.md. Reply with only the first heading line from README.md and
the names of the MCP tools you used.
```

Expected behavior:

- ChatGPT asks to use the connector tools or calls them directly, depending on
  current product settings.
- The connector receives `open_workspace` and `read`.
- The answer includes the README heading and the tool names.

## What To Tell A First-Time User

When the local agent cannot operate a browser, report only the steps the user
must do manually:

```text
I started the readonly MCP server locally and exposed it through this temporary
HTTPS endpoint:

https://<tunnel-host>/mcp

In ChatGPT web:
1. Open Settings -> Apps & Connectors -> Advanced settings and enable Developer mode.
2. Go to Connectors -> Create.
3. Name it "Codex Pro Workspace".
4. Paste the endpoint above.
5. Use the OAuth/authenticated setup. When ChatGPT opens the approval page,
   enter this owner approval password:
   <owner-approval-password>
6. Open a new chat, select the connector, and send the golden prompt I provide.

Tell me when ChatGPT shows the connector tools or if it reports an error.
```

## Shutdown

After the test:

- Stop the HTTPS tunnel.
- Stop `codex-connector` / `./bin/codex-connector serve`.
- Remove or disable any no-auth ChatGPT connector created only for testing.
- Do not leave a no-auth public tunnel to local source code running.
- For persistent connectors, clean up old ChatGPT connector entries and rotate
  `state_dir/oauth_owner.local.json` if the approval password was exposed.

## Troubleshooting

- ChatGPT cannot connect: verify the endpoint is HTTPS and ends in `/mcp`, not
  `/rpc`.
- ChatGPT authorization fails before showing tools: verify
  `--public-base-url` matches the tunnel origin exactly and does not include
  `/mcp`.
- ChatGPT lists no tools: restart the connector and reconnect the app.
- ChatGPT labels readonly tools as write-capable: ensure the server exposes tool
  `annotations.readOnlyHint` and restart the connector.
- The local server rejects the workspace: set `allowed_roots` to the exact repo
  path or a narrow parent directory.
- A no-browser agent is blocked on ChatGPT UI: the agent should keep the local
  server/tunnel running and ask the user to complete only the web setup steps.
- The user installed only the skill: install or clone the project distribution
  that includes `skills/codex-web-bridge/`, the root-level `connector-rs/`
  crate, and `bin/codex-connector`. A skill-only install supports Bridge Mode
  but cannot start MCP Connector Mode.
