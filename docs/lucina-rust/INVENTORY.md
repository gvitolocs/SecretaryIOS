# Lucina (Node) inventory for the Rust port

Snapshot of 2026-10-21. Sources: `nezopt:/home/nez/Projects/Lucina` (live tree, branch `secretary-timeline`,
HEAD `bc9f243`), the worktree `Lucina-shopping-v2` (branch `secretary-shopping-v2`, HEAD `fd4a15c`, unmerged:
shopping v2 + device bridge WIP), the running container `lucina-nez`, its data under `data/`, the Honcho
workspace `hermes-peer1`, and the grocery Postgres database. Secret values were never read or printed; env
vars are listed by name only.

Marks:

- **PORT**: Lucina needs it; reimplement in Rust. `dormant` = Lucina feature with no traffic now (kept on
  owner request or for the planned phases).
- **DROP**: dead, disabled, broken on this host, or replaced by a simpler Rust design. Its data stays on disk.
- **OTHER-BOT**: Poko, newsroom, Cityhop, Griffin, Bot Hospital, or dev tooling. Stays in Node for now.

## 1. What actually runs in `lucina-nez`

| Fact | Evidence |
|---|---|
| Runtime | `HERMES_RUNTIME=lucina`, `command: node src/index.js`, Node 20.20.2, `network_mode: host`, port `HERMES_PORT=8788`, runs as root. |
| Started surfaces | Telegram poller as `@LucinaN_bot` (gateway lock `telegram-<botid>.lock` in `/run/hermes-gateway-locks`), Discord gateway (logs in as `Flareon#…`), reminder scheduler (60 s poll), Bot Hospital daily Poko-news audit. Poko and Cityhop bots are **not** started (`Poko Discord disabled for runtime "lucina"`). |
| HTTP | Every route in `src/index.js` except the two Cityhop ones is mounted regardless of runtime (see §4); Poko and OAuth routes answer on 8788 too. |
| Reasoning | `LUCINA_DEEPSEEK_ONLY=true` (owner decision 2026-10-21): planner, intent model and replies call only `https://api.deepseek.com/v1`, model `deepseek-flash` (`DEEPSEEK_MODEL` default), key from `DEEPSEEK_KEY_FILE`. The rest of the provider chain (OpenAI proxy `:18080`, Gemini planner, OpenRouter, Cursor, Ollama, ZCode) is unreachable for Lucina; `:18080` and `:11437` are not even listening. |
| Still uses Gemini | Photo vision (`LUCINA_IMAGE_VISION_PRIMARY=gemini`) and the second voice-STT step (`GEMINI_API_KEY`). Local `faster-whisper` (python, model `small`) is the first STT step. |
| Memory | Honcho v3 at `http://127.0.0.1:8000`, workspace `hermes-peer1`, agent peer `hermes`, owner peer `giuseppe` (legacy alias `flareon_owner`, `telegram_user_8802741204`), vector search on, mirror writes on, peer chat off. |
| Disabled by env | Alexa (`ALEXA_LUCINA_ENABLED` unset → 404), Oracle monitor, semantic judge (`off`, GLiClass `:8678` not running), Cursor/Cursor planner/Telegram cursor chat, OpenRouter/Gemma first, Poko news autopost. |
| Broken on this host | `graph_task` (no `rclone` in the image, cache dir `/opt/hermes/data` missing), `generate_image` (ComfyUI `trycloudflare` URL unreachable), log inspect's `journalctl`/`rg` (missing in image; `docker logs` works through the mounted socket). |
| Logs (72 h) | Container recreated 2026-10-21 18:53 UTC, so `docker logs` holds 110 lines: secretary chat turns (`channel: web`, `ios`), `intent: model_status` / `social_greeting` / `chat`, `Planner chain: DeepSeek only`, reminder scheduler start, `reminder seed skipped`, Bot Hospital audit `ok`. |
| Traffic by channel (Honcho `hermes-peer1`) | `telegram_chat_<owner>` 1485 messages, last 2026-10-21 12:40 (Telegram timeline items, both roles, per month: May 572, Jun 40, Aug 96, Sep 134, Oct 16). Discord 3 sessions (52 messages), last 2026-08-28. Alexa only `alexa_session_*` smoke sessions, last 2026-05-25. Secretary: `secretary_app`, `secretary_ios-*`, `secretary_lucina-page-*`, all 2026-10-21. |
| What Lucina is asked (assistant messages since 2026-08 matching a topic) | Poko/news 17, images 18, model/Qwen 15, Harness 14, Oracle/peer1 13, URLs 13, spreadsheets 10, reminders 10, grocery 6, vulnerable-support 6, email 5, graphs 3. May–June traffic was dominated by spreadsheets (83), Oracle (144), Poko news (144), email (124, Gmail removed 2026-09-20). |

## 2. `src/` modules

Columns: LOC; what it does; Lucina use on `lucina-nez`; external APIs, stores, env vars; tests (`tests/*.test.mjs`
unless noted); mark. 137 of the 149 `src/` files are reachable from `src/index.js`; the 12 unreachable ones are
noted.

### 2.1 Boot, config, cross-cutting

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `index.js` | 637 | Boot: memory, grocery, bots per runtime, HTTP router (all routes, §4), managed Telegram polling with gateway lock + 409 back-off, Discord launch, reminder scheduler, Bot Hospital audit, SIGTERM shutdown (releases locks first). | Yes | `HERMES_RUNTIME`, `HERMES_PORT`, `HERMES_PUBLIC_URL` (empty → polling) | — | PORT |
| `config.js` | 590 | Loads `private/lucina/.env.local`, `.env.local`, cityhop env; builds `config` for every bot. | Yes (Lucina subset) | ~250 env names; Lucina subset listed in §7 | — | PORT (subset) |
| `gateway-lock.js` | 195 | Host-wide single-poller lock per Telegram bot: JSON record `{name,holder,instance,hostname,pid,pidNs,bootId,botKey,startedAt,heartbeatAt}` at `<dir>/telegram-<botid>.lock`, live when same boot id and heartbeat < 90 s, heartbeat 30 s, retry 60 s, atomic rename. Already ported to Rust in `pokoin-poko-bot/pokoin-rust/apps/poko-bot/src/gateway_lock.rs`. | Yes | `HERMES_GATEWAY_LOCK`, `_DIR`, `_STALE_MS`, `_HEARTBEAT_MS`, `_RETRY_MS`, `HERMES_GATEWAY_INSTANCE` | `agent-v022-port` | PORT (format-compatible) |
| `runtime-host.js` | 51 | Host label (`nezopt`) and "Oracle monitor enabled?" lines for prompts. | Yes | `LUCINA_RUNTIME_HOST`, `LUCINA_ORACLE_MONITOR_ENABLED` | — | PORT (fold into config) |
| `log-redaction.js` | 40 | Redacts Telegram ids and payloads in logs. | Yes | — | — | PORT |
| `invisible-unicode.js` | 96 | Detects/strips invisible Unicode in prompts sent to shell/project-work tools (Hermes Agent port). Rust copy exists in poko-bot `invisible.rs`. | Yes (Harness prompts) | — | `agent-v022-port` | PORT |
| `active-task.js` | 28 | Writes `data/active-tasks/<ts>-<kind>.json` while a long task runs, removes it after. 7 stale files left from crashes. | Yes, write-only | `data/active-tasks/` | — | DROP (in-process task registry) |
| `chat-registry.js` | 36 | Remembers Telegram chats `{chatId,chatType,firstName,lastName,username,updatedAt}` keyed by chat id and `@username`. | Yes | `data/telegram-chats.json` (`HERMES_CHAT_REGISTRY_PATH`) | — | PORT |
| `status-report.js` | 78 | `/status` and `/status full` text (Hermes `status --short/--full` port): runtime, memory, planner blocks, gateway locks, reminders. | Yes | — | `agent-v022-port` | PORT |
| `turn-notifier.js` | 21 | AsyncLocalStorage progress channel ("providers busy, retrying") used by the planner wait-out. | Yes | — | `agent-v022-port` | PORT |
| `turn-trace.js` | 58 | One JSON log line per turn (policy, sources, sizes, latency). | Yes (logs) | `LUCINA_TURN_TRACE` | `turn-context` | PORT |

### 2.2 Channels and the Secretary API

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `telegram.js` | 2517 | Telegraf bot for Lucina/Poko/Cityhop. Lucina path: allow-list (chat ids, usernames, unauthorized-DM policy), `/start`, `/status [full]`, text → grocery pipeline → device commands (worktree) → `prepareLucinaTurnContext` → `planLucinaResponse` → action dispatch (§5) → reply + `memory.remember`; voice/audio/video_note → STT (local faster-whisper → Gemini `gemini-2.5-flash-lite` → ChatGPT OAuth) → text path; photo → vision → photo plan (spreadsheet log or reply); document → download → plan (spreadsheet import); provider-wait notices; timeline middleware. Poko/Cityhop branches inside. | Yes, main channel | Telegram Bot API (getUpdates, sendMessage, sendPhoto, sendChatAction, getFile, deleteWebhook, getMe); Gemini API; `TELEGRAM_BOT_TOKEN_LUCINA`, `TELEGRAM_ALLOWED_CHAT_IDS`, `TELEGRAM_ALLOWED_USERNAMES`, `TELEGRAM_UNAUTHORIZED_DM`, `TELEGRAM_HOME_CHAT_ID`, `TELEGRAM_MAX_AUDIO_*`, `LOCAL_STT_ENABLED`, `LOCAL_WHISPER_MODEL`, `LOCAL_WHISPER_TIMEOUT_MS`, `GEMINI_API_KEY`, `GEMINI_STT_MODEL`, `LUCINA_OPERATOR_CHAT_ID`, `LUCINA_OPERATOR_USER_PEER_ID` | `identity`, `no-honcho-greeting`, `reminder-timers`, `recent-chat` (indirect) | PORT (Lucina path; Poko/Cityhop branches are OTHER-BOT) |
| `discord.js` | 1239 | discord.js client for Lucina and Poko. Lucina path: replies only in DMs or "admin" channels (`DISCORD_ADMIN_CHANNEL_IDS`, names `admin, admin-chat, operator, operators, ops`), elsewhere "I'm currently disabled, ask Poko instead" on mention; planner actions incl. `discord_admin_task` with 12 operations (§5). `DISCORD_PERSONA` unset → `auto` → persona = bot display name **"Flareon"**, so the Lucina timeline hook and Lucina persona checks (`activePersona === 'Lucina'`) never fire today. | Low: last message 2026-08-28 | Discord gateway + REST; `DISCORD_BOT_TOKEN_LUCINA`, `DISCORD_PERSONA`, `DISCORD_ALLOWED_CHANNEL_IDS`, `DISCORD_ALLOWED_USER_IDS`, `DISCORD_ADMIN_CHANNEL_IDS`, `DISCORD_ADMIN_CHANNEL_NAMES`; `LUCINA_OWNER_DISCORD_USER_ID` (unset) | `poko-discord-*` (Poko only) | PORT (Lucina path, P4) |
| `alexa-lucina.js` | 576 | `POST /api/alexa/lucina` (§6): plans the query, runs async actions in the background and reports to Telegram, otherwise speaks a ≤700-char reply. Peer `alexa_user_<id>`, session `alexa_session_<id>`, timeline channel `alexa`. | Dormant: endpoint 404 (env unset); last traffic 2026-05-25 | `ALEXA_LUCINA_ENABLED`, `ALEXA_LUCINA_TOKEN`, `ALEXA_LUCINA_MAX_REPLY_CHARS` | — | PORT (dormant, P5) |
| `secretary-api.js` | 1127 (worktree 1348) | `/api/secretary/*` (§4.2): bearer auth (constant time), `{ok,data}` envelope, chat turn via the shared pipeline, async jobs, reminders CRUD, agenda briefing (≤6 sentences, cached 15 min per date), shopping v1, timeline list + SSE; worktree adds shopping v2 lists/ops and device bridge routes. In-memory `clientMessageId` replay (10 min). | Yes (web + iOS since 2026-10-21) | `SECRETARY_API_ENABLED`, `SECRETARY_API_TOKEN`, `SECRETARY_SESSION_ID`, `SECRETARY_GROCERY_USER_ID`, `SECRETARY_BRIEFING_CACHE_MS`, `SECRETARY_JOBS_PATH`; `data/secretary-jobs.json` (not created yet) | `secretary-api`, `timeline-api`, worktree `shopping-v2-api`, `device-bridge` | PORT (P1) |
| `timeline.js` | 610 | Owner timeline JSONL store: monotonic ULIDs, deterministic ULIDs for backfill, dedupe hash, in-memory offset index + 2000-item cache, tail re-scan for lines written by other processes, rotation `.1`/`.2` at 50 MB, EventEmitter for SSE. | Yes | `data/timeline.jsonl` (root-owned, 892 lines); `LUCINA_TIMELINE_ENABLED`, `_PATH`, `_MAX_BYTES`, `_MAX_STREAMS`, `_PING_MS` | `timeline`, `timeline-api` | PORT (P1) |
| `timeline-channels.js` | 198 | Owner filters and hooks: Telegram middleware (owner `from.id`, groups only when replied), Discord owner filter, reminder-delivery items (`meta.kind: reminder`). | Yes (Telegram, reminders) | `LUCINA_OWNER_DISCORD_USER_ID` | `timeline` | PORT |
| worktree `device-bridge.js` | 713 | Device bridge (contract `SecretaryIOS/docs/device-bridge.md`): event ingest (10 types, 4 sources, ≤500/batch, URL → domain+title), state snapshot, turn-context block, daily Honcho digest, reachability. | Not deployed | `data/device-context.jsonl`, `device-state.json`, `device-commands.json`, `device-digest-state.json`; `LUCINA_DEVICE_*` (8 names) | worktree `device-bridge` | PORT (P1, contract still moving) |
| worktree `device-commands.js` | 481 | Deterministic it/en/da parser for alarms, timers, calendar adds, deletions; commands delivered as timeline items (`meta.kind: device_command`), result ack → confirmation item. | Not deployed | as above | worktree `device-bridge` | PORT (P1) |

### 2.3 Turn pipeline (intent, context, persona)

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `lucina-text-turn.js` | 221 (worktree 232) | Shared text-turn pipeline for Telegram and the Secretary app: turn metadata → intent → context policy → gated retrieval → planner. | Yes | — | `turn-context`, `secretary-api` | PORT |
| `lucina-turn-context.js` | 135 | Policy-gated context assembly (metadata, recent turns, delivered reminders, Honcho) with per-source char budgets. | Yes | — | `turn-context`, `budget-truncation` | PORT |
| `context-policy.js` | 194 | 8 policies (`PERSONA_ONLY`, `CURRENT_SESSION`, `RECENT_CONTEXT`, `NEW_TOPIC`, `PROACTIVE_CHECKIN`, `HONCHO_RECALL`, `PROJECT_CONTEXT`, `LEGACY_FULL`), budgets, `TOOL_INTENTS`, deterministic resolver. | Yes | — | `context-policy`, `semantic-invariants` | PORT |
| `intent-router.js` | 102 | Pluggable `classifyIntent`; `heuristic` (default) or `jev` (unimplemented). | Yes | `LUCINA_INTENT_ROUTER` | `intent-taxonomy` | PORT |
| `lucina-intent.js` | 1009 | Tier-0 regex classifier (greetings, pings, recall, temporal, continuation, tools, 11 grocery intents; §5.2), model classifier (`purpose: intent`), direct plans for tool intents, `routeLucinaPlan` (direct vs full plan, capability hints). | Yes (logs: `intentSource: tier0`) | `LUCINA_INTENT_ROUTER_ENABLED`, `LUCINA_INTENT_CONFIDENCE_MIN`, `LUCINA_AGGRESSIVE_TOOL_ROUTING`, `LUCINA_FAST_CHAT_*` | `intent-taxonomy`, `plan-parse`, `temporal-robustness`; `scripts/test-lucina-intent.mjs`, `lucina-intent-50.mjs`, `docs/lucina-intent-eval-set.json` (36 cases) | PORT |
| `intent-heuristics.js` | 521 | IT/EN/DA regexes: timers, reminders, graphs, sheets, social posts, Oracle, model status, web crawl, code project, vulnerable support, travel, etc. Shared with grocery. | Yes | — | `scripts/test-intent-heuristics.mjs` | PORT |
| `turn-metadata.js` | 120 | Deterministic clock/idle metadata (`currentTime`, `elapsedSinceLastActivitySeconds`, `isFirstTurnAfterIdle`). | Yes | `LUCINA_TIMEZONE`, `LUCINA_IDLE_BOUNDARY_MS` | `turn-metadata`, `temporal-robustness` | PORT |
| `recent-chat-context.js` | 147 | Last 20 turns / 5000 chars per chat, persisted as `{chatKey: [{at,role,text}]}`; also secretary sessions. | Yes | `data/recent-telegram-chat.json` (`LUCINA_RECENT_CHAT_PATH`) | `recent-chat` | PORT |
| `reply-language.js` | 54 | IT/EN detector for replies. | Yes | — | `capability-hints-language` | PORT |
| `lucina-capability-hints.js` | 139 | Appends "💡 Posso anche / I can also" lines to chat replies that look actionable. | Yes | `LUCINA_CAPABILITY_HINTS_ENABLED` | `capability-hints-language` | PORT (P6, optional) |
| `lucina-persona.js` | 35 | Loads `docs/lucina-persona.md` (Fire Emblem Lucina tone rules) into the system prompt. | Yes | `LUCINA_PERSONA_PATH` | `lucina-style` | PORT (asset) |
| `lucina-operator-context.js` | 111 | Loads `docs/lucina-operator-brief.md` (≤2800 chars) and optionally the full operator doc; secrets dir and peer1 key path. | Yes | `LUCINA_OPERATOR_BRIEF_MAX_CHARS`, `LUCINA_OPERATOR_FULL_DOC_IN_PROMPTS`, `LUCINA_SECRETS_DIR`, `LUCINA_POKOIN_HANDOFF_PATH` | — | PORT (asset; brief needs a rewrite: it still describes Qwen, ComfyUI, Poko peer1) |
| `lucina-recovery-companion.js` | 46 | Loads `docs/lucina-recovery-companion.md` (153 lines, psychiatric-informed recovery companion) into prompts; priority over persona. | Yes (default on) | `LUCINA_RECOVERY_COMPANION_ENABLED`, `_MAX_CHARS`, `_PATH` | `proactive-prompt` | PORT (asset, safety-critical) |
| `owner-identity.js` | 98 | Canonical owner peer `giuseppe`; legacy aliases `flareon_owner`, `telegram_user_<owner>`, `discord_user_<owner>`; timeline owner ids. | Yes | `LUCINA_OWNER_PEER_ID`, `LUCINA_OPERATOR_CHAT_ID`, `LUCINA_OWNER_DISCORD_ID` | `identity`, `identity-negative` | PORT |
| `lucina-image-intent.js` | 125 | Classifies a photo (scale → weight sheet, receipt → expense sheet, chat) and builds the spreadsheet plan. | Yes (photos) | — | — | PORT (P3) |
| `lucina-async-actions.js` | 184 | Runs long actions (`cursor_task`, `oracle_monitor_task`, `spreadsheet_update_task`, `graph_task`, `generate_image`, `social_post_task`) for Alexa and app jobs; blockers when not ready. | Yes (app jobs) | — | `secretary-api` | PORT (only the live actions) |
| `semantic-judge.js` | 145 | Client for the local GLiClass judge with breaker. | No: `LUCINA_SEMANTIC_JUDGE=off`, service down | `LUCINA_SEMANTIC_JUDGE*` (6 names), `http://127.0.0.1:8678` | `semantic-judge`, `semantic-judge-down`, `gliclass-schema` | DROP |
| `semantic-resolver.js` | 250 | Maps judge scores to policy decisions. | No (judge off) | — | `semantic-resolver`, `semantic-invariants` | DROP |

### 2.4 LLM providers

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `openai.js` | 1064 | Lucina: `planResponse` (JSON-action planner prompt, §5.1), `generateReply`, `generateSkillReply`, proactive check-in prompt/decision, `inspectImage` (vision chain), `transcribeAudioFile` (ChatGPT OAuth). Poko: market/plan prompts, output sanitizers. | Yes (Lucina prompts) | `OPENAI_API_KEY` (required at boot), `OPENAI_BASE_URL`, `OPENAI_MODEL`, `OPENAI_*` | `plan-parse`, `proactive-prompt`, `lucina-style` | PORT (Lucina prompts and reply/skill generators); Poko half OTHER-BOT |
| `planner-fallback.js` | 1770 | Provider chain: Cursor, OpenRouter free chain, Gemini chain with day blocks, local Ollama, OpenAI proxy, ZCode, DeepSeek last; Poko chain; transient wait-out (`PLANNER_TRANSIENT_WAIT_MS` 10 s, 30 s) with progress notices; tool-markup rejection. With `LUCINA_DEEPSEEK_ONLY=true` only `fetchDeepSeekCompletion` + wait-out run for Lucina. | DeepSeek slice only | DeepSeek `POST /v1/chat/completions` (`DEEPSEEK_API_KEY`/`DEEPSEEK_KEY_FILE`, `DEEPSEEK_BASE_URL`, `DEEPSEEK_MODEL`, `DEEPSEEK_TIMEOUT_MS` 90 s); 401/402 day-block → `data/planner-provider-blocks.json` | `poko-tool-markup`, `concurrency`, `cache-isolation` | PORT (DeepSeek client + wait-out, ~10% of the file); rest DROP; Poko chain OTHER-BOT |
| `gemini-openai-bridge.js` | 240 | Gemini `generateContent` with OpenAI-style tools; `inspectImageWithGemini`. | Yes (vision) | `GEMINI_API_KEY`, `generativelanguage.googleapis.com` | — | PORT (vision provider) |
| `image-vision.js` | 622 | Vision order (`LUCINA_IMAGE_VISION_PRIMARY`, `_FALLBACKS=cursor_auto,openai_inspect,gemini,openrouter`), Gemma local endpoints, benchmark. | Gemini step only | `LUCINA_IMAGE_GEMMA_*` (8 names), `OPENROUTER_IMAGE_*` | `scripts/bench-lucina-image-vision.mjs` | PORT (Gemini only); rest DROP |
| `chatgpt-oauth.js` | 182 | Codex/ChatGPT OAuth headers + refresh for audio transcription. | Third STT fallback; `auth.openai.com` | `CODEX_HOME`, `CODEX_AUTH_PATH`, `OPENAI_AUDIO_TRANSCRIPTION_MODE=chatgpt_oauth` | — | DROP |
| `cursor-planner.js` | 95 | Cursor Auto through an OpenAI-compatible proxy. | No (disabled) | `CURSOR_PLANNER_*`, `HERMES_CURSOR_PLANNER_*` | — | DROP |
| `cursor-agent.js` | 81 | Cursor CLI project work. | No (`HERMES_CURSOR_ENABLED=false`) | `HERMES_CURSOR_*`, `CURSOR_API_KEY` | — | DROP |
| `ollama-runtime.js` | 328 | Qwen/Gemma residency status, revive Qwen, last planner model. | `model_status` intent still answered from it (reports stale Ollama facts) | `LUCINA_QWEN_*`, `LUCINA_CPU_*`, `LUCINA_OLLAMA_URL`, `OLLAMA_*` | — | DROP (model status becomes a 10-line tool) |
| `gpu-budget.js` | 84 | VRAM admission rules for Ollama models. | No | `LUCINA_GPU_*` | `gpu-budget` | DROP |
| `planner-block-store.js` | 81 | Persists provider day blocks. | Only DeepSeek 401/402 | `data/planner-provider-blocks.json` (`HERMES_PLANNER_BLOCK_PATH`) | — | DROP (in-memory breaker) |
| `zcode-api.js` | 88 | Z.ai GLM Anthropic-compatible chain provider. | No (Poko chain only) | `ZCODE_API_*` | `zcode-api` | OTHER-BOT |
| `zcode-cli.js` | 138 | ZCode CLI provider (disabled). | No | `ZCODE_CLI_*` | `zcode-cli` | OTHER-BOT |
| `llm-proxy-server.js` | 96 | Standalone OpenAI-compatible proxy over the planner chain. Unreachable from `index.js`. | No | `HERMES_LLM_PROXY_HOST`, `_PORT` | — | DROP |

### 2.5 Memory

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `memory/honcho.js` | 557 | `@honcho-ai/sdk` 2.1.2 (API v3). Context: `peer()`/`session()` get-or-create, `session.context({summary, tokens, peerTarget, peerPerspective, representationOptions{searchQuery, searchTopK 5, maxConclusions 20}})`, policy-gated assembly, legacy alias recall blocks, optional read-only workspaces. Write: `session.addPeers` + `addMessages` (user + assistant with `metadata.role`) to every target when mirroring. | Yes | REST: `POST /v3/workspaces/{ws}/peers`, `/sessions`, `/sessions/{id}/peers`, `/sessions/{id}/messages`, `GET /sessions/{id}/context`, `POST /peers/{id}/chat` (off); `HONCHO_URL`, `HONCHO_FALLBACK_URLS` (empty), `HONCHO_API_KEY`, `HONCHO_WORKSPACE_ID`, `HONCHO_AGENT_PEER_ID`, `HONCHO_USE_VECTOR_SEARCH`, `HONCHO_MIRROR_WRITES`, `HONCHO_MERGE_CONTEXTS`, `HONCHO_CONTEXT_TOKEN_LIMIT`, `HONCHO_READ_WORKSPACES`, `HONCHO_READ_CONCLUSIONS_LIMIT`, `HONCHO_PEER_CHAT_ENABLED`, `LUCINA_HONCHO_PEER_CHAT_ENABLED`, `MEMORY_PROVIDER` | `cache-isolation`, `no-honcho-greeting`, `budget-truncation`, `identity` | PORT |

### 2.6 Reminders and proactive messages

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `reminders.js` | 981 | IT/EN natural-language parser (clock times, dates, named months, relative and duration expressions, "Timer 1 hour and an half"), zoned → UTC, store CRUD with dedupe key `chatId:dueAt:text`, list/scope formatting, delivered-reminders context, 60 s scheduler (send → `sent`/`lastError`), seed (Google AI Plus), unwritable-store warning. Write lock is **in-process only** and `writeFile` is **not atomic**. | Yes | `data/reminders.json` (§3.2); `LUCINA_REMINDERS_ENABLED`, `_STORE_PATH`, `LUCINA_REMINDER_TIMEZONE` (Europe/Rome), `_DEFAULT_HOUR`, `_DEFAULT_MINUTE`, `_POLL_INTERVAL_MS`, `_INITIAL_DELAY_MS`, `_MAX_STORED`, `_SEED_*` | `reminder-timers`, `temporal-robustness` | PORT (P2) |
| `study-focus-reminders.js` | 136 | `study_focus_reminders` action: seeds fixed study tips as reminders. 8 sent in 2026-06. | No since June | — | — | DROP (generic reminders cover it) |
| `lucina-seed-pokoin-tax-reminders.mjs` | 192 | One-off seed of Danish tax reminders (10 pending in the store, 2027). Unreachable from `index.js`. | Data only | `LUCINA_TAX_REMINDER_CHAT_ID`, `HERMES_CHAT_REGISTRY_PATH` | — | DROP (data stays) |
| `lucina-seed-study-focus-reminders.mjs` | 34 | One-off seed. Unreachable. | No | `LUCINA_REMINDER_SEED_USERNAME` | — | DROP |
| `lucina-proactive-checkin.js` | 299 | `POST /api/internal/lucina/proactive-checkin` (bearer): within 08–22 Europe/Rome decides send/skip with the `PROACTIVE_CHECKIN` policy and the recovery companion, sends to the owner chat. Caller is `scripts/lucina-proactive-checkin.py` on a timer that is **not installed on nezopt**; state path points at nespc (`/home/nes/...`), local state file last written 2026-08-03. | Dormant (enabled flag, no caller) | `LUCINA_PROACTIVE_CHECKIN_*` (12 names) | `proactive-prompt` | PORT (dormant, P6: native scheduler job, no HTTP hop) |
| `lucina-cursor-nudge.js` | 130 | `POST /api/internal/lucina/cursor-nudge`: Telegram nudges from Cursor/Honcho activity, quiet hours. Caller `scripts/honcho-cursor-watch.py`, state last 2026-08-26. | No | `LUCINA_CURSOR_NUDGE_*`, `HONCHO_CURSOR_WATCH_*` | — | DROP |
| `guardian-alerts.js` | 125 | `POST /api/internal/guardian/alert` → Telegram; also exports helpers used by other handlers. No caller found on nezopt. | No | `GUARDIAN_ALERT_*` | — | DROP (helpers inlined) |
| `guardian-digest.js` | 105 | `POST /api/internal/guardian/digest`. No caller found. | No | as above | — | DROP |

### 2.7 Tools behind planner actions

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `project-work.js` | 31 | `cursor_task` executor selection (Harness, else Cursor). | Yes (14 Harness mentions since Aug) | — | — | PORT |
| `dsh-remote-client.js` | 170 | JSON-RPC client for the DeepSeek Harness remote (Orchestra) at `:8765`; submit, poll, wait. | Yes (`DSH_REMOTE_ENABLED=true`) | `DSH_REMOTE_URL`, `_TOKEN`, `_TOKEN_FILE`, `_OWNER_ID` (`flareon`), `_WORKSPACE`, `_TIMEOUT_MS`, `_WAIT_SECONDS`, `_POLL_MS`, `_RPC_TIMEOUT_MS` | — | PORT |
| `harness-agent.js` | 107 | Harness readiness; remote first, else spawns `scripts/run-dsh-headless.sh`. | Yes (remote) | `HERMES_HARNESS_*`, `DSH_HOME`, `DSH_TELEMETRY_MODE`, `HONCHO_*` | — | PORT (remote path); headless fallback DROP |
| `harness-cursor-bridge.js` | 51 | Mirrors Harness results into Honcho workspace `cursor-global`. | Yes (`LUCINA_CURSOR_HARNESS_MIRROR=true`) | `HONCHO_HARNESS_WORKSPACE`, `LUCINA_CURSOR_HARNESS_SESSION` | — | DROP (open question) |
| `spreadsheet-updates.js` | 103 | `spreadsheet_update_task`: builds a Harness prompt that runs `scripts/append-weight-log.py` / `append-receipt-log.py` (rclone, Drive) on the host. | Yes (10 since Aug) | Harness; `data/drive-backups` | — | PORT (as a Harness-delegating tool) |
| `spreadsheet-graphs.js` | 453 | `graph_task`: `rclone copyto drive:<file>.xlsx`, `xlsx` parse, LLM chart spec, PNG render, vision check. | Broken here (no rclone, cache dir missing); 3 mentions since Aug | `rclone`, `/opt/hermes/data/drive-cache` | — | DROP (or re-implement on the Drive API; open question) |
| `image-generation.js` | 128 | ComfyUI workflow submit + poll. | Broken (tunnel URL dead) | `IMAGE_PROVIDER=comfyui`, `COMFYUI_BASE_URL`, `COMFYUI_WORKFLOW_PATH`, `IMAGE_TIMEOUT_MS`; `workflows/comfyui-text-to-image.json` | — | DROP (open question) |
| `web-crawl.js` | 154 | `web_crawl_task`: Obscura first, else `scripts/lucina-crawl4ai.py`; markdown ≤12000 chars. | Yes (13 URL turns since Aug) | `LUCINA_WEB_CRAWL_*` | — | PORT (Obscura path) |
| `obscura-web.js` | 144 | Spawns `obscura fetch <url> --dump`; public-URL validation (SSRF guard); readiness. | Yes | `OBSCURA_BINARY`, `OBSCURA_WEB_ENABLED` | — | PORT |
| `oracle-monitor.js` | 195 | SSH CPU/RAM probe of Oracle peers. | No (disabled) | `ORACLE_MONITOR_*` (22 names), keys under `private/lucina/keys/` | — | DROP |
| `lucina-log-inspect.js` | 106 | `lucina_log_inspect`: `docker logs` + `journalctl` tails as evidence for a reply. | Partly (journalctl missing) | `LUCINA_LOG_CONTAINER`, `_TAIL`, `_TIMEOUT_MS`, `_UNIT` | — | PORT (P6, journald + own logs) |
| `google-keep.js` | 79 | Keep notes listing. Only used by `scripts/sync-keep-notes.js`. | No | `KEEP_*` | — | DROP |
| `lucina-cursor-chat.js` | 190 | Telegram chat through a Cursor agent session. | No (`LUCINA_TELEGRAM_CURSOR_CHAT_ENABLED=false`) | `LUCINA_CURSOR_CHAT_*` | — | DROP |
| `lucina-telegram-context.js` | 223 | Cursor-chat prompt/state (`data/lucina-telegram-cursor-state.json`). | No | as above | — | DROP |
| `social-operations.js` | 665 | Posts to Pokoin Telegram channel, Discord #pokenews, X with Poko tokens; card image resolution. Lucina reaches it via `social_post_task` and `poko_news_scout_retry` (Bot Hospital). | Indirect (Poko publishing) | `TELEGRAM_BOT_TOKEN_POKO`, `DISCORD_BOT_TOKEN_POKO`, `TELEGRAM_CHANNEL_ID_POKO`, `DISCORD_NEWS_CHANNEL_ID_POKO`, `X_*`; Telegram, Discord, X, pokemontcg.io, pokeapi | — | OTHER-BOT (Rust Lucina delegates over HTTP or drops the action) |

### 2.8 Grocery (`src/grocery/`)

Database: `GROCERY_DATABASE_URL` → `127.0.0.1:5432/hermes_grocery`, schema `grocery`. Rows today:
`canonical_products` 15, `stores` 1, `pantry_events` 12, `shopping_list_items` 24, every other table 0.
Migration `002_shopping_v2` is **not applied** in production (`shopping_lists` absent).

| Module | LOC | What it does | Lucina use | APIs / stores / env | Tests | Mark |
|---|---|---|---|---|---|---|
| `index.js` | 267 (worktree +19) | `createGroceryService`: `process({userId,text,recentContext})` before the planner; purchases, consumption, shopping list add/remove/query, expenses, pantry, offers. English replies. | Yes (2 handled since Aug; shopping list via app) | — | `grocery` | PORT |
| `intent.js` | 375 | IT/EN/DA grocery intent parser, store context decision. | Yes | — | `grocery` | PORT |
| `products.js` | 45 | Canonical product resolution (`latte` → `milk`), Danish aliases. | Yes | — | `grocery` | PORT |
| `repository.js` | 164 (worktree +98) | pg queries for every grocery table; worktree adds `modifiedBy`, v2 hooks. | Yes | Postgres | `grocery`, worktree `shopping-v2-pg` | PORT |
| worktree `shopping.js` | 575 | Shopping v2: lists, idempotent ops with `op_id` receipts, revisions, `recent` status. | Not deployed | Postgres `shopping_lists`, `shopping_ops` | worktree `shopping-v2-api`, `shopping-v2-pg`, `scripts/test-shopping-v2-pg.sh` | PORT (P1) |
| worktree `shopping-catalog.js` | 115 | Loads the shopping catalog (395 items, it/en/da) for names/icons. | Not deployed | `SECRETARY_CATALOG_DIR` / `assets/catalog/*.json` | — | PORT (share `secretary-core` catalog) |
| `api.js` | 46 | `/grocery/*` dashboard API (§4.3), bearer `GROCERY_API_TOKEN`, `?userId`. | Unknown caller (Vercel `grocery-dashboard/`) | `GROCERY_API_TOKEN` | — | PORT (P6, low; open question) |
| `catalog.js` | 456 | Catalog adapters: ShelfAtlas, Salling stores, chain JSON (SPAR, MENY, REMA, Lidl, Coop/eTilbudsavis), composite. | Offer questions only | `SHELFATLAS_API_KEY`, `SALLING_API_KEY`, `GROCERY_*` (~30 names) | `grocery` | PORT (P6) |
| `nearby.js` | 70 | Chains with a store near home (eTilbudsavis `/v2/stores`, 24 h cache). | Offer questions | `GROCERY_HOME_LAT`, `_LNG`, `_LABEL`, `GROCERY_NEARBY_RADIUS_M`, `GROCERY_ETILBUD_*` | `grocery` | PORT (P6) |
| `rapidapi-quota.js` | 57 | Fail-closed monthly quota (20) for RapidAPI grocery. | Offer questions | `data/grocery/rapidapi-quota.json` (absent), `RAPIDAPI_GROCERY_*` | `grocery` | PORT (P6) |
| `receipts.js` | 107 | Filesystem receipt provider. | No (`data/grocery/receipts/` empty) | `GROCERY_RECEIPT_DIR` | — | DROP |
| `chain-json-adapters.js` | 17 | Re-export shim. Unreachable. | No | — | — | DROP |
| `webhook.js` | 17 | `/webhooks/shelfatlas` ingest. | No: `GROCERY_WEBHOOK_FORWARD_TOKEN` unset → always 401 | — | — | DROP |

### 2.9 Poko, newsroom, Cityhop, Griffin, Bot Hospital (OTHER-BOT)

None of these run for persona `lucina`, except the Bot Hospital daily audit and the routes that `index.js` mounts
unconditionally. Poko itself now runs as the Rust `poko-bot` (`pokoin-poko-bot/pokoin-rust/apps/poko-bot`) and a
separate Hermes Poko runtime on `:8789`.

| Module(s) | LOC | What it is | Mark |
|---|---|---|---|
| `bot-hospital.js` | 892 | Poko incident intake, repair (Gemini CLI, SSH to peer1), daily Poko-news proof audit (started by the Lucina runtime; sends to Lucina's Telegram). `BOT_HOSPITAL_*`, `data/bot-hospital/incidents.json` (last 2026-05-25). | OTHER-BOT (needs a host when Node Lucina is retired) |
| `poko-api.js` 507, `poko-market.js` 1202, `poko-connect.js` 490, `poko-lolbets.js` 1748, `poko-lol-analysis.js` 237, `poko-lol-matchups.js` 119, `poko-news-scout.js` 518, `poko-x-webhook.js` 328, `poko-x-hook.js` 185\*, `poko-card-x.js` 79\*, `poko-truth-friend.js` 96\*, `poko-reply-cards.js` 144, `poko-reply-gate.js` 84, `poko-isolation.js` 70, `poko-knowledge.js` 50, `poko-discord-context.js` 88, `poko-discord-slash.js` 60, `poko-abuse.js` 101 (pipe to `rust/poko-abuse`), `poko-runtime-switch.js` 17, `poko-youtube-oauth.js` 246 | 6369 | Poko assistant, market, linking, LoL bets, news scout, X presence. \* unreachable from `index.js` (run by `scripts/` + systemd timers). | OTHER-BOT |
| `hypemeter-news.js` 223, `newsroom/*` 24 files (angles 94, article-store 86, cadence 69\*, claims 687, claude 709, comment-moderation 196\*, enrich 827, evidence 294, games 206, images 547, index 212, memory 102, pipeline 231, publication 675, reporter 429, schema 557, source-fetch 371, sources 217, store 89, stories 274, style 187\*, tcg-feeds 253, templates 293, website 107) | 7935 | Poko Newsroom (articles, claims, editorial passes, website API). | OTHER-BOT |
| `cityhop-*.js` 15 files (video 1430, short-title 316, title-formats 298, pregen 242, subscribe-overlay 237, youtube-description 230, queue-api 172, subtitle 142, mpt-clips 131, narration-archive 129, telegram 82, pregen-store 49, planner-env 45, place 29, knowledge 12), `youtube-oauth.js` 157, `youtube-upload.js` 111 | 3812 | Cityhop Shorts pipeline and YouTube. `cityhop-planner-env.js` is imported by `config.js`. | OTHER-BOT |
| `griffin-youtube-oauth.js` 232, `oauth-callback.js` 48 | 280 | `/oauth2callback` dispatcher for Cityhop/Griffin/Poko YouTube tokens (served on 8788). | OTHER-BOT |
| `x-client.js` 178 | 178 | X API client (OAuth 1.0a, CRC, tweets). | OTHER-BOT |

## 3. Persisted data that the Rust Lucina must read and write unchanged

### 3.1 `data/timeline.jsonl` (+ `.1`, `.2`)

- One JSON object per line, appended with `O_APPEND`; file is `root:root` (container runs as root).
- Item: `id` (26-char Crockford ULID, monotonic per process; backfill ids derive from `sha256(seed)`), `ts`
  (ISO UTC), `channel` (`telegram|discord|web|ios|alexa|other`), `role` (`user|lucina`), `text` (≤8000),
  optional `clientMessageId` (≤128), `replyTo`, `attachments[{kind: image|voice|file, name?}]` (≤10),
  `meta{isDm, chatTitle, guildName, channelName, kind, jobId}` (worktree adds `kind: device_command*`,
  `commandId`, `ok`, `command`), `backfilled: true`.
- Today: 892 lines: 888 backfilled from Honcho (telegram 858, alexa 16, web 12, ios 2) and 4 live (web 2,
  ios 2, all 2026-10-21). No live Telegram, Discord or reminder item has been written yet.
- Rotation at 50 MB to `.1`, `.2`. Readers rebuild an id → (generation, offset, length) index and must accept
  lines appended by another process. Dedupe hash = sha256(channel, ts, role, text) for backfill.
- Writer: `scripts/backfill-timeline-from-honcho.mjs` (session → channel mapping in `docs/secretary-api.md`).

### 3.2 `data/reminders.json`

```json
{ "reminders": [ { "id": "<uuid v4>", "dedupeKey": "<chatId>:<dueAt ISO>:<lowercased text>",
  "text": "…", "dueAt": "<ISO UTC>", "timeZone": "Europe/Rome", "chatId": "<telegram chat id>",
  "userPeerId": "giuseppe", "userLabel": "…", "persona": "Lucina",
  "source": "telegram|secretary|discord|alexa|seed|seed-pokoin-tax|gmail-learn|study-focus",
  "status": "pending|sent|cancelled", "createdAt": "…", "sentAt": null, "cancelledAt": "<only when cancelled>",
  "lastError": null } ],
  "updatedAt": "<ISO>" }
```

- Pretty-printed (`JSON.stringify(…, null, 2)` + newline). Kept: pending + sent + cancelled, last 500.
- Today: 45 entries; 33 `sent` (gmail-learn 25, study-focus 8), 12 `pending` (seed-pokoin-tax 10 in 2027,
  gmail-learn 3 in 2026-12). Unknown `source` values are opaque.
- Concurrency today: one Node process, an in-process promise chain, non-atomic `writeFile`. A second writer
  will corrupt it (see the single-writer rule in `PORT_PLAN.md`). Backup sibling:
  `reminders.json.bak-pokoin-tax-20260929T055623Z`.

### 3.3 Grocery Postgres (`hermes_grocery`, schema `grocery`)

- `migrations/001_grocery.sql` (applied): `canonical_products(id text pk, display_name, category, aliases jsonb)`,
  `stores(id, chain_slug, display_name, metadata)`, `store_context(user_id pk, store_id, confidence numeric(4,3),
  source, observed_at, metadata)`, `catalog_products(… UNIQUE(store_id, external_id))`, `price_observations`
  and `grocery_transactions` and `grocery_transaction_items` (with `verification_status IN (receipt_verified,
  catalog_verified, offer_verified, historical_snapshot, catalog_inferred, estimated)`), `pantry_events(event_type
  IN (purchased, consumed, finished, discarded, adjusted, expired))`, `shopping_list_items(id uuid, user_id,
  product_id, raw_name, status IN (needed, purchased, removed), created_at, updated_at, metadata)`,
  `transaction_reconciliations`, `webhook_deliveries`; 5 indexes. Requires `pgcrypto`.
- Worktree `migrations/002_shopping_v2.sql` (+ `.down.sql`), additive and idempotent: `shopping_lists(id, user_id,
  name, theme IN 8 values, position, is_default, revision bigint, created_at, updated_at, deleted_at)` with one
  default list per user; `shopping_list_items` gains `list_id NOT NULL, quantity, unit, specification,
  section_override, icon_key, catalog_item_id, position, purchased_at, modified_by, rev, name_key GENERATED`;
  status `purchased → recent`; unique active row per `(list_id, coalesce(catalog_item_id, name_key))`;
  `shopping_ops(op_id uuid pk, list_id, user_id, op_type, outcome IN (applied, rejected), error, revision,
  modified_by, applied_at)`.
- Runner: `scripts/grocery-migrate.mjs` (every `NNN_*.sql` in order; `--down NNN_name`). The Rust side must not
  run its own migration framework against this schema; use these files (sqlx `migrate` with the same files, or
  keep the Node runner) and never reorder them.

### 3.4 Honcho identifiers

- Workspace `hermes-peer1` (`HONCHO_WORKSPACE_ID`); agent peer `hermes` (`HONCHO_AGENT_PEER_ID`); owner peer
  `giuseppe`; legacy owner aliases `flareon_owner`, `telegram_user_8802741204`, `discord_user_<owner>`;
  other users `telegram_user_<id>`, `discord_user_<id>`, Alexa `alexa_user_<id>`.
- Sessions: `telegram_chat_<chatId>`, `discord_channel_<channelId>`, `alexa_session_<id>`,
  `secretary_<sessionId>` (default `secretary_app`; iOS `secretary_ios-<hex>`, web `secretary_lucina-page-<uuid>`),
  `gmail_learned_facts` (frozen), diagnostics/tests.
- Message metadata: `{role: "user"|"assistant", …turn metadata}`. Linked workspace `cursor-global` (Harness mirror).
  Poko uses `poko-peer1`/`poko`, Cityhop `cityhop-peer1`/`cityhop` (not Lucina).

### 3.5 Other files Lucina owns

| File | Format | Notes |
|---|---|---|
| `data/recent-telegram-chat.json` | `{ "<chatId or secretary session>": [{at, role, text}] }`, ≤20 turns / 5000 chars each | Written by every surface; last-writer-wins today. |
| `data/telegram-chats.json` | `{ "<chatId>" or "@user": {chatId, chatType, firstName, lastName, username, updatedAt} }` | Chat registry. |
| `data/secretary-jobs.json` | last 200 jobs (Job object in `docs/secretary-api.md`) | Not created yet. |
| worktree `data/device-context.jsonl`, `device-state.json`, `device-commands.json`, `device-digest-state.json` | events JSONL (90-day retention), state snapshot, last 200 commands, digest cursor | Device-context store; not deployed. |
| `data/lucina-proactive-checkin-state.json` | `{campaign_id, started_at, ends_at, next_at, run_count, history, completed_at, updated_at}` | Dormant. |
| `data/planner-provider-blocks.json` | `{providers, geminiModels, updatedAt}` | DROP. |
| `/run/hermes-gateway-locks/telegram-<botid>.lock` | gateway lock record (§2.1) | Shared with every Hermes-family bot on the host. |

## 4. HTTP routes (`lucina-nez`, port 8788)

### 4.1 `src/index.js` router

| Route | Methods | Auth | Handler | Lucina use | Mark |
|---|---|---|---|---|---|
| `/health` | any | none | inline `{ok, service:"hermes", memoryProvider}` | probes | PORT |
| `/telegram/<token>` | POST | secret token | Telegraf webhook, only when `HERMES_PUBLIC_URL` is set (it is empty) | no | DROP |
| `/grocery/*` | see §4.3 | Bearer `GROCERY_API_TOKEN` | `grocery/api.js` | unknown caller | PORT (P6) |
| `/webhooks/shelfatlas` | POST | `x-grocery-webhook-token` | `grocery/webhook.js` | no (token unset) | DROP |
| `/oauth2callback*` | GET | OAuth state | `oauth-callback.js` (Cityhop, Griffin, Poko YouTube) | no | OTHER-BOT |
| `/youtube-oauth2callback` | GET | OAuth | Cityhop runtime only | no | OTHER-BOT |
| `/api/secretary/*` | §4.2 | Bearer `SECRETARY_API_TOKEN` | `secretary-api.js` | yes (via `secretary-server` on p1: `SECRETARY_LUCINA_URL=http://127.0.0.1:18788`, SSH relay to nezopt:8788) | PORT |
| `/api/cityhop/video-queue` | GET | Bearer | Cityhop runtime only | no | OTHER-BOT |
| `/api/alexa/lucina` | POST | Bearer `ALEXA_LUCINA_TOKEN` | `alexa-lucina.js` | 404 (disabled) | PORT (P5) |
| `/api/poko/chat`, `/api/poko/x-webhook`, `/api/poko/observe`, `/api/poko/operation`, `/api/poko/social-post`, `/api/poko/admin`, `/api/poko/news` | POST/GET | Poko tokens | `poko-api.js`, `poko-x-webhook.js`, `newsroom/website.js` | no (poko-bot calls `:8789`) | OTHER-BOT |
| `/api/internal/guardian/alert`, `/api/internal/guardian/digest` | POST | Bearer | guardian | no caller | DROP |
| `/api/internal/bot-hospital/incident`, `/api/internal/bot-hospital/daily-news-audit` | POST | Bearer | `bot-hospital.js` | Poko | OTHER-BOT |
| `/api/internal/poko-news-scout/run` (POST), `/state` (GET), `/preview` (GET) | | hospital token | scout | Poko | OTHER-BOT |
| `/api/internal/lucina/cursor-nudge` | POST | Bearer | cursor nudge | no | DROP |
| `/api/internal/lucina/proactive-checkin` | POST | Bearer | proactive check-in | dormant | PORT (as a scheduler job) |
| `/api/internal/flareon/*` | any | — | rewritten to `/api/internal/lucina/*` | legacy | DROP |
| anything else | | | `404 {"error":"not_found"}` | | — |

### 4.2 `/api/secretary/*` (contract: Lucina `docs/secretary-api.md`; worktree adds 329 lines)

Live: `GET /health`; `POST /chat` (with `Accept: text/event-stream` and/or `"voice": true`: LucinaVoice, see "Streaming chat" below); `GET /jobs/:id`; `GET /reminders?scope=upcoming|today|all&includeSent=0|1`;
`POST /reminders` (`{text,dueAt}` or `{naturalText}`; 201/200); `DELETE /reminders/:id`;
`GET /agenda?date=&refresh=`; `GET /shopping`; `POST /shopping`; `PATCH /shopping/:itemId`;
`GET /timeline?before=&limit=`; `GET /timeline/stream` (SSE, `Last-Event-ID`/`?lastEventId=`, replay ≤500,
`retry: 5000`, `: ping` every 20 s, ≤8 streams).
**Streaming chat (LucinaVoice, `src/voice.rs`; contract: `docs/lucinavoice-protocol.md` section 1).**
`POST /chat` takes an optional `"voice": true` (any other body is unchanged; nothing new is rejected). It adds
the spoken-style rule (`voice::SPOKEN_RULE`: one to three short sentences, no markdown, lists, emoji or URLs,
numbers/dates/times as said) to the turn's fixed system text (`planner::TURN_SYSTEM`), so the JSON planner and
every `generate_reply` see it; the intent classifier does not. It works with or without the stream. With
`Accept: text/event-stream` the route answers `200 text/event-stream` at once (`Cache-Control: no-store`,
`X-Accel-Buffering: no`; after the HTTP checks: bearer, JSON body size) and runs the turn in its own task:

| event | data | when |
|---|---|---|
| `: connected` | comment | first bytes |
| `delta` | `{"text":"…"}` | reply text as the model writes it; only from `generate_reply` and its callers (the casual-chat shortcut, skill replies, fallbacks); zero or more; never tool-call markup |
| `reply` | `{"text":"…","sensitive":bool,"replaces":bool}` | exactly once, when the turn's reply text is settled: in `chat::run_turn_scoped` right after the `Finish`, before rich-reply, Honcho, memory, timeline (same for the tools planner). Turns that never reach a `Finish` (`/secret` command, OTP, project offers, a replayed `clientMessageId`) settle from the result (`sensitive` = the body's `sensitive`). `replaces` = the text, trimmed, is not the concatenation of the deltas (so also true when there were none) |
| `done` | `{"ok":true,"data":{…}}` | the exact body the plain call returns (redacted like it) |
| `error` | `{"code","message"}` | the turn failed (HTTP-level errors of the chat route, e.g. `bad_request`; 500s are `internal_error`); no `done` |
| `: ping` | comment | every 15 s while the turn runs |

The deltas come from `ChatModel::complete_streaming` (default: `complete`, the whole text as one delta;
`DeepSeek` streams; `RedactingModel` holds each fragment back to the last whitespace so a secret split across
deltas is whole when redacted). A tool-call-markup retry in `generate_reply` is not streamed: its text (or the
fixed failure line) goes out in `reply` with `replaces: true`. A client that disconnects does not cancel the
turn: it finishes and writes the timeline as usual. The `reply` text is the answer before the rich composer;
`done.data.reply` can differ when the composer rewrites it.

Worktree: `GET|POST /shopping/lists`; `GET|PATCH|DELETE /shopping/lists/:id` (`:id` = uuid or `default`);
`POST /shopping/lists/:id/ops` (`{ops, expectedRevision, channel}` → `{revision, applied, duplicates, rejected,
list, items}`); `POST /device/events`; `GET|POST /device/state`; `GET /device/commands`;
`GET /device/commands/:id`; `POST /device/commands/:id/result` (409 `command_expired`/`already_resolved`).
Error codes: 20 in the doc (`invalid_json`, `bad_request`, `due_in_past`, `unparseable_reminder`,
`invalid_status`, `unauthorized`, `not_found`, `job_not_found`, `reminder_not_found`, `item_not_found`,
`method_not_allowed` + `Allow`, `already_sent`, `payload_too_large` (64 KiB), `too_many_streams`,
`internal_error`, `reminders_disabled`, `owner_chat_unconfigured`, `grocery_unavailable`) plus worktree
`list_not_found`, `command_not_found`, `command_expired`, `already_resolved`.

### 4.3 `/grocery/*`

`GET /grocery/provider-usage`, `/expenses/summary?from&to`, `/expenses/timeline`, `/expenses/by-store`,
`/expenses/by-category`, `/purchases`, `/pantry`, `/shopping-list`, `/price-history?productId`,
`/offers?query&store`, `/savings` (stub); `PATCH /grocery/shopping-list/:id`; `POST /grocery/basket/optimize`
(stub). All take `?userId=`.

## 5. Telegram and Discord commands and intents

### 5.1 Commands

| Surface | Command | Behaviour |
|---|---|---|
| Telegram (Lucina) | `/start` | "Lucina is online." (allow-listed chats only) |
| Telegram (Lucina) | `/status`, `/status full`, `/status@bot` | `buildStatusReport` |
| Telegram (Poko, OTHER-BOT) | `/start`, `/connect CODE`, `/whoami`, `/disconnect`, `/help` (+ `setMyCommands`) | Pokoin linking |
| Discord (Poko, OTHER-BOT) | slash `/connect`, LoL bets commands; text `start/connect/disconnect/whoami/help` | |
| Discord (Lucina) | none; plain messages in DMs or admin channels, or mentions | |

### 5.2 Intents and planner actions (Lucina)

Pipeline per text turn (Telegram text/voice transcript, Secretary chat, Discord): allow-list → `/status` →
chat registry → grocery parser (handled → reply) → device command parser (worktree) → turn metadata →
tier-0 classifier → model classifier (`purpose: intent`, DeepSeek) → context policy → Honcho retrieval →
direct plan or full planner (`purpose: plan`, JSON) → action → reply/skill reply (`purpose: reply`) →
capability hints → `memory.remember` → timeline.

Tier-0 intents: `chat`, `social_greeting`, `agent_status_check`, `explicit_memory_recall`, `temporal_query`,
`continue_current_topic`, `new_topic`, `list_reminders`, `schedule_reminder`, `study_focus_reminders`,
`generate_image`, `graph_task`, `spreadsheet_update`, `web_crawl`, `oracle_monitor`, `model_status`,
`revive_qwen`, `poko_news_scout_retry`, `social_post`, `cursor_task`, `lucina_log_inspect`, `discord_admin`,
`unknown`, and grocery `grocery.purchase`, `.consume`, `.finished`, `.shopping_list.add`, `.shopping_list.remove`,
`.shopping_list.query`, `.offer.search`, `.expense.query`, `.pantries.query`, `.price.query`, `.analytics.query`.

| Planner action | Executor | Status on lucina-nez | Mark |
|---|---|---|---|
| `reply` | DeepSeek | live | PORT |
| `schedule_reminder` (`text`, `dueDate` YYYY-MM-DD or HH:MM) | `reminders.js` parser + store | live | PORT |
| `list_reminders` | store | live | PORT |
| `study_focus_reminders` | seed tips | unused since June | DROP |
| `web_crawl_task` (`url`, `query`) | Obscura | live | PORT |
| `cursor_task` (`prompt`) | DSH remote (Harness) | live | PORT |
| `spreadsheet_update_task` (`prompt`, `spreadsheetName`) | Harness + python scripts + rclone on host | live | PORT (delegating) |
| `graph_task` | rclone + xlsx | broken | DROP (open question) |
| `generate_image` (`prompt`, `caption`) | ComfyUI | broken | DROP (open question) |
| `social_post_task` (`text`, `target`) | Poko tokens via `social-operations.js` | Poko | OTHER-BOT (delegate or drop) |
| `poko_news_scout_retry` | Bot Hospital → Poko peer1 | Poko | OTHER-BOT |
| `oracle_monitor_task` | SSH | disabled | DROP |
| `model_status_task` | `ollama-runtime.js` | stale facts (DeepSeek-only) | PORT as trivial tool |
| `revive_qwen_task` | Ollama | irrelevant | DROP |
| `lucina_log_inspect` | docker logs/journalctl | partial | PORT (P6) |
| `discord_admin_task` (`operation`: `setup_support`, `create_text_channel`, `create_voice_channel`, `create_category`, `create_role`, `assign_role`, `grant_channel_access`, `lock_channel`, `create_thread`, `pin_message`, `create_invite`, `server_summary`) | discord.js | Discord only, rarely used | PORT (P4) |
| photo plan (`spreadsheet_update_task` from scale/receipt photos, or `reply`) | Gemini vision + `lucina-image-intent.js` | live | PORT (P3) |
| document plan (spreadsheet import) | download + planner | live | PORT (P3) |
| device command (worktree: alarm, timer, calendar add, delete) | timeline item to phone | not deployed | PORT (P1) |

## 6. Alexa

- One endpoint: `POST /api/alexa/lucina`, `Authorization: Bearer <ALEXA_LUCINA_TOKEN>`, JSON body
  `{query|text, userId?, sessionId?, background?: bool}`. Responses: `{ok, reply, action, started?}`;
  `background: true` → immediate "I am on it. I will send the result to Telegram." and the plan runs detached;
  async actions answer "I started the …" and notify Telegram (text or photo) when done; `discord_admin_task` is
  refused; replies are cleaned for speech and capped at `ALEXA_LUCINA_MAX_REPLY_CHARS` (700).
- It is not the Alexa Skills Kit JSON: a skill backend outside this repo translates ASK requests to this API.
  That backend was not found on nezopt, Hermes or the Pi repos; the endpoint has been 404 on `lucina-nez`
  (`ALEXA_LUCINA_ENABLED` unset) and the last Alexa session is a smoke test of 2026-05-25.

## 7. Lucina env var names (the Rust config surface)

Required now: `TELEGRAM_BOT_TOKEN_LUCINA`, `TELEGRAM_ALLOWED_CHAT_IDS`, `TELEGRAM_ALLOWED_USERNAMES`,
`TELEGRAM_UNAUTHORIZED_DM`, `DISCORD_BOT_TOKEN_LUCINA`, `DEEPSEEK_KEY_FILE` (or `DEEPSEEK_API_KEY`),
`DEEPSEEK_BASE_URL`, `DEEPSEEK_MODEL`, `DEEPSEEK_TIMEOUT_MS`, `LUCINA_DEEPSEEK_ONLY`, `GEMINI_API_KEY`,
`GEMINI_STT_MODEL`, `LOCAL_STT_ENABLED`, `LOCAL_WHISPER_MODEL`, `LOCAL_WHISPER_TIMEOUT_MS`, `HONCHO_URL`,
`HONCHO_API_KEY`, `HONCHO_WORKSPACE_ID`, `HONCHO_AGENT_PEER_ID`, `HONCHO_USE_VECTOR_SEARCH`,
`HONCHO_MIRROR_WRITES`, `HONCHO_FALLBACK_URLS`, `HONCHO_CONTEXT_TOKEN_LIMIT`, `MEMORY_PROVIDER`,
`LUCINA_OWNER_PEER_ID`, `LUCINA_OWNER_USER_PEER_ID`, `LUCINA_OPERATOR_CHAT_ID`, `LUCINA_OWNER_DISCORD_USER_ID`,
`LUCINA_REMINDERS_ENABLED`, `LUCINA_REMINDERS_STORE_PATH`, `LUCINA_REMINDER_TIMEZONE`, `LUCINA_REMINDER_*`,
`LUCINA_TIMELINE_*`, `SECRETARY_API_ENABLED`, `SECRETARY_API_TOKEN`, `SECRETARY_SESSION_ID`,
`SECRETARY_GROCERY_USER_ID`, `SECRETARY_BRIEFING_CACHE_MS`, `SECRETARY_JOBS_PATH`, `GROCERY_DATABASE_URL`,
`GROCERY_API_TOKEN`, `GROCERY_HOME_*`, `GROCERY_NEARBY_RADIUS_M`, `GROCERY_ETILBUD_*`, `SHELFATLAS_API_KEY`,
`SALLING_API_KEY`, `RAPIDAPI_GROCERY_*`, `DSH_REMOTE_ENABLED`, `DSH_REMOTE_URL`, `DSH_REMOTE_TOKEN_FILE`,
`DSH_REMOTE_OWNER_ID`, `DSH_REMOTE_WORKSPACE`, `DSH_REMOTE_TIMEOUT_MS`, `DSH_REMOTE_POLL_MS`,
`DSH_REMOTE_WAIT_SECONDS`, `OBSCURA_BINARY`, `OBSCURA_WEB_ENABLED`, `LUCINA_WEB_CRAWL_*`,
`LUCINA_IMAGE_VISION_PRIMARY`, `HERMES_GATEWAY_LOCK_DIR`, `HERMES_GATEWAY_INSTANCE`, `LUCINA_RUNTIME_HOST`,
`LUCINA_RECOVERY_COMPANION_*`, `LUCINA_OPERATOR_BRIEF_MAX_CHARS`, `LUCINA_CAPABILITY_HINTS_ENABLED`,
`LUCINA_FAST_CHAT_*`, `LUCINA_INTENT_*`, `LUCINA_TURN_TRACE`, `PLANNER_TRANSIENT_WAIT_MS`.
Later phases: `ALEXA_LUCINA_*`, `LUCINA_PROACTIVE_CHECKIN_*`, `LUCINA_DEVICE_*`, `DISCORD_*` (allow/admin lists),
`LUCINA_LOG_*`.
Not carried over: `OPENAI_*` (Node requires `OPENAI_API_KEY` at boot even though Lucina never calls it),
`OPENROUTER_*`, `GEMINI_PLANNER_*`, `LUCINA_GEMINI_*`, `LUCINA_GEMMA_*`, `LUCINA_OPENROUTER_*`,
`LUCINA_CURSOR_*`, `HERMES_CURSOR_*`, `CURSOR_*`, `HERMES_CODEX_*`, `CODEX_*`, `ZCODE_*`, `LUCINA_IMAGE_GEMMA_*`,
`LUCINA_CPU_*`, `LUCINA_QWEN_*`, `LUCINA_SHARE_LOADED_QWEN`, `LUCINA_YIELD_GPU_TO_HARNESS`, `OLLAMA_*`,
`LUCINA_SEMANTIC_JUDGE*`, `ORACLE_MONITOR_*`, `PEER1_*`, `KEEP_*`, `GMAIL_*`, `LUCINA_GMAIL_*`, `COMFYUI_*`,
`IMAGE_*`, `HERMES_HARNESS_*` (headless path), `GOOGLE_AI_STUDIO_*`, `GUARDIAN_*`, `LUCINA_CURSOR_NUDGE_*`,
and every Poko/Cityhop/Griffin/Bot Hospital name.

## 8. `scripts/`, `services/`, `tools/`

Runtime-relevant entries are listed one by one; the rest are grouped.

| Path | LOC | Role | Mark |
|---|---|---|---|
| `scripts/run-dsh-headless.sh` | 53 | Headless Harness fallback (`HERMES_HARNESS_AGENT_PATH`). | DROP (remote path only) |
| `scripts/append-weight-log.py`, `append-receipt-log.py` | 175, 322 | Run by Harness for `spreadsheet_update_task` (Drive via rclone on host). | PORT-as-is (stay Python, called through Harness) |
| `scripts/lucina-crawl4ai.py` | 239 | Crawl4AI fallback for web crawl. | DROP |
| `scripts/backfill-timeline-from-honcho.mjs` | 309 | Imports Honcho history into the timeline (one-off, idempotent). | PORT (P1, `lucina` CLI subcommand) |
| `scripts/grocery-migrate.mjs` | 23 | Grocery migration runner. | PORT (CLI subcommand over the same SQL files) |
| `scripts/grocery-price-snapshot.mjs` + `systemd/grocery-price-snapshot.{service,timer}` | 61 | Offer snapshots; timer not installed on nezopt. | DROP (P6 if wanted) |
| `scripts/lucina-proactive-checkin.py` + `install-lucina-proactive-checkins.sh` | 388, 92 | Timer caller of the proactive check-in route; not installed on nezopt. | DROP (Rust scheduler replaces it) |
| `scripts/honcho-cursor-watch.{py,sh}`, `install-honcho-cursor-watch.sh` | 232, 13, 169 | Cursor nudge caller. | DROP |
| `scripts/keep-auth.js`, `sync-keep-notes.js` | 51, 52 | Google Keep import. | DROP |
| `scripts/lucina-seed-study-focus-reminders.mjs` | 34 | Seed. | DROP |
| `scripts/test-intent-heuristics.mjs`, `test-lucina-intent.mjs`, `lucina-intent-50.mjs`, `lucina-intent-eval.mjs`, `run-intent-eval.mjs`, `run-semantic-eval.mjs`, `capture-semantic-traces.mjs`, `lucina-context-analyze.mjs` | 66–283 | Intent/semantic evals; their case sets become Rust golden tests. | PORT (as test fixtures) |
| `scripts/check-imports.mjs` | 28 | CI import check. | DROP |
| `scripts/grocery-telegram-*.mjs` | 171 | Grocery canaries over Telegram. | PORT (as golden transcripts) |
| `scripts/honcho-memory-hook.py`, `honcho-mcp-lib.sh`, `cursor-*`, `dsh-*`, `codevira-*`, `install-codex-*`, `qwen-mcp-server.py`, `gemma4-cursor-proxy.mjs` | ~1600 | Dev-tool memory hooks and MCP wrappers for Cursor/Codex/DSH. | OTHER-BOT (tooling) |
| `scripts/cityhop-*`, `griffin-*`, `patch-cityhop-*`, `patches/*`, `build-cityhop-lower-thirds.py`, `configure-cityhop-*` | ~8000 | Cityhop/Griffin video pipelines. | OTHER-BOT |
| `scripts/poko-*`, `newsroom-*`, `push-riot-key-peer1.sh`, `configure-peer1-poko-*` | ~1900 | Poko timers and newsroom desk. | OTHER-BOT |
| `scripts/install-*`, `deploy-*`, `configure-*`, `sync-*`, `peer1-*`, `peer2-*`, `pi-*`, `start-*`, `nes*`, `remote-*`, `verify-*`, `lucina-secrets.sh` (~110 files) | ~8500 | Host installers for retired hosts (nespc, peer1/peer2 failover, Pi replica), tunnels, secrets sync. | DROP (for Lucina; nezopt deploy is the compose file) |
| `services/gliclass-judge/` | 1703 (py) | GLiClass semantic judge service + benches. Not running. | DROP |
| `tools/honcho-mcp/` | 1380 (ts) | Honcho MCP server for Cursor/DSH. | OTHER-BOT (tooling) |
| `tools/hermes-local-mcp/` | 125 | peer1 Docker MCP. | OTHER-BOT (tooling) |
| `rust/poko-abuse` | — | Poko abuse screener. | OTHER-BOT |
| `workflows/comfyui-text-to-image.json` | — | ComfyUI workflow. | DROP |
| `docs/lucina-persona.md`, `lucina-recovery-companion.md`, `lucina-operator-brief.md` | 19, 153, 69 | Prompt assets loaded at runtime. | PORT (copy into the Rust crate, keep one source) |
| `docs/lucina-intent-eval-set.json` (36 cases), `lucina-semantic-eval-set.json` (106), `lucina-semantic-active-traces.json` | — | Eval fixtures. | PORT (tests) |

## 9. Counts

`src/`: 149 files in the live tree + 4 new worktree files = 153 entries (every Poko, newsroom and Cityhop file
counted one by one). LOC are the live files, the four new ones from the worktree.

| Mark | Files | Node LOC |
|---|---|---|
| PORT | 58 | 21 022 (includes the four mixed files `telegram.js`, `discord.js`, `openai.js`, `planner-fallback.js` whole: 6 590 LOC, of which roughly 3 300 are Lucina code) |
| DROP | 26 | 3 552 |
| OTHER-BOT | 69 | 20 357 |
| Total | 153 | 44 931 |

Outside `src/`: scripts/services/tools entries in §8: PORT 6 groups (fixtures, backfill, migrate runner, Harness
python scripts, prompt assets, eval sets), DROP 11 groups, OTHER-BOT 7 groups.

Tests: 57 files in `tests/` (9177 LOC, `node --test`), 3 more in the worktree. Lucina-relevant: `agent-v022-port`,
`budget-truncation`, `cache-isolation`, `capability-hints-language`, `concurrency`, `context-policy`, `grocery`,
`identity`, `identity-negative`, `intent-taxonomy`, `lucina-style`, `no-honcho-greeting`, `plan-parse`,
`proactive-prompt`, `recent-chat`, `reminder-timers`, `secretary-api`, `temporal-robustness`, `timeline`,
`timeline-api`, `turn-context`, `turn-metadata`, worktree `shopping-v2-api`, `shopping-v2-pg`, `device-bridge`.
Semantic-judge, gpu-budget, gliclass, zcode and every `poko-*`/`newsroom-*` test belong to DROP or OTHER-BOT modules.
