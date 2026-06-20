# MCP Connector Mode

Use this reference when the user wants a DevSpace-like flow where ChatGPT Pro,
Claude, or another MCP-capable web host connects to a local workspace through
MCP instead of relying on Codex browser automation.

For the practical first-time ChatGPT Pro setup steps, read
`chatgpt-pro-mcp-setup.md` after this file.

## Mental Model

Bridge Mode and Connector Mode are different directions:

- Bridge Mode: Codex sends a scrubbed packet to a web model and brings the
  answer back. The web model cannot touch local files or run commands.
- Connector Mode: a web MCP host connects to a local MCP server and uses tools
  against approved local workspaces. This can work even when the local agent has
  no browser-control capability.

Do not describe Connector Mode as "local agent calls GPT Pro" unless the local
agent is also an MCP host connected to the same server. In the common ChatGPT
Pro case, GPT Pro is the host driving local tools.

## Trust Levels

Default to the lowest useful trust level.

- `readonly`: open approved workspaces, read files, list files, search text, and
  inspect Git status/diff. No write, edit, shell, or network side effects.
- `review`: readonly plus structured review artifacts such as response notes,
  checkpoints, and imported external comments. Still no shell or writes.
- `execute`: edit/write/shell/worktree tools are available only after explicit
  user opt-in.

Default public ChatGPT setup should remain `readonly`; `execute` is for explicit,
authenticated code-editing sessions.

## Required Safety Boundaries

- Allowlist workspace roots. Avoid broad roots such as `~`, `/`, or a whole user
  profile directory.
- Bind locally by default. Public tunnel setup must be explicit and user-owned.
- Require OAuth owner approval or an owner token before any remote MCP host can
  connect.
- Keep write, edit, shell, and worktree tools disabled by default.
- Never expose arbitrary local shell access in readonly or review mode.
- Log tool calls and workspace openings, but avoid logging shell command bodies
  or sensitive file contents by default.
- Report that a public tunnel URL is not a secret; authentication still matters.
- Treat worktrees as workflow isolation, not a security boundary.

## Tool Surface

Readonly mode — **implemented** in root-level `connector-rs/` (see repo README):

- `open_workspace`: open a path inside an allowed root and return a workspace id.
  It also returns bounded root-level `AGENTS.md`, `CLAUDE.md`, and `CONTEXT.md`
  instructions when present.
- `read`: read bounded text from a workspace-relative file.
- `search`: search text with ignore rules and bounded results.
- `list`: list a workspace-relative directory with bounded entries.
- `git_status`: return branch, HEAD, and short status.
- `git_diff`: return bounded diff/stat for the workspace.
- `preview_patch`: validate a bounded unified diff against current workspace
  contents and return the files, byte counts, and bounded diff that would
  result without writing files. Use this before `apply_patch` when an agent
  wants a preflight check.
- `show_session`: summarize recent audited tool calls for the current/requested
  MCP session without file contents.
- `show_changes`: summarize branch, HEAD, short status, bounded diff/stat, and
  recent change-oriented actions for a workspace.
- `render_changes`: return the same data as `show_changes` and bind the bundled
  Apps widget resource `ui://codex-web-bridge/changes.html`.
- `show_review`: summarize recoverable review notes and edit plans for a
  workspace. It requires authenticated connector access because note bodies are
  returned.
- `render_review`: return the same data as `show_review` and bind the bundled
  Apps widget resource `ui://codex-web-bridge/review.html`.
- `list_worktrees`: list connector-managed Git worktrees and return workspace
  ids for available worktrees without exposing absolute local paths. Includes
  optional `task_id` / `task` metadata when a worktree was opened with it.
- `list_pull_requests`: list connector-created pull request handoff records
  without reading PR body files or calling GitHub.
- `show_pull_requests`: summarize connector-created pull request handoff
  records and lifecycle status counts without reading PR body files or calling
  GitHub.
- `render_pull_requests`: return the same data as `show_pull_requests` and bind
  the bundled Apps widget resource
  `ui://codex-web-bridge/pull-requests.html`.
- `list_notes`: list connector-created review notes, including note bodies, so
  later agents can recover prior findings from `review-notes.jsonl`. It
  requires authenticated connector access and an explicit `workspace_id`; do
  not use it for no-auth smoke tunnels.
- `list_edit_plans`: list connector-created edit plan summaries so later agents
  can recover proposed intent before requesting execute-mode mutation. It
  accepts optional `workspace_id`, `status`, and `limit` filters, and requires
  authenticated connector access because plan intent is returned.
- `show_edit_plans`: summarize connector-created edit plan history with
  lifecycle status counts and optional status filtering.
- `render_edit_plans`: return the same data as `show_edit_plans` and bind the
  bundled Apps widget resource `ui://codex-web-bridge/edit-plans.html`.
- `list_skills`: list configured skill roots and `SKILL.md` entrypoints. The
  host must read a skill's `SKILL.md` before using other files in that skill
  directory.

All tool results return `content` for plain MCP clients, `structuredContent`
for model-readable structured data, and `_meta` with compact Apps-compatible
summary metadata. The `_meta` layer contains counts, status, paths, and
character lengths, not raw file contents, diff bodies, or shell output.
The connector also exposes `resources/list` and `resources/read` for the
self-contained change-summary and review-handoff widgets used by
`render_changes` and `render_review`.

Review mode — **implemented**:

- All readonly tools.
- `create_note`: save a model review note under connector state without
  mutating workspace files. Note bodies are stored in `review-notes.jsonl`;
  audit and session state retain only summaries. Later authenticated agents can
  recover the notes through readonly `list_notes` scoped to a workspace id.
- `create_edit_plan`: save a structured edit plan under connector state without
  mutating workspace files. Plans include title, intent, expected paths, and
  optional validated patch file summaries; patch diff content is not stored in
  audit/state summaries.
- `update_edit_plan_status`: move an edit plan through `draft`, `approved`, or
  `superseded` without mutating workspace files. A plan becomes `applied` only
  when execute-mode `apply_patch` succeeds with that approved `plan_id`; the
  plan must include a validated patch summary for automatic apply linkage.

Execute mode — **partially implemented**; must stay behind an explicit,
separate opt-in with its own trust model and tests:

- All review tools.
- `write`: create or overwrite scoped UTF-8 text files under the workspace.
- `edit`: exact-match UTF-8 text replacement under the workspace.
- `apply_patch`: apply bounded unified diffs to existing, added, and deleted
  UTF-8 files under the workspace after validating all hunk contexts. Added
  files require an existing workspace-contained parent directory; rename/move
  patches are rejected. Optional `plan_id` links the patch to an approved edit
  plan with a validated patch summary and marks it `applied` after success.
- `move_path`: rename or move a regular file within the workspace. Existing
  destination files are replaced only when `overwrite=true`; symlinks and
  directories are rejected.
- `shell`: non-interactive command execution with timeout, cwd containment,
  bounded output, scrubbed environment, and an explicit warning that local user
  permissions apply.
- `open_worktree`: create a managed isolated Git worktree under connector state
  and return a new workspace id for parallel execution. Optional `task_id` and
  `task` metadata help agents recover why the worktree exists.
- `publish_branch`: push the current workspace branch to a Git remote with
  upstream tracking. It does not commit changes, change remotes, or create pull
  requests.
- `create_pull_request`: create a GitHub pull request for the current workspace
  branch through `gh pr create`. It writes the PR body under connector state and
  does not commit or publish the branch itself.
- `refresh_pull_request_status`: refresh a persisted PR handoff record through
  `gh pr view` and update connector state with remote state, merged flag, PR
  number, URL, title, base branch, and draft flag. It changes connector state
  only, not workspace files.

## User-Facing Choice

When a user asks to use GPT Pro from an agent that cannot operate a browser,
offer:

1. Browser Bridge: Codex operates Chrome, the Codex side-panel browser, or a
   manual paste flow.
2. MCP Connector: ChatGPT Pro or another MCP host connects to this local
   workspace. Works without browser automation in the local agent, but grants
   the web host local tool access.

Mention that Connector Mode requires setup: local server, allowed roots, OAuth
owner approval or owner token, and usually a user-managed HTTPS tunnel for web
hosts.

When the local agent cannot operate a browser, it should still complete the
local side: validate the connector, create the local config, start the readonly
server, start the HTTPS tunnel, and give the user the ChatGPT web checklist from
`chatgpt-pro-mcp-setup.md`.

## Non-Goals

- Do not promise doubled limits as a product guarantee.
- Do not bypass provider terms, authentication, CAPTCHAs, or paywalls.
- Do not hide that execute mode gives the remote host local code-editing and
  command-running power.
- Do not merge Connector Mode into Bridge Mode's low-trust defaults.
