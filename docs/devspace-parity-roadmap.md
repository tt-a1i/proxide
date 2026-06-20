# DevSpace Parity Roadmap

This project is later to the ChatGPT-local-workspace space than
`Waishnav/devspace`, so the product goal is:

1. Match the core capabilities that make DevSpace useful to agents.
2. Keep our stricter trust boundaries while those capabilities are added.
3. Differentiate with bridge-mode workflows that DevSpace does not cover.

DevSpace reference: https://github.com/Waishnav/devspace

## Current Position

Implemented here:

- Codex Skill bridge mode: packet building, scrub gate, browser/manual handoff,
  and response capture.
- Readonly MCP connector mode: `open_workspace`, `read`, `search`, `list`,
  `git_status`, `git_diff`, `show_changes`, and `render_changes`.
- `/mcp` HTTP JSON-RPC endpoint with `/rpc` legacy alias.
- Apps-compatible widget resources: `resources/list` and `resources/read`
  expose self-contained change-summary, review-handoff, and pull-request
  handoff HTML components for `render_changes`, `render_review`, and
  `render_pull_requests`.
- Tool schemas and readonly annotations for ChatGPT MCP hosts.
- First-time ChatGPT Pro setup runbook.
- Rust production connector foundation: `connector-rs` with `init`, `serve`,
  `doctor`, `/mcp`, `/rpc`, readonly tool schemas, readonly annotations, and
  HTTP smoke tests.
- Rust audit foundation: local `audit.jsonl` records tool call names, argument
  summaries, outcomes, and bounded result summaries without file contents.
- Session visibility foundation: `workspace_state.json` records session,
  workspace, and bounded tool-call summaries; readonly `show_session` MCP tool
  plus `codex-connector sessions list/show` inspect that state with audit-log
  fallback.
- Review trust layer: explicit `trust_level=review` exposes `create_note`,
  `create_edit_plan`, and `update_edit_plan_status`, storing structured review
  notes and edit plans under connector state without workspace mutation.
  Readonly `list_notes` and `list_edit_plans` let later agents recover those
  artifacts; `show_review` / `render_review` aggregate them into an agent
  handoff surface.
- Instruction and skill discovery foundation: `open_workspace` returns bounded
  root `AGENTS.md` / `CLAUDE.md` / `CONTEXT.md` content, nested instruction
  file paths, and configured or auto-discovered `skill://.../SKILL.md`
  entrypoints. Automatic skill discovery includes only workspace-local
  `.pi/skills` and `skills` directories that remain inside the opened
  workspace after canonicalization; other local skill directories require
  explicit `skill_roots`. `read` enforces that a skill's `SKILL.md` entrypoint
  is read before other files in that skill directory can be read.
- Execute edit/write foundation: explicit `trust_level=execute` exposes scoped
  `write`, exact-match `edit`, bounded unified-diff `apply_patch`, and
  `move_path` tools with containment checks and bounded outputs.
- Patch preview foundation: readonly `preview_patch` validates unified diffs
  against current workspace contents and returns the would-change files, byte
  counts, and bounded diff without writing files.
- Edit plan foundation: review-mode `create_edit_plan` persists title, intent,
  expected paths, lifecycle status, and optional validated patch file summaries
  without mutating workspace files; readonly `list_edit_plans` lets later
  agents recover them. Execute-mode `apply_patch` can mark an approved plan
  `applied`.
- Edit plan history visibility foundation: readonly `show_edit_plans` and
  `render_edit_plans` aggregate edit plan records with lifecycle status counts
  and optional status filtering, without reading workspace files.
- Execute shell foundation: explicit `trust_level=execute` exposes bounded
  non-interactive `/bin/bash -lc` commands with workspace cwd containment and
  environment scrubbing.
- Managed worktree foundation: explicit `trust_level=execute` exposes
  `open_worktree`, creating Git worktrees under connector state and returning
  a new workspace id; readonly `list_worktrees` lists managed worktrees and
  returns fresh workspace ids for available worktrees.
- PR lifecycle refresh foundation: explicit `trust_level=execute` exposes
  `refresh_pull_request_status` and `refresh_pull_requests`, which use
  `gh pr view` to refresh one or multiple persisted PR handoff summaries with
  remote state, merged flag, PR number, URL, title, base branch, and draft
  flag.
- PR handoff visibility foundation: readonly `show_pull_requests` and
  `render_pull_requests` aggregate connector-created PR records and lifecycle
  status counts without reading PR body files or calling GitHub.
- First-run CLI setup foundation: `codex-connector init` prompts human users
  for allowed roots, local port, optional public origin, and optional skill
  roots when run interactively without `--root`; scripted agents can continue
  using explicit flags.
- Python reference connector: retained only as a verified protocol reference
  while the Rust connector reaches full parity.

Still missing versus DevSpace:

- Richer edit/write workflows beyond bounded text writes, patch preview, edit
  plans, file moves, unified-diff patching, review handoff widgets, and edit
  plan history cards, such as interactive approval controls.
- Richer managed Git worktree workflows beyond create/list/cleanup/publish/task
  metadata/PR handoff records and on-demand PR status refresh, such as
  continuous PR review polling and richer PR lifecycle widgets.
- Richer workspace state persistence beyond bounded session snapshots, audit
  logs, recoverable review notes/edit plans, worktree metadata, and PR handoff
  summaries.
- Richer ChatGPT Apps widget interactions beyond the current change-summary,
  review-handoff, pull-request handoff, and edit-plan history cards.
- Published registry/tap install path beyond the generated source-checkout
  release package.

## Parity Phases

### P1: Auth And Public Endpoint Hardening

Goal: make the connector usable as a persistent ChatGPT connector without
no-auth tunnels.

Deliverables:

- OAuth protected-resource and authorization-server metadata endpoints. Status:
  implemented.
- Owner password generation and storage outside repo-local config. Status:
  implemented under `state_dir/oauth_owner.local.json`.
- Browser approval page for new MCP client sessions. Status: implemented.
- Access token and refresh token storage with TTLs. Status: implemented under
  `state_dir/oauth_tokens.json`.
- Host header allowlist derived from `public_base_url`. Status: implemented.
- `doctor` checks for OAuth metadata, host allowlist, and public URL shape.
  Status: implemented.

### P2: Workspace State And Audit Log

Goal: make MCP sessions inspectable by humans and agents.

Deliverables:

- State directory for workspace/session metadata. Status: partial, `state_dir`
  exists with `workspace_state.json` session/workspace snapshots and
  `audit.jsonl`.
- Tool-call audit log with timestamps, session ids, tool names, and paths.
  Status: implemented for append-only audit plus bounded state snapshots.
- Default redaction of file contents and shell command bodies. Status:
  implemented for audit and state snapshots; read content is summarized by
  character count in audit and omitted from state.
- `show_session` MCP tool for opened workspaces, read paths, searches, and git
  summaries. Status: implemented using `workspace_state.json` with audit-log
  fallback.
- `codex-connector sessions list/show` commands. Status: implemented using
  `workspace_state.json` with audit-log fallback.
- `create_note` MCP tool for review-mode model findings. Status: implemented
  for `trust_level=review` and `trust_level=execute`; note bodies are stored in
  `review-notes.jsonl`, while audit and session state keep only summaries.
- `list_notes` MCP tool for recovering review-mode findings in later sessions.
  Status: implemented as a readonly state-backed tool with workspace, severity,
  path, and limit filters; it returns note bodies from `review-notes.jsonl`
  while audit/session summaries only record counts.
- `create_edit_plan` / `update_edit_plan_status` / `list_edit_plans` tools for
  review-mode edit intent. Status: implemented; plans are stored in
  `workspace_state.json` with expected paths, lifecycle status, and optional
  patch file summaries, while patch diff content is not stored in audit/state
  summaries.

### P3: Edit And Write Tools

Goal: let ChatGPT make scoped code changes without shell access.

Deliverables:

- `read` remains unchanged and bounded.
- `write` creates or overwrites workspace-relative files after containment
  checks. Status: implemented for bounded UTF-8 text.
- `edit` applies exact-match or patch-style edits with clear failure modes.
  Status: exact-match UTF-8 text edit plus bounded unified-diff
  `apply_patch` implemented for existing, added, and deleted UTF-8 files.
- `preview_patch` validates a unified diff and returns files, byte counts, and a
  bounded diff without writing files. Status: implemented as readonly preflight.
- `move_path` handles rename/move for regular files with containment checks,
  symlink/directory rejection, and explicit overwrite guard. Status:
  implemented.
- Text mutation outputs include changed path, byte counts, and a bounded diff.
  `move_path` returns source path, destination path, overwrite status, and file
  bytes. Status: implemented.
- Mutating tools are hidden unless `trust_level` is `execute`; `preview_patch`
  remains readonly. Status: implemented.
- Tests cover path escapes, symlink escapes, binary files, nonexistent parents,
  and partial edit failures. Status: implemented for exact-match and
  unified-diff patch paths.

### P4: Shell Tool

Goal: support tests, builds, git commands, and package scripts.

Deliverables:

- `shell` tool with workspace cwd containment, timeout, max output bytes, and
  explicit execute trust requirement. Status: implemented for non-interactive
  commands.
- Bash detection in `doctor`. Status: implemented.
- Environment scrub for common secret variables by default. Status:
  implemented with an allowlist plus secret-name filtering.
- Optional command preview logging, disabled by default. Status: command bodies
  are redacted from audit logs.
- Clear non-goals for interactive TTY sessions and background daemons in the
  first shell release. Status: implemented in docs; shell has no stdin and is
  timeout-bound.

### P5: Managed Worktrees

Goal: let ChatGPT work in isolated Git worktrees for parallel coding sessions.

Deliverables:

- `open_worktree` tool with `workspace_id`, `base_ref`, and optional branch
  name. Status: implemented.
- Worktree root under connector state, not inside the active checkout. Status:
  implemented under `state_dir/worktrees`.
- `doctor` checks Git availability and basic worktree support. Status:
  implemented for configured roots.
- Cleanup command for stale worktrees. Status: implemented as
  `codex-connector worktrees cleanup`.
- `list_worktrees` MCP tool so the host can rediscover managed worktrees and
  continue with `read`, `show_changes`, or execute tools. Status: implemented.
- Per-worktree task metadata so agents can recover why a managed worktree
  exists. Status: implemented as optional `task_id` / `task` on
  `open_worktree`, persisted under `state_dir/worktree-metadata`, and returned
  by `list_worktrees`.
- `publish_branch` MCP tool so the host can publish the current worktree branch
  to a Git remote with upstream tracking. Status: implemented for execute mode;
  it does not commit changes, change remotes, or create pull requests.
- `create_pull_request` MCP tool so the host can hand off a completed branch as
  a GitHub PR. Status: implemented for execute mode through `gh pr create`; it
  writes PR bodies under `state_dir/pr-bodies` and does not commit or publish
  the branch itself.
- `list_pull_requests` MCP tool so later agents can recover PR handoff state.
  Status: implemented as readonly persisted summaries in `workspace_state.json`
  without reading PR body files or calling GitHub.
- `refresh_pull_request_status` MCP tool so later agents can update a persisted
  PR handoff after review, close, or merge events. Status: implemented for
  execute mode through `gh pr view`; it updates connector state only and does
  not read PR body files.
- `refresh_pull_requests` MCP tool so later agents can batch-refresh persisted
  PR handoff records for an opened workspace after review, close, or merge
  events. Status: implemented for execute mode through `gh pr view`; it uses
  each persisted PR URL when available, falls back to branch, caps each call to
  a small batch with `truncated=true` continuation signaling, updates connector
  state only, and does not read PR body files.
- `show_pull_requests` / `render_pull_requests` MCP tools so later agents can
  inspect connector-created PR lifecycle records and status counts without
  reading PR body files or calling GitHub. Status: implemented as readonly
  structured output plus a self-contained Apps-compatible card.
- Documentation that worktrees are workflow isolation, not a security boundary.
  Status: implemented.

### P6: Instructions And Skills Discovery

Goal: make the MCP host behave more like a local coding agent.

Deliverables:

- `open_workspace` returns relevant `AGENTS.md`, `CLAUDE.md`, and `CONTEXT.md`
  instructions when present. Status: implemented for root-level files and
  nested instruction path discovery.
- Skill discovery from configured and project-local directories. Status:
  implemented; configured `skill_roots`, workspace `.pi/skills`, and workspace
  `skills` are surfaced from `open_workspace` when present, with workspace
  auto-discovery disabled by `auto_skill_roots: false` or
  `--no-auto-skill-roots`.
- A tool or structured field that lists available skills and their `SKILL.md`
  entrypoints. Status: implemented as `open_workspace` skill summaries and
  `list_skills`.
- Guardrail: the host must read a skill's `SKILL.md` before using files inside
  that skill directory. Status: implemented in `read` for `skill://` resources:
  advertised `SKILL.md` entrypoints can be read immediately, while other skill
  files require that entrypoint to have been read in the same workspace session.

### P7: ChatGPT Apps Widgets And Change Summaries

Goal: improve ChatGPT-side usability once execute mode exists.

Deliverables:

- Optional Apps widget metadata for workspace, file, edit, and shell results.
  Status: implemented at tool-result level as Apps-compatible `_meta`
  summaries. The `render_changes` tool now binds a self-contained HTML widget
  resource at `ui://codex-web-bridge/changes.html`.
- `show_changes` aggregate tool. Status: implemented as a readonly structured
  fallback with branch, HEAD, short status, bounded diff/stat, and recent
  change-oriented actions.
- `show_review` aggregate tool. Status: implemented as a readonly structured
  fallback with recoverable review notes and edit plans for authenticated,
  workspace-scoped agents.
- `show_edit_plans` aggregate tool. Status: implemented as a readonly
  structured fallback with edit plan lifecycle status counts and optional
  status filtering for authenticated, workspace-scoped agents.
- Diff/change summary card for ChatGPT Apps-compatible hosts. Status:
  implemented as `render_changes` plus `resources/list` / `resources/read`;
  review handoff card implemented as `render_review` plus the same resource
  endpoints; pull-request handoff card implemented as `render_pull_requests`
  plus the same resource endpoints; edit-plan history card implemented as
  `render_edit_plans` plus the same resource endpoints. Richer interactive UI
  remains future work.
- Plain-text fallback for non-ChatGPT MCP clients. Status: implemented via
  tool-result `content`.

### P8: Packaging

Goal: make installation match user expectations.

Deliverables:

- One documented install path that includes both `skills/codex-web-bridge/` and
  `connector-rs/`. Status: implemented for full source checkout via
  `./bin/codex-connector`; source-checkout binary install implemented as
  `scripts/install-connector.sh`; distributable source-checkout release package
  implemented as `scripts/package-connector.sh` with binary, skill, connector
  source/docs, helper scripts, manifest, and SHA-256 checksum. Published
  registry/tap remains future work.
- CLI alias or package entrypoint beyond `cargo run --manifest-path connector-rs/Cargo.toml --`.
  Status: implemented as `bin/codex-connector`.
- Release binary install to a user prefix. Status: implemented as
  `scripts/install-connector.sh --prefix <dir>`, defaulting to `~/.local`.
- Interactive first-run setup. Status: implemented for TTY runs of
  `codex-connector init` without `--root`; it asks for allowed project roots,
  local port, optional public HTTPS origin, and optional connector skill roots.
  Non-interactive runs keep the previous current-directory default,
  TTY-attached scripts can pass `--no-interactive`, and agents should continue
  passing explicit flags. Non-loopback public URLs must use HTTPS.
- Upgrade notes for users who installed only the skill. Status: implemented in
  README and ChatGPT setup runbook.
- Release checklist that runs unit tests, CLI smoke tests, and a ChatGPT MCP
  smoke prompt. Status: implemented as `scripts/verify-release.sh` with local
  MCP smoke, installer smoke, package smoke, and printed manual ChatGPT prompt.
- Public CI signal. Status: implemented as `.github/workflows/ci.yml`; Linux
  and macOS run the release verifier, while Windows runs Rust fmt, clippy, and
  build as a compile smoke. Full Windows runtime tests remain future work
  because the shell tool currently requires a Bash-compatible shell.

## Differentiators To Preserve

DevSpace is strong as a direct remote coding environment. This project should
also keep the workflows that are not just a clone:

- Bridge Mode: send scrubbed local context to a web model without granting local
  tool access.
- Scrub gate: block obvious secrets before external transmission.
- File-based handoff ledger: outbox/inbox artifacts for model consultations.
- Agent-first routing: browser bridge, manual bridge, or MCP connector depending
  on what the local agent can do.
- Progressive trust: readonly first, review artifacts second, execute last.
- Provider breadth: ChatGPT Pro, Claude web, Grok, Gemini, and other web model
  surfaces for advisory workflows.

## Near-Term Implementation Order

1. Move the MCP service path to Rust. Status: readonly, OAuth, edit/write,
   shell, and worktree foundations implemented.
2. Add state/audit before write tools, so high-trust actions are observable.
3. Add edit/write before shell, because file mutation is easier to bound and
   test than arbitrary command execution. Status: exact-match and
   unified-diff patch foundations implemented.
4. Add shell with strict timeout/output/cwd limits. Status: implemented for
   non-interactive commands.
5. Add worktrees after shell, because useful worktree sessions need test/build
   commands. Status: managed worktree create/list/cleanup foundation
   implemented.
6. Add OAuth before recommending long-lived public ChatGPT connectors. Status:
   implemented as an owner approval flow with PKCE and refresh tokens.
