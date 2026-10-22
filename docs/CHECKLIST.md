# Secretary checklist

**Current focus (owner, 2026-10-21): rewrite Lucina completely in Rust.** Everything else waits below. Work already
running finishes, but nothing new starts until the Rust Lucina is live.

## Focus: Lucina in Rust
- [x] Inventory of the Node Lucina (every module PORT / DROP / OTHER-BOT) and the port plan → `docs/lucina-rust/`
- [ ] Core crates on branch `lucina-rs`: `lucina-llm` (DeepSeek + tools), `lucina-tools` (plugins, confirmation flow),
      `lucina-memory` (Honcho), `lucina-store` (timeline/reminders, Node-compatible), `lucina-grocery`, `lucina-server`
- [ ] Phase 1: the Secretary API (app + web) served by the Rust Lucina, side by side with Node, then cut over
- [ ] Phase 2: reminders and scheduler (single writer)
- [ ] Phase 3: Telegram
- [ ] Phase 4: Discord (owner: keep, with Lucina's persona; needs the owner's Discord user id)
- [ ] Phase 5: Alexa (owner: keep, as a new Rust skill backend)
- [ ] Phase 6: the remaining PORT modules; Node container retired for Lucina
- [ ] Lucina plugins as Rust tools: weather, places, web, notes, calculator, reminders, shopping, device,
      gym/body, memory search, translate, plus the useful Hermes upstream tools
- [ ] Rebuild image generation and spreadsheet graphs as Rust tools (owner, 2026-10-21)
- [x] Lucina MCP live at https://getsecretary.tech/mcp (8bc9cc9, p1 `secretary-mcp`), Claude Code connected
- [x] Lucina guard for MCP turns (allowlist of actions) + strip the MCP prefix before intents (c37de51, live)
- [x] ChatGPT (custom plugin, OAuth) and Grok Bot (custom MCP server, OAuth) connected
- [ ] Grok Bot caller shows as "www": map the cursor.com redirect host to "grok"

## Waiting
### Web
- [x] Persistent full-body Lucina in the right column on desktop, compact on mobile; `/lucina/` retired (20e811e, live)
- [x] Shopping identical to the Bring! layout (78c7aab, live 2026-10-21; icons decision pending)
- [x] Gym parity with Lucina Gym, plus a layout guard for the clipping (b40dec6, live)
- [x] i18n: it / en / da for shell, chat, login, weight (20e811e); Gym and Shopping still to adopt it
- [x] Deploy the new web shell (48d52d3)
- [x] Futuristic redesign of the whole UI and the Lucina stage (Custom Cast study), live edb6405

### Avatar
- [ ] Base = variant B (Summer face + tiara on the Engage body), owner pick 2026-10-22: deploy B + its clips to p1; game toon shading only as an optional material
- [ ] Wardrobe: outfits (gym+ponytail, maid, chef, c584, c591) + Wardrobe UI (Custom Cast style), remembered, auto per tab, Lucina can change it
- [x] Owner picked B (flower removed, tiara restored)
- [x] Animation library: 22 clips per model (f432912 stage API; MMD clips live on p1)

### iOS
- [ ] Milestones on the iPhone: timeline chat, persistent 3D Lucina, i18n, Bring-identical shopping, gym on FFI
- [ ] Device bridge in the app: AlarmKit, EventKit, HealthKit, location, App Intents, Safari extension
- [ ] Reinstall before the free 7-day profile expires

### Server and Lucina (Node, until the port replaces it)
- [ ] Device bridge routes: done in `secretary-server` (53d4ec4), not yet deployed
- [ ] Lucina device bridge (Node worktree `secretary-shopping-v2`)
- [x] Shopping v2 deploy: migration 002 + catalog to Lucina and p1 (2026-10-21)
- [x] Discord owner user ID: 649020113026023475 (vitologic), set in Lucina 2026-10-22

### Links, calendar, notifications (owner, 2026-10-22)
- [ ] Links tab: connections hub (Google Calendar OAuth, ICS, iPhone calendar, channels, AI clients, custom links) — branch links-calendar
- [ ] Lucina + Hermes read/write the calendar (with confirmation), agenda/briefing
- [x] Notifications LIVE 2026-10-22 (branch notifications 6314a87 deployed; merge into main pending the hair agent). iOS native part on branch ios-notifications (not installed). APNs needs a paid team

### Roadmap modules (`docs/ROADMAP.md`)
- [ ] Calendar · Languages · Memory exercises · Purchases (always confirmed)

### Infra follow-ups
- [ ] p2 ingest-api still calls 10.0.0.170:8789
- [ ] Hermes drop-ins hold an inline token
- [ ] Honcho memory gap
- [ ] Unmerged branches: Hermes v0.22.3 port, `fix/cursor-api-key-env`
