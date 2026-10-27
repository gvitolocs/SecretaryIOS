# Retiring the Node Lucina (2026-10-26)

The Node Lucina (`lucina-nez` container, image `lucina:nez`, repo `~/Projects/Lucina`, branch `secretary-live`)
handed every capability to the Rust `lucina-rs` (systemd user unit on nezopt). This file is the runbook and the
record. Verification: `docs/verification.md` protocol 15 (branch `nr-verify`).

## What Node still did on 2026-10-26, and where it went

| Capability | Node | Rust | Switch |
|---|---|---|---|
| Reminder scheduler + delivery (Telegram, timeline, app notification) | `reminders.js` scheduler, 60 s | `lucina-store` `process_due` (claim before send; uncertain send → `failed`, never resent) + `reminder_delivery.rs` | Node `LUCINA_REMINDERS_ENABLED=false`, Rust `LUCINA_REMINDERS_WRITER=local`, `LUCINA_SCHEDULER=true` |
| Telegram @Lucina00_bot | Telegraf poller + gateway lock | `lucina-telegram` poller (flock + Node-compatible lock record, persisted update claim) + `src/telegram/` | Node `TELEGRAM_BOT_TOKEN_LUCINA=`, Rust `LUCINA_TELEGRAM_ENABLED=true` |
| Discord Lucina#8184 | discord.js | `lucina-discord` message session (flock) + `src/discord/` | Node `DISCORD_BOT_TOKEN_LUCINA=`, Rust `LUCINA_DISCORD_ENABLED=true` |
| Bot Hospital daily Poko-news audit | `bot-hospital.js` | `src/bot_hospital.rs` (`lucina-rs bot-hospital --dry-run`) | Node `BOT_HOSPITAL_ENABLED=false`, Rust `BOT_HOSPITAL_ENABLED=true` |
| Proactive check-ins | `/api/internal/lucina/proactive-checkin` (no caller since 2026-08) | `lucina-proactive` engine (own unit) | `timeline_sink = "file"`, `LUCINA_PROACTIVE_DRY_RUN=false` |
| Harness agent (`cursor_task`, spreadsheets) | `dsh-remote-client.js` | `jobs.rs` / `lucina-delegate` (already live) | — |
| Secretary API, timeline, grocery, deals, OTP, share, media, memos | — | already Rust before this step | — |
| Delivery-path alerts | — | `src/delivery_monitor.rs`: a path down for `LUCINA_DELIVERY_ALERT_AFTER_MIN` (5) alerts the app (Telegram when the app path is the one down) | always on |
| Watchdog | — | `deploy/lucina-rs/health-check.sh` via `lucina-rs-health.timer`: two `/health` failures in a row → restart; failed unit → reset + start | timer |

Dropped with Node (no traffic, or replaced): Alexa endpoint (404 since May; a new Rust skill backend is on
`docs/CHECKLIST.md`), `/grocery/*` dashboard API, ShelfAtlas webhook, guardian alerts/digest, cursor nudges,
Cityhop and Poko routes on :8788 (Poko runs on p2), `/oauth2callback` (Poko/Griffin YouTube re-auth: run the
re-auth from the Poko host), Gemini CLI consultation and SSH restarts in the Bot Hospital, `graph_task` and
`generate_image` (broken on this host).

## Order (one capability at a time, each through `/tmp/lucina-merge.lock` + `deploy-guard`)

Every Node change is an override at the end of `~/Projects/Lucina/private/lucina/lucina.nez.env` (the last
`env_file` of `docker-compose.lucina.nez.yml`, so it wins), then
`docker compose -f docker-compose.lucina.nez.yml up -d lucina` in `~/Projects/Lucina`.

1. **Reminders.** Pick a moment with no reminder due in the next 10 minutes (Node's tick is then idle). Node
   `LUCINA_REMINDERS_ENABLED=false` and recreate; confirm the log no longer prints "reminder scheduler started".
   Only then Rust `LUCINA_REMINDERS_WRITER=local`, `LUCINA_SCHEDULER=true`, restart. Between the two, reminder
   requests fail visibly ("reminders disabled"); nothing writes `reminders.json`. Check: `/health` `scheduler`
   ticks; a reminder one minute ahead arrives once on Telegram and in the app.
   Rollback: Rust back to `writer=node`, scheduler off, restart **first**; then Node's override removed.
2. **Telegram.** Node `TELEGRAM_BOT_TOKEN_LUCINA=` and recreate (SIGTERM releases its lock record); then Rust
   `LUCINA_TELEGRAM_ENABLED=true`. Rust never drops pending updates, so messages sent during the switch are
   answered. Check: `/health` `telegram.leaseHeld`, `lastPollOkAt`; the lock record names `instance: lucina-rs`.
3. **Bot Hospital.** Node `BOT_HOSPITAL_ENABLED=false`; Rust `BOT_HOSPITAL_ENABLED=true` (+ the `BOT_HOSPITAL_*` and
   `POKO_HOSPITAL_TOKEN` values copied into `lucina.env`).
4. **Discord.** Node `DISCORD_BOT_TOKEN_LUCINA=`, recreate, wait until the bot shows offline; then Rust
   `LUCINA_DISCORD_ENABLED=true`.
5. **Proactive.** `~/.config/lucina-proactive/config.toml` `timeline_sink = "file"` (was `docker exec lucina-nez`),
   then the unit's `LUCINA_PROACTIVE_DRY_RUN=false`.
6. **Node off.** `docker stop lucina-nez`, watch, then `docker rm lucina-nez`, `docker save lucina:nez | zstd` to
   `~/backups/`, `docker rmi lucina:nez`.
7. **Config.** `~/.config/lucina-rs/node.env` → `lucina.env` (frozen copy plus the Telegram/Discord/Bot Hospital keys
   that only the container had; values never printed); the unit loads `lucina.env` and runs with
   `WorkingDirectory=%h`. `lucina-memo.service` loads `lucina.env` too.
8. **Archive.** `~/Projects/Lucina` becomes read-only (the data directory `data/` and the `rust/lucina-pokemon`
   crate stay writable: other services use them). The repo itself is deleted only with the owner's approval.

## Record (2026-10-27, UTC)

| Time | Step | lucina-rs |
|---|---|---|
| 08:24 | Reminders: Node `LUCINA_REMINDERS_ENABLED=false` (idle tick), Rust `writer=local` + scheduler | `8fda1b6` |
| 09:00 | First Rust delivery in production (reminder due 09:00, sent 09:00:23, Telegram + timeline + app) | |
| 12:38 | Merge deploy: relative-day reminders ("domani mattina", regression test from the iPhone), chat-reply, memo-events, share secrets fix | `1b210c7` |
| 12:40 | Telegram: Node token emptied (no update handled for 6 h, nothing unconfirmed), Rust poller holds `telegram-8781061108` (flock + record `instance: lucina-rs`); unit on `lucina.env`, `WorkingDirectory=%h`, new watchdog | `1b210c7` |
| 12:41 | Bot Hospital: Node off, Rust on (`BOT_HOSPITAL_DEDUPE_MS=86400000`); first Rust audit 12:48 healthy | `1b210c7` |
| 12:43 | Discord: Node token emptied, Rust message session ready (3 guilds), `DISCORD_ALLOWED_USER_IDS` = the owner | `1b210c7` |
| 12:44 | `lucina-proactive` live (`dry_run=false`, drop-in `live.conf`), timeline sink `file` | |
| 12:45–12:49 | `lucina-nez` stopped, image saved, container and image removed; Node app files read-only, tag `node-lucina-final` | |
| 12:50 | `node.env` retired (`lucina-memo`, `lucina-diagnostics` load `lucina.env`) | |
| 12:55 | Bot Hospital waits a day across restarts; default paths after the Projects reorganisation; `mode.env` without the Node settings | `9418561` |

Backups: `~/mnt/mybook/backups/lucina-node-retired-20261027/` (0600: image, container config, git bundle, uncommitted diff),
`~/.local/bin/lucina-rs.bak-*`, `~/.config/lucina-rs/*.bak-*`, `lucina.nez.env.bak-*`.
