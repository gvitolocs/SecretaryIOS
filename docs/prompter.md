# Lucina Prompter

Talking points on screen while the owner records. Lucina listens and crosses each point out as he
makes it, keeps a Missed row of the points he passed over, and writes every take back to the
script's Keep note.

```
web /prompter  ──wss /v1/memos/prompter/live──►  p1 Caddy @memos gate  ──tailnet──►  lucina-memo (nezopt :8795)
 mic 16 kHz PCM, open points                        (device auth, gateway key)          segmenter → whisper-server (small, gpu-lease)
 ◄── transcript, verdicts (local, then Jev)                                              ──► lucina-rs /api/secretary/prompter/judge
                                                                                              Jev (Lucina's key) + local matcher
Keep note (label "prompter") ◄── script, take log ──  secretary-server /v1/notes (p1)
```

## Pieces

| Piece | Where |
| --- | --- |
| Matcher and judge (pure) | `crates/lucina-prompter` |
| Judge endpoint | lucina-rs `crates/lucina-server/src/prompter.rs`, `POST /api/secretary/prompter/judge` (Secretary API bearer) |
| Live socket, speech to text, draft, summary | lucina-memo `crates/lucina-memo/src/prompter/` |
| Web | `web/src/views/Prompter.tsx`, `PrompterLive.tsx`, `lib/prompterTake.ts`, `lib/prompterScript.ts`, `lib/prompterLive.ts`, `api/prompter.ts` |
| Overlay window | Secretary Code (the Mac app): the `openPrompterOverlay` bridge, below |

## Scripts

A script is a Keep note with the label `prompter`, so it syncs everywhere and Lucina finds it with
her notes tool. Its Markdown:

```
## Intro                     ← a section (any heading)
- Lucina Prompter on screen  ← a point (-, *, •, 1., or a plain line)

---                          ← everything below is the take log
### Take 28 Oct 23:40 · 3:12
```

Points get positional ids (`s<section>p<point>`). The page edits points as a list or as text, drafts
them with Lucina (`POST /v1/memos/prompter/draft {topic, document?, language?}` → `{markdown}`),
or turns any Keep note into a script by adding the label.

## A take

1. The page opens the microphone (AudioWorklet, 16 kHz mono, 100 ms chunks) and the socket, then
   sends `start` and the open points (current section, Missed row, next section).
2. lucina-memo cuts the stream ([`segmenter.rs`](../crates/lucina-memo/src/prompter/segmenter.rs)):
   a partial transcript every 0.5 s while he speaks, a final at a 0.6 s pause, forced cuts after
   12 s at the quietest 20 ms of the last 2.5 s. Silence is never sent to whisper. A final much
   shorter than its partial (whisper derailing on the closing pass) is replaced by the partial.
3. Speech to text: one `whisper-server` with `ggml-small.bin`, started by the first take and stopped
   90 s after the last one. GPU first, under `gpu-lease run --name prompter --mb 1200 --wait 0`
   on the RX 7900 XTX only; when no lease is free, the CPU in a `CPUQuota=400%` user scope.
   Language: auto, locked after the first 1.5 s final (en/it/da), or set in the page.
4. Each new transcript window is judged twice: the local matcher at once (obvious hits tick in
   microseconds), then lucina-rs, where Jev answers one yes/no question per open nearby point (at
   most 8: current section, then Missed, then next section). Jev's probability replaces the
   matcher's for the points it answered. Jev off, cooling down, failing or lucina-rs down: the
   matcher's verdicts stand (fail open).
5. The page ([`prompterTake.ts`](../web/src/lib/prompterTake.ts)): p ≥ 0.75 crosses a point out,
   p ≥ 0.4 marks it started; a next-section point needs 0.85 and moves the take on, sending the
   open points left behind to Missed; a missed point said later still ticks. Done is sticky.
   A finished section moves on by itself (setting). Tap a point to toggle it; tap a missed one
   to go back.
6. Stop: the report (said in order with times, missed, not reached, out-of-order count, tick
   latency, Jev calls, transcript) goes on top of the note's take log. "Summarise with Lucina"
   (`POST /v1/memos/prompter/summary`) adds her review to that take. With "Also save the audio as
   a voice memo", the Record button's recorder runs alongside and the memo is uploaded as usual.

Keys: Space pause · ← → (N/P, PgUp/PgDn) section · R reset section · 1-9 tick · M first missed ·
E edit points · Esc stop.

### Socket protocol

Client → server: text `{"type":"start","language","vocabulary":[…],"useJev"}`; binary PCM s16le;
text `{"type":"points","section","candidates":[{"id","text","kind":"current|missed|next"}]}`;
text `{"type":"stop"}`.

Server → client: `status` (`loading`/`ready`, `engine` gpu|cpu); `transcript` (`final`, `partial`,
`startMs`, `audioMs`, `sttMs`, `language`); `verdicts` (`phase` local|jev, `verdicts`
[{id, p, local, jev?, source}], `audioMs`, `sttMs`, `judgeMs`, `jev` {asked, ms, inputTokens,
error}, `jevCalls`, `jevFailed`); `judge` (lucina-rs unreachable); `error`.
`audioMs` counts from the first sample; the page keeps each chunk's send time and shows
speech-to-tick latency from it.

## Privacy

- Audio goes from the device to nezopt only (through p1's Caddy, not stored there) and is never
  written to disk; only its text leaves lucina-memo, to lucina-rs on the same host.
- Jev sees transcript text and point texts only, on Lucina's own key
  (`Lucina/prod#JEV_API_KEY_LUCINA`). The judge refuses any `LUCINA_JEV_URL` that is not
  `https://api.typesafe.ai`, so an anonymous free route never sees his speech.
- Draft and summary use Lucina's own model key in lucina-memo.

## Settings (lucina-memo environment)

| Variable | Default |
| --- | --- |
| `LUCINA_PROMPTER_MODEL` | `~/models/whisper/ggml-small.bin` |
| `LUCINA_PROMPTER_WHISPER_SERVER` | `whisper-server` next to `LUCINA_MEMO_WHISPER_CLI` |
| `LUCINA_PROMPTER_STT_PORT` | 28796 (127.0.0.1) |
| `LUCINA_PROMPTER_GPU_MB` / `_THREADS` / `_CPU_QUOTA` | 1200 / 4 / 400% |
| `LUCINA_PROMPTER_JUDGE_URL` | `http://127.0.0.1:8790/api/secretary/prompter/judge` |
| `SECRETARY_API_TOKEN(_FILE)` | from `~/.config/lucina-rs/lucina.env` (already loaded by the unit) |

`GET /v1/memos/prompter/status` shows whether the engine runs and how many takes use it.

## Secretary Code overlay

The web page calls an optional bridge on `window.secretaryShell` (`web/src/lib/desktopShell.ts`):

- `openPrompterOverlay(path, {opacity})`: open `path` (this origin, `/prompter/<slug>?live=1&overlay=1`)
  in one small frameless window, always on top (also over full-screen apps), resizable, at
  `opacity` 0.3-1; a second call reuses the window.
- `setOverlayOpacity(v)`: inside the overlay, change its opacity.
- `closePrompterOverlay()`: inside the overlay, the take ended; close it.

The overlay runs the take itself (mic, socket, saving the take). The page marks the drag region with
`-webkit-app-region`. Without the bridge the Overlay button is hidden.

## Tests

- `cargo test -p lucina-prompter` (matcher, judge fail-open), `-p lucina-server --lib prompter`
  (keyed route only, endpoint), `-p lucina-memo --lib prompter` (segmenter timing, derailed finals,
  windows, replay rules).
- End to end, on nezopt: `PROMPTER_E2E=1 cargo test -p lucina-memo --test prompter_e2e -- --nocapture`
  streams `tests/prompter-take.wav` (26 s, English) in real time through the socket, the real
  whisper-server and the judge, and checks the ticks, their order, the Missed row and latency.
  With `PROMPTER_E2E_JUDGE_URL` (and `SECRETARY_API_TOKEN`) Jev answers and all six spoken points
  must tick; without it the judge is down and the matcher must still tick five.
- Web: `npx vitest run src/lib/prompterTake.test.ts` (reorder, repeats, reset, Missed, rebase).
