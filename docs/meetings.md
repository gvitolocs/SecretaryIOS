# Meetings

The owner records a meeting on his Mac (Secretary Code's ScreenCaptureKit recorder: Teams, Zoom, Discord, Meet, any
app or display). On Stop it uploads `mic.m4a` (his microphone alone), `system.m4a` (everyone else), optionally
`video.mp4`, and the metadata. Lucina transcribes each track, tells the voices apart, labels who is who and what each
line does (question, answer, decision, action item), writes a brief, files everything on the MyBook, puts a note in
Obsidian and her memory, and notifies him with the brief and a transcript link.

```
Mac recorder ──HTTPS──► p1 Caddy /v1/meetings* ──tailnet──► lucina-memo (nezopt :8795)
  meeting.json,           forward_auth to                    store (MyBook) ─► worker: Whisper per track ─► diarize
  resumable upload        secretary-server (/v1/avatar)      ─► timeline ─► roles/kinds ─► brief ─► files, Obsidian,
                                                             memory, media library, notify, "who is this voice?"
```

It runs inside `lucina-memo` (same binary, port, auth, Whisper path, notifications and vault code as the voice
memos); its own worker runs beside the memo worker. Code: `crates/lucina-memo/src/meetings/` (API, store, pipeline)
and `crates/lucina-memo/src/diarize/` (diarization, voice profiles; memos use it too).

## API

The upload contract is Secretary Code's `packages/desktop/src/main/secretary/recorder/MEETINGS_API.md`; this server
implements it exactly. Auth as `/v1/memos`: through p1 (`X-Memo-Gateway` after secretary-server accepted the device
token or cookie; cookie writes need `X-Secretary-CSRF: 1`) or `Authorization: Bearer <service.token>` locally.

| Route | Body / headers | Answer |
| --- | --- | --- |
| `POST /v1/meetings` | `Idempotency-Key: <local id>`; JSON `title`, `started_at`, `ended_at`, `source_app {bundle_id, name}`, `calendar_event_id` or null, `tracks [{name, bytes, sha256, mime, start_offset_ms?}]` | `201 {id, status, folder}`; `200` with the same id for a repeated key. Track names `mic.m4a`, `system.m4a`, `video.mp4` (`mic`, `system`, `video` accepted) |
| `PUT /v1/meetings/{id}/tracks/{name}` | `Content-Range: bytes */N` (how much is stored?) or `bytes a-b/N` with those bytes (≤ 64 MiB) | `308` + `Range: bytes=0-<last>` (none = 0 bytes, never a `Location`); `200 {name, bytes, sha256}` when whole and the SHA-256 equals the declared one (also the answer to `*/N` afterwards); `409`/`416` + `Range` when `a` is not the stored offset; `400` when `N` is not the declared size |
| `POST /v1/meetings/{id}/complete` | – | `202 {id, status: "processing"}`; `409` until every track is whole |
| `GET /v1/meetings/{id}` | – | `{id, status: uploading\|processing\|ready\|failed, brief, transcript, transcript_url, error, stage, title, speakers, brief_json, meeting}`; `brief` = `brief.md`, `transcript` = `[mm:ss] Professor: …` lines, `transcript_url` = `https://getsecretary.tech/v1/meetings/{id}/transcript` once ready |
| `GET /v1/meetings?limit&before` | – | `{meetings: [...], next}` newest first |
| `GET /v1/meetings/{id}/transcript[?format=json]` | – | `transcript.md` as text (the browser link; p1 checks the owner's cookie), or `transcript.json` |
| `POST /v1/meetings/{id}/speakers` | `{speaker: "S2", name?: "Anna", role?: "client"}` | The name in every output of the meeting, the cluster's voice into Anna's profile, her name in the other recordings of that voice; the files and note are made again (no second notification). `409` while processing |
| `GET /v1/meetings/voices` | – | `{profiles: [{id, name, owner, samples}], unknown: [{id, recordings, recurring, asked_at, sightings}]}` |
| `POST /v1/meetings/voices/{voice}` | `{name}` | Names an unknown voice everywhere it was heard (meetings and memos) |
| `POST /v1/meetings/{id}/reprocess` | `{from: "analysis" \| "transcript" \| "whisper"}` | Redoes the steps from there (new revision, new notification) |

A track whose bytes reach the declared size but hash differently is reset to 0 and answered `200` with the server's
digest and `"verified": false`: the Mac sees the mismatch and keeps its copy; a retry uploads the track again.
Bytes that arrived before a dropped connection are kept, and the `308` reports them.

## Storage

`LUCINA_MEETINGS_DIR` (default `/home/nez/mnt/mybook/lucina-meetings`) must exist: the service never creates it, so an
unmounted MyBook answers `503 storage_unavailable` instead of filling the system disk.

```
<base>/2026/2026-10-26 1430 Acme – Weekly sync/
  mic.m4a  system.m4a  video.mp4      the tracks (.<name>.part while uploading)
  meeting.json                        state (atomic writes, 0600); the folder is the record
  transcript.json                     segments: start, end, speaker id + name, role, kind, text, track, words
  transcript.md                       readable transcript
  speakers.json                       id, label, name (+ source), suggested name, role, track, profile/voice, similarity, talk time
  brief.md                            the brief
  .work/                              Whisper output per track, turns, voice centroids, analysis, brief.json
```

The folder name is `<YYYY-MM-DD HHmm> <company or calendar title> – <title>` in the owner's zone: the company from
the attendees' e-mail domains (personal providers and `LUCINA_MEETINGS_OWN_DOMAINS` skipped), else the calendar
event's title, else none (the title alone, never the recording app or display); the prefix is left out when it equals the title; a second recording with the
same name gets ` 2`. The calendar event comes from secretary-server by the Mac's `calendar_event_id` (looked up for
4 s at create; later by the worker, or the event the recording falls in, kernel K8). Each step writes its output
before `meeting.json` records it, so a crash or restart continues; at start-up the index is rebuilt from every
`meeting.json` and each upload resumes from the length of its `.part` file. Media: each track is registered in
lucina-media (`PUT 127.0.0.1:8830/internal/v1/external/meeting/<id>-<mic|system|video>`, album "Meetings").

## Pipeline

Statuses `uploading → queued → transcribing → diarizing → analyzing → publishing → done` (or `failed` after
`LUCINA_MEMO_MAX_ATTEMPTS`; a step may go back to `queued` for a retry).

1. **Transcripts**: per track (`mic.m4a`; `system.m4a`, else the video's audio) ffmpeg → 16 kHz WAV → whisper.cpp
   large-v3 on the GPU through the `gpu-lease` shim with `-ojf` (word timings). GPU busy or model missing (MyBook
   locked): retried every minute without counting an attempt.
2. **Speakers**: the system track is diarized (below); the owner's voice profile is refreshed from up to 60 s of his
   microphone speech; each cluster is the owner (his profile), a known person (profile, cosine ≥
   `LUCINA_VOICE_MATCH_THRESHOLD`, kernel MT5) or an unknown voice (`vu_…`, its sightings recorded). Timelines merge on
   the shared clock (`start_offset_ms`): system words take the speaker whose turn overlaps them most (MT7), a Whisper
   segment spanning two speakers is split, microphone lines that repeat a simultaneous system line (the remote voices
   through the owner's speakers) are dropped, and one speaker's lines < 1.5 s apart are joined.
3. **Roles and kinds**: Lucina's model (DeepSeek, text only) gets the labelled lines and the calendar context and
   answers a strict JSON schema: roles from a fixed list (professor, lecturer, student, client, colleague,
   interviewer, candidate, …) and the kinds of the lines that are not plain statements; a name only when it is said
   in the meeting or the invitation (kept as `suggested_name`, never applied without the owner). Any schema violation
   rejects the whole reply and the deterministic rules apply (lecture: the other speaker who talks most is the
   lecturer; `?` = question, the next speaker answers; decision and action phrases in English, Italian, Danish).
4. **Brief**: JSON (about, key points by speaker, questions and answers, decisions, action items with owner and due
   date, follow-ups, tags), validated (speaker ids, times, lengths), else a deterministic brief from the kinds. Long
   meetings are noted in parts first. The model unreachable on the last attempt also gives the deterministic versions.
5. **Publishing**: the folder files; the Obsidian note `Meetings/<folder name>.md` (vault rules of the memos: rsync to
   `pi-cursor:/home/nes/Obsidian`, frontmatter, the brief, the transcript with block ids, index
   `00 Lucina meetings.md`; the path recorded before the push); Lucina's memory (Honcho, session
   `secretary_meetings`, as the owner's own turn; only with `readwrite`; marker before the write, at most once per
   revision); the notification (app inbox `/meetings/<id>`, dedup `meeting:<id>:<rev>`, and Telegram, marker first);
   then the "who is this voice?" question for every unknown voice heard in `LUCINA_VOICE_ASK_AFTER` (≥ 2) recordings,
   once (MT6, marker first).

## Diarization

CPU, in Rust (`diarize/onnx.rs`, the sherpa-onnx recipe): pyannote segmentation-3.0 over 10 s windows every
`LUCINA_DIARIZE_STEP_SEC` (2 s), one speaker embedding per window and local speaker (Kaldi fbank, WeSpeaker ResNet34
by default), average-linkage clustering by cosine distance (`LUCINA_DIARIZE_CLUSTER_THRESHOLD`, 0.6), turns per
cluster. ONNX Runtime is loaded at run time (`ort` with `load-dynamic`): nothing is downloaded or linked at build
time; missing models leave the system track as one speaker (meetings) or skip the labels (memos). On nezopt a 44 s
two-voice clip takes 1.3 s.

**One-time model download** (into `~/models/diarization`, the default `LUCINA_DIARIZE_MODELS`):

```bash
mkdir -p ~/models/diarization && cd ~/models/diarization
curl -fLO https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2
tar xjf sherpa-onnx-pyannote-segmentation-3-0.tar.bz2 && rm sherpa-onnx-pyannote-segmentation-3-0.tar.bz2
curl -fLO https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx
# optional alternative embedding model (LUCINA_DIARIZE_EMBEDDING_MODEL):
curl -fLO https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx
curl -fL https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-1.22.0.tgz | tar xz
mv onnxruntime-linux-x64-1.22.0 onnxruntime   # LUCINA_DIARIZE_ORT_LIB = …/onnxruntime/lib/libonnxruntime.so
```

(The `speaker-recongition-models` spelling is sherpa-onnx's release tag.) `GET /health` reports `"diarization": true`
when all three files are there.

**Voice profiles** (`LUCINA_VOICES_FILE`, default `<memo data>/voices.json`, mode 0600, biometric: never leaves
nezopt): the owner's from his microphone, one per person the owner named (up to 12 embeddings each), and the unknown
voices with where they were heard.

## Voice memos

The memo worker labels speakers too (step 1a, kernel K11): once per transcript, when the diarizer is available; two
or more voices give `speaker` on the transcript lines (`Giuseppe`, a profile's name, or `Speaker N`) and
`speakers: true`; the summary prompt already shows them. The memo API is unchanged.

## p1 Caddy

`deploy/lucina-memo/Caddyfile.p1`: an `@meetings` block (`/v1/meetings`, `/v1/meetings/*`) with the same
forward_auth, CSRF rule and `X-Memo-Gateway` key as `@memos`, a 65 MiB body limit and 10-minute proxy timeouts,
proxied to nezopt `:8795`. Paste it next to `@memos` in the `secretary_app` snippet, then
`caddy validate --config /etc/caddy/Caddyfile` and `systemctl reload caddy`. Check: `curl -s
https://getsecretary.tech/v1/meetings` answers 401 without a token.

## Deploy

1. Models: the download above (once).
2. `deploy/lucina-memo/install.sh` (builds, installs, creates `~/mnt/mybook/lucina-meetings` when the MyBook is mounted,
   restarts `lucina-memo.service`; the unit now allows 3 GiB for diarization).
3. Add the meetings block of `deploy/lucina-memo/memo.env.example` to `~/.config/lucina-memo/memo.env` (an existing
   file is not overwritten), restart.
4. p1 Caddy as above.
5. `curl -fsS 127.0.0.1:8795/health` shows `meetings` and `diarization`.

## Verification

`docs/verification.md` §10 and §16: Stateright `verification/models/src/meetings.rs` and its TLA+ twin
`formal/meetings/Meetings.tla`, the Verus kernel `verification/verus/meeting_kernel.rs` (MT1–MT8) whose bodies are
`crates/lucina-memo/src/meetings/kernel.rs` (differential test `tests/verus_meeting_kernel.rs`), and K11 in the memo
model and kernel. Tests with fakes for Whisper, the diarizer and the LLM: `crates/lucina-memo/tests/meetings.rs`.
