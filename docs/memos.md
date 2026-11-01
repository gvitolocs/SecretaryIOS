# Voice memos

(Meetings recorded on the Mac run in the same service: `docs/meetings.md`.)

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
| Config | `~/.config/lucina-memo/memo.env` (template `deploy/lucina-memo/memo.env.example`) plus Lucina's `~/.config/lucina-rs/lucina.env` (DeepSeek key file, secretary-server URL and service token, Telegram chat) |
| Gateway | p1 `/etc/caddy/Caddyfile`, block `@memos` (same gate as `/v1/media`); key `/etc/caddy/memo-gateway.key` = nezopt `~/.config/lucina-memo/gateway.token` |
| iOS | `ios/Secretary/Features/Memos/*`, Live Activity `ios/Extensions/SecretaryWidgets`, `ios/Secretary/Shared/MemoRecordingActivity.swift` |
| Web | `web/src/views/Memos.tsx`, `web/src/lib/memoRecorder.ts`, `web/src/api/memos.ts` |
| Lucina's tool | `crates/lucina-tools/src/tools/memos.rs` (`memos`: list, search, read, resummarize) |
| Protocol model | `crates/lucina-memo/tests/upload_model.rs` (Stateright) |

## Recording

- **iPhone**: the Plugins tab → Voice memos, or one tap without opening the app: the Lock Screen / Control Center
  control, the Voice memo widget, the Action button or Siri ("Registra memo con Lucina"). Those run
  `StartMemoRecordingIntent` (an `AudioRecordingIntent` and `LiveActivityIntent`) in the app's process, launched in
  the background with only the recorder (`MemoRecordingCenter`; AppModel waits for a window or the stop).
  ADTS AAC (`.aac`) 48 kHz, stereo from the built-in mics where they have a stereo pattern, 128 kbit/s per channel, so a recording cut off when iOS kills the app still decodes; the next
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

1. `POST /v1/memos {clientId, title?, recordedAt, endedAt?, timezone, durationSec, mime, sizeBytes, source,
   location?: {lat, lon, accuracyM?}}` → `{memo}` (`endedAt` = wall-clock end with pauses, ≤ 24 h after the start;
   `location` = where the recording started, only when the app already has location access);
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
`GET /v1/memos/search?q&from&to&days&limit` (dates `YYYY-MM-DD` in the owner's zone; words also match the linked
event's title, course, place, attendees and calendar), `GET …/events` (`{current, eventSource, candidates:
[{event, overlapMs, locMatch}]}` for the recording ± 2 h, best first; `stale: true` when using the cache after a live failure; 502 only when the live query fails and the cache has no events in the window),
`PUT …/event {eventId | null}` (the owner's choice, `null` = no event; re-summarises a finished memo; 409 while a
summary runs; 404 for an event not around the recording).
`POST /v1/memos/stt[?lang=auto|it|en|da]` (speech-to-text for one short clip, no memo is made; used by
secretary-server's `POST /v1/voice` for the LucinaVoice speaker): body `audio/wav` (16 kHz mono 16-bit) or
`audio/x-s16le;rate=16000`, at most 2 MiB (413); another type or WAV format 415, under 0.3 s of audio 422
`no_speech`, unknown query parameter or `lang` 400. It runs on the prompter's whisper-server (small model, GPU
lease first, CPU if none; `docs/prompter.md`) as a session that is always released. `200 {text, language, ms, mode,
corrections, ambiguous}` (`language` it/en/da, `mode` gpu/cpu; silence is `text: ""`; `corrections` `[{from, to}]` and
`ambiguous` `[{heard, candidates}]` are the Voice Learn result below, `[]` when there is none), 503 `stt_unavailable`
when the engine does not start, 502 `stt_failed` when it fails on the clip.
Auth: `X-Memo-Gateway` from Caddy (after secretary-server accepted the device), or `Authorization: Bearer <service.token>`
for local callers. Cookie writes need `X-Secretary-CSRF: 1`. `GET /health` is open.

## Voice Learn (the taught vocabulary)

`docs/lucinavoice-protocol.md` section 8. Lucina owns the vocabulary (a JSON file written by lucina-rs,
`LUCINA_VOICE_LEARN_FILE`, default `$HOME/Projects/Lucina/data/voice-learn.json`); this service only reads it, through
the `lucina-vocab` crate, and re-reads it when the file changes (one `stat` per transcription). A missing or empty
file changes nothing. Every transcription path here uses it ([`src/vocab.rs`](../crates/lucina-memo/src/vocab.rs)):

- **Bias:** the words go into Whisper's initial prompt, newest first, after the caller's own prompt ("Lucina" for
  `/v1/memos/stt`, the app's terms for the prompter, a video's title for a video report). whisper.cpp keeps the end
  of an over-long prompt, so the vocabulary goes last; the whole prompt stays under about 500 characters.
  `whisper-cli` is run with `-mc 0` (no text context), which makes whisper.cpp drop `--prompt` as well (measured
  2026-11-01: a vocabulary prompt changed nothing, and neither did the title prompt of video reports). With a prompt
  the worker therefore passes `--carry-initial-prompt` and an `-mc` just big enough for the prompt (at most 224).
  `soundsLike` is not in the prompt (it would steer Whisper toward the mishearing); it is used by the correction.
- **Post-correction:** after Whisper, a word or span of up to four words that is close to an entry's word or
  `soundsLike` becomes the canonical word. Phonetic key (diacritics, å/ø/æ, aa, ph/f, w/v, y/i, ch/k, silent h,
  doubled letters, a Danish "vej" = "vai"; a lone h is kept but cheap to drop) plus a weighted Damerau distance, a
  threshold that grows with the length (a five-letter word tolerates one vowel, a real name one step further, such as
  "Horace" for "Horus", is only reported), never
  on words of three letters or fewer, never on common Italian/English/Danish words or on another entry's exact word.
  A match that is only borderline, or equally close to two entries, is not replaced: it is returned in `ambiguous`
  (the caller may ask Jev "which did they mean").
- **Paths:** `POST /v1/memos/stt` (voice turns) and the prompter's live engine (`Engine::transcribe`); the memo worker,
  the idle upgrade of an iPhone transcript, meetings, and every other caller of `transcribe::transcribe*` (lucina-rs
  uses these for chat voice notes and shared audio, and for video reports); and the iPhone's own recognizer text, which
  is corrected when it is stored (`PUT …/device-transcript`). Word timings follow: the words of a replaced span become one word.
- **Record:** `Transcript.corrections` (`[{from, to}]`, absent when empty) in `transcript.json` keeps what was changed.
- **Accuracy:** `cargo run -p lucina-memo --example vocab_bench` (Kokoro clips through the engine and the worker,
  without the vocabulary / correction only / prompt only / prompt + correction; temporary vocabulary file).

## Pipeline (worker)

One memo at a time, oldest first; each step writes its output before recording it, so a restart continues.

1. **Transcript**: `ffmpeg` → 16 kHz WAV; whisper.cpp **large-v3 on the RX 7900 XTX** through the `gpu-lease`
   shim (`-l auto`, Silero VAD, `-mc 0 -sns`, silence hallucinations dropped). If the GPU is busy:
   the iPhone's transcript when there is one (memo continues now; the idle worker upgrades it to Whisper later
   and re-summarises without a second notification), otherwise the memo waits and retries every minute.
   No CPU Whisper model is installed (owner's choice, 2026-10-24).
1a. **Speakers** (best effort, kernel K11): when the diarizer is available (`docs/meetings.md`: models, voice
   profiles), once per transcript: two or more voices give each transcript line a `speaker` (the owner's name, a
   named person's, or `Speaker N`) and `speakers: true`; the summary sees them. A new transcript (Whisper upgrade,
   reprocess) is labelled again. The memo API is unchanged.
1b. **Calendar event** (best effort, before the summary): secretary-server's `/v1/calendar/events` (Google, ICS,
   iPhone calendars; service token) for the recording ± 30 min. `kernel::pick_event` (K8): timed events covering
   half the recording or ≥ 10 min of it; the one at the device's location first (optional: `LUCINA_MEMO_GEOCODE_URL`
   = secretary-maps' local Photon, match within 400 m + accuracy), then the most overlap, the shorter event, the
   earlier one. Kind (`events::kind`): lecture (lecture/lezione/forelæsning/… or a university calendar), meeting
   (meeting/riunione/møde/sync/… or 2+ attendees), other. Series: the recurring master id, else the title without
   "Lecture 5"-style suffixes. Title = `<event> · <date>` (`title_source: event`) unless the owner named the memo
   (K10). The owner's choice on the memo screen is never replaced (K9). Calendar replies are merged into `<data_dir>/calendar-cache.json`, keyed by event id, with first/last seen times.
   Changed events replace their previous copy; missing events remain until their end is over 180 days old.
   A persistent generation advances only on additions or changes. The worker refreshes the calendar at startup
   and every 15 idle minutes for now −14 days through +30 days; candidate queries also merge their replies.
   Candidates always use this history, so a later phone report cannot erase a known event. A live failure is
   logged and the cache alone answers when it has events in the requested window.

   A memo without an event stays **link-pending** for 90 days after recording unless the owner chose an event
   or explicitly chose “no event”. Uploading memos are excluded. The pipeline continues with the generic summary
   and publishes normally. After the Whisper upgrade pass, the idle re-link job checks the oldest pending memo
   whose `link_checked_gen` is behind the cache generation, using cached events only (no network requests).
   A miss records the generation; an event learned later triggers another check. A match atomically re-checks
   the owner's choice, links and names the memo, and queues a **quiet re-summary**: summary, PDF and publication
   markers reset, revision increases, and `notified_at` stays so the owner receives no second notification.
2. **Summary**: Lucina's DeepSeek model (text only, never audio), JSON: title, overview, key points,
   decisions, action items (owner, due date resolved from the recording date), tags, with seconds into the audio.
   Long transcripts are summarised in parts and merged. Linked to a **lecture**: lecture notes, key concepts,
   definitions, questions to review, homework, connected to the overviews of the course's earlier memos. Linked to
   a **meeting**: minutes, decisions, action items with owners. The event (title, time, place, attendees) is context
   in the prompt.
3. **PDF**: HTML printed by headless Chrome (summary page, then the timed transcript).
4. **Obsidian**: the canonical vault on the Raspberry Pi (`pi-cursor:/home/nes/Obsidian`), folder
   `Voice Memos/Lucina/`: `<YYYY-MM-DD HHmm> <title>.md` (frontmatter in the vault's Voice Memos style, embedded
   audio, summary callout, action items as checkboxes linking to transcript block ids `^sNNN`), attachments in
   `Attachments/`, index `00 Lucina memos.md`. A note keeps its file name after the first publish. Linked memos go
   to `Courses/<course>/` or `Meetings/<meeting>/` with event frontmatter (`event`, `event_start`, `location`,
   `attendees`, `course`/`meeting`, `series`), a "Previous" section linking the series' earlier notes and a series
   index `<folder>/<course>.md`. When the event changes the note moves: the new path and every old one
   (`obsidian_old_notes`) are recorded before the push, the old copies deleted after it (rsync itself never deletes).
   The exact published Markdown is also saved atomically as `<memo dir>/note.md`; a series index written
   for that memo is saved as `series-index.md`. Existing memos acquire these on their next publish.
5. **Notify**: app inbox (`/v1/notify`, route `/memos/<id>`) and Telegram from Lucina's bot.
6. **Media library**: `PUT 127.0.0.1:8830/internal/v1/external/voice-memo/{id}` (lucina-media; best effort).

Statuses: `uploading → queued → transcribing → summarizing → rendering → publishing → done` (or `failed` after 5
attempts; `reprocess` re-queues).

## Lucina

Tool `memos` (lucina-rs): `list`, `search` ("what did I say in yesterday's memo about X": `from`/`to` = that date
plus words; without words every memo of the range with its summary; "last Tuesday's Algorithms lecture" = words
`Algorithms`, that Tuesday, matched on the linked event), `read` (summary + timed transcript),
`resummarize`. Action items become tasks through the `tasks` tool, or with + in the iPhone's memo screen.

## Verification

`docs/verification.md` §7 (branch main): the Stateright model `verification/models/src/memos.rs`, its TLA+ twin
`formal/memos/Memos.tla`, and the Verus kernel `verification/verus/memo_kernel.rs` (K1–K11) whose function bodies are
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


## Memo archives and snapshots

Set `LUCINA_MEMO_ARCHIVE_DIR=/home/nez/mnt/mybook/lucina-memos` in `memo.env` to enable the archive (unset = off).
`LUCINA_MEMO_ARCHIVE_REQUIRE_MOUNT` defaults to `/home/nez/mnt/mybook`: before writing, its device id must
differ from its parent's. An empty value disables this check, useful for a local test archive. An absent mount
is logged and retried; the worker never creates an archive on the bare mount directory.

After pipeline work, Whisper upgrades and re-linking, the idle worker archives one oldest done/failed memo
whose `archived_rev` differs from its revision. Every regular top-level file is copied to `<archive>/<recorded year>/<id>/`,
including original audio, device/server transcripts, summary, HTML, PDF, saved Markdown and metadata.
`work/`, temporary files and partial uploads are excluded. Each file is hashed with SHA-256: matching files
are skipped, changed files use a same-directory temporary file, fsync, rename and a verified re-read.
Sorted `SHA256SUMS` covers every retained archive file; files missing from the source are never deleted.
Success records `archived_rev` and `archived_at`. Three consecutive failures for a memo defer it for an hour
(the retry counters are in memory); other memos can still proceed. Trash is archived once at `<archive>/trash/<id>/`.

```bash
lucina-memo archive                 # pending done/failed memos and trash
lucina-memo archive --all --verify  # backfill all memos, including trash; verify before and after copying
```

Verification hashes archived files against `SHA256SUMS` and available source files, reports one line per memo
and totals (memos, copied/unchanged files, bytes, verified files, mismatches), and exits 1 on any mismatch/error.
Pre-copy verification detects corruption even if copying subsequently repairs it. Backfill saves the files already
on disk; it does not reconstruct old vault notes absent from the memo directory. The archive CLI needs no LLM key.

`deploy/lucina-memo/install.sh` installs `lucina-memo-backup` and enables `lucina-memo-backup.timer`.
Daily at 04:30 (`Persistent=true`), the service runs `archive --all --verify`, then creates
`/home/nez/mnt/mybook/backups/lucina-memos/YYYY-MM-DD/` with `rsync -a --link-dest=<yesterday>`.
Dated snapshots older than 60 days are removed. The job checks the mount before starting, uses `Nice=10` and
idle I/O priority, and reports failures through `pokoin-notify@%n.service`. This repository change does not
install units or start the timer until the installer is explicitly run.
