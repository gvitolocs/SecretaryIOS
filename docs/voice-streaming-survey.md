# Voice input on every client: survey (2026-11-08)

The owner found Lucina's voice notes on the DSi far too slow and asked that on every device the audio flow to the
server *while* he talks. This is how each client sent voice before the change (origin/main a6fdbe7a, LucinaOS main
8c58a5dd4), what the server already had, and what changed. The contract for the change is
`docs/lucinavoice-protocol.md` section 10.

## What the server already had

`GET /v1/voice/ws` (section 2), built for the ESP32 speaker (abandoned 2026-11-01). The server collects s16le 16 kHz
frames while the owner talks, so the upload already overlaps speech. Its energy VAD trims the recording. At the end
it runs local Whisper (lucina-memo `/v1/memos/stt`, about 80 ms), then streaming chat (lucina-rs SSE, Jev fast path)
and Kokoro sentence by sentence. **No client used it.** It knew only spoken commands, only 16 kHz PCM, and only the
"Lucina Voice" thread.

## Clients before the change

| client | feature | capture | transport | when the upload starts | reply |
|---|---|---|---|---|---|
| iOS | voice command (Lucina tab) | `AVAudioEngine` tap → `SFSpeechRecognizer` it-IT, partial results, Apple's servers allowed (`LucinaStage.swift:133-221`) | text only: `POST /v1/chat` after recognition ends | no audio is uploaded; the text goes after a 1.6 s silence timer | whole JSON reply, then `POST /v1/tts` → MP3 of the whole reply (if unmuted) |
| iOS | voice note (chat mic) | `AVAudioRecorder` → AAC 48 kHz file (`MemoRecorder.swift:129`), on-device STT off (`VoiceNotes.swift:21`) | `POST /v1/share`, background URLSession (`ChatStore.swift:885`) | after release, whole file | lucina-rs Whisper large-v3 on the file, then the planner; app sees it on the timeline SSE or the 2 s share poll; optional `/v1/tts` read-out |
| web | none in chat | (Memos tab: `MediaRecorder` → resumable `/v1/memos` after recording; Prompter: AudioWorklet 16 kHz PCM over its own WebSocket) | – | – | – |
| Mac (Secretary Code, Electron around the web app) | none in chat | same as web; native capture only in the meeting recorder | – | – | – |
| Juno | none in chat (`lucina-app-chat` is text only, `ureq` + rustls, `POST /v1/chat`) | Recorder app: `/dev/lucina-audio-in`, s16le mono 48 kHz, `Mic` trait (`lucina-audio-decode/src/device.rs:88`) | Recorder: resumable `/v1/memos` after recording | – | – |
| Fortuna (DSi) | voice note (Lucina tab) | ARM7 mic 10909 Hz → `fortuna-ring` → IMA-ADPCM 256-byte blocks (`fortuna-voice`) in RAM | `POST /v1/share` multipart WAV over the one kept-alive TLS connection (`fortuna/kernel/src/lucina.rs:531-610`) | after release, whole file | lucina-rs Whisper large-v3, planner; the DSi polls `GET /v1/share/{id}` every 2 s |

Other facts that shaped the design:
- The DSi's TLS handshake costs about 6 s on the ARM9 (three P-384 checks of about 1.9 s), and there is no session
  resumption. One socket has to stay open while the Lucina tab shows. The DSi has no WebSocket client and no chunked
  upload yet.
- Juno has an SSE client (`lucina-account/src/sse.rs`) but no WebSocket client. tungstenite is already in the tree on
  the server side (`lucina-desk-web`).
- The web app already has an AudioWorklet that downsamples to 16 kHz s16 (`web/src/lib/prompterLive.ts`).
- None of the clients measured end of speech to first reply. The only numbers were server-side, from the ESP32
  work (section 6): first audio 1.6 to 7 s after the end of speech depending on the intent, STT about 80 ms, first
  Kokoro sentence 0.4 to 0.9 s.

## Measured before the change

Measured 2026-11-08 17:52-18:10 UTC from nezopt against production (getsecretary.tech), with the device token
"Voice probe (nezopt)" and Kokoro-made Italian clips (`it` voice `if_sara`, a 5.5 s note and a 1.7 s question). The
script is `~/scratch/voice-stream/baseline.sh` on nezopt. Times are ms after the recording ended.

| path | step | runs |
|---|---|---|
| voice note, iOS shape (AAC 48 kHz, `POST /v1/share`) | upload accepted (202) | 0.1 s, 0.2 s, 0.6 s |
| | Lucina's reply written (share `done`, 250 ms poll) | **62.3 s, 91.6 s, 90.9 s** |
| | the same seen with the clients' 2 s poll | 62.6 s, 92.2 s, 92.0 s |
| voice note, DSi shape (IMA-ADPCM WAV) | upload | `429` each time, while the iOS-shape share of the same device was still processing |
| voice command, iOS shape (`POST /v1/chat` with the text, then `/v1/tts`) | reply JSON | 2.8 s, 2.5 s, 1.8 s |
| | whole MP3 back from `/v1/tts` (32-character reply) | 8.2 s, 15.8 s, 8.2 s |
| fast local STT used by the socket (`/v1/memos/stt`, 16 kHz PCM) | server time | 87-228 ms warm, 3.4 s cold |

Where the minute goes (lucina-rs journal for share 9f1202e3): received 17:52:32.9, first step of the chat turn
(Jev intent) 17:53:32.2, reply model done 17:53:34.0. **About 59 s is Whisper large-v3 on the shared file**
(GPU lease next to the resident Qwen server, cold load); the reply itself takes about 2 s.

On top of these the iOS command path waits 1.6 s of silence before it sends the text, and Apple's recognizer must
finish first, so end of speech to first audio was about 10 to 17 s. The DSi pays its handshake (about 6 s) whenever
the kept-alive connection has been dropped, and polls every 2 s.

## What changed

Section 10 of the protocol, on the same socket:
- `codec: "ima-adpcm"` with `rate`/`block`, so the DSi streams its existing encoder output and the server decodes and
  resamples it as it arrives;
- `purpose: "note"`: at release the server transcribes with the fast local STT and posts the share itself with the
  transcript, so lucina-rs skips Whisper large-v3. The reply comes down the socket. The device is free for the next
  turn at once;
- `thread: "main"`, `text`, `speak`: phone and laptop commands stay in the main chat and show the reply text as it is
  written;
- clients open the socket when their voice UI shows and keep it open.

## Measured after the change

To be filled in once the clients run against the deployed server (the probe `tools/voice-probe` and each client's logged timings).
