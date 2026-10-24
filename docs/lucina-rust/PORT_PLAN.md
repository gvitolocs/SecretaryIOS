# Lucina Rust port plan

Companion to `INVENTORY.md` (module marks, data formats, routes). Written 2026-10-21. Crate versions were checked on
crates.io the same day with a cutoff of 2026-10-07 (at least two weeks old).

## 1. Goals and constraints

- One Rust binary replaces every **PORT** module of the Node Lucina. **OTHER-BOT** code stays in the Node container,
  which keeps port 8788 for its routes (`/oauth2callback`, `/api/poko/*`, Bot Hospital).
- The Rust Lucina runs side by side on **port 8790** (free on nezopt; 18790 is taken) and takes over one surface at a
  time. Node stays the rollback until each phase passes parity and a 7-day watch.
- Every persisted file and the grocery schema stay byte-compatible in both directions (INVENTORY §3), so a rollback
  never needs a data migration.
- `/api/secretary/*` is identical to Lucina `docs/secretary-api.md` (main plus the worktree's shopping v2 and device
  sections). `secretary-server` on p1 must not notice the switch.
- Reasoning is DeepSeek only (`LUCINA_DEEPSEEK_ONLY`, owner decision 2026-10-21). Gemini stays for vision and STT
  fallback.

## 2. Target crate layout

Refines the proposal. Two changes: a `lucina-agent` crate for the turn pipeline (it has no home in the proposal and is
the largest piece), and shared low-level pieces (config, owner identity, leases) in `lucina-core` so channels and the
server do not depend on each other. The other agent's scaffold on branch `lucina-rs`
(`SecretaryIOS-lucina/crates/lucina-store`: atomic writes, timeline, reminders, parser, scheduler, jobs, ULID) fits
this layout unchanged.

```
crates/
  lucina-core      config (env names from INVENTORY §7), owner identity + legacy aliases, time zone helpers,
                   leases (gateway-lock record format, kinds telegram/discord/scheduler), redaction,
                   invisible-Unicode guard, error types
  lucina-llm       OpenAI-compatible client (DeepSeek): chat, JSON mode, tool calling, SSE streaming, wait-out
                   (10 s, 30 s) with progress events, 401/402 breaker; Gemini generateContent (vision, STT);
                   local STT runner (faster-whisper subprocess or whisper.cpp HTTP)
  lucina-memory    Honcho v3 REST client (peers, sessions, peers-in-session, messages, context, search),
                   mirror targets, policy-gated context builder with the Node char budgets
  lucina-store     timeline JSONL (ULID, index, rotation, tail-follow, dedupe), reminders store + NL parser,
                   recent-chat, chat registry, secretary jobs, device stores; flock + atomic rename everywhere
  lucina-grocery   sqlx Postgres repository (schema `grocery`, migrations 001/002 as-is), grocery intent parser,
                   product canonicalisation, shopping v2 ops; catalog/offer adapters in phase 6
  lucina-tools     Tool trait, registry, JSON Schema (schemars) + validation (jsonschema), risk classes,
                   confirmation flow, audit log; built-in tools (§5)
  lucina-agent     turn metadata, tier-0 classifier (port of lucina-intent + intent-heuristics), context policies,
                   prompt assets (persona, recovery companion, operator brief), planner, reply/skill generation,
                   capability hints, async job runner
  lucina-channels  telegram (Bot API polling, media, formatting), discord (gateway, admin ops), alexa adapter,
                   owner filters and timeline hooks per channel
  lucina-server    axum: /health, /api/secretary/* (incl. SSE), /api/alexa/lucina, /grocery/* (phase 6),
                   /api/internal/reminders/* (phase 2 shim target), /api/internal/leases (read-only)
  lucina           binary: wiring, singleton supervisor (leases), scheduler loop, CLI subcommands:
                   serve | check-data | backfill-timeline | grocery-migrate | parity (record/replay)
```

Dependency direction: `core ← llm, memory, store, grocery ← tools ← agent ← channels, server ← lucina`.
Reuse from the workspace: `secretary-core` shopping catalog types and validator (`src/shopping/catalog.rs`), and
`secretary-server`'s Lucina client as a client-side contract test.

## 3. Recommended crates (pinned, all published before 2026-10-07)

| Purpose | Crate | Version | Notes |
|---|---|---|---|
| Runtime | `tokio` | 1.53.2 | Workspace already locks 1.54.0; require `>=1.53`. |
| HTTP server | `axum` | 0.8.9 | Same as `secretary-server`. |
| Middleware | `tower` 0.5.3, `tower-http` | 0.6.11 | Stay on the 0.6 line used by `secretary-server` (0.7.1 exists). |
| HTTP client | `reqwest` | 0.12.28 | Workspace pin (`rustls-tls`, `json`, `stream`). |
| SSE parsing (DeepSeek stream) | `eventsource-stream` | 0.2.3 | Old but stable; or a 60-line parser. Not `async-openai` 0.42.1 (needs reqwest 0.13, hides DeepSeek extras). |
| Serde | `serde` 1.0.229, `serde_json` | 1.0.151 | `preserve_order` for byte-compatible JSON writes. |
| Postgres | `sqlx` | 0.8.6 | `runtime-tokio`, `tls-rustls`, `postgres`, `uuid`, `chrono`, `json`, `migrate`. Same line as `poko-bot`; 0.9.0 (2026-05-21) is an option later. |
| Telegram | `frankenstein` | 0.52.1 | Feature `client-reqwest`; same crate as `poko-bot`. It pulls reqwest 0.13 next to 0.12 (accepted; bump the workspace later). Alternative: `teloxide` 0.17.0 (reqwest 0.12, heavier, 2025-07). |
| Discord | `serenity` | 0.12.5 | `builder, cache, client, gateway, http, model, rustls_backend` as in `poko-bot`. Alternative: `twilight-gateway`/`-http`/`-model` 0.17.1. |
| Tool schemas | `schemars` | 1.2.2 | Derive JSON Schema for tool args. |
| Schema validation | `jsonschema` | 0.58.6 | Validate model-produced tool args before execution. |
| Regex (Node parity) | `fancy-regex` | 0.19.2 | Node patterns use look-ahead (`(?![a-z])`); the scaffold uses 0.14, align to 0.19.2. `regex` 1.13.1 for the rest. |
| Time | `chrono` 0.4.45, `chrono-tz` | 0.10.4 | Workspace crates; Europe/Rome DST. |
| IDs | `uuid` | 1.27.0 | v4 for reminders/jobs. ULIDs: own module (deterministic backfill ids must match Node's sha256 scheme); `ulid` 3.0.0 only if it reproduces that. |
| File locks | `fs4` | 1.1.0 | `flock` for stores; leases use the gateway-lock record. |
| Errors / CLI | `thiserror` 2.0.21, `anyhow` 1.0.104, `clap` | 4.6.7 | |
| Logging | `tracing` 0.1.44, `tracing-subscriber` | 0.3.23 | `json`, `env-filter`; never log message text. |
| Async utils | `tokio-util` 0.7.19, `tokio-stream` 0.1.19, `futures` 0.3.34, `async-trait` | 0.1.92 | |
| Caches / retry | `moka` 0.12.16, `backon` | 1.6.0 | Briefing cache, retries. |
| Secrets / auth | `secrecy` 0.10.3, `subtle` 2.6.1, `sha2` 0.10.9, `base64` | 0.22.1 | Workspace lines. |
| File watch | `notify` | 8.2.0 | Timeline tail-follow while Node still appends; 1 s polling fallback. |
| Env files | `dotenvy` | 0.15.7 | Same env files as the Node compose. |
| URL | `url` | 2.5.8 | SSRF checks for web extract. |
| Recurring jobs (phase 6) | `croner` | 4.0.1 | Proactive check-ins, recurring reminders. |
| Tests | `insta` 1.49.0, `wiremock` 0.6.5, `tempfile` 3.27.0, `proptest` | 1.11.0 | Golden snapshots, fake DeepSeek/Telegram/Honcho, parser properties. |
| Phase 6 options | `calamine` 0.36.1, `rust_xlsxwriter` 0.99.1, `plotters` 0.3.7, `htmd` 0.5.5 | | Only if graphs/spreadsheets move off Harness. |

## 4. Test kit (built in phase 0, reused by every phase)

- **Data copies.** `rsync` of `Lucina/data` into a temp dir per run (timeline, reminders, recent chat, registry,
  device files), and a disposable `postgres:17-alpine` loaded with `pg_dump` of `hermes_grocery` (schema + data, 52
  rows today; add migration 002 for the v2 suite). Honcho: the existing sandbox `honcho-sandbox-api` on
  `127.0.0.1:8001` with a throwaway workspace seeded from an export of `hermes-peer1`.
- **Node reference instance.** A second container from the same `lucina:nez` image on port 8791, data dir = copy,
  `TELEGRAM_BOT_TOKEN_LUCINA`/`DISCORD_BOT_TOKEN_LUCINA` empty, `SECRETARY_API_ENABLED=true`, a test token, and
  `DEEPSEEK_BASE_URL` pointed at the fake LLM. No Node code change is needed for phases 1–2.
- **Fake DeepSeek.** An axum stub that returns scripted completions keyed by `(purpose, normalised prompt hash)` and
  records every request. Both runtimes talk to it, so parity diffs exclude model randomness. A separate "live"
  run against the real API compares action labels only.
- **Contract suite.** One request list per route (every success shape and every error code in INVENTORY §4.2,
  `Cache-Control: no-store`, `Allow`, 64 KiB limit, constant-time auth, SSE framing: `retry: 5000`, `: connected`,
  `: ping`, `Last-Event-ID` replay ≤500, 429 on the 9th stream). Run against 8791 (Node) and 8790 (Rust); diff JSON
  with volatile fields (ids, timestamps, LLM text) masked. `secretary-server`'s own tests are re-run with
  `SECRETARY_LUCINA_URL` at the Rust port.
- **Golden transcripts.** Inputs: `docs/lucina-intent-eval-set.json` (36), `lucina-semantic-eval-set.json` (106),
  `scripts/lucina-intent-50.mjs` (50), the grocery canaries, and 300 owner messages sampled from Honcho
  `telegram_chat_<owner>` and the secretary sessions (texts stay on nezopt; fixtures are not committed). Compared per
  turn: tier-0 intent, context policy, Honcho calls and sources, planner action and arguments, reminder `dueAt`,
  grocery intent and DB effect, outgoing channel calls. Gate: 100 % on reminders, grocery, greetings and
  owner-identity cases, ≥ 95 % action agreement overall in the live run.
- **Parser corpus.** ≥ 2 000 generated IT/EN reminder and timer phrases (times, dates, named months, relative and
  duration forms, "e mezza", "Timer 1 hour and an half") evaluated by Node (`node -e` over `reminders.js`) and Rust
  with a fixed clock; identical `dueAt` or identical failure reason. Include the DST switches 2026-10-25 and
  2027-03-28 (Europe/Rome).
- **Data-compat check.** `lucina check-data --data <dir>`: parse every file, re-serialise, and require byte
  equality (field order as Node writes it, 2-space pretty JSON for `reminders.json`, unknown fields preserved via
  `#[serde(flatten)]`). Round trip both ways: Rust writes, the Node reference reads and lists the same items.

## 5. Strangler phases

Single-writer state is summarised in §6; each phase states which singleton moves.

### Phase 0: foundations (no traffic)

Crates `lucina-core`, `lucina-llm` (DeepSeek), `lucina-memory`, `lucina-store` (read/write compat), the test kit
(§4), container `lucina-rs` (compose next to `lucina-nez`, host network, port 8790, same `data/` volume, same
read-only secret mounts, `/run/hermes-gateway-locks`, root inside the container because `timeline.jsonl` is
root-owned). Exit: `check-data` byte-equal on the live data copy; Honcho context requests identical to the SDK's
(recorded through a logging proxy) for every policy.

### Phase 1: Secretary API (app + web)

Scope: every `/api/secretary/*` route; the chat turn (metadata → tier-0 → DeepSeek intent → policy → Honcho →
planner → tools → reply → `remember`); agenda briefing; jobs (`cursor_task`, `spreadsheet_update_task` through DSH
remote; `graph_task`, `generate_image`, `oracle_monitor_task` answer with the Node blocker text; `social_post_task`
per the owner's answer to Q4); grocery v1 and shopping v2; timeline list + SSE; device bridge. Keep the Node
JSON-action planner prompt verbatim in this phase (native tool calling comes later behind a flag, Q9).

Writers during phase 1:
- `reminders.json`: still Node. Rust reads it (list, agenda, delivered-reminder context) and forwards create/cancel
  to Node `POST /api/secretary/reminders` and `DELETE /api/secretary/reminders/:id` on 8788 with the Node token
  (`source: "secretary"` is what Node already sets). No Node change.
- `timeline.jsonl`: both append (Node: Telegram, reminders; Rust: app turns, device results). Rust owns rotation;
  set Node `LUCINA_TIMELINE_MAX_BYTES=100000000000` so Node never rotates. Rust tail-follows the file so Node's
  Telegram items reach SSE clients within 1 s.
- Recent turns: Rust writes `data/lucina-rs/recent-chat.json` (secretary keys only) because Node rewrites
  `recent-telegram-chat.json` whole from memory. Merged in phase 3.
- `secretary-jobs.json`, device stores: Rust only. Do not merge the device bridge into Node; ship it only in Rust
  (Q12), or the phone would have two command writers.
- Grocery: apply `002_shopping_v2.sql` with the existing Node runner before the cutover (both runtimes then share
  the schema; Postgres handles concurrent writers).

Parity: full contract suite, golden transcripts for secretary sessions, data-compat on timeline pages (every
`before`/`limit` pair over the 892-line copy returns the same ids as Node), shopping v1/v2 suites ported from
`tests/shopping-v2-api.test.mjs`, `shopping-v2-pg.test.mjs`, device suite from `tests/device-bridge.test.mjs`.

Cutover: on p1, point `secretary-server` at the Rust port (change the SSH relay behind
`SECRETARY_LUCINA_URL=http://127.0.0.1:18788` from nezopt:8788 to nezopt:8790, or add relay port 18790 and change
the URL), restart `secretary-server`, then set Node `SECRETARY_API_TOKEN` to a value only Rust knows (Node keeps
serving reminder writes to Rust). Rollback: point the relay back to 8788 and restore the token; Rust-written timeline
lines and jobs stay readable by Node.

### Phase 2: reminders and scheduler

Node shim (the only Node code change, ~120 LOC, behind env flags that default off):
- `LUCINA_REMINDERS_REMOTE_URL` + `LUCINA_REMINDERS_REMOTE_TOKEN`: `createReminder`, `listReminders`,
  `cancelReminder`, `buildRecentDeliveredRemindersContext` call Rust `/api/internal/reminders/*` instead of the file.
- `LUCINA_REMINDER_SCHEDULER_ENABLED=false`: no Node scheduler while creation stays enabled (today both hang off
  `LUCINA_REMINDERS_ENABLED`).
- Test-only `TELEGRAM_API_ROOT` (Telegraf `apiRoot`) for phase 3 replays.

Rust: store under `flock` with write-to-temp, fsync, rename; same JSON layout and retention (500); scheduler holds
the `scheduler-lucina.lock` lease; delivers with `sendMessage` using the Lucina bot token (allowed while Node still
polls: only `getUpdates` is exclusive), writes the `meta.kind: "reminder"` timeline item, records `sent`/`lastError`.

Handover (checked by the model `ownership.rs::Scheduler`, `formal/ownership/SchedulerHandover.tla`; the order
first written here ran two `reminders.json` writers and lost a reminder, docs/verification.md P2a):

1. Rust restarts with **only** `/api/internal/reminders/*` open: its scheduler off and its secretary path still
   forwarding to Node (`LUCINA_REMINDERS_WRITER=node`). Nothing calls the internal endpoint yet, so Node is still
   the only writer.
2. Node restarts with the shim on and its scheduler off, **after its current tick has finished** (drain: Node waits
   for a running `processDueReminders` on SIGTERM; or restart only while no reminder is due). From here Rust is the
   only writer.
3. Rust restarts with `LUCINA_REMINDERS_WRITER=local` and `LUCINA_SCHEDULER=true` (lease acquired).

Due reminders wait at most one poll; none are lost (they stay `pending`). Rollback, in this order: Rust stops
writing first (scheduler off, secretary path forwarding, internal endpoint closed: Node's shim calls fail visibly for
a few seconds), then Node takes the file and the scheduler back (env reverted, restart).

Delivery is at most once in both runtimes (P2b, `ReminderStore::process_due`, Node `processDueReminders`): a tick
claims its due reminders (`status: "sending"`, written atomically) before the first `sendMessage`; a claim found
by the next tick (the process died mid-send) becomes `failed` with `lastError: "interrupted; not resent"` and shows
in the reminder list. Writing `sent` once at the end of the tick, as both did before, resent the batch after a
restart.

Parity: same store copy and injected clock in both runtimes deliver the same ids with the same text
(`formatReminderDelivery`); dedupe key semantics; `already_sent` / `cancelled: false` rules; parser corpus; restart
in the middle of a delivery does not double-send (claim before send, above).

### Phase 3: Telegram

Scope: polling with the gateway lock (same file `telegram-<botid>.lock`, same record), allow-list and
`TELEGRAM_UNAUTHORIZED_DM`, `/start`, `/status`, text pipeline, voice/audio/video_note (≤25 MB, ≤15 min; local
whisper then Gemini; ChatGPT OAuth dropped), photos (Gemini vision → photo plan), documents (download → plan),
groups (owner timeline only when replied), progress notices, chunking at 4096 chars and the same parse mode, chat
registry, recent turns (Rust now owns `recent-telegram-chat.json` and merges its phase-1 file into it).

Handover: (1) Rust poller in standby: it sees Node's live lock and retries every 5 s; (2) restart Node with
`TELEGRAM_BOT_TOKEN_LUCINA` empty (SIGTERM releases the lock first); (3) Rust acquires the lock and continues from
`getUpdates` without `drop_pending_updates` (Node drops pending updates at start; Rust must not, so messages sent
during the switch survive). Rollback: stop the Rust poller (releases the lock), restore the Node token, restart Node.
Node's other `bot` users (Alexa notifications, proactive check-ins, Bot Hospital alerts) go quiet with the token
empty; Bot Hospital needs its own token or a home first (Q4).

Parity: replay recorded/synthesised `Update` JSON (text, voice, photo, document, group, unauthorized DM, `/status`)
into both bots against a fake Bot API (wiremock) and diff the outgoing call sequence (method, chat, reply-to, parse
mode, text with LLM text masked); golden transcripts from the Telegram sample; timeline items identical.

### Phase 4: Discord

Prerequisites: Q1 (keep Discord? persona name), `LUCINA_OWNER_DISCORD_USER_ID`.
Scope: DMs and admin channels (`DISCORD_ADMIN_CHANNEL_IDS`/names), the "disabled, ask Poko" mention reply, planner
with `discord_admin_task` (12 operations; destructive ones stay refused), reminders, timeline owner filter.

Handover: Node has **no** Discord lock, so the order is strict: restart Node with `DISCORD_BOT_TOKEN_LUCINA` empty,
confirm the bot shows offline / no gateway session, then start Rust, which takes a `discord-<appid>.lock` lease so a
second Rust instance cannot connect. Rollback: the same in reverse.

Parity: golden replies on the 52 historical Discord messages; admin operations replayed against a private test guild
in dry-run mode that records REST calls, compared with Node's calls for the same plans.

Already live (2026-10-22): the owner **presence watcher**, crate `lucina-discord` (twilight-gateway 0.17.1),
`lucina presence-watch`, systemd user unit `lucina-presence.service` on nezopt (`deploy/lucina-presence.service`).
It is a second gateway session on the Lucina token with intents `GUILDS | GUILD_PRESENCES` only (no message events,
so it never replies) and is not part of the `discord-<appid>.lock` singleton above. It keeps only
`LUCINA_OWNER_DISCORD_USER_ID`'s presence (merged across guilds, freshest wins) in `data/presence/discord.json`
(current, 0600) and `data/presence/discord.jsonl` (transitions, `LUCINA_PRESENCE_RETENTION_DAYS`, 90). Readers:
`GET /api/secretary/presence`, the `presence` tool, the chat context line (`LUCINA_PRESENCE_CONTEXT`, default on in
Rust) and, until the cutover, the Node hook `src/discord-presence-context.js` (`LUCINA_PRESENCE_CONTEXT=true`).
The phase-4 port reuses the crate for the message session.

### Phase 5: Alexa

Prerequisite: Q2 (find the skill backend that translates Alexa Skills Kit requests into `POST /api/alexa/lucina`).
Scope: the endpoint, background runs, Telegram result notices (Rust owns Telegram now), speech cleaning, 700-char cap.
Cutover: repoint the skill backend to the Rust URL, enable `ALEXA_LUCINA_ENABLED` on Rust only. Rollback: repoint.
Parity: contract tests against the Node reference with Alexa enabled on the copy; golden runs of the 8 historical
Alexa turns (16 timeline items).

### Phase 6: remaining PORT modules, Node retirement for Lucina

Scope: proactive check-ins as a native scheduler job (Q5), `lucina_log_inspect` (own logs + journald), capability
hints, `model_status` tool, grocery offers (`catalog.js`, `nearby.js`, `rapidapi-quota.js`), `/grocery/*` dashboard
API (Q10), `backfill-timeline` and `grocery-migrate` subcommands, Hermes upstream tools from §7.
Grocery deal watch (Lucina `src/grocery/deals/`, added 2026-10-24; Node is canonical, the Hermes grocery copy is not
updated): Salling food-waste + Tjek flyer sources, the shopping-list matcher with its synonym rules and owner override
file `data/grocery/deal-synonyms.json`, the `grocery.deals.cheapest`/`grocery.deals.lunch` intents, and the Telegram
digest/alert scheduler whose state file `data/grocery/deal-watch.json` must stay byte-compatible (INVENTORY §3). Port
it with its recorded fixtures (`tests/fixtures/grocery-deals/`) as parity tests; until then only Node runs the watcher.

Retirement: switch `lucina-nez` to a runtime without Lucina (`HERMES_RUNTIME` set to a value that disables
`lucina`, for example `poko-routes`, after checking which OTHER-BOT pieces still need to run there), keep it on 8788
for OTHER-BOT routes, remove the Lucina tokens and the shim env from it. Before that, run Node's access log for 7 days
(a one-line request logger; Node logs no requests today) to catch callers this inventory missed.

## 6. Single-writer rule

Only one runtime may own each item below at any time. Leases use the gateway-lock record (`{name, holder, instance,
hostname, pid, pidNs, bootId, botKey, startedAt, heartbeatAt}`, live when same boot id and heartbeat younger than
90 s, heartbeat 30 s) in `/run/hermes-gateway-locks`, which both containers mount. The Rust code ports
`poko-bot/src/gateway_lock.rs`. `/api/internal/leases` and `/health` on the Rust side report every lease holder.

| Singleton | Guard today (Node) | Guard in Rust | Moves in phase |
|---|---|---|---|
| Telegram `getUpdates` for `@LucinaN_bot` | `telegram-<botid>.lock` | same file | 3 |
| Discord gateway session (Lucina app) | none | `discord-<appid>.lock` + strict stop-then-start | 4 |
| Reminder scheduler | in-process | `scheduler-lucina.lock` | 2 |
| `reminders.json` writes | in-process promise chain, non-atomic write | `flock` + atomic rename; Node via shim | 2 (forwarded to Node during 1) |
| `timeline.jsonl` rotation | in-process | Rust only (Node limit raised) | 1 |
| `timeline.jsonl` appends | `O_APPEND` | `O_APPEND` under `flock` | shared, both safe |
| `recent-telegram-chat.json` | whole-file rewrite | Rust only | 3 (separate file in 1–2) |
| `secretary-jobs.json`, device stores | — | Rust only | 1 |
| Honcho messages, Postgres rows | — | — | shared, no rule |

Handover protocol for any singleton:
1. The new owner starts in standby and polls the lease (5 s).
2. The old owner is switched off by env and restarted; Node releases its locks first on SIGTERM.
3. The new owner acquires the lease, logs it, and `/api/internal/leases` shows it.
4. Smoke check (send one owner message, schedule a reminder one minute ahead, read the timeline).
5. Rollback is the same steps in reverse. Neither side runs with leases disabled (`HERMES_GATEWAY_LOCK=false` is
   forbidden in both composes).

## 7. Hermes Agent upstream (NousResearch/hermes-agent)

Catalogued at `main` on 2026-10-21 (latest release v0.22.4, same day). **Licence: MIT** for the repository. Ten
`optional-skills` carry their own LICENSE files (`creative/ai-presenter-video`, `anatomy`, `auteur`, `dream-loop`,
`ip-as-logo`, `mono-color`, `system-atlas`, `software-development/ast-grep`, `pr-lens`,
`web-development/scrollcraft`); none is on the list below. Rust reimplementations are new code; where logic is
translated closely (as the Node gateway lock and invisible-Unicode guard already were), keep an "adapted from
NousResearch/hermes-agent, MIT" header with the upstream copyright line.

Size of the catalogue: 285 modules in `tools/`, toolsets in `toolsets.py` (core set of 51 tool names), plugin
families (`web` 11 search backends, `memory` byterover/retaindb, 27 `platforms` with a `plugin.yaml`, 41 `model-providers`, 8 `image_gen`),
71 bundled skills, 158 optional skills, 66 optional MCP servers, 762 `plugin-catalog` entries.

Reimplement as Rust tools, by priority for a personal secretary:

| Priority | Upstream | Lucina tool | Notes |
|---|---|---|---|
| P1 (phases 1–3) | `approval*.py`, `write_approval.py`, `slash_confirm.py`, `clarify_tool.py` | confirmation and clarification flow in `lucina-tools` | Side-effect tools need an explicit owner "yes" (Telegram inline buttons, app buttons); pending actions expire. |
| P1 | `memory_tool.py`, `session_search_tool.py` | `memory_search` (Honcho `/search`, `/peers/{id}/search`) and `timeline_search` | Honcho replaces Hermes' own memory store. |
| P1 | `cronjob_tools.py` + `cron/` | reminders (one-shot now), recurring reminders and scheduled prompts (phase 6) | Delivery queue idea from `cron/delivery_queue.py`. |
| P1 | `web_tools.py`, `web_tools_extract.py`, `url_safety.py`, `website_policy.py`, `threat_patterns.py` | `web_extract` (Obscura), SSRF and prompt-injection screening of fetched text | Add `web_search` with one backend from `plugins/web` (SearXNG self-hosted, or Brave/DDGS). |
| P1 | `vision_tools.py`, `transcription_tools.py`, `transcription_whisper_cpp.py` | vision (Gemini), STT (whisper.cpp on nezopt or faster-whisper) | |
| P2 | `todo_tool.py` | `notes` / `todo` (store-backed) | Checklist item "notes". |
| P2 | `tts_tool.py` | voice replies through the running Kokoro container (`lucina-tts-kokoro`) | `secretary-server` already has a TTS route. |
| P2 | `discord_tool.py` (`discord_admin`) | Discord admin tool (phase 4) | Map Lucina's 12 operations; no deletes. |
| P2 | skills `productivity/maps`, `productivity/google-workspace` (calendar part), `apple/apple-reminders` | places/directions, calendar read, device reminders via the device bridge | Prompts and API choices only; Lucina has no Google OAuth for Calendar yet. |
| P3 | skills `productivity/weekly-review-planning`, `optional-skills/health/fitness-nutrition`, `optional-skills/productivity/memento-flashcards`, `optional-skills/research/rss-feeds`, `media/youtube-content`, `note-taking/obsidian`, `productivity/xlsx`, `productivity/pdf` | briefing/review prompts, gym/body tie-in, memory exercises (roadmap), feeds, video summaries, notes export, spreadsheets | Each one is a prompt + one small tool. |
| P3 | `send_message_tool.py` | `notify_owner` across channels | Owner only; never arbitrary recipients. |

Skip: `terminal`, `process_manage`, file tools (`read_file`, `write_file`, `patch`, `search_files`), `execute_code`,
`delegate_task` (DSH Harness covers project work), every `browser_*` and `computer_use` tool (Obscura covers page
reads), `kanban_*`, `skills_*` and the skills hub (no self-modifying skills), checkpoints, desktop UI tools,
Feishu/Yuanbao/Teams, `x_search`, video generation, `image_generate` (until a provider exists, Q3), the
model-provider and memory plugins (DeepSeek and Honcho are fixed), every platform plugin except Telegram and Discord,
email skills (Gmail was removed by the owner), finance/trading skills.

## 8. Size per phase

Rust LOC excludes tests (expect +40–60 % in tests and fixtures). Effort is focused agent-days including parity runs.

| Phase | Rust LOC | Replaces Node LOC (PORT) | Effort |
|---|---|---|---|
| 0 Foundations + test kit | 2 500–3 000 | config, lock, store, honcho, LLM client (~2 000) | 3 days |
| 1 Secretary API + turn pipeline + grocery + device | 9 000–11 000 | secretary-api, timeline, pipeline, intent, grocery, device, tools (~9 500) | 8–10 days |
| 2 Reminders + scheduler (+120 Node shim) | 1 200–1 500 | reminders (~1 000) | 2 days |
| 3 Telegram | 3 000–3 500 | telegram Lucina path, image intent, STT, vision (~2 500) | 4 days |
| 4 Discord | 1 500–2 000 | discord Lucina path (~700) | 2–3 days |
| 5 Alexa | 500–700 | alexa-lucina (~580) | 1 day (+ finding the skill backend) |
| 6 Remaining PORT + Hermes tools P1–P2 | 2 500–3 500 | proactive, log inspect, hints, offers, dashboard (~1 200) | 3–4 days |
| Total | ≈ 20 000–25 000 | ≈ 17 700 Lucina Node LOC (21 022 counting mixed files whole) | ≈ 23–27 days |

## 9. Risks

1. **Turn-pipeline drift.** 1 530 LOC of IT/EN/DA regex heuristics plus a long JSON-action planner prompt decide what
   Lucina does. A rewrite, or a switch to native tool calling, changes action choice silently. Mitigation: port tier-0
   and the prompt verbatim first, golden transcripts as a gate, native tool calling only behind a flag with an A/B on
   the golden set.
2. **Two writers on JSON stores during coexistence.** `reminders.json` is written non-atomically under an in-process
   lock; `recent-telegram-chat.json` is rewritten whole; timeline rotation is per process. Mitigation: the §6 table,
   forwarding writes to the current owner, the phase-2 Node shim, `flock` + atomic rename in Rust, Node rotation off.
3. **Duplicate consumers.** Node has no Discord lock; a Telegram takeover that drops pending updates loses messages;
   two schedulers double-send. Mitigation: leases, strict stop-then-start, lease report on `/health`, smoke check per
   handover.
4. **Hidden callers on 8788.** `secretary-server` reaches Lucina through the p1 relay `18788 → nezopt:8788`;
   `/oauth2callback` serves Poko/Griffin YouTube re-auth; Poko routes and the Bot Hospital audit live in the Lucina
   runtime; the Alexa skill backend and the grocery dashboard were not found. Mitigation: Rust on its own port, Node
   keeps 8788 for OTHER-BOT, a 7-day Node access log before retirement.
5. **Single LLM provider.** DeepSeek-only by owner decision: an outage or a model rename (`deepseek-flash`) stops every
   surface; DeepSeek has no vision or audio, so Gemini stays a dependency. Mitigation: wait-out with progress notices,
   breaker, clear failure replies, an optional fallback that stays off unless the owner enables it (Q7).
6. **Honcho without an SDK.** The Node SDK 2.1.2 builds `session.context` requests (summary, token limit, peer target
   and perspective, search query, top-k 5, max conclusions 20); the REST call must match. Mitigation: record the SDK's
   HTTP traffic per policy and contract-test against the sandbox on `:8001`.
7. **Reminder parsing and DST.** The NL parser is the most user-visible logic; Europe/Rome leaves DST on 2026-10-25.
   Mitigation: the 2 000-phrase corpus compared with Node, DST cases, injected clock.
8. **Moving contracts.** Shopping v2 and the device bridge are unmerged and still being written; migration 002 is not
   applied in production. Mitigation: port them from the frozen docs after the merge; apply 002 before phase 1.
9. **Permissions and mounts.** `timeline.jsonl` is root-owned, Node runs as root with the Docker socket mounted (log
   inspect). Mitigation: same user and mounts in `lucina-rs` first, then drop the socket when log inspect uses journald.

## 10. Open questions for the owner

1. **Discord:** keep Lucina on Discord? The bot still presents as "Flareon" (`DISCORD_PERSONA=auto`), so Lucina's
   persona and timeline never run there; last use 2026-08-28. If yes: the owner's Discord user id.
2. **Alexa:** is it still wanted, and where does the skill backend live (it is not in the repos on nezopt)?
3. **Broken or disabled actions:** drop or rebuild `graph_task` (no rclone in the image), `generate_image` (ComfyUI
   tunnel dead), `oracle_monitor_task`, `revive_qwen_task`, `study_focus_reminders`?
4. **Poko-facing pieces in Lucina:** `social_post_task`, `poko_news_scout_retry` and the Bot Hospital daily audit
   (started by the Lucina runtime, alerting Lucina's Telegram): keep in Node, move to `poko-bot`, or drop?
5. **Proactive check-ins** with the recovery companion: revive as a Rust scheduler job? No caller since 2026-08-03.
6. **Harness mirror** of results into Honcho `cursor-global` (`LUCINA_CURSOR_HARNESS_MIRROR=true`): keep?
7. **DeepSeek-only:** no fallback at all, or a configured fallback behind a flag?
8. **Speech-to-text:** faster-whisper inside the container (today), whisper.cpp on nezopt, or Gemini only?
9. **Planner:** keep the JSON-action planner for parity and move to native tool calling afterwards (recommended), or
   switch at once?
10. **Grocery extras:** is the `/grocery/*` dashboard API used? Port the offer/catalog adapters (ShelfAtlas, Salling,
    chain JSON, eTilbudsavis, RapidAPI)?
11. **Deployment:** Docker `lucina-rs` on port 8790 (recommended) or a systemd user unit; branch `lucina-rs` merge
    policy into `main`.
12. **Device bridge:** ship it only in Rust and never merge it into Node (recommended), to avoid a second command writer?
13. **After retirement:** which OTHER-BOT duties stay in the Node container on 8788, and under which runtime value?
