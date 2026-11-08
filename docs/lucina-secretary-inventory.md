# Lucina, Hermes, Secretary and Obsidian inventory (Phase 2)

Inspected on 2026-10-24 from the real repositories and hosts. This file names no secrets.

## Where things run

| Component | Host | Source | Notes |
|---|---|---|---|
| Secretary server (`secretary-server`, Rust/axum) | Oracle `p1`, :8800 behind Caddy at getsecretary.tech | `crates/secretary-server` (this repo) | Owner devices use Bearer tokens or a cookie plus `X-Secretary-CSRF`; Lucina uses a service token |
| Lucina MCP (`secretary-mcp`, Rust, rmcp 3.5.1) | `p1`, 127.0.0.1:8810, `/mcp` | `crates/secretary-mcp` | Calls secretary-server with its own device token. Scopes read/chat/write; OAuth 2.1 for ChatGPT |
| Secretary web | `p1` static files | `web/` (Solid + Vite) | Views: Chat, Notes, Links, Shopping, Tasks, Gym, Weight, Models, Desk |
| Secretary iOS | iPhone | `ios/` | Separate SwiftUI app |
| Lucina (Rust, `lucina-rs`) | `nezopt` :8790 | branch `lucina-rs`, worktree `.worktrees/SecretaryIOS/lucina` (`crates/lucina-*`) | Serves the Secretary chat API since the phase-1 cutover (2026-10-22) |
| Lucina (Node, `lucina-nez`) | `nezopt` | `~/Projects/Lucina` | Still runs Telegram, Discord and the reminder scheduler |
| Hermes | `nezopt` | `~/Projects/Hermes`, profiles in `Lucina/data/hermes-*-profile` | Agent runtime with bundled prompt skills |
| Honcho memory | `nezopt` 127.0.0.1:8000 | `/home/nez/services/honcho` | Workspace `hermes-peer1` |
| Obsidian vault | `nezopt` | `/home/nez/mnt/mybook/Obsidian` (moved from pi-home `/home/nes/Obsidian` on 2026-11-08; the Pi copy is a read-only backup) | See below |

## Lucina's tool and memory design (what Lucina Ink reuses)

- **Tools for outside clients:** `secretary-mcp` has typed tool specs (`tools.rs`), scopes (`scope.rs`) and calls
  the server through `backend.rs`. The `library_*` tools go here, so Claude Code, ChatGPT and Grok get them with
  the existing auth.
- **Lucina's own tools:** `crates/lucina-tools` on the `lucina-rs` branch. It does not exist on `main` yet. When
  that branch merges, the library tools should reuse the same server endpoints rather than add another store.
- **Long-term memory:** Honcho, through `crates/lucina-memory` (Rust) and `src/memory/honcho.js` (Node). Owner
  peer `giuseppe`, sessions `secretary_<id>`. Library data should not be poured into Honcho. Lucina stores short
  references there (book id, highlight id, location, time), and the library API stays the source of truth.
- **Notes:** `secretary-server/src/notes.rs`, one JSON file per note in `$SECRETARY_DATA_DIR/notes/`. Briefings
  from Lucina go there. Lucina finds them by slug through `note_get` and `notes_search`.
- **Notifications and background jobs:** `secretary-server/src/notify/` (APNs push, ingest, settings) and
  `proactive.rs`. Nothing yet runs on a schedule for books; weekly reading summaries would use this path.
- **Storage pattern:** JSON files written atomically with mode 0600 under `SECRETARY_DATA_DIR`, plus Postgres for
  grocery only. **The Library follows the JSON-file pattern**, so there is no new database server.
- **PDF and documents:** Node Lucina extracts PDF text with `pdftotext` and an image-vision fallback
  (`src/telegram-document.js`). There is no Rust PDF stack yet, and no EPUB or book handling anywhere before this
  work.

## Obsidian

**Status: partially implemented. A prompt skill is installed but not wired to the vault; there is no code
integration.**

- The vault exists on nezopt at `/home/nez/mnt/mybook/Obsidian` (on `pi-home` at `/home/nes/Obsidian` until 2026-11-08). When this inventory was made it held 1,324 Markdown files, about 253 MB, project folders using
  a `Permanent/`, `Events/`, `MOC/`, `Sources/`, `Templates/` layout. It has **no community plugins**
  (`.obsidian/` holds only core config) and no Obsidian app on the Mac (`obsidian.json` has no vaults).
- Hermes ships the generic `note-taking/obsidian` prompt skill in each profile. It expects `OBSIDIAN_VAULT_PATH`,
  which is unset on nezopt, and its default `~/Documents/Obsidian Vault` does not exist there. **Installed but not
  working** for Lucina.
- `docs/lucina-rust/PORT_PLAN.md` lists `note-taking/obsidian` as a P3 skill to port.
- No Secretary or Lucina Rust code reads the vault.

Plan: a first-party Rust adapter (`ink-obsidian`) reading an **approved root** on pi-home read-only, either over SSH
or from a read-only synced copy. Private folders are skipped by default. The owner approves which folders are
indexed and approves any note it writes.

## What this means for the vertical slice

Kindle USB → `lucina-ink` CLI on the Mac → `POST /v1/library/import` on secretary-server → Library view in the web
app → `library_*` tools in secretary-mcp. Nothing new runs on the Kindle for this slice.
