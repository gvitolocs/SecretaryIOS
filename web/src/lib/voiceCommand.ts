// A voice command to Lucina on the stage: `start {purpose:"command", mode:"vad", text:true}`, the
// microphone streamed until the server's `endpoint` (or a second press: `end`), the reply text shown
// as it is written (`delta`; `answer` with `replaces` swaps it) and her voice played as the audio
// frames arrive (docs/lucinavoice-protocol.md 2 and 10.3). Socket, microphone and player are injected.

import { openMicrophone as openMic, type Microphone } from "./micCapture";
import type { PcmPlayer } from "./pcmPlayer";
import type { VoiceTurnEvent } from "./voiceProtocol";
import type { VoiceSocket } from "./voiceSocket";
import { failureOf, NOTE_CHUNK_MS, type NoteFailure } from "./voiceNote";

export type CommandFailure = NoteFailure;

export interface CommandDeps {
  socket: Pick<VoiceSocket, "startTurn" | "oldServer" | "busy" | "status">;
  player: Pick<PcmPlayer, "push" | "end" | "stop">;
  openMicrophone?: typeof openMic;
}

export interface CommandParams {
  /** false when she is muted: no audio frames, no filler, the reply comes as text only. */
  speak: boolean;
  sessionId?: string;
  lang?: string;
}

export interface CommandCallbacks {
  onProgress(level: number): void;
  onTranscript(text: string): void;
  /** The reply text so far (grows with each `delta`, replaced by `answer`). */
  onText(text: string): void;
  /** The server heard the end of speech (or the device stopped it): she is working on it. */
  onSpoken(): void;
  /** The server has said everything (`done`) and what it sent has been played. */
  onFinished(): void;
  onFailure(why: CommandFailure): void;
  /** The microphone is closed (any reason). Once. */
  onStopped(): void;
}

export interface CommandRun {
  /** A second press while recording: the server starts work now. */
  finish(): void;
  /** Drop everything, and stop her voice. */
  cancel(): void;
}

/** The reply text after one more message (pure; the display rule of 10.3). */
export function nextReplyText(current: string, e: VoiceTurnEvent): string {
  if (e.type === "delta") return current + e.text;
  // `replaces: false` means the deltas already hold it; with no deltas, the answer is all there is.
  if (e.type === "answer") return e.replaces || current === "" ? e.text : current;
  return current;
}

export function runCommand(deps: CommandDeps, params: CommandParams, cb: CommandCallbacks): CommandRun | CommandFailure {
  if (deps.socket.oldServer) return "oldServer";
  if (deps.socket.status !== "open") return "offline";
  let mic: Microphone | null = null;
  let micOpen = true;
  let alive = true;
  let spoken = false;
  let text = "";
  const stopMic = () => {
    if (!micOpen) return;
    micOpen = false;
    mic?.stop();
    mic = null;
    cb.onStopped();
  };
  const markSpoken = () => {
    if (spoken) return;
    spoken = true;
    stopMic();
    cb.onSpoken();
  };
  const turn = deps.socket.startTurn(
    {
      mode: "vad",
      purpose: "command",
      thread: "main",
      text: true,
      speak: params.speak,
      ...(params.sessionId ? { sessionId: params.sessionId } : {}),
      ...(params.lang ? { lang: params.lang } : {}),
    },
    (e) => {
      if (!alive) return;
      switch (e.type) {
        case "transcript":
          cb.onTranscript(e.text);
          break;
        case "delta":
        case "answer":
          text = nextReplyText(text, e);
          cb.onText(text);
          break;
        case "endpoint":
          markSpoken();
          break;
        case "frame":
          if (params.speak) deps.player.push(e.pcm, e.rate);
          break;
        case "done":
          markSpoken();
          alive = false;
          deps.player.end(() => cb.onFinished());
          break;
        case "error":
          alive = false;
          stopMic();
          deps.player.stop();
          cb.onFailure(failureOf(e.code));
          break;
        case "unsupported":
          alive = false;
          stopMic();
          cb.onFailure("oldServer");
          break;
        case "closed":
          alive = false;
          stopMic();
          deps.player.stop();
          cb.onFailure("offline");
          break;
        default:
          break;
      }
    },
  );
  if (!turn) return deps.socket.busy ? "busy" : "offline";
  (deps.openMicrophone ?? openMic)(
    (pcm, level) => {
      if (!micOpen) return;
      turn.audio(pcm);
      cb.onProgress(level);
    },
    { chunkMs: NOTE_CHUNK_MS },
  ).then(
    (m) => {
      if (micOpen) mic = m;
      else m.stop();
    },
    () => {
      if (!alive) return;
      alive = false;
      stopMic();
      turn.cancel();
      cb.onFailure("micBlocked");
    },
  );
  return {
    finish() {
      if (!micOpen) return;
      stopMic();
      turn.end();
      spoken = true;
      cb.onSpoken();
    },
    cancel() {
      const was = alive;
      alive = false;
      stopMic();
      deps.player.stop();
      if (was) turn.cancel();
    },
  };
}
