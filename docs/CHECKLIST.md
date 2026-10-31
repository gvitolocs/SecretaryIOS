# Secretary checklist

**Current focus (owner, 2026-10-21): rewrite Lucina completely in Rust.** Everything else waits below. Work already
running finishes, but nothing new starts until the Rust Lucina is live.

## Focus: Lucina in Rust
- [x] Inventory of the Node Lucina (every module PORT / DROP / OTHER-BOT) and the port plan → `docs/lucina-rust/`
- [ ] Core crates on branch `lucina-rs`: `lucina-llm` (DeepSeek + tools), `lucina-tools` (plugins, confirmation flow),
      `lucina-memory` (Honcho), `lucina-store` (timeline/reminders, Node-compatible), `lucina-grocery`, `lucina-server`
- [x] Integration LIVE 2026-10-22 20:30: lucina-rs 766f2c5 (items, tasks bridge, secrets redaction, Verus-proof fixes incl. MCP-confirm), Node d372ecf (rich replies, tasks, redact on Telegram/Discord), grocery migration 003 + item objects applied
- [ ] Lucina delegates to Claude/Grok/ChatGPT (autonomy policy) — branch lucina-delegate
- [x] Meshy AI 3D (lucina-meshy 0c1944f + 341b542) merged into lucina-rs 3028e37 and LIVE on nezopt 2026-10-22 18:43 with Meshy OFF (LUCINA_MESHY unset, health `meshy: null`, 15 built-in tools). Owner step: the Meshy API key (docs/lucina-meshy.md), then LUCINA_MESHY=true
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
- [x] Lucina delegates on her own to Claude (CLI), Grok (Grok Bot / xAI) and ChatGPT (OpenAI / Codex): self-judged,
      no confirmations, hard limits (no secrets out, no money, depth 1, never from MCP, code only in a bwrap sandbox
      on `lucina/<job>`), budget as a runaway guard, cards + notes + audit → `docs/lucina-rust/DELEGATION.md`
      (branch `lucina-delegate`)
- [ ] Deploy delegation: `LUCINA_DELEGATE=true` + `~/.config/lucina-rs/delegate.toml` on lucina-rs
- [ ] Grok for Lucina: sign the Grok Bot app on nezopt back in (it is signed out), or add an xAI key
      (`delegate.xai_api_key`); until then research goes to ChatGPT (Codex)
- [ ] Proactive engine: call `/api/secretary/delegate` (dry run) for research once `lucina-proactive` merges
- [ ] Telegram/Discord (Node Lucina): thin bridge to `/api/secretary/delegate`-style delegation, behind a flag
- Verification findings (docs/verification.md on main), merged into lucina-rs 2026-10-24 (93a023b, 6fb355c); workspace
  fmt/clippy clean, lucina-* tests 339 passed with Postgres; lucina-rs binary LIVE 2026-10-24 12:43:
  - [x] T1: timeline stream resumes by arrival order (fix-verification-lucina 845b04b)
  - [x] P1a/P1b: lucina-rs keeps its own jobs file (`data/lucina-rs/secretary-jobs.json`), Node's read only (cad62c5)
  - [x] P2a/P2b: reminders claimed before they are sent; phase-2 handover order in PORT_PLAN (4b395cb)
  - [x] S1: the 409 names the batch's duplicates (390c0e5)
  - [x] S2: client row ids kept (390c0e5); migration 004 (`grocery.shopping_row_aliases`) not yet applied to the live database
  - [x] G4: Meshy per-step credit guard, worst-case reservation (5c3b038); LUCINA_MESHY still off
  - [x] Secrets redactor overlap: only well-formed patterns admitted (52f55e0)
  - [x] G3: proactive tick lock (fix-verification-proactive 6ca0059); the running lucina-proactive binary is older (not redeployed)

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
- [x] Spring-bone physics (avatar-physics d195216) merged into main fe9925a and LIVE on web 2026-10-22 18:46; p1 manifest.json has the `physics` block (19 chains bind on G in headless SwiftShader). iOS bundle on ios-app 809cc5a, not yet built/installed
- [x] G LIVE 2026-10-22 20:15 (main dc79130 + G pack + 22 clips; 5 held back); iPhone ios-app 73b38a3
- [x] Touch + Custom Cast studio LIVE on web (main d1c7384): orbit/pinch/tap reactions, studio, photo, dance, gyro, presets
- [ ] Refit 12 outfits to G (wardrobe rules temporarily all → uniform)
- [ ] Physics (spring bones) on G chains
- [ ] Character editor (Custom Cast SceneEdit parity): body/face sliders, eyes/skin/makeup, hair colours, accessory slots, outfit mix & match, saved looks — branch avatar-editor
- [ ] Studio extras: hand shapes, IK posing, face sliders, props, photo decorations, flick shortcuts, filters, gaze, image backgrounds — branch studio-extras
- [ ] AR + face tracking: web Mirror me (MediaPipe, self-hosted) + stage face drive + .usdz assets merged into main bd720eb and LIVE on p1 2026-10-22 19:09; iOS AR (ios-ar 31b4305) merged into ios-app 29ff62f, not built yet. lucina.usdz + lucina-ar.json NOT on p1 yet (held for the G neck fix pack)
- [x] Per-model idle clip pools from the manifest state_clips (g-idle 86f8d1e) merged into main bc58870, LIVE on web 2026-10-22 19:09
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

### Models gallery (owner, 2026-10-22)
- [x] secretary-server /v1/models (auth-only, `$SECRETARY_DATA_DIR/models`, 200 MB cap in the server; Caddy has no body limit) + web /models gallery and inline model3d viewer + lucina-stage connection-leak fix (models-gallery 45163de) — merged into main fe9925a, LIVE on p1 2026-10-22 18:46 (unauth GET /v1/models → 401)

### Verification (owner, 2026-10-22)
- [x] Fixes found by the models (verify-fixes 1e19f27): secretary-mcp OAuth replay races + web timeline stream gap — merged into main, LIVE on p1 2026-10-22 18:46
- [x] TLA+ specs + Stateright models + scripts/verify.sh + docs/verification.md (verification 2599196) and Verus proofs (verus-proofs 81076a2) — merged into main fe9925a; `cargo test --workspace` runs the models
- Open findings (docs/verification.md; not fixed yet):
  - [ ] T1: a late timeline line is skipped on resume (resume is by id; a slow Node write lands behind a newer id). Fix: one writer, or resume with overlap
  - [ ] P1a/P1b: the cutover shares `secretary-jobs.json` between Node and Rust (false `interrupted`, lost job update). Fix: Rust gets its own jobs file
  - [ ] P2a/P2b: the phase-2 reminders handover as written runs two `reminders.json` writers; a restart mid-tick re-sends the batch
  - [ ] S1: a lost ack followed by a 409 rebase applies a shopping op twice. Fix: duplicates in the 409 body
  - [ ] S2: offline add-then-undo: the remove is rejected `item_not_found` and dropped, so the item stays on the list (the web mock hides it)
  - [ ] G3: proactive attention budget and cooldown hold only within one tick (daemon + `once` race). Fix: tick lock
  - [ ] G4: the Meshy daily cap is enforced on plan estimates, not on credits charged (money). Fix: per-step guard, before LUCINA_MESHY=true
  - [ ] Secrets redactor overlap: a secret that is part of, or ends, a placeholder survives or is re-created (`[secret:X]`), and a second pass rewrites it. Fix: reject such values and names (the proof's wf condition)

### Notes (owner, 2026-10-22)
- [x] Notes LIVE (main 326fbf2): server /v1/notes, MCP note_save/append/get/search, Lucina LUCINA_NOTES, web + iOS (ios-app 0986f57); craftit-meeting briefing restored; ask_lucina limit 12000

### Roadmap modules (`docs/ROADMAP.md`)
- [ ] Calendar · Languages · Memory exercises · Purchases (always confirmed)

### Infra follow-ups
- [ ] p2 ingest-api still calls 10.0.0.170:8789
- [ ] Hermes drop-ins hold an inline token
- [ ] Honcho memory gap
- [ ] Unmerged branches: Hermes v0.22.3 port, `fix/cursor-api-key-env`
