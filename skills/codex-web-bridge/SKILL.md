---
name: codex-web-bridge
description: Bridge Codex to web-based AI model products by packaging local task context, scrub-checking it, sending it through an approved browser session to ChatGPT Pro, Claude, Grok, Gemini, or another web model, waiting for the answer, and returning the model response to the user or Codex. Use when the user asks for GPT Pro, ChatGPT Pro, Claude web, Grok, Gemini web, web model bridge, external model consult, ask another model, second opinion, use a web AI model, or send local repo context to a browser-based model for planning, review, debugging, architecture discussion, or implementation guidance.
---

# Codex Web Bridge

Use this skill as a communication bridge between Codex and web-based AI model products. Codex handles context packaging, basic outbound safety checks, browser submission, waiting, and response capture. The target model and the user handle judgment.

## Boundary

This skill does:

- Build a bounded context packet from the current repo, diff, selected files, logs, and user question.
- Run a local scrub check before anything is sent to a third-party web model.
- Optionally create a file-based outbox/inbox handoff under `.codex-web-bridge/` for traceability.
- Use an approved browser surface to send the packet to ChatGPT Pro, Claude, Grok, Gemini, or another selected web model.
- Wait for completion and capture the full response.
- Return the response to the user or use it as input for the next Codex step when the user asked Codex to continue.

This skill does not:

- Decide whether the target model is correct.
- Force `FIX` / `DEFER` / `DISMISS` classifications.
- Let the web model directly edit local files or run local commands.
- Send context with `BLOCK` scrub findings.
- Post, publish, or share anything beyond the selected web model unless the user separately authorizes that action.

## Workflow

1. Define the bridge request.
   - State the destination provider: `chatgpt`, `claude`, `grok`, `gemini`, or `other`.
   - State the exact question for the target model.
   - State whether Codex should only report the response or continue executing after reading it.
   - State what local context is in scope and out of scope.
   - If the browser surface is not specified, ask one concise choice before sending: normal Chrome/browser session, Codex app side-panel browser, or manual paste. Tell the user the side-panel browser may require one-time sign-in on first use.

2. Build the context packet.
   - Prefer the bundled script when working in a Git repo:

```bash
python3 /path/to/codex-web-bridge/scripts/build_context_packet.py \
  --repo "$PWD" \
  --purpose planning \
  --question "What is the safest implementation plan for this change?" \
  --output /tmp/codex-web-bridge-packet.md
```

   - Add missing user context manually when needed: product goal, constraints, failed commands, exact error text, screenshots already approved for sharing, and what kind of answer is wanted.
   - Keep the packet scoped. Do not paste unrelated repo areas.
   - By default the script omits the local absolute repo path.

3. Run the scrub gate.

```bash
python3 /path/to/codex-web-bridge/scripts/scrub_context.py \
  /tmp/codex-web-bridge-packet.md \
  --fail-on block
```

   - `PASS`: proceed if the user has authorized this provider and data class.
   - `WARN`: review and redact or summarize before sending when appropriate.
   - `BLOCK`: do not send externally. Remove or summarize sensitive material, then rerun the scrub.

4. Create a file handoff when the interaction should be traceable or resumed later.

```bash
python3 /path/to/codex-web-bridge/scripts/bridge_handoff.py create \
  --repo "$PWD" \
  --provider chatgpt \
  --purpose planning \
  --surface ask \
  --question "What is the safest implementation plan for this change?" \
  --scope "Current implementation diff"
```

   - Send `01_PASTE_TO_WEB_MODEL.md` from the generated outbox.
   - Import the copied model response with `bridge_handoff.py done <handoff-id> --from-clipboard` or `--response-file`.
   - Keep `.codex-web-bridge/` local; it may contain proprietary task context and model output.

5. Submit through the web provider.
   - Read `references/providers.md` before using a provider that is not already familiar in the current browser session.
   - Use the selected browser surface:
     - `chrome`: Codex controls the user's normal Chrome/browser session, usually best when the user is already logged in.
     - `in-app-browser`: Codex uses the app side-panel browser. If this is the first time, ask the user to authenticate there once.
     - `manual`: do not automate the browser; give the outbox paste prompt to the user and import their response.
   - Reuse an existing relevant thread when it preserves context; start a new thread when the old one is stale, noisy, unrelated, or the user asks for a clean thread.
   - Verify the visible model/provider when possible. If model selection cannot be verified, say so.
   - Paste or type the final packet and submit it only after the scrub result is acceptable.

6. Wait and capture.
   - Read `references/response-capture.md` for provider-agnostic completion checks.
   - Do not abandon slow Pro/large-model responses just because they take 10-15 minutes.
   - If login, auth, CAPTCHA, browser interruption, or model access blocks the bridge, report the specific blocker and wait for user direction.
   - Capture the final answer with enough surrounding context to avoid losing code blocks, lists, or follow-up questions.

7. Return control.
   - If the user only asked for the model's answer, report it clearly with provider and thread context.
   - If the user asked Codex to continue, use the response as advisory input and proceed with normal Codex execution, including local verification for any code changes.
   - Preserve traceability: provider, model if known, packet summary, scrub result, response summary, and any browser blocker.

## Report Shape

```text
Bridge result: COMPLETE | INCOMPLETE | LOCAL-ONLY
Provider: [chatgpt | claude | grok | gemini | other]
Model: [visible model name or unknown]

Packet:
- Scope: ...
- Scrub: PASS | WARN | BLOCK resolved

Response:
[captured model answer or concise summary plus key excerpts]

Next:
- [what Codex will do next, or that no action was taken]
```

`COMPLETE` means the packet was sent and a final model response was captured. `INCOMPLETE` means the bridge could not finish because of auth, browser, provider, model, CAPTCHA, or user-decision blockers. `LOCAL-ONLY` means no web provider was used.

## Resources

- `scripts/build_context_packet.py`: Generate a bounded Markdown packet from Git state, diffs, selected evidence files, and bridge metadata.
- `scripts/scrub_context.py`: Scan a packet for obvious secrets and sensitive transmission risks before external submission.
- `scripts/bridge_handoff.py`: Create `.codex-web-bridge/outbox/<id>` prompts and import `.codex-web-bridge/inbox/<id>` responses.
- `references/providers.md`: Provider-specific browser guidance for ChatGPT, Claude, Grok, Gemini, and generic web models.
- `references/response-capture.md`: Rules for waiting on and extracting model responses.
