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
(`events.jsonl`, rotated at 2 MiB, keeping 2 files; `status.json`).

- `GET /v1/voice/devices`: `[{id, name, online, last_seen, fw, rssi?, est_mw?}]`.
- `GET /v1/voice/devices/{id}/status`:
  `{id, name, online, connected_since?, last_seen, specs, live:{…last stats…}, network:{today_up_bytes, today_down_bytes, total_up_bytes, total_down_bytes}, power:{est_mw, measured, energy_mwh_today}, turns:[last 20 {at, ok, err?, transcript_chars, speech_ms, spans:{stt, lucina_first, tts_first, first_audio, total}, device_first_audio_ms?, up_bytes, down_bytes}]}`.
  The last turns combine the server's spans with the device's `turn` event.
- `GET /v1/voice/devices/{id}/events?since=<rfc3339>&kind=<k>&limit=<≤500>`: newest first,
  `{events:[{at, k, …}], next?}`.

Both device routes accept the device's own token (a device sees only itself) or the owner's session.
