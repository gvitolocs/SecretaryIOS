# Voice memos

Lucina as a voice-memo app, like Apple's Voice Memos. The owner records in the Secretary app (iPhone, web), and
without doing anything else gets a transcript, Lucina's summary (*resoconto*), a PDF, a note in the Obsidian vault and
a notification.

```
iPhone / web ──HTTPS──► p1 Caddy /v1/memos* ──tailnet──► lucina-memo (nezopt :8795)
  record, queue,           forward_auth to                 store ─► worker: transcript ─► summary ─► PDF ─► Obsidian ─► notify
  resumable upload         secretary-server (/v1/avatar)   ~/Projects/Lucina/data/memos
```

## Pieces

| Piece | Where |
| --- | --- |
| Service, pipeline, CLI | `crates/lucina-memo` (binary `lucina-memo`), user unit `lucina-memo.service`, `deploy/lucina-memo/` |
| Config | `~/.config/lucina-memo/memo.env` (template `deploy/lucina-memo/memo.env.example`) plus Lucina's `~/.config/lucina-rs/node.env` (DeepSeek key file, secretary-server URL and service token, Telegram chat) |
| Gateway | p1 `/etc/caddy/Caddyfile`, block `@memos` (same gate as `/v1/media`); key `/etc/caddy/memo-gateway.key` = nezopt `~/.config/lucina-memo/gateway.token` |
| iOS | `ios/Secretary/Features/Memos/*`, Live Activity `ios/Extensions/SecretaryWidgets`, `ios/Secretary/Shared/MemoRecordingActivity.swift` |
| Web | `web/src/views/Memos.tsx`, `web/src/lib/memoRecorder.ts`, `web/src/api/memos.ts` |
| Lucina's tool | `crates/lucina-tools/src/tools/memos.rs` (`memos`: list, search, read, resummarize) |
| Protocol model | `crates/lucina-memo/tests/upload_model.rs` (Stateright) |

## Recording

- **iPhone**: the Plugins tab → Voice memos (or Siri/Shortcuts/Action button: "Record a memo in Secretary").
  ADTS AAC (`.aac`) mono 44.1 kHz 96 kbit/s, so a recording cut off when iOS kills the app still decodes; the next
  launch recovers and queues it (`MemoUploadQueue.recoverOrphans`). The server rewraps it as `.m4a` (step 0). live level meter, pause/resume, keeps recording with the screen locked
  (`UIBackgroundModes: audio`); a call or Siri pauses it and it resumes when iOS allows.
  The **Live Activity** shows the timer on the Lock Screen and in the Dynamic Island with Pause/Resume and Done
  buttons (`LiveActivityIntent`s, run in the app's process, which is alive while recording).
- **Web**: `/memos`. MediaRecorder (AAC/MP4 on Safari, Opus/WebM elsewhere), waveform from an AnalyserNode.
- **Queue**: recordings are saved on the device first (iOS `Application Support/Memos/` + `queue.json`; web
  IndexedDB `secretary-memos`) and uploaded one at a time. The queue survives app kills, restarts and offline
  periods; it runs after a recording, when the network comes back, in the foreground and in background refresh.
- **On-device transcript (iPhone, memos ≤ 10 min)**: Apple `SpeechTranscriber` (iOS 26, on-device), in the app's
  language. Shown at once, sent to the server (`PUT …/device-transcript`), used while the GPU is busy, then replaced.

## Upload protocol

1. `POST /v1/memos {clientId, title?, recordedAt, timezone, durationSec, mime, sizeBytes, source}` → `{memo}`;
   201 when created, 200 with the same memo for a repeated `clientId`.
2. `PUT /v1/memos/{id}/audio` with header `Upload-Offset: <receivedBytes>`, body = the next bytes (≤ 16 MiB;
   apps send 4 MiB). → `{receivedBytes, sizeBytes}`. A wrong offset → 409 `offset_mismatch` with the server's
   `receivedBytes` (also in `Upload-Offset`). A body cut short keeps the bytes that arrived.
3. `POST /v1/memos/{id}/complete {sha256}` → queued. Short file → 409 with the offset; hash mismatch → 409, the
   upload restarts at 0.

The Stateright model checks, for every interleaving with message loss, duplication, reordering, cut-off chunks and a
client crash: the server's bytes are always a prefix of the file, one memo per recording, queued only when whole, and
"done" on the client implies queued on the server.

Other routes: `GET /v1/memos?limit&before`, `GET /v1/memos/{id}` (`{memo, summary, pdf, url}`),
`GET …/audio` (Range, ETag), `GET …/pdf`, `GET …/transcript`, `PUT …/device-transcript`,
`PATCH /v1/memos/{id} {title}` (the owner's title wins everywhere), `DELETE` (moves to `trash/`, never deletes),
`POST …/resummarize {instructions?}`, `POST …/reprocess {fromTranscript?}`,
`GET /v1/memos/search?q&from&to&days&limit` (dates `YYYY-MM-DD` in the owner's zone).
Auth: `X-Memo-Gateway` from Caddy (after secretary-server accepted the device), or `Authorization: Bearer <service.token>`
for local callers. Cookie writes need `X-Secretary-CSRF: 1`. `GET /health` is open.

## Pipeline (worker)

One memo at a time, oldest first; each step writes its output before recording it, so a restart continues.

1. **Transcript**: `ffmpeg` → 16 kHz WAV; whisper.cpp **large-v3 on the RX 7900 XTX** through the `gpu-lease`
   shim (`-l auto`, Silero VAD, `-mc 0 -sns`, silence hallucinations dropped). If the GPU is busy:
   the iPhone's transcript when there is one (memo continues now; the idle worker upgrades it to Whisper later
   and re-summarises without a second notification), otherwise the memo waits and retries every minute.
   No CPU Whisper model is installed (owner's choice, 2026-10-24).
2. **Summary**: Lucina's DeepSeek model (text only, never audio), JSON: title, overview, key points,
   decisions, action items (owner, due date resolved from the recording date), tags, with seconds into the audio.
   Long transcripts are summarised in parts and merged.
3. **PDF**: HTML printed by headless Chrome (summary page, then the timed transcript).
4. **Obsidian**: the canonical vault on the Raspberry Pi (`pi-cursor:/home/nes/Obsidian`), folder
   `Voice Memos/Lucina/`: `<YYYY-MM-DD HHmm> <title>.md` (frontmatter in the vault's Voice Memos style, embedded
   audio, summary callout, action items as checkboxes linking to transcript block ids `^sNNN`), attachments in
   `Attachments/`, index `00 Lucina memos.md`. rsync, never deletes. A note keeps its file name after the first publish.
5. **Notify**: app inbox (`/v1/notify`, route `/memos/<id>`) and Telegram from Lucina's bot.
6. **Media library**: `PUT 127.0.0.1:8830/internal/v1/external/voice-memo/{id}` (lucina-media; best effort).

Statuses: `uploading → queued → transcribing → summarizing → rendering → publishing → done` (or `failed` after 5
attempts; `reprocess` re-queues).

## Lucina

Tool `memos` (lucina-rs): `list`, `search` ("what did I say in yesterday's memo about X": `from`/`to` = that date
plus words; without words every memo of the range with its summary), `read` (summary + timed transcript),
`resummarize`. Action items become tasks through the `tasks` tool, or with + in the iPhone's memo screen.

## Verification

`docs/verification.md` §7 (branch main): the Stateright model `verification/models/src/memos.rs`, its TLA+ twin
`formal/memos/Memos.tla`, and the Verus kernel `verification/verus/memo_kernel.rs` (K1–K7) whose function bodies are
`crates/lucina-memo/src/kernel.rs` (differential test `tests/verus_kernel.rs`). Bugs found and fixed there: B1 a
second vault note after a crash and a rename, B2 Whisper replaced by the iPhone transcript on a reprocess, B3 Telegram
sent twice after a crash, B4 a recording lost when iOS killed the app.

## Operations

```bash
systemctl --user status lucina-memo
lucina-memo list                       # with memo.env loaded
lucina-memo import file.m4a --title "…" # through the local API, like an app
lucina-memo reprocess mm_… [--transcript]; lucina-memo resummarize mm_… --instructions "…"
deploy/lucina-memo/install.sh          # build, install, restart
```

Data: `~/Projects/Lucina/data/memos/memos/<id>/` (`meta.json`, `audio.<ext>`, `device-transcript.json`,
`transcript.json`, `summary.json`, `memo.html`, `memo.pdf`); deleted memos in `…/trash/`.
