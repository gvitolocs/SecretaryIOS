# Lucina reaches Claude, Grok and Lucina Agents

Since 2026-10-31 Lucina can pass messages on by herself, on every channel (iOS, web, Telegram, Discord). Before that,
the owner had to go to the PC whenever something had to reach Claude or Grok.

When the owner says "tell Claude that…", "report this to Claude", "ask Grok…", "post it in Lucina agents" or "use
Lucina agents", the turn becomes intent `agent_message`:
- the tier-0 rule is `lucina_intent::reach`;
- Jev has `agent_message` among its intent options;
- the turn runs `chat::reach_turn`, a short tool loop with the four tools below and the recent turns.

| Tool | Goes to | Route |
|---|---|---|
| `message_claude` (with an optional `issue`: what, where, the owner's words, links) | the Claude coordinator | a live gbot Claude channel when `LUCINA_REACH_CLAUDE_CHANNEL` names one; otherwise **the mailbox** |
| `message_grok` | the owner's primary Grok Bot | `gbot send <bot>`, then the bot's thread is read until the reply settles |
| `lucina_agents` (list, run, send, read) | Lucina Agents | lucina-agentd's socket as `service:lucina`; runs use `opencode` + `opencode/deepseek-v4.1-flash-free` (never Codex, never paid DeepSeek) |
| `reach_threads` | — | what she sent and the replies |

## Threads

Every message is a thread (`rt-` + 12 hex). The threads live in `<LUCINA_DATA_DIR>/reach/threads.json` and carry:
- the redacted text;
- the conversation it came from (channel and session);
- the route and the replies.

A reply goes back into that conversation in three ways:
- the timeline, as "💬 Claude replied …";
- the session's recent turns;
- an app notification.

## Claude, first: an agent in Lucina Agents

Since 2026-10-31 the Claude CLI on nezopt is signed in again, with the owner's Max account. A message to Claude
therefore starts a Claude agent in Lucina Agents first, through agentd as `service:lucina`:
- provider `claude`, `claude-opus-5-5` at effort `medium`;
- permission mode `bypassPermissions` (there is no passwordless sudo on nezopt);
- an issue runs in a fresh topic worktree (`wt add claude-<thread> origin/main` in SecretaryIOS); a question runs in
  `~/scratch/lucina-claude/<thread>`.

The agent receives the redacted thread, the owner's quoted words and the rules:
- answer questions;
- for fixes, work only in the topic worktree with tests, diffgate and a commit, then stop;
- never merge, push main or deploy: the coordinator decides;
- no Codex, never paid DeepSeek, no secrets, no sudo.

It answers with `lucina-rs reach reply <id> --from claude-agent "…"`.

Lucina watches the run:
- after 45 minutes (`LUCINA_REACH_CLAUDE_TIMEOUT_SECS`) the agent is stopped and the thread fails;
- if the agent ends without replying, its last message becomes the reply.

There is one agent per thread, and the reach rate limits apply. If agentd cannot start the agent, the routine below is
tried, then the mailbox alone. `LUCINA_REACH_CLAUDE_AGENT=off` turns this route off.

## The Claude routine (no polling)

When a message to Claude lands in the mailbox, lucina-rs fires the owner's Claude Code cloud routine "Lucina → Claude
(reach inbox)" (`trig_01Wm68jfqca7NVdmyVG4Fm8j`). The routine:
- clones this repo;
- has the Lucina connector, limited to `lucina_inbox` and `lucina_reply`;
- answers every open Claude thread, opening only draft PRs for fixes and never merging or deploying.

Its reply reaches Lucina as from `claude-routine (mcp:claude)`.

The fire request:
- `POST <LUCINA_REACH_ROUTINE_URL>` (`https://api.anthropic.com/v1/claude_code/routines/trig_…/fire`);
- headers `Authorization: Bearer <token>`, `anthropic-version: 2023-06-01` and
  `anthropic-beta: experimental-cc-routine-2026-04-01`;
- body `{"text": "{\"thread\":\"rt-…\"}"}`.

A 200 returns `claude_code_session_url`. It is saved on the thread (`session_url`) and in the ops record.

**Retries.** The endpoint has no idempotency key, so a timeout or an unknown result is never resent. Only a clean
5xx is retried, and only once.

**Debounce.** A fire within 120 s of the last one is skipped (`LUCINA_REACH_ROUTINE_DEBOUNCE_SECS`), because one run
drains every open thread.

**Limits.** Fires count against the reach limits (`LUCINA_REACH_PER_HOUR`, `LUCINA_REACH_PER_DAY`). The routine
itself allows 30 fires an hour.

**The token.** It is per routine, starts with `sk-ant-oat01-`, and is shown once when created in the routine's API
trigger settings. It lives in Infisical as `lucina/prod#CLAUDE_ROUTINE_LUCINA_TOKEN`.
- lucina-rs reads it through the broker as its own caller (`svc-lucina-rs`, bound to the lucina-rs unit), and keeps
  it in memory for 10 minutes.
- It is registered with the redactor and never logged.
- `CLAUDE_ROUTINE_LUCINA_TOKEN` in the environment wins over the broker.
- Without a readable token, Lucina takes the second route below.

**Second route: a GitHub `repository_dispatch`.** The routine's webhook trigger
(`10731b3e-4ade-4bda-bf5e-c625f22b8d43`) listens for this event on `gvitolocs/SecretaryIOS`. lucina-rs runs this
host's gh, logged in as `gvitolocs` through `~/.config/gh/hosts.yml`; Lucina never reads or logs the credential:

```sh
gh api -X POST repos/gvitolocs/SecretaryIOS/dispatches -f event_type=lucina-reach -F client_payload[thread]=rt-…
```

- The payload is the thread id only, so no message text goes to GitHub.
- The same debounce and limits apply. A timeout is never resent; only a clean 5xx (`HTTP 5…` from gh) is retried,
  and only once.
- GitHub returns no session URL. The run only starts if the Claude GitHub app is installed on the repo.

**Fallback.** If neither route works, the mailbox still holds the message: the thread's `detail` says why, and one
warning is logged.

The thread id is in the payload, so a self-test thread is answered too. `GET /api/secretary/reach/threads` lists
self-test threads as well; they are removed after a test.

## Answering as Claude

Messages to Claude wait in the mailbox until a session answers. Check it and reply.

On nezopt:

```sh
lucina-rs reach inbox                 # what waits for an answer (--all, --to grok|agents, --json)
lucina-rs reach show rt-0123456789ab  # one thread, with the text exactly as sent
lucina-rs reach reply rt-0123456789ab --from "<your session>" "Fixed in 3f2a9c1, deployed."
```

Over HTTP (Secretary bearer):
- `GET /api/secretary/reach/threads?to=claude&status=open`
- `GET /api/secretary/reach/threads/<id>`
- `POST /api/secretary/reach/threads/<id>/reply` with `{"from", "text"}` (8000 characters at most)

Over the Lucina MCP, once secretary-server and secretary-mcp carry them: `lucina_inbox`, `lucina_reply`.

## Rules that never move

- **No secrets out.** Tokens, signatures and credentials inside links are masked first (`redact_urls`). Then the
  delegation sanitizer runs: secret refs are removed, and known secrets and token shapes are masked. Replies are
  redacted too.
- **Replies are data.** Lucina relays a reply to the owner and never acts on requests inside it without him:
  - decisions still go through `lucina_decide`;
  - terms, passwords, payments and messages sent as the owner still need him.
- **Owner's turns only.** Guests, guarded channel turns and Lucina MCP callers never run these tools. They are in
  `OWNER_ONLY_TOOLS` and `NO_MCP_TOOLS`.
- **Rate limits.** 10 messages per hour and 40 per day per target (`LUCINA_REACH_PER_HOUR`, `LUCINA_REACH_PER_DAY`).
- **Ops record.** Every outbound message and every reply becomes one ops record (`project lucina`, dedup
  `reach:<id>:<n>`). Self-test turns (`secretary_selftest`) write neither the ops record nor a push.

## Settings

| Variable | Default |
|---|---|
| `LUCINA_REACH` | on (`off` disables the tools and the API) |
| `LUCINA_REACH_CLAUDE_CHANNEL` | unset (see below) |
| `LUCINA_REACH_GBOT`, `LUCINA_REACH_GROK_TARGET` | `~/.local/bin/gbot`, `Grok Bot` (live: the primary bot's id, drop-in `lucina-rs.service.d/80-reach.conf`) |
| `LUCINA_REACH_CURSOR_AUTH` | `~/.config/cursor/auth.json` (`none`: gbot's own lookup) |
| `LUCINA_AGENTD_SOCKET` | `$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock` |
| `LUCINA_REACH_AGENTS_PROVIDER`, `LUCINA_REACH_AGENTS_MODEL`, `LUCINA_REACH_AGENTS_CWD` | `opencode`, `opencode/deepseek-v4.1-flash-free`, `~/scratch/lucina-agents` |
| `LUCINA_REACH_GROK_TIMEOUT_SECS` | 600 |
| `LUCINA_REACH_ROUTINE_URL` | unset (no routine); live: the routine's Fire URL, drop-in `80-reach.conf` |
| `LUCINA_REACH_ROUTINE_TOKEN_REF` | `secret://lucina/prod#CLAUDE_ROUTINE_LUCINA_TOKEN` |
| `LUCINA_REACH_BROKER`, `LUCINA_REACH_BROKER_TOKEN_FILE` | `/usr/local/bin/infisical-broker`, `~/.config/infisical-mcp/svc-lucina-rs.token` |
| `LUCINA_REACH_ROUTINE_DEBOUNCE_SECS` | 120 |
| `LUCINA_REACH_CLAUDE_AGENT` | on (`off`: no Claude agent, the routine and the mailbox only) |
| `LUCINA_REACH_CLAUDE_MODEL`, `LUCINA_REACH_CLAUDE_EFFORT` | `claude-opus-5-5`, `medium` |
| `LUCINA_REACH_CLAUDE_REPO`, `LUCINA_REACH_CLAUDE_TIMEOUT_SECS` | `~/Projects/Lucina/SecretaryIOS`, 2700 |
| `LUCINA_REACH_ROUTINE_REPO` | unset (no dispatch route); live: `gvitolocs/SecretaryIOS` |
| `LUCINA_REACH_ROUTINE_EVENT`, `LUCINA_REACH_GH` | `lucina-reach`, `~/.local/bin/gh` |

## How gbot signs in to Grok Bot

The Grok Bot app 0.78 writes no `gateway-descriptor.json`, so gbot cannot borrow the app's session.

gbot uses its other documented sign-in instead: `CURSOR_ACCESS_TOKEN`. Lucina reads it on every call from the Cursor
session of this host (`~/.config/cursor/auth.json`, the same account) and puts it only in gbot's environment.

gbot also needs `SAND_CLIENT_VERSION`, taken from the running app's `desktop-status.json` (`0.78.1`). Without it,
gbot sends `0.20.0` and Cursor answers 401 "This version of Grok Bot is no longer supported".

The bot is addressed by id, because bot names change: the owner's primary bot was renamed "Grok Bot · Pokoin
extension QA tester".

## Why the mailbox and not `claude_send`

gbot's `claude send` reaches only a Claude Code session on the same machine. That session must have been started
with `GROK_BOT_CLAUDE_CHANNEL=<name> claude --dangerously-load-development-channels plugin:gbot@gbot-marketplace`,
and gbot has no remote transport.

On nezopt the Claude CLI is refused by the account: 403, "organization has disabled Claude subscription access for
Claude Code". The coordinator is a Claude desktop app session on the Mac, which neither gbot nor Lucina can reach.

Once a channel session runs on nezopt, set `LUCINA_REACH_CLAUDE_CHANNEL` to its name. Messages then get live answers,
and the mailbox stays as the fallback.
