# 更新日志

本项目遵循语义化版本风格记录用户可见变更。

## [Unreleased]

### Changed

- 将项目从 `review-gate` 重构为 `codex-web-bridge`，核心定位从“审查 gate”收敛为“Codex 到网页端强模型的通信桥”。
- 将 skill 目录改为 `skills/codex-web-bridge`。
- 将 packet builder 的主参数改为 `--provider`、`--purpose`、`--question`，并保留旧 `--mode` / `--decision` 的兼容别名。

### Added

- 新增 provider 指南，覆盖 ChatGPT、Claude、Grok、Gemini 和其他网页模型。
- 新增响应抓取指南，明确等待、完整性和 traceability 规则。
- 新增 `bridge_handoff.py`，支持用 `.codex-web-bridge/outbox/<id>` 生成网页模型粘贴内容，并用 `.codex-web-bridge/inbox/<id>` 保存回复。
- 新增浏览器 surface 选择说明，支持普通 Chrome/浏览器、Codex 应用侧边栏浏览器和手动粘贴，并提示侧边栏首次使用可能需要登录认证。
- 新增 MCP Connector Mode 设计参考，用于 DevSpace-like 工作流，让 ChatGPT Pro 或其他 MCP host 在用户授权后连接本地 workspace，服务不支持浏览器操作的 agent/host 场景。
- 新增 `connector/` 只读优先脚手架：包含 trust 模型与 allowed roots 校验（`config.py`）、路径包含边界（`workspace.py`）、只读工具面与权限分级（`tools.py`）、本地 JSON-RPC 服务（`server.py`，默认 loopback + owner token），以及路径包含与权限分级测试（`tests/test_connector.py`）。写文件/shell/worktree 等 execute 工具尚未实现，须在独立信任模型和测试就绪后再加入。
- connector 实现标准 MCP 协议层（`protocol.py`）：`initialize` 协议版本协商（`2025-06-18` / `2025-03-26` / `2024-11-05`）+ `serverInfo` + `tools` 能力、`notifications/initialized`、`ping`，以及符合规范的 `tools/list`（含 JSON Schema `inputSchema`）和 `tools/call`（`content` 文本块 + `structuredContent` + `isError`）。`server.py` 改为纯 HTTP 传输，通知返回 HTTP 202 空响应。新增 `tests/test_protocol.py` 覆盖握手、版本协商、工具 schema、工具执行错误分流。这样 ChatGPT Pro、Claude 等 MCP host 可以直接连接本地 connector。

### Security

- connector 加固（基于安全与 MCP 规范双重审查）：
  - 路径包含改用 realpath + 大小写归一的组件比对，拒绝 `..` 与 final symlink；`search` 对每个候选重新校验并跳过 symlink，修复树内 symlink 读到 root 外文件的漏洞。
  - `search` 增加时间、扫描文件数与单文件大小上限；workspace registry 加 LRU 上限，防内存耗尽。
  - `open_workspace` 不再回传本机绝对路径（只回 basename）；git 失败不再转发 stderr，降低信息泄漏。
  - HTTP 层：校验 `Origin`（防 DNS-rebinding）、要求 `Content-Type: application/json`、`GET`/`DELETE` 返回 405、增加 `nosniff` / `no-store` 头、owner token 常量时间比对、加固 `Content-Length` 解析。
  - MCP 会话：`initialize` 颁发 `Mcp-Session-Id`，per-session 跟踪初始化状态（替换原先跨线程共享的 racy flag）；`initialize` 之前的请求（除 `ping`）被拒绝；信任级别不足的工具调用返回 `isError` 结果而非协议错误。
  - handler 内的 `OSError`（TOCTOU、权限）兜底为 `isError`，请求层加 catch-all，避免崩线程或泄漏 traceback / 绝对路径。
- 新增 `tests/test_server.py`：用真实 `ThreadingHTTPServer` + `http.client` 做 HTTP 传输端到端测试（401/403/405/415/404、Origin、安全头、会话握手、open+read、路径逃逸）。测试合计 48 个，可用 `python3 -m unittest discover -s connector/tests -t .` 运行。

### Fixed

- `bridge_handoff.py list` 现在也会显示只有 inbox 响应、没有 outbox manifest 的网页响应导入记录。
