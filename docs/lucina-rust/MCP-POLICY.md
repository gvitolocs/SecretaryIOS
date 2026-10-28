# Lucina MCP: who may make Lucina act (policy, 2026-10-28)

Lucina MCP (`secretary-mcp` on p1) lets clients talk to Lucina with a token. Every token names its caller
(`claude-code`, `chatgpt`, `www` for Cursor, …); secretary-mcp sets it, the client cannot. Two kinds of
caller:

| Kind | Callers (2026-10-28) | `ask_lucina` turns |
|---|---|---|
| The owner's own assistant apps (`LUCINA_MCP_OWNER_CLIENTS`, default `chatgpt,claude,grok`) | ChatGPT, claude.ai, Grok | He types into them and they relay his request. With `LUCINA_MCP_TRUSTED=true` (live) they act as he would; with `false` they are held to `mcp::ALLOWED_ACTIONS`. |
| Agents: everyone else | Claude Code sessions, Cursor (`www`), ZCode, Hermes, secretary-code, unknown | **Consult**: answers only. Lucina never starts work because an agent asked in prose. |

## Consult turns (agents)

- Run: replies; reading reminders (`list_reminders`); reading notes (named note, list); grocery reads
  (`mcp::CONSULT_GROCERY_INTENTS`); the memo and photo searches.
- Never run from prose: Harness / `cursor_task`, delegation, computer tasks (the Outlook route included),
  `schedule_reminder`, notes writes, tasks, calendar writes, shopping writes, posts, Meshy, game control,
  YouTube jobs, spreadsheet writes, Qwen restarts.
- What happens instead: Lucina answers the message as a consultation ("what I would do"), and the chat
  response carries `consult: true`, `notRun: <action>` and `hint: <the MCP tool to use>`.
- One message may act on one kind of request when the agent passes `allow` on `ask_lucina`
  (`reminders`, `notes`, `tasks`, `shopping`); secretary-mcp writes it in the prefix
  (`[via Lucina MCP, from claude-code; allow=reminders]`). Jobs, computer work and posts have no flag:
  they have their own tools (`agent_run`, `computer_task`) with their own scopes and budgets.
- Go-aheads: `lucina_decide` (`docs/design/decisions-api.md`), never `ask_lucina`.

## Every MCP turn (both kinds)

- Tier-0 rules: `cursor_task` needs an explicit request to delegate (Cursor, Codex, Harness, "fai fare a…"),
  `schedule_reminder` a reminder verb (or a short note-to-self starting with the time), not a timestamp in a
  report. When Jev reads a side-effect tier-0 rule as anything else, an MCP turn becomes plain chat
  (`jev_route::mcp_falls_back_to_chat`).
- Attribution: the timeline items carry `meta.via = "mcp"`, `meta.caller` and `meta.conversation`; Honcho
  stores the caller's words under its own peer (`mcp-<caller>`, `metadata.speaker = "agent"`), in the
  owner's scope but never as his messages; the memory label says "written by that agent, not by Giuseppe".
  Decisions read only his own words as evidence.
- Secrets, the owner's plugins (Instagram, Pixiv), Meshy credits and delegation switches stay owner-only.

## Why

On 2026-10-28 an agent's "need GO/NO now … restart cloudflared-cdn" started a Harness job under the owner's
authority (`LUCINA_MCP_TRUSTED=true` skipped every guard), and its follow-up "I applied it myself at 17:16"
became a reminder request. Agents' prose is technical and imperative by nature; regexes written for the
owner's chat over-match it, and trusting it gave every token holder the owner's hands.
