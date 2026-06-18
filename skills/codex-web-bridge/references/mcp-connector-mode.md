# MCP Connector Mode

Use this reference when the user wants a DevSpace-like flow where ChatGPT Pro,
Claude, or another MCP-capable web host connects to a local workspace through
MCP instead of relying on Codex browser automation.

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
- `execute`: edit/write and shell tools are available only after explicit user
  opt-in. Prefer worktree mode for execution.

The first public Connector Mode should ship `readonly` before `execute`.

## Required Safety Boundaries

- Allowlist workspace roots. Avoid broad roots such as `~`, `/`, or a whole user
  profile directory.
- Bind locally by default. Public tunnel setup must be explicit and user-owned.
- Require an owner token or equivalent approval before any remote MCP host can
  connect.
- Keep write, edit, shell, and worktree tools disabled by default.
- Never expose arbitrary local shell access in readonly or review mode.
- Log tool calls and workspace openings, but avoid logging shell command bodies
  or sensitive file contents by default.
- Report that a public tunnel URL is not a secret; authentication still matters.
- Treat worktrees as workflow isolation, not a security boundary.

## Tool Surface

Readonly mode:

- `open_workspace`: open a path inside an allowed root and return a workspace id.
- `read`: read bounded text from a workspace-relative file.
- `search`: search text with ignore rules and bounded results.
- `list`: list a workspace-relative directory with bounded entries.
- `git_status`: return branch, HEAD, and short status.
- `git_diff`: return bounded diff/stat for the workspace.

Review mode:

- All readonly tools.
- `create_note`: save a model review note under bridge runtime state.
- `show_session`: summarize what was opened/read/searched.

Execute mode:

- All review tools.
- `edit` / `write`: scoped file mutation under the workspace.
- `shell`: command execution with timeout, cwd containment, and an explicit
  warning that local user permissions apply.
- `open_worktree`: create or open an isolated worktree for parallel execution.

## User-Facing Choice

When a user asks to use GPT Pro from an agent that cannot operate a browser,
offer:

1. Browser Bridge: Codex operates Chrome, the Codex side-panel browser, or a
   manual paste flow.
2. MCP Connector: ChatGPT Pro or another MCP host connects to this local
   workspace. Works without browser automation in the local agent, but grants
   the web host local tool access.

Mention that Connector Mode requires setup: local server, allowed roots, owner
approval, and usually a user-managed HTTPS tunnel for web hosts.

## Non-Goals

- Do not promise doubled limits as a product guarantee.
- Do not bypass provider terms, authentication, CAPTCHAs, or paywalls.
- Do not hide that execute mode gives the remote host local code-editing and
  command-running power.
- Do not merge Connector Mode into Bridge Mode's low-trust defaults.
