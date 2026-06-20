# Codex Connector RS

Rust is the production MCP connector direction for `codex-web-bridge`.

The Python `connector/` package remains as a protocol reference while this
crate reaches feature parity, but new MCP service work should land here.

## Commands

Run these commands from the repository root. `./bin/codex-connector` is the
stable source-checkout entrypoint; it wraps this crate without requiring users
or agents to remember the Cargo manifest path.

```bash
# Human first run: answer prompts for allowed roots, port, public URL, and
# optional skill roots. Defaults to connector-rs/connector.local.json.
./bin/codex-connector init

# Agent/automation setup: pass explicit flags.
./bin/codex-connector \
  init --root /absolute/path/to/project

# Preserve the current-directory default in a TTY-attached script without prompts.
./bin/codex-connector \
  init --no-interactive --force

# Include the connector package's agent skills that ChatGPT can discover
# through MCP. --root is the target workspace; --skill-root is this
# checkout/package's skills directory.
./bin/codex-connector \
  init --root /absolute/path/to/project \
  --skill-root /absolute/path/to/codex-pro/skills \
  --force

# Persistent ChatGPT / GPT Pro connector. public_base_url is the public origin,
# without /mcp.
./bin/codex-connector \
  init --root /absolute/path/to/project \
  --skill-root /absolute/path/to/codex-pro/skills \
  --public-base-url "https://<tunnel-host>" --force

# Disable automatic workspace-local skill discovery if the connector should
# only expose explicitly configured skill roots.
./bin/codex-connector \
  init --root /absolute/path/to/project \
  --no-auto-skill-roots --force

# Temporary no-auth ChatGPT smoke test only.
./bin/codex-connector \
  init --root /absolute/path/to/project --no-owner-token --force

# Start the MCP server.
./bin/codex-connector serve

# Print config, endpoint, auth state, Git availability, and tool surface.
./bin/codex-connector doctor

# Show recent local audit events.
./bin/codex-connector audit

# List audited MCP sessions and inspect one session.
./bin/codex-connector sessions list
./bin/codex-connector sessions show <session-id>

# List or clean up managed Git worktrees under connector state.
./bin/codex-connector worktrees list
./bin/codex-connector worktrees cleanup
```

If a `skills/` directory is detected during the interactive setup, it is
offered as the default skill root; type `none` at that prompt to skip skill
discovery.

Install the release binary from a full checkout:

```bash
./scripts/install-connector.sh
codex-connector --help
```

Use `./scripts/install-connector.sh --prefix /path/to/prefix` to install under
another prefix. The binary install does not include the skill files; keep the
full connector checkout or unpacked release package available when configuring
`--skill-root`.

Build a distributable package from a full checkout:

```bash
./scripts/package-connector.sh
```

The package tarball includes the release binary, `skills/codex-web-bridge/`,
`connector-rs/` source and docs, helper scripts, and a SHA-256 checksum. This is
the source-checkout release artifact for users who need MCP Connector Mode, not
only the Codex skill.

Release verification:

```bash
./scripts/verify-release.sh
```

The release verifier runs Rust fmt/clippy/test/build, the Python reference
tests, whitespace checks, wrapper, installer, and package smoke tests, a local
`/mcp` HTTP smoke test, and prints the manual ChatGPT connector smoke prompt.

The ChatGPT connector URL is the public HTTPS tunnel URL plus `/mcp`; non-loopback
`public_base_url` values must use `https`. Persistent
ChatGPT use should use the built-in OAuth owner approval flow:

- protected resource metadata at `/.well-known/oauth-protected-resource`
- authorization server metadata at `/.well-known/oauth-authorization-server`
- authorization code + PKCE at `/oauth/authorize` and `/oauth/token`
- dynamic client registration compatibility at `/oauth/register`
- refresh tokens stored under `state_dir`

`init` creates an owner approval password under `state_dir` and prints it once
when newly generated. Keep that password local; ChatGPT only receives OAuth
access tokens after the owner approval page succeeds. `owner_token` remains
available for local/self-managed MCP clients that send
`Authorization: Bearer <owner_token>`.
Requested OAuth scopes are validated against the connector trust level. Empty
OAuth scope requests default to the supported scopes, and review handoff tools
that return note bodies or edit plan intent require `workspace:read` unless the
caller uses the local owner token. Execute file, Git, worktree, and PR tools
require `workspace:write`; the shell tool requires the separate `shell` scope.

## Current Tool Surface

Readonly parity with the existing Python reference:

- `open_workspace`
- `read`
- `list`
- `search`
- `git_status`
- `git_diff`
- `show_session`
- `show_changes`
- `render_changes`
- `show_review`
- `render_review`
- `list_worktrees`
- `list_pull_requests`
- `show_pull_requests`
- `render_pull_requests`
- `list_notes`
- `list_edit_plans`
- `show_edit_plans`
- `render_edit_plans`
- `list_skills`

When `state_dir` is configured, the connector writes local state artifacts:

- `<state_dir>/workspace_state.json`: bounded session/workspace/tool-call
  snapshot used by `show_session`, `sessions list/show`, `list_edit_plans`,
  and `list_pull_requests`.
- `<state_dir>/audit.jsonl`: append-only audit events without file contents or
  shell output.
- `<state_dir>/review-notes.jsonl`: append-only review notes created by
  `create_note`.
- `<state_dir>/pr-bodies/`: temporary body files passed to `gh pr create` by
  `create_pull_request`.

The state snapshot tracks opened workspace ids, workspace names, worktree
metadata, edit plan summaries, pull request handoff summaries, tool names,
workspace-relative paths,
queries, cwd values, outcomes, and bounded errors. It does not store file
contents, pull request bodies, shell command bodies, or shell output.

`open_workspace` also returns bounded root-level `AGENTS.md`, `CLAUDE.md`, and
`CONTEXT.md` content when present, plus nested instruction file paths the host
should read before working under those directories. It also returns explicitly
configured and workspace-local auto-discovered skills as
`skill://.../SKILL.md` entrypoints. Automatic skill discovery is enabled by
default and only looks in the opened workspace's real `.pi/skills` and
`skills` directories after canonical containment checks. Other local skill
directories must be explicitly authorized with `--skill-root`. Set
`auto_skill_roots` to `false` in config, pass `--no-auto-skill-roots` during
`init`, or type `none` at the interactive skill-root prompt to disable
workspace-local automatic skill roots. `list_skills` returns explicitly
configured global skill entrypoint summaries without embedding full skill
bodies. The `read` tool allows advertised `SKILL.md` entrypoints immediately,
then unlocks other files inside that skill directory only after the entrypoint
has been read in the same workspace session.

`show_changes` is the agent-facing change summary: it returns branch, HEAD,
short status, bounded diff stat, bounded diff text, and recent change-oriented
actions from the current session. `render_changes` returns the same structured
data and binds the built-in Apps widget resource
`ui://codex-web-bridge/changes.html` for hosts that support ChatGPT Apps-style
components. `show_review` aggregates recoverable review notes and edit plans
for a workspace, and `render_review` binds the review handoff widget
`ui://codex-web-bridge/review.html`. `show_pull_requests` aggregates
connector-created pull request handoff records with lifecycle status counts
without reading PR body files or calling GitHub, and `render_pull_requests`
binds `ui://codex-web-bridge/pull-requests.html`. `show_edit_plans` provides a
dedicated plan-history view with lifecycle status counts and optional status
filtering, and `render_edit_plans` binds
`ui://codex-web-bridge/edit-plans.html`. Plain MCP clients can continue using
`show_changes`, `show_review`, `show_pull_requests`, and `show_edit_plans`.

Tool results include `content`, `structuredContent`, and an Apps-compatible
`_meta` summary. The metadata carries compact tool names, counts, paths,
status values, and character lengths; it does not duplicate file contents,
diff text, or shell output bodies.

The connector also implements `resources/list` and `resources/read` for the
bundled change-summary, review-handoff, pull-request handoff, and edit-plan
history widgets. The widgets are self-contained HTML with no external resource
or network domains.

Review mode is opt-in with `--trust-level review` and adds `create_note`,
`create_edit_plan`, and `update_edit_plan_status` on top of readonly tools.
They store structured artifacts under connector state without mutating
workspace files. Edit plans persist the full intent text for later recovery,
plus lifecycle status, expected paths, and optional validated patch file
summaries. Patch diff content is not stored in audit/state summaries. Edit plan
intent is recoverable from local connector state and through authenticated
review handoff tools, but audit events and Apps `_meta` keep only counts and
character summaries.

`open_worktree` accepts optional `task_id` and `task` metadata. `list_worktrees`
lists connector-managed Git worktrees and returns fresh workspace ids for
available worktrees, plus task metadata when present. It returns worktree names,
branch, HEAD, and short status without exposing absolute local paths.
`list_pull_requests` returns connector-created pull request handoff records
without reading PR body files or calling GitHub. Execute-mode
`refresh_pull_request_status` calls `gh pr view` for a persisted handoff record
and updates connector state with the remote state, merged flag, PR number, URL,
title, base branch, and draft flag. `show_pull_requests` and
`render_pull_requests` summarize those persisted records and status counts
without network calls.
`list_notes` returns connector-created review notes, including note bodies, so
later agents can recover prior findings. It requires authenticated connector
access and an explicit `workspace_id`; no-auth smoke connectors cannot use it.
Audit and session state still keep only note summaries.
`list_edit_plans` returns connector-created edit plan summaries so later agents
can recover proposed intent before requesting execute-mode mutation. It accepts
an optional `status` filter (`draft`, `approved`, `superseded`, or `applied`).
`show_edit_plans` and `render_edit_plans` add lifecycle status counts for
handoff and approval review. These tools return plan intent, so OAuth callers
must have `workspace:read`; the local owner token also works.

`preview_patch` validates a bounded unified diff against the current workspace
and returns the files, byte counts, and bounded diff that would result without
writing files. It is available in readonly mode so agents can inspect a patch
before requesting execute-mode `apply_patch`.
When `apply_patch` is called with an approved `plan_id`, the plan must include a
validated patch summary. The connector validates that the patch matches the
plan summary, applies it, and marks the plan `applied` with applied file
summaries. Intent-only plans can be recovered and reviewed, but cannot be used
as automatic apply evidence.

Execute mode is opt-in with `--trust-level execute` and includes review tools
plus:

- `write`
- `edit`
- `apply_patch`
- `move_path`
- `shell`
- `open_worktree`
- `publish_branch`
- `create_pull_request`
- `refresh_pull_request_status`
- `refresh_pull_requests`

`apply_patch` accepts bounded unified diffs for existing, added, and deleted
UTF-8 files under the workspace. Added files must target an existing
workspace-contained parent directory, and rename/move patches are rejected. The
server validates all touched files and hunk contexts before writing, so a
mismatch does not leave a partially applied patch.

`move_path` handles file rename/move operations directly. It only moves regular
files within the workspace, rejects symlinks and directories, and requires
`overwrite=true` before replacing an existing regular destination file.

The `shell` tool is non-interactive, runs under `/bin/bash -lc` in a
workspace-contained cwd, uses a scrubbed environment, and has timeout/output
bounds. `open_worktree` creates managed Git worktrees under connector state and
returns a new workspace id for isolated coding sessions; optional `task_id` and
`task` metadata are stored under connector state, not inside the worktree.
`publish_branch` pushes the current workspace branch to a Git remote with
upstream tracking; it does not commit changes, change remotes, or create pull
requests. `create_pull_request` uses the GitHub CLI (`gh pr create`) to create
a PR for the current branch after writing the PR body under connector state; it
does not commit or publish the branch itself. `refresh_pull_request_status`
uses the GitHub CLI (`gh pr view`) to refresh one persisted PR handoff after
review, merge, or close events. `refresh_pull_requests` refreshes multiple
persisted PR handoffs for an opened workspace in one call, using each record's
PR URL when available and falling back to its branch. Batch refresh is capped
at five records per call and returns `truncated=true` when another small batch
should be requested. Both tools update
connector state only and do not change workspace files. Richer audit widgets
and continuous PR polling are planned in `docs/devspace-parity-roadmap.md`.
