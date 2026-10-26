# Share to Lucina: wire contract (v1, 2026-10-25)

The owner taps Share on anything (photo, video, PDF, document, link, text, audio, contact,
location, a file from Files/Drive) on iOS, Android or the installed web app and picks Lucina.
Every client sends ONE request; the server does the rest asynchronously, so a client that is
killed right after the request (or whose background upload finishes later) loses nothing.

## Request

`POST https://getsecretary.tech/v1/share` — `multipart/form-data`.

Auth: `Authorization: Bearer <device token>` (iOS, Android), or the web cookie session plus the
header `X-Secretary-CSRF: 1`. p1 Caddy checks it (forward_auth on secretary-server, like
`/v1/media/uploads/*`), then proxies to lucina-rs on nezopt `100.107.164.33:8790` and adds
`X-Share-Gateway: <key>`. Request bodies up to 2 GB stream through.

Form fields (all optional, but at least one `text`, `url` or `file` part is required):

| name | repeat | meaning |
|---|---|---|
| `shareId` | no | client-generated UUID; idempotency key. A repeat of a finished or running share returns the first result with `duplicate: true` and stores nothing twice. |
| `intent` | no | `ask` (default), `library`, `shopping`, `note`, `remind` |
| `note` | no | the owner's words to Lucina, max 4000 chars |
| `lang` | no | device language (`it`, `en`, `da`…), used for default wording when `note` is empty |
| `sessionId` | no | chat session, same rules as `/v1/chat` (`[A-Za-z0-9_-]{0,64}`); empty = default session |
| `channel` | no | `ios` or `web` (timeline channel; Android sends `web`, like its voice turns) |
| `source` | no | `ios-share`, `ios-intent`, `android-share`, `web-share`… (logged, stored as media `source`) |
| `title` | no | a page or item title the OS gave (e.g. Safari page title) |
| `text` | yes | a text snippet |
| `url` | yes | a link |
| `file` | yes | a file part; `filename` and `Content-Type` from the client (`application/octet-stream` if unknown) |

Limits: 32 parts, 2 GB total. Clients convert HEIC/HEIF images to JPEG before upload.

## Response

`202 Accepted`:

```json
{"ok": true, "shareId": "…", "duplicate": false, "status": "processing",
 "userItemId": "…",
 "attachments": [{"kind": "image", "name": "IMG_1.jpg", "mime": "image/jpeg", "mediaId": "itm_…", "bytes": 123}]}
```

`400` bad input (`{"ok":false,"error":"…"}`), `401` wrong/missing gateway key, `413` too large.

`GET /v1/share/{shareId}` → `{"ok":true,"shareId","status":"processing|done|error","userItemId","lucinaItemId"?, "reply"?, "error"?}`.

## Timeline items

The share appears in the chat as one user item (written at once, channel from the request) and,
when processing ends, one Lucina item (the normal reply item). Timeline `attachments` entries are
extended (all new fields optional, old readers ignore them):

```json
{"kind": "image|video|audio|voice|pdf|document|file|link|text|contact|location",
 "name": "IMG_1.jpg", "mime": "image/jpeg", "mediaId": "itm_…", "url": "https://…", "bytes": 123}
```

- Files carry `mediaId` (the lucina-media item: thumbnails at `GET /v1/media/items/{id}/thumb?size=256`,
  original at `/v1/media/items/{id}/original`, both with the device token).
- Links carry `url` (`name` = page title once fetched, else the host).
- Text snippets are not attachments: they go into the user item text (quoted) if short, else `kind:"text"` with the first 80 chars as `name`.
- The user item `text` is the owner's `note`, or a short default (e.g. "📎 IMG_1.jpg") when there is none.
- User item `meta.kind = "share"`, `meta.shareId`, `meta.intent`.

## Intents

| intent | server does |
|---|---|
| `ask` | read everything (vision, OCR, PDF text, transcript, page text), then a normal Lucina turn with the note (or "Look at what I shared and tell me about it") plus the extracted content |
| `library` | store files in lucina-media (album "Shared"); text/links become a note; short confirmation, no model call |
| `shopping` | extract, then a turn "Add the items in this to my shopping list" |
| `note` | extract, then a turn "Save this as a note" (title from the content) |
| `remind` | extract, then a turn "Remind me about this" + note (the note usually holds the time) |

Files always go into lucina-media (album "Shared", source = the request `source`), whatever the intent.

## Contract additions (v1.1)

- `POST /v1/share` is now ALSO the chat's attachment path: the composer sends files + caption the same way
  (`source` = `ios-chat`, `android-chat`, `web-chat`; `note` = the caption/question).
- New optional form field `meta` (JSON, ≤64 KB): `{"files": [{"index": 0, "transcript": "…", "width": 1170,
  "height": 2532, "durationMs": 4200, "takenAt": "2026-10-25T09:00:00Z", "lat": 55.6, "lon": 12.5}],
  "location": {"lat": 55.6, "lon": 12.5, "name": "…"}}`. `index` = the 0-based order of `file` parts.
  `transcript` = an on-device transcript the phone already made (the server uses it instead of Whisper).
  `location` = the owner shared their current location from the composer.
- `GET /v1/share/{id}` adds `stage`: `received` → `reading` → `thinking` → `done` | `error`, and per attachment
  `{index, kind, mediaId, state: "pending|reading|read|failed", summary?}`. Clients poll it (every 2 s, max 10 min)
  to show "Lucina is reading…" on the bubble until the Lucina item arrives.
- Timeline attachment objects may carry `thumbhash` (base64 ThumbHash from lucina-media, when known),
  `width`, `height`, `durationMs`.
- lucina-media item JSON (`GET /v1/media/items/{id}`) now has `thumbhash` and `urls.grid` (256 px JPEG XL),
  `urls.preview` (1440 px progressive JPEG XL) at `/v1/media/items/{id}/r/grid|preview`, plus `/thumb?size=256|1024`
  (JPEG) for clients that cannot decode JPEG XL. Playable audio/video: `POST /v1/media/items/{id}/play` →
  tokenised `/v1/media/s/{token}/{id}/file` URLs (no header needed).

How each file type is read, and how Lucina answers, is in [attachments.md](attachments.md).

Server notes for v1.1:
- Files go into lucina-media with `POST /internal/v1/ingest` (`move: true`), and are read back from the
  library path (`/internal/v1/items/{id}/path`). Work files (vision JPEG, WAV, frames) stay in the share dir:
  a `work-NN-*` symlink points at the stored original. An older lucina-media without ingest (404/405) gets
  the in-place `PUT /internal/v1/external/...` registration instead.
- The timeline user item is written once, with what is known at receipt (library ids, link hosts). Later
  details (page titles, reading state, summaries) are in `GET /v1/share/{id}` only: the shared
  `timeline.jsonl` is append-only (Node also appends to it), so items are never rewritten.
- `LUCINA_SHARE_DIR` defaults to `$LUCINA_DATA_DIR/share`; production sets it explicitly. The attachment
  memory is `$LUCINA_SHARE_DIR/attachments.db`, created on the first share.

## v1.2 voice notes

Chat clients send one `file` part (`audio/mp4`, `.m4a`, or any supported audio),
`source` = `ios-chat`, `web-chat`, or `android-chat`, and `note` = optional caption
(usually empty). The optional `meta` field includes:

```json
{"files":[{"index":0,"voice":true,"transcript":"ricordami di comprare il latte","durationMs":4200,"waveform":"AAEC/w=="}]}
```

`voice` is accepted only for routed audio; it is ignored on other types. `transcript`
is optional: a non-empty on-device transcript takes precedence over Whisper.
`waveform` is optional standard base64 of up to 64 bytes, levels 0–255, oldest first;
invalid or oversized waveforms are dropped (maximum 88 encoded characters).
Voice audio is decoded for at most ten minutes and never gets a long-document summary.

The single append-only user item carries what was known at receipt:

```json
{"role":"user","channel":"ios","text":"📎 note.m4a","meta":{"kind":"share","shareId":"voice-note-0001","intent":"ask"},"attachments":[{"kind":"voice","name":"note.m4a","mime":"audio/mp4","mediaId":"itm_…","bytes":12345,"durationMs":4200,"waveform":"AAEC/w==","transcript":"ricordami di comprare il latte"}]}
```

Attachment `transcript` is included only if known at receipt; it is trimmed and clipped
to 4000 characters. `durationMs`, `waveform`, and `mediaId` are optional when unknown.
The ordinary timeline identity and timestamp fields are unchanged.

Exactly one voice note with an empty/whitespace caption enters the normal typed-chat
planner with the transcript itself as the user message. A short system addition tells
Lucina to answer normally in the spoken language, without describing the audio. A caption
or other shared parts keep the ordinary share turn, with the transcript in Shared content
and voice guidance. The transcript is remembered as handler `audio`, subtype `voice`.
If transcription fails or is empty, a single reply in the device language asks the owner
to repeat, without calling the chat model.

The single reply item adds (other reply metadata may also be present):

```json
{"role":"lucina","text":"…","replyTo":"<userItemId>","meta":{"transcript":"ricordami di comprare il latte","voiceReply":true}}
```

`meta.transcript` is the answered text, trimmed and clipped to 4000 characters; it lets
clients show a server transcript under the owner's bubble after reloading. With several
voice notes, known transcripts are joined in part order with newlines and clipped together.
A failed transcription has no transcript. `voiceReply: true` hints that clients may speak
the reply through the existing `/v1/tts`.

`GET /v1/share/{id}` includes known voice transcripts in its attachments, including those
obtained by Whisper after receipt:

```json
{"index":0,"kind":"voice","name":"note.m4a","mime":"audio/mp4","mediaId":"itm_…","bytes":12345,"durationMs":4200,"waveform":"AAEC/w==","transcript":"ricordami di comprare il latte","state":"read","summary":"ricordami di comprare il latte"}
```

Retries with the same `shareId` return `duplicate: true`, with no extra timeline items
or model turn, just as for other shares.

## Server implementation and configuration

Both lucina-rs binds expose the routes. The server checks `X-Share-Gateway` against the trimmed
key file, with a constant-time digest comparison, before parsing a body. An unset, empty, or unreadable
key file disables sharing (503); an incorrect/missing header is 401. The device token/cookie is verified
on p1; the gateway key grants the same owner memory surface as the authenticated Secretary chat.

| Variable | Default / meaning |
|---|---|
| `LUCINA_SHARE_GATEWAY_KEY_FILE` | Unset: feature disabled. File containing the gateway key. |
| `LUCINA_SHARE_DIR` | `$HOME/data/lucina-share`; permanent originals and status manifests. Must be on persistent local storage. |
| `LUCINA_MEDIA_INTERNAL_URL` | `http://127.0.0.1:8830` (loopback media registration). |
| `LUCINA_VISION_SERVER_BIN` | `$HOME/llama.cpp/build/bin/llama-server` |
| `LUCINA_VISION_MODEL` | `$HOME/models/gemma-4-12b-it-qat/gemma-4-12b-it-qat-q4_0.gguf` |
| `LUCINA_VISION_MMPROJ` | `$HOME/models/gemma-4-12b-it-qat/mmproj-gemma-4-12b-it-qat-q4_0.gguf` |
| `LUCINA_VISION_URL` | Unset: manage a local server under `gpu-lease`. Optional already managed OpenAI-compatible endpoint (used by integration tests). |
| `LUCINA_WHISPER_BIN` | `$HOME/Projects/whisper.cpp/build/bin/whisper-cli` (gpu-lease shim). |
| `LUCINA_WHISPER_MODEL` | `$HOME/models/whisper/ggml-large-v3.bin` |

Whisper reuses lucina-memo's library without changing its behaviour. Share-specific limits are a
90 second GPU lease wait and a 14 minute transcription timeout; CPU fallback uses the installed
`$HOME/models/whisper/ggml-large-v3-turbo-q8_0.bin`. ffmpeg limits audio to the first 30 minutes.

The managed vision server uses an 11 GB GPU lease (90 second wait), then CPU fallback. It disables
Gemma thinking and accepts `reasoning_content` when `content` is empty. The managed process stops
five minutes after its last image. An explicitly configured `LUCINA_VISION_URL` is managed externally.
Images are converted to JPEG at at most 1536 pixels; OCR uses eng plus ita/dan only when installed.
PDF text is read with pure Rust; scanned PDFs use embedded DCTDecode images from the first five pages,
with vision on the first extracted image. No external PDF renderer is needed. Office ZIP/XML extraction
is bounded; unsupported RTF or proprietary iWork binary content is reported as unreadable while keeping
the original. Each extraction has a deadline (4 minutes, or 15 minutes for audio/video); extracted text
is capped at 12,000 characters per part and approximately 30,000 per share including metadata.

Multipart ingestion counts all fields toward the 32 part limit. Text fields are UTF-8; text snippets
are limited to 3 MB each, URLs to 8192 bytes, source to 120 characters, title to 1000, lang to 32,
and note to 4000 characters. Short snippets (at most 500 characters) are quoted in the timeline.
Originals stream into a private staging directory, then move to
`<share-dir>/<yyyy-mm>/<shareId>/<nn>-<sanitized filename>`. Names retain their extension and fit in
120 bytes. Media registration uses stable external ids; an unavailable media server does not fail
ingestion. The worker retries missing registrations, and originals stay on disk even if both attempts fail.

`status.json` contains both the public status and the processing manifest. Writes use sync + atomic
rename. A process-local ingest lock serializes final idempotency checks and publication. Completed
uploads with the same shareId across months/restarts return their stored result; staging files from
a duplicate request are removed. Startup resumes processing manifests, recreates an absent user item
using its persisted id, and recognizes an already written Lucina reply after a crash. A semaphore
serializes processing. The status endpoint includes attachments as an additional optional field.

Share turns enter the normal chat lifecycle using the existing user item. A focused hook, analogous
to the media and memo hooks, passes all extracted content to a tool loop even with `LUCINA_PLANNER=json`.
It exposes notes, shopping, and reminders, avoiding keyword shortcuts over untrusted source text. It
writes one final Lucina item and no tool progress items. Chat calls outside this hook retain their
existing path. Share guidance allows up to eight sentences for document summaries. Library shares
skip file extraction and the chat model; text/links are saved through the existing notes tool (including
its local fallback) and as a permanent markdown file registered in Shared.

Link fetching has a 15 second total deadline, at most five redirects, and a 3 MB decoded response cap.
Every hop resolves and validates all addresses, pins those addresses for the connection, and disables
proxy environment variables. Loopback, private, link-local, CGNAT/tailnet, IPv6 ULA, mapped private
addresses and other nonpublic/transition ranges are refused. The loopback test override is a debug-only
config field, defaults off, and has no environment variable. Downloaded PDFs/images are saved and
registered in Shared. Page titles/media ids update the original timeline item's attachments, preserving
its id and arrival position; secretary-server's opaque attachment/meta fields preserve the extensions.

## Deployment notes (manual; this change does not deploy)

Generate one secret and provision identical contents on both hosts. On p1:

```sh
sudo sh -c 'umask 077; openssl rand -hex 32 > /etc/caddy/share-gateway.key'
sudo chmod 600 /etc/caddy/share-gateway.key
```

Transfer that key securely to nezopt as `~/.config/lucina-rs/share-gateway.key` (create the directory
with mode 700 and the key with mode 600). Configure
`LUCINA_SHARE_GATEWAY_KEY_FILE=/home/nez/.config/lucina-rs/share-gateway.key` in nezopt's
`~/.config/lucina-rs/mode.env`. See [the environment template](../deploy/lucina-rs/share.env.example).
Caddy must be able to read its key (use its service account as the file owner).

Add a handler beside p1's existing `@media_upload` handler, before any catch-all proxy. Copy the
existing media handler's device/session + CSRF `forward_auth` stanza verbatim: it is the same auth
check, not a request forwarded through secretary-server's share API. The resulting handler is:

```caddyfile
@share path /v1/share /v1/share/*
handle @share {
    request_body {
        max_size 2GB
    }
    # Insert the complete forward_auth stanza from @media_upload here,
    # including its deployed upstream, URI, and forwarded method/URI headers.
    reverse_proxy 100.107.164.33:8790 {
        header_up X-Share-Gateway {file./etc/caddy/share-gateway.key}
        flush_interval -1
    }
}
```

The deployed Caddy configuration is not in this repository; its existing `@media_upload`
forward_auth stanza must be copied into the indicated position before using this handler. Both POST and GET require the gateway check. Preserve
`Authorization`, cookies, and `X-Secretary-CSRF` for the forward_auth subrequest and its existing
method/URI forwarding rules. The share body goes straight from Caddy to lucina-rs with streaming;
secretary-server is only involved in authentication and subsequent timeline reads.
