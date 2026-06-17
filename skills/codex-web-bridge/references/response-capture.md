# Response Capture

Use these rules after submitting a bridge packet to a web model.

## Completion Checks

Prefer provider UI signals over fixed sleeps:

- The stop-generating button disappears.
- The send button becomes available again.
- The assistant message stops changing.
- A copy/regenerate/action toolbar appears on the final response.
- The page exposes a final assistant message in the DOM.

Use fixed waits only as a fallback, and follow them with a concrete state check.

## Extraction Rules

- Capture the full final response, including code blocks, lists, tables, and explicit follow-up questions.
- If the response is long, capture a faithful summary plus any code blocks or commands verbatim.
- If the response is truncated by the provider UI, report truncation and ask whether to request continuation.
- If the model asks a clarification question, return that question instead of inventing an answer.
- If the page generated multiple candidate answers, identify which one was selected or visible.
- When using a file handoff, save the final answer through `bridge_handoff.py done <handoff-id>` so `.codex-web-bridge/inbox/<handoff-id>/response.md` is the durable response copy.

## Traceability

Report:

- Provider and model if visible.
- Browser surface: normal Chrome/browser session, Codex app side-panel browser, or manual paste.
- Thread URL or enough context to identify the conversation.
- Packet scope and scrub result.
- Outbox/inbox handoff id when `bridge_handoff.py` was used.
- Whether the response was complete, truncated, interrupted, or blocked.

## No Extra Judgment

Do not classify the response as correct or incorrect as part of the bridge. If the user asked Codex to continue executing, treat the model response as advisory input and use normal Codex verification for any subsequent local changes.
