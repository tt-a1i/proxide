# Codex Web Bridge

`codex-web-bridge` 是一个 Codex Skill，用来把 Codex 的当前任务上下文发送给网页端强模型，再把回复带回 Codex 或交给人继续判断。

它的默认边界很窄：**只做通信，不替人和目标模型做判断**。

它负责：

- 从当前 repo、diff、未跟踪文件、指定证据文件和用户问题生成 context packet；
- 外发前扫描常见 secret、token、private key、内部 URL 等风险；
- 用 `.codex-web-bridge/outbox` / `.codex-web-bridge/inbox` 保存可追踪的本地交接记录；
- 让用户选择普通 Chrome/浏览器、Codex 应用侧边栏浏览器或手动粘贴；
- 通过浏览器把 packet 发给 ChatGPT Pro、Claude、Grok、Gemini 等网页模型；
- 等待模型生成完成并抓取完整回复；
- 把回复交回 Codex 或用户。

它不负责：

- 判断模型回答对不对；
- 强制本地 reconciliation；
- 自动决定 `FIX / DEFER / DISMISS`；
- 让网页模型直接改本地代码或运行本地命令；
- 未经确认发布社交评论、上传文件或执行外部副作用。

## 两种模式

### Bridge Mode

这是当前已实现的默认模式：Codex 打包上下文，scrub 之后通过普通 Chrome/浏览器、Codex 应用侧边栏浏览器或手动粘贴发送给 ChatGPT Pro、Claude、Grok、Gemini 等网页模型，再把回复带回 Codex。

适合：

- 本地 Codex 支持浏览器操作；
- 用户只想借用网页强模型做规划、审查、解释；
- 不希望网页模型直接读写本地文件或运行命令。

### MCP Connector Mode

这是新增的设计方向：像 DevSpace 那样启动一个本地 MCP connector，让 ChatGPT Pro、Claude 或其他 MCP host 连接到用户允许的本地 workspace。这样即使本地 agent 不支持浏览器操作，也可以让网页端 GPT Pro 使用本地项目上下文。

这个模式和 Bridge Mode 的信任边界完全不同：Connector Mode 是网页模型主动调用本地工具。第一版应该默认只读，只开放 workspace、read、search、list、git status/diff 等能力；写文件、运行 shell、worktree 执行都必须是显式高信任升级。

参考设计见 [skills/codex-web-bridge/references/mcp-connector-mode.md](skills/codex-web-bridge/references/mcp-connector-mode.md)。

## 安装

从 GitHub 安装：

```text
Use $skill-installer to install https://github.com/tt-a1i/codex-web-bridge/tree/main/skills/codex-web-bridge
```

安装后重启 Codex。

本地开发时，也可以在仓库根目录用相对路径安装：

```text
Use $skill-installer to install ./skills/codex-web-bridge
```

## 使用

把当前任务发给 ChatGPT Pro：

```text
Use $codex-web-bridge to ask ChatGPT Pro for a plan using the current diff and relevant files.
```

把 bug 上下文发给 Claude：

```text
Use $codex-web-bridge to send this failing test and implementation context to Claude, then bring the answer back.
```

支持的 provider 目标：

- `chatgpt`：ChatGPT / GPT Pro / GPT-5.5 Pro 网页端；
- `claude`：Claude 网页端；
- `grok`：Grok 网页端；
- `gemini`：Gemini 网页端；
- `other`：其他有输入框和输出区的网页模型。

## 工作流

1. 明确要问哪个网页模型，以及要问什么。
2. 用 `build_context_packet.py` 打包上下文。
3. 用 `scrub_context.py` 做外发前扫描。
4. 选择浏览器 surface：普通 Chrome/浏览器、Codex 应用侧边栏浏览器，或手动粘贴。
5. 可选：用 `bridge_handoff.py create` 生成 outbox 目录和可直接粘贴的 prompt。
6. 通过浏览器打开或复用对应网页模型线程。
7. 发送 scrub 通过后的 packet。
8. 等待模型完整回复。
9. 抓取回复，用 `bridge_handoff.py done` 写回 inbox，或直接交回 Codex / 用户。

如果选择 Codex 应用侧边栏浏览器，第一次访问对应网页模型时可能需要用户在侧边栏里登录认证一次；它和用户日常 Chrome 登录态不一定共享。

如果本地 agent 不支持浏览器操作，优先考虑 MCP Connector Mode：让网页端 GPT Pro 作为 MCP host 连接本地 connector，而不是要求本地 agent 操作浏览器。

## 脚本

生成 context packet：

```bash
python3 skills/codex-web-bridge/scripts/build_context_packet.py \
  --repo . \
  --provider chatgpt \
  --purpose planning \
  --question "What is the safest implementation plan for this change?" \
  --scope "Current implementation diff" \
  --output /tmp/codex-web-bridge-packet.md
```

扫描敏感内容：

```bash
python3 skills/codex-web-bridge/scripts/scrub_context.py \
  /tmp/codex-web-bridge-packet.md \
  --fail-on block
```

生成本地 outbox 交接：

```bash
python3 skills/codex-web-bridge/scripts/bridge_handoff.py create \
  --repo . \
  --provider chatgpt \
  --purpose planning \
  --surface ask \
  --question "What is the safest implementation plan for this change?" \
  --scope "Current implementation diff"
```

把网页模型回复写回 inbox：

```bash
python3 skills/codex-web-bridge/scripts/bridge_handoff.py done \
  20260617T120000Z-chatgpt-planning \
  --from-clipboard
```

默认生成的 packet 不包含本机仓库绝对路径，减少外发时泄漏本地用户名或目录结构。确实需要时可传 `--include-repo-path`。

`bridge_handoff.py` 默认写入 `.codex-web-bridge/`，该目录是本地运行态，已被 `.gitignore` 忽略。

## 目录

```text
skills/codex-web-bridge/
├── SKILL.md
├── agents/openai.yaml
├── references/
│   ├── providers.md
│   ├── mcp-connector-mode.md
│   └── response-capture.md
└── scripts/
    ├── bridge_handoff.py
    ├── build_context_packet.py
    └── scrub_context.py
```

## 隐私边界

`scrub_context.py` 只能发现常见 secret 形态，不是完整 DLP 系统。外发前仍然要确认上下文是否包含客户数据、内部链接、日志、截图、账号信息或其他不该发送给目标网页模型的内容。

## 关系与授权

这个项目最初受 [christianaranda/codex-pro-skill](https://github.com/christianaranda/codex-pro-skill) 和 [steipete/oracle](https://github.com/steipete/oracle) 这类“把本地上下文交给强模型”的工作流启发，但定位更窄：只做 Codex 到网页端模型的通信桥。

代码以 MIT License 发布。
