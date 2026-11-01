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
- `{"type":"listening"}`: reply to `start`.
- `{"type":"endpoint"}`: VAD found the end of speech (vad mode), or the 1 MiB (about 32 s) recording cap was hit (either
  mode). The client stops sending.
- `{"type":"transcript","text":"…"}`
- `{"type":"audio","rate":24000}`: precedes the first binary audio frame of the turn. Binary frames are s16le mono
  24 kHz PCM, at most 8 KiB each. Sample boundaries are kept (even lengths).
- `{"type":"reply","text":"…"}`: the text that was spoken (for logs), sent after the last audio frame and before
  `done`. For a sensitive reply it is the fixed phrase.
- `{"type":"done","ms":{"stt":…,"lucina_first":…,"tts_first":…,"first_audio":…,"total":…}}`: all measured from
  end-of-speech (the `end` frame or the VAD endpoint).
- `{"type":"error","code":"…","message":"…"}`: the turn ended without (complete) audio. `no_speech`, `stt_unavailable`,
  `lucina_unavailable`, `tts_failed`, `busy`, `rate_limited`, `bad_request`. The socket stays open after an error.

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
  `{id, name, online, connected_since?, last_seen, specs, boot?, live:{…last stats…}, network:{today_up_bytes, today_down_bytes, total_up_bytes, total_down_bytes}, power:{est_mw, measured, energy_mwh_today}, turns:[last 20, newest first: {at, ok, err?, transcript_chars, speech_ms, spans:{stt, lucina_first, tts_first, first_audio, total}, device_first_audio_ms?, up_bytes, down_bytes}]}`.
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
- **Jev starts first.** Jev intent classification starts at the beginning of the turn, concurrently with the rest of
  the turn's setup. Its state is facts only: the transcript, the current local date and time, and the channel `voice`,
  in the same template every time with no guessed context. Cap: 400 ms. A slower answer, a quota error or a breaker
  means fail open to today's path (tier-0, then planner), unchanged.
- **Fast intents** are answered without the planner LLM, with a short templated spoken reply in the turn's language
  (it/en). They are taken only when Jev's confidence is at least `LUCINA_VOICE_FAST_MIN` (default 0.85) and the
  existing tier-0 rules don't contradict it:
  - `temporal_query`, only for the current time or date (today's date, weekday, time); anything needing data
    (deadlines, appointments) goes the normal way;
  - `grocery.shopping_list.query`, read from the store;
  - `list_reminders`, today's or upcoming, read from the store;
  - `grocery.shopping_list.add` / `.remove` and `schedule_reminder` (including relative timers), only when the
    existing deterministic parsers resolve them without an LLM (items, due time). Otherwise they take the normal
    path. The write goes through the same store and writers as the normal path.
- A fast turn still writes the timeline (user item and Lucina item) like a normal turn, so the app shows it.
- **SSE additions** (both optional for clients):
  - `event: route` / `data: {"intent","confidence","jev_ms","fast":bool,"source":"jev"|"tier0"|"fallback"}`, as soon
    as the route is decided, before any reply text;
  - `event: ack` / `data: {"text":"Un attimo…"}`, sent right after `route` when the turn will take a slow path
    (intents `web_crawl`, `computer_task`, `explicit_memory_recall`, `calendar_event`, `agent_message`,
    `generate_image`, `graph_task`, `spreadsheet_update`, `cursor_task`, or any planner turn whose reply is not
    streaming), at most once per turn. It is never sent for fast turns or for streamed chat.
- **Log line:** one `event="voice_route"` line per voice turn with intent, confidence, jev_ms, fast and source.

### secretary-server
- **Filler audio:** at startup, and again on a TTS-voice change, synthesize a few filler phrases ("Un attimo…",
  "Vediamo…", "One moment…") with Kokoro and keep the PCM in memory. That is a few hundred KB at most; it is
  rebuilt lazily if Kokoro was down.
- **On `ack`:** if no audio has been sent for the turn yet, send the cached filler PCM at once, preceded by the
  `audio` header if it is the first audio, then continue with the real answer. If the cache is empty, synthesize the
  ack text like a sentence.
- **Timings:** the `route` fields go into the turn's `done.ms` (`route` = ms from end of speech to the route event,
  `ack` = ms to the filler audio) and into the telemetry turn record (`intent`, `confidence`, `jev_ms`, `fast`,
  `source`). They never include transcript text.
