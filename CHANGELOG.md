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

### Fixed

- `bridge_handoff.py list` 现在也会显示只有 inbox 响应、没有 outbox manifest 的网页响应导入记录。
