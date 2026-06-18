# 贡献指南

这个仓库的发布物是 `skills/codex-web-bridge`。skill 目录内只放 Codex 执行通信桥任务需要的内容；仓库级维护说明放在根目录。

## 维护校验

提交或推送前至少运行：

```bash
find . -path ./.git -prune -o -maxdepth 5 -type f -print | sort
git status --short
git diff --stat
python3 -m py_compile \
  skills/codex-web-bridge/scripts/bridge_handoff.py \
  skills/codex-web-bridge/scripts/build_context_packet.py \
  skills/codex-web-bridge/scripts/scrub_context.py
ruby -ryaml -e 'front=File.read("skills/codex-web-bridge/SKILL.md").split(/^---\s*$/)[1]; YAML.safe_load(front).fetch("name"); YAML.load_file("skills/codex-web-bridge/agents/openai.yaml").fetch("interface"); puts "yaml OK"'
python3 skills/codex-web-bridge/scripts/build_context_packet.py \
  --repo . \
  --provider chatgpt \
  --purpose planning \
  --question "Verify codex-web-bridge before release" \
  --scope "Current repository state" \
  --output /tmp/codex-web-bridge-packet.md
python3 skills/codex-web-bridge/scripts/scrub_context.py /tmp/codex-web-bridge-packet.md --fail-on block
rm -rf /tmp/codex-web-bridge-handoff
python3 skills/codex-web-bridge/scripts/bridge_handoff.py create \
  --repo . \
  --bridge-dir /tmp/codex-web-bridge-handoff \
  --provider chatgpt \
  --purpose planning \
  --surface in-app-browser \
  --question "Verify file handoff protocol" \
  --scope "Current repository state"
python3 skills/codex-web-bridge/scripts/bridge_handoff.py done \
  "$(basename "$(find /tmp/codex-web-bridge-handoff/outbox -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1)")" \
  --bridge-dir /tmp/codex-web-bridge-handoff \
  --response-text "Synthetic model response for validation."
python3 skills/codex-web-bridge/scripts/bridge_handoff.py done \
  response-only-smoke \
  --bridge-dir /tmp/codex-web-bridge-handoff \
  --provider chatgpt \
  --response-text "Synthetic response-only import."
python3 skills/codex-web-bridge/scripts/bridge_handoff.py list \
  --bridge-dir /tmp/codex-web-bridge-handoff
git diff --check
```

如果本机装了 `PyYAML`，也运行官方 skill 校验脚本：

```bash
python3 /path/to/skill-creator/scripts/quick_validate.py skills/codex-web-bridge
```

如果本机装了 secret 扫描工具，也运行：

```bash
gitleaks detect --no-git --source . --redact --verbose
trufflehog filesystem . --no-update --fail
```

## 修改原则

- 默认 README 使用中文。
- `skills/codex-web-bridge/SKILL.md` 保持短而可执行，把 provider 细节和响应抓取规则放进 `references/`。
- 只有重复、易错、需要确定性的步骤才放进 `scripts/`。
- 不要在 skill 目录里增加 README、安装指南、发布日志等维护文档。
- 修改外发逻辑时，优先保证“不发送 BLOCK scrub finding”这个安全边界。
- MCP Connector Mode 必须和默认 Bridge Mode 分离，默认只读，不要把写入或 shell 权限塞进低信任路径。
- `.codex-web-bridge/` 是本地运行态，不要提交真实 outbox/inbox 内容。
- 不要把本项目重新扩大成审查框架；MCP Connector Mode 是独立高信任模式，不能改变默认网页模型通信桥的低信任边界。
