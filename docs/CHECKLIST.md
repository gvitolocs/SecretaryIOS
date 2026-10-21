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
- [ ] Lucina MCP (`crates/secretary-mcp`, being built): Claude Code, ChatGPT connector (OAuth), Grokbot

## Waiting
### Web
- [x] Persistent full-body Lucina in the right column on desktop, compact on mobile; `/lucina/` retired (20e811e, live)
- [x] Shopping identical to the Bring! layout (78c7aab, live 2026-10-21; icons decision pending)
- [x] Gym parity with Lucina Gym, plus a layout guard for the clipping (b40dec6, live)
- [x] i18n: it / en / da for shell, chat, login, weight (20e811e); Gym and Shopping still to adopt it
- [x] Deploy the new web shell (48d52d3)
- [ ] Futuristic redesign of the whole UI and the Lucina stage (Custom Cast study), in progress

### Avatar
- [ ] Owner picked Summer, but in her uniform: variants A/B being built → → owner picks → convert → p1 private assets
- [ ] Animation library (VMD, Quaternius, CMU, Bandai Namco, text-to-motion) via a bone map

### iOS
- [ ] Milestones on the iPhone: timeline chat, persistent 3D Lucina, i18n, Bring-identical shopping, gym on FFI
- [ ] Device bridge in the app: AlarmKit, EventKit, HealthKit, location, App Intents, Safari extension
- [ ] Reinstall before the free 7-day profile expires

### Server and Lucina (Node, until the port replaces it)
- [ ] Device bridge routes: done in `secretary-server` (53d4ec4), not yet deployed
- [ ] Lucina device bridge (Node worktree `secretary-shopping-v2`)
- [x] Shopping v2 deploy: migration 002 + catalog to Lucina and p1 (2026-10-21)
- [ ] Discord owner user ID for the timeline filter (ask the owner)

### Roadmap modules (`docs/ROADMAP.md`)
- [ ] Calendar · Languages · Memory exercises · Purchases (always confirmed)

### Infra follow-ups
- [ ] p2 ingest-api still calls 10.0.0.170:8789
- [ ] Hermes drop-ins hold an inline token
- [ ] Honcho memory gap
- [ ] Unmerged branches: Hermes v0.22.3 port, `fix/cursor-api-key-env`
