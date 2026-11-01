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

## 2. secretary-server: `GET /v1/voice/ws` (WebSocket, device auth via `Authorization: Bearer` on the upgrade)

One connection per device, kept open. The server sends a WS ping every 20 s. One turn at a time.

Client → server:
- text `{"type":"start","mode":"ptt"|"vad","lang":"it"|"en"?}`: a turn begins (button pressed). The server starts
  warming STT now.
- binary: s16le mono PCM at 16 kHz, any frame size up to 8 KiB.
- text `{"type":"end"}`: the button was released (ptt), or the device gives up.
- text `{"type":"cancel"}`: drop the current turn (stop sending audio frames).

Server → client:
- `{"type":"listening"}`: reply to `start`.
- `{"type":"endpoint"}`: VAD found the end of speech (vad mode only). The client stops sending.
- `{"type":"transcript","text":"…"}`
- `{"type":"audio","rate":24000}`: precedes the first binary audio frame of the turn. Binary frames are s16le mono
  24 kHz PCM, at most 8 KiB each. Sample boundaries are kept (even lengths).
- `{"type":"reply","text":"…"}`: the final spoken text (for logs).
- `{"type":"done","ms":{"stt":…,"lucina_first":…,"tts_first":…,"first_audio":…,"total":…}}`: all measured from
  end-of-speech (the `end` frame or the VAD endpoint).
- `{"type":"error","code":"…","message":"…"}`: the turn ended without (complete) audio. `no_speech`, `stt_unavailable`,
  `lucina_unavailable`, `tts_failed`, `busy`, `bad_request`.

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
