# LucinaVoice low-latency protocol (2026-11-01)

Goal: speech starts as soon as possible after the owner stops talking. Three hops:
ESP32 ⇄ secretary-server (p1, `/v1/voice/ws`) → lucina-rs (`/api/secretary/chat`, SSE) and Kokoro and lucina-memo STT.

## 1. lucina-rs: streaming chat (`POST /api/secretary/chat` with `Accept: text/event-stream`)

The body is unchanged (`ChatRequest`), plus an optional `"voice": true`. Without the Accept header, the route behaves
exactly as today.

- `voice: true` appends a spoken-style rule to the turn's extra system text: "This turn is spoken aloud through a
  speaker: answer in one to three short sentences, no markdown, lists, emoji or URLs; write numbers, dates and times
  the way they are said."
- The response is `text/event-stream`. Events, in this order:
  - `event: delta` / `data: {"text":"<fragment>"}`: reply text as the model writes it. Sent only when the reply
    comes from a streaming LLM call (`generate_reply` and its callers). There are zero or more of these.
  - `event: reply` / `data: {"text":"<the whole final reply>","sensitive":bool,"replaces":bool}`: sent exactly once,
    as soon as the turn's reply text is settled. That is before rich-reply, Honcho, memory and other post-processing.
    `replaces` is true when the final text is not the concatenation of the deltas (a retry after tool-call markup, a
    fallback text, a sensitive replacement).
  - `event: done` / `data: <the exact JSON the non-streaming call would have returned>`.
  - `event: error` / `data: {"code":"…","message":"…"}`, on failure. No `done` follows.
  - `: ping` comment lines every 15 s while the turn runs.

Implemented in `crates/lucina-server/src/voice.rs` (details and edge cases: "Streaming chat" in
`docs/lucina-rust/INVENTORY.md`). `done` carries the plain call's whole body, `{"ok":true,"data":{…}}`.
`replaces` is also true when the turn had no deltas (the reply came from the JSON plan, a fixed text or a skill):
the client has nothing spoken yet and speaks `text`.

## 2. secretary-server: `GET /v1/voice/ws` (WebSocket, device auth via `Authorization: Bearer` on the upgrade)

One connection per device, kept open. The server sends a WS ping every 20 s. One turn at a time.

Client → server:
- text `{"type":"start","mode":"ptt"|"vad","lang":"it"|"en"?}`: a turn begins (button pressed). The server starts
  warming STT now.
- binary: s16le mono PCM at 16 kHz, any frame size up to 8 KiB.
- text `{"type":"end"}`: the button was released (ptt), or the device gives up.
- text `{"type":"cancel"}`: drop the current turn (stop sending audio frames). The server aborts it at once and sends
  nothing for it; frames, `end` and `cancel` with no turn running are ignored.

Server → client:
- `{"type":"listening"}`: reply to `start`. Since section 10 it also carries `purpose`, `codec`, `rate` and `turn`, for
  every `start`, old-style ones included (10.1); a client that reads only `type` is not affected.
- `{"type":"endpoint"}`: VAD found the end of speech (vad mode), or the 1 MiB (about 32 s) recording cap was hit (either
  mode). The client stops sending.
- `{"type":"transcript","text":"…"}`
- `{"type":"audio","rate":24000}`: precedes the first binary audio frame of the turn. Binary frames are s16le mono
  24 kHz PCM, at most 8 KiB each. Sample boundaries are kept (even lengths).
- `{"type":"reply","text":"…"}`: the text that was spoken (for logs), sent after the last audio frame and before
  `done`. For a sensitive reply it is the fixed phrase.
- `{"type":"done","ms":{"stt":…,"lucina_first":…,"tts_first":…,"first_audio":…,"total":…}}`: all measured from
  end-of-speech (the `end` frame or the VAD endpoint). With the Jev fast path (section 6) `ms` also carries `route`
  and, when a filler phrase was played, `ack`; both are absent otherwise. When a filler is played, `first_audio` is
  the filler's and can be earlier than `tts_first`.
- `{"type":"error","code":"…","message":"…"}`: the turn ended without (complete) audio. `no_speech`, `stt_unavailable`,
  `lucina_unavailable`, `tts_failed`, `busy`, `rate_limited`, `bad_request`, `silent` (the recording had no signal: 10.7, message "The recording was silent: check the microphone."), and for notes `note_failed` (10.4). The socket stays open after an error.

Server pipeline per turn: `start` warms STT (a background 0.5 s-silence call to the local STT, unless one ran in the
last 60 s). PCM accumulates while an energy VAD watches it. On end-of-speech the server runs STT, then streaming chat
with `voice: true`. A sentence splitter emits the first segment at the first sentence end (`. ! ? …` or a newline),
or at a comma after 60 characters, then every following sentence. Each segment goes to Kokoro (`pcm`) in order while
the LLM keeps writing, and PCM is forwarded to the socket as it arrives.

## 3. ESP32

The device connects at boot once Wi-Fi is up and it is paired, and reconnects with backoff (1, 2, 4 … 30 s). It
answers pings and pings the server every 30 s. Press: an earcon right away, `start`, then streaming mic frames
(about 32 ms each). Release: `end` and a short "ack" blip. It plays audio frames as they arrive. The LED pulses while
waiting.

## 4. Telemetry over the link (device → server text frames)

The device never logs audio content, Wi-Fi passwords or tokens. Times are the device's uptime in ms (`t`); each frame
carries `now` (the uptime when it was sent), so the server places events in wall time as
`received_at − (now − t)`. The device sends telemetry only on the open link: after `hello`, piggybacked after a turn,
and otherwise every 5 minutes (the radio is on anyway with power save off, so no extra wake-ups). Events wait in a
RAM ring buffer (64 events, oldest dropped and counted) while the link is down.

- `{"type":"hello","now":…,"specs":{…},"boot":{"reset":"<reason>","seq":<boot counter>}}`: the first frame on every
  connection.
  - `specs`: `chip` ("esp32"), `rev` ("v3.1"), `cores`, `cpu_mhz`, `flash_bytes`, `psram_bytes` (0), `fw`
    (version), `commit` (git short sha or "unknown"), `mac` (last two bytes, "da:b8"), `heap_total`,
    `ina` ("ina219" | "ina226" | null), `temp_sensor` (false on the classic ESP32).
- `{"type":"telemetry","now":…,"dropped":<n>,"events":[…]}`: up to 32 events, each `{"t":…,"k":"<kind>",…}`. Kinds:
  - `boot` `{reset, seq}`;
  - `wifi` `{up, rssi?, reason?}`;
  - `link` `{up, ms?, err?}` (`ms`: connect plus upgrade time);
  - `turn` `{ok, err?, mode:"link"|"http", speech_ms, first_audio_ms?, total_ms, up_bytes, down_bytes, server_ms?:{…}}`,
    where `first_audio_ms` is measured on the device from end of speech, and `server_ms` is the `done.ms` object as
    received;
  - `stats` `{uptime_ms, heap_free, heap_min_free, rssi?, up_bytes, down_bytes, tx_air_ms, cpu_mhz, temp_c?,
    power:{est_mw, rail_mv?, rail_ma?, energy_mwh}}`: totals since boot;
  - `error` `{what, detail?}`.

Power estimate: `est_mw` comes from a duty-cycle model of the ESP32-WROOM-32 at 3.3 V: CPU at 240 MHz with the radio
listening about 100 mA, Wi-Fi TX bursts about +140 mA for the estimated airtime, the I2S amp and mic while active.
When an INA219 or INA226 answers at I2C address 0x40 (SDA GPIO21, SCL GPIO18, 0.1 Ω shunt on the 5 V input),
`rail_mv`/`rail_ma` are measured and `est_mw` is their product.

## 5. Device telemetry API (secretary-server, device or owner auth)

The server stores what the device sends, plus its own per-turn spans, in `SECRETARY_DATA_DIR/voice/<device id>/`
(`events.jsonl`, rotated at 2 MiB, keeping 2 files; `status.json`, written atomically at most every 5 s). Code:
`voice_telemetry.rs`.

Ingest (on `/v1/voice/ws`, never answered): a `hello` or `telemetry` frame over 16 KiB, or not JSON, is ignored; a batch
keeps its first 32 events; an event over 1 KiB or of an unknown kind is dropped; only the keys listed in section 4 are
kept, numbers are clamped, strings are cut (64 to 160 characters) and lose control characters and `<` `>`. The socket
and the turn in progress are never affected. An event's wall time is `received_at - (now - t)`, the difference clamped
to 0..7 days. Bytes and energy counters come from the `stats` events' totals since boot: the server adds the difference
to the previous sample (a smaller value is a reboot and counts in full; the first sample ever counts in full only if
the device booted in the last 10 minutes). "Today" is the date in the server's `SECRETARY_TIMEZONE`.

Every turn of the socket and of `POST /v1/voice` leaves a server record (the transcript's length, never its text); the
device's later `turn` event is merged into it when it lies within 30 s (either order). Server spans are the `done.ms`
moments: milliseconds since the end of speech, cumulative, not durations. The record's `up_bytes`/`down_bytes` are the
audio bytes received and sent for the turn. In `events.jsonl` it is an event of kind `server_turn`.

- `GET /v1/voice/devices`: `[{id, name, online, last_seen, fw, rssi?, est_mw?}]`. `online`: a voice socket is open now.
- `GET /v1/voice/devices/{id}/status`:
  `{id, name, online, connected_since?, last_seen, specs, boot?, live:{…last stats…}, network:{today_up_bytes, today_down_bytes, total_up_bytes, total_down_bytes}, power:{est_mw, measured, energy_mwh_today}, turns:[last 20, newest first: {at, ok, err?, transcript_chars, speech_ms, spans:{stt, lucina_first, tts_first, first_audio, total, route?, ack?}, device_first_audio_ms?, up_bytes, down_bytes, intent?, confidence?, jev_ms?, fast?, source?}]}`.
  `spans.route`/`spans.ack` and the route fields (`intent`, `confidence`, `jev_ms`, `fast`, `source`: Lucina's `route`
  event, section 6) are present only for turns where Lucina sent them; they never carry transcript text. The
  `server_turn` events in `events.jsonl` carry the same fields.
  The turns combine the server's spans with the device's `turn` event; a device turn the server never saw stands alone
  (`transcript_chars` 0, spans from the device's `server_ms`). `power.measured` is true when the last stats carry
  `rail_mv`.
- `GET /v1/voice/devices/{id}/events?since=<rfc3339>&before=<rfc3339>&kind=<k>&limit=<1..500, default 100>`: newest first,
  `{events:[{at, k, t?, …}], next?}`. `since` is inclusive, `before` exclusive; `kind` is a device kind or `server_turn`.
  `next` is present when more remain: the `at` of the oldest event returned, to pass as `before` for the next page.
  `at` is UTC with milliseconds (`2026-11-01T10:00:09.000Z`). A malformed `since`, `before`, `kind` or `limit` is `400
  bad_request`.

Who may read: only the owner's browser session (cookie) sees all devices. **Every device token** sees only its
own id: a speaker, a speaker paired but not connected yet, the owner's phone app alike (`403 owner_only` for any
other id, even an unknown one; its own list holds at most itself). An id with no voice data is `404
device_not_found` for the owner's session or a device asking for itself. No credentials: `401`.

## 6. Jev fast path for voice turns (2026-11-01)

Measured before (stub audio → first audio, server side): "che ore sono?" 1.6–5 s, "che giorno è oggi?" 2.0 s,
"quali promemoria ho oggi?" 4.1 s, "cosa c'è nella lista della spesa?" 7.0 s. STT is about 80 ms and the first TTS
sentence 0.4–0.9 s. The rest is the planner LLM, which runs after Jev has already classified the turn in about 0.2 s.

### lucina-rs (voice turns only: `voice: true`)
Implemented in `crates/lucina-server/src/chat/voice_fast.rs` (details: `docs/lucina-rust/JEV-ROUTING.md`).
It routes the owner's own spoken turns on the JSON planner (`LUCINA_PLANNER=json`). A turn from an MCP caller,
a share, a guest channel or the tools planner is not routed: no Jev call, no `route`, no `ack`, and the
normal path as before.
- **Jev starts first.** Jev intent classification starts at the very beginning of the turn, in a task of its
  own, concurrently with the rest of the turn's setup (context building runs next to it; it holds its
  tier-1 model call until the route is known, so a fast turn makes no model call at all). Its state is
  facts only: the transcript, the current local date and time (with weekday and time zone), and the channel
  `voice`, in the same template every time with no guessed context. The questions are the ones of everyday
  intent routing, so verdicts stay comparable. Cap: `LUCINA_VOICE_JEV_CAP_MS`, 400 ms. A slower answer,
  a quota error or a breaker means fail open to today's path (tier-0, then planner), unchanged.
- **Fast intents** are answered without the planner LLM, with a short templated spoken reply in the turn's
  language (it/en; the language the turn's other replies use). They are taken only when Jev's confidence is
  at least `LUCINA_VOICE_FAST_MIN` (default 0.85) and the existing tier-0 rules don't contradict it (a tier-0
  rule for another intent wins; plain `chat` does not count as a contradiction for a read):
  - `temporal_query`, only when the whole sentence is a plain question about the current time or date
    ("che ore sono?", "che giorno è oggi?", "what time is it?"); anything else, such as a deadline, an
    appointment, another city or another day, goes the normal way;
  - `grocery.shopping_list.query`, read from the store (at most 8 items read out);
  - `list_reminders`, today's or upcoming, read from the store (at most 3 read out);
  - `grocery.shopping_list.add` / `.remove` and `schedule_reminder` (including relative timers), only when
    the existing deterministic parsers resolve them without an LLM: `parse_grocery_intent` returns the same
    intent with a product; `parse_reminder_message` and `resolve_reminder` give the text and the due time.
    Otherwise they take the normal path. The write goes through the same store and writers as the normal
    path (`grocery_turn::process`, the reminder writer).
  - Never fast: a compound question over several data sources, a notes request, and (for the intents with
    it/en templates) a transcript in Danish.
- A fast turn still writes the timeline (user item and Lucina item) like a normal turn, so the app shows it.
  Its `reply` event has `replaces: true` (nothing was streamed) and no `delta` comes before it; `done` is the
  normal body (no rich-reply composer runs, so no model call).
- **SSE additions** (both optional for clients; `route` and `ack` are not sent for turns that are not routed,
  nor when lucina-rs has no Jev client configured: then the stream is the one of section 1, event for
  event. A configured Jev that is down, cooling or too slow still gets `route`, with `source` `tier0` or
  `fallback`):
  - `event: route` / `data: {"intent","confidence","jev_ms","fast":bool,"source":"jev"|"tier0"|"fallback"}`, as
    soon as the route is decided, before any reply text. `source` is `jev` when Jev's verdict decided (fast or
    not), `tier0` when Jev gave none (slow, failed or unavailable) and a tier-0 rule names the intent, else
    `fallback` (intent `unknown`, confidence 0). `jev_ms` is the time spent on Jev (0 when it was not called;
    the cap on a timeout). A fast route is always `source: "jev"`;
  - `event: ack` / `data: {"text":"Un attimo…"}` (it, `Et øjeblik…` da, `One moment…` otherwise), sent when
    the turn will take a slow path: right after `route` for the intents `web_crawl`, `computer_task`,
    `explicit_memory_recall`, `calendar_event`, `agent_message`, `generate_image`, `graph_task`,
    `spreadsheet_update`, `cursor_task`, and, for any other planner turn whose reply is not streaming, when the
    JSON plan call starts. At most once per turn, always after `route`, and never for fast turns, never
    once words were streamed or the reply was settled (so never for streamed chat).
- **Log line:** one `event="voice_route"` line per routed voice turn (target `lucina_voice::route`) with intent,
  confidence, jev_ms, fast, source, and `jev` (`ok`/`failed`) with `reason` (`unavailable`, `timeout`, `error`).

### secretary-server
- **Filler audio:** at startup (a background task, `voice_filler::spawn_prewarm`; it never delays the listener),
  synthesize the filler phrases with Kokoro `pcm` and keep the PCM in memory, keyed by voice: Italian (`if_sara`)
  "Un attimo…", "Vediamo…" and English (`af_heart`) "One moment…", "Let me see…". Bounded: at most 256 KB per phrase
  and 1 MiB in all (about 100 KB in practice). A phrase missing from the cache (Kokoro was down) is rebuilt lazily:
  the next `ack` that finds the turn's language empty starts a rebuild in the background. The cache is keyed by voice
  name, so a different voice for a language is simply built the same way.
- **On `ack`:** if nothing of the answer has been queued or sent for the turn yet, and no earlier `ack` played, and
  the route was not `fast`, send the cached filler PCM at once, preceded by the `audio` header if it is the first
  audio, then continue with the real answer. The phrase is the next one for the turn's language (the same rule as
  the voice: the `lang` the client gave, else the transcript's), rotating. The filler is the first segment of the
  turn's ordered audio queue, so the answer's segments play after it without overlap, and the socket's `audio`
  header goes out once. If the cache is empty, the ack text (Lucina's, or "Un attimo…"/"One moment…" if it is empty
  or over 60 characters) is synthesized like a sentence and goes first. An `ack` that arrives after a sentence was
  queued, a second `ack`, or one on a fast route is ignored. The filler is not part of the `reply` text and not
  part of the 1200-character budget. `POST /v1/voice` does the same: its body starts with the filler.
- **Timings:** the `route` fields go into the turn's `done.ms` (`route` = ms from end of speech to the route event,
  `ack` = ms to the filler audio, measured when its first frame is handed to the socket) and into the telemetry turn
  record (`intent`, `confidence`, `jev_ms`, `fast`, `source`, cleaned: strings cut and without control characters or
  `<` `>`, confidence in 0..1). They never include transcript text. A Lucina that sends neither event behaves exactly
  as before: no filler, no extra keys.
- **Log line:** one `voice route` info line per turn that had a `route` event (device id, intent, confidence,
  jev_ms, fast, source), and `route_ms`/`ack_ms` on the `voice turn` line.
- **Device page:** the Voice view shows each turn's intent (hover: confidence, Jev ms, source), a "Fast" badge, and a
  tick on the timing bar where the filler went out.
- **Firmware:** the device's `turn` event keeps `server_ms` as received, so `route` and `ack` pass through the
  existing ingest (they are in the clean-list of span keys); firmware that copies only the five old keys just does
  not report them.

## 7. The "Lucina Voice" chat (one shared thread)

- **Thread:** voice turns get their own timeline channel `voice` and conversation key `voice`, titled "Lucina Voice".
  It is ONE shared thread for all LucinaVoice devices; each item's `meta.device` and `meta.deviceName` say which
  speaker heard it.
  - `voice` joins `lucina-store` `CHANNELS`, `secretary-core` `Channel` and `CHAT_CHANNELS`. The conversation key
    rule maps `voice` → `voice`, and the title is "Lucina Voice".
  - secretary-server sends `channel: "voice"` (with `sessionId: "lucinavoice"`) for every `/v1/voice` and
    `/v1/voice/ws` turn.
- **What is stored:** text only, no audio. The user item is the transcript and the Lucina item is the spoken reply
  text. Both carry `meta.voice = {turnId, ms: {…done.ms…}, route: {intent, fast, …}}`, which is enough for the app to
  show per-turn timing; it holds no transcript beyond `text`.
- **Memory:** voice items go into Lucina's memory exactly like other channels (timeline ingestion, Honcho, recent-turn
  context), so a voice conversation is remembered in the main chat and vice versa. The voice channel is treated like
  the app channels (owner device), not like an external platform: no surface restrictions beyond what the owner's app
  has.
- **App:** the chat list shows "Lucina Voice" next to the main chat and the agent chats. A message shows the
  transcript and the reply, with an expandable timing line (the same spans as the device page). The thread is
  read-only in the app except for the transcript correction of §8.

## 8. Voice Learn (one shared vocabulary for every speech-to-text path)

- **Storage:** Lucina (lucina-rs) owns the vocabulary in its store (data dir on nezopt). Each entry is
  `{id, word, soundsLike?: [..], meaning?, lang?: "it"|"en"|"da", createdAt, updatedAt, source: "app"|"correction"}`,
  with at most 500 entries and 100 characters per field. No audio is stored server-side; an optional phone recording
  is used only once, to suggest `soundsLike` through STT, and is then discarded.
- **API:**
  - lucina-rs: `GET|POST /api/secretary/voice-learn`, `PATCH|DELETE /api/secretary/voice-learn/{id}`.
  - secretary-server proxies them as `/v1/voice/learn[/{id}]`. `GET` is open to the owner's session and to any device
    token (a speaker needs the list). Every write (`POST`, `PATCH`, `DELETE`, `/suggest`, `/sample`) is the owner's
    browser session only, like `/v1/voice/devices*`: a device token, even one just paired that never said hello, gets
    `403 owner_only`.
  - `POST /v1/voice/learn/suggest {transcript, corrected}` returns candidate entries from a transcript edit (the
    "add to Voice Learn" offer).
- **Every STT path uses it:**
  - lucina-memo `/v1/memos/stt` (voice turns), the memo/voice-note worker (chat voice notes from iOS, Mac and web),
    the prompter, and secretary-server's Gemini fallback.
  - **Biasing:** the words (plus `soundsLike`) go into the Whisper prompt (`transcribe_prompted` / the whisper-server
    `prompt` field), at most about 200 tokens, most recently used first. For Gemini, they become a short "Vocabulary:"
    line in its prompt.
  - **Post-correction:** after STT, close mishearings of an entry are replaced by its canonical `word`. Matching is by
    a normalized phonetic and edit-distance check against `word` and `soundsLike`, with a conservative threshold and
    whole-word only.
  - **Ambiguity:** when more than one entry is close, or the match is borderline, one Jev choice question ("which of
    these did the speaker mean: … / none") decides, within a 300 ms budget, failing open to no correction.
  - **Reporting:** applied corrections are reported back (`corrections: [{from, to}]`).
- **Meaning:** entries with `meaning` reach Lucina as context on every turn whose text contains the word, e.g.
  "Skibigårdsvej = the owner's home street". They are also written once to her memory (Honcho conclusions /
  lucina-memory) when an entry is created or updated, and removed or updated on delete or edit.
- **Learn from corrections:** editing a transcript in the Lucina Voice chat offers "add to Voice Learn" with the
  suggested entry.
- **Accuracy check:** a reproducible bench with Kokoro-generated clips of the entries (it/da/en voices, including a
  voice-note style clip through the memo path), measured without and with the vocabulary. It reports the word hit rate.

### Implementation notes: the shared core and lucina-memo (2026-11-01)

- **`crates/lucina-vocab`** is the one implementation of the vocabulary file, validation, the Whisper prompt, the
  post-correction and the meanings; lucina-rs (owner of the file), lucina-memo and anything else that transcribes
  use it. `Vocab::load/save` (atomic, 0600), `default_path()` (`LUCINA_VOICE_LEARN_FILE`, else
  `$HOME/Projects/Lucina/data/voice-learn.json`), `Vocab::prompt(max_chars)`, `Vocab::correct(text) -> Corrected
  {text, corrections: [(heard, word)], ambiguous: [{heard, candidates}]}`, `Vocab::meanings_for(text)`, and
  `Watched` (re-reads when the file changes). The caps are validated there: 500 entries, 100 characters per
  field, 20 `soundsLike` phrases per entry, unique words case-insensitively.
- **Prompt:** the words only, newest `updatedAt` first, after the caller's own prompt (`Lucina` for `/v1/memos/stt`),
  at most about 500 characters in all (a Whisper prompt holds 224 tokens and whisper.cpp keeps the end, so the
  vocabulary goes last). `soundsLike` is *not* put in the prompt: it lists mishearings, and a prompt only steers
  spelling, so it would make Whisper write them. `soundsLike` feeds the post-correction (exact match, any case) and
  the "did they mean" check.
- **`POST /v1/memos/stt`** answers `{text, language, ms, mode, corrections: [{from, to}], ambiguous: [{heard,
  candidates}]}`; `text` already has the corrections, both lists are `[]` when there are none. `ambiguous` entries
  are spans left unchanged (borderline, or equally close to two entries): the caller asks Jev (the `vocab` layer
  of §9) and, on a yes, applies the replacement itself.
- **Where chat voice notes are transcribed:** not by a separate service. A voice note from the iOS, Mac or web chat
  is an audio attachment (kind `voice`) of the chat turn; lucina-rs (`crates/lucina-server/src/share/extract.rs`,
  `audio()`) either takes the transcript the client made on the device (Apple's recognizer, `on_device`) or runs
  `lucina_memo::transcribe::transcribe` in its own process (whisper-cli large-v3). The second path uses the
  vocabulary through lucina-memo's `transcribe` module with no change in lucina-server; the first does not, so
  lucina-rs should run `Vocab::correct` on an `on_device` transcript. Voice memos (the Memos tab) go through
  lucina-memo's worker, whose device transcript is corrected when stored. Video reports (`video/`) use the same
  function.

## 9. Layered Jev decisions (voice turns)

Every layer has a fixed facts-only state, a budget of at most 400 ms, runs concurrently with other work where it
can, and fails open. A voice turn makes at most 3 Jev calls, and the jev-keyproxy caps apply. Each layer's
`{layer, verdict, confidence, ms, used}` goes into the turn's `route.layers` (SSE `route` event, `done.ms`, the
timeline `meta.voice`, the telemetry turn record), so the device page and the chat show which layers fired.

- **Kept:**
  - `intent`: quick command, tool or chat, and which intent (§6).
  - `slots`: a confirm for quick-command slots, only when the deterministic parser is unsure.
  - `confirm`: whether a risky voice action (purchase, message to someone else, delete) must be confirmed by voice
    first.
  - `vocab`: the "did they mean X" choice of §8, only on ambiguity.
- **Not kept:**
  - End-of-turn: push-to-talk release is the endpoint, and in vad mode an energy VAD is free while a Jev call costs
    about 200 ms per check.
  - Barge-in: the device mic is off during playback; a button press is the barge-in.
  - Language: Whisper already detects it.
  - Reply length: the `voice: true` rule already sets it.
  - Filler decision: derived from the intent, with no extra call.

## 10. Streaming input from every client: codecs, notes, text (2026-11-08)

The owner asked (2026-11-08) that on every device the audio flows to the server *while* he talks, so the work
overlaps his speech: the Secretary iOS app, the Mac app (Secretary Code, the web app in Electron), Juno and Fortuna
(the DSi). They all use this socket. Section 2 stays valid word for word: a `start` without the new keys is exactly
today's turn. The survey that led here is `docs/voice-streaming-survey.md`.

### 10.1 `start`, new optional keys

```json
{"type":"start","mode":"ptt","lang":"it",
 "purpose":"command"|"note", "codec":"pcm16"|"ima-adpcm", "rate":16000, "block":256,
 "thread":"voice"|"main", "channel":"ios"|"web", "sessionId":"…", "replyTo":"…", "source":"ios-chat",
 "text":false, "speak":true}
```

| key | default | meaning |
|---|---|---|
| `purpose` | `command` | `command`: a spoken (or shown) reply, today's turn. `note`: a voice note, a chat message with the audio file and its transcript, answered in the chat like a typed message; no speech is made for it. |
| `codec` | `pcm16` | `pcm16`: s16le mono, as in section 2. `ima-adpcm`: IMA-ADPCM in the WAV (`adpcm_ima_wav`) block layout, mono (10.2). |
| `rate` | 16000 | `pcm16`: 16000 only. `ima-adpcm`: 8000 to 22050 (Fortuna: 10909). |
| `block` | – | `ima-adpcm` only, and required there: the block size in bytes (WAV `nBlockAlign`), 8 to 4096 (Fortuna: 256). |
| `thread` | `voice` for a command | `voice`: the shared "Lucina Voice" thread (section 7). `main`: the owner's main chat, as `POST /v1/chat` would write it (the channel comes from the upgrade request's `X-Secretary-Client` header, else `channel`, else `web`, by the rule of `/v1/chat`). A note is always `main`; `thread: "voice"` with `purpose: "note"` is `bad_request`. |
| `channel` | – | `ios` or `web`, used only when the upgrade had no `X-Secretary-Client` header (Fortuna and Juno send `web`). |
| `sessionId` | – | the chat session for `thread: "main"`, the rule of `/v1/chat` (`[A-Za-z0-9_-]{0,64}`). |
| `replyTo` | – | note only: the timeline id the note answers (swipe to reply), `[A-Za-z0-9_-]{1,64}`. |
| `source` | `voice-ws` | note only: logged and stored as the media source, `[a-z0-9-]{1,32}` (`ios-chat`, `web-chat`, `juno-chat`, `fortuna-chat`). |
| `text` | false | command only: also send the reply as text while it is written (`delta`, `answer`, 10.3), for a screen. |
| `speak` | true | command only: false = no speech at all (no `audio` frames, no filler); the reply comes as text (`text` is implied). |

A key the server does not know is ignored, as before. A bad value of a known key (wrong type included; `null` counts as
absent) is `bad_request` and starts no turn. `mode: "vad"` with `purpose: "note"` is `bad_request` (a note ends when the
owner lets go, never on a pause). `thread`, `channel` and `sessionId` matter only for `thread: "main"` and for notes, and
`text` and `speak` only for commands; given anyway they are checked and then ignored. An empty `sessionId` is none. A
`thread: "main"` start whose upgrade carries a bad `X-Secretary-Client` is `bad_request` (the `/v1/chat` rule refuses it).
Voice notes can be switched off on the server (`SECRETARY_VOICE_SHARE_URL=off`): `start {purpose:"note"}` is then
`bad_request` "voice notes are off".

**Detecting the server.** `listening` now echoes what the server understood:
`{"type":"listening","purpose":"note","codec":"ima-adpcm","rate":10909,"turn":"<turn id>"}`. The turn id is a UUID v4,
unique per `start`; for a note it is the `shareId`. A server from before
this section answers a bare `{"type":"listening"}`; a client that sent `ima-adpcm` or `note` and sees no `codec`
key sends `cancel` and uses its old path.

### 10.2 Audio frames

- `pcm16`: as section 2 (binary frames of s16le, any size up to 8 KiB, a sample may be split between frames).
- `ima-adpcm`: the byte stream of the WAV `data` chunk, cut anywhere (a frame need not hold whole blocks), at most
  8 KiB per frame. Each block of `block` bytes is: bytes 0-1 the first sample (i16 LE), byte 2 the step index
  (0..88; a larger value is taken as 88), byte 3 zero (ignored); then `block - 4` bytes of 4-bit codes, low nibble
  first, one sample each. A block holds `2 * (block - 4) + 1` samples. This is ffmpeg's `adpcm_ima_wav` for one
  channel; Fortuna's encoder (`fortuna-voice`, bit-exact with ffmpeg) writes it. At `end`, a last partial block of at
  least 4 bytes is decoded for the codes it has; under 4 bytes it is dropped.
- The server decodes as frames arrive and resamples to 16 kHz (linear) for the VAD and speech-to-text, so a
  turn's work starts the moment the owner stops. The caps count decoded 16 kHz audio: a command keeps section 2's
  1 MiB (about 32 s), a note up to 10 minutes. Reaching the cap sends `endpoint` and runs the turn, as before. A note
  is never ended by a pause and never trimmed: the server runs no VAD on it.

### 10.3 Command turns: text while it is written

With `text: true` (or `speak: false`) a command turn also sends, before or between the audio frames:

- `{"type":"delta","text":"<fragment>"}` as Lucina writes (her SSE `delta`s, section 1);
- `{"type":"answer","text":"<the whole reply>","replaces":bool}` once, when Lucina has settled the reply (her
  `reply` event). `replaces: true` means: show `text` instead of the deltas. A sensitive reply's text is the
  fixed phrase, as for speech.

`transcript`, `reply` and `done` stay as in section 2. With `speak: false` no `audio` frames come (and no filler, no
Kokoro call); `reply.text` is then the whole settled reply, not cut at the 1200 characters speech is held to, and
`done.ms` has `stt`, `lucina_first` and `total` (and `route`, when Lucina sent one) but no `tts_first`, `first_audio` or
`ack`. An empty reply is the apology of section 2 in `answer` and `reply` alike, with `replaces: true`; a reply that
reached the server only in Lucina's `done` (no `reply` event) is an `answer` with `replaces: true`. Lucina is asked in
her spoken style (`voice: true`) either way.

### 10.4 Note turns

1. `start {purpose:"note"}` → `listening`. Frames stream in while the owner talks; nothing is stored yet.
2. `end` (the owner let go) → the turn runs, t0 = that moment:
   - **Transcript.** Recordings up to 120 s go to the same local speech-to-text as a command (lucina-memo, about
     80 ms; Gemini as fallback); then `{"type":"transcript","text":"…"}`. Longer ones, or a failed or empty local
     transcription, send no `transcript`: lucina-rs then transcribes the file itself (Whisper large-v3), as for any
     shared voice note. Under 0.3 s of audio the note is `error no_speech` and nothing is posted.
   - **The file.** `pcm16` becomes a 16 kHz PCM WAV; `ima-adpcm` becomes an IMA-ADPCM WAV of the client's own
     blocks (the same file Fortuna used to upload). Name `voice-<UTC yyyymmddThhmmss>.wav`.
   - **The share.** The server posts it as `POST /v1/share` (docs/share.md) through Caddy's loopback site
     (`SECRETARY_VOICE_SHARE_URL`, default `http://127.0.0.1:8801/v1/share`) with the caller's own credentials (as
     for speech-to-text): `shareId` = the turn id, `intent=ask`, `lang`, `sessionId`, `channel`, `source`,
     `replyTo`, the file, and `meta={"files":[{"index":0,"voice":true,"durationMs":…,"transcript"?:…}]}`.
     A non-empty transcript makes lucina-rs skip Whisper. Then `{"type":"note","shareId":"…","userItemId":"…"}`.
3. **The device is free now.** After `note` a new `start` is accepted; the note is durable on the server and its
   answer is followed in the background. When Lucina has answered, the server sends
   `{"type":"reply","shareId":"…","text":"…","itemId":"<Lucina's timeline id>"}` and then
   `{"type":"done","shareId":"…","ms":{"stt":…,"note":…,"reply":…,"total":…}}` (ms from t0; `stt` absent when the
   server did not transcribe). The server follows the share by event, not by polling (each status read goes through
   the gateway's auth and counts against the device's request budget): it opens Lucina's timeline stream with its own
   credentials, makes ONE status read (`GET /v1/share/{id}`) to catch an answer written before the stream opened, and
   takes the answer from the timeline item with `role` `lucina` and `replyTo` equal to the note's `userItemId`. At the
   3-minute deadline it makes one last status read. A stream that drops is reopened once with its last event id; after
   that the status is read every 5 s. This is done for at most 4 notes per connection; the oldest is dropped beyond that. When the socket
   closes, watching stops; the note and its answer are in the chat anyway.
4. Errors: before `note`, `error` as in section 2 plus `note_failed` (the share was refused; the message has the
   status). After `note`, a share that ends in `error` sends `{"type":"error","code":"lucina_unavailable","shareId":…}`,
   and so does one that has not answered at the 3-minute deadline (the note is in the chat anyway). A share's `error` is not on the timeline: the first or the deadline status read finds it, or the 5 s reads of a dropped stream.
   `cancel` before `note` drops the recording (nothing is posted); after `note` it changes nothing. A `cancel` that
   arrives while the share request is already on its way may come too late to stop it; the device then still gets `note`
   if the share was accepted.
5. The server keeps a record of a note's turn as it does for a command (section 5): `ok`, `err` (`note_failed`), the
   `stt` and `total` spans, the audio bytes received, the transcript's length. It has no `first_audio`.

Every message about a note after `note` carries its `shareId`, because a new turn may already be running.

### 10.5 Clients

- **Connect early, keep it.** A client opens the socket when its voice UI shows (the Lucina tab, the chat screen,
  the DSi's Lucina tab), not when the owner presses; it keeps it while that UI is open and reopens with backoff.
  For a new TLS session the DSi pays about 6 s, so it opens the socket the moment the Lucina tab opens and keeps it.
  The server warms its speech-to-text when a socket opens (its engine loads in about 3 s and stops after 90 s
  idle), so opening early also takes that load off the first turn.
- **Stream from the first sample.** `start` goes on press, then frames of 20 to 100 ms as the mic delivers them.
- **Show and play as it comes.** `transcript` under the owner's bubble; `delta`/`answer` as text; `audio` frames into
  the player at once.
- **Measure.** Each client logs (never the audio, the text or a token) the time from `end` to `transcript`, to the
  first `delta` or `answer`, and to the first `audio` frame.

### 10.6 Not done, and why

- **Partial transcripts while talking.** The local Whisper is not a streaming recognizer, and it answers about 80 ms
  after the end of a command-length recording, so the gain is small. Measure before adding a sliding window.
- **Opus.** Every client but the DSi has bandwidth to spare for 16 kHz PCM (32 KB/s), and the DSi has no Opus
  encoder; the server would need libopus for nothing. Not added.
- **Streaming the note's answer.** A note is answered by the share worker, which writes the reply when it is
  settled; `reply` for a note therefore comes whole. A command can stream its text (10.3).

### 10.7 Quiet input: level, lift, `silent` (2026-11-08)

The DSi's notes arrived about 30 dB too quiet (peak -29.6 dBFS, RMS -41.5 dBFS): the speech-to-text returned nothing and Lucina
said "I couldn't hear you". Before any speech-to-text the server measures the recording (the 16 kHz decode of a command or a
note; the pure rules are `crates/secretary-server/src/voice_rules.rs`, Verus-checked, with `voice_level.rs` as the glue; the
same code runs in lucina-rs for `/v1/share` voice notes, docs/share.md):

- **Measure.** Frames of 20 ms. The noise floor is the quietest tenth of the frames (a power of two over their mean
  squares); the gate is the floor plus 6 dB, never under -60 dBFS RMS and never over -45 dBFS RMS. Speech RMS is the RMS of the frames above the gate.
  Reported in dBFS: the peak, the overall RMS, the speech RMS.
- **Silent.** A peak under -60 dBFS, or no frame above the gate, or no whole frame: the socket answers
  `{"type":"error","code":"silent","message":"The recording was silent: check the microphone."}` and runs no speech-to-text and no chat; a
  note is not posted. (Before the VAD's `no_speech` check, so a dead microphone says so; a recording under 0.3 s is still `no_speech`.)
- **Lift.** Speech more than 3 dB under the target of -20 dBFS speech RMS gets the gain that brings it to the target, at most **+30 dB**; speech within
  3 dB of the target, or above it, gets none. Frames at or under the gate are not boosted (room noise and the gaps stay as they
  were). A limiter follows: no sample goes past -1 dBFS (the gain of a frame is cut so that its peak fits), so nothing clips.
- **Where.** Only the audio sent to the speech-to-text (and its Gemini fallback). A note's file, posted to `/v1/share`, is exactly
  as recorded; so is anything stored.
- **Logs and records.** The turn's log line and the server's record of the turn (`GET /v1/voice/devices/{id}/status`, `turns[]`)
  carry `in_peak_dbfs`, `in_rms_dbfs`, `speech_rms_dbfs`, `gain_db` (numbers, 0.1 dB; `gain_db` 0 = none). A silent turn's record
  has `err: "silent"` and the same numbers. Never audio or text.

### 10.8 Test devices (2026-11-08)

Agents measure the voice pipeline with a real device token, and their turns must reach neither the owner's chat nor his phone nor
his memory. `SECRETARY_VOICE_TEST_DEVICES` (comma-separated device ids; default none) lists such devices. For them:

- **Command turns** go to Lucina flagged `"test": true` (a body field of `/v1/chat`; only this pipeline sets it, `/v1/chat` clears it from a
  client's body) with the session `voice-test`, whatever session the `start` named. Lucina runs it as the owner's own turn (not guarded, routed, the
  Jev fast path allowed) and writes every item with `meta.via = "test"` and `meta.conversation = "voice-test"`: `via` makes no
  push (`notify::ingest::agent_chat`: any `via` is silent), and the store files the items under the conversation `test:voice-test`,
  neither the main conversation nor the Lucina Voice thread. The turn is not remembered (no Honcho write, no About-you observation).
- **Note turns** post the share with the form field `test=1` and the header `X-Voice-Test: <shareId>.<hex HMAC-SHA256(key, "voice-test:" + shareId)>`, the key being the Lucina API token (`SECRETARY_LUCINA_TOKEN_FILE`; the token itself is never sent, Caddy logs custom headers). Shares reach lucina-rs from any device, so lucina-rs believes `test=1` only with that proof, checked in constant time and for that very share; otherwise `400 bad test marker`. A marker without `test=1` is ignored (docs/share.md, "Test devices"); lucina-rs marks the share's items the same way, runs the
  turn in `voice-test`, remembers nothing and does not put the audio in the library. The note path is the real one (level rules, Whisper,
  the chat turn), so a measurement is of what a note really does.
- The socket's own messages (`transcript`, `reply`, `done`, …) are unchanged: the agent sees the same as any device.

Chat voice is never a memo: a voice message on this socket (command or note) is ordinary talk. The server calls lucina-memo only to
transcribe (`/v1/memos/stt`); nothing here, and nothing in lucina-rs's share path, makes a memo, a summary, an Obsidian note or a brief.
Memos come from the memo and meeting recorders (`/v1/memos`).
