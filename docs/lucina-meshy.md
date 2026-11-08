# Lucina × Meshy AI (3D models)

Lucina makes 3D models with [Meshy](https://www.meshy.ai): from text or a photo, then remesh,
retexture, rig, animate with Meshy's clip library and export (GLB, FBX, USDZ, OBJ, STL, 3MF).
She shows the plan and its credit cost first and spends nothing until Giuseppe says yes, in the
app or with the Telegram button. Jobs run in the background; the finished model lands in the
private gallery, renders inline in the chat and arrives on Telegram as a short turntable video.

Inspired by the Meshy plugin for Hermes Agent (NousResearch/hermes-official-plugins `meshy/`,
MIT, © 2025 Nous Research; it wraps Meshy's own MCP server `@meshy-ai/meshy-mcp-server`, MIT,
© 2026 Meshy AI). No code was copied; the behaviour it describes (plan and cost before credits,
inline viewer, gallery with prompts, preview video on messaging apps) is reimplemented here.

## REST, not Meshy's MCP server

Lucina calls Meshy's REST API directly from Rust (`crates/lucina-meshy`):

- **Fewer moving parts.** Meshy publishes its MCP server only as a local stdio npm package
  (no hosted endpoint). Using it would mean a Node child process per Lucina, `npx` fetching a
  pinned package at start-up, and a second process holding the API key.
- **The gate comes first.** Every credit-spending request is built by Lucina from a confirmed
  plan, after the budget check. With MCP the model would call spending tools directly and the
  approval would have to wrap someone else's tool surface (28 tools, ~19K tokens of schemas).
- **Small surface.** Lucina needs eight task endpoints, status reads, the animation library and
  the balance: about 300 lines of client. Meshy's OpenAPI spec (docs.meshy.ai/openapi.yaml) is
  the contract.
- What MCP would add that Lucina does not need: local file paths, saving downloads to arbitrary
  folders, slicer detection.

## Meshy's test-mode key is gone

The brief asked for Meshy's published test-mode key. Meshy **retired it on 2026-09-16**
(changelog: "The shared test-mode API key has been retired; requests that still send it now
return 401 Unauthorized"). Checked on 2026-10-22: the old key answers `401 {"message":"Invalid
API key"}`. So all tests use **`meshy-mock`** (`crates/lucina-meshy/src/mock.rs`), a local
stand-in that follows the OpenAPI contract: `202 {"result": id}` on create, PENDING →
IN_PROGRESS → SUCCEEDED over a few polls with `Retry-After`, the error shapes (400/401/402/404,
429 rate and `NoMorePendingTasks`, 5xx), `consumed_credits` from the price table, refunds for
FAILED tasks, a balance, the animation library. Like the old test mode it returns sample
results: GLB/FBX/USDZ/OBJ/STL files made with Blender by `fixtures/make_fixtures.py` (a chest,
and a rigged knight with walk and wave clips). Prompt markers simulate failures (`[fail]`,
`[never]`, `[402]`, `[429]`, `[500]`). **No real credits were ever spent.**

## How it fits together

```
app chat / Telegram ──► Lucina (Rust)                         Meshy REST API
                         ├─ json planner: meshy turn ─┐        (or meshy-mock)
                         ├─ tools mode: `meshy` tool ─┤ plan ──► nothing spent
                         │   pending action (10 min) ◄┘
                         ├─ "sì" / ✅ button ──► MeshyService::start
                         │      budget reserve → job (secretary-jobs.json)
                         │      preview → refine → rig → animate (poll, never re-create)
                         │      download GLB + thumb + formats (Meshy asset hosts only)
                         │      turntable MP4 (headless Chrome, SwiftShader = CPU)
                         ├─► secretary-server /v1/models (gallery, service token)
                         ├─► timeline item with a v1 `model3d` block
                         ├─► /v1/notify (deduplicated with the timeline ingester)
                         └─► Node bridge (LUCINA_MESHY=true) ──► Telegram plan buttons, sendVideo
```

- `crates/lucina-meshy`: `api` (client, error mapping, asset-host allowlist), `key` (API key
  trait), `plan` + `pricing` + `anim` (plans, prices, the clip library with in-place variants),
  `budget` (ledger), `service` (jobs), `gallery`, `preview`, `telegram` (bridge client), `mock`.
- `crates/lucina-tools/src/tools/meshy.rs`: the `meshy` tool (registered when `LUCINA_MESHY=true`).
- `crates/lucina-server/src/chat/meshy_turn.rs`: the json-planner hook (the live default), the
  pending yes/no, the confirm buttons, `POST /api/secretary/meshy/confirm`.
- `crates/lucina-store/src/model3d.rs`: the `model3d` block validator; Meta gets `format`,
  `blocks`, `actions`.
- `tools/meshy-preview/render-turntable.mjs`: the CPU turntable renderer.
- secretary-server (branch `models-gallery`): `/v1/models`, the web viewer and gallery
  (`web/src/components/Model3dViewer.tsx`, `views/Models.tsx`).
- iOS (branch `ios-models`, from `ios-app`): `Model3DBlock.swift` (card, AR Quick Look of the
  USDZ, link to the web viewer).
- Node Lucina (branch `secretary-live`): `src/meshy-bridge.js`.

## The `meshy` tool

| Action | Spends | What |
|---|---|---|
| `plan` | no | Steps, clip ids (in-place variants), formats, total credits, today's budget |
| `text_to_3d` | yes | preview (20; low-poly 5) → refine/texture (10, 8K 15) → optional rig (5) → clips (3 each) |
| `image_to_3d` | yes | 1 image (or 2-4 views): 30 textured, 20 mesh only, 35 at 8K; `image_url` is http(s), a `data:` URI or `upload:<file>` from `LUCINA_MESHY_UPLOADS_DIR` |
| `remesh` | yes | 5: target poly count, quad/triangle |
| `texture` | yes | 10 (8K 15): new textures from a prompt |
| `rig` | yes | 5: humanoid rig, free Walking and Running clips |
| `animate` | yes | 3 per clip (1-10), rigs first when needed |
| `export` | yes | 1 per conversion (free when Meshy already made the format) |
| `status` | no | a job's status and progress |
| `list` | no | the gallery |

Post-processing actions take a gallery `model_id`; Lucina finds the Meshy task ids in the
gallery entry. Animation names resolve against a snapshot of Meshy's 678-clip library
(`data/animations.tsv`) with it/en/da aliases ("cammina" → Casual Walk, in place #613).

## Spending rules

1. **Plan first.** The tool description and the prompt make Lucina call `plan` and show it
   ("Procedo?"). A spending call is never run in the turn that asks for it: it becomes a
   pending action (10 minutes, one per session).
2. **Explicit yes.** "sì"/"yes"/… in the app (the v1 `confirm` buttons send "sì"), or the ✅
   button on Telegram (`POST /api/secretary/meshy/confirm`, owner chat only). It runs at
   most once; a second tap answers "already used".
3. **Budget.** `LUCINA_MESHY_MAX_PLAN_CREDITS` (default 60) per plan and
   `LUCINA_MESHY_DAILY_CREDITS` (default 60) per day (Europe/Rome), both clamped to the
   compiled-in hard cap of 300 per day. A plan over a limit is refused before anyone is asked.
   Credits are reserved when a job starts and settled with Meshy's `consumed_credits`
   (`data/lucina-rs/meshy-ledger.json`).
4. **MCP callers** (ChatGPT, Grok, Claude via Lucina MCP) may only `plan` and `list`. A
   confirmation-gated tool is refused for them in the agent loop (no pending action), and
   their "yes" never answers the owner's pending action.
5. **Never twice.** A create request is never resent automatically (a timeout or 5xx may
   still have made a task). Polls retry; a step that polls past
   `LUCINA_MESHY_STEP_TIMEOUT_MS` (20 min) fails with the task id kept.

## API key

`MESHY_API_KEY_FILE`: a file with the `msy_…` key, mode 0600 (anything readable by group or
others is refused). It is read per call (rotation needs no restart), held as a `Secret` that
prints `msy_…<last 4>`, and sent only as `Authorization: Bearer` to `MESHY_API_BASE`. Result
files are downloaded from Meshy's asset hosts (`https://*.meshy.ai`) without the key.

With the secrets skill (`crates/lucina-secrets`, `LUCINA_SECRETS_CONFIG`), map the setting
`meshy.api_key` in `secrets.toml` to a reference such as
`secret://lucina/prod/lucina#MESHY_API_KEY`: the server then uses `SecretRefKey` (resolved
per call through Infisical, audited, never written to disk, redacted everywhere) instead of
the file. Without that mapping it reads `MESHY_API_KEY_FILE`.

## Owner steps (Meshy account and key)

1. Sign up at https://www.meshy.ai. API access needs a paid plan (Pro or above; the free tier
   cannot create API tasks since March 2025).
2. Developer Platform → **API Keys** (https://www.meshy.ai/developers/keys) → *Create API key*,
   name it `lucina-nezopt`, copy the `msy_…` value. Optionally restrict it to nezopt's public
   IP (refused calls answer `403 ip_not_authorized` and cost nothing).
3. Store it without it ever touching chat or shell history, either
   - in Infisical as `MESHY_API_KEY` in Lucina's folder, and in `secrets.toml`:
     `meshy.api_key = "secret://lucina/prod/lucina#MESHY_API_KEY"` (preferred), or
   - on nezopt: `install -m 600 /dev/null ~/.config/lucina-rs/meshy.key && nano ~/.config/lucina-rs/meshy.key`
     (paste, save; `stat -c %a` must print `600`) and `MESHY_API_KEY_FILE` below.
4. Add to `~/.config/lucina-rs/mode.env` (Rust Lucina):
   ```
   LUCINA_MESHY=true
   MESHY_API_KEY_FILE=/home/nez/.config/lucina-rs/meshy.key
   LUCINA_MESHY_DAILY_CREDITS=60
   LUCINA_MESHY_MAX_PLAN_CREDITS=60
   LUCINA_MESHY_TELEGRAM_URL=http://127.0.0.1:8796
   LUCINA_MESHY_TELEGRAM_TOKEN_FILE=/home/nez/.config/lucina-rs/meshy-bridge.token
   LUCINA_MESHY_PREVIEW_SCRIPT=/home/nez/Projects/SecretaryIOS/tools/meshy-preview/render-turntable.mjs
   ```
   (`LUCINA_SECRETARY_SERVER_URL` and `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` are already set.)
5. Node Lucina (`lucina-nez`): `LUCINA_MESHY=true`, `LUCINA_MESHY_BRIDGE_TOKEN_FILE` (the same
   bridge token, `openssl rand -hex 24`, mode 0600), `LUCINA_RS_URL=http://127.0.0.1:8790`;
   the container must reach and expose 127.0.0.1:8796 (host network or a published port).
6. Deploy secretary-server with `/v1/models` (branch `models-gallery`) and Caddy's body limit
   for `PUT /v1/models/*/files/*` at 200 MB if it sits in front.
7. Watch the balance at meshy.ai; Lucina's `/health` shows today's spend (`meshy.budget`).

## Typical costs (Meshy prices of 2026-10-22)

| Request | Steps | Credits |
|---|---|---|
| Low-poly prop for Godot, untextured | preview meshy-t2.5 | 5 |
| Low-poly prop, textured | preview 5 + texture 10 | 15 |
| Textured prop ("a treasure chest") | preview 20 + texture 10 | 30 |
| Photo of a mug → printable model (STL) | image to 3D textured 30 (STL made at generation) | 30 |
| Photo → mesh only, for printing | image to 3D 20 | 20 |
| Character with the free walk and run | preview 20 + texture 10 + rig 5 | 35 |
| Character + 2 clips (the E2E knight) | 20 + 10 + 5 + 2×3 | 41 |
| Character + 5 clips | 20 + 10 + 5 + 15 | 50 |
| 3 more clips on a rigged gallery model | 3×3 | 9 |
| Remesh to 5k quads / new textures / extra format | 5 / 10 / 1 | |

With the defaults (60 per plan, 60 per day) one textured character with up to 8 clips fits a
day; a 9-clip plan (62) is refused until the limits are raised.

## Rendering

- **Web**: the `model3d` block is a card (thumbnail, "View in 3D"); a click loads three.js
  and the GLB through lucina-stage's `loadModel` (meshopt, no Draco) with the session cookie:
  orbit, zoom, auto-turn, clip picker, play/pause, downloads, AR Quick Look link for the USDZ.
  `/models` is the gallery (prompt, clips, formats, delete).
- **iOS**: a card with the thumbnail, "View in AR" (USDZ in AR Quick Look) and "Open in 3D"
  (the web viewer).
- **Telegram/Discord**: `render-turntable.mjs`, headless Chrome on **SwiftShader** (the page
  refuses to draw unless the WebGL renderer is SwiftShader, and Rust rejects any other
  report), 1280×720, 30 fps, 5 s, H.264 (libx264, never NVENC), ≤ 8 MB. An animated model
  plays its first clip with a half turn. About 45-60 s of CPU per video on nezopt.

## Avatar pipeline (design note, no credits spent)

Meshy's rig and clip library could feed Lucina's own clip library (`~/scratch/lucina-anim`):

1. **Never upload Lucina's model.** Her PMX conversion is licensed for private use only.
   Instead rig a neutral humanoid mannequin once (Meshy text-to-3D "neutral A-pose
   mannequin", or any CC0 humanoid GLB through `model_url`): rig 5 credits.
2. **Animate the mannequin** with the clips wanted (in-place variants for the stage: idles
   #0/#11/#12, Casual Walk #613, Wave One Hand #290, Formal Bow #41, Talk with Hands Open
   #313, Victory Cheer #59, Agree Gesture #25): 3 credits each, up to 10 per request in one
   GLB, `post_process: extract_armature` for an armature-only FBX.
3. **Retarget with the existing pipeline**: a new source profile
   `tools/profiles/sources/meshy.json` mapping Meshy's skeleton (read from the first
   downloaded armature) to the canonical joints, then `./build.sh b recipes-meshy.json`:
   loop closure, foot planting, arm clearance against Lucina's mesh, packed into
   `lucina-anims.glb` with `clips.json` (source: Meshy, action id, licence per Meshy's terms
   for generated assets).
4. Cost for a 10-clip starter set: 5 + 30 = 35 credits, once. `text_to_motion` (prime 10,
   swift 3) could make custom clips from a sentence the same way.

## Testing

- `cargo test --workspace` (lucina-meshy: plan/cost, budget, error mapping, key file rules,
  the pipeline against the mock: success, timeout without re-creating, FAILED refund, 402,
  5xx not resent, 429 queue full; lucina-tools: the confirmation gate, the cap, MCP;
  lucina-server: plan → buttons → "sì" → job in the json planner, Telegram confirm once, MCP).
- `meshy-mock --assets <fixtures>` plus `lucina fake-deepseek` for end-to-end runs; the
  transcript of 2026-10-22 is in the session report.

## Rich replies

`crate::rich::normalize_block` validates `model3d` blocks with `crate::model3d::normalize`
and the plain-text fallback uses `crate::model3d::plain_text`. A pending Meshy plan is a
pending confirmation for the composer (`action: "meshy"`, `[meshy plan awaiting
confirmation]`), so with `LUCINA_RICH_REPLIES=true` it gets the Sì / No / Modifica buttons;
without rich replies Lucina sends the v1 Sì / No buttons herself. The web branch `rich-chat`
renders `model3d` blocks with `Model3dViewer` (branch `models-gallery`).
