# Secretary checklist

**Current focus (owner, 2026-10-21): rewrite Lucina completely in Rust.** Everything else waits below. Work already
running finishes, but nothing new starts until the Rust Lucina is live.

## Focus: Lucina in Rust
- [x] Inventory of the Node Lucina (every module PORT / DROP / OTHER-BOT) and the port plan → `docs/lucina-rust/`
- [ ] Core crates on branch `lucina-rs`: `lucina-llm` (DeepSeek + tools), `lucina-tools` (plugins, confirmation flow),
      `lucina-memory` (Honcho), `lucina-store` (timeline/reminders, Node-compatible), `lucina-grocery`, `lucina-server`
- [x] Rich replies LIVE on Rust (lucina-rs 9c3dddc, LUCINA_RICH_REPLIES=true) + web rich chat LIVE (main 2bfe0b6); iOS rich chat installed on iPhone (ios-rich-chat bff95d9). Node (Telegram) still needs flag + restart
- [x] Intent false-positive fix live on Rust (c33f67c); Node f4b61e2 committed, restart of lucina-nez held until the shopping item-objects migration 003 is applied
- [x] Phase 1: Secretary API on the Rust Lucina — CUT OVER 2026-10-22 16:35 (parity 106/106, parsers 239/239)
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

## Proactive Lucina (owner, 2026-10-22: "she reaches out before you ask")
- [x] MVP1+2 built; lucina-proactive.service RUNNING IN DRY-RUN since 2026-10-22 15:13 (server/web routes live). Go live: LUCINA_PROACTIVE_DRY_RUN=false in ~/.config/lucina-proactive/env after owner reviews `lucina-proactive report --hours 24`
- [ ] MVP3 persistent background tasks + Smart Approval (Approve/Edit/Ignore)
- [ ] MVP4 external sources (email, GitHub, Pokoin servers)
- [ ] MVP5 learning from feedback + Lucina proposes automations

## Waiting
### Web
- [x] Persistent full-body Lucina in the right column on desktop, compact on mobile; `/lucina/` retired (20e811e, live)
- [x] Shopping identical to the Bring! layout (78c7aab, live 2026-10-21; icons decision pending)
- [x] Gym parity with Lucina Gym, plus a layout guard for the clipping (b40dec6, live)
- [x] i18n: it / en / da for shell, chat, login, weight (20e811e); Gym and Shopping still to adopt it
- [x] Deploy the new web shell (48d52d3)
- [x] Futuristic redesign of the whole UI and the Lucina stage (Custom Cast study), live edb6405

### Avatar
- [ ] Base = G (game-accurate, owner pick 2026-10-22 18:30): deploy G pack + clips + toon-g stage; refit 12 outfits to G; physics (spring bones) on G chains
- [x] Wardrobe UI LIVE (outfit/hair/colour tabs, rules per tab, remembered); LUCINA_AVATAR_OUTFIT not yet on
- [ ] Outfit + hairstyle GLBs (Custom Cast first, then free assets) on the FIXED B
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
- [x] Links tab LIVE 2026-10-22 (main 64c747a): Google Calendar (waits on owner's Google Cloud Console steps), ICS, iPhone calendar, channels, AI clients, custom links
- [x] Lucina calendar bridge LIVE (LUCINA_CALENDAR=true); MCP calendar_list/calendar_create (proposals only)
- [x] Notifications LIVE 2026-10-22 (merged into main 64c747a). iOS native part on branch ios-notifications (not installed). APNs needs a paid team

### Tasks / Todoist (owner, 2026-10-22)
- [ ] secretary-tasks engine + Todoist two-way sync (official API), Todoist-like UX on web + iOS, Lucina/MCP tools, Links card — branch tasks-todoist

### Shopping icons (owner, 2026-10-22)
- [x] 395 original SVG item icons + 17 sections, style B, each inspected — LIVE on web (main 2556872); iOS wiring + 8 list-theme images pending

### Secrets (owner, 2026-10-22)
- [x] lucina-secrets skill (Infisical refs, redaction everywhere, no reveal path, sudo out of scope) on branch lucina-secrets — owner steps pending (Infisical identity, tss group, store-identity.sh)

### Notes (owner, 2026-10-22)
- [x] Notes LIVE (main 326fbf2): server /v1/notes, MCP note_save/append/get/search, Lucina LUCINA_NOTES, web + iOS (ios-app 0986f57); craftit-meeting briefing restored; ask_lucina limit 12000

### Roadmap modules (`docs/ROADMAP.md`)
- [ ] Calendar · Languages · Memory exercises · Purchases (always confirmed)

### Infra follow-ups
- [ ] p2 ingest-api still calls 10.0.0.170:8789
- [ ] Hermes drop-ins hold an inline token
- [ ] Honcho memory gap
- [ ] Unmerged branches: Hermes v0.22.3 port, `fix/cursor-api-key-env`
