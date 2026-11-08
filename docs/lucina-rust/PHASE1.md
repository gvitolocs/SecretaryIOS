# Lucina Rust port, phase 1: the Secretary API

Status 2026-10-22: the Rust Lucina runs side by side with the Node Lucina (shadow), parity is green against the Node
tree at `fee2ccf` (`secretary-live`), and the cutover is one command away. Node (`lucina-nez`, :8788) still serves
secretary-server. Companion to `PORT_PLAN.md` §5 and `INVENTORY.md` §4.2.

## 1. Parity: Node `secretary-live` vs Rust `lucina-rs`

| Surface | Node (live) | Rust | Status |
|---|---|---|---|
| Envelope, auth (constant time), 404/405 + `Allow`, 64 KiB, `Cache-Control: no-store` | yes | yes | same (harness) |
| `GET /health` | yes | yes | same; `memoryProvider` is hardcoded `honcho` in Rust |
| `POST /chat` validation (12 000 chars after the MCP prefix + 100) | yes | yes | same |
| Chat pipeline: tier-0 regex classifier, DeepSeek tier-1 on recent turns, context policy (Honcho only for recall/tool turns, budgets) | yes | ported (`lucina-intent`) | same: 232/232 phrases, 16/16 real messages |
| JSON-action planner, prompts verbatim, chat path with capability hints, direct plans, misrouted-intent guard | yes | yes | same (harness: identical DeepSeek prompt-kind sequences) |
| MCP turns: prefix stripping, caller from prefix or `mcp-<caller>` session, timeline `meta: {via, caller}`, note in prompts, Honcho label | yes | yes | same |
| MCP guard: allowlisted actions/grocery intents, `needs_confirmation` + `blockedAction` (social post → `poko_news_scout_retry`, list removal, calendar create) | yes | yes | same |
| Grocery it/en/da intents, store context, list add/remove/query, finished/consumed (pantry), spend, pantry | yes | yes | same |
| Grocery offers / purchase prices (ShelfAtlas, chain JSON, eTilbudsavis, Salling) | yes | no | **open** (phase 6): Rust answers "Live grocery catalog lookup is not configured yet." / prices only from history |
| Shopping v1 + v2 routes (ops, revisions, conflicts, default list) | yes | yes | same |
| Catalog (Node `f1906d6`): normalise() fold, names before aliases, `{it,en,da,all}` aliases, catalogId/section/icon resolved on read for rows without a catalog id (v2 state and legacy `/shopping`) | yes | yes | same |
| Capability self-description (Node `fee2ccf`): "Runtime capabilities" block from the flags at the head of the operator brief and for "cosa sai fare?", DeepSeek-only runtime line, reworded skills line, reminder hint without "Telegram" | yes | yes | same: identical 26-line block; `LUCINA_SYSTEM_TELEGRAM`/`_DISCORD` come from the Node container |
| Rich replies, Lucina message format v1 (`LUCINA_RICH_REPLIES`, Node `fee2ccf`) | default off, not enabled anywhere | no | **open**: port `lucina-message-format.js` + `lucina-rich-reply.js` (~1 000 LOC) before anyone turns the flag on |
| Jobs `GET /jobs/:id` interrupted-race fix (Node `fee2ccf`) | yes | yes | same (decided under the jobs lock) |
| Reminders create/list/cancel, chat reminders, `already_sent`, `due_in_past` | yes | yes | same; Rust forwards writes to Node (phase 1), verified |
| Agenda briefing (reminders + calendar events, 15-min cache, fingerprint) | yes | yes | same |
| Notes bridge (save, append, multi-part, read with LLM, list, named-note context, Honcho summary) | yes | yes | same |
| Calendar bridge (it/en/da questions, create with yes/no, MCP refusal, briefing events, context) | yes | yes | same; Node throws on "sæt tandlæge i kalenderen i morgen kl. 9.30" (reads 9.30 as a date; HTTP 500), Rust asks again |
| Outfit/hair intents (`LUCINA_AVATAR_OUTFIT`, off on live Node) | yes | yes | same |
| Discord presence line + presence questions (online minutes / unknown) | yes | yes | same |
| App notify (`/v1/notify`) | reminder delivery, check-ins (not Secretary API) | client in `bridges::notify` | **phase 2** (scheduler) |
| Jobs: `cursor_task`, `spreadsheet_update_task` on dsh-remote; blockers; `GET /jobs/:id`; interrupted jobs | yes | yes | same contract; `graph_task`, `social_post_task`, `generate_image` jobs fail with a clear reason in Rust (rclone, Poko tokens, ComfyUI stay in Node) |
| Timeline list, pages, SSE (`retry: 5000`, `: connected`, ping, replay ≤500, 429 on the 9th stream) | yes | yes | same; Rust serves Node-written lines byte for byte |
| Shared timeline writer | O_APPEND | O_APPEND + flock + ids after the newest line; SSE publishes other writers' lines once | 300 + 300 concurrent appends: file whole |
| Device bridge `/device/*`, `GET /presence` | no (404) | yes | Rust only (plan Q12) |

## 2. Checks (all on nezopt)

- `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` for the `lucina-*` crates.
- `target/release/lucina parity-parsers` — the deterministic layer vs the live Node parsers (read only, `docker exec`).
- `tools/parity/run-harness.sh` — Node reference (live image and src, every bot token empty, Telegram/Discord/DeepSeek
  pinned to 127.0.0.1, Harness, Bot Hospital and Honcho off, preflight-checked) vs Rust, each on its own copy of the
  data and of the grocery database, fake DeepSeek, fake secretary-server; 106 steps (`tools/parity/http-cases.json`),
  reminder forwarding, concurrent writers. Ends with `PARITY GREEN`. The reference mounts the Node tree's `src`, so it
  tests the tree HEAD, which `lucina-nez` runs after its next restart.
- `lucina capability-block` (with the generated env) vs Node's `lucinaCapabilityBlock()` in the container: identical.
- `tools/parity/run-shadow-replay.sh [N]` — the last N real app/web turns from the live timeline (read only) against a
  Rust sandbox (real DeepSeek, Honcho read only, real notes/calendar read through, writes to copies). `SHOW_TEXT=1`
  prints the texts.

## 3. Deployment (shadow)

`deploy/lucina-rs/install.sh`: binary `~/.local/bin/lucina-rs` (the presence watcher uses the same path and keeps its
old binary until its own restart), `~/.config/lucina-rs/node.env` generated from the running `lucina-nez`
(`lucina-rs env-from-node`, values never printed), `mode.env` = `shadow.env`, user units `lucina-rs.service`
(health gate on start) and `lucina-rs-health.timer` (restart when `/health` stops answering).
Since the Node retirement (2026-10-27, `RETIREMENT.md`) `install.sh` no longer touches `node.env` or `lucina-nez`:
it needs `lucina.env` and `mode.env` in place, always builds the release binary, installs the units and restarts
`lucina-rs` (2026-10-28).

Shadow: listens on 127.0.0.1:8790 and 100.107.164.33:8790, real DeepSeek, Honcho read only, reminders forwarded to
Node, scheduler/Telegram/Discord/Harness jobs off, its own timeline `data/lucina-rs/timeline-shadow.jsonl`.
secretary-server does not call it; do not POST writes to it (they are real: reminders go to Node, the grocery
database is the live one).

Firewall: ufw on nezopt admits `tailscale0` as a whole (8788 and 8790 both answer p1 and the other tailnet devices;
8790 is not on the LAN). For p1 only:
`sudo ufw allow in on tailscale0 from 100.115.191.80 to any port 8790 proto tcp` and deny the rest of tailscale0
for 8790 (needs sudo on nezopt).

## 4. Cutover and rollback

- Switch: `deploy/lucina-rs/cutover.sh` (from the Mac). Preflight; live timeline handed to nez (Node, root, keeps
  writing Telegram/Discord/reminder lines); `mode.env` = `cutover.env` (live timeline, rotation at 40 MiB so Node
  never rotates, Honcho read-write, Harness jobs on, reminders still forwarded); p1 `SECRETARY_LUCINA_URL` →
  `http://100.107.164.33:8790`, restart secretary-server, print its Lucina probe.
- Rollback: `deploy/lucina-rs/rollback.sh`. p1 back to :8788; lucina-rs back to shadow. Rust-written timeline lines,
  job records and Honcho messages stay readable by Node.
- Later (not needed for the switch): give Node a `SECRETARY_API_TOKEN` only Rust knows at Node's next planned restart.

## 5. Open items and risks

1. Grocery offers/catalog prices are not ported (phase 6). Offer questions and new purchases answer differently.
2. `graph_task`, `social_post_task`, `generate_image` jobs fail in Rust (broken or Poko-owned in Node too).
3. After the cutover two processes append to `timeline.jsonl`. Lines stay whole (single O_APPEND writes, tested), but
   Node does not take the lock; a Node line written in the same millisecond as a Rust line can carry a smaller id (SSE
   still delivers it; pages are by id, as in Node).
4. The recent-turn buffer is per runtime (`data/lucina-rs/recent-chat.json`, seeded from Node's on start); a rollback
   loses the turns made on Rust from Node's short-term context (Honcho has them).
5. In-memory state does not survive a restart in either runtime: `clientMessageId` replays, notes multi-part and
   calendar confirmations (10–30 min), briefing cache.
6. Real-turn evidence is thin: 16 app/web turns so far (5 MCP). All differences in the replay came from Node changing
   after the turn (notes bridge, grocery query fix on 2026-10-22); today's Node parsers agree with Rust on all 16.
7. Node keeps moving (two Lucina commits landed during this phase). Re-run `parity-parsers` and the harness after
   every Node change; port before cutover. Node reads its prompt docs from the image (`/app/docs` is not mounted):
   its operator brief is older than the tree's, which Rust uses.
