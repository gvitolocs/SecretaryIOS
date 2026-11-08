# voice-probe

Measures voice latency against Secretary, on the two paths:

- **`ws`** — the voice socket (`docs/lucinavoice-protocol.md`, sections 2 and 10).
  Streams the clip as frames while it is being recorded, sends `end`, and
  records every event relative to t0 (the moment `end` was written).
- **`share`** — the old path (`docs/share.md`): POST `/v1/share` with the
  whole recording, then poll `GET /v1/share/{id}` every 250 ms. This is the
  "before" number (the recording is uploaded at the end, and the old clients
  poll every 2 s; `reply_at_2s_poll` reports the latency as the old client
  would have seen it).

## Build

```sh
cargo build --manifest-path tools/voice-probe/Cargo.toml
cargo test --manifest-path tools/voice-probe/Cargo.toml
```

## Usage

`--token-file` is a file holding the Secretary device token (sent as
`Authorization: Bearer <token>`). It is never printed. The input is a mono
WAV: s16 at any rate, or IMA-ADPCM (for `--codec ima-adpcm` the input must
already be IMA-ADPCM; for `pcm16` it is decoded and resampled to 16 kHz).
Only character counts are printed by default; `--show-text` also prints the
transcript and the reply.

```sh
# Voice socket, one turn (the "after" number)
voice-probe ws \
  --url wss://getsecretary.tech/v1/voice/ws \
  --token-file token.txt \
  --wav ask.wav \
  --source probe

# Old path, one turn (the "before" number)
voice-probe share \
  --url https://getsecretary.tech \
  --token-file token.txt \
  --wav ask.wav \
  --source probe
```

Useful variants:

```sh
# Three turns on the same socket (how a client keeps the connection open),
# IMA-ADPCM over the wire, no real-time pacing, JSON output, save the reply audio
voice-probe ws --url wss://getsecretary.tech/v1/voice/ws --token-file token.txt \
  --wav ask.wav --codec ima-adpcm --no-realtime --repeat 3 --json \
  --out-audio reply.wav --source probe

# A note (the reply comes as text)
voice-probe ws --url wss://getsecretary.tech/v1/voice/ws --token-file token.txt \
  --wav ask.wav --purpose note --source probe
```

## Recorded events (t, in ms from t0 = `end` written)

| key            | when                                                        |
| -------------- | ----------------------------------------------------------- |
| `transcript`   | first `transcript` message (STT result)                     |
| `first_delta`  | first `delta` (reply text starts)                           |
| `answer`       | first `answer`                                              |
| `first_audio`  | first binary frame after the `audio` header                 |
| `reply`        | `reply` text (text-mode turns)                              |
| `done`         | `done` (the total)                                          |

`done` also carries the server's own `ms` object when present; with
`--repeat N` a summary (min / median / max per key) is printed at the end.
A `section10` marker in the output says whether the server speaks the
section-10 dialect (`listening` echoed the `codec`).

## Tests

- Unit: WAV parsing (plain + IMA-ADPCM with `fact`/`LIST` chunks), the IMA
  decoder against an ffmpeg-generated fixture (`tests/fixtures/`, bit-exact
  vs the ffmpeg s16le decode), the resampler, frame pacing math, 2 s poll
  rounding, the summary, the JSON shape.
- End-to-end: `tests/e2e_mock.rs` runs a real WebSocket in-process (mock
  Secretary) and checks the full turn: handshake, paced frames, event order
  and timings, the `done` ms object, byte accounting.
